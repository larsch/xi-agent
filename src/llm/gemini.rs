use futures_util::StreamExt;

use super::{
    AssistantPhase, LlmEvent, LlmProvider, LlmStream, Message, ModelListFuture, Role,
    ToolDefinition, UsageStats,
};

pub const DEFAULT_BASE_URL: &str = "https://cloudcode-pa.googleapis.com";

pub struct GeminiProvider {
    base_url: String,
    model: String,
    access_token: String,
    project_id: String,
    thinking_level: Option<GeminiThinkingLevel>,
    client: reqwest::Client,
}

#[derive(Debug, Clone, Copy)]
pub enum GeminiThinkingLevel {
    Minimal,
    Low,
    Medium,
    High,
}

impl GeminiThinkingLevel {
    fn as_api_str(self) -> &'static str {
        match self {
            Self::Minimal => "MINIMAL",
            Self::Low => "LOW",
            Self::Medium => "MEDIUM",
            Self::High => "HIGH",
        }
    }
}

impl GeminiProvider {
    pub fn new(
        base_url: impl Into<String>,
        model: impl Into<String>,
        access_token: impl Into<String>,
        project_id: impl Into<String>,
    ) -> Self {
        Self {
            base_url: base_url.into(),
            model: model.into(),
            access_token: access_token.into(),
            project_id: project_id.into(),
            thinking_level: None,
            client: reqwest::Client::new(),
        }
    }

    pub fn with_thinking_level(mut self, level: Option<GeminiThinkingLevel>) -> Self {
        self.thinking_level = level;
        self
    }

    fn stream_inner(&self, messages: Vec<Message>, tools: Vec<ToolDefinition>) -> LlmStream {
        let url = format!(
            "{}/v1internal:streamGenerateContent?alt=sse",
            self.base_url.trim_end_matches('/')
        );
        let model = self.model.clone();
        let access_token = self.access_token.clone();
        let project_id = self.project_id.clone();
        let thinking_level = self.thinking_level;
        let client = self.client.clone();

        Box::pin(async_stream::stream! {
            let body = build_request(&messages, &tools, &project_id, &model, thinking_level);

            log::debug!(
                "[TAU_DEBUG] → gemini request:\n{}",
                serde_json::to_string_pretty(&body).unwrap_or_default()
            );

            let response = match client
                .post(&url)
                .bearer_auth(&access_token)
                .header("Content-Type", "application/json")
                .header("Accept", "text/event-stream")
                .header("User-Agent", "google-cloud-sdk vscode_cloudshelleditor/0.1")
                .header("X-Goog-Api-Client", "gl-node/22.17.0")
                .header(
                    "Client-Metadata",
                    r#"{"ideType":"IDE_UNSPECIFIED","platform":"PLATFORM_UNSPECIFIED","pluginType":"GEMINI"}"#,
                )
                .json(&body)
                .send()
                .await
                .map_err(|e| format!("Failed to connect to Gemini at {url}: {e}"))
            {
                Ok(r) => r,
                Err(e) => {
                    yield LlmEvent::Error(e);
                    return;
                }
            };

            if !response.status().is_success() {
                let status = response.status();
                let text = response.text().await.unwrap_or_default();
                let preview: String = text.chars().take(1000).collect();
                log::warn!("gemini api error: status={} body={}", status, preview);
                yield LlmEvent::Error(format!("Gemini returned {status}: {text}"));
                return;
            }

            let mut byte_stream = response.bytes_stream();
            let mut buf = String::new();
            let mut emitted_tool_intent = false;

            while let Some(chunk) = byte_stream.next().await {
                let bytes = match chunk {
                    Ok(b) => b,
                    Err(e) => {
                        yield LlmEvent::Error(e.to_string());
                        return;
                    }
                };
                buf.push_str(&String::from_utf8_lossy(&bytes));

                while let Some(pos) = buf.find('\n') {
                    let raw = buf[..pos].trim().to_string();
                    buf.drain(..=pos);

                    if raw.is_empty() || raw.starts_with(':') {
                        continue;
                    }
                    let line = if let Some(rest) = raw.strip_prefix("data:") {
                        rest.trim()
                    } else {
                        continue;
                    };

                    if line.is_empty() {
                        continue;
                    }

                    let chunk: serde_json::Value = match serde_json::from_str(line) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };

                    if let Some(response) = chunk.get("response") {
                        if let Some(usage) = parse_usage(response) {
                            yield LlmEvent::Usage(usage);
                        }

                        let Some(candidate) = response
                            .get("candidates")
                            .and_then(|c| c.as_array())
                            .and_then(|arr| arr.first())
                        else {
                            continue;
                        };

                        if let Some(parts) = candidate
                            .get("content")
                            .and_then(|c| c.get("parts"))
                            .and_then(|p| p.as_array())
                        {
                            for part in parts {
                                if let Some(function_call) = part.get("functionCall") {
                                    if !emitted_tool_intent {
                                        emitted_tool_intent = true;
                                        yield LlmEvent::ToolIntentStart;
                                    }

                                    let name = function_call
                                        .get("name")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or_default()
                                        .to_string();
                                    let id = function_call
                                        .get("id")
                                        .and_then(|v| v.as_str())
                                        .map(ToString::to_string)
                                        .unwrap_or_else(|| format!(
                                            "gemini_call_{}",
                                            std::time::SystemTime::now()
                                                .duration_since(std::time::UNIX_EPOCH)
                                                .map(|d| d.as_millis())
                                                .unwrap_or(0)
                                        ));
                                    let args = function_call
                                        .get("args")
                                        .cloned()
                                        .unwrap_or_else(|| serde_json::json!({}));

                                    yield LlmEvent::ToolCall { id, name, args };
                                }

                                if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                                    if text.is_empty() {
                                        continue;
                                    }
                                    let is_thinking = part
                                        .get("thought")
                                        .and_then(|v| v.as_bool())
                                        .unwrap_or(false);
                                    if is_thinking {
                                        yield LlmEvent::ThinkingToken(text.to_string());
                                    } else {
                                        yield LlmEvent::Token {
                                            text: text.to_string(),
                                            phase: if emitted_tool_intent {
                                                AssistantPhase::Provisional
                                            } else {
                                                AssistantPhase::Unknown
                                            },
                                        };
                                    }
                                }
                            }
                        }
                    }
                }
            }

            yield LlmEvent::Done;
        })
    }
}

fn parse_usage(response: &serde_json::Value) -> Option<UsageStats> {
    let usage = response.get("usageMetadata")?;
    let input = usage
        .get("promptTokenCount")
        .and_then(|v| v.as_u64())
        .and_then(|n| usize::try_from(n).ok());
    let output = usage
        .get("candidatesTokenCount")
        .and_then(|v| v.as_u64())
        .and_then(|n| usize::try_from(n).ok());
    let total = usage
        .get("totalTokenCount")
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

fn build_request(
    messages: &[Message],
    tools: &[ToolDefinition],
    project_id: &str,
    model: &str,
    thinking_level: Option<GeminiThinkingLevel>,
) -> serde_json::Value {
    let system_instruction = messages.iter().find(|m| m.role == Role::System).map(|m| {
        serde_json::json!({
            "parts": [{"text": m.content}],
        })
    });

    let contents = to_gemini_contents(messages);

    let mut request = serde_json::json!({
        "project": project_id,
        "model": model,
        "request": {
            "contents": contents,
        },
        "userAgent": "tau",
        "requestId": format!(
            "tau-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        ),
    });

    if let Some(system) = system_instruction {
        request["request"]["systemInstruction"] = system;
    }

    let is_gemini_model = model.to_ascii_lowercase().starts_with("gemini");
    if let Some(level) = thinking_level
        && is_gemini_model
    {
        let thinking_config = if is_gemini_3_model(model) {
            serde_json::json!({
                "includeThoughts": true,
                "thinkingLevel": level.as_api_str(),
            })
        } else {
            serde_json::json!({
                "includeThoughts": true,
                "thinkingBudget": thinking_budget_for(level),
            })
        };
        request["request"]["generationConfig"] = serde_json::json!({
            "thinkingConfig": thinking_config,
        });
    }

    if !tools.is_empty() {
        request["request"]["tools"] = serde_json::json!([
            {
                "functionDeclarations": tools.iter().map(|t| serde_json::json!({
                    "name": t.name,
                    "description": t.description,
                    "parametersJsonSchema": t.parameters,
                })).collect::<Vec<_>>()
            }
        ]);
        request["request"]["toolConfig"] = serde_json::json!({
            "functionCallingConfig": {
                "mode": "AUTO"
            }
        });
    }

    request
}

fn is_gemini_3_model(model: &str) -> bool {
    let m = model.to_ascii_lowercase();
    m.starts_with("gemini-3")
}

fn thinking_budget_for(level: GeminiThinkingLevel) -> usize {
    match level {
        GeminiThinkingLevel::Minimal => 1024,
        GeminiThinkingLevel::Low => 2048,
        GeminiThinkingLevel::Medium => 8192,
        GeminiThinkingLevel::High => 16384,
    }
}

fn to_gemini_contents(messages: &[Message]) -> Vec<serde_json::Value> {
    let mut contents = Vec::new();
    let mut tool_names_by_id: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();

    for msg in messages {
        match msg.role {
            Role::System => {}
            Role::User => {
                contents.push(serde_json::json!({
                    "role": "user",
                    "parts": [{ "text": msg.content }],
                }));
            }
            Role::Assistant => {
                if msg.content.trim().is_empty() {
                    continue;
                }
                contents.push(serde_json::json!({
                    "role": "model",
                    "parts": [{ "text": msg.content }],
                }));
            }
            Role::ToolCall => {
                let name = msg.tool_name.clone().unwrap_or_default();
                let id = msg
                    .tool_call_id
                    .clone()
                    .unwrap_or_else(|| "call_0".to_string());
                let args = msg
                    .tool_args
                    .clone()
                    .unwrap_or_else(|| serde_json::json!({}));
                tool_names_by_id.insert(id.clone(), name.clone());
                contents.push(serde_json::json!({
                    "role": "model",
                    "parts": [{
                        "functionCall": {
                            "name": name,
                            "id": id,
                            "args": args,
                        }
                    }],
                }));
            }
            Role::ToolResult => {
                let tool_call_id = msg
                    .tool_call_id
                    .clone()
                    .unwrap_or_else(|| "call_0".to_string());
                let tool_name = msg
                    .tool_name
                    .clone()
                    .or_else(|| tool_names_by_id.get(&tool_call_id).cloned())
                    .unwrap_or_else(|| "tool".to_string());
                contents.push(serde_json::json!({
                    "role": "user",
                    "parts": [{
                        "functionResponse": {
                            "name": tool_name,
                            "id": tool_call_id,
                            "response": if msg.is_error {
                                serde_json::json!({"error": msg.content})
                            } else {
                                serde_json::json!({"output": msg.content})
                            },
                        }
                    }],
                }));
            }
        }
    }

    contents
}

impl LlmProvider for GeminiProvider {
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
        let model = self.model.clone();
        Box::pin(async move {
            Ok(vec![
                model,
                "gemini-2.5-pro".to_string(),
                "gemini-2.5-flash".to_string(),
                "gemini-2.0-flash".to_string(),
                "claude-sonnet-4.5".to_string(),
            ])
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{GeminiThinkingLevel, build_request, to_gemini_contents};
    use crate::llm::{Message, ToolDefinition};

    #[test]
    fn build_request_uses_thinking_budget_for_gemini_2_models() {
        let messages = vec![Message::system("rules"), Message::user("hello")];
        let req = build_request(
            &messages,
            &[],
            "proj-1",
            "gemini-2.5-pro",
            Some(GeminiThinkingLevel::Low),
        );
        assert_eq!(
            req["request"]["generationConfig"]["thinkingConfig"]["thinkingBudget"],
            2048
        );
    }

    #[test]
    fn build_request_uses_thinking_level_for_gemini_3_models() {
        let messages = vec![Message::system("rules"), Message::user("hello")];
        let req = build_request(
            &messages,
            &[],
            "proj-1",
            "gemini-3-pro",
            Some(GeminiThinkingLevel::Low),
        );
        assert_eq!(
            req["request"]["generationConfig"]["thinkingConfig"]["thinkingLevel"],
            "LOW"
        );
    }

    #[test]
    fn build_request_includes_tool_schema() {
        let messages = vec![Message::user("hi")];
        let tools = vec![ToolDefinition {
            name: "read_file".to_string(),
            description: "Read".to_string(),
            parameters: serde_json::json!({"type":"object"}),
        }];
        let req = build_request(&messages, &tools, "proj-1", "gemini-2.5-pro", None);
        assert_eq!(
            req["request"]["tools"][0]["functionDeclarations"][0]["name"],
            "read_file"
        );
    }

    #[test]
    fn tool_result_uses_preceding_tool_call_name() {
        let messages = vec![
            Message::tool_call("call_1", "read_file", serde_json::json!({"path":"a.txt"})),
            Message::tool_result("call_1", "ok", false),
        ];
        let contents = to_gemini_contents(&messages);
        assert_eq!(
            contents[1]["parts"][0]["functionResponse"]["name"],
            "read_file"
        );
    }
}
