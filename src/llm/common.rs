//! Shared helpers used by multiple LLM provider implementations.
//!
//! Rather than duplicating scaffolding code (SSE parsing, HTTP error handling,
//! tool-name normalisation, initiator inference) in every provider module, this
//! module provides a single authoritative implementation of each.

use std::time::Duration;

use super::{Message, Role};

// ── HTTP client factory ───────────────────────────────────────────────────────

/// The maximum time allowed for the TCP connection to be established.
///
/// Once connected, long-running streaming responses are not affected — only
/// the initial handshake is bounded.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// Build a shared [`reqwest::Client`] with a sensible connect timeout.
///
/// All LLM providers should use this instead of [`reqwest::Client::new()`] so
/// that a stalled or unreachable endpoint surfaces as an error rather than
/// hanging silently forever.
pub fn build_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .build()
        .expect("failed to build HTTP client")
}

// ── Initiator inference ───────────────────────────────────────────────────────

/// Returns `"user"` when the last message is from a user (or the history is
/// empty), and `"agent"` otherwise.  Used by providers that support an
/// `X-Initiator` hint header.
pub fn infer_initiator(messages: &[Message]) -> &'static str {
    match messages.last().map(|m| &m.role) {
        Some(Role::User) | None => "user",
        _ => "agent",
    }
}

// ── Tool-name normalisation ───────────────────────────────────────────────────

/// Map emoji shorthand tool names to their canonical ASCII names.
/// Returns the name unchanged for names that are already ASCII identifiers.
pub fn normalize_tool_name(name: &str) -> &str {
    match name {
        "👀" => "read_file",
        "✏️" | "✍️" => "write_file",
        "📝" => "edit_file",
        "💻" => "bash",
        "🔍" => "find_files",
        "🧑" | "❓" => "ask_user",
        other => other,
    }
}

// ── SSE line decoder ──────────────────────────────────────────────────────────

/// Stateful SSE line extractor.
///
/// Call [`push_bytes`] whenever new bytes arrive from the network, then call
/// [`next_data_line`] in a loop until it returns `None` to drain all complete
/// SSE `data:` lines from the buffer.
///
/// The returned strings are:
/// - Already stripped of the `"data: "` / `"data:"` prefix.
/// - Trimmed of leading/trailing ASCII whitespace.
/// - Never blank lines or SSE comment lines (`:…`).
/// - The literal string `"[DONE]"` when the stream signals completion.
///
/// Both `\n` and `\r\n` line endings are handled.
pub struct SseLineDecoder {
    buf: String,
}

impl SseLineDecoder {
    pub fn new() -> Self {
        Self { buf: String::new() }
    }

    /// Append raw bytes from the network into the internal buffer.
    pub fn push_bytes(&mut self, bytes: &[u8]) {
        self.buf.push_str(&String::from_utf8_lossy(bytes));
    }

    /// Return the next complete, non-empty SSE data line, or `None` if the
    /// buffer does not yet contain a full line.
    pub fn next_data_line(&mut self) -> Option<String> {
        loop {
            let pos = self.buf.find('\n')?;
            // Extract the raw line, stripping the newline and any preceding \r.
            let raw = self.buf[..pos].trim_end_matches('\r').trim().to_string();
            self.buf.drain(..=pos);

            // Skip blank lines and SSE comment lines.
            if raw.is_empty() || raw.starts_with(':') {
                continue;
            }

            // Strip the mandatory "data:" prefix; skip non-data SSE fields.
            let data = if let Some(rest) = raw.strip_prefix("data:") {
                rest.trim()
            } else {
                // event:, id:, retry: — ignore
                continue;
            };

            return Some(data.to_string());
        }
    }
}

impl Default for SseLineDecoder {
    fn default() -> Self {
        Self::new()
    }
}

// ── HTTP streaming request helper ─────────────────────────────────────────────

/// Map an HTTP status code and response body to a typed `ProviderError`.
///
/// - 401 → `Unauthorized` (triggers token refresh)
/// - 403 → `Forbidden` (authenticated but not allowed; does not trigger refresh)
/// - 429 → `RateLimited`
/// - 5xx → `ServerError`
/// - other non-2xx → `Other`
///
/// Public for use in provider `list_models()` implementations.
pub fn map_http_error(
    provider: &str,
    status: reqwest::StatusCode,
    body: String,
) -> super::ProviderError {
    use super::{ProviderError, ProviderErrorKind};

    match status.as_u16() {
        401 => ProviderError::unauthorized(provider, body),
        403 => ProviderError::forbidden(provider, body),
        429 => ProviderError::rate_limited(provider, body),
        500..=599 => ProviderError::server_error(provider, status.as_u16(), body),
        _ => ProviderError {
            kind: ProviderErrorKind::Other,
            status_code: Some(status.as_u16()),
            source: provider.to_string(),
            message: body,
        },
    }
}

/// Send `req` and, on success, return the [`reqwest::Response`].
///
/// On network failure or a non-2xx HTTP status the function returns an `Err`
/// containing a typed [`ProviderError`].  The caller is responsible for
/// translating that into an [`LlmEvent::Error`] if needed.
///
/// `provider_name` is only used in log and error messages.
///
/// # Example
///
/// ```ignore
/// let response = match send_streaming_request(req, "openai").await {
///     Ok(r) => r,
///     Err(e) => { yield LlmEvent::Error(e); return; }
/// };
/// let mut byte_stream = response.bytes_stream();
/// ```
pub async fn send_streaming_request(
    req: reqwest::RequestBuilder,
    provider_name: &str,
) -> Result<reqwest::Response, super::ProviderError> {
    use super::ProviderError;

    let response = req
        .send()
        .await
        .map_err(|e| ProviderError::network(provider_name, format!("Failed to connect: {e}")))?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        let preview: String = text.chars().take(1000).collect();
        log::warn!("{provider_name} api error: status={status} body={preview}");
        return Err(map_http_error(provider_name, status, text));
    }

    log::debug!("← HTTP {} from {provider_name}", response.status());
    Ok(response)
}

// ── Model-list fetch helper ───────────────────────────────────────────────────

/// Fetch a JSON endpoint that lists available models, parse the response with
/// `extract`, sort the resulting names, and return them.
///
/// Handles: request construction, optional bearer auth, extra headers,
/// `send()` error, non-2xx status (via [`map_http_error`]), JSON parse
/// error, debug/warn logging.  The caller supplies a closure that turns the
/// parsed response body into a `Vec<String>` of model names.
///
/// # Example
///
/// ```ignore
/// fetch_model_list::<ModelsResponse, _>(
///     &self.client,
///     &url,
///     "OpenAI",
///     Some(&self.api_key),
///     &self.extra_headers,
///     |r| r.data.into_iter().map(|m| m.id).collect(),
/// ).await
/// ```
pub async fn fetch_model_list<T, F>(
    client: &reqwest::Client,
    url: &str,
    provider_name: &str,
    bearer_token: Option<&str>,
    extra_headers: &[(String, String)],
    extract: F,
) -> Result<Vec<String>, super::ProviderError>
where
    T: serde::de::DeserializeOwned,
    F: FnOnce(T) -> Vec<String>,
{
    use super::ProviderError;

    let mut req = client.get(url);
    if let Some(token) = bearer_token {
        req = req.bearer_auth(token);
    }
    for (k, v) in extra_headers {
        req = req.header(k.as_str(), v.as_str());
    }

    log::debug!("→ GET {url}");

    let response = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            log::warn!("{provider_name} list_models error: {e}");
            return Err(ProviderError::network(
                provider_name,
                format!("request failed: {e}"),
            ));
        }
    };

    let status = response.status();
    log::debug!("← HTTP {status} from {provider_name} list_models");

    if !status.is_success() {
        let text = response.text().await.unwrap_or_default();
        let preview: String = text.chars().take(500).collect();
        log::warn!("{provider_name} list_models failed: status={status} body={preview}");
        return Err(map_http_error(provider_name, status, text));
    }

    let parsed: T = match response.json().await {
        Ok(v) => v,
        Err(e) => {
            log::warn!("{provider_name} list_models parse error: {e}");
            return Err(ProviderError::other(
                provider_name,
                format!("failed to parse response: {e}"),
            ));
        }
    };

    let mut ids = extract(parsed);
    ids.sort();
    Ok(ids)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::Message;

    // ── infer_initiator ───────────────────────────────────────────────────────

    #[test]
    fn infer_initiator_empty_history_is_user() {
        assert_eq!(infer_initiator(&[]), "user");
    }

    #[test]
    fn infer_initiator_last_user_is_user() {
        assert_eq!(infer_initiator(&[Message::user("hi")]), "user");
    }

    #[test]
    fn infer_initiator_last_assistant_is_agent() {
        assert_eq!(infer_initiator(&[Message::assistant("ok")]), "agent");
    }

    #[test]
    fn infer_initiator_last_system_is_agent() {
        assert_eq!(infer_initiator(&[Message::system("rules")]), "agent");
    }

    // ── normalize_tool_name ───────────────────────────────────────────────────

    #[test]
    fn normalize_tool_name_emoji_aliases() {
        assert_eq!(normalize_tool_name("👀"), "read_file");
        assert_eq!(normalize_tool_name("✏️"), "write_file");
        assert_eq!(normalize_tool_name("✍️"), "write_file");
        assert_eq!(normalize_tool_name("📝"), "edit_file");
        assert_eq!(normalize_tool_name("💻"), "bash");
        assert_eq!(normalize_tool_name("🔍"), "find_files");
        assert_eq!(normalize_tool_name("🧑"), "ask_user");
        assert_eq!(normalize_tool_name("❓"), "ask_user");
    }

    #[test]
    fn normalize_tool_name_ascii_passthrough() {
        assert_eq!(normalize_tool_name("custom_tool"), "custom_tool");
        assert_eq!(normalize_tool_name("bash"), "bash");
    }

    // ── SseLineDecoder ────────────────────────────────────────────────────────

    fn decode_all(input: &str) -> Vec<String> {
        let mut dec = SseLineDecoder::new();
        dec.push_bytes(input.as_bytes());
        let mut out = Vec::new();
        while let Some(line) = dec.next_data_line() {
            out.push(line);
        }
        out
    }

    #[test]
    fn sse_extracts_data_lines() {
        let lines = decode_all("data: hello\ndata: world\n");
        assert_eq!(lines, ["hello", "world"]);
    }

    #[test]
    fn sse_skips_blank_lines() {
        let lines = decode_all("data: a\n\ndata: b\n");
        assert_eq!(lines, ["a", "b"]);
    }

    #[test]
    fn sse_skips_comment_lines() {
        let lines = decode_all(": keep-alive\ndata: hello\n");
        assert_eq!(lines, ["hello"]);
    }

    #[test]
    fn sse_skips_non_data_fields() {
        let lines = decode_all("event: message\ndata: payload\nid: 1\n");
        assert_eq!(lines, ["payload"]);
    }

    #[test]
    fn sse_passes_done_sentinel() {
        let lines = decode_all("data: [DONE]\n");
        assert_eq!(lines, ["[DONE]"]);
    }

    #[test]
    fn sse_handles_crlf_line_endings() {
        let lines = decode_all("data: hello\r\ndata: world\r\n");
        assert_eq!(lines, ["hello", "world"]);
    }

    #[test]
    fn sse_handles_data_prefix_without_space() {
        // Some providers emit "data:{...}" without a space.
        let lines = decode_all("data:{\"key\":\"val\"}\n");
        assert_eq!(lines, ["{\"key\":\"val\"}"]);
    }

    #[test]
    fn sse_partial_line_across_two_pushes() {
        let mut dec = SseLineDecoder::new();
        dec.push_bytes(b"data: hel");
        assert!(dec.next_data_line().is_none(), "no complete line yet");
        dec.push_bytes(b"lo\n");
        assert_eq!(dec.next_data_line(), Some("hello".to_string()));
    }

    #[test]
    fn sse_multiple_lines_in_one_push() {
        let mut dec = SseLineDecoder::new();
        dec.push_bytes(b"data: first\ndata: second\ndata: third\n");
        assert_eq!(dec.next_data_line(), Some("first".to_string()));
        assert_eq!(dec.next_data_line(), Some("second".to_string()));
        assert_eq!(dec.next_data_line(), Some("third".to_string()));
        assert!(dec.next_data_line().is_none());
    }

    #[test]
    fn sse_returns_none_when_buffer_empty() {
        let mut dec = SseLineDecoder::new();
        assert!(dec.next_data_line().is_none());
    }

    // ── map_http_error ────────────────────────────────────────────────────────

    #[test]
    fn http_401_maps_to_unauthorized() {
        let err = map_http_error(
            "test",
            reqwest::StatusCode::UNAUTHORIZED,
            "bad token".into(),
        );
        assert_eq!(err.kind, crate::llm::ProviderErrorKind::Unauthorized);
        assert_eq!(err.status_code, Some(401));
        assert_eq!(err.message, "bad token");
    }

    #[test]
    fn http_403_maps_to_forbidden() {
        let err = map_http_error("test", reqwest::StatusCode::FORBIDDEN, "quota".into());
        assert_eq!(err.kind, crate::llm::ProviderErrorKind::Forbidden);
        assert_eq!(err.status_code, Some(403));
    }

    #[test]
    fn http_429_maps_to_rate_limited() {
        let err = map_http_error(
            "test",
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            "slow down".into(),
        );
        assert_eq!(err.kind, crate::llm::ProviderErrorKind::RateLimited);
        assert_eq!(err.status_code, Some(429));
    }

    #[test]
    fn http_503_maps_to_server_error() {
        let err = map_http_error(
            "test",
            reqwest::StatusCode::SERVICE_UNAVAILABLE,
            "overload".into(),
        );
        assert_eq!(err.kind, crate::llm::ProviderErrorKind::ServerError);
        assert_eq!(err.status_code, Some(503));
    }

    #[test]
    fn http_418_maps_to_other() {
        let err = map_http_error("test", reqwest::StatusCode::IM_A_TEAPOT, "teapot".into());
        assert_eq!(err.kind, crate::llm::ProviderErrorKind::Other);
        assert_eq!(err.status_code, Some(418));
    }
}
