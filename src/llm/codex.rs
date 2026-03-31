use futures_util::StreamExt;
use std::collections::HashMap;

use super::{
    AssistantPhase, LlmEvent, LlmProvider, LlmStream, Message, ModelListFuture, ProviderError,
    Role, ToolDefinition, UsageStats,
    common::{SseLineDecoder, build_http_client, infer_initiator, send_streaming_request},
};

pub const DEFAULT_BASE_URL: &str = "https://chatgpt.com/backend-api";

pub struct CodexProvider {
    base_url: String,
    model: String,
    api_key: String,
    extra_headers: Vec<(String, String)>,
    reasoning_effort: Option<String>,
    client: reqwest::Client,
}

impl CodexProvider {
    /// Create a provider for direct chatgpt.com Codex access (requires account_id).
    pub fn new(
        base_url: impl Into<String>,
        model: impl Into<String>,
        api_key: impl Into<String>,
        account_id: impl Into<String>,
    ) -> Self {
        let account_id = account_id.into();
        let extra_headers = vec![
            ("chatgpt-account-id".to_string(), account_id),
            ("originator".to_string(), "pi".to_string()),
        ];
        Self::new_with_headers(base_url, model, api_key, extra_headers)
    }

    /// Create a provider with custom extra headers (e.g. for GitHub Copilot proxy).
    pub fn new_with_headers(
        base_url: impl Into<String>,
        model: impl Into<String>,
        api_key: impl Into<String>,
        extra_headers: Vec<(String, String)>,
    ) -> Self {
        Self {
            base_url: base_url.into(),
            model: model.into(),
            api_key: api_key.into(),
            extra_headers,
            reasoning_effort: None,
            client: build_http_client(),
        }
    }

    pub fn with_reasoning_effort(mut self, effort: Option<String>) -> Self {
        self.reasoning_effort = effort;
        self
    }

    fn stream_inner(&self, messages: Vec<Message>, tools: Vec<ToolDefinition>) -> LlmStream {
        let url = resolve_codex_url(&self.base_url);
        let model = self.model.clone();
        let api_key = self.api_key.clone();
        let extra_headers = self.extra_headers.clone();
        let reasoning_effort = self.reasoning_effort.clone();
        let client = self.client.clone();

        Box::pin(async_stream::stream! {
            // Separate system prompt from the rest.
            let instructions: Option<String> = messages.iter()
                .find(|m| m.role == Role::System)
                .map(|m| m.content.clone());

            let input: Vec<serde_json::Value> = convert_messages(&messages);

            let mut body = serde_json::json!({
                "model": model,
                "store": false,
                "stream": true,
                "input": input,
                "text": { "verbosity": "medium" },
                "include": ["reasoning.encrypted_content"],
                "tool_choice": "auto",
                "parallel_tool_calls": true,
            });

            let explicit_reasoning = reasoning_effort
                .as_deref()
                .map(|effort| clamp_reasoning_effort_for_model(&model, effort));

            if let Some(effort) = explicit_reasoning {
                body["reasoning"] = serde_json::json!({
                    "effort": effort,
                    "summary": "auto",
                });
            } else if model.starts_with("gpt-5") {
                // Align with pi-mono OpenAI-Responses behavior: when no explicit
                // reasoning level is configured, bias GPT-5 toward low/no reasoning.
                let low_reasoning_hint = serde_json::json!({
                    "role": "developer",
                    "content": [{
                        "type": "input_text",
                        "text": "# Juice: 0 !important"
                    }]
                });
                if let Some(arr) = body["input"].as_array_mut() {
                    arr.push(low_reasoning_hint);
                }
            }

            if let Some(sys) = instructions {
                body["instructions"] = serde_json::Value::String(sys);
            }

            if !tools.is_empty() {
                body["tools"] = serde_json::json!(convert_tools(&tools));
            }

            log::debug!(
                "[TAU_DEBUG] → codex request:\n{}",
                serde_json::to_string_pretty(&body).unwrap_or_default()
            );

            let mut req = client
                .post(&url)
                .header("Authorization", format!("Bearer {api_key}"))
                .header("OpenAI-Beta", "responses=experimental")
                .header("accept", "text/event-stream")
                .header("content-type", "application/json")
                .json(&body);
            let use_dynamic_initiator = extra_headers
                .iter()
                .any(|(k, _)| k.eq_ignore_ascii_case("X-Initiator"));
            for (k, v) in &extra_headers {
                if !k.eq_ignore_ascii_case("X-Initiator") {
                    req = req.header(k.as_str(), v.as_str());
                }
            }
            if use_dynamic_initiator {
                req = req.header("X-Initiator", infer_initiator(&messages));
            }
            let response = match send_streaming_request(req, "Codex").await {
                Ok(r) => r,
                Err(e) => { yield LlmEvent::Error(e); return; }
            };

            let mut byte_stream = response.bytes_stream();
            let mut sse = SseLineDecoder::new();
            // Track pending function calls keyed by item index or call_id.
            let mut pending_calls: HashMap<String, PendingCall> = HashMap::new();
            let mut current_call_item_id: Option<String> = None;
            let mut emitted_tool_intent = false;
            let mut line_num = 0usize;

            while let Some(chunk) = byte_stream.next().await {
                let bytes = match chunk {
                    Ok(b) => b,
                    Err(e) => { yield LlmEvent::Error(ProviderError::network("Codex", e.to_string())); return; }
                };
                sse.push_bytes(&bytes);

                while let Some(data) = sse.next_data_line() {
                    if data == "[DONE]" {
                        yield LlmEvent::Done;
                        return;
                    }

                    log::debug!("[TAU_DEBUG] ← chunk {line_num}: {data}");
                    line_num += 1;

                    let ev: serde_json::Value = match serde_json::from_str(&data) {
                        Ok(v) => v,
                        Err(e) => { yield LlmEvent::Error(ProviderError::other("Codex", format!("Parse error: {e}"))); return; }
                    };

                    let ev_type = ev["type"].as_str().unwrap_or("");

                    match ev_type {
                        // ── Text token ────────────────────────────────────────
                        "response.output_text.delta" => {
                            if let Some(delta) = ev["delta"].as_str()
                                && !delta.is_empty() {
                                    yield LlmEvent::Token {
                                        text: delta.to_string(),
                                        phase: if emitted_tool_intent {
                                            AssistantPhase::Provisional
                                        } else {
                                            AssistantPhase::Unknown
                                        },
                                    };
                                }
                        }

                        // ── Thinking / reasoning token ────────────────────────
                        "response.reasoning_summary_text.delta" => {
                            if let Some(delta) = ev["delta"].as_str()
                                && !delta.is_empty() {
                                    yield LlmEvent::ThinkingToken(delta.to_string());
                                }
                        }

                        // ── Function call started ─────────────────────────────
                        "response.output_item.added" => {
                            let item = &ev["item"];
                            if item["type"].as_str() == Some("function_call") {
                                if !emitted_tool_intent {
                                    emitted_tool_intent = true;
                                    yield LlmEvent::ToolIntentStart;
                                }
                                let item_id = item["id"].as_str()
                                    .unwrap_or("")
                                    .to_string();
                                pending_calls.insert(item_id.clone(), PendingCall {
                                    arguments: String::new(),
                                });
                                current_call_item_id = Some(item_id);
                            }
                        }

                        // ── Function call arguments delta ─────────────────────
                        "response.function_call_arguments.delta" => {
                            let item_id = ev["item_id"].as_str().unwrap_or("");
                            if let Some(call) = pending_calls.get_mut(item_id) {
                                if let Some(delta) = ev["delta"].as_str() {
                                    call.arguments.push_str(delta);
                                }
                            } else if let Some(ref id) = current_call_item_id.clone()
                                && let Some(call) = pending_calls.get_mut(id)
                                    && let Some(delta) = ev["delta"].as_str() {
                                        call.arguments.push_str(delta);
                                    }
                        }

                        // ── Item completed ────────────────────────────────────
                        "response.output_item.done" => {
                            let item = &ev["item"];
                            if item["type"].as_str() == Some("function_call") {
                                let call_id = item["call_id"].as_str()
                                    .unwrap_or("")
                                    .to_string();
                                let name = item["name"].as_str()
                                    .unwrap_or("")
                                    .to_string();
                                let args_str = item["arguments"].as_str()
                                    .unwrap_or("{}");
                                let args: serde_json::Value = serde_json::from_str(args_str)
                                    .unwrap_or(serde_json::Value::Object(Default::default()));

                                // Remove from pending.
                                let item_id = item["id"].as_str().unwrap_or("");
                                pending_calls.remove(item_id);
                                if current_call_item_id.as_deref() == Some(item_id) {
                                    current_call_item_id = None;
                                }

                                yield LlmEvent::ToolCall { id: call_id, name, args };
                            }
                        }

                        // ── Stream complete ───────────────────────────────────
                        "response.completed" | "response.done" | "response.incomplete" => {
                            if let Some(usage) = extract_usage_stats(&ev) {
                                yield LlmEvent::Usage(usage);
                            }
                            yield LlmEvent::Done;
                            return;
                        }

                        // ── Errors ────────────────────────────────────────────
                        "response.failed" => {
                            let msg = ev["response"]["error"]["message"]
                                .as_str()
                                .unwrap_or("Codex response failed")
                                .to_string();
                            yield LlmEvent::Error(ProviderError::other("Codex", msg));
                            return;
                        }
                        "error" => {
                            let msg = ev["message"].as_str()
                                .or_else(|| ev["code"].as_str())
                                .unwrap_or("Unknown error")
                                .to_string();
                            yield LlmEvent::Error(ProviderError::other("Codex", format!("Codex error: {msg}")));
                            return;
                        }

                        _ => {} // ignore other event types
                    }
                }
            }

            yield LlmEvent::Done;
        })
    }
}

fn extract_usage_stats(ev: &serde_json::Value) -> Option<UsageStats> {
    let usage = ev
        .get("response")
        .and_then(|r| r.get("usage"))
        .or_else(|| ev.get("usage"))?;

    let input = usage
        .get("input_tokens")
        .or_else(|| usage.get("prompt_tokens"))
        .and_then(|v| v.as_u64())
        .and_then(|n| usize::try_from(n).ok());

    let output = usage
        .get("output_tokens")
        .or_else(|| usage.get("completion_tokens"))
        .and_then(|v| v.as_u64())
        .and_then(|n| usize::try_from(n).ok());

    let total = usage
        .get("total_tokens")
        .and_then(|v| v.as_u64())
        .and_then(|n| usize::try_from(n).ok());

    if input.is_none() && output.is_none() && total.is_none() {
        None
    } else {
        Some(UsageStats {
            input_tokens: input,
            output_tokens: output,
            total_tokens: total,
        })
    }
}

fn clamp_reasoning_effort_for_model(model: &str, requested: &str) -> String {
    let effort = requested.to_ascii_lowercase();
    let supports_minimal = model.starts_with("gpt-5.2")
        || model.starts_with("gpt-5.3")
        || model.starts_with("gpt-5.4");
    let supports_xhigh = model.starts_with("gpt-5.1");
    let max_effort = if model.contains("mini") {
        "high"
    } else {
        "xhigh"
    };

    match effort.as_str() {
        "minimal" if supports_minimal => "minimal".to_string(),
        "minimal" => "low".to_string(),
        "xhigh" if supports_xhigh && max_effort == "xhigh" => "xhigh".to_string(),
        "xhigh" => "high".to_string(),
        "high" if max_effort == "high" || max_effort == "xhigh" => "high".to_string(),
        "medium" | "low" => effort,
        _ => "low".to_string(),
    }
}

// ── URL helpers ───────────────────────────────────────────────────────────────

fn resolve_codex_url(base_url: &str) -> String {
    let normalized = base_url.trim_end_matches('/');
    if normalized.ends_with("/responses") {
        // Already a complete responses URL (e.g. .../v1/responses or .../codex/responses)
        normalized.to_string()
    } else if normalized.ends_with("/codex") {
        format!("{normalized}/responses")
    } else {
        format!("{normalized}/codex/responses")
    }
}

// ── Message conversion ────────────────────────────────────────────────────────

/// Convert tau `Message` history to the Responses API `input` array.
/// System messages are excluded (they go in `instructions`).
fn convert_messages(messages: &[Message]) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    let mut msg_idx = 0usize;

    for msg in messages {
        match msg.role {
            Role::System => {} // handled as `instructions`

            Role::User => {
                out.push(serde_json::json!({
                    "role": "user",
                    "content": [{ "type": "input_text", "text": msg.content }]
                }));
                msg_idx += 1;
            }

            Role::Assistant => {
                out.push(serde_json::json!({
                    "type": "message",
                    "role": "assistant",
                    "status": "completed",
                    "id": format!("msg_{msg_idx}"),
                    "content": [{ "type": "output_text", "text": msg.content, "annotations": [] }]
                }));
                msg_idx += 1;
            }

            Role::ToolCall => {
                let call_id = msg.tool_call_id.as_deref().unwrap_or("call_0");
                let name = msg.tool_name.as_deref().unwrap_or("");
                let args = msg
                    .tool_args
                    .as_ref()
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "{}".to_string());
                out.push(serde_json::json!({
                    "type": "function_call",
                    "call_id": call_id,
                    "name": name,
                    "arguments": args,
                }));
            }

            Role::ToolResult => {
                let call_id = msg.tool_call_id.as_deref().unwrap_or("call_0");
                out.push(serde_json::json!({
                    "type": "function_call_output",
                    "call_id": call_id,
                    "output": msg.content,
                }));
            }
        }
    }

    out
}

fn convert_tools(tools: &[ToolDefinition]) -> Vec<serde_json::Value> {
    tools
        .iter()
        .map(|t| {
            serde_json::json!({
                "type": "function",
                "name": t.name,
                "description": t.description,
                "parameters": t.parameters,
            })
        })
        .collect()
}

// ── Pending call accumulator ──────────────────────────────────────────────────

struct PendingCall {
    arguments: String,
}

// ── Known models ──────────────────────────────────────────────────────────────

fn known_models() -> Vec<String> {
    vec![
        "gpt-5.4".to_string(),
        "gpt-5.4-pro".to_string(),
        "gpt-5.3-codex".to_string(),
        "gpt-5.3-codex-spark".to_string(),
        "gpt-5.2".to_string(),
        "gpt-5.2-pro".to_string(),
        "gpt-5.2-codex".to_string(),
        "gpt-5.1".to_string(),
        "gpt-5.1-codex".to_string(),
        "gpt-5.1-codex-max".to_string(),
        "gpt-5.1-codex-mini".to_string(),
        "gpt-5".to_string(),
        "gpt-5-codex".to_string(),
        "gpt-5-mini".to_string(),
        "codex-mini-latest".to_string(),
    ]
}

// ── LlmProvider impl ──────────────────────────────────────────────────────────

impl LlmProvider for CodexProvider {
    fn stream_chat(&self, messages: Vec<Message>) -> LlmStream {
        self.stream_inner(messages, vec![])
    }

    fn stream_chat_with_tools(
        &self,
        messages: Vec<Message>,
        tools: Vec<ToolDefinition>,
    ) -> LlmStream {
        self.stream_inner(messages, tools)
    }

    fn list_models(&self) -> ModelListFuture {
        let models = known_models();
        Box::pin(async move { Ok(models) })
    }
}
