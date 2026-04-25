use serde::Deserialize;
use std::collections::HashMap;

use super::{
    AssistantPhase, LlmEvent, LlmProvider, LlmStream, Message, ModelListFuture, ProviderError,
    Role, ToolDefinition, UsageStats,
    common::{
        StreamControl, build_http_client, infer_initiator, send_streaming_request, stream_sse_lines,
    },
    provider_format::to_codex_wire,
};

// ── Typed SSE event structs ───────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum CodexEvent {
    #[serde(rename = "response.output_text.delta")]
    OutputTextDelta { delta: String },
    #[serde(rename = "response.reasoning_summary_text.delta")]
    ReasoningSummaryTextDelta { delta: String },
    #[serde(rename = "response.output_item.added")]
    OutputItemAdded { item: OutputItem },
    #[serde(rename = "response.function_call_arguments.delta")]
    FunctionCallArgumentsDelta { item_id: String, delta: String },
    #[serde(rename = "response.output_item.done")]
    OutputItemDone { item: OutputItem },
    #[serde(rename = "response.completed")]
    ResponseCompleted { response: Option<CodexResponse> },
    #[serde(rename = "response.done")]
    ResponseDone { response: Option<CodexResponse> },
    #[serde(rename = "response.incomplete")]
    ResponseIncomplete { response: Option<CodexResponse> },
    #[serde(rename = "response.failed")]
    ResponseFailed { response: Option<CodexResponse> },
    #[serde(rename = "error")]
    Error {
        message: Option<String>,
        code: Option<String>,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum OutputItem {
    FunctionCall {
        id: String,
        call_id: String,
        name: String,
        arguments: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct CodexResponse {
    usage: Option<CodexUsage>,
    error: Option<CodexError>,
}

#[derive(Deserialize)]
struct CodexUsage {
    input_tokens: Option<usize>,
    output_tokens: Option<usize>,
    total_tokens: Option<usize>,
    // Fallback field names used by some proxy versions.
    prompt_tokens: Option<usize>,
    completion_tokens: Option<usize>,
}

impl From<CodexUsage> for UsageStats {
    fn from(u: CodexUsage) -> Self {
        let input = u.input_tokens.or(u.prompt_tokens);
        let output = u.output_tokens.or(u.completion_tokens);
        let total = u.total_tokens.or_else(|| {
            let i = input?;
            let o = output?;
            Some(i.saturating_add(o))
        });
        Self {
            input_tokens: input,
            output_tokens: output,
            total_tokens: total,
        }
    }
}

#[derive(Deserialize)]
struct CodexError {
    message: Option<String>,
}

// ── Provider ──────────────────────────────────────────────────────────────────

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

            let input: Vec<serde_json::Value> = to_codex_wire(&messages);

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

            // Track pending function calls keyed by item id.
            let mut pending_calls: HashMap<String, PendingCall> = HashMap::new();
            let mut current_call_item_id: Option<String> = None;
            let mut emitted_tool_intent = false;

            let mut stream = stream_sse_lines("Codex", response, move |data, events| {
                if data == "[DONE]" {
                    events.push(LlmEvent::Done);
                    return StreamControl::Done;
                }

                let ev: CodexEvent = match serde_json::from_str(data) {
                    Ok(v) => v,
                    Err(e) => {
                        events.push(LlmEvent::Error(ProviderError::other("Codex", format!("Parse error: {e}"))));
                        return StreamControl::Done;
                    }
                };

                match ev {
                    CodexEvent::OutputTextDelta { delta } if !delta.is_empty() => {
                        events.push(LlmEvent::Token {
                            text: delta,
                            phase: if emitted_tool_intent {
                                AssistantPhase::Provisional
                            } else {
                                AssistantPhase::Unknown
                            },
                        });
                    }

                    CodexEvent::ReasoningSummaryTextDelta { delta } if !delta.is_empty() => {
                        events.push(LlmEvent::ThinkingToken(delta));
                    }

                    CodexEvent::OutputItemAdded {
                        item: OutputItem::FunctionCall { id, .. },
                    } => {
                        if !emitted_tool_intent {
                            emitted_tool_intent = true;
                            events.push(LlmEvent::ToolIntentStart);
                        }
                        pending_calls.insert(id.clone(), PendingCall { arguments: String::new() });
                        current_call_item_id = Some(id);
                    }

                    CodexEvent::FunctionCallArgumentsDelta { item_id, delta } => {
                        let key = if pending_calls.contains_key(&item_id) {
                            Some(item_id.clone())
                        } else {
                            current_call_item_id.clone()
                        };
                        if let Some(k) = key
                            && let Some(call) = pending_calls.get_mut(&k)
                        {
                            call.arguments.push_str(&delta);
                        }
                    }

                    CodexEvent::OutputItemDone {
                        item: OutputItem::FunctionCall { id, call_id, name, arguments },
                    } => {
                        let args: serde_json::Value = serde_json::from_str(&arguments)
                            .unwrap_or(serde_json::Value::Object(Default::default()));
                        pending_calls.remove(&id);
                        if current_call_item_id.as_deref() == Some(&id) {
                            current_call_item_id = None;
                        }
                        events.push(LlmEvent::ToolCall { id: call_id, name, args });
                    }

                    CodexEvent::ResponseCompleted { response }
                    | CodexEvent::ResponseDone { response }
                    | CodexEvent::ResponseIncomplete { response } => {
                        if let Some(usage) = response.and_then(|r| r.usage).map(UsageStats::from)
                            && (usage.input_tokens.is_some()
                                || usage.output_tokens.is_some()
                                || usage.total_tokens.is_some())
                        {
                            events.push(LlmEvent::Usage(usage));
                        }
                        events.push(LlmEvent::Done);
                        return StreamControl::Done;
                    }

                    CodexEvent::ResponseFailed { response } => {
                        let msg = response
                            .and_then(|r| r.error)
                            .and_then(|e| e.message)
                            .unwrap_or_else(|| "Codex response failed".to_string());
                        events.push(LlmEvent::Error(ProviderError::other("Codex", msg)));
                        return StreamControl::Done;
                    }
                    CodexEvent::Error { message, code } => {
                        let msg = message
                            .or(code)
                            .unwrap_or_else(|| "Unknown error".to_string());
                        events.push(LlmEvent::Error(ProviderError::other("Codex", format!("Codex error: {msg}"))));
                        return StreamControl::Done;
                    }

                    _ => {}
                }

                StreamControl::Continue
            });

            use futures_util::StreamExt as _;
            while let Some(ev) = stream.next().await {
                yield ev;
            }

            yield LlmEvent::Done;
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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> CodexEvent {
        serde_json::from_str(json).expect("parse failed")
    }

    #[test]
    fn output_text_delta_parses() {
        let ev = parse(r#"{"type":"response.output_text.delta","delta":"hello"}"#);
        let CodexEvent::OutputTextDelta { delta } = ev else {
            panic!()
        };
        assert_eq!(delta, "hello");
    }

    #[test]
    fn reasoning_summary_text_delta_parses() {
        let ev = parse(r#"{"type":"response.reasoning_summary_text.delta","delta":"think"}"#);
        let CodexEvent::ReasoningSummaryTextDelta { delta } = ev else {
            panic!()
        };
        assert_eq!(delta, "think");
    }

    #[test]
    fn output_item_added_function_call_parses() {
        let ev = parse(
            r#"{"type":"response.output_item.added","item":{"type":"function_call","id":"item_1","call_id":"call_1","name":"read_file","arguments":""}}"#,
        );
        let CodexEvent::OutputItemAdded {
            item: OutputItem::FunctionCall {
                id, call_id, name, ..
            },
        } = ev
        else {
            panic!("wrong variant")
        };
        assert_eq!(id, "item_1");
        assert_eq!(call_id, "call_1");
        assert_eq!(name, "read_file");
    }

    #[test]
    fn output_item_added_non_function_parses_as_other() {
        let ev = parse(r#"{"type":"response.output_item.added","item":{"type":"text","id":"x"}}"#);
        assert!(matches!(
            ev,
            CodexEvent::OutputItemAdded {
                item: OutputItem::Other
            }
        ));
    }

    #[test]
    fn function_call_arguments_delta_parses() {
        let ev = parse(
            r#"{"type":"response.function_call_arguments.delta","item_id":"item_1","delta":"{\"k\":"}"#,
        );
        let CodexEvent::FunctionCallArgumentsDelta { item_id, delta } = ev else {
            panic!()
        };
        assert_eq!(item_id, "item_1");
        assert_eq!(delta, "{\"k\":");
    }

    #[test]
    fn output_item_done_function_call_parses() {
        let ev = parse(
            r#"{"type":"response.output_item.done","item":{"type":"function_call","id":"item_1","call_id":"call_1","name":"read_file","arguments":"{\"path\":\"a.txt\"}"}}"#,
        );
        let CodexEvent::OutputItemDone {
            item:
                OutputItem::FunctionCall {
                    id,
                    call_id,
                    name,
                    arguments,
                },
        } = ev
        else {
            panic!()
        };
        assert_eq!(id, "item_1");
        assert_eq!(call_id, "call_1");
        assert_eq!(name, "read_file");
        assert_eq!(arguments, r#"{"path":"a.txt"}"#);
    }

    #[test]
    fn response_completed_with_usage_parses() {
        let ev = parse(
            r#"{"type":"response.completed","response":{"usage":{"input_tokens":10,"output_tokens":20,"total_tokens":30}}}"#,
        );
        let CodexEvent::ResponseCompleted { response } = ev else {
            panic!()
        };
        let usage = UsageStats::from(response.unwrap().usage.unwrap());
        assert_eq!(usage.input_tokens, Some(10));
        assert_eq!(usage.output_tokens, Some(20));
        assert_eq!(usage.total_tokens, Some(30));
    }

    #[test]
    fn response_completed_without_response_parses() {
        let ev = parse(r#"{"type":"response.completed"}"#);
        let CodexEvent::ResponseCompleted { response } = ev else {
            panic!()
        };
        assert!(response.is_none());
    }

    #[test]
    fn response_failed_parses_error_message() {
        let ev =
            parse(r#"{"type":"response.failed","response":{"error":{"message":"Rate limited"}}}"#);
        let CodexEvent::ResponseFailed { response } = ev else {
            panic!()
        };
        let msg = response.unwrap().error.unwrap().message.unwrap();
        assert_eq!(msg, "Rate limited");
    }

    #[test]
    fn error_event_parses_message_and_code() {
        let ev = parse(r#"{"type":"error","message":"oops","code":"E42"}"#);
        let CodexEvent::Error { message, code } = ev else {
            panic!()
        };
        assert_eq!(message.as_deref(), Some("oops"));
        assert_eq!(code.as_deref(), Some("E42"));
    }

    #[test]
    fn unknown_event_type_parses_as_unknown() {
        let ev = parse(r#"{"type":"response.audio.delta","delta":"..."}"#);
        assert!(matches!(ev, CodexEvent::Unknown));
    }

    #[test]
    fn usage_fallback_field_names() {
        let usage = CodexUsage {
            input_tokens: None,
            output_tokens: None,
            total_tokens: None,
            prompt_tokens: Some(5),
            completion_tokens: Some(10),
        };
        let stats = UsageStats::from(usage);
        assert_eq!(stats.input_tokens, Some(5));
        assert_eq!(stats.output_tokens, Some(10));
        assert_eq!(stats.total_tokens, Some(15));
    }

    #[test]
    fn resolve_codex_url_appends_path() {
        assert_eq!(
            resolve_codex_url("https://chatgpt.com/backend-api"),
            "https://chatgpt.com/backend-api/codex/responses"
        );
        assert_eq!(
            resolve_codex_url("https://proxy.example.com/codex"),
            "https://proxy.example.com/codex/responses"
        );
        assert_eq!(
            resolve_codex_url("https://proxy.example.com/v1/responses"),
            "https://proxy.example.com/v1/responses"
        );
    }

    #[test]
    fn clamp_reasoning_effort_minimal_fallback() {
        // gpt-5.1 doesn't support "minimal" → falls back to "low"
        assert_eq!(
            clamp_reasoning_effort_for_model("gpt-5.1", "minimal"),
            "low"
        );
        // gpt-5.2 supports "minimal"
        assert_eq!(
            clamp_reasoning_effort_for_model("gpt-5.2", "minimal"),
            "minimal"
        );
    }
}
