use std::{future::Future, pin::Pin};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AssistantPhase {
    Unknown,
    Provisional,
    Final,
}

/// Normalized token usage reported by a provider for a completed turn.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
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
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
    /// Chain-of-thought / "thinking" content emitted before the answer.
    /// `None` for messages that carry no thinking output.
    pub thinking: Option<String>,
    /// Optional phase classification for assistant-visible answer text.
    /// `None` preserves compatibility for old persisted sessions.
    #[serde(default)]
    pub assistant_phase: Option<AssistantPhase>,
    /// When true, this message is sent to the LLM and persisted in the session
    /// but is not rendered in the chat log UI.
    #[serde(default)]
    pub hidden: bool,
    // ── Tool-call fields (Role::ToolCall) ─────────────────────────────────────
    /// Opaque identifier linking a tool call to its result.
    pub tool_call_id: Option<String>,
    /// Name of the tool being called or that produced this result.
    pub tool_name: Option<String>,
    /// Arguments passed to the tool (JSON object).
    pub tool_args: Option<serde_json::Value>,
    // ── Tool-result fields (Role::ToolResult) ─────────────────────────────────
    /// True when the tool returned an error.
    pub is_error: bool,
}

impl Message {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
            thinking: None,
            assistant_phase: None,
            hidden: false,
            tool_call_id: None,
            tool_name: None,
            tool_args: None,
            is_error: false,
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            thinking: None,
            assistant_phase: None,
            hidden: false,
            tool_call_id: None,
            tool_name: None,
            tool_args: None,
            is_error: false,
        }
    }

    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
            thinking: None,
            assistant_phase: None,
            hidden: false,
            tool_call_id: None,
            tool_name: None,
            tool_args: None,
            is_error: false,
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
            tool_call_id: Some(id.into()),
            tool_name: Some(name.into()),
            tool_args: Some(args),
            is_error: false,
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
            tool_call_id: Some(call_id.into()),
            tool_name: None,
            tool_args: None,
            is_error,
        }
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

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::ToolCall => "assistant", // serialised as assistant with tool_calls
            Role::ToolResult => "tool",
        }
    }
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
    /// The provider indicated that an assistant tool call is forthcoming.
    ToolIntentStart,
    /// The model requested a tool call.
    ToolCall {
        id: String,
        name: String,
        args: serde_json::Value,
    },
    /// The stream finished successfully.
    Done,
    /// The request failed.
    Error(String),
}

/// Description of a tool sent to the LLM so it can choose to call it.
#[derive(Debug, Clone)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    /// JSON Schema object describing the tool's parameters.
    pub parameters: serde_json::Value,
}

/// A boxed, heap-allocated stream of `LlmEvent`s that is `Send` and `'static`,
/// suitable for passing across thread boundaries and storing in `App`.
pub type LlmStream = Pin<Box<dyn futures_util::Stream<Item = LlmEvent> + Send + 'static>>;

/// A boxed future that resolves to a list of model names, or an error string.
/// Returned by `LlmProvider::list_models`.
pub type ModelListFuture =
    Pin<Box<dyn Future<Output = Result<Vec<String>, String>> + Send + 'static>>;

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
pub mod copilot;
pub mod gemini;
pub mod ollama;
pub mod openai;

#[cfg(test)]
mod tests {
    use super::*;

    // ── Role::as_str ─────────────────────────────────────────────────────────

    #[test]
    fn role_as_str_system() {
        assert_eq!(Role::System.as_str(), "system");
    }

    #[test]
    fn role_as_str_user() {
        assert_eq!(Role::User.as_str(), "user");
    }

    #[test]
    fn role_as_str_assistant() {
        assert_eq!(Role::Assistant.as_str(), "assistant");
    }

    #[test]
    fn role_as_str_tool_call_is_assistant() {
        // ToolCall messages are sent to the API as role "assistant" with a
        // tool_calls array — the Role variant is only for internal bookkeeping.
        assert_eq!(Role::ToolCall.as_str(), "assistant");
    }

    #[test]
    fn role_as_str_tool_result_is_tool() {
        assert_eq!(Role::ToolResult.as_str(), "tool");
    }

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
