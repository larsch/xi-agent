use ratatui::text::Line;
use ratatui_textarea::{CursorMove, TextArea};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::task::JoinHandle;

use crate::{
    agent::{
        AgentLoopConfig, ToolOutputLog, run_agent_loop,
        types::{AgentEvent, AskRequest, AskRequestTx, AskUserOption, AskUserResponse},
    },
    auth::{self, AuthFlow, LoginEvent},
    commands::{self, CompletionItem},
    llm::{AssistantPhase, LlmProvider, Message, Role, UsageStats},
    provider::{ProviderKind, ThinkingSupport, thinking_support_for},
    session::SessionStore,
    shell::{self, ShellKind},
    skills::SkillMeta,
    thinking::ThinkingLevel,
};

use crate::export;

// ── Selection result ──────────────────────────────────────────────────────────

/// Value returned when the user confirms a choice in the selection menu.
pub enum SelectionResult {
    Model(String),
    Thinking(ThinkingLevel),
    Provider(String),
    LoginProvider(String),
    ResumeSession(String),
    AskOption(String),
    AskFreeform,
    /// A login-panel action was selected.
    LoginAction(LoginActionKind),
    /// An Ollama endpoint URL was confirmed (either picked from history or typed).
    OllamaEndpoint(String),
    /// The user chose to type a custom Ollama endpoint URL.
    OllamaEndpointFreeform,
}

/// Actions available in the login action menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginActionKind {
    OpenBrowser,
    CopyUrl,
    CopyCode,
    Cancel,
}

struct PendingAsk {
    options: Vec<AskUserOption>,
    allow_freeform: bool,
}

/// Target operation to retry after token refresh completes.
#[derive(Debug, Clone, Copy)]
enum RetryTarget {
    /// Retry the last agent turn (chat request).
    AgentTurn,
    /// Retry the model list fetch.
    ModelFetch,
}

/// Maximum number of rows shown in the selection menu before scrolling.
pub const MAX_SELECTION_VISIBLE: usize = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SelectionKind {
    Model,
    Thinking,
    Provider,
    LoginProvider,
    ResumeSession,
    AskUser,
    LoginAction,
    OllamaEndpoint,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputMode {
    Chat,
    Shell,
}

// ── App state ─────────────────────────────────────────────────────────────────

pub struct App {
    pub messages: Vec<Message>,
    pub textarea: TextArea<'static>,
    pub shell_textarea: TextArea<'static>,
    pub input_mode: InputMode,
    pub selected_shell: ShellKind,
    pub available_shells: Vec<ShellKind>,
    /// Monotonic revision bump for any visible log-content change.
    /// Used to invalidate UI wrapping caches.
    pub log_revision: u64,
    /// Cached pre-wrapped log lines for the most recent `(log_revision, width)`.
    pub cached_log_lines: Option<(u64, usize, Vec<Line<'static>>)>,
    pub log_scroll: usize,
    /// When true, the view always follows the bottom (auto-scrolls).
    pub auto_scroll: bool,
    /// Height of the log pane from the last draw — used as page-size scrolling.
    pub last_log_height: usize,
    /// Whether the log pane had a visible scrollbar on the previous frame.
    /// Used by the renderer to pick the likely wrap width and avoid redundant
    /// full log re-wrap work on every draw.
    pub log_had_scrollbar: bool,
    pub streaming: bool,
    /// Throbber animation frame index, advanced on every UI tick while streaming.
    pub throbber_tick: u8,
    /// Instant of the last visible agent output (text/thinking tokens, tool
    /// calls, tool results, etc.); used to suppress the throbber while output
    /// is actively arriving and re-show it after 1 s of idle time.
    pub last_output_at: Option<std::time::Instant>,
    /// Transient status message from the active provider (e.g. "Rate limited, retrying in 7s…").
    /// Shown in the chat log while streaming; cleared on turn end / error.
    pub provider_status: Option<String>,
    /// Optional system prompt prepended to every request.
    pub system_prompt: Option<String>,
    /// Currently active model name (mirrors the provider; updated on `/model`).
    pub current_model: String,
    /// Currently active provider name (e.g. `"copilot"`).
    pub current_provider: String,
    /// Currently active thinking / reasoning level.
    pub current_thinking: ThinkingLevel,
    /// Agent loop configuration (tools, hooks).
    pub agent_config: AgentLoopConfig,
    /// Skills loaded from all supported skill roots.
    pub loaded_skills: Vec<SkillMeta>,

    // ── Completion popup ──────────────────────────────────────────────────────
    /// Items to display in the completion popup (empty = popup hidden).
    pub completions: Vec<CompletionItem>,
    /// Index of the currently highlighted completion row.
    pub completion_selected: usize,

    // ── Available models (for /model completions) ─────────────────────────────
    /// Cached model list from the provider; `None` until first successful fetch.
    pub available_models: Option<Vec<String>>,
    /// True while a `list_models` task is in flight.
    pub models_loading: bool,
    /// Set to the error message when the last model fetch failed.
    pub model_fetch_error: Option<String>,

    // ── Generic selection menu ────────────────────────────────────────────────
    /// True when the full-screen selection picker is active.
    pub selection_mode: bool,
    /// Header title shown in the selection menu.
    pub selection_title: &'static str,
    /// Items shown in the selection menu.
    pub selection_items: Vec<CompletionItem>,
    /// Unfiltered source items for selection search.
    selection_all_items: Vec<CompletionItem>,
    /// Current search query for selection filtering.
    pub selection_query: String,
    /// Kind of selection currently being displayed.
    selection_kind: Option<SelectionKind>,
    /// Index of the currently highlighted selection row.
    pub selection_selected: usize,
    /// First visible item index in the selection menu (scroll offset).
    pub selection_scroll: usize,

    // ── Info bar ──────────────────────────────────────────────────────────────
    /// When true, the info bar (provider / model / context window) is shown
    /// below the input panel.  Toggled by Ctrl+I.
    pub show_info: bool,
    /// Best-effort token usage reported for the latest completed turn.
    pub latest_usage: Option<UsageStats>,

    // ── Login overlay ─────────────────────────────────────────────────────────
    pub login_active: bool,
    pub login_provider: Option<String>,
    pub login_info: String,
    pub login_url: Option<String>,
    pub login_code: Option<String>,
    /// Which OAuth flow is in use; drives the UI's instruction text and
    /// available keyboard actions.
    pub login_auth_flow: Option<AuthFlow>,
    pub login_needs_rebuild: bool,
    pub refresh_in_progress: bool,
    pub retry_after_refresh: bool,
    /// Set when a `list_models` call fails with a 401 so the fetch is
    /// re-issued automatically once the token refresh completes.
    pub retry_model_fetch_after_refresh: bool,
    auth_retry_budget: u8,
    login_cancel: Option<Arc<AtomicBool>>,
    /// Persistent clipboard instance used during the login flow.
    ///
    /// On Linux the clipboard is owned by the process: dropping the
    /// `arboard::Clipboard` instance releases ownership and the text
    /// disappears from other applications.  We therefore keep it alive for
    /// the entire duration of the login panel and only drop it once login
    /// finishes.
    clipboard: Option<arboard::Clipboard>,

    // ── Session persistence ───────────────────────────────────────────────────
    session_store: Option<SessionStore>,
    current_session_id: Option<String>,
    current_cwd: String,
    resume_available_for_cwd: bool,

    // ── ask_user overlay state ───────────────────────────────────────────────
    pending_ask: Option<PendingAsk>,
    ask_reply: Option<tokio::sync::oneshot::Sender<AskUserResponse>>,
    /// When true the textarea is used to type a freeform answer to an
    /// ask_user question that has no predefined options.  The question text
    /// is stored in `ask_user_question` and displayed as a hint in the UI.
    pub ask_user_freeform_mode: bool,
    /// The question text to display when `ask_user_freeform_mode` is active.
    pub ask_user_question: Option<String>,

    // ── Ollama endpoint input mode ────────────────────────────────────────────
    /// When true the textarea is used to type a custom Ollama endpoint URL
    /// rather than a regular chat message.
    pub ollama_endpoint_input_mode: bool,

    // ── Open WebUI setup input modes ──────────────────────────────────────────
    /// When true the textarea is used to type the Open WebUI base URL.
    pub open_webui_url_input_mode: bool,
    /// When true the textarea is used to type the Open WebUI API key/token.
    pub open_webui_token_input_mode: bool,
    /// URL entered during Open WebUI setup (held while the user types the token).
    pub open_webui_pending_url: Option<String>,

    // ── Async channels ────────────────────────────────────────────────────────
    /// Receives AgentEvents forwarded from the active agent loop task.
    pub(crate) event_rx: tokio::sync::mpsc::UnboundedReceiver<AgentEvent>,
    event_tx: tokio::sync::mpsc::UnboundedSender<AgentEvent>,
    steering_tx: Option<tokio::sync::mpsc::UnboundedSender<String>>,
    /// User steering messages queued while a loop is running; rendered pinned
    /// at the bottom of the log with a 🕹️ icon until consumed.
    pub queued_steering: Vec<String>,
    /// JoinHandle for the currently running agent loop task (if any).
    agent_task: Option<JoinHandle<()>>,
    /// Receives model lists forwarded from `list_models` tasks.
    pub(crate) models_rx:
        tokio::sync::mpsc::UnboundedReceiver<Result<Vec<String>, crate::llm::ProviderError>>,
    models_tx: tokio::sync::mpsc::UnboundedSender<Result<Vec<String>, crate::llm::ProviderError>>,
    /// Receives login status events from background auth tasks.
    pub(crate) login_rx: tokio::sync::mpsc::UnboundedReceiver<LoginEvent>,
    login_tx: tokio::sync::mpsc::UnboundedSender<LoginEvent>,
    /// Receives ask_user requests from AskUserTool executions.
    pub(crate) ask_rx: tokio::sync::mpsc::UnboundedReceiver<AskRequest>,
    ask_tx: AskRequestTx,
}

// Convenience alias used throughout this module.
type DynProvider = Arc<dyn LlmProvider + Send + Sync + 'static>;

impl App {
    fn bump_log_revision(&mut self) {
        self.log_revision = self.log_revision.saturating_add(1);
        self.cached_log_lines = None;
    }

    pub fn mark_log_dirty(&mut self) {
        self.bump_log_revision();
    }

    pub fn new(
        initial_model: impl Into<String>,
        initial_provider: &ProviderKind,
        initial_thinking: ThinkingLevel,
        agent_config: AgentLoopConfig,
    ) -> Self {
        let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
        let (models_tx, models_rx) = tokio::sync::mpsc::unbounded_channel();
        let (login_tx, login_rx) = tokio::sync::mpsc::unbounded_channel();
        let (ask_tx, ask_rx) = tokio::sync::mpsc::unbounded_channel();
        let available_shells = shell::discover_available_shells();
        let selected_shell = available_shells.first().copied().unwrap_or(ShellKind::Bash);
        Self {
            messages: Vec::new(),
            textarea: Self::make_textarea(),
            shell_textarea: Self::make_textarea(),
            input_mode: InputMode::Chat,
            selected_shell,
            available_shells,
            log_revision: 0,
            cached_log_lines: None,
            log_scroll: 0,
            auto_scroll: true,
            last_log_height: 0,
            log_had_scrollbar: false,
            streaming: false,
            throbber_tick: 0,
            last_output_at: None,
            provider_status: None,
            system_prompt: None,
            current_model: initial_model.into(),
            current_provider: initial_provider.name().to_string(),
            current_thinking: initial_thinking,
            agent_config,
            loaded_skills: Vec::new(),
            completions: Vec::new(),
            completion_selected: 0,
            available_models: None,
            models_loading: false,
            model_fetch_error: None,
            selection_mode: false,
            selection_title: "Select model",
            selection_items: Vec::new(),
            selection_all_items: Vec::new(),
            selection_query: String::new(),
            selection_kind: None,
            selection_selected: 0,
            selection_scroll: 0,
            show_info: false,
            latest_usage: None,
            login_active: false,
            login_provider: None,
            login_info: String::new(),
            login_url: None,
            login_code: None,
            login_auth_flow: None,
            login_needs_rebuild: false,
            refresh_in_progress: false,
            retry_after_refresh: false,
            retry_model_fetch_after_refresh: false,
            auth_retry_budget: 0,
            login_cancel: None,
            clipboard: None,
            session_store: None,
            current_session_id: None,
            current_cwd: String::new(),
            resume_available_for_cwd: false,
            pending_ask: None,
            ask_reply: None,
            ask_user_freeform_mode: false,
            ask_user_question: None,
            ollama_endpoint_input_mode: false,
            open_webui_url_input_mode: false,
            open_webui_token_input_mode: false,
            open_webui_pending_url: None,
            event_rx,
            event_tx,
            steering_tx: None,
            queued_steering: Vec::new(),
            agent_task: None,
            models_rx,
            models_tx,
            login_rx,
            login_tx,
            ask_rx,
            ask_tx,
        }
    }

    /// Advance the throbber animation frame.  Called on every UI tick.
    pub fn tick(&mut self) {
        if self.streaming {
            self.throbber_tick = self.throbber_tick.wrapping_add(1);
        }
    }

    /// Returns true when the throbber should be visible.
    ///
    /// Three-state model:
    /// - Machine waiting for **user** (`has_pending_ask` / `ask_user_freeform_mode`):
    ///   throbber hidden — the ball is in the user's court.
    /// - Machine producing **output** (visible content added within the last second):
    ///   throbber hidden — something is actively appearing on screen.
    /// - Machine working **silently** (streaming, no output for ≥ 1 s):
    ///   throbber visible — signals that work is in progress.
    pub fn throbber_visible(&self) -> bool {
        if !self.streaming {
            return false;
        }
        // The agent loop is paused waiting for user input — don't spin.
        if self.has_pending_ask() || self.ask_user_freeform_mode {
            return false;
        }
        match self.last_output_at {
            None => true,
            Some(t) => t.elapsed() >= std::time::Duration::from_millis(240),
        }
    }

    /// Toggle the info bar visibility.
    pub fn toggle_info(&mut self) {
        self.show_info = !self.show_info;
    }

    pub fn ask_request_tx(&self) -> AskRequestTx {
        self.ask_tx.clone()
    }

    pub fn init_session_persistence(&mut self, cwd: String) {
        self.current_cwd = cwd;
        match SessionStore::open() {
            Ok(store) => {
                self.session_store = Some(store);
                self.refresh_resume_availability();
            }
            Err(e) => {
                log::debug!("session persistence disabled: {}", e);
                self.messages.push(Message::assistant(format!(
                    "[session persistence unavailable: {e}]"
                )));
                self.bump_log_revision();
            }
        }
    }

    pub fn current_cwd(&self) -> &str {
        &self.current_cwd
    }

    pub fn should_show_resume_hint(&self) -> bool {
        self.resume_available_for_cwd
            && self.messages.is_empty()
            && !self.selection_mode
            && !self.login_active
            && !self.streaming
    }

    pub fn resume_latest_for_current_cwd(&mut self) {
        let Some(store) = self.session_store.as_ref() else {
            return;
        };
        let Some(meta) = store.latest_for_cwd(&self.current_cwd) else {
            self.messages.push(Message::assistant(
                "[no resumable session in this working folder]",
            ));
            self.bump_log_revision();
            return;
        };
        self.resume_session_by_id(&meta.id);
    }

    pub fn resume_session_by_id(&mut self, session_id: &str) {
        let Some(store) = self.session_store.as_ref() else {
            return;
        };
        match store.load_messages(session_id) {
            Ok(messages) => {
                self.messages = messages;
                self.current_session_id = Some(session_id.to_string());
                self.auto_scroll = true;
                self.log_scroll = 0;
                self.bump_log_revision();
            }
            Err(e) => {
                self.messages.push(Message::assistant(format!(
                    "[failed to resume session: {e}]"
                )));
                self.bump_log_revision();
            }
        }
        self.refresh_resume_availability();
    }

    pub fn enter_resume_selection_mode(&mut self) {
        self.reset_textarea();
        self.selection_mode = true;
        self.selection_kind = Some(SelectionKind::ResumeSession);
        self.selection_title = "  Resume session  ";
        self.selection_query.clear();

        let Some(store) = self.session_store.as_ref() else {
            self.set_selection_items(vec![CompletionItem {
                label: "session persistence unavailable".to_string(),
                detail: String::new(),
                complete_to: String::new(),
                loading: true,
                error: false,
                match_range: None,
            }]);
            return;
        };

        let mut sessions = store.list_sessions();
        sessions.sort_by(|a, b| {
            let a_local = a.cwd == self.current_cwd;
            let b_local = b.cwd == self.current_cwd;
            b_local
                .cmp(&a_local)
                .then_with(|| b.updated_at_ms.cmp(&a.updated_at_ms))
        });

        if sessions.is_empty() {
            self.set_selection_items(vec![CompletionItem {
                label: "no saved sessions yet".to_string(),
                detail: String::new(),
                complete_to: String::new(),
                loading: true,
                error: false,
                match_range: None,
            }]);
            return;
        }

        let items = sessions
            .iter()
            .map(|meta| {
                let is_local = meta.cwd == self.current_cwd;
                let scope = if is_local { "local" } else { "foreign" };
                let when =
                    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(meta.updated_at_ms)
                        .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
                        .unwrap_or_else(|| "unknown time".to_string());

                CompletionItem {
                    label: format!("[{scope}] {when}  —  {}", meta.id),
                    detail: format!("{} msgs • {}", meta.message_count, meta.cwd),
                    complete_to: format!("/resume_session {}", meta.id),
                    loading: false,
                    error: false,
                    match_range: None,
                }
            })
            .collect();
        self.set_selection_items(items);
    }

    fn make_textarea() -> TextArea<'static> {
        TextArea::default()
    }

    fn provider_supports_token_refresh(&self) -> bool {
        matches!(
            self.current_provider.as_str(),
            "copilot" | "codex" | "gemini"
        )
    }

    /// Trigger a token refresh for unauthorized errors and set up automatic retry.
    ///
    /// Returns `true` if refresh was triggered, `false` if conditions weren't met
    /// (already refreshing, provider doesn't support refresh, etc.).
    fn trigger_auth_refresh(&mut self, target: RetryTarget) -> bool {
        if !self.provider_supports_token_refresh() || self.refresh_in_progress {
            return false;
        }

        log::debug!(
            "triggering token refresh: provider={} target={:?}",
            self.current_provider,
            target
        );

        self.refresh_in_progress = true;

        match target {
            RetryTarget::AgentTurn => {
                self.retry_after_refresh = true;
            }
            RetryTarget::ModelFetch => {
                self.retry_model_fetch_after_refresh = true;
            }
        }

        let provider = self.current_provider.clone();
        let tx = self.login_tx.clone();
        tokio::spawn(async move {
            auth::refresh_provider(&provider, tx).await;
        });

        true
    }

    /// Check if the current provider's token needs proactive refresh before
    /// making a request. If so, trigger refresh and return `true`.
    ///
    /// This prevents requests from failing mid-flight due to known token expiry.
    /// Guards: only triggers when not already streaming and not already refreshing.
    fn check_token_preflight(&mut self, target: RetryTarget) -> bool {
        if self.streaming || self.refresh_in_progress || !self.provider_supports_token_refresh() {
            return false;
        }

        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        let state = match auth::token_state(
            &self.current_provider,
            now_secs,
            auth::AUTH_REFRESH_LEEWAY_SECS,
        ) {
            Ok(s) => s,
            Err(e) => {
                log::warn!("preflight token check failed: {e}");
                return false;
            }
        };

        match state {
            auth::AuthTokenState::Expired | auth::AuthTokenState::ExpiringSoon => {
                log::debug!(
                    "preflight: token {:?}, triggering refresh before request",
                    state
                );
                self.trigger_auth_refresh(target)
            }
            _ => false,
        }
    }

    /// Reset the chat input area to a blank state between submissions.
    /// Also clears any active completion state.
    pub fn reset_textarea(&mut self) {
        self.textarea = Self::make_textarea();
        self.completions.clear();
        self.completion_selected = 0;
    }

    pub fn shell_input_is_empty(&self) -> bool {
        self.shell_textarea
            .lines()
            .iter()
            .all(|line| line.trim().is_empty())
    }

    pub fn enter_shell_mode(&mut self) {
        self.input_mode = InputMode::Shell;
        self.shell_textarea = Self::make_textarea();
        self.completions.clear();
        self.completion_selected = 0;
    }

    pub fn exit_shell_mode(&mut self) {
        self.input_mode = InputMode::Chat;
        self.shell_textarea = Self::make_textarea();
    }

    pub fn cycle_shell(&mut self) {
        if self.available_shells.len() <= 1 {
            return;
        }
        let idx = self
            .available_shells
            .iter()
            .position(|s| *s == self.selected_shell)
            .unwrap_or(0);
        self.selected_shell = self.available_shells[(idx + 1) % self.available_shells.len()];
    }

    pub fn submit_shell_command(&mut self) {
        let lines: Vec<String> = self.shell_textarea.lines().to_vec();
        let command = lines.join("\n").trim().to_string();
        if command.is_empty() || self.streaming || self.login_active {
            return;
        }

        let cwd = if self.current_cwd.is_empty() {
            ".".to_string()
        } else {
            self.current_cwd.clone()
        };
        let prompt = self.selected_shell.prompt_char();

        let cmd_prefix = if self.available_shells.len() > 1 {
            format!("[{}] {}{}", self.selected_shell.label(), cwd, prompt)
        } else {
            format!("{}{}", cwd, prompt)
        };

        let call_id = format!("local-shell-{}", self.messages.len());
        let mut call_msg = Message::tool_call(
            call_id.clone(),
            "local_shell",
            serde_json::json!({
                "prefix": cmd_prefix,
                "command": command,
            }),
        );
        call_msg.include_in_llm = false;
        self.messages.push(call_msg);

        let output = shell::run_shell_command_blocking(self.selected_shell, &cwd, &command);
        let mut body = String::new();
        if !output.stdout.is_empty() {
            body.push_str(&output.stdout);
            if !output.stdout.ends_with('\n') {
                body.push('\n');
            }
        }
        if !output.stderr.is_empty() {
            body.push_str(&output.stderr);
            if !output.stderr.ends_with('\n') {
                body.push('\n');
            }
        }
        if output.exit_code != 0 {
            body.push_str(&format!("exit {}\n", output.exit_code));
        }

        let mut out_msg = Message::tool_result(call_id, body, output.exit_code != 0);
        out_msg.include_in_llm = false;
        self.messages.push(out_msg);
        self.bump_log_revision();

        self.persist_messages();
        self.exit_shell_mode();
        self.auto_scroll = true;
    }

    /// True when the input is a single line beginning with `/`.
    pub fn in_slash_mode(&self) -> bool {
        let lines = self.textarea.lines();
        lines.len() == 1 && lines[0].trim_start().starts_with('/')
    }

    /// Resolve which slash-command text should execute when Enter is pressed.
    ///
    /// If a completion row is highlighted, prefer its `complete_to` text so
    /// partial inputs like `/mo` execute the selected command immediately.
    pub fn slash_submit_text(&self) -> Option<String> {
        let lines = self.textarea.lines();
        if lines.len() != 1 {
            return None;
        }

        let raw = lines[0].trim().to_string();
        if !raw.starts_with('/') {
            return None;
        }

        if let Some(item) = self.completions.get(self.completion_selected)
            && !item.loading
            && !item.complete_to.is_empty()
        {
            return Some(item.complete_to.trim_end().to_string());
        }

        Some(raw)
    }

    /// Handle `Esc` in normal chat-input mode (outside shell/selection).
    ///
    /// Priority order is intentional:
    /// 1) cancel pending ask
    /// 2) cancel slash-command input/completion
    /// 3) cancel Ollama endpoint input
    /// 4) cancel Open WebUI setup input
    /// 5) cancel login flow
    /// 6) abort streaming agent loop
    pub fn handle_escape_in_chat_mode(&mut self) {
        if self.has_pending_ask() {
            self.cancel_pending_ask();
        } else if self.in_slash_mode() {
            self.reset_textarea();
        } else if self.ollama_endpoint_input_mode {
            self.cancel_ollama_endpoint_input();
        } else if self.open_webui_url_input_mode {
            self.cancel_open_webui_url_input();
        } else if self.open_webui_token_input_mode {
            self.cancel_open_webui_token_input();
        } else if self.login_active {
            self.cancel_login();
        } else if self.streaming {
            self.abort_agent_loop();
        }
    }

    // ── Completion helpers ────────────────────────────────────────────────────

    /// Recompute the completion list from the current textarea content and
    /// cached model list. Call this after every keystroke.
    pub fn update_completions(&mut self) {
        let lines = self.textarea.lines().to_vec();
        let input = if lines.len() == 1 {
            lines[0].trim().to_string()
        } else {
            String::new()
        };
        let available = self.available_models.as_deref();
        let loading = self.models_loading;
        let fetch_error = self.model_fetch_error.as_deref();
        let thinking_enabled = ProviderKind::from_name(&self.current_provider)
            .map(|kind| {
                thinking_support_for(&kind, &self.current_model) == ThinkingSupport::Applied
            })
            .unwrap_or(false);
        let new = commands::completions_for(
            &input,
            available,
            loading,
            fetch_error,
            &self.loaded_skills,
            thinking_enabled,
        );

        if new.len() != self.completions.len() {
            self.completion_selected = 0;
        }
        self.completions = new;
    }

    /// Returns true if a model-list fetch should be triggered now.
    pub fn should_fetch_models(&self) -> bool {
        if self.available_models.is_some()
            || self.models_loading
            || self.model_fetch_error.is_some()
        {
            return false;
        }
        let lines = self.textarea.lines();
        lines.len() == 1 && lines[0].trim_start().starts_with("/model ")
    }

    /// Spawn a background task to fetch the model list from the provider.
    pub fn start_model_fetch(&mut self, provider: &DynProvider) {
        // Proactive token refresh check before fetching models
        if self.check_token_preflight(RetryTarget::ModelFetch) {
            // Refresh triggered; fetch will be retried after refresh completes
            return;
        }

        self.models_loading = true;
        self.model_fetch_error = None;
        let future = provider.list_models();
        let tx = self.models_tx.clone();
        tokio::spawn(async move {
            let result = future.await;
            let _ = tx.send(result);
        });
    }

    /// Store a freshly fetched model list (or error) and refresh completions.
    pub fn apply_model_list(&mut self, result: Result<Vec<String>, crate::llm::ProviderError>) {
        self.models_loading = false;
        match result {
            Ok(models) => {
                self.available_models = Some(models);
                self.model_fetch_error = None;
            }
            Err(e) => {
                let is_unauthorized = e.kind == crate::llm::ProviderErrorKind::Unauthorized;

                if is_unauthorized && self.trigger_auth_refresh(RetryTarget::ModelFetch) {
                    // Refresh triggered; retry will happen automatically after refresh completes
                } else {
                    self.model_fetch_error = Some(e.to_string());
                }
            }
        }
        self.update_completions();

        if self.selection_mode && self.selection_kind == Some(SelectionKind::Model) {
            if let Some(err) = &self.model_fetch_error {
                let items = vec![commands::CompletionItem::error_indicator(err)];
                self.set_selection_items(items);
            } else {
                let items: Vec<_> = self
                    .available_models
                    .as_deref()
                    .unwrap_or(&[])
                    .iter()
                    .map(|m| commands::CompletionItem::from_model(m))
                    .collect();
                if !items.is_empty() {
                    self.set_selection_items(items);
                    if self.selection_query.trim().is_empty() {
                        self.select_current_default();
                    }
                }
            }
        }
    }

    /// Navigate the completion selection down (wraps around).
    pub fn completion_select_next(&mut self) {
        let len = self.completions.len();
        if len > 0 {
            self.completion_selected = (self.completion_selected + 1) % len;
        }
    }

    /// Navigate the completion selection up (wraps around).
    pub fn completion_select_prev(&mut self) {
        let len = self.completions.len();
        if len > 0 {
            self.completion_selected = (self.completion_selected + len - 1) % len;
        }
    }

    /// Replace the textarea with the selected item's `complete_to` text and
    /// move the cursor to the end of the line. No-ops on loading indicators.
    pub fn apply_completion(&mut self) {
        let item = match self.completions.get(self.completion_selected) {
            Some(i) if !i.loading && !i.complete_to.is_empty() => i,
            _ => return,
        };
        let text = item.complete_to.clone();
        self.textarea = TextArea::new(vec![text]);
        self.textarea.move_cursor(CursorMove::End);
        self.update_completions();
    }

    // ── Selection menu ────────────────────────────────────────────────────────

    fn set_selection_items(&mut self, items: Vec<CompletionItem>) {
        self.selection_all_items = items;
        self.selection_selected = 0;
        self.selection_scroll = 0;
        self.apply_selection_filter();
    }

    fn select_current_default(&mut self) {
        let target = match self.selection_kind {
            Some(SelectionKind::Model) => Some(format!("/model {}", self.current_model)),
            Some(SelectionKind::Thinking) => {
                Some(format!("/thinking {}", self.current_thinking.as_str()))
            }
            Some(SelectionKind::Provider) => Some(format!("/provider {}", self.current_provider)),
            Some(SelectionKind::LoginProvider)
            | Some(SelectionKind::ResumeSession)
            | Some(SelectionKind::AskUser)
            | Some(SelectionKind::LoginAction)
            | Some(SelectionKind::OllamaEndpoint)
            | None => None,
        };

        if let Some(target) = target
            && let Some(idx) = self
                .selection_items
                .iter()
                .position(|item| item.complete_to == target)
        {
            self.selection_selected = idx;
            self.ensure_selection_visible();
        }
    }

    fn apply_selection_filter(&mut self) {
        let query = self.selection_query.trim();
        if query.is_empty() {
            self.selection_items = self.selection_all_items.clone();
        } else {
            let needle = query.to_lowercase();
            self.selection_items = self
                .selection_all_items
                .iter()
                .filter(|item| {
                    item.label.to_lowercase().contains(&needle)
                        || item.detail.to_lowercase().contains(&needle)
                })
                .cloned()
                .collect();
        }

        if self.selection_items.is_empty() {
            self.selection_selected = 0;
            self.selection_scroll = 0;
            return;
        }

        if self.selection_selected >= self.selection_items.len() {
            self.selection_selected = 0;
        }
        self.ensure_selection_visible();
    }

    fn ensure_selection_visible(&mut self) {
        if self.selection_items.is_empty() {
            self.selection_scroll = 0;
            return;
        }
        if self.selection_selected < self.selection_scroll {
            self.selection_scroll = self.selection_selected;
        }
        if self.selection_selected >= self.selection_scroll + MAX_SELECTION_VISIBLE {
            self.selection_scroll = self.selection_selected + 1 - MAX_SELECTION_VISIBLE;
        }
    }

    /// Open the model selection menu, pre-populating from cache or showing a
    /// loading indicator when the list hasn't been fetched yet.
    pub fn enter_model_selection_mode(&mut self) {
        self.reset_textarea();
        self.selection_mode = true;
        self.selection_kind = Some(SelectionKind::Model);
        self.selection_title = "  Select model  ";
        self.selection_query.clear();
        let items = if let Some(err) = &self.model_fetch_error {
            vec![CompletionItem::error_indicator(err)]
        } else if let Some(models) = &self.available_models {
            models
                .iter()
                .map(|m| CompletionItem::from_model(m))
                .collect()
        } else {
            vec![CompletionItem::loading_indicator()]
        };
        self.set_selection_items(items);
        self.select_current_default();
    }

    /// Open the thinking-level selection menu.
    pub fn enter_thinking_selection_mode(&mut self) {
        self.reset_textarea();
        self.selection_mode = true;
        self.selection_kind = Some(SelectionKind::Thinking);
        self.selection_title = "  Select thinking  ";
        self.selection_query.clear();
        let items = ThinkingLevel::all()
            .iter()
            .map(|lvl| CompletionItem {
                label: lvl.as_str().to_string(),
                detail: String::new(),
                complete_to: format!("/thinking {}", lvl.as_str()),
                loading: false,
                error: false,
                match_range: None,
            })
            .collect();
        self.set_selection_items(items);
        self.select_current_default();
    }

    /// Open the provider selection menu with the fixed list of known providers.
    pub fn enter_provider_selection_mode(&mut self) {
        self.reset_textarea();
        self.selection_mode = true;
        self.selection_kind = Some(SelectionKind::Provider);
        self.selection_title = "  Select provider  ";
        let items = ProviderKind::all()
            .iter()
            .map(|p| CompletionItem::from_provider(p.name(), p.label()))
            .collect();
        self.set_selection_items(items);
        self.select_current_default();
    }

    /// Open the Ollama endpoint picker showing recent endpoints and a
    /// "Enter URL…" sentinel at the bottom.
    pub fn enter_ollama_endpoint_selection_mode(&mut self, recent: Vec<String>) {
        self.reset_textarea();
        self.selection_mode = true;
        self.selection_kind = Some(SelectionKind::OllamaEndpoint);
        self.selection_title = "  Ollama endpoint  ";
        self.selection_query.clear();

        let mut items: Vec<CompletionItem> = recent
            .into_iter()
            .map(|url| CompletionItem {
                label: url.clone(),
                detail: String::new(),
                complete_to: format!("/ollama_endpoint {url}"),
                loading: false,
                error: false,
                match_range: None,
            })
            .collect();

        // Sentinel item at the bottom to let the user type a custom URL.
        items.push(CompletionItem {
            label: "Enter URL…".to_string(),
            detail: String::new(),
            complete_to: "/ollama_endpoint_freeform".to_string(),
            loading: false,
            error: false,
            match_range: None,
        });

        self.set_selection_items(items);
    }

    /// Switch the textarea into Ollama endpoint freeform input mode.
    /// The UI will render a `ollama endpoint: ` prefix and submit the text
    /// as a new endpoint URL when Enter is pressed.
    pub fn enter_ollama_endpoint_freeform_mode(&mut self) {
        self.exit_selection_mode();
        self.reset_textarea();
        self.ollama_endpoint_input_mode = true;
    }

    /// Cancel Ollama endpoint freeform input and return to normal mode.
    pub fn cancel_ollama_endpoint_input(&mut self) {
        self.ollama_endpoint_input_mode = false;
        self.reset_textarea();
    }

    /// Normalize a user-entered Ollama endpoint.
    ///
    /// Accepted shorthand forms:
    /// - `host`               → `http://host:11434`
    /// - `host:1234`          → `http://host:1234`
    /// - `http://host`        → `http://host:11434`
    /// - `https://host`       → `https://host:11434`
    /// - `http://host:1234`   → unchanged
    /// - `https://host:1234`  → unchanged
    ///
    /// Returns `None` for empty input or values that still do not parse as an
    /// absolute HTTP(S) URL after normalization.
    fn normalize_ollama_endpoint(raw: &str) -> Option<String> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return None;
        }

        let with_scheme = if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
            trimmed.to_string()
        } else {
            format!("http://{trimmed}")
        };

        let mut url = reqwest::Url::parse(&with_scheme).ok()?;

        match url.scheme() {
            "http" | "https" => {}
            _ => return None,
        }

        url.host_str()?;

        if url.port().is_none() {
            url.set_port(Some(11434)).ok()?;
        }

        Some(url.to_string().trim_end_matches('/').to_string())
    }

    /// Read the textarea as an Ollama endpoint URL, normalize shorthand
    /// forms, and return `Some(url)` if it looks valid, `None` otherwise.
    pub fn take_ollama_endpoint_input(&mut self) -> Option<String> {
        let raw = self.textarea.lines().join("");
        let url = Self::normalize_ollama_endpoint(&raw)?;
        self.ollama_endpoint_input_mode = false;
        self.reset_textarea();
        Some(url)
    }

    // ── Open WebUI interactive setup ──────────────────────────────────────────

    /// Enter URL input mode for Open WebUI setup.
    pub fn enter_open_webui_url_input_mode(&mut self) {
        self.exit_selection_mode();
        self.reset_textarea();
        self.open_webui_url_input_mode = true;
    }

    /// Cancel Open WebUI URL input mode.
    pub fn cancel_open_webui_url_input(&mut self) {
        self.open_webui_url_input_mode = false;
        self.open_webui_pending_url = None;
        self.reset_textarea();
    }

    /// Normalize a user-entered Open WebUI URL (must be http/https, no default port).
    pub fn normalize_open_webui_url(raw: &str) -> Option<String> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return None;
        }
        let with_scheme = if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
            trimmed.to_string()
        } else {
            format!("https://{trimmed}")
        };
        // Basic parse check — just ensure host is present.
        let url = reqwest::Url::parse(&with_scheme).ok()?;
        url.host_str()?;
        Some(with_scheme.trim_end_matches('/').to_string())
    }

    /// Submit the URL typed in Open WebUI URL input mode.
    /// Returns the normalised URL if valid, and transitions to token input mode.
    pub fn submit_open_webui_url_input(&mut self) -> Option<String> {
        let raw = self.textarea.lines().join("");
        let url = Self::normalize_open_webui_url(&raw)?;
        self.open_webui_url_input_mode = false;
        self.open_webui_pending_url = Some(url.clone());
        self.reset_textarea();
        // Immediately transition to token input mode.
        self.open_webui_token_input_mode = true;
        Some(url)
    }

    /// Cancel Open WebUI token input mode.
    pub fn cancel_open_webui_token_input(&mut self) {
        self.open_webui_token_input_mode = false;
        self.open_webui_pending_url = None;
        self.reset_textarea();
    }

    /// Submit the token typed in Open WebUI token input mode.
    /// Returns `Some((url, token))` if a pending URL exists and the token is non-empty.
    pub fn take_open_webui_token_input(&mut self) -> Option<(String, String)> {
        let token = self.textarea.lines().join("").trim().to_string();
        if token.is_empty() {
            return None;
        }
        let url = self.open_webui_pending_url.take()?;
        self.open_webui_token_input_mode = false;
        self.reset_textarea();
        Some((url, token))
    }

    /// Open provider picker for `/login` command.
    pub fn enter_login_selection_mode(&mut self) {
        self.reset_textarea();
        self.selection_mode = true;
        self.selection_kind = Some(SelectionKind::LoginProvider);
        self.selection_title = "  Login provider  ";
        self.selection_query.clear();
        let items = ["copilot", "codex", "gemini"]
            .iter()
            .map(|p| CompletionItem {
                label: (*p).to_string(),
                detail: String::new(),
                complete_to: format!("/login {p}"),
                loading: false,
                error: false,
                match_range: None,
            })
            .collect();
        self.set_selection_items(items);
    }

    /// Dismiss the selection menu without applying a choice.
    pub fn exit_selection_mode(&mut self) {
        self.selection_mode = false;
        self.selection_kind = None;
        self.selection_items.clear();
        self.selection_all_items.clear();
        self.selection_query.clear();
        self.selection_selected = 0;
        self.selection_scroll = 0;
    }

    /// Returns true when a model fetch should be triggered for the model
    /// selection menu (models not yet loaded, no fetch in flight).
    pub fn should_fetch_models_for_selection(&self) -> bool {
        // Only fetch if the menu shows the loading indicator (model menu).
        self.selection_mode
            && self.selection_kind == Some(SelectionKind::Model)
            && self.selection_items.iter().any(|i| i.loading)
            && !self.models_loading
    }

    /// Returns true if the active selection menu supports free-text filtering.
    /// The login-action menu is a fixed short list and disables filtering.
    pub fn selection_filter_enabled(&self) -> bool {
        self.selection_kind != Some(SelectionKind::LoginAction)
    }

    pub fn selection_add_char(&mut self, c: char) {
        // The login action menu is a small fixed list; filtering adds no value.
        if self.selection_kind == Some(SelectionKind::LoginAction) {
            return;
        }
        self.selection_query.push(c);
        self.apply_selection_filter();
    }

    pub fn selection_backspace(&mut self) {
        if self.selection_kind == Some(SelectionKind::LoginAction) {
            return;
        }
        self.selection_query.pop();
        self.apply_selection_filter();
    }

    /// Navigate the selection menu down (wraps around).
    pub fn selection_select_next(&mut self) {
        let len = self.selection_items.len();
        if len > 0 {
            self.selection_selected = (self.selection_selected + 1) % len;
            if self.selection_selected == 0 {
                self.selection_scroll = 0;
            } else {
                self.ensure_selection_visible();
            }
        }
    }

    /// Navigate the selection menu up (wraps around).
    pub fn selection_select_prev(&mut self) {
        let len = self.selection_items.len();
        if len > 0 {
            self.selection_selected = (self.selection_selected + len - 1) % len;
            if self.selection_selected == len - 1 {
                self.selection_scroll = len.saturating_sub(MAX_SELECTION_VISIBLE);
            } else {
                self.ensure_selection_visible();
            }
        }
    }

    /// Confirm the currently highlighted selection.
    pub fn apply_selection(&mut self) -> Option<SelectionResult> {
        let item = self.selection_items.get(self.selection_selected)?;
        if item.loading || item.complete_to.is_empty() {
            return None;
        }

        let result = match self.selection_kind {
            Some(SelectionKind::Model) => item
                .complete_to
                .strip_prefix("/model ")
                .map(|name| SelectionResult::Model(name.to_string())),
            Some(SelectionKind::Thinking) => item
                .complete_to
                .strip_prefix("/thinking ")
                .and_then(ThinkingLevel::parse)
                .map(SelectionResult::Thinking),
            Some(SelectionKind::Provider) => item
                .complete_to
                .strip_prefix("/provider ")
                .map(|name| SelectionResult::Provider(name.to_string())),
            Some(SelectionKind::LoginProvider) => item
                .complete_to
                .strip_prefix("/login ")
                .map(|name| SelectionResult::LoginProvider(name.to_string())),
            Some(SelectionKind::ResumeSession) => item
                .complete_to
                .strip_prefix("/resume_session ")
                .map(|id| SelectionResult::ResumeSession(id.to_string())),
            Some(SelectionKind::AskUser) => item
                .complete_to
                .strip_prefix("/ask_user_option ")
                .map(|name| SelectionResult::AskOption(name.to_string()))
                .or_else(|| {
                    (item.complete_to == "/ask_user_freeform")
                        .then_some(SelectionResult::AskFreeform)
                }),
            Some(SelectionKind::LoginAction) => match item.complete_to.as_str() {
                Self::LOGIN_ACTION_OPEN_BROWSER => {
                    Some(SelectionResult::LoginAction(LoginActionKind::OpenBrowser))
                }
                Self::LOGIN_ACTION_COPY_URL => {
                    Some(SelectionResult::LoginAction(LoginActionKind::CopyUrl))
                }
                Self::LOGIN_ACTION_COPY_CODE => {
                    Some(SelectionResult::LoginAction(LoginActionKind::CopyCode))
                }
                Self::LOGIN_ACTION_CANCEL => {
                    Some(SelectionResult::LoginAction(LoginActionKind::Cancel))
                }
                _ => None,
            },
            Some(SelectionKind::OllamaEndpoint) => {
                if item.complete_to == "/ollama_endpoint_freeform" {
                    Some(SelectionResult::OllamaEndpointFreeform)
                } else {
                    item.complete_to
                        .strip_prefix("/ollama_endpoint ")
                        .map(|url| SelectionResult::OllamaEndpoint(url.to_string()))
                }
            }
            None => None,
        }?;

        self.exit_selection_mode();
        Some(result)
    }

    pub fn has_pending_ask(&self) -> bool {
        self.pending_ask.is_some()
    }

    pub fn receive_ask_request(&mut self, req: AskRequest) {
        let AskRequest {
            question,
            context: _context,
            options,
            allow_multiple: _allow_multiple,
            allow_freeform,
            reply,
        } = req;

        self.pending_ask = Some(PendingAsk {
            options: options.clone(),
            allow_freeform,
        });
        self.ask_reply = Some(reply);

        // Don't push an [ask_user] assistant message to app.messages — the
        // agent's ToolCall message already represents this in the conversation
        // history and UI. Adding an extra assistant message here would corrupt
        // the tool_use / tool_result pairing expected by the Anthropic API.

        if options.is_empty() {
            // No options: go straight to freeform input so the user can type
            // their answer without an intermediate selection-menu step.
            // Store the question for display in the input area.
            self.ask_user_freeform_mode = true;
            self.ask_user_question = Some(question);
            self.exit_selection_mode();
            self.reset_textarea();
            return;
        }

        self.reset_textarea();
        self.selection_mode = true;
        self.selection_kind = Some(SelectionKind::AskUser);
        self.selection_title = "  Ask user  ";
        self.selection_query.clear();

        let mut items: Vec<CompletionItem> = options
            .iter()
            .map(|opt| CompletionItem {
                label: opt.title.clone(),
                detail: opt.description.clone().unwrap_or_default(),
                complete_to: format!("/ask_user_option {}", opt.title),
                loading: false,
                error: false,
                match_range: None,
            })
            .collect();

        // Include freeform sentinel when options are present but allow_freeform is true.
        if allow_freeform {
            items.push(CompletionItem {
                label: "Type your response…".to_string(),
                detail: String::new(),
                complete_to: "/ask_user_freeform".to_string(),
                loading: false,
                error: false,
                match_range: None,
            });
        }

        self.set_selection_items(items);
    }

    pub fn enter_ask_freeform_mode(&mut self) {
        self.exit_selection_mode();
        self.reset_textarea();
    }

    pub fn submit_pending_ask_answer(&mut self) {
        let Some(pending) = self.pending_ask.as_ref() else {
            return;
        };

        if !pending.allow_freeform && !pending.options.is_empty() {
            return;
        }

        let text = self.textarea.lines().join("\n").trim().to_string();
        if text.is_empty() {
            return;
        }

        // Don't push the answer as a plain user message — the agent's
        // ToolResult message will represent it in history and UI.
        self.finish_pending_ask(AskUserResponse::Answer(text));
    }

    pub fn select_pending_ask_option(&mut self, answer: String) {
        if self.pending_ask.is_none() {
            return;
        }
        // Don't push the answer as a plain user message — the agent's
        // ToolResult message will represent it in history and UI.
        self.finish_pending_ask(AskUserResponse::Answer(answer));
    }

    pub fn cancel_pending_ask(&mut self) {
        if self.pending_ask.is_none() {
            return;
        }
        self.finish_pending_ask(AskUserResponse::Cancelled);
        self.abort_agent_loop();
    }

    fn finish_pending_ask(&mut self, answer: AskUserResponse) {
        if let Some(reply) = self.ask_reply.take() {
            let _ = reply.send(answer);
        }
        self.pending_ask = None;
        self.ask_user_freeform_mode = false;
        self.ask_user_question = None;
        self.exit_selection_mode();
        self.reset_textarea();
    }

    // ── Login panel actions ───────────────────────────────────────────────────

    // Internal complete_to tokens for login action items.
    const LOGIN_ACTION_OPEN_BROWSER: &str = "/login_action open_browser";
    const LOGIN_ACTION_COPY_URL: &str = "/login_action copy_url";
    const LOGIN_ACTION_COPY_CODE: &str = "/login_action copy_code";
    const LOGIN_ACTION_CANCEL: &str = "/login_action cancel";

    /// Build a single login-action `CompletionItem`.
    fn login_action_item(label: &str, detail: &str, token: &str) -> CompletionItem {
        CompletionItem {
            label: label.to_string(),
            detail: detail.to_string(),
            complete_to: token.to_string(),
            loading: false,
            error: false,
            match_range: None,
        }
    }

    /// Open the action selection menu for the active login panel.
    ///
    /// Items are populated based on what is currently available:
    /// - "Open browser" and "Copy URL" only when a URL has arrived
    /// - "Copy code" only when a device code is present (Copilot flow)
    /// - "Cancel" always
    pub fn enter_login_action_menu(&mut self) {
        if !self.login_active {
            return;
        }

        let mut items: Vec<CompletionItem> = Vec::new();
        if self.login_url.is_some() {
            items.push(Self::login_action_item(
                "Open browser",
                "Launch the authentication URL in your default browser",
                Self::LOGIN_ACTION_OPEN_BROWSER,
            ));
            items.push(Self::login_action_item(
                "Copy URL",
                "Copy the authentication URL to the clipboard",
                Self::LOGIN_ACTION_COPY_URL,
            ));
        }
        if self.login_code.is_some() {
            items.push(Self::login_action_item(
                "Copy code",
                "Copy the device code to the clipboard",
                Self::LOGIN_ACTION_COPY_CODE,
            ));
        }
        items.push(Self::login_action_item(
            "Cancel",
            "Abort the login flow",
            Self::LOGIN_ACTION_CANCEL,
        ));

        self.selection_mode = true;
        self.selection_kind = Some(SelectionKind::LoginAction);
        self.selection_title = "  Login actions  ";
        self.selection_query.clear();
        self.set_selection_items(items);
    }

    /// Execute a login action chosen from the action menu.
    pub fn apply_login_action(&mut self, action: LoginActionKind) {
        // Always close the menu first so the login panel is visible behind
        // the feedback message written to login_info.
        self.exit_selection_mode();

        match action {
            LoginActionKind::OpenBrowser => {
                let Some(url) = self.login_url.clone() else {
                    return;
                };
                match auth::open_url::open_url(&url) {
                    Ok(()) => {
                        log::debug!("login: opened browser for {url}");
                        self.login_info = "Browser opened.".to_string();
                    }
                    Err(e) => {
                        log::debug!("login: failed to open browser: {e}");
                        self.login_info =
                            format!("Could not open browser: {e}. Copy the URL manually.");
                    }
                }
            }
            LoginActionKind::CopyUrl => {
                let Some(url) = self.login_url.clone() else {
                    return;
                };
                match self.clipboard_set(url) {
                    Ok(()) => {
                        log::debug!("login: copied URL to clipboard");
                        self.login_info = "URL copied to clipboard.".to_string();
                    }
                    Err(e) => {
                        log::debug!("login: clipboard unavailable: {e}");
                        self.login_info =
                            "Clipboard unavailable — select the URL above to copy.".to_string();
                    }
                }
            }
            LoginActionKind::CopyCode => {
                let Some(code) = self.login_code.clone() else {
                    return;
                };
                match self.clipboard_set(code) {
                    Ok(()) => {
                        log::debug!("login: copied device code to clipboard");
                        self.login_info = "Code copied to clipboard.".to_string();
                    }
                    Err(e) => {
                        log::debug!("login: clipboard unavailable: {e}");
                        self.login_info =
                            "Clipboard unavailable — type the code shown above manually."
                                .to_string();
                    }
                }
            }
            LoginActionKind::Cancel => {
                self.cancel_login();
            }
        }
    }

    /// Copy `text` to the clipboard using the persistent `self.clipboard`
    /// instance. Lazily initialises it on first call. Returns an error
    /// string on failure.
    fn clipboard_set(&mut self, text: String) -> Result<(), String> {
        // Lazily open the clipboard and keep it alive for the whole login
        // session. On Linux the clipboard is owner-based: dropping the
        // Clipboard instance clears the content for other applications.
        if self.clipboard.is_none() {
            match arboard::Clipboard::new() {
                Ok(cb) => self.clipboard = Some(cb),
                Err(e) => return Err(e.to_string()),
            }
        }
        self.clipboard
            .as_mut()
            .unwrap()
            .set_text(text)
            .map_err(|e| e.to_string())
    }

    pub fn start_login(&mut self, provider: &str) {
        if self.login_active {
            return;
        }

        log::debug!("login start requested: provider={provider}");

        self.login_active = true;
        self.login_provider = Some(provider.to_string());
        self.login_info = format!("Starting login for {provider}...");
        self.login_url = None;
        self.login_code = None;
        self.login_auth_flow = None;

        let cancel = Arc::new(AtomicBool::new(false));
        self.login_cancel = Some(cancel.clone());
        let tx = self.login_tx.clone();
        let provider = provider.to_string();

        tokio::spawn(async move {
            auth::login_provider(&provider, tx, cancel).await;
        });
    }

    pub fn cancel_login(&mut self) {
        if let Some(cancel) = &self.login_cancel {
            log::debug!("login cancel requested");
            cancel.store(true, Ordering::Relaxed);
        }
    }

    pub fn apply_login_event(&mut self, ev: LoginEvent) {
        match ev {
            LoginEvent::Info(msg) => {
                log::debug!("login info: {msg}");
                self.login_info = msg;
            }
            LoginEvent::AuthCode { url, code, flow } => {
                log::debug!("login auth prompt: url={} has_code={}", url, code.is_some());
                self.login_url = Some(url);
                self.login_code = code;
                self.login_auth_flow = Some(flow);
                // Automatically open the action menu so the user can choose
                // how to proceed without needing to know any keyboard shortcuts.
                self.enter_login_action_menu();
            }
            LoginEvent::Success { provider } => {
                log::debug!("login success: provider={provider}");
                self.messages.push(Message::assistant(format!(
                    "[login successful: {provider}]"
                )));
                self.bump_log_revision();
                self.persist_messages();
                self.login_needs_rebuild = true;
            }
            LoginEvent::Error { provider, message } => {
                log::debug!("login error: provider={} err={}", provider, message);
                self.messages.push(Message::assistant(format!(
                    "[login failed for {provider}: {message}]"
                )));
                self.bump_log_revision();
                self.persist_messages();
            }
            LoginEvent::RefreshResult {
                provider,
                success,
                message,
            } => {
                log::debug!(
                    "token refresh result: provider={} success={} msg={}",
                    provider,
                    success,
                    message
                );
                self.refresh_in_progress = false;
                if success {
                    // Silently refresh — no message added to the chat log or
                    // LLM history; the retry will continue seamlessly.
                    self.login_needs_rebuild = true;
                } else {
                    self.retry_after_refresh = false;
                    self.messages.push(Message::assistant(format!(
                        "[token refresh failed for {provider}: {message}. Run /login {provider}]"
                    )));
                    self.bump_log_revision();
                    self.persist_messages();
                }
            }
            LoginEvent::Finished => {
                log::debug!("login flow finished");
                self.login_active = false;
                self.login_provider = None;
                self.login_cancel = None;
                self.login_auth_flow = None;
                self.exit_selection_mode();
                // Drop the clipboard instance; on Linux this releases clipboard
                // ownership so the content is no longer served by this process.
                self.clipboard = None;
            }
        }
    }

    // ── Conversation management ───────────────────────────────────────────────

    fn refresh_resume_availability(&mut self) {
        self.resume_available_for_cwd = self
            .session_store
            .as_ref()
            .and_then(|s| s.latest_for_cwd(&self.current_cwd))
            .is_some();
    }

    /// Return the current session ID, creating a new session if one does not
    /// yet exist.  Falls back to `"unknown"` if persistence is unavailable.
    fn ensure_session_id(&mut self) -> String {
        if let Some(ref id) = self.current_session_id {
            return id.clone();
        }
        if let Some(ref mut store) = self.session_store {
            match store.create_session(&self.current_cwd) {
                Ok(id) => {
                    self.current_session_id = Some(id.clone());
                    return id;
                }
                Err(e) => {
                    log::debug!("failed to create session for tool output log: {e}");
                }
            }
        }
        "unknown".to_string()
    }

    fn persist_messages(&mut self) {
        let Some(store) = self.session_store.as_mut() else {
            return;
        };

        let session_id = match self.current_session_id.clone() {
            Some(id) => id,
            None => match store.create_session(&self.current_cwd) {
                Ok(id) => {
                    self.current_session_id = Some(id.clone());
                    id
                }
                Err(e) => {
                    log::debug!("failed to create session: {}", e);
                    return;
                }
            },
        };

        if let Err(e) = store.save_messages(&session_id, &self.current_cwd, &self.messages) {
            log::debug!("failed to persist session {}: {}", session_id, e);
        }
        self.refresh_resume_availability();
    }

    /// Export the current visible session to a standalone HTML file.
    pub fn export_session_html(&mut self, requested_path: Option<&str>) {
        let path = export::resolve_export_path(&self.current_cwd, requested_path);
        let html = export::build_session_export_html(
            &self.messages,
            &self.current_cwd,
            &self.current_provider,
            &self.current_model,
            self.current_session_id.as_deref(),
        );

        match export::write_export_file(&path, &html) {
            Ok(()) => {
                self.messages.push(Message::assistant(format!(
                    "[session exported to {}]",
                    path.display()
                )));
            }
            Err(e) => {
                self.messages
                    .push(Message::assistant(format!("[export failed: {e}]")));
            }
        }
        self.bump_log_revision();
        self.persist_messages();
    }

    /// Clear the conversation history and reset the input area.
    pub fn new_conversation(&mut self) {
        self.messages.clear();
        self.current_session_id = None;
        self.queued_steering.clear();
        self.steering_tx = None;
        self.latest_usage = None;
        self.reset_textarea();
        self.auto_scroll = true;
        self.bump_log_revision();
        self.refresh_resume_availability();
    }

    // ── LLM submission ────────────────────────────────────────────────────────

    fn start_agent_task(&mut self, llm_messages: Vec<Message>, provider: &DynProvider) {
        // Ensure the session ID is assigned before creating the log so the
        // output directory uses the real session key, not the "init" placeholder.
        let session_id = self.ensure_session_id();
        // Replace the agent_config log with one keyed to the real session ID.
        // Keeping it in agent_config ensures it outlives the task and the files
        // remain accessible after the agent loop completes.
        self.agent_config.tool_output_log =
            Arc::new(std::sync::Mutex::new(ToolOutputLog::new(&session_id)));
        let config = AgentLoopConfig {
            tools: self.agent_config.tools.clone(),
            file_tracker: Arc::clone(&self.agent_config.file_tracker),
            tool_output_log: Arc::clone(&self.agent_config.tool_output_log),
            before_tool_call: None,
            after_tool_call: None,
        };
        let (steering_tx, steering_rx) = tokio::sync::mpsc::unbounded_channel();
        self.steering_tx = Some(steering_tx);
        self.queued_steering.clear();
        self.bump_log_revision();

        let provider = Arc::clone(provider);
        let tx = self.event_tx.clone();
        self.agent_task = Some(tokio::spawn(async move {
            run_agent_loop(llm_messages, config, provider, tx, steering_rx).await;
        }));
    }

    /// Queue a user steering message while the agent loop is running.
    pub fn enqueue_steering_from_input(&mut self) {
        let lines: Vec<String> = self.textarea.lines().to_vec();
        let text = lines.join("\n");
        let trimmed = text.trim().to_string();
        if trimmed.is_empty() || !self.streaming || self.login_active {
            return;
        }

        let Some(tx) = self.steering_tx.as_ref() else {
            return;
        };

        if tx.send(trimmed.clone()).is_ok() {
            self.queued_steering.push(trimmed);
            self.bump_log_revision();
            self.reset_textarea();
            self.auto_scroll = true;
        }
    }

    /// Take the textarea content and start the agent loop.
    pub fn submit(&mut self, provider: &DynProvider) {
        let lines: Vec<String> = self.textarea.lines().to_vec();
        let text = lines.join("\n");
        let trimmed = text.trim().to_string();
        if trimmed.is_empty() || self.streaming || self.login_active {
            return;
        }

        self.messages.push(Message::user(trimmed));
        self.bump_log_revision();
        self.persist_messages();
        self.reset_textarea();
        self.latest_usage = None;
        self.auto_scroll = true;

        // Proactive token refresh check before starting the request
        if self.check_token_preflight(RetryTarget::AgentTurn) {
            // Refresh triggered; request will be retried after refresh completes
            return;
        }

        self.streaming = true;
        self.auth_retry_budget = 1;

        let mut llm_messages: Vec<Message> = self
            .system_prompt
            .iter()
            .map(Message::system)
            .chain(self.messages.iter().filter(|m| m.include_in_llm).cloned())
            .collect();

        if matches!(llm_messages.last().map(|m| &m.role), Some(Role::Assistant))
            && llm_messages
                .last()
                .map(|m| m.content.is_empty())
                .unwrap_or(false)
        {
            llm_messages.pop();
        }

        self.start_agent_task(llm_messages, provider);
    }

    /// Submit a pre-built text string directly to the agent loop, bypassing the
    /// textarea.  Used by `/skill:<name>` command expansion.
    pub fn submit_with_text(&mut self, text: String, provider: &DynProvider) {
        if text.trim().is_empty() || self.streaming || self.login_active {
            return;
        }

        let sanitized = text.trim().to_string();
        let msg = Message::user(sanitized);
        self.messages.push(msg);
        self.bump_log_revision();
        self.persist_messages();
        self.reset_textarea();
        self.latest_usage = None;
        self.auto_scroll = true;

        // Proactive token refresh check before starting the request
        if self.check_token_preflight(RetryTarget::AgentTurn) {
            // Refresh triggered; request will be retried after refresh completes
            return;
        }

        self.streaming = true;
        self.auth_retry_budget = 1;

        let llm_messages: Vec<Message> = self
            .system_prompt
            .iter()
            .map(Message::system)
            .chain(self.messages.iter().filter(|m| m.include_in_llm).cloned())
            .collect();

        self.start_agent_task(llm_messages, provider);
    }

    pub fn retry_last_request(&mut self, provider: &DynProvider) {
        if self.streaming || self.login_active {
            return;
        }

        if let Some(last) = self.messages.last()
            && last.role == Role::Assistant
            && (last.content.starts_with("[Error:") || last.content.starts_with("[token refresh"))
        {
            self.messages.pop();
            self.bump_log_revision();
            self.persist_messages();
        }

        self.streaming = true;
        self.latest_usage = None;
        self.auto_scroll = true;

        let llm_messages: Vec<Message> = self
            .system_prompt
            .iter()
            .map(Message::system)
            .chain(self.messages.iter().filter(|m| m.include_in_llm).cloned())
            .collect();

        self.start_agent_task(llm_messages, provider);
    }

    /// Remove any trailing `ToolCall` messages that have no paired `ToolResult`.
    ///
    /// This can happen when the agent loop is aborted after the model emits a
    /// `ToolCall` but before the tool finishes and its `ToolResult` is appended.
    /// Leaving orphaned `ToolCall` messages in the history causes subsequent LLM
    /// requests to fail because every tool call must be accompanied by a result.
    ///
    /// The method also removes a preceding empty provisional `Assistant` message
    /// if one was added solely to hold the tool-intent phase marker.
    fn strip_orphaned_tool_calls(&mut self) {
        // Collect the IDs of all tool calls that already have a result.
        let paired_ids: std::collections::HashSet<String> = self
            .messages
            .iter()
            .filter(|m| m.role == Role::ToolResult)
            .filter_map(|m| m.tool_call_id.clone())
            .collect();

        // Remove ToolCall messages whose IDs are not in paired_ids.
        self.messages.retain(|m| {
            if m.role == Role::ToolCall {
                m.tool_call_id
                    .as_ref()
                    .map(|id| paired_ids.contains(id))
                    .unwrap_or(false)
            } else {
                true
            }
        });

        // If the last message is now an empty provisional assistant message
        // (written only to hold the ToolIntentStart phase), drop it too so the
        // history ends cleanly on the last complete user/assistant exchange.
        if let Some(last) = self.messages.last()
            && last.role == Role::Assistant
            && last.content.is_empty()
            && last.thinking.is_none()
            && last.assistant_phase == Some(AssistantPhase::Provisional)
        {
            self.messages.pop();
        }
    }

    pub fn abort_agent_loop(&mut self) {
        if let Some(handle) = self.agent_task.take() {
            handle.abort();
            self.streaming = false;
            self.last_output_at = None;
            self.steering_tx = None;
            self.queued_steering.clear();
            self.strip_orphaned_tool_calls();
            self.messages
                .push(Message::assistant("[agent loop aborted]"));
            self.bump_log_revision();
            self.persist_messages();
        }
    }

    // ── Scrolling ─────────────────────────────────────────────────────────────

    pub fn scroll_up(&mut self) {
        self.scroll_up_lines(self.last_log_height.max(1));
    }

    pub fn scroll_up_lines(&mut self, n: usize) {
        self.auto_scroll = false;
        self.log_scroll = self.log_scroll.saturating_sub(n);
    }

    pub fn scroll_down_lines(&mut self, n: usize) {
        self.log_scroll = self.log_scroll.saturating_add(n);
    }

    pub fn scroll_down(&mut self) {
        self.auto_scroll = false;
        self.log_scroll = self.log_scroll.saturating_add(self.last_log_height.max(1));
    }

    // ── Agent event handling ──────────────────────────────────────────────────

    pub fn apply_event(&mut self, ev: AgentEvent) {
        match ev {
            AgentEvent::ThinkingToken(token) => {
                self.last_output_at = Some(std::time::Instant::now());
                self.ensure_assistant_message();
                if let Some(last) = self.messages.last_mut() {
                    last.thinking
                        .get_or_insert_with(String::new)
                        .push_str(&token);
                }
                self.bump_log_revision();
            }
            AgentEvent::Usage(usage) => {
                self.latest_usage = Some(usage);
            }
            AgentEvent::TextToken { text, phase } => {
                self.last_output_at = Some(std::time::Instant::now());
                self.ensure_assistant_message();
                if let Some(last) = self.messages.last_mut() {
                    last.content.push_str(&text);
                    if phase != AssistantPhase::Unknown {
                        last.assistant_phase = Some(phase);
                    }
                }
                self.bump_log_revision();
            }
            AgentEvent::ToolIntentStart => {
                self.ensure_assistant_message();
                if let Some(last) = self.messages.last_mut() {
                    last.assistant_phase = Some(AssistantPhase::Provisional);
                }
                self.bump_log_revision();
            }
            AgentEvent::SteeringConsumed { text } => {
                self.last_output_at = Some(std::time::Instant::now());
                self.messages.push(Message::user(text.clone()));
                if let Some(pos) = self.queued_steering.iter().position(|m| m == &text) {
                    self.queued_steering.remove(pos);
                }
                self.bump_log_revision();
            }
            AgentEvent::StatusUpdate(msg) => {
                self.last_output_at = Some(std::time::Instant::now());
                self.provider_status = if msg.is_empty() { None } else { Some(msg) };
                self.bump_log_revision();
            }
            AgentEvent::ToolCallStart { id, name, args } => {
                self.last_output_at = Some(std::time::Instant::now());
                self.messages.push(Message::tool_call(id, name, args));
                self.bump_log_revision();
            }
            AgentEvent::ToolCallEnd { id, name, result } => {
                self.last_output_at = Some(std::time::Instant::now());
                self.messages.push(Message::tool_result(
                    &id,
                    result.content.clone(),
                    result.is_error,
                ));

                // For ask_user, also record the selected/typed answer as an
                // explicit user message after the tool_result so the chat log
                // and persisted history reflect what the user chose.
                //
                // Ordering matters: tool_result must immediately follow
                // tool_call for Anthropic tool_use/tool_result pairing.
                if name == "ask_user" && !result.is_error {
                    self.messages.push(Message::user(result.content));
                }
                self.bump_log_revision();
            }
            AgentEvent::ExternalFileChange {
                paths: _,
                notification,
            } => {
                self.last_output_at = Some(std::time::Instant::now());
                // Mirror the notification into the UI message log so it appears
                // in the conversation display, just as a user message would.
                self.messages.push(Message::user(notification));
                self.bump_log_revision();
            }
            AgentEvent::TurnEnd => {
                self.provider_status = None;
                self.persist_messages();
            }
            AgentEvent::Done => {
                self.streaming = false;
                self.last_output_at = None;
                self.provider_status = None;
                self.agent_task = None;
                self.steering_tx = None;
                self.queued_steering.clear();
                self.bump_log_revision();
                self.persist_messages();
            }
            AgentEvent::Error(e) => {
                self.provider_status = None;
                self.last_output_at = None;
                self.agent_task = None;
                self.steering_tx = None;
                self.queued_steering.clear();
                self.bump_log_revision();

                let is_unauthorized = e.kind == crate::llm::ProviderErrorKind::Unauthorized;

                if is_unauthorized
                    && self.auth_retry_budget > 0
                    && self.trigger_auth_refresh(RetryTarget::AgentTurn)
                {
                    log::debug!(
                        "received 401, refresh triggered: provider={} remaining_budget= {}",
                        self.current_provider,
                        self.auth_retry_budget
                    );
                    self.auth_retry_budget -= 1;
                    self.streaming = false;
                    // Refresh triggered; retry will happen automatically after refresh completes
                } else {
                    self.messages
                        .push(Message::assistant(format!("[Error: {e}]")));
                    self.bump_log_revision();
                    self.streaming = false;
                    self.persist_messages();
                }
            }
        }
    }

    fn ensure_assistant_message(&mut self) {
        match self.messages.last().map(|m| &m.role) {
            Some(Role::Assistant) => {}
            _ => {
                self.messages.push(Message::assistant(""));
                self.bump_log_revision();
            }
        }
    }

    pub fn apply_events(&mut self) {
        while let Ok(ev) = self.event_rx.try_recv() {
            self.apply_event(ev);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::App;
    use crate::{
        agent::{
            AgentLoopConfig,
            types::{AskRequest, AskUserOption, AskUserResponse},
        },
        llm::{AssistantPhase, Message, Role},
        provider::ProviderKind,
        thinking::ThinkingLevel,
    };

    fn make_app() -> App {
        App::new(
            "gpt-4o",
            &ProviderKind::OpenAi,
            ThinkingLevel::Off,
            AgentLoopConfig {
                tools: Default::default(),
                file_tracker: std::sync::Arc::new(std::sync::Mutex::new(
                    crate::agent::FileTracker::new(),
                )),
                tool_output_log: std::sync::Arc::new(std::sync::Mutex::new(
                    crate::agent::ToolOutputLog::new("test-session"),
                )),
                before_tool_call: None,
                after_tool_call: None,
            },
        )
    }

    // ── strip_orphaned_tool_calls ──────────────────────────────────────────────

    #[test]
    fn strip_orphaned_tool_calls_removes_unpaired_tool_call() {
        let mut app = make_app();
        app.messages.push(Message::user("hello"));
        app.messages.push(Message::tool_call(
            "tc-1",
            "bash",
            serde_json::json!({"command": "ls"}),
        ));
        // No ToolResult added — simulates an interrupt mid-execution.

        app.strip_orphaned_tool_calls();

        // The orphaned ToolCall must be gone.
        assert!(
            !app.messages.iter().any(|m| m.role == Role::ToolCall),
            "orphaned ToolCall should have been removed"
        );
    }

    #[test]
    fn strip_orphaned_tool_calls_keeps_paired_tool_call() {
        let mut app = make_app();
        app.messages.push(Message::user("hello"));
        app.messages.push(Message::tool_call(
            "tc-1",
            "bash",
            serde_json::json!({"command": "ls"}),
        ));
        app.messages
            .push(Message::tool_result("tc-1", "file.txt", false));

        app.strip_orphaned_tool_calls();

        // Both the ToolCall and its result must remain.
        assert!(
            app.messages.iter().any(|m| m.role == Role::ToolCall),
            "paired ToolCall should not be removed"
        );
        assert!(
            app.messages.iter().any(|m| m.role == Role::ToolResult),
            "ToolResult should not be removed"
        );
    }

    #[test]
    fn strip_orphaned_tool_calls_removes_trailing_provisional_assistant_msg() {
        let mut app = make_app();
        app.messages.push(Message::user("hello"));
        // Provisional empty assistant message added when tool intent starts.
        let mut asst = Message::assistant("");
        asst.assistant_phase = Some(AssistantPhase::Provisional);
        app.messages.push(asst);
        app.messages.push(Message::tool_call(
            "tc-1",
            "bash",
            serde_json::json!({"command": "ls"}),
        ));

        app.strip_orphaned_tool_calls();

        // ToolCall removed; provisional assistant message also removed.
        assert!(
            !app.messages.iter().any(|m| m.role == Role::ToolCall),
            "orphaned ToolCall should have been removed"
        );
        assert_eq!(
            app.messages.last().map(|m| &m.role),
            Some(&Role::User),
            "trailing empty provisional assistant message should be removed"
        );
    }

    #[test]
    fn strip_orphaned_tool_calls_noop_when_no_tool_calls() {
        let mut app = make_app();
        app.messages.push(Message::user("hello"));
        app.messages.push(Message::assistant("world"));
        let original_len = app.messages.len();

        app.strip_orphaned_tool_calls();

        assert_eq!(
            app.messages.len(),
            original_len,
            "messages should be unchanged"
        );
    }

    #[test]
    fn normalize_ollama_endpoint_adds_default_scheme_and_port() {
        assert_eq!(
            App::normalize_ollama_endpoint("gpu-box"),
            Some("http://gpu-box:11434".to_string())
        );
    }

    #[test]
    fn normalize_ollama_endpoint_adds_default_scheme_only_when_port_present() {
        assert_eq!(
            App::normalize_ollama_endpoint("gpu-box:8080"),
            Some("http://gpu-box:8080".to_string())
        );
    }

    #[test]
    fn normalize_ollama_endpoint_adds_default_port_when_scheme_present() {
        assert_eq!(
            App::normalize_ollama_endpoint("https://gpu-box"),
            Some("https://gpu-box:11434".to_string())
        );
    }

    #[test]
    fn normalize_ollama_endpoint_keeps_existing_scheme_and_port() {
        assert_eq!(
            App::normalize_ollama_endpoint("http://gpu-box:8080"),
            Some("http://gpu-box:8080".to_string())
        );
    }

    #[test]
    fn normalize_ollama_endpoint_rejects_empty_input() {
        assert_eq!(App::normalize_ollama_endpoint("   "), None);
    }

    // ── receive_ask_request ───────────────────────────────────────────────────

    /// When ask_user has no options, receive_ask_request must go directly into
    /// freeform mode (not selection mode) and store the question for display.
    #[test]
    fn receive_ask_request_no_options_enters_freeform_mode() {
        let mut app = make_app();
        let (reply_tx, _reply_rx) = tokio::sync::oneshot::channel::<AskUserResponse>();
        app.receive_ask_request(AskRequest {
            question: "What is your name?".to_string(),
            context: None,
            options: vec![],
            allow_multiple: false,
            allow_freeform: true,
            reply: reply_tx,
        });

        assert!(
            !app.selection_mode,
            "selection mode should NOT be active for no-options"
        );
        assert!(app.ask_user_freeform_mode, "freeform mode should be active");
        assert_eq!(
            app.ask_user_question.as_deref(),
            Some("What is your name?"),
            "question should be stored for display"
        );
        assert!(app.has_pending_ask(), "pending ask should be set");
    }

    /// When ask_user has options and allow_freeform is true, the freeform
    /// sentinel should appear after the option items.
    #[test]
    fn receive_ask_request_with_options_and_freeform_includes_sentinel() {
        let mut app = make_app();
        let (reply_tx, _reply_rx) = tokio::sync::oneshot::channel::<AskUserResponse>();
        app.receive_ask_request(AskRequest {
            question: "Pick one".to_string(),
            context: None,
            options: vec![
                AskUserOption {
                    title: "Alpha".to_string(),
                    description: None,
                },
                AskUserOption {
                    title: "Beta".to_string(),
                    description: None,
                },
            ],
            allow_multiple: false,
            allow_freeform: true,
            reply: reply_tx,
        });

        assert!(app.selection_mode);
        assert_eq!(app.selection_items.len(), 3); // 2 options + freeform sentinel
        assert_eq!(app.selection_items[2].complete_to, "/ask_user_freeform");
    }

    /// When ask_user has options and allow_freeform is false, the freeform
    /// sentinel should NOT appear.
    #[test]
    fn receive_ask_request_with_options_no_freeform_omits_sentinel() {
        let mut app = make_app();
        let (reply_tx, _reply_rx) = tokio::sync::oneshot::channel::<AskUserResponse>();
        app.receive_ask_request(AskRequest {
            question: "Pick one".to_string(),
            context: None,
            options: vec![
                AskUserOption {
                    title: "Alpha".to_string(),
                    description: None,
                },
                AskUserOption {
                    title: "Beta".to_string(),
                    description: None,
                },
            ],
            allow_multiple: false,
            allow_freeform: false,
            reply: reply_tx,
        });

        assert!(app.selection_mode);
        assert_eq!(app.selection_items.len(), 2); // only the 2 options
        assert!(
            app.selection_items
                .iter()
                .all(|i| i.complete_to != "/ask_user_freeform")
        );
    }

    #[test]
    fn slash_submit_text_prefers_highlighted_completion() {
        let mut app = make_app();
        app.textarea.insert_str("/mo");
        app.update_completions();

        let selected = app
            .completions
            .get(app.completion_selected)
            .expect("expected at least one completion");
        assert_eq!(selected.complete_to, "/model ");
        assert_eq!(app.slash_submit_text().as_deref(), Some("/model"));
    }

    #[test]
    fn slash_submit_text_falls_back_to_raw_input_when_no_completion() {
        let mut app = make_app();
        app.textarea.insert_str("/unknown");
        app.update_completions();
        assert!(app.completions.is_empty());

        assert_eq!(app.slash_submit_text().as_deref(), Some("/unknown"));
    }

    #[test]
    fn handle_escape_in_chat_mode_prefers_slash_cancel_over_stream_abort() {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        rt.block_on(async {
            let mut app = make_app();
            app.streaming = true;
            app.agent_task = Some(tokio::spawn(async {
                std::future::pending::<()>().await;
            }));
            app.textarea.insert_str("/model gpt");

            app.handle_escape_in_chat_mode();

            assert!(
                app.streaming,
                "streaming should remain active when ESC cancels slash input"
            );
            assert!(
                app.agent_task.is_some(),
                "agent task should not be aborted when ESC cancels slash input"
            );
            assert!(
                app.textarea
                    .lines()
                    .iter()
                    .all(|line| line.trim().is_empty()),
                "slash input should be cleared"
            );
            assert!(
                !app.messages
                    .iter()
                    .any(|m| m.content == "[agent loop aborted]"),
                "ESC slash cancel should not append an abort notice"
            );

            if let Some(handle) = app.agent_task.take() {
                handle.abort();
            }
        });
    }

    #[test]
    fn handle_escape_in_chat_mode_aborts_stream_when_not_in_slash_mode() {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        rt.block_on(async {
            let mut app = make_app();
            app.streaming = true;
            app.agent_task = Some(tokio::spawn(async {
                std::future::pending::<()>().await;
            }));
            app.textarea.insert_str("hello");

            app.handle_escape_in_chat_mode();

            assert!(
                !app.streaming,
                "streaming should stop when ESC is used outside slash mode"
            );
            assert!(
                app.agent_task.is_none(),
                "agent task should be removed when stream is aborted"
            );
            assert!(
                app.messages
                    .iter()
                    .any(|m| m.content == "[agent loop aborted]"),
                "abort should append user-visible abort notice"
            );
        });
    }
}
