use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

use super::{
    AssistantPhase, LlmEvent, LlmProvider, LlmStream, Message, ModelListFuture, ProviderError,
    Role, ToolDefinition, UsageStats,
    common::{build_http_client, send_streaming_request},
};

pub struct OllamaProvider {
    pub base_url: String,
    pub model: String,
    /// Optional Bearer token injected as `Authorization: Bearer <api_key>`.
    /// Used when connecting to an authenticated proxy such as Open WebUI.
    pub api_key: Option<String>,
    client: reqwest::Client,
}

impl OllamaProvider {
    pub fn new(base_url: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            model: model.into(),
            api_key: None,
            client: build_http_client(),
        }
    }

    /// Add a Bearer token to all outgoing requests (e.g. for Open WebUI).
    pub fn with_api_key(mut self, key: impl Into<String>) -> Self {
        self.api_key = Some(key.into());
        self
    }
}

// ── Serde types ───────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<OllamaMessage>,
    stream: bool,
}

#[derive(Serialize)]
struct ChatRequestWithTools {
    model: String,
    messages: Vec<OllamaMessage>,
    tools: Vec<OllamaToolDef>,
    stream: bool,
}

#[derive(Serialize)]
struct OllamaMessage {
    role: &'static str,
    #[serde(skip_serializing_if = "String::is_empty")]
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<String>,
    /// Populated for Role::ToolCall messages (assistant turn with tool calls).
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<OllamaToolCallItem>>,
    /// Populated for Role::ToolResult messages so the model can match results
    /// back to the originating call.
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

/// A single tool call entry inside an assistant message.
#[derive(Serialize)]
struct OllamaToolCallItem {
    function: OllamaToolCallFunction,
}

#[derive(Serialize)]
struct OllamaToolCallFunction {
    name: String,
    arguments: serde_json::Value,
}

/// Tool definition sent in the request.
#[derive(Serialize)]
struct OllamaToolDef {
    r#type: &'static str,
    function: OllamaFunctionDef,
}

#[derive(Serialize)]
struct OllamaFunctionDef {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Deserialize)]
struct ChatChunk {
    message: ChunkMessage,
    #[serde(default)]
    done: bool,
    #[serde(default)]
    prompt_eval_count: Option<usize>,
    #[serde(default)]
    eval_count: Option<usize>,
}

#[derive(Deserialize)]
struct ChunkMessage {
    #[serde(default)]
    content: String,
    #[serde(default)]
    thinking: String,
    /// Present when the model decides to call a tool.
    #[serde(default)]
    tool_calls: Vec<ToolCallChunk>,
}

#[derive(Deserialize)]
struct ToolCallChunk {
    function: ToolCallFunction,
}

#[derive(Deserialize)]
struct ToolCallFunction {
    name: String,
    /// Ollama may return `arguments` as a JSON object **or** as a
    /// string-encoded JSON object depending on the model/version.
    /// `coerce_arguments` normalises the string case.
    arguments: serde_json::Value,
}

/// Normalise tool-call arguments: if Ollama returned them as a JSON string
/// (e.g. `"{\"path\":\".\"}"`), parse that string into an object.
/// Returns the value unchanged if it is already an object or array.
fn coerce_arguments(v: serde_json::Value) -> serde_json::Value {
    if let serde_json::Value::String(s) = &v
        && let Ok(parsed) = serde_json::from_str(s)
    {
        return parsed;
    }
    v
}
#[derive(Deserialize)]
struct TagsResponse {
    models: Vec<TagModel>,
}

#[derive(Deserialize)]
struct TagModel {
    name: String,
}

// ── History serialisation ─────────────────────────────────────────────────────

/// Convert a `Message` to an `OllamaMessage` for inclusion in a chat request.
fn to_ollama_message(msg: &Message) -> OllamaMessage {
    match msg.role {
        Role::ToolCall => OllamaMessage {
            role: "assistant",
            content: String::new(),
            thinking: None,
            tool_calls: Some(vec![OllamaToolCallItem {
                function: OllamaToolCallFunction {
                    name: msg.tool_name.clone().unwrap_or_default(),
                    arguments: msg.tool_args.clone().unwrap_or(serde_json::Value::Null),
                },
            }]),
            tool_call_id: None,
        },
        Role::ToolResult => OllamaMessage {
            role: "tool",
            content: msg.content.clone(),
            thinking: None,
            tool_calls: None,
            tool_call_id: msg.tool_call_id.clone(),
        },
        _ => OllamaMessage {
            role: msg.role.as_str(),
            content: msg.content.clone(),
            thinking: msg.thinking.clone().filter(|t| !t.is_empty()),
            tool_calls: None,
            tool_call_id: None,
        },
    }
}

// ── NDJSON helper ─────────────────────────────────────────────────────────────
//
// Parses an Ollama NDJSON chunk and emits the corresponding LlmEvents.
// Returns `true` when the stream is finished (done=true or error).
fn parse_ndjson_line(
    line: &str,
    events: &mut Vec<LlmEvent>,
    emitted_tool_intent: &mut bool,
) -> bool {
    if line.is_empty() {
        return false;
    }
    match serde_json::from_str::<ChatChunk>(line) {
        Ok(chunk) => {
            if !chunk.message.tool_calls.is_empty() {
                if !*emitted_tool_intent {
                    *emitted_tool_intent = true;
                    events.push(LlmEvent::ToolIntentStart);
                }
                for (i, tc) in chunk.message.tool_calls.iter().enumerate() {
                    events.push(LlmEvent::ToolCall {
                        id: format!("call_{i}"),
                        name: tc.function.name.clone(),
                        args: coerce_arguments(tc.function.arguments.clone()),
                    });
                }
            } else {
                if !chunk.message.thinking.is_empty() {
                    events.push(LlmEvent::ThinkingToken(chunk.message.thinking.clone()));
                }
                if !chunk.message.content.is_empty() {
                    events.push(LlmEvent::Token {
                        text: chunk.message.content.clone(),
                        phase: if *emitted_tool_intent {
                            AssistantPhase::Provisional
                        } else {
                            AssistantPhase::Unknown
                        },
                    });
                }
            }
            if chunk.done {
                if chunk.prompt_eval_count.is_some() || chunk.eval_count.is_some() {
                    events.push(LlmEvent::Usage(UsageStats {
                        input_tokens: chunk.prompt_eval_count,
                        output_tokens: chunk.eval_count,
                        total_tokens: match (chunk.prompt_eval_count, chunk.eval_count) {
                            (Some(i), Some(o)) => Some(i.saturating_add(o)),
                            _ => None,
                        },
                    }));
                }
                events.push(LlmEvent::Done);
                return true;
            }
        }
        Err(e) => {
            events.push(LlmEvent::Error(ProviderError::other(
                "Ollama",
                format!("Parse error: {e}"),
            )));
            return true;
        }
    }
    false
}

// ── Provider implementation ───────────────────────────────────────────────────

impl LlmProvider for OllamaProvider {
    fn stream_chat(&self, messages: Vec<Message>) -> LlmStream {
        let url = format!("{}/api/chat", self.base_url);
        let model = self.model.clone();
        let client = self.client.clone();
        let api_key = self.api_key.clone();

        Box::pin(async_stream::stream! {
            let body = ChatRequest {
                model,
                messages: messages.iter().map(to_ollama_message).collect(),
                stream: true,
            };

            if let Ok(json) = serde_json::to_string_pretty(&body) {
                log::debug!("[TAU_DEBUG] → ollama request:\n{json}");
            }

            let mut req = client.post(&url).json(&body);
            if let Some(key) = &api_key { req = req.bearer_auth(key); }

            let response = match send_streaming_request(req, "Ollama").await {
                Ok(r) => r,
                Err(e) => { yield LlmEvent::Error(e); return; }
            };

            let mut byte_stream = response.bytes_stream();
            let mut buf = String::new();
            let mut line_num = 0usize;
            let mut emitted_tool_intent = false;
            while let Some(chunk) = byte_stream.next().await {
                let bytes = match chunk {
                    Ok(b) => b,
                    Err(e) => { yield LlmEvent::Error(ProviderError::network("Ollama", e.to_string())); return; }
                };
                buf.push_str(&String::from_utf8_lossy(&bytes));
                while let Some(pos) = buf.find('\n') {
                    let line = buf[..pos].trim().to_string();
                    buf.drain(..=pos);
                    if !line.is_empty() {
                        log::debug!("[TAU_DEBUG] ← ollama chunk {line_num}: {line}");
                        line_num += 1;
                    }
                    let mut events = Vec::new();
                    let done = parse_ndjson_line(&line, &mut events, &mut emitted_tool_intent);
                    for ev in events { yield ev; }
                    if done { return; }
                }
            }
            yield LlmEvent::Done;
        })
    }

    fn stream_chat_with_tools(
        &self,
        messages: Vec<Message>,
        tools: Vec<ToolDefinition>,
    ) -> LlmStream {
        let url = format!("{}/api/chat", self.base_url);
        let model = self.model.clone();
        let client = self.client.clone();
        let api_key = self.api_key.clone();

        Box::pin(async_stream::stream! {
            let ollama_tools: Vec<OllamaToolDef> = tools
                .iter()
                .map(|t| OllamaToolDef {
                    r#type: "function",
                    function: OllamaFunctionDef {
                        name: t.name.clone(),
                        description: t.description.clone(),
                        parameters: t.parameters.clone(),
                    },
                })
                .collect();

            let body = ChatRequestWithTools {
                model,
                messages: messages.iter().map(to_ollama_message).collect(),
                tools: ollama_tools,
                stream: true,
            };

            if let Ok(json) = serde_json::to_string_pretty(&body) {
                log::debug!("[TAU_DEBUG] → ollama request:\n{json}");
            }

            let mut req = client.post(&url).json(&body);
            if let Some(key) = &api_key { req = req.bearer_auth(key); }

            let response = match send_streaming_request(req, "Ollama").await {
                Ok(r) => r,
                Err(e) => { yield LlmEvent::Error(e); return; }
            };

            let mut byte_stream = response.bytes_stream();
            let mut buf = String::new();
            let mut line_num = 0usize;
            let mut emitted_tool_intent = false;
            while let Some(chunk) = byte_stream.next().await {
                let bytes = match chunk {
                    Ok(b) => b,
                    Err(e) => { yield LlmEvent::Error(ProviderError::network("Ollama", e.to_string())); return; }
                };
                buf.push_str(&String::from_utf8_lossy(&bytes));
                while let Some(pos) = buf.find('\n') {
                    let line = buf[..pos].trim().to_string();
                    buf.drain(..=pos);
                    if !line.is_empty() {
                        log::debug!("[TAU_DEBUG] ← chunk {line_num}: {line}");
                        line_num += 1;
                    }
                    let mut events = Vec::new();
                    let done = parse_ndjson_line(&line, &mut events, &mut emitted_tool_intent);
                    for ev in events { yield ev; }
                    if done { return; }
                }
            }
        })
    }

    fn list_models(&self) -> ModelListFuture {
        let url = format!("{}/api/tags", self.base_url);
        let client = self.client.clone();
        let api_key = self.api_key.clone();
        Box::pin(async move {
            super::common::fetch_model_list::<TagsResponse, _>(
                &client,
                &url,
                "Ollama",
                api_key.as_deref(),
                &[],
                |r| r.models.into_iter().map(|m| m.name).collect(),
            )
            .await
        })
    }
}
