use std::{future::Future, pin::Pin};

pub mod error;
pub use error::{ProviderError, ProviderErrorKind};

/// Binary image payload attached to a [`Message`].
///
/// Used when a tool result (e.g. `read_file`) returns an image rather than
/// text.  The `content` field of the message carries a human-readable
/// placeholder; providers that support vision encode the image from here.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ImageData {
    /// Base64-encoded image bytes (standard alphabet, no line breaks).
    pub base64: String,
    /// MIME type, e.g. `"image/png"`.
    pub mime_type: String,
}

/// Line-range metadata for a partially-shown file read result.
///
/// Stored on [`Message`] when the corresponding `read_file` tool call only
/// returned a window of the file (because `offset`/`limit` were used, or
/// because the file exceeded the output cap).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DisplayRange {
    /// 1-indexed first line that was returned.
    pub first_line: usize,
    /// 1-indexed last line that was returned (inclusive).
    pub last_line: usize,
    /// Total number of lines in the file.
    pub total_lines: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum AssistantPhase {
    #[default]
    Unknown,
    Provisional,
    Final,
}

/// Normalized token usage reported by a provider for a completed turn.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UsageStats {
    pub input_tokens: Option<usize>,
    pub output_tokens: Option<usize>,
    pub total_tokens: Option<usize>,
}

impl UsageStats {
    /// Best-effort count of used tokens for utilization display.
    pub fn used_tokens(&self) -> Option<usize> {
        self.total_tokens
            .or_else(|| match (self.input_tokens, self.output_tokens) {
                (Some(i), Some(o)) => Some(i.saturating_add(o)),
                _ => None,
            })
            .or(self.input_tokens)
            .or(self.output_tokens)
    }
}

/// A single message in the conversation history.
///
/// Not all fields are meaningful for every [`Role`].  The table below shows
/// which fields are populated by each role's canonical constructor:
///
/// | Field              | User | System | Assistant | ToolCall | ToolResult |
/// |--------------------|------|--------|-----------|----------|------------|
/// | `content`          | ✓    | ✓      | ✓         | —        | ✓          |
/// | `thinking`         | —    | —      | ✓         | —        | —          |
/// | `assistant_phase`  | —    | —      | ✓         | —        | —          |
/// | `hidden`           | ✓    | ✓      | ✓         | ✓        | ✓          |
/// | `include_in_llm`   | ✓    | ✓      | ✓         | ✓        | ✓          |
/// | `tool_call_id`     | —    | —      | —         | ✓        | ✓          |
/// | `tool_name`        | —    | —      | —         | ✓        | —          |
/// | `tool_args`        | —    | —      | —         | ✓        | —          |
/// | `tool_partial_args`| —    | —      | —         | ✓        | —          |
/// | `tool_partial_snapshot` | — | —    | —         | ✓        | —          |
/// | `is_error`         | —    | —      | —         | —        | ✓          |
/// | `display_range`    | —    | —      | —         | —        | ✓          |
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
    /// Chain-of-thought / "thinking" content emitted before the answer.
    /// Populated only for [`Role::Assistant`]; `None` for all other roles.
    pub thinking: Option<String>,
    /// Phase classification for assistant-visible answer text.
    /// Populated only for [`Role::Assistant`]; `None` for all other roles.
    /// `None` also preserves compatibility for old persisted sessions.
    #[serde(default)]
    pub assistant_phase: Option<AssistantPhase>,
    /// When true, this message is sent to the LLM and persisted in the session
    /// but is not rendered in the chat log UI.
    #[serde(default)]
    pub hidden: bool,
    /// When true, this message is included in outbound LLM requests.
    /// Defaults to true for backwards compatibility.
    #[serde(default = "default_true")]
    pub include_in_llm: bool,
    // ── Tool-call fields (Role::ToolCall) ─────────────────────────────────────
    /// Opaque identifier linking a tool call to its result.
    /// Set for [`Role::ToolCall`] and [`Role::ToolResult`]; `None` otherwise.
    pub tool_call_id: Option<String>,
    /// Name of the tool being invoked.
    /// Set only for [`Role::ToolCall`]; `None` for all other roles.
    pub tool_name: Option<String>,
    /// Arguments passed to the tool (JSON object).
    /// Set only for [`Role::ToolCall`]; `None` for all other roles.
    pub tool_args: Option<serde_json::Value>,
    /// Partial raw JSON string for an in-progress tool call whose args are
    /// still streaming. Display-only; never sent to the LLM or persisted.
    #[serde(skip)]
    pub tool_partial_args: Option<String>,
    /// Last successfully completed+parsed partial args snapshot for an
    /// in-progress tool call. Used to keep streamed previews stable across
    /// transient parse failures. Display-only; never sent to the LLM or
    /// persisted.
    #[serde(skip)]
    pub tool_partial_snapshot: Option<serde_json::Value>,
    /// The argument field to stream for display (from `ToolDefinition::streaming_field`).
    /// Display-only; never sent to the LLM or persisted.
    #[serde(skip)]
    pub tool_streaming_field: Option<String>,
    // ── Tool-result fields (Role::ToolResult) ─────────────────────────────────
    /// True when the tool returned an error.
    /// Meaningful only for [`Role::ToolResult`]; always `false` for other roles.
    pub is_error: bool,
    /// Line-range metadata for a partial `read_file` result.
    /// Populated only for [`Role::ToolResult`] messages whose preceding tool
    /// call was `read_file` and where only a window of the file was returned.
    #[serde(default)]
    pub display_range: Option<DisplayRange>,
    /// Binary image content for tool results that return an image
    /// (e.g. `read_file` on an image path).  The tuple is
    /// `(raw_bytes, mime_type)`.  `content` carries a text placeholder when
    /// this is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_data: Option<ImageData>,
}

fn default_true() -> bool {
    true
}

impl Message {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
            thinking: None,
            assistant_phase: None,
            hidden: false,
            include_in_llm: true,
            tool_call_id: None,
            tool_name: None,
            tool_args: None,
            tool_partial_args: None,
            tool_partial_snapshot: None,
            tool_streaming_field: None,
            is_error: false,
            display_range: None,
            image_data: None,
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            thinking: None,
            assistant_phase: None,
            hidden: false,
            include_in_llm: true,
            tool_call_id: None,
            tool_name: None,
            tool_args: None,
            tool_partial_args: None,
            tool_partial_snapshot: None,
            tool_streaming_field: None,
            is_error: false,
            display_range: None,
            image_data: None,
        }
    }

    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
            thinking: None,
            assistant_phase: None,
            hidden: false,
            include_in_llm: true,
            tool_call_id: None,
            tool_name: None,
            tool_args: None,
            tool_partial_args: None,
            tool_partial_snapshot: None,
            tool_streaming_field: None,
            is_error: false,
            display_range: None,
            image_data: None,
        }
    }

    /// An assistant message that contains a tool call request.
    pub fn tool_call(
        id: impl Into<String>,
        name: impl Into<String>,
        args: serde_json::Value,
    ) -> Self {
        Self {
            role: Role::ToolCall,
            content: String::new(),
            thinking: None,
            assistant_phase: None,
            hidden: false,
            include_in_llm: true,
            tool_call_id: Some(id.into()),
            tool_name: Some(name.into()),
            tool_args: Some(args),
            tool_partial_args: None,
            tool_partial_snapshot: None,
            tool_streaming_field: None,
            is_error: false,
            display_range: None,
            image_data: None,
        }
    }

    /// A tool result message sent back to the model.
    pub fn tool_result(
        call_id: impl Into<String>,
        content: impl Into<String>,
        is_error: bool,
    ) -> Self {
        Self {
            role: Role::ToolResult,
            content: content.into(),
            thinking: None,
            assistant_phase: None,
            hidden: false,
            include_in_llm: true,
            tool_call_id: Some(call_id.into()),
            tool_name: None,
            tool_args: None,
            tool_partial_args: None,
            tool_partial_snapshot: None,
            tool_streaming_field: None,
            is_error,
            display_range: None,
            image_data: None,
        }
    }

    /// Builder: attach a [`DisplayRange`] to a tool-result message.
    pub fn with_display_range(mut self, range: DisplayRange) -> Self {
        self.display_range = Some(range);
        self
    }

    /// Builder: attach [`ImageData`] to a tool-result message.
    pub fn with_image_data(mut self, data: ImageData) -> Self {
        self.image_data = Some(data);
        self
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Role {
    System,
    User,
    Assistant,
    /// An assistant turn that contains one or more tool-call requests.
    ToolCall,
    /// A tool result sent back to the model after executing a tool call.
    ToolResult,
}

/// Events emitted by a streaming LLM response.
#[derive(Debug)]
pub enum LlmEvent {
    /// A token chunk from the model's thinking / chain-of-thought block.
    ThinkingToken(String),
    /// A token chunk from the model's answer with phase classification.
    Token { text: String, phase: AssistantPhase },
    /// Final/best-effort token usage stats for the turn.
    Usage(UsageStats),
    /// The model started a tool call block; name is known, args are still streaming.
    ToolCallStart { id: String, name: String },
    /// A partial JSON chunk for an in-progress tool call's arguments.
    ToolCallArgsDelta { id: String, partial_json: String },
    /// The model completed a tool call; args are fully parsed.
    ToolCall {
        id: String,
        name: String,
        args: serde_json::Value,
    },
    /// A transient status message from the provider (e.g. "Rate limited, retrying in 7s…").
    /// Should be shown to the user but is not part of the conversation history.
    StatusUpdate(String),
    /// The stream finished successfully.
    Done,
    /// The request failed.
    Error(ProviderError),
}

/// Description of a tool sent to the LLM so it can choose to call it.
#[derive(Debug, Clone)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    /// JSON Schema object describing the tool's parameters.
    pub parameters: serde_json::Value,
    /// The argument field whose value should be streamed live to the display
    /// as argument deltas arrive. `None` means no partial display.
    pub streaming_field: Option<String>,
}

/// A boxed, heap-allocated stream of `LlmEvent`s that is `Send` and `'static`,
/// suitable for passing across thread boundaries and storing in `App`.
pub type LlmStream = Pin<Box<dyn futures_util::Stream<Item = LlmEvent> + Send + 'static>>;

/// A boxed future that resolves to a list of model names, or a provider error.
/// Returned by `LlmProvider::list_models`.
pub type ModelListFuture =
    Pin<Box<dyn Future<Output = Result<Vec<String>, ProviderError>> + Send + 'static>>;

/// Trait every LLM backend must implement.
///
/// `stream_chat` returns an `LlmStream` rather than accepting a channel
/// sender. This decouples the trait from any specific async runtime primitive
/// and makes implementors independently testable by collecting the stream.
pub trait LlmProvider: Send + Sync {
    fn stream_chat(&self, messages: Vec<Message>) -> LlmStream;

    /// Stream a chat request with tool schemas available to the model.
    ///
    /// The returned stream may yield `LlmEvent::ToolCall` events when the
    /// model decides to call a tool, or `LlmEvent::Token` events for normal
    /// text responses — or a mix of both.
    ///
    /// The default implementation ignores `tools` and delegates to `stream_chat`.
    fn stream_chat_with_tools(
        &self,
        messages: Vec<Message>,
        _tools: Vec<ToolDefinition>,
    ) -> LlmStream {
        self.stream_chat(messages)
    }

    /// Return the list of model names available from this provider.
    /// The default implementation returns an empty list; providers that
    /// support model discovery (e.g. Ollama) should override this.
    fn list_models(&self) -> ModelListFuture {
        Box::pin(async { Ok(vec![]) })
    }
}

pub mod anthropic;
pub mod codex;
pub mod common;
pub mod copilot;
pub mod gemini;
pub mod ollama;
pub mod openai;
pub mod provider_format;
pub mod test_provider;

#[cfg(test)]
mod tests {
    use super::*;

    // ── Message constructors ─────────────────────────────────────────────────

    #[test]
    fn message_user_fields() {
        let m = Message::user("hello");
        assert_eq!(m.role, Role::User);
        assert_eq!(m.content, "hello");
        assert!(m.thinking.is_none());
        assert!(m.tool_call_id.is_none());
    }

    #[test]
    fn message_assistant_fields() {
        let m = Message::assistant("reply");
        assert_eq!(m.role, Role::Assistant);
        assert_eq!(m.content, "reply");
        assert!(m.assistant_phase.is_none());
    }

    #[test]
    fn message_system_fields() {
        let m = Message::system("you are helpful");
        assert_eq!(m.role, Role::System);
        assert_eq!(m.content, "you are helpful");
    }

    #[test]
    fn message_tool_call_fields() {
        let args = serde_json::json!({"command": "ls"});
        let m = Message::tool_call("id-1", "bash", args.clone());
        assert_eq!(m.role, Role::ToolCall);
        assert_eq!(m.tool_call_id.as_deref(), Some("id-1"));
        assert_eq!(m.tool_name.as_deref(), Some("bash"));
        assert_eq!(m.tool_args.as_ref().unwrap(), &args);
        assert!(m.content.is_empty());
    }

    #[test]
    fn message_tool_result_fields() {
        let m = Message::tool_result("id-1", "output text", false);
        assert_eq!(m.role, Role::ToolResult);
        assert_eq!(m.tool_call_id.as_deref(), Some("id-1"));
        assert_eq!(m.content, "output text");
        assert!(!m.is_error);
    }

    #[test]
    fn message_tool_result_is_error_flag() {
        let m = Message::tool_result("id-2", "something went wrong", true);
        assert!(m.is_error);
    }

    // ── Serde round-trip ─────────────────────────────────────────────────────

    #[test]
    fn message_round_trips_through_json() {
        let original = Message::tool_call(
            "call-42",
            "read_file",
            serde_json::json!({"path": "/etc/hosts"}),
        );
        let json = serde_json::to_string(&original).unwrap();
        let decoded: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.role, original.role);
        assert_eq!(decoded.tool_call_id, original.tool_call_id);
        assert_eq!(decoded.tool_name, original.tool_name);
        assert_eq!(decoded.tool_args, original.tool_args);
    }
}
