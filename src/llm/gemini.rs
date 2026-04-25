use crate::thinking::GeminiThinkingLevel;
use serde::Deserialize;

use super::{
    AssistantPhase, LlmEvent, LlmProvider, LlmStream, Message, ModelListFuture, ProviderError,
    Role, ToolDefinition, UsageStats,
    common::{StreamControl, build_http_client, stream_sse_lines},
    provider_format::to_gemini_wire,
};

// ── Typed response structs ────────────────────────────────────────────────────

#[derive(Deserialize)]
struct GeminiStreamChunk {
    response: Option<GeminiResponse>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiResponse {
    candidates: Option<Vec<GeminiCandidate>>,
    usage_metadata: Option<GeminiUsage>,
}

#[derive(Deserialize)]
struct GeminiCandidate {
    content: Option<GeminiContent>,
}

#[derive(Deserialize)]
struct GeminiContent {
    parts: Option<Vec<GeminiPart>>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum GeminiPart {
    FunctionCall {
        #[serde(rename = "functionCall")]
        function_call: GeminiFunctionCall,
    },
    Text {
        text: String,
        #[serde(default)]
        thought: bool,
    },
}

#[derive(Deserialize)]
struct GeminiFunctionCall {
    name: String,
    id: Option<String>,
    args: Option<serde_json::Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiUsage {
    prompt_token_count: Option<usize>,
    candidates_token_count: Option<usize>,
    total_token_count: Option<usize>,
}

impl From<GeminiUsage> for UsageStats {
    fn from(u: GeminiUsage) -> Self {
        Self {
            input_tokens: u.prompt_token_count,
            output_tokens: u.candidates_token_count,
            total_tokens: u.total_token_count,
        }
    }
}

const MAX_RETRIES: u32 = 3;
/// For server-directed 429s (where we parsed an explicit delay), allow more
/// attempts — the server is telling us exactly when to retry.
const MAX_SERVER_DIRECTED_RETRIES: u32 = 8;
const BASE_DELAY_MS: u64 = 1000;
/// Maximum server-requested delay we will honour (ms). Above this we give up.
const MAX_RETRY_DELAY_MS: u64 = 60_000;

/// Parse the server-requested retry delay out of a 429 response body (ms).
///
/// Matches the same patterns as pi-mono's `extractRetryDelay`:
/// - `"Your quota will reset after 7s"` / `"...18h31m10s"` / `"...10m15s"`
/// - `"Please retry in 5s"` / `"Please retry in 500ms"`
/// - `"retryDelay": "34.074s"` (JSON error details field)
fn extract_retry_delay_ms(body: &str, headers: &reqwest::header::HeaderMap) -> Option<u64> {
    // 1. Retry-After header (seconds)
    if let Some(val) = headers.get("retry-after").and_then(|v| v.to_str().ok())
        && let Ok(secs) = val.parse::<f64>()
        && secs > 0.0
    {
        return Some((secs * 1000.0).ceil() as u64 + 1000);
    }

    // 2. x-ratelimit-reset-after header (seconds)
    if let Some(val) = headers
        .get("x-ratelimit-reset-after")
        .and_then(|v| v.to_str().ok())
        && let Ok(secs) = val.parse::<f64>()
        && secs > 0.0
    {
        return Some((secs * 1000.0).ceil() as u64 + 1000);
    }

    // Body pattern helpers
    let lower = body.to_ascii_lowercase();

    // 3. "quota will reset after [Xh][Ym]Zs"
    if let Some(ms) = parse_duration_after(&lower, "reset after ") {
        return Some(ms + 1000);
    }

    // 4. "Please retry in X[ms|s]"
    if let Some(after) = find_after(&lower, "please retry in ")
        && let Some(ms) = parse_time_value(after)
    {
        return Some(ms + 1000);
    }

    // 5. "retryDelay": "34.074s"
    if let Some(after) = find_after(body, "\"retryDelay\":") {
        let after = after.trim().trim_start_matches('"');
        if let Some(ms) = parse_time_value(after) {
            return Some(ms + 1000);
        }
    }

    None
}

/// Find the text that immediately follows `needle` in `haystack`.
fn find_after<'a>(haystack: &'a str, needle: &str) -> Option<&'a str> {
    haystack
        .find(needle)
        .map(|pos| &haystack[pos + needle.len()..])
}

/// Parse a duration like `"18h31m10s"` or `"7s"` or `"10m15s"` from the text
/// immediately following `prefix`. Returns milliseconds, or `None` if no valid
/// duration is found. Uses only the digits/unit at the start of the matched
/// substring so stray letters in the surrounding text don't corrupt the parse.
fn parse_duration_after(text: &str, prefix: &str) -> Option<u64> {
    let mut s = find_after(text, prefix)?;
    let mut hours: f64 = 0.0;
    let mut mins: f64 = 0.0;
    let mut secs: f64 = 0.0;
    let mut found_any = false;

    // Each unit: consume leading digits (and optional '.'), then the unit char.
    // If the unit isn't found we stop — don't scan further into unrelated text.
    if let Some(h_pos) = s.find('h') {
        // Only accept if everything before 'h' is a valid number
        if let Ok(v) = s[..h_pos].trim().parse::<f64>() {
            hours = v;
            found_any = true;
            s = &s[h_pos + 1..];
        }
    }
    if let Some(m_pos) = s.find('m') {
        // Exclude "ms" — that's milliseconds, not minutes
        if s.as_bytes().get(m_pos + 1) != Some(&b's')
            && let Ok(v) = s[..m_pos].trim().parse::<f64>()
        {
            mins = v;
            found_any = true;
            s = &s[m_pos + 1..];
        }
    }
    // Seconds: find first 's' that follows a digit run
    if let Some(s_pos) = s.find('s')
        && let Ok(v) = s[..s_pos].trim().parse::<f64>()
    {
        secs = v;
        found_any = true;
    }

    if !found_any {
        return None;
    }

    let total = hours * 3_600_000.0 + mins * 60_000.0 + secs * 1000.0;
    if total > 0.0 {
        Some(total.ceil() as u64)
    } else {
        None
    }
}

/// Parse a value like `"5s"` or `"500ms"` from the start of `text`, ignoring
/// any trailing punctuation/whitespace after the unit. Returns milliseconds.
fn parse_time_value(text: &str) -> Option<u64> {
    let text = text.trim();
    // Find where the numeric part ends (digits and at most one '.')
    let num_end = text
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(text.len());
    if num_end == 0 {
        return None;
    }
    let num_str = &text[..num_end];
    let unit_start = &text[num_end..];

    let v: f64 = num_str.trim().parse().ok()?;
    if v <= 0.0 {
        return None;
    }

    if unit_start.starts_with("ms") {
        Some(v.ceil() as u64)
    } else if unit_start.starts_with('s') {
        Some((v * 1000.0).ceil() as u64)
    } else {
        None
    }
}

fn is_retryable(status: u16) -> bool {
    matches!(status, 429 | 500 | 502 | 503 | 504)
}

pub const DEFAULT_BASE_URL: &str = "https://cloudcode-pa.googleapis.com";

pub struct GeminiProvider {
    base_url: String,
    model: String,
    access_token: String,
    project_id: String,
    thinking_level: Option<GeminiThinkingLevel>,
    client: reqwest::Client,
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
            client: build_http_client(),
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

            // Retry loop: honours server-provided delay on 429, exponential
            // backoff on other retryable errors, up to MAX_RETRIES attempts.
            let response = 'retry: {
                let mut last_err: Option<ProviderError> = None;
                for attempt in 0..=MAX_SERVER_DIRECTED_RETRIES {
                    let req = client
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
                        .json(&body);

                    let raw = match req.send().await {
                        Ok(r) => r,
                        Err(e) => {
                            let err = ProviderError::network("Gemini", format!("Failed to connect: {e}"));
                            if attempt < MAX_RETRIES {
                                let delay = BASE_DELAY_MS * 2u64.pow(attempt.min(6));
                                log::debug!("Gemini network error (attempt {attempt}), retrying in {delay}ms: {e}");
                                tokio::time::sleep(tokio::time::Duration::from_millis(delay)).await;
                                last_err = Some(err);
                                continue;
                            }
                            break 'retry Err(err);
                        }
                    };

                    if raw.status().is_success() {
                        break 'retry Ok(raw);
                    }

                    let status = raw.status().as_u16();
                    let headers = raw.headers().clone();
                    let body_text = raw.text().await.unwrap_or_default();
                    let preview: String = body_text.chars().take(1000).collect();
                    log::warn!("Gemini api error: status={status} body={preview}");

                    if is_retryable(status) {
                        let server_delay = extract_retry_delay_ms(&body_text, &headers);

                        if server_delay.is_some_and(|d| d > MAX_RETRY_DELAY_MS) {
                            let secs = server_delay.unwrap() / 1000;
                            break 'retry Err(ProviderError::rate_limited(
                                "Gemini",
                                format!("Rate limited; server requests {secs}s retry delay (exceeds limit). {body_text}"),
                            ));
                        }

                        let max_attempts = if server_delay.is_some() {
                            MAX_SERVER_DIRECTED_RETRIES
                        } else {
                            MAX_RETRIES
                        };

                        if attempt < max_attempts {
                            let delay = server_delay.unwrap_or(BASE_DELAY_MS * 2u64.pow(attempt));
                            log::debug!("Gemini {status} (attempt {attempt}), retrying in {delay}ms");

                            // Countdown: emit a StatusUpdate each second so
                            // the UI shows a live decrementing counter.
                            let mut elapsed_ms: u64 = 0;
                            while elapsed_ms < delay {
                                let remaining_ms = delay - elapsed_ms;
                                let remaining_secs = remaining_ms.div_ceil(1000);
                                yield LlmEvent::StatusUpdate(
                                    format!("Rate limited — retrying in {remaining_secs}s…")
                                );
                                let tick_ms = remaining_ms.min(1000);
                                tokio::time::sleep(
                                    tokio::time::Duration::from_millis(tick_ms)
                                ).await;
                                elapsed_ms += tick_ms;
                            }

                            last_err = Some(super::common::map_http_error("Gemini", reqwest::StatusCode::from_u16(status).unwrap(), body_text));
                            yield LlmEvent::StatusUpdate(String::new());
                            continue;
                        }
                    }

                    break 'retry Err(super::common::map_http_error(
                        "Gemini",
                        reqwest::StatusCode::from_u16(status).unwrap(),
                        body_text,
                    ));
                }
                Err(last_err.unwrap_or_else(|| ProviderError::network("Gemini", "failed after retries")))
            };

            let response = match response {
                Ok(r) => r,
                Err(e) => { yield LlmEvent::Error(e); return; }
            };

            let mut emitted_tool_intent = false;

            let mut stream = stream_sse_lines("Gemini", response, move |line, events| {
                let chunk: GeminiStreamChunk = match serde_json::from_str(line) {
                    Ok(v) => v,
                    Err(_) => return StreamControl::Continue,
                };

                let Some(response) = chunk.response else { return StreamControl::Continue };

                if let Some(usage) = response.usage_metadata {
                    let stats = UsageStats::from(usage);
                    if stats.input_tokens.is_some()
                        || stats.output_tokens.is_some()
                        || stats.total_tokens.is_some()
                    {
                        events.push(LlmEvent::Usage(stats));
                    }
                }

                let Some(candidate) = response.candidates.as_deref().and_then(|c| c.first())
                else {
                    return StreamControl::Continue;
                };

                let parts = candidate
                    .content
                    .as_ref()
                    .and_then(|c| c.parts.as_deref())
                    .unwrap_or(&[]);

                for part in parts {
                    match part {
                        GeminiPart::FunctionCall { function_call } => {
                            if !emitted_tool_intent {
                                emitted_tool_intent = true;
                                events.push(LlmEvent::ToolIntentStart);
                            }
                            let id = function_call
                                .id
                                .clone()
                                .unwrap_or_else(|| format!(
                                    "gemini_call_{}",
                                    std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .map(|d| d.as_millis())
                                        .unwrap_or(0)
                                ));
                            let args = function_call
                                .args
                                .clone()
                                .unwrap_or_else(|| serde_json::json!({}));
                            events.push(LlmEvent::ToolCall {
                                id,
                                name: function_call.name.clone(),
                                args,
                            });
                        }
                        GeminiPart::Text { text, thought } => {
                            if text.is_empty() {
                                continue;
                            }
                            if *thought {
                                events.push(LlmEvent::ThinkingToken(text.clone()));
                            } else {
                                events.push(LlmEvent::Token {
                                    text: text.clone(),
                                    phase: if emitted_tool_intent {
                                        AssistantPhase::Provisional
                                    } else {
                                        AssistantPhase::Unknown
                                    },
                                });
                            }
                        }
                    }
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

    let contents = to_gemini_wire(messages);

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
            let mut models = vec![
                "gemini-2.0-flash".to_string(),
                "gemini-2.5-flash".to_string(),
                "gemini-2.5-pro".to_string(),
                "gemini-3-flash-preview".to_string(),
                "gemini-3-pro-preview".to_string(),
                "gemini-3.1-pro-preview".to_string(),
            ];

            if !models.iter().any(|m| m == &model) {
                models.insert(0, model);
            }

            Ok(models)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        GeminiPart, GeminiStreamChunk, GeminiUsage, build_request, extract_retry_delay_ms,
    };
    use crate::llm::provider_format::to_gemini_wire;
    use crate::llm::{Message, ToolDefinition, UsageStats};
    use crate::thinking::GeminiThinkingLevel;

    fn parse_chunk(json: &str) -> GeminiStreamChunk {
        serde_json::from_str(json).expect("parse failed")
    }

    // ── New typed-parsing tests ───────────────────────────────────────────────

    #[test]
    fn text_part_parses_correctly() {
        let chunk = parse_chunk(
            r#"{"response":{"candidates":[{"content":{"parts":[{"text":"hello"}]}}]}}"#,
        );
        let parts = chunk.response.unwrap().candidates.unwrap();
        let part = &parts[0].content.as_ref().unwrap().parts.as_ref().unwrap()[0];
        let GeminiPart::Text { text, thought } = part else {
            panic!("expected Text")
        };
        assert_eq!(text, "hello");
        assert!(!thought);
    }

    #[test]
    fn thought_part_parses_correctly() {
        let chunk = parse_chunk(
            r#"{"response":{"candidates":[{"content":{"parts":[{"text":"reasoning","thought":true}]}}]}}"#,
        );
        let parts = chunk.response.unwrap().candidates.unwrap();
        let part = &parts[0].content.as_ref().unwrap().parts.as_ref().unwrap()[0];
        let GeminiPart::Text { text, thought } = part else {
            panic!("expected Text")
        };
        assert_eq!(text, "reasoning");
        assert!(thought);
    }

    #[test]
    fn function_call_part_parses_correctly() {
        let chunk = parse_chunk(
            r#"{"response":{"candidates":[{"content":{"parts":[{"functionCall":{"name":"read_file","id":"fc_1","args":{"path":"a.txt"}}}]}}]}}"#,
        );
        let parts = chunk.response.unwrap().candidates.unwrap();
        let part = &parts[0].content.as_ref().unwrap().parts.as_ref().unwrap()[0];
        let GeminiPart::FunctionCall { function_call } = part else {
            panic!("expected FunctionCall")
        };
        assert_eq!(function_call.name, "read_file");
        assert_eq!(function_call.id.as_deref(), Some("fc_1"));
        assert_eq!(function_call.args.as_ref().unwrap()["path"], "a.txt");
    }

    #[test]
    fn function_call_without_id_parses_correctly() {
        let chunk = parse_chunk(
            r#"{"response":{"candidates":[{"content":{"parts":[{"functionCall":{"name":"list_dir","args":{}}}]}}]}}"#,
        );
        let parts = chunk.response.unwrap().candidates.unwrap();
        let part = &parts[0].content.as_ref().unwrap().parts.as_ref().unwrap()[0];
        let GeminiPart::FunctionCall { function_call } = part else {
            panic!()
        };
        assert_eq!(function_call.name, "list_dir");
        assert!(function_call.id.is_none());
    }

    #[test]
    fn usage_metadata_parses_correctly() {
        let chunk = parse_chunk(
            r#"{"response":{"usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":20,"totalTokenCount":30}}}"#,
        );
        let usage: GeminiUsage = chunk.response.unwrap().usage_metadata.unwrap();
        let stats = UsageStats::from(usage);
        assert_eq!(stats.input_tokens, Some(10));
        assert_eq!(stats.output_tokens, Some(20));
        assert_eq!(stats.total_tokens, Some(30));
    }

    #[test]
    fn chunk_without_response_parses_cleanly() {
        let chunk = parse_chunk(r#"{}"#);
        assert!(chunk.response.is_none());
    }

    #[test]
    fn extract_retry_delay_parses_quota_reset_message() {
        // Actual Gemini 429 body — delay is buried inside the JSON "message" field.
        let body = r#"{
  "error": {
    "code": 429,
    "message": "You have exhausted your capacity on this model. Your quota will reset after 39s.",
    "status": "RESOURCE_EXHAUSTED"
  }
}"#;
        let headers = reqwest::header::HeaderMap::new();
        let delay = extract_retry_delay_ms(body, &headers).expect("should parse delay");
        // 39s + 1s buffer = 40_000 ms
        assert_eq!(delay, 40_000);
    }

    #[test]
    fn extract_retry_delay_parses_compound_duration() {
        let body = "quota will reset after 1h2m30s please wait";
        let headers = reqwest::header::HeaderMap::new();
        let delay = extract_retry_delay_ms(body, &headers).expect("should parse delay");
        // (3600 + 120 + 30) * 1000 + 1000 buffer = 3_751_000
        assert_eq!(delay, 3_751_000);
    }

    #[test]
    fn extract_retry_delay_parses_please_retry_in() {
        let body = "Please retry in 5s";
        let headers = reqwest::header::HeaderMap::new();
        let delay = extract_retry_delay_ms(body, &headers).expect("should parse delay");
        assert_eq!(delay, 6_000);
    }

    #[test]
    fn extract_retry_delay_returns_none_when_no_hint() {
        let body = r#"{"error": {"code": 429, "message": "Too many requests"}}"#;
        let headers = reqwest::header::HeaderMap::new();
        assert!(extract_retry_delay_ms(body, &headers).is_none());
    }

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
        let contents = to_gemini_wire(&messages);
        assert_eq!(
            contents[1]["parts"][0]["functionResponse"]["name"],
            "read_file"
        );
    }
}
