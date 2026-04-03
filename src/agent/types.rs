use std::{collections::HashMap, sync::Arc};

use tokio::sync::{mpsc::UnboundedSender, oneshot};

use crate::agent::tools::truncate::TruncationResult;
use crate::llm::{AssistantPhase, UsageStats};

// ── Tool result ───────────────────────────────────────────────────────────────

/// The output produced by a tool execution.
#[derive(Debug, Clone)]
pub struct ToolResult {
    /// Text content returned to the model (may be truncated).
    pub content: String,
    /// True when the tool encountered an error.
    pub is_error: bool,
    /// True when the content was truncated and the full output is longer.
    pub is_truncated: bool,
    /// Truncation metadata when `is_truncated` is true.
    pub truncation: Option<TruncationResult>,
    /// Full pre-truncation stdout, set when `saves_output` is true.
    pub raw_stdout: Option<String>,
    /// Full pre-truncation stderr, set when `saves_output` is true.
    pub raw_stderr: Option<String>,
}

impl ToolResult {
    pub fn ok(tr: TruncationResult) -> Self {
        Self {
            content: tr.content,
            is_error: false,
            is_truncated: false,
            truncation: None,
            raw_stdout: None,
            raw_stderr: None,
        }
    }

    pub fn ok_truncated(tr: TruncationResult, raw_stdout: String, raw_stderr: String) -> Self {
        Self {
            content: tr.content.clone(),
            is_error: false,
            is_truncated: true,
            truncation: Some(tr),
            raw_stdout: Some(raw_stdout),
            raw_stderr: Some(raw_stderr),
        }
    }

    pub fn err(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: true,
            is_truncated: false,
            truncation: None,
            raw_stdout: None,
            raw_stderr: None,
        }
    }

    /// Convenience constructor for plain (non-truncated) ok results.
    pub fn ok_str(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
            is_truncated: false,
            truncation: None,
            raw_stdout: None,
            raw_stderr: None,
        }
    }
}

// ── Tool trait ────────────────────────────────────────────────────────────────

/// A tool the agent can invoke.
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    /// JSON Schema object describing the tool's input parameters.
    fn parameters_schema(&self) -> serde_json::Value;
    /// Execute the tool with the given arguments (JSON object).
    fn execute(
        &self,
        args: serde_json::Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ToolResult> + Send + '_>>;
    /// Whether the agent loop should save this tool's full output to a log
    /// file and append the path to the result.  Defaults to `false`; shell
    /// and custom tools override this to `true`.
    fn saves_output(&self) -> bool {
        false
    }
}

/// A registry mapping tool names to their implementations.
pub type ToolRegistry = HashMap<String, Arc<dyn Tool>>;

// ── ask_user request/response bridge ─────────────────────────────────────────

/// One selectable option for the `ask_user` tool.
#[derive(Debug, Clone)]
pub struct AskUserOption {
    pub title: String,
    pub description: Option<String>,
}

/// Payload sent from `AskUserTool` to the TUI loop.
#[derive(Debug)]
pub struct AskRequest {
    pub question: String,
    pub context: Option<String>,
    pub options: Vec<AskUserOption>,
    pub allow_multiple: bool,
    pub allow_freeform: bool,
    pub reply: oneshot::Sender<AskUserResponse>,
}

/// User response returned from the TUI loop back to `AskUserTool`.
#[derive(Debug)]
pub enum AskUserResponse {
    Answer(String),
    Cancelled,
}

/// Sender type used by `AskUserTool` to post requests to the UI.
pub type AskRequestTx = UnboundedSender<AskRequest>;

// ── Agent events ──────────────────────────────────────────────────────────────

/// Events emitted by the agent loop to `App` over a tokio channel.
#[derive(Debug)]
pub enum AgentEvent {
    // ── LLM streaming ─────────────────────────────────────────────────────────
    /// A text token chunk from the model's answer.
    TextToken { text: String, phase: AssistantPhase },
    /// A token chunk from the model's thinking / chain-of-thought block.
    ThinkingToken(String),
    /// Final/best-effort token usage stats for the turn.
    Usage(UsageStats),
    /// The provider indicated that an assistant tool call is forthcoming.
    ToolIntentStart,
    /// A queued steering message was consumed and inserted into loop history.
    SteeringConsumed { text: String },
    /// A transient status message from the provider (e.g. "Rate limited, retrying in 7s…").
    /// Should be shown to the user but is not part of the conversation history.
    StatusUpdate(String),
    // ── Tool lifecycle ─────────────────────────────────────────────────────────
    /// The model requested a tool call; execution is about to begin.
    ToolCallStart {
        id: String,
        name: String,
        args: serde_json::Value,
    },
    /// A tool call finished; contains the result.
    ToolCallEnd {
        id: String,
        name: String,
        result: ToolResult,
    },
    // ── Loop lifecycle ─────────────────────────────────────────────────────────
    /// One or more tracked files were modified externally before this turn.
    /// `notification` is the pre-formatted user message text that was injected
    /// into the conversation history; `paths` lists the affected files.
    ExternalFileChange {
        paths: Vec<std::path::PathBuf>,
        notification: String,
    },
    /// One LLM turn (assistant response + any tool calls) is complete.
    TurnEnd,
    /// The agent loop finished successfully.
    Done,
    /// The agent loop encountered a fatal error from the LLM provider.
    Error(crate::llm::ProviderError),
}

// ── Agent loop configuration ──────────────────────────────────────────────────

/// Hook called before each tool execution. Return `false` to block the call.
pub type BeforeToolCall = Box<dyn Fn(&str, &serde_json::Value) -> bool + Send + Sync>;

/// Hook called after each tool execution. Return `Some(result)` to override.
pub type AfterToolCall = Box<dyn Fn(&str, &ToolResult) -> Option<ToolResult> + Send + Sync>;

/// Configuration passed to `run_agent_loop`.
pub struct AgentLoopConfig {
    /// Tools available to the model.
    pub tools: ToolRegistry,
    /// Tracker for files touched by built-in file tools; used to detect
    /// external modifications before each LLM turn.
    pub file_tracker: std::sync::Arc<std::sync::Mutex<crate::agent::file_tracker::FileTracker>>,
    /// Log that persists full tool output to temp files for the session.
    pub tool_output_log:
        std::sync::Arc<std::sync::Mutex<crate::agent::tool_output_log::ToolOutputLog>>,
    /// Optional hook called before each tool execution.
    /// Return `false` to block the tool call (an error result is returned instead).
    pub before_tool_call: Option<BeforeToolCall>,
    /// Optional hook called after each tool execution.
    /// Return `Some(result)` to override the tool's result.
    pub after_tool_call: Option<AfterToolCall>,
}
