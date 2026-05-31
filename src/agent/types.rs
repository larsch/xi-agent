use std::{collections::HashMap, sync::Arc};

use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;

use crate::agent::compaction::CompactionOutcome;
use crate::agent::tools::truncate::TruncationResult;
use crate::llm::{AssistantPhase, UsageStats};

// ── Tool result ───────────────────────────────────────────────────────────────

/// The content payload of a tool result — either plain text or a binary image.
#[derive(Debug, Clone)]
pub enum ToolContent {
    /// Plain text output (the common case).
    Text(String),
    /// A binary image returned by the tool (e.g. from `read_file` on an image
    /// path).  `data` is the raw bytes; `mime_type` is a supported image MIME
    /// type such as `"image/png"`.
    Image { data: Vec<u8>, mime_type: String },
}

impl ToolContent {
    /// Return the text content, or a short placeholder for images.
    pub fn as_text(&self) -> &str {
        match self {
            Self::Text(s) => s.as_str(),
            Self::Image { .. } => "[image]",
        }
    }

    /// Convenience: unwrap as a `&str`, panicking if this is an image.
    /// Base64-encode the image data.  Returns `None` for text content.
    pub fn image_base64(&self) -> Option<(&str, String)> {
        match self {
            Self::Image { data, mime_type } => {
                use base64::{Engine as _, engine::general_purpose::STANDARD};
                Some((mime_type.as_str(), STANDARD.encode(data)))
            }
            Self::Text(_) => None,
        }
    }
}

impl From<String> for ToolContent {
    fn from(s: String) -> Self {
        Self::Text(s)
    }
}

impl From<&str> for ToolContent {
    fn from(s: &str) -> Self {
        Self::Text(s.to_string())
    }
}

/// The output produced by a tool execution.
#[derive(Debug, Clone)]
pub struct ToolResult {
    /// Content returned to the model — text or an image.
    pub content: ToolContent,
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
            content: ToolContent::Text(tr.content),
            is_error: false,
            is_truncated: false,
            truncation: None,
            raw_stdout: None,
            raw_stderr: None,
        }
    }

    pub fn ok_truncated(tr: TruncationResult, raw_stdout: String, raw_stderr: String) -> Self {
        Self {
            content: ToolContent::Text(tr.content.clone()),
            is_error: false,
            is_truncated: true,
            truncation: Some(tr),
            raw_stdout: Some(raw_stdout),
            raw_stderr: Some(raw_stderr),
        }
    }

    pub fn err(content: impl Into<String>) -> Self {
        Self {
            content: ToolContent::Text(content.into()),
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
            content: ToolContent::Text(content.into()),
            is_error: false,
            is_truncated: false,
            truncation: None,
            raw_stdout: None,
            raw_stderr: None,
        }
    }

    /// Convenience constructor for an image result.
    pub fn ok_image(data: Vec<u8>, mime_type: impl Into<String>) -> Self {
        Self {
            content: ToolContent::Image {
                data,
                mime_type: mime_type.into(),
            },
            is_error: false,
            is_truncated: false,
            truncation: None,
            raw_stdout: None,
            raw_stderr: None,
        }
    }

    /// If this result is truncated, write `raw_stdout`/`raw_stderr` to `log`,
    /// build a `[Showing lines X-Y of Z. Full output in …]` notice, append it
    /// to `content`, and return the updated result.  Returns `self` unchanged
    /// when `!self.is_truncated` or when no log paths are produced.
    ///
    /// `tool_id` is the opaque call identifier used as the log-file key.
    /// `cmd_summary` is an optional human-readable command label that appears
    /// in the notice (e.g. `" of \`ls -la\`"`).
    pub fn with_log_notice(
        self,
        tool_id: &str,
        cmd_summary: Option<&str>,
        log: &mut crate::agent::tool_output_log::ToolOutputLog,
    ) -> Self {
        if !self.is_truncated {
            return self;
        }

        let stdout = self.raw_stdout.as_deref().unwrap_or("");
        let stderr = self.raw_stderr.as_deref().unwrap_or("");
        let (out_path, err_path) = log.record_streams(tool_id, stdout, stderr);

        let mut file_parts: Vec<String> = Vec::new();
        if let Some(ref p) = out_path {
            file_parts.push(p.display().to_string());
        }
        if let Some(ref p) = err_path {
            file_parts.push(p.display().to_string());
        }

        if file_parts.is_empty() {
            return self;
        }

        let cmd_label = cmd_summary
            .map(|s| format!(" of `{s}`"))
            .unwrap_or_default();
        let files = file_parts.join(" and ");

        let notice = if let Some(ref tr) = self.truncation {
            let start = tr.first_kept_line;
            let end = tr.first_kept_line + tr.output_lines - 1;
            format!(
                "[Showing lines {start}-{end} of {total}. \
                 Full output{cmd_label} in {files}]",
                total = tr.total_lines,
            )
        } else {
            format!("[Output truncated. Full output{cmd_label} in {files}]")
        };

        // Only text content can have a log notice appended.
        let content = match self.content {
            ToolContent::Text(ref s) => {
                let mut c = s.clone();
                if !c.ends_with('\n') {
                    c.push('\n');
                }
                c.push('\n');
                c.push_str(&notice);
                ToolContent::Text(c)
            }
            // Image content cannot be appended to; return unchanged.
            ToolContent::Image { .. } => self.content.clone(),
        };

        Self {
            content,
            is_error: self.is_error,
            is_truncated: true,
            truncation: self.truncation,
            raw_stdout: None,
            raw_stderr: None,
        }
    }
}

// ── ToolCallContext ───────────────────────────────────────────────────────────

/// Context passed to every tool execution.
///
/// Subprocess tools use this to forward live output chunks back to the UI via
/// [`AgentEvent::ToolOutputChunk`].  Non-subprocess tools may ignore it.
#[derive(Clone)]
pub struct ToolCallContext {
    /// The opaque tool call identifier assigned by the LLM provider.
    pub id: String,
    /// Optional sender for live output chunks.  `None` in tests or wherever
    /// live streaming is not wired up.
    pub tx: Option<UnboundedSender<crate::app_event::AppEvent>>,
}

#[cfg(test)]
impl ToolCallContext {
    /// Construct a context with no live-output sender (for tests).
    pub fn noop(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            tx: None,
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
    /// The core implementation method — implement this, not `execute`.
    ///
    /// `ctx` carries the call identifier and an optional sender for live output
    /// chunks.  Subprocess tools forward output via `ctx`; others may ignore it.
    fn run(
        &self,
        args: serde_json::Value,
        ctx: ToolCallContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ToolResult> + Send + '_>>;

    /// Execute without live output (for tests only — uses a noop context).
    #[cfg(test)]
    fn execute(
        &self,
        args: serde_json::Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ToolResult> + Send + '_>> {
        self.run(args, ToolCallContext::noop(""))
    }
    /// The argument field whose string value should be streamed live to the
    /// display as JSON argument deltas arrive. `None` means no partial display.
    fn streaming_field(&self) -> Option<String> {
        None
    }
}

/// A registry mapping tool names to their implementations.
pub type ToolRegistry = HashMap<String, Arc<dyn Tool>>;

// ── ToolExecutor ──────────────────────────────────────────────────────────────

/// Abstraction over the execution of a single tool call.
///
/// Implementors decide whether to allow, block, override, or log the call.
/// The agent loop calls [`ToolExecutor::execute_tool`] instead of invoking the
/// `Tool` trait directly, so test doubles can inject controlled behaviour
/// without constructing shared-state wrappers.
pub trait ToolExecutor: Send + Sync {
    /// Execute the named tool with the given arguments.
    ///
    /// `id` is the opaque call identifier (used for log-file keying).
    /// `name` is the tool name.
    /// `args` is the JSON argument object.
    /// `tools` is the registry used to look up the implementation.
    /// `log` is the output log used to persist truncated output.
    /// `tx` is the optional event sender for live output chunks.
    fn execute_tool<'a>(
        &'a self,
        id: &'a str,
        name: &'a str,
        args: serde_json::Value,
        tools: &'a ToolRegistry,
        log: &'a std::sync::Arc<std::sync::Mutex<crate::agent::tool_output_log::ToolOutputLog>>,
        tx: Option<UnboundedSender<crate::app_event::AppEvent>>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ToolResult> + Send + 'a>>;
}

/// The default [`ToolExecutor`] used in production.
///
/// Runs the optional `before_tool_call` guard (returning an error result when
/// it returns `false`), dispatches to the matching [`Tool`], applies the log
/// notice for tools that save output, then runs the optional `after_tool_call`
/// override.
pub struct DefaultToolExecutor {
    /// Optional hook called before each tool execution. Return `false` to block.
    pub before_tool_call: Option<BeforeToolCall>,
    /// Optional hook called after each tool execution. Return `Some(result)` to override.
    pub after_tool_call: Option<AfterToolCall>,
}

impl DefaultToolExecutor {
    /// Create a new executor with no hooks.
    pub fn new() -> Self {
        Self {
            before_tool_call: None,
            after_tool_call: None,
        }
    }

    /// Create an executor with optional before/after hooks (for tests only).
    #[cfg(test)]
    pub fn with_hooks(
        before_tool_call: Option<BeforeToolCall>,
        after_tool_call: Option<AfterToolCall>,
    ) -> Self {
        Self {
            before_tool_call,
            after_tool_call,
        }
    }
}

impl Default for DefaultToolExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolExecutor for DefaultToolExecutor {
    fn execute_tool<'a>(
        &'a self,
        id: &'a str,
        name: &'a str,
        args: serde_json::Value,
        tools: &'a ToolRegistry,
        log: &'a std::sync::Arc<std::sync::Mutex<crate::agent::tool_output_log::ToolOutputLog>>,
        tx: Option<UnboundedSender<crate::app_event::AppEvent>>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ToolResult> + Send + 'a>> {
        Box::pin(async move {
            let blocked = self
                .before_tool_call
                .as_ref()
                .map(|f| !f(name, &args))
                .unwrap_or(false);

            let mut result = if blocked {
                ToolResult::err(format!("Tool call '{name}' was blocked"))
            } else {
                match tools.get(name) {
                    Some(tool) => {
                        let ctx = ToolCallContext {
                            id: id.to_string(),
                            tx,
                        };
                        let r = tool.run(args.clone(), ctx).await;
                        let cmd_summary = args.get("command").and_then(|v| v.as_str());
                        r.with_log_notice(id, cmd_summary, &mut log.lock().unwrap())
                    }
                    None => ToolResult::err(format!("Unknown tool: '{name}'")),
                }
            };

            if let Some(f) = &self.after_tool_call
                && let Some(override_result) = f(name, &result)
            {
                result = override_result;
            }

            result
        })
    }
}

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
    /// The model started a tool call block; name is known, args are still streaming.
    ToolCallIntent {
        id: String,
        name: String,
        streaming_field: Option<String>,
    },
    /// A partial JSON argument chunk for an in-progress tool call.
    ToolCallArgsDelta { id: String, partial_json: String },
    /// A queued steering message was consumed and inserted into loop history.
    SteeringConsumed { text: String },
    /// A transient status message from the provider (e.g. "Rate limited, retrying in 7s…").
    /// Should be shown to the user but is not part of the conversation history.
    StatusUpdate(String),
    /// The loop is performing a compaction pass.
    Compacting,
    /// A compaction summary was produced and should be appended to the session log.
    CompactionDone(CompactionOutcome),
    // ── Tool lifecycle ─────────────────────────────────────────────────────────
    /// The model requested a tool call; execution is about to begin.
    ToolCallStart {
        id: String,
        name: String,
        args: serde_json::Value,
    },
    /// A live output chunk from a running subprocess tool.
    ToolOutputChunk { id: String, chunk: String },
    /// A tool call finished; contains the result.
    ToolCallEnd { id: String, result: ToolResult },
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
    /// Executor responsible for dispatching individual tool calls.
    /// Wraps the before/after hooks and any override logic.
    pub executor: std::sync::Arc<dyn ToolExecutor>,
    /// Current session event log snapshot used for compaction decisions.
    pub session_events: Vec<crate::session_event::SessionEvent>,
    /// Active model name used for context window lookup and summary requests.
    pub current_model: String,
    /// When true, allow threshold-based auto-compaction after completed turns.
    pub auto_compaction_enabled: bool,
    /// Optional manual compaction instructions to apply immediately when the
    /// loop starts, before any normal assistant turn is requested.
    pub manual_compaction_instructions: Option<String>,
    /// System prompt prepended to all LLM requests.  When `None`, no system
    /// message is added.
    pub system_prompt: Option<String>,
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::ToolResult;
    use crate::agent::tool_output_log::ToolOutputLog;
    use crate::agent::tools::truncate::TruncationResult;

    fn truncated_result() -> ToolResult {
        ToolResult {
            content: super::ToolContent::Text("line1\nline2".to_string()),
            is_error: false,
            is_truncated: true,
            truncation: Some(TruncationResult {
                content: "line1\nline2".to_string(),
                truncated: true,
                total_lines: 100,

                output_lines: 2,
                first_kept_line: 99,
            }),
            raw_stdout: Some("line1\nline2".to_string()),
            raw_stderr: Some(String::new()),
        }
    }

    #[test]
    fn with_log_notice_noop_when_not_truncated() {
        let mut log = ToolOutputLog::new("test-noop");
        let r = ToolResult::ok_str("hello");
        let out = r.with_log_notice("call-1", None, &mut log);
        assert!(!out.is_truncated);
        assert_eq!(out.content.as_text(), "hello");
    }

    #[test]
    fn with_log_notice_appends_notice_when_truncated() {
        let mut log = ToolOutputLog::new("test-notice");
        let r = truncated_result();
        let out = r.with_log_notice("call-2", None, &mut log);
        // Notice should be appended after a blank line.
        assert!(
            out.content.as_text().contains("[Showing lines"),
            "notice should contain line range: {}",
            out.content.as_text()
        );
        assert!(
            out.content.as_text().contains("99"),
            "notice should reference first kept line: {}",
            out.content.as_text()
        );
        assert!(
            out.content.as_text().contains("100"),
            "notice should reference total lines: {}",
            out.content.as_text()
        );
    }

    #[test]
    fn with_log_notice_includes_cmd_summary_when_provided() {
        let mut log = ToolOutputLog::new("test-cmd");
        let r = truncated_result();
        let out = r.with_log_notice("call-3", Some("ls -la"), &mut log);
        assert!(
            out.content.as_text().contains("of `ls -la`"),
            "notice should include command summary: {}",
            out.content.as_text()
        );
    }
}
