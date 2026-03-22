//! Shared helpers used by multiple LLM provider implementations.
//!
//! Rather than duplicating scaffolding code (SSE parsing, HTTP error handling,
//! tool-name normalisation, initiator inference) in every provider module, this
//! module provides a single authoritative implementation of each.

use super::{Message, Role};

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
        "✍️" => "write_file",
        "📝" => "edit_file",
        "💻" => "bash",
        "🔍" => "find_files",
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
        Self {
            buf: String::new(),
        }
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

/// Send `req` and, on success, return the [`reqwest::Response`].
///
/// On network failure or a non-2xx HTTP status the function returns an `Err`
/// containing a user-facing error string.  The caller is responsible for
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
) -> Result<reqwest::Response, String> {
    let response = req
        .send()
        .await
        .map_err(|e| format!("Failed to connect to {provider_name}: {e}"))?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        let preview: String = text.chars().take(1000).collect();
        log::warn!("{provider_name} api error: status={status} body={preview}");
        return Err(format!("{provider_name} returned {status}: {text}"));
    }

    log::debug!("← HTTP {} from {provider_name}", response.status());
    Ok(response)
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
        assert_eq!(normalize_tool_name("✍️"), "write_file");
        assert_eq!(normalize_tool_name("📝"), "edit_file");
        assert_eq!(normalize_tool_name("💻"), "bash");
        assert_eq!(normalize_tool_name("🔍"), "find_files");
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
}
