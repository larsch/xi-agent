use ratatui::text::Line;
use ratatui_textarea::{CursorMove, TextArea};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::{sync::mpsc::error::TryRecvError, task::JoinHandle};

use crate::{
    agent::{
        AgentLoopConfig, ToolOutputLog, run_agent_loop,
        types::{AgentEvent, AskRequest, AskUserOption, AskUserResponse},
    },
    app_event::{AppEvent, AppEventTx},
    auth::{self, AuthFlow, LoginEvent},
    commands::{self, CompletionItem},
    live_turn::{LiveToolEntry, LiveToolResult, LiveTurnState, compose_display},
    llm::{
        AssistantPhase, DisplayRange, LlmProvider, Message, ProviderErrorKind, Role, UsageStats,
    },
    provider_instance::{ApiType, AuthMode, BackendPreset, EndpointBehavior, ProviderInstance},
    session::SessionStore,
    session_state::SessionState,
    shell::{self, ShellKind},
    skills::SkillMeta,
    thinking::ThinkingLevel,
};

use crate::export;
use crate::session_event::SessionEvent;

// ── Streaming status ──────────────────────────────────────────────────────────

pub const DEFAULT_OLLAMA_ENDPOINT: &str = "http://localhost:11434";

/// Describes what the agent/provider is currently doing while a turn is active.
#[derive(Debug, Clone)]
pub enum StreamingStatus {
    /// Waiting for the first token — throbber should animate.
    Waiting,
    /// Provider-supplied transient message (e.g. rate-limit countdown).
    Message(String),
    /// A completed-turn status message that remains visible until the next turn starts.
    CompletedMessage(String),
}

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
    /// The user started the add-provider flow.
    AddProvider,
    /// The user cancelled a pending provider removal confirmation.
    CancelProviderRemoval,
    /// The user confirmed removing a custom provider instance.
    RemoveProvider(String),
    /// A provider backend type was chosen during add-provider setup.
    ProviderBackendPreset(BackendPreset),
    /// A provider API type was chosen during add-provider setup.
    ProviderApiType(ApiType),
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
    question: String,
    options: Vec<AskUserOption>,
    allow_freeform: bool,
}

struct AskUserState {
    pending: Option<PendingAsk>,
    reply: Option<tokio::sync::oneshot::Sender<AskUserResponse>>,
    freeform_mode: bool,
    question: Option<String>,
}

impl AskUserState {
    fn new() -> Self {
        Self {
            pending: None,
            reply: None,
            freeform_mode: false,
            question: None,
        }
    }
}

struct AgentRuntime {
    /// Receives background app events forwarded from tasks targeting the UI.
    app_event_rx: tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    app_event_tx: AppEventTx,
    steering_tx: Option<tokio::sync::mpsc::UnboundedSender<String>>,
    /// User steering messages queued while a loop is running; rendered pinned
    /// at the bottom of the log with a 🕹️ icon until consumed.
    queued_steering: Vec<String>,
    /// JoinHandle for the currently running agent loop task (if any).
    agent_task: Option<JoinHandle<()>>,
    /// Cancellation sender for the active agent loop task.
    /// Sending `true` signals the loop to exit at its next cooperative checkpoint.
    cancel_tx: Option<tokio::sync::watch::Sender<bool>>,
}

impl AgentRuntime {
    fn new() -> Self {
        let (app_event_tx, app_event_rx) = tokio::sync::mpsc::unbounded_channel();
        Self {
            app_event_rx,
            app_event_tx,
            steering_tx: None,
            queued_steering: Vec::new(),
            agent_task: None,
            cancel_tx: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupInputKind {
    Name,
    BaseUrl,
    ApiKey,
}

impl SetupInputKind {
    pub fn prompt_label(self, instance: Option<&ProviderInstance>) -> String {
        match self {
            Self::Name => "provider instance name: ".to_string(),
            Self::BaseUrl => match instance {
                Some(p) => match p.backend_preset.def().endpoint_behavior {
                    EndpointBehavior::UserSupplied => match p.backend_preset {
                        BackendPreset::Ollama => "ollama URL: ".to_string(),
                        BackendPreset::OpenWebUi => "open-webui URL: ".to_string(),
                        BackendPreset::OpenAiCompatible => "URL: ".to_string(),
                        _ => "URL: ".to_string(),
                    },
                    EndpointBehavior::Overrideable => "URL override: ".to_string(),
                    _ => "URL: ".to_string(),
                },
                None => "URL: ".to_string(),
            },
            Self::ApiKey => match instance {
                Some(p) => match p.backend_preset.def().auth_mode {
                    AuthMode::ApiKey => match p.backend_preset {
                        BackendPreset::OpenRouter => "OpenRouter API key: ".to_string(),
                        BackendPreset::OpenWebUi => "open-webui token: ".to_string(),
                        _ if p.base_url.is_some() => {
                            "API key (leave empty to keep current): ".to_string()
                        }
                        _ => "API key: ".to_string(),
                    },
                    _ => "token: ".to_string(),
                },
                None => "API key: ".to_string(),
            },
        }
    }

    pub fn prompt_hint(self, instance: Option<&ProviderInstance>) -> String {
        match self {
            Self::Name => "work-webui   Enter confirm   Esc cancel".to_string(),
            Self::BaseUrl => match instance.map(|p| p.backend_preset.clone()) {
                Some(BackendPreset::Ollama) => {
                    "http://host:11434   Enter confirm   Esc cancel".to_string()
                }
                Some(BackendPreset::OpenWebUi) => {
                    "https://my-webui.example.com   Enter confirm   Esc cancel".to_string()
                }
                Some(BackendPreset::OpenAiCompatible) => {
                    "https://my-endpoint.example.com/v1   Enter confirm   Esc cancel".to_string()
                }
                Some(BackendPreset::OpenRouter) => {
                    "https://openrouter.ai/api/v1   Enter confirm   Esc cancel".to_string()
                }
                _ => "https://example.com   Enter confirm   Esc cancel".to_string(),
            },
            Self::ApiKey => match instance {
                Some(p) if p.api_key.is_some() => "Enter keep current   Esc cancel".to_string(),
                Some(p) => match p.backend_preset {
                    BackendPreset::OpenRouter => "sk-or-…   Enter confirm   Esc cancel".to_string(),
                    BackendPreset::OpenWebUi => "sk-…   Enter confirm   Esc cancel".to_string(),
                    BackendPreset::OpenAiCompatible | BackendPreset::OpenAi => {
                        "sk-…   Enter confirm   Esc cancel".to_string()
                    }
                    _ => "token   Enter confirm   Esc cancel".to_string(),
                },
                None => "token   Enter confirm   Esc cancel".to_string(),
            },
        }
    }
}

#[derive(Debug, Clone)]
pub struct PendingProviderSetup {
    pub original_id: String,
    pub id: String,
    pub backend_preset: Option<BackendPreset>,
    pub api_type: Option<ApiType>,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub editing_existing: bool,
}

#[derive(Debug, Clone)]
pub struct PendingProviderRemoval {
    pub id: String,
}

impl PendingProviderSetup {
    fn new(id: String) -> Self {
        Self {
            original_id: id.clone(),
            id,
            backend_preset: None,
            api_type: None,
            base_url: None,
            api_key: None,
            editing_existing: false,
        }
    }

    pub(crate) fn from_instance(instance: &ProviderInstance) -> Self {
        Self {
            original_id: instance.id.clone(),
            id: instance.id.clone(),
            backend_preset: Some(instance.backend_preset.clone()),
            api_type: Some(instance.api_type.clone()),
            base_url: instance.base_url.clone(),
            api_key: instance.api_key.clone(),
            editing_existing: true,
        }
    }
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
pub(crate) enum SelectionKind {
    Model,
    Thinking,
    Provider,
    ProviderBackendPreset,
    ProviderApiType,
    LoginProvider,
    ResumeSession,
    AskUser,
    LoginAction,
    ConfirmProviderRemoval,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputMode {
    Chat,
    Shell,
}

// ── Selection state ───────────────────────────────────────────────────────────

/// All state for the selection menu panel.
pub struct SelectionState {
    /// True when the selection picker is active.
    pub active: bool,
    /// Header title shown in the selection menu.
    pub title: &'static str,
    /// Items currently visible (after filtering).
    pub items: Vec<CompletionItem>,
    /// Unfiltered source items for search filtering.
    all_items: Vec<CompletionItem>,
    /// Current free-text search query.
    pub query: String,
    /// Kind of selection currently being displayed.
    kind: Option<SelectionKind>,
    /// Index of the currently highlighted row.
    pub selected: usize,
    /// First visible item index (scroll offset).
    pub scroll: usize,
}

impl SelectionState {
    fn new() -> Self {
        Self {
            active: false,
            title: "",
            items: Vec::new(),
            all_items: Vec::new(),
            query: String::new(),
            kind: None,
            selected: 0,
            scroll: 0,
        }
    }
}

// ── Login state ───────────────────────────────────────────────────────────────

/// All state for the login/authentication panel.
pub struct LoginState {
    pub active: bool,
    pub provider: Option<String>,
    pub info: String,
    pub url: Option<String>,
    pub code: Option<String>,
    /// Which OAuth flow is in use; drives the UI's instruction text and
    /// available keyboard actions.
    pub auth_flow: Option<AuthFlow>,
    pub needs_rebuild: bool,
    pub refresh_in_progress: bool,
    pub retry_after_refresh: bool,
    /// Set when a `list_models` call fails with a 401 so the fetch is
    /// re-issued automatically once the token refresh completes.
    pub retry_model_fetch_after_refresh: bool,
    auth_retry_budget: u8,
    cancel: Option<Arc<AtomicBool>>,
    /// Persistent clipboard instance used during the login flow.
    ///
    /// On Linux the clipboard is owned by the process: dropping the
    /// `arboard::Clipboard` instance releases ownership and the text
    /// disappears from other applications.  We therefore keep it alive for
    /// the entire duration of the login panel and only drop it once login
    /// finishes.
    clipboard: Option<arboard::Clipboard>,
}

impl LoginState {
    fn new() -> Self {
        Self {
            active: false,
            provider: None,
            info: String::new(),
            url: None,
            code: None,
            auth_flow: None,
            needs_rebuild: false,
            refresh_in_progress: false,
            retry_after_refresh: false,
            retry_model_fetch_after_refresh: false,
            auth_retry_budget: 0,
            cancel: None,
            clipboard: None,
        }
    }
}

// ── App state ─────────────────────────────────────────────────────────────────

pub struct App {
    pub textarea: TextArea<'static>,
    pub shell_textarea: TextArea<'static>,
    pub input_mode: InputMode,
    pub selected_shell: ShellKind,
    pub available_shells: Vec<ShellKind>,
    /// Monotonic revision bump for any visible log-content change.
    /// Used to invalidate cached wrapped log lines.
    pub log_revision: u64,
    /// Cached pre-wrapped log lines for the most recent `(log_revision, width)`.
    pub cached_log_lines: Option<(u64, usize, Vec<Line<'static>>)>,
    pub log_scroll: usize,
    /// When true, the view always follows the bottom (auto-scrolls).
    pub auto_scroll: bool,
    /// Height of the log pane from the last draw — used as page-size scrolling.
    pub last_log_height: usize,
    /// Current streaming state; `None` when no turn is active.
    pub streaming_status: Option<StreamingStatus>,
    /// Throbber animation frame index, advanced on every UI tick while streaming.
    pub throbber_tick: u8,
    /// Instant of the last visible agent output (text/thinking tokens, tool
    /// calls, tool results, etc.); used to suppress the throbber while output
    /// is actively arriving and re-show it after a short idle time.
    pub last_output_at: Option<std::time::Instant>,
    /// Optional system prompt prepended to every request.
    pub system_prompt: Option<String>,
    /// Currently active model name (mirrors the provider; updated on `/model`).
    pub current_model: String,
    /// Currently active provider name (e.g. `"copilot"`).
    pub current_provider: String,
    /// Currently active thinking / reasoning level.
    pub current_thinking: ThinkingLevel,
    /// Whether the current provider+model combination supports thinking.
    /// Updated by main.rs whenever provider or model changes.
    pub thinking_supported: bool,
    /// Snapshot of configured provider instances for completions and selection.
    /// Updated by main.rs whenever the provider list changes.
    pub provider_instances: Vec<crate::provider_instance::ProviderInstance>,
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
    pub selection: SelectionState,

    // ── Info bar ──────────────────────────────────────────────────────────────
    /// When true, the info bar (provider / model / context window) is shown
    /// below the input panel.  Toggled by Ctrl+I.
    pub show_info: bool,
    /// Best-effort token usage reported for the latest completed turn.
    pub latest_usage: Option<UsageStats>,

    // ── Login panel ───────────────────────────────────────────────────────────
    pub login: LoginState,

    // ── Session persistence ───────────────────────────────────────────────────
    session_store: Option<SessionStore>,
    current_session_id: Option<String>,
    current_cwd: String,
    resume_available_for_cwd: bool,

    // ── Session state ─────────────────────────────────────────────────────────
    /// Committed session state: durable event log plus derived read models.
    /// `None` until the first session is created or resumed.
    pub(crate) session_state: Option<SessionState>,
    /// Transient in-flight state for the current (or most recently flushed)
    /// agent turn. Streaming assistant text, tool call/result pairs, and
    /// UI-only notices all live here until committed or cleared.
    pub(crate) live_turn: LiveTurnState,
    /// Buffer of events accumulated during the current in-flight turn.
    /// Flushed to session state as a batch on `TurnEnd`, `Done`, or `Error`.
    pending_turn_events: Vec<crate::session_event::SessionEvent>,

    /// Optional manual compaction instructions for the next launched compaction-only task.
    pending_manual_compaction_instructions: Option<String>,

    // ── Ask-user interaction state ──────────────────────────────────────────
    ask_user: AskUserState,

    // ── Add-provider setup state ─────────────────────────────────────────────
    /// When set, the textarea is being used for a structured provider-setup prompt.
    pub setup_input_mode: Option<SetupInputKind>,
    /// Pending provider instance being configured through the add-provider flow.
    pub pending_provider_setup: Option<PendingProviderSetup>,
    /// Pending custom provider instance being confirmed for removal.
    pub pending_provider_removal: Option<PendingProviderRemoval>,

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

    // ── Runtime/task state ───────────────────────────────────────────────────
    runtime: AgentRuntime,
}

// Convenience alias used throughout this module.
type DynProvider = Arc<dyn LlmProvider + Send + Sync + 'static>;

pub(crate) fn format_provider_error_for_display(
    provider_label: &str,
    err: &crate::llm::ProviderError,
) -> String {
    let subject = if provider_label.trim().is_empty() {
        "The provider"
    } else {
        provider_label
    };

    let summary = match (err.kind, err.status_code) {
        (ProviderErrorKind::Unauthorized, Some(code)) => {
            format!("{subject} rejected the request because authentication expired ({code}).")
        }
        (ProviderErrorKind::Unauthorized, None) => {
            format!("{subject} rejected the request because authentication expired.")
        }
        (ProviderErrorKind::Forbidden, Some(code)) => {
            format!("{subject} rejected the request because access was denied ({code}).")
        }
        (ProviderErrorKind::Forbidden, None) => {
            format!("{subject} rejected the request because access was denied.")
        }
        (ProviderErrorKind::RateLimited, Some(code)) => {
            format!("{subject} is rate limiting requests ({code}).")
        }
        (ProviderErrorKind::RateLimited, None) => {
            format!("{subject} is rate limiting requests.")
        }
        (ProviderErrorKind::ServerError, Some(524)) => {
            format!("{subject} timed out on the backend (524).")
        }
        (ProviderErrorKind::ServerError, Some(code)) => {
            format!("{subject} reported a backend error ({code}).")
        }
        (ProviderErrorKind::ServerError, None) => {
            format!("{subject} reported a backend error.")
        }
        (ProviderErrorKind::Network, _) => {
            format!("Could not reach {subject}.")
        }
        (ProviderErrorKind::Other, Some(code)) => {
            format!("{subject} could not process the request ({code}).")
        }
        (ProviderErrorKind::Other, None) => {
            format!("{subject} could not process the request.")
        }
    };

    let message = err.message.trim();
    if message.is_empty() {
        summary
    } else {
        format!("{summary}\nProvider message: {message}")
    }
}

fn active_provider_display_name(
    current_provider: &str,
    provider_instances: &[ProviderInstance],
) -> String {
    provider_instances
        .iter()
        .find(|instance| instance.id == current_provider)
        .map(|instance| instance.backend_preset.label().to_string())
        .unwrap_or_else(|| current_provider.to_string())
}

impl App {
    fn bump_log_revision(&mut self) {
        self.log_revision = self.log_revision.wrapping_add(1);
        self.cached_log_lines = None;
    }

    pub fn mark_log_dirty(&mut self) {
        self.bump_log_revision();
    }

    pub fn new(
        initial_model: impl Into<String>,
        initial_provider: &str,
        initial_thinking: ThinkingLevel,
        agent_config: AgentLoopConfig,
    ) -> Self {
        let available_shells = shell::discover_available_shells();
        let selected_shell = available_shells.first().copied().unwrap_or(ShellKind::Bash);
        Self {
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
            streaming_status: None,
            throbber_tick: 0,
            last_output_at: None,
            system_prompt: None,
            current_model: initial_model.into(),
            current_provider: initial_provider.to_string(),
            current_thinking: initial_thinking,
            thinking_supported: false, // updated by main.rs after construction
            provider_instances: Vec::new(), // updated by main.rs after construction
            agent_config,
            loaded_skills: Vec::new(),
            completions: Vec::new(),
            completion_selected: 0,
            available_models: None,
            models_loading: false,
            model_fetch_error: None,
            selection: SelectionState::new(),
            show_info: false,
            latest_usage: None,
            login: LoginState::new(),
            session_store: None,
            current_session_id: None,
            current_cwd: String::new(),
            resume_available_for_cwd: false,
            session_state: None,
            live_turn: LiveTurnState::new(),
            pending_turn_events: Vec::new(),
            pending_manual_compaction_instructions: None,
            ask_user: AskUserState::new(),
            setup_input_mode: None,
            pending_provider_setup: None,
            pending_provider_removal: None,
            ollama_endpoint_input_mode: false,
            open_webui_url_input_mode: false,
            open_webui_token_input_mode: false,
            open_webui_pending_url: None,
            runtime: AgentRuntime::new(),
        }
    }

    /// Returns true when an agent turn is active (streaming or waiting for first token).
    pub fn streaming(&self) -> bool {
        matches!(
            self.streaming_status,
            Some(StreamingStatus::Waiting | StreamingStatus::Message(_))
        )
    }

    /// Advance the throbber animation frame.  Called on every UI tick.
    pub fn tick(&mut self) {
        if self.streaming() {
            self.throbber_tick = self.throbber_tick.wrapping_add(1);
        }
    }

    /// Record a model/provider change in the event log.
    ///
    /// Call this whenever `current_model` or `current_provider` is updated so
    /// that the change is preserved in the session history.
    pub fn record_model_changed(&mut self) {
        self.append_event_immediate(SessionEvent::ModelChanged {
            model: self.current_model.clone(),
            provider: self.current_provider.clone(),
            timestamp: Self::now_ts(),
        });
    }

    /// Record a thinking-level change in the event log.
    ///
    /// Call this whenever `current_thinking` is updated.
    pub fn record_thinking_level_changed(&mut self) {
        self.append_event_immediate(SessionEvent::ThinkingLevelChanged {
            level: self.current_thinking,
            timestamp: Self::now_ts(),
        });
    }

    /// Returns true when the throbber should be visible.
    ///
    /// Three-state model:
    /// - Machine waiting for **user** (`has_pending_ask` / `ask_user_freeform_mode`):
    ///   throbber hidden — the ball is in the user's court.
    /// - Machine producing **output** (visible content added very recently):
    ///   throbber hidden — something is actively appearing on screen.
    /// - Machine working **silently** (streaming, no visible output for a short interval):
    ///   throbber visible — signals that work is in progress.
    pub fn throbber_visible(&self) -> bool {
        if !self.streaming() {
            return false;
        }
        // The agent loop is paused waiting for user input — don't spin.
        if self.has_pending_ask() || self.ask_user_freeform_mode() {
            return false;
        }
        match self.last_output_at {
            None => true,
            Some(t) => t.elapsed() >= std::time::Duration::from_millis(240),
        }
    }

    /// Returns true when provider/system status text should be visible.
    pub fn provider_status_visible(&self) -> bool {
        if self.login.active {
            return false;
        }
        matches!(
            self.streaming_status,
            Some(StreamingStatus::Message(_) | StreamingStatus::CompletedMessage(_))
        )
    }

    pub fn ask_user_freeform_mode(&self) -> bool {
        self.ask_user.freeform_mode
    }

    pub fn ask_user_question(&self) -> Option<&str> {
        self.ask_user.question.as_deref()
    }

    pub fn queued_steering(&self) -> &[String] {
        &self.runtime.queued_steering
    }

    /// Toggle the info bar visibility.
    pub fn toggle_info(&mut self) {
        self.show_info = !self.show_info;
    }

    pub async fn recv_app_event(&mut self) -> Option<AppEvent> {
        self.runtime.app_event_rx.recv().await
    }

    pub fn app_event_tx(&self) -> AppEventTx {
        self.runtime.app_event_tx.clone()
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
                self.live_turn.notices.push(Message::assistant(format!(
                    "[session persistence unavailable: {e}]"
                )));
                self.bump_log_revision();
            }
        }
    }

    pub fn current_cwd(&self) -> &str {
        &self.current_cwd
    }

    /// Return all messages to display in the chat log: committed session
    /// messages followed by the live turn overlay (streaming assistant,
    /// in-flight tools, and UI-only notices).
    pub fn display_messages_combined(&self) -> Vec<Message> {
        let committed = self
            .session_state
            .as_ref()
            .map(|s| s.display_messages())
            .unwrap_or(&[]);
        compose_display(committed, &self.live_turn, self.streaming())
    }

    /// Push a transient UI-only notice (not backed by a `SessionEvent`).
    pub fn push_notice(&mut self, msg: Message) {
        self.live_turn.notices.push(msg);
    }

    /// Whether there are no committed display messages and no live overlay.
    pub fn display_is_empty(&self) -> bool {
        self.session_state
            .as_ref()
            .map(|s| s.display_is_empty())
            .unwrap_or(true)
            && self.live_turn.notices.is_empty()
            && !self.live_turn.has_assistant_content()
            && !self.live_turn.has_tool_entries()
    }

    /// Number of displayed messages (committed + live overlay).
    pub fn display_len(&self) -> usize {
        let committed = self
            .session_state
            .as_ref()
            .map(|s| s.display_len())
            .unwrap_or(0);
        // Use streaming=false for counting purposes (we don't want the
        // waiting-cursor empty slot to affect the count used for shell IDs).
        committed + self.live_turn.render_overlay(false).len()
    }

    pub fn should_show_resume_hint(&self) -> bool {
        self.resume_available_for_cwd
            && self.display_is_empty()
            && !self.selection.active
            && !self.login.active
            && !self.streaming()
    }

    pub fn resume_latest_for_current_cwd(&mut self) {
        let Some(store) = self.session_store.as_ref() else {
            return;
        };
        let Some(meta) = store.latest_for_cwd(&self.current_cwd) else {
            self.live_turn.notices.push(Message::assistant(
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
        // Load the event log; fall back to legacy messages path for old sessions.
        match store.load_events(session_id) {
            Ok(log) => {
                self.session_state = Some(SessionState::from_event_log(log));
                self.live_turn.clear_all();
                self.current_session_id = Some(session_id.to_string());
                self.auto_scroll = true;
                self.log_scroll = 0;
                self.bump_log_revision();
            }
            Err(e) => {
                self.live_turn.notices.push(Message::assistant(format!(
                    "[failed to resume session: {e}]"
                )));
                self.bump_log_revision();
            }
        }
        self.refresh_resume_availability();
    }

    pub fn enter_resume_selection_mode(&mut self) {
        self.reset_textarea();
        self.selection.active = true;
        self.selection.kind = Some(SelectionKind::ResumeSession);
        self.selection.title = "  Resume session  ";
        self.selection.query.clear();

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
                let prompt_hint = meta.first_prompt.as_deref().unwrap_or(&meta.id);

                CompletionItem {
                    label: format!("[{scope}] {when}  —  {prompt_hint}"),
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
        if !self.provider_supports_token_refresh() || self.login.refresh_in_progress {
            return false;
        }

        log::debug!(
            "triggering token refresh: provider={} target={:?}",
            self.current_provider,
            target
        );

        self.login.refresh_in_progress = true;

        match target {
            RetryTarget::AgentTurn => {
                self.login.retry_after_refresh = true;
            }
            RetryTarget::ModelFetch => {
                self.login.retry_model_fetch_after_refresh = true;
            }
        }

        let provider = self.current_provider.clone();
        let tx = self.app_event_tx();
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
        if self.streaming()
            || self.login.refresh_in_progress
            || !self.provider_supports_token_refresh()
        {
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
        if command.is_empty() || self.streaming() || self.login.active {
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

        let call_id = format!("local-shell-{}", self.display_len());
        let mut call_msg = Message::tool_call(
            call_id.clone(),
            "local_shell",
            serde_json::json!({
                "prefix": cmd_prefix,
                "command": command,
            }),
        );
        call_msg.include_in_llm = false;
        self.live_turn.notices.push(call_msg);

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
        self.live_turn.notices.push(out_msg);
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
    /// 3) cancel provider-name input
    /// 4) cancel Ollama endpoint input
    /// 5) cancel Open WebUI setup input
    /// 6) cancel login flow
    /// 7) abort streaming agent loop
    pub fn handle_escape_in_chat_mode(&mut self) {
        if self.has_pending_ask() {
            self.cancel_pending_ask();
        } else if self.in_slash_mode() {
            self.reset_textarea();
        } else if self.selection.kind == Some(SelectionKind::ConfirmProviderRemoval) {
            self.exit_selection_mode();
            self.clear_pending_provider_removal();
        } else if self.setup_input_mode.is_some() {
            self.cancel_setup_input();
        } else if self.ollama_endpoint_input_mode {
            self.cancel_ollama_endpoint_input();
        } else if self.open_webui_url_input_mode {
            self.cancel_open_webui_url_input();
        } else if self.open_webui_token_input_mode {
            self.cancel_open_webui_token_input();
        } else if self.login.active {
            self.cancel_login();
        } else if self.streaming() {
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
        let thinking_enabled = self.thinking_supported;
        let new = commands::completions_for(
            &input,
            available,
            loading,
            fetch_error,
            &self.loaded_skills,
            thinking_enabled,
            &self.provider_instances,
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
        let tx = self.app_event_tx();
        tokio::spawn(async move {
            let result = future.await;
            let _ = tx.send(AppEvent::ModelsReady(result));
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
                    let provider_label = active_provider_display_name(
                        &self.current_provider,
                        &self.provider_instances,
                    );
                    self.model_fetch_error =
                        Some(format_provider_error_for_display(&provider_label, &e));
                }
            }
        }
        self.update_completions();

        if self.selection.active && self.selection.kind == Some(SelectionKind::Model) {
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
                    if self.selection.query.trim().is_empty() {
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
        self.selection.all_items = items;
        self.selection.selected = 0;
        self.selection.scroll = 0;
        self.apply_selection_filter();
    }

    fn select_current_default(&mut self) {
        let target = match self.selection.kind {
            Some(SelectionKind::Model) => Some(format!("/model {}", self.current_model)),
            Some(SelectionKind::Thinking) => {
                Some(format!("/thinking {}", self.current_thinking.as_str()))
            }
            Some(SelectionKind::Provider) => Some(format!("/provider {}", self.current_provider)),
            Some(SelectionKind::LoginProvider)
            | Some(SelectionKind::ResumeSession)
            | Some(SelectionKind::AskUser)
            | Some(SelectionKind::LoginAction)
            | Some(SelectionKind::ConfirmProviderRemoval)
            | Some(SelectionKind::ProviderBackendPreset)
            | Some(SelectionKind::ProviderApiType)
            | None => None,
        };

        if let Some(target) = target
            && let Some(idx) = self
                .selection
                .items
                .iter()
                .position(|item| item.complete_to == target)
        {
            self.selection.selected = idx;
            self.ensure_selection_visible();
        }
    }

    fn apply_selection_filter(&mut self) {
        let query = self.selection.query.trim();
        if query.is_empty() {
            self.selection.items = self.selection.all_items.clone();
        } else {
            let needle = query.to_lowercase();
            self.selection.items = self
                .selection
                .all_items
                .iter()
                .filter(|item| {
                    item.label.to_lowercase().contains(&needle)
                        || item.detail.to_lowercase().contains(&needle)
                })
                .cloned()
                .collect();
        }

        if self.selection.items.is_empty() {
            self.selection.selected = 0;
            self.selection.scroll = 0;
            return;
        }

        if self.selection.selected >= self.selection.items.len() {
            self.selection.selected = 0;
        }
        self.ensure_selection_visible();
    }

    fn ensure_selection_visible(&mut self) {
        if self.selection.items.is_empty() {
            self.selection.scroll = 0;
            return;
        }
        if self.selection.selected < self.selection.scroll {
            self.selection.scroll = self.selection.selected;
        }
        if self.selection.selected >= self.selection.scroll + MAX_SELECTION_VISIBLE {
            self.selection.scroll = self.selection.selected + 1 - MAX_SELECTION_VISIBLE;
        }
    }

    /// Open the model selection menu, pre-populating from cache or showing a
    /// loading indicator when the list hasn't been fetched yet.
    pub fn enter_model_selection_mode(&mut self) {
        self.reset_textarea();
        self.selection.active = true;
        self.selection.kind = Some(SelectionKind::Model);
        self.selection.title = "  Select model  ";
        self.selection.query.clear();
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

    /// Returns true when the active selection is the provider picker.
    pub fn in_provider_selection_mode(&self) -> bool {
        self.selection.kind == Some(SelectionKind::Provider)
    }

    /// Returns true when the active selection is the provider-removal confirmation.
    pub fn in_provider_removal_confirmation_mode(&self) -> bool {
        self.selection.kind == Some(SelectionKind::ConfirmProviderRemoval)
    }

    /// Returns the currently highlighted provider id in the provider picker.
    pub fn selected_provider_id(&self) -> Option<&str> {
        if self.selection.kind != Some(SelectionKind::Provider) {
            return None;
        }
        self.selection
            .items
            .get(self.selection.selected)?
            .complete_to
            .strip_prefix("/provider ")
    }

    /// Open the thinking-level selection menu.
    pub fn enter_thinking_selection_mode(&mut self) {
        self.reset_textarea();
        self.selection.active = true;
        self.selection.kind = Some(SelectionKind::Thinking);
        self.selection.title = "  Select thinking  ";
        self.selection.query.clear();
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

    /// Open the provider selection menu with the configured instances plus an
    /// explicit action to add a new instance.
    pub fn enter_provider_selection_mode(&mut self, instances: &[ProviderInstance]) {
        self.reset_textarea();
        self.selection.active = true;
        self.selection.kind = Some(SelectionKind::Provider);
        self.selection.title = "  Select provider  ";
        self.selection.query.clear();

        let mut items = Vec::with_capacity(instances.len() + 1);
        items.push(CompletionItem {
            label: "Add provider…".to_string(),
            detail: "Create a new named provider instance".to_string(),
            complete_to: "/provider_add".to_string(),
            loading: false,
            error: false,
            match_range: None,
        });
        items.extend(
            instances
                .iter()
                .map(|p| CompletionItem::from_provider(&p.id, &p.label())),
        );
        self.set_selection_items(items);
        self.select_current_default();
    }

    /// Start editing an existing custom provider instance.
    pub fn enter_provider_edit_mode(&mut self, instance: &ProviderInstance) {
        self.exit_selection_mode();
        self.pending_provider_removal = None;
        self.pending_provider_setup = Some(PendingProviderSetup::from_instance(instance));
        match instance.backend_preset {
            BackendPreset::Ollama => {
                self.enter_ollama_endpoint_freeform_mode();
                self.textarea = Self::make_textarea();
                if let Some(base_url) = instance.base_url.as_deref() {
                    self.textarea.insert_str(base_url);
                }
            }
            BackendPreset::OpenWebUi | BackendPreset::OpenAiCompatible => {
                self.enter_provider_base_url_input_mode();
                self.textarea = Self::make_textarea();
                if let Some(base_url) = instance.base_url.as_deref() {
                    self.textarea.insert_str(base_url);
                }
                self.open_webui_url_input_mode = false;
                self.ollama_endpoint_input_mode = false;
            }
            _ => {
                self.enter_provider_base_url_input_mode();
                if let Some(base_url) = instance.base_url.as_deref() {
                    self.textarea.insert_str(base_url);
                }
            }
        }
    }

    pub fn pending_provider_setup_is_edit(&self) -> bool {
        self.pending_provider_setup
            .as_ref()
            .map(|setup| setup.editing_existing)
            .unwrap_or(false)
    }

    pub fn pending_provider_original_id(&self) -> Option<&str> {
        self.pending_provider_setup
            .as_ref()
            .and_then(|setup| setup.editing_existing.then_some(setup.original_id.as_str()))
    }

    /// Begin setup for a new custom provider instance.
    pub fn begin_new_provider_setup(&mut self) {
        self.exit_selection_mode();
        self.reset_textarea();
        self.setup_input_mode = None;
        self.pending_provider_setup = Some(PendingProviderSetup::new(String::new()));
        self.pending_provider_removal = None;
    }

    /// Enter freeform input mode for the new provider instance name.
    pub fn enter_provider_name_input_mode(&mut self) {
        self.exit_selection_mode();
        self.reset_textarea();
        self.setup_input_mode = Some(SetupInputKind::Name);
        self.pending_provider_removal = None;
        if let Some(existing_id) = self
            .pending_provider_setup
            .as_ref()
            .filter(|setup| setup.editing_existing)
            .map(|setup| setup.id.clone())
        {
            self.textarea.insert_str(&existing_id);
        } else if let Some(suggested) = self.suggested_pending_provider_id() {
            self.textarea.insert_str(&suggested);
        }
    }

    /// Enter freeform input mode for a provider endpoint / base URL.
    pub fn enter_provider_base_url_input_mode(&mut self) {
        self.exit_selection_mode();
        self.reset_textarea();
        self.setup_input_mode = Some(SetupInputKind::BaseUrl);
    }

    /// Enter freeform input mode for a provider API key / token.
    pub fn enter_provider_api_key_input_mode(&mut self) {
        self.exit_selection_mode();
        self.reset_textarea();
        self.setup_input_mode = Some(SetupInputKind::ApiKey);
    }

    /// Cancel the active provider setup input and clear pending state.
    pub fn cancel_setup_input(&mut self) {
        self.setup_input_mode = None;
        self.pending_provider_setup = None;
        self.pending_provider_removal = None;
        self.reset_textarea();
    }

    fn normalize_provider_id(raw: &str) -> Option<String> {
        let mut out = String::new();
        let mut prev_sep = false;
        for ch in raw.trim().chars() {
            let mapped = match ch {
                'a'..='z' | '0'..='9' | '.' => Some(ch),
                'A'..='Z' => Some(ch.to_ascii_lowercase()),
                _ => None,
            };
            if let Some(c) = mapped {
                out.push(c);
                prev_sep = false;
            } else if !out.is_empty() && !prev_sep {
                out.push('-');
                prev_sep = true;
            }
        }
        while out.ends_with(['-', '.']) {
            out.pop();
        }
        if out.is_empty() { None } else { Some(out) }
    }

    fn provider_type_suffix(backend_preset: &BackendPreset) -> &'static str {
        match backend_preset {
            BackendPreset::Ollama => "ollama",
            BackendPreset::OpenWebUi => "open-webui",
            BackendPreset::OpenAiCompatible => "openai-compatible",
            BackendPreset::Copilot => "copilot",
            BackendPreset::OpenAi => "openai",
            BackendPreset::OpenRouter => "openrouter",
            BackendPreset::Codex => "codex",
            BackendPreset::Gemini => "gemini",
            BackendPreset::OllamaCom => "ollama-com",
            BackendPreset::Test => "test",
        }
    }

    fn suggested_pending_provider_id(&self) -> Option<String> {
        let setup = self.pending_provider_setup.as_ref()?;
        if setup.editing_existing {
            return Some(setup.id.clone());
        }
        let backend_preset = setup.backend_preset.as_ref()?;
        let host = setup
            .base_url
            .as_deref()
            .and_then(|base| reqwest::Url::parse(base).ok())
            .and_then(|url| url.host_str().map(ToOwned::to_owned))
            .unwrap_or_else(|| backend_preset.id().to_string());
        let raw = match backend_preset {
            BackendPreset::Ollama => format!("ollama-{host}"),
            _ => format!("{}-{}", host, Self::provider_type_suffix(backend_preset)),
        };
        Self::normalize_provider_id(&raw)
    }

    /// Read the typed provider name, normalize it into a stable id, and store
    /// it as the pending add-provider setup target.
    pub fn submit_provider_name_input(
        &mut self,
        existing_instances: &[ProviderInstance],
    ) -> Option<String> {
        let raw = self.textarea.lines().join(" ");
        let id = Self::normalize_provider_id(&raw)?;
        let setup = self.pending_provider_setup.as_mut()?;
        if existing_instances
            .iter()
            .any(|p| p.id == id && (!setup.editing_existing || p.id != setup.original_id))
        {
            return None;
        }
        self.setup_input_mode = None;
        setup.id = id.clone();
        self.reset_textarea();
        Some(id)
    }

    pub fn submit_pending_provider_base_url(&mut self) -> Option<String> {
        let instance = self.pending_provider_instance()?;
        let raw = self.textarea.lines().join("");
        let url = match instance.backend_preset {
            BackendPreset::Ollama => Self::normalize_ollama_endpoint(&raw)?,
            BackendPreset::OpenWebUi
            | BackendPreset::OpenAiCompatible
            | BackendPreset::OpenRouter => Self::normalize_open_webui_url(&raw)?,
            _ => return None,
        };
        self.setup_input_mode = None;
        if let Some(setup) = self.pending_provider_setup.as_mut() {
            setup.base_url = Some(url.clone());
        }
        self.reset_textarea();
        Some(url)
    }

    pub fn submit_pending_provider_api_key(&mut self) -> Option<String> {
        let token = self.textarea.lines().join("").trim().to_string();
        let existing_token = self
            .pending_provider_setup
            .as_ref()
            .and_then(|setup| setup.api_key.clone());
        let keep_existing = token.is_empty() && self.pending_provider_setup_is_edit();
        if token.is_empty() && !keep_existing {
            return None;
        }
        self.setup_input_mode = None;
        if let Some(setup) = self.pending_provider_setup.as_mut()
            && !keep_existing
        {
            setup.api_key = Some(token.clone());
        }
        self.reset_textarea();
        if keep_existing {
            existing_token
        } else {
            Some(token)
        }
    }

    /// Show the backend-type menu for the pending provider instance.
    pub fn enter_provider_backend_preset_selection_mode(&mut self) {
        self.reset_textarea();
        self.selection.active = true;
        self.selection.kind = Some(SelectionKind::ProviderBackendPreset);
        self.selection.title = "  Select backend type  ";
        self.selection.query.clear();
        let items = BackendPreset::user_visible()
            .iter()
            .map(|service| CompletionItem {
                label: service.label().to_string(),
                detail: service.id().to_string(),
                complete_to: format!("/provider_service {}", service.id()),
                loading: false,
                error: false,
                match_range: None,
            })
            .collect();
        self.set_selection_items(items);
    }

    /// Show the API-type menu for the pending provider instance.
    pub fn enter_provider_api_type_selection_mode(&mut self, backend_preset: &BackendPreset) {
        self.reset_textarea();
        self.selection.active = true;
        self.selection.kind = Some(SelectionKind::ProviderApiType);
        self.selection.title = "  Select API type  ";
        self.selection.query.clear();
        let items = backend_preset
            .def()
            .allowed_apis
            .iter()
            .map(|api| CompletionItem {
                label: api.label().to_string(),
                detail: String::new(),
                complete_to: format!("/provider_api {}", api.label()),
                loading: false,
                error: false,
                match_range: None,
            })
            .collect();
        self.set_selection_items(items);
    }

    pub fn set_pending_provider_backend_preset(&mut self, backend_preset: BackendPreset) {
        if let Some(setup) = self.pending_provider_setup.as_mut() {
            setup.backend_preset = Some(backend_preset);
            setup.api_type = None;
        }
    }

    pub fn set_pending_provider_api_type(&mut self, api_type: ApiType) {
        if let Some(setup) = self.pending_provider_setup.as_mut() {
            setup.api_type = Some(api_type);
        }
    }

    pub fn pending_provider_instance(&self) -> Option<ProviderInstance> {
        let setup = self.pending_provider_setup.as_ref()?;
        let backend_preset = setup.backend_preset.clone()?;
        let api_type = setup
            .api_type
            .clone()
            .unwrap_or_else(|| backend_preset.def().default_api.clone());
        let id = if setup.id.is_empty() {
            self.suggested_pending_provider_id()?
        } else {
            setup.id.clone()
        };
        let mut instance = ProviderInstance::new(id, backend_preset);
        instance.api_type = api_type;
        instance.base_url = setup.base_url.clone();
        instance.api_key = setup.api_key.clone();
        Some(instance)
    }

    pub fn finish_pending_provider_setup(&mut self) -> Option<ProviderInstance> {
        let instance = self.pending_provider_instance()?;
        self.pending_provider_setup = None;
        self.pending_provider_removal = None;
        Some(instance)
    }

    pub fn clear_pending_provider_setup(&mut self) {
        self.pending_provider_setup = None;
        self.pending_provider_removal = None;
    }

    pub fn enter_provider_removal_confirmation_mode(&mut self, instance: &ProviderInstance) {
        self.reset_textarea();
        self.selection.active = true;
        self.selection.kind = Some(SelectionKind::ConfirmProviderRemoval);
        self.selection.title = "  Remove provider?  ";
        self.selection.query.clear();
        self.pending_provider_setup = None;
        self.pending_provider_removal = Some(PendingProviderRemoval {
            id: instance.id.clone(),
        });
        self.set_selection_items(vec![
            CompletionItem {
                label: format!("Remove {}", instance.id),
                detail: format!(
                    "Delete custom provider ({})",
                    instance.backend_preset.label()
                ),
                complete_to: "/provider_remove_confirm".to_string(),
                loading: false,
                error: false,
                match_range: None,
            },
            CompletionItem {
                label: "Cancel".to_string(),
                detail: "Keep provider".to_string(),
                complete_to: "/provider_remove_cancel".to_string(),
                loading: false,
                error: false,
                match_range: None,
            },
        ]);
    }

    pub fn clear_pending_provider_removal(&mut self) {
        self.pending_provider_removal = None;
    }

    /// Switch the textarea into Ollama endpoint input mode.
    pub fn enter_ollama_endpoint_freeform_mode(&mut self) {
        self.exit_selection_mode();
        self.reset_textarea();
        self.setup_input_mode = Some(SetupInputKind::BaseUrl);
        self.ollama_endpoint_input_mode = true;
        if !self.pending_provider_setup_is_edit() {
            self.textarea.insert_str(DEFAULT_OLLAMA_ENDPOINT);
        }
    }

    /// Cancel Ollama endpoint input and return to normal mode.
    pub fn cancel_ollama_endpoint_input(&mut self) {
        self.ollama_endpoint_input_mode = false;
        self.cancel_setup_input();
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
        self.setup_input_mode = None;
        self.reset_textarea();
        Some(url)
    }

    // ── Open WebUI interactive setup ──────────────────────────────────────────

    /// Enter URL input mode for Open WebUI setup.
    pub fn enter_open_webui_url_input_mode(&mut self) {
        self.enter_provider_base_url_input_mode();
        self.open_webui_url_input_mode = true;
    }

    /// Cancel Open WebUI URL input mode.
    pub fn cancel_open_webui_url_input(&mut self) {
        self.open_webui_url_input_mode = false;
        self.open_webui_pending_url = None;
        self.cancel_setup_input();
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
        let url = self.submit_pending_provider_base_url()?;
        self.open_webui_url_input_mode = false;
        self.open_webui_pending_url = Some(url.clone());
        self.enter_provider_api_key_input_mode();
        if let Some(existing_token) = self
            .pending_provider_setup
            .as_ref()
            .and_then(|setup| setup.api_key.as_deref())
        {
            self.textarea.insert_str(existing_token);
        }
        self.open_webui_token_input_mode = true;
        Some(url)
    }

    /// Cancel Open WebUI token input mode.
    pub fn cancel_open_webui_token_input(&mut self) {
        self.open_webui_token_input_mode = false;
        self.open_webui_pending_url = None;
        self.cancel_setup_input();
    }

    /// Submit the token typed in Open WebUI token input mode.
    /// Returns `Some((url, token))` if a pending URL exists and the token is non-empty.
    pub fn take_open_webui_token_input(&mut self) -> Option<(String, String)> {
        let token = self.submit_pending_provider_api_key()?;
        let url = self.open_webui_pending_url.take()?;
        self.open_webui_token_input_mode = false;
        Some((url, token))
    }

    /// Open provider picker for `/login` command.
    pub fn enter_login_selection_mode(&mut self) {
        self.reset_textarea();
        self.selection.active = true;
        self.selection.kind = Some(SelectionKind::LoginProvider);
        self.selection.title = "  Login provider  ";
        self.selection.query.clear();
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
        self.selection.active = false;
        self.selection.kind = None;
        self.selection.items.clear();
        self.selection.all_items.clear();
        self.selection.query.clear();
        self.selection.selected = 0;
        self.selection.scroll = 0;
    }

    /// Returns true when a model fetch should be triggered for the model
    /// selection menu (models not yet loaded, no fetch in flight).
    pub fn should_fetch_models_for_selection(&self) -> bool {
        // Only fetch if the menu shows the loading indicator (model menu).
        self.selection.active
            && self.selection.kind == Some(SelectionKind::Model)
            && self.selection.items.iter().any(|i| i.loading)
            && !self.models_loading
    }

    /// Returns true if the active selection menu supports free-text filtering.
    /// The login-action menu is a fixed short list and disables filtering.
    pub fn selection_filter_enabled(&self) -> bool {
        !matches!(
            self.selection.kind,
            Some(SelectionKind::LoginAction) | Some(SelectionKind::ConfirmProviderRemoval)
        )
    }

    pub fn selection_add_char(&mut self, c: char) {
        // The login action menu is a small fixed list; filtering adds no value.
        if self.selection.kind == Some(SelectionKind::LoginAction) {
            return;
        }
        self.selection.query.push(c);
        self.apply_selection_filter();
    }

    pub fn selection_backspace(&mut self) {
        if self.selection.kind == Some(SelectionKind::LoginAction) {
            return;
        }
        self.selection.query.pop();
        self.apply_selection_filter();
    }

    /// Navigate the selection menu down (wraps around).
    pub fn selection_select_next(&mut self) {
        let len = self.selection.items.len();
        if len > 0 {
            self.selection.selected = (self.selection.selected + 1) % len;
            if self.selection.selected == 0 {
                self.selection.scroll = 0;
            } else {
                self.ensure_selection_visible();
            }
        }
    }

    /// Navigate the selection menu up (wraps around).
    pub fn selection_select_prev(&mut self) {
        let len = self.selection.items.len();
        if len > 0 {
            self.selection.selected = (self.selection.selected + len - 1) % len;
            if self.selection.selected == len - 1 {
                self.selection.scroll = len.saturating_sub(MAX_SELECTION_VISIBLE);
            } else {
                self.ensure_selection_visible();
            }
        }
    }

    /// Jump forward one page (MAX_SELECTION_VISIBLE items) in the selection menu.
    pub fn selection_page_down(&mut self) {
        let len = self.selection.items.len();
        if len > 0 {
            let new = (self.selection.selected + MAX_SELECTION_VISIBLE).min(len - 1);
            self.selection.selected = new;
            self.ensure_selection_visible();
        }
    }

    /// Jump backward one page (MAX_SELECTION_VISIBLE items) in the selection menu.
    pub fn selection_page_up(&mut self) {
        let len = self.selection.items.len();
        if len > 0 {
            self.selection.selected = self
                .selection
                .selected
                .saturating_sub(MAX_SELECTION_VISIBLE);
            self.ensure_selection_visible();
        }
    }

    /// Confirm the currently highlighted selection.
    pub fn apply_selection(&mut self) -> Option<SelectionResult> {
        let item = self.selection.items.get(self.selection.selected)?;
        if item.loading || item.complete_to.is_empty() {
            return None;
        }

        let result = match self.selection.kind {
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
                .map(|name| SelectionResult::Provider(name.to_string()))
                .or_else(|| {
                    (item.complete_to == "/provider_add").then_some(SelectionResult::AddProvider)
                }),
            Some(SelectionKind::ConfirmProviderRemoval) => match item.complete_to.as_str() {
                "/provider_remove_confirm" => self
                    .pending_provider_removal
                    .as_ref()
                    .map(|pending| SelectionResult::RemoveProvider(pending.id.clone())),
                "/provider_remove_cancel" => Some(SelectionResult::CancelProviderRemoval),
                _ => None,
            },
            Some(SelectionKind::ProviderBackendPreset) => item
                .complete_to
                .strip_prefix("/provider_service ")
                .and_then(BackendPreset::from_id)
                .map(SelectionResult::ProviderBackendPreset),
            Some(SelectionKind::ProviderApiType) => item
                .complete_to
                .strip_prefix("/provider_api ")
                .and_then(|label| {
                    self.pending_provider_setup
                        .as_ref()?
                        .backend_preset
                        .as_ref()?
                        .def()
                        .allowed_apis
                        .iter()
                        .find(|api| api.label() == label)
                        .cloned()
                })
                .map(SelectionResult::ProviderApiType),
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
            None => None,
        }?;

        self.exit_selection_mode();
        Some(result)
    }

    pub fn has_pending_ask(&self) -> bool {
        self.ask_user.pending.is_some()
    }

    /// Returns true when the current pending ask allows a free-form typed answer.
    pub fn pending_ask_allows_freeform(&self) -> bool {
        self.ask_user
            .pending
            .as_ref()
            .map(|p| p.allow_freeform)
            .unwrap_or(false)
    }

    /// Returns true when a pending ask is showing its selection menu and does
    /// NOT allow free-form input.
    pub fn ask_user_selection_no_freeform(&self) -> bool {
        self.selection.active
            && self.selection.kind == Some(SelectionKind::AskUser)
            && !self.pending_ask_allows_freeform()
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

        self.ask_user.pending = Some(PendingAsk {
            question: question.clone(),
            options: options.clone(),
            allow_freeform,
        });
        self.ask_user.reply = Some(reply);

        // Don't push an [ask_user] assistant message into the projected
        // display log — the agent's ToolCall message already represents this
        // in the conversation history and UI. Adding an extra assistant
        // message here would corrupt the tool_use / tool_result pairing
        // expected by the Anthropic API.

        if options.is_empty() {
            // No options: go straight to freeform input so the user can type
            // their answer without an intermediate selection-menu step.
            // Store the question for display in the input area.
            self.ask_user.freeform_mode = true;
            self.ask_user.question = Some(question);
            self.exit_selection_mode();
            self.reset_textarea();
            return;
        }

        self.reset_textarea();
        self.selection.active = true;
        self.selection.kind = Some(SelectionKind::AskUser);
        self.selection.title = "  Ask user  ";
        self.selection.query.clear();

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
        if self.pending_ask_allows_freeform() && !self.ask_user.freeform_mode {
            let question = self
                .ask_user
                .pending
                .as_ref()
                .map(|p| p.question.clone())
                .unwrap_or_default();
            self.ask_user.freeform_mode = true;
            self.ask_user.question = Some(question);
        }
    }

    /// While the ask_user selection menu is still visible, mark the freeform
    /// sentinel as selected and activate the brown input field so the user can
    /// see what they are typing.  The selection menu stays open so they can
    /// still navigate back to a predefined option.
    pub fn begin_ask_freeform_typing(&mut self) {
        if !self.pending_ask_allows_freeform() {
            return;
        }
        // Select the freeform sentinel item in the list.
        if let Some(idx) = self
            .selection
            .items
            .iter()
            .position(|item| item.complete_to == "/ask_user_freeform")
        {
            self.selection.selected = idx;
            self.ensure_selection_visible();
        }
        // Activate the brown input field and show the question as a hint.
        if !self.ask_user.freeform_mode {
            let question = self
                .ask_user
                .pending
                .as_ref()
                .map(|p| p.question.clone())
                .unwrap_or_default();
            self.ask_user.freeform_mode = true;
            self.ask_user.question = Some(question);
        }
    }

    /// Clear freeform typing state without dismissing the selection menu.
    /// Called when the user navigates away from the freeform sentinel.
    pub fn cancel_ask_freeform_typing(&mut self) {
        self.ask_user.freeform_mode = false;
        self.ask_user.question = None;
        self.reset_textarea();
    }

    pub fn submit_pending_ask_answer(&mut self) {
        let Some(pending) = self.ask_user.pending.as_ref() else {
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
        if self.ask_user.pending.is_none() {
            return;
        }
        // Don't push the answer as a plain user message — the agent's
        // ToolResult message will represent it in history and UI.
        self.finish_pending_ask(AskUserResponse::Answer(answer));
    }

    pub fn cancel_pending_ask(&mut self) {
        if self.ask_user.pending.is_none() {
            return;
        }
        self.finish_pending_ask(AskUserResponse::Cancelled);
        self.abort_agent_loop();
    }

    fn finish_pending_ask(&mut self, answer: AskUserResponse) {
        if let Some(reply) = self.ask_user.reply.take() {
            let _ = reply.send(answer);
        }
        self.ask_user.pending = None;
        self.ask_user.freeform_mode = false;
        self.ask_user.question = None;
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
        if !self.login.active {
            return;
        }

        let mut items: Vec<CompletionItem> = Vec::new();
        if self.login.url.is_some() {
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
        if self.login.code.is_some() {
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

        self.selection.active = true;
        self.selection.kind = Some(SelectionKind::LoginAction);
        self.selection.title = "  Login actions  ";
        self.selection.query.clear();
        self.set_selection_items(items);
    }

    /// Execute a login action chosen from the action menu.
    pub fn apply_login_action(&mut self, action: LoginActionKind) {
        // Always close the menu first so the login panel is visible behind
        // the feedback message written to login_info.
        self.exit_selection_mode();

        match action {
            LoginActionKind::OpenBrowser => {
                let Some(url) = self.login.url.clone() else {
                    return;
                };
                match auth::open_url::open_url(&url) {
                    Ok(()) => {
                        log::debug!("login: opened browser for {url}");
                        self.login.info = "Browser opened.".to_string();
                    }
                    Err(e) => {
                        log::debug!("login: failed to open browser: {e}");
                        self.login.info =
                            format!("Could not open browser: {e}. Copy the URL manually.");
                    }
                }
            }
            LoginActionKind::CopyUrl => {
                let Some(url) = self.login.url.clone() else {
                    return;
                };
                match self.clipboard_set(url) {
                    Ok(()) => {
                        log::debug!("login: copied URL to clipboard");
                        self.login.info = "URL copied to clipboard.".to_string();
                    }
                    Err(e) => {
                        log::debug!("login: clipboard unavailable: {e}");
                        self.login.info =
                            "Clipboard unavailable — select the URL above to copy.".to_string();
                    }
                }
            }
            LoginActionKind::CopyCode => {
                let Some(code) = self.login.code.clone() else {
                    return;
                };
                match self.clipboard_set(code) {
                    Ok(()) => {
                        log::debug!("login: copied device code to clipboard");
                        self.login.info = "Code copied to clipboard.".to_string();
                    }
                    Err(e) => {
                        log::debug!("login: clipboard unavailable: {e}");
                        self.login.info =
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

    /// Copy `text` to the clipboard using the persistent `self.login.clipboard`
    /// instance. Lazily initialises it on first call. Returns an error
    /// string on failure.
    fn clipboard_set(&mut self, text: String) -> Result<(), String> {
        // Lazily open the clipboard and keep it alive for the whole login
        // session. On Linux the clipboard is owner-based: dropping the
        // Clipboard instance clears the content for other applications.
        if self.login.clipboard.is_none() {
            match arboard::Clipboard::new() {
                Ok(cb) => self.login.clipboard = Some(cb),
                Err(e) => return Err(e.to_string()),
            }
        }
        self.login
            .clipboard
            .as_mut()
            .unwrap()
            .set_text(text)
            .map_err(|e| e.to_string())
    }

    pub fn start_login(&mut self, provider: &str) {
        if self.login.active {
            return;
        }

        log::debug!("login start requested: provider={provider}");

        self.login.active = true;
        self.login.provider = Some(provider.to_string());
        self.login.info = format!("Starting login for {provider}...");
        self.login.url = None;
        self.login.code = None;
        self.login.auth_flow = None;

        let cancel = Arc::new(AtomicBool::new(false));
        self.login.cancel = Some(cancel.clone());
        let tx = self.app_event_tx();
        let provider = provider.to_string();

        tokio::spawn(async move {
            auth::login_provider(&provider, tx, cancel).await;
        });
    }

    pub fn cancel_login(&mut self) {
        if let Some(cancel) = &self.login.cancel {
            log::debug!("login cancel requested");
            cancel.store(true, Ordering::Relaxed);
        }
    }

    pub fn apply_login_event(&mut self, ev: LoginEvent) {
        match ev {
            LoginEvent::Info(msg) => {
                log::debug!("login info: {msg}");
                self.login.info = msg;
            }
            LoginEvent::AuthCode { url, code, flow } => {
                log::debug!("login auth prompt: url={} has_code={}", url, code.is_some());
                self.login.url = Some(url);
                self.login.code = code;
                self.login.auth_flow = Some(flow);
                // Automatically open the action menu so the user can choose
                // how to proceed without needing to know any keyboard shortcuts.
                self.enter_login_action_menu();
            }
            LoginEvent::Success { provider } => {
                log::debug!("login success: provider={provider}");
                self.live_turn.notices.push(Message::assistant(format!(
                    "[login successful: {provider}]"
                )));
                self.bump_log_revision();
                self.persist_messages();
                self.login.needs_rebuild = true;
            }
            LoginEvent::Error { provider, message } => {
                log::debug!("login error: provider={} err={}", provider, message);
                self.live_turn.notices.push(Message::assistant(format!(
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
                self.login.refresh_in_progress = false;
                if success {
                    // Silently refresh — no message added to the chat log or
                    // LLM history; the retry will continue seamlessly.
                    self.login.needs_rebuild = true;
                } else {
                    self.login.retry_after_refresh = false;
                    self.live_turn.notices.push(Message::assistant(format!(
                        "[token refresh failed for {provider}: {message}. Run /login {provider}]"
                    )));
                    self.bump_log_revision();
                    self.persist_messages();
                }
            }
            LoginEvent::Finished => {
                log::debug!("login flow finished");
                self.login.active = false;
                self.login.provider = None;
                self.login.cancel = None;
                self.login.auth_flow = None;
                self.exit_selection_mode();
                // Drop the clipboard instance; on Linux this releases clipboard
                // ownership so the content is no longer served by this process.
                self.login.clipboard = None;
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

    /// Ensure a [`SessionState`] exists for the current session before submitting
    /// a user message. Creates the session and loads (or initialises) the state
    /// if needed. No-op when session state is already populated.
    ///
    /// When persistent session storage is unavailable, falls back to an
    /// ephemeral event log in the system temp directory so the refactored
    /// ownership model still holds: committed conversation state always enters
    /// through `SessionEvent` ingestion and `SessionState` is always present
    /// before a turn launches.
    fn ensure_event_log_for_submit(&mut self) {
        if self.session_state.is_some() {
            return;
        }
        let session_id = self.ensure_session_id();
        if let Some(store) = &self.session_store {
            match store.load_events(&session_id) {
                Ok(log) => {
                    self.session_state = Some(SessionState::from_event_log(log));
                    return;
                }
                Err(e) => {
                    log::debug!("failed to load event log for session {session_id}: {e}");
                }
            }
        }

        // Persistence unavailable: create an ephemeral event log so all turn
        // flows still operate through SessionState and SessionEvent ingestion.
        let path = std::env::temp_dir().join(format!("tau-ephemeral-session-{session_id}.jsonl"));
        match crate::event_log::EventLog::load(&path) {
            Ok(log) => {
                self.session_state = Some(SessionState::from_event_log(log));
            }
            Err(e) => {
                log::debug!("failed to create ephemeral event log for session {session_id}: {e}");
            }
        }
    }

    fn persist_messages(&mut self) {
        // Persistence is now driven incrementally by `flush_turn_events` and
        // `append_event_immediate` via the event log.  This method is kept as
        // a call-site placeholder so that callers do not need to be updated
        // individually; its only remaining job is to refresh the resume hint.
        //
        // The legacy full-rewrite path is intentionally not called here any
        // more. It will be removed in the final cleanup once
        // `SessionStore::save_messages` is retired.
        self.refresh_resume_availability();
    }

    /// Append a user-visible user message to the active session.
    ///
    /// When event-log persistence is available, writes the durable event and
    /// lets the display projection update from that source of truth. When
    /// persistence is unavailable, falls back to a transient visible message.
    fn append_user_message(&mut self, content: String) {
        self.ensure_event_log_for_submit();
        assert!(
            self.session_state.is_some(),
            "append_user_message called before session_state was initialised"
        );
        self.append_event_immediate(SessionEvent::UserMessage {
            content,
            timestamp: Self::now_ts(),
        });
    }

    /// Export the current visible session to a standalone HTML file.
    pub fn export_session_html(&mut self, requested_path: Option<&str>) {
        let path = export::resolve_export_path(&self.current_cwd, requested_path);
        // Use the committed session state projection when available.
        let display_messages;
        let messages_ref: &[Message] = if let Some(ss) = &self.session_state {
            display_messages = ss.projected_display_messages();
            &display_messages
        } else {
            &[]
        };
        let html = export::build_session_export_html(
            messages_ref,
            &self.current_cwd,
            &self.current_provider,
            &self.current_model,
            self.current_session_id.as_deref(),
        );

        match export::write_export_file(&path, &html) {
            Ok(()) => {
                self.live_turn.notices.push(Message::assistant(format!(
                    "[session exported to {}]",
                    path.display()
                )));
            }
            Err(e) => {
                self.live_turn
                    .notices
                    .push(Message::assistant(format!("[export failed: {e}]")));
            }
        }
        self.bump_log_revision();
        self.persist_messages();
    }

    /// Clear the conversation history and reset the input area.
    pub fn new_conversation(&mut self) {
        self.current_session_id = None;
        self.session_state = None;
        self.live_turn.clear_all();
        self.pending_turn_events.clear();
        self.runtime.queued_steering.clear();
        self.runtime.steering_tx = None;
        self.streaming_status = None;
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
        let session_events = self
            .session_state
            .as_ref()
            .expect("start_agent_task called before session_state was initialised")
            .events()
            .to_vec();
        let config = AgentLoopConfig {
            tools: self.agent_config.tools.clone(),
            file_tracker: Arc::clone(&self.agent_config.file_tracker),
            tool_output_log: Arc::clone(&self.agent_config.tool_output_log),
            session_events,
            current_model: self.current_model.clone(),
            auto_compaction_enabled: true,
            manual_compaction_instructions: self.pending_manual_compaction_instructions.take(),
            before_tool_call: None,
            after_tool_call: None,
        };
        let (steering_tx, steering_rx) = tokio::sync::mpsc::unbounded_channel();
        self.runtime.steering_tx = Some(steering_tx);
        self.runtime.queued_steering.clear();
        self.bump_log_revision();

        let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        self.runtime.cancel_tx = Some(cancel_tx);

        let provider = Arc::clone(provider);
        let tx = self.app_event_tx();
        self.runtime.agent_task = Some(tokio::spawn(async move {
            run_agent_loop(llm_messages, config, provider, tx, steering_rx, cancel_rx).await;
        }));
    }

    /// Build the message list to send to the LLM from the committed session history.
    ///
    /// Prepends the system prompt (if set) and then appends the committed LLM
    /// projection from `SessionState`. Live-turn state (streaming assistant text,
    /// in-flight tools, notices, shell output) is deliberately excluded.
    ///
    /// # Panics
    ///
    /// Panics if called before `session_state` has been initialised. This is a
    /// programming error: all turn-launch paths must ensure committed session
    /// state exists first.
    fn prepare_llm_messages(&mut self) -> Vec<Message> {
        let mut msgs: Vec<Message> = self.system_prompt.iter().map(Message::system).collect();

        let ss = self
            .session_state
            .as_mut()
            .expect("prepare_llm_messages called before session_state was initialised");
        msgs.extend(ss.llm_messages().iter().cloned());

        msgs
    }

    /// Set streaming flags and spawn the agent task using the current history.
    ///
    /// Call after pushing any new user message(s) and persisting state.
    /// Does **not** perform the pre-flight token check — callers are
    /// responsible for calling `check_token_preflight` before this.
    fn launch_turn(&mut self, provider: &DynProvider) {
        self.clear_abort_status_notice();
        self.ensure_event_log_for_submit();
        assert!(
            self.session_state.is_some(),
            "launch_turn called before session_state was initialised"
        );
        self.streaming_status = Some(StreamingStatus::Waiting);
        self.login.auth_retry_budget = 1;
        self.latest_usage = None;
        self.auto_scroll = true;
        let llm_messages = self.prepare_llm_messages();
        self.start_agent_task(llm_messages, provider);
    }

    /// Queue a user steering message while the agent loop is running.
    pub fn enqueue_steering_from_input(&mut self) {
        let lines: Vec<String> = self.textarea.lines().to_vec();
        let text = lines.join("\n");
        let trimmed = text.trim().to_string();
        if trimmed.is_empty() || !self.streaming() || self.login.active {
            return;
        }

        let Some(tx) = self.runtime.steering_tx.as_ref() else {
            return;
        };

        if tx.send(trimmed.clone()).is_ok() {
            self.runtime.queued_steering.push(trimmed);
            self.bump_log_revision();
            self.reset_textarea();
            self.auto_scroll = true;
        }
    }

    /// Trigger a compaction-only task against the current session state.
    pub fn trigger_manual_compaction(
        &mut self,
        instructions: Option<String>,
        provider: &DynProvider,
    ) {
        if self.streaming() || self.login.active {
            return;
        }

        self.ensure_event_log_for_submit();
        self.pending_manual_compaction_instructions = instructions;

        if self.check_token_preflight(RetryTarget::AgentTurn) {
            return;
        }

        self.streaming_status = Some(StreamingStatus::Waiting);
        self.login.auth_retry_budget = 1;
        self.auto_scroll = true;
        let llm_messages = self.prepare_llm_messages();
        self.start_agent_task(llm_messages, provider);
    }

    /// Take the textarea content and start the agent loop.
    pub fn submit(&mut self, provider: &DynProvider) {
        let lines: Vec<String> = self.textarea.lines().to_vec();
        let text = lines.join("\n");
        let trimmed = text.trim().to_string();
        if trimmed.is_empty() || self.streaming() || self.login.active {
            return;
        }

        self.append_user_message(trimmed);
        self.persist_messages();
        self.reset_textarea();

        // Proactive token refresh check before starting the request.
        if self.check_token_preflight(RetryTarget::AgentTurn) {
            // Refresh triggered; request will be retried after refresh completes.
            return;
        }

        self.launch_turn(provider);
    }

    /// Submit a pre-built text string directly to the agent loop, bypassing the
    /// textarea.  Used by `/skill:<name>` command expansion.
    pub fn submit_with_text(&mut self, text: String, provider: &DynProvider) {
        if text.trim().is_empty() || self.streaming() || self.login.active {
            return;
        }

        let trimmed = text.trim().to_string();
        self.append_user_message(trimmed);
        self.persist_messages();
        self.reset_textarea();

        // Proactive token refresh check before starting the request
        if self.check_token_preflight(RetryTarget::AgentTurn) {
            // Refresh triggered; request will be retried after refresh completes
            return;
        }

        self.launch_turn(provider);
    }

    pub fn retry_last_request(&mut self, provider: &DynProvider) {
        if self.streaming() || self.login.active {
            return;
        }

        // Pop the trailing error notice if present. Error messages live in
        // `live_turn.notices` (they are not committed session events).
        if let Some(last) = self.live_turn.notices.last()
            && last.role == Role::Assistant
            && (last.content.starts_with("[Error:") || last.content.starts_with("[token refresh"))
        {
            self.live_turn.notices.pop();
            self.bump_log_revision();
            self.persist_messages();
        }

        self.launch_turn(provider);
    }

    fn append_abort_results_for_pending_tool_calls(&mut self) {
        // Find tool call IDs in the pending turn buffer that haven't been
        // completed with a ToolResult yet.
        let mut pending_ids: Vec<String> = Vec::new();
        for ev in &self.pending_turn_events {
            match ev {
                SessionEvent::ToolCall { id, .. } => {
                    if !pending_ids.iter().any(|p| p == id) {
                        pending_ids.push(id.clone());
                    }
                }
                SessionEvent::ToolResult { id, .. } => {
                    pending_ids.retain(|p| p != id);
                }
                _ => {}
            }
        }

        for id in pending_ids {
            if let Some(entry) = self.live_turn.find_tool_entry_mut(&id)
                && entry.result.is_none()
            {
                entry.result = Some(LiveToolResult {
                    content: "failure: aborted by user".to_string(),
                    is_error: true,
                    display_range: None,
                });
            }

            let name = self
                .pending_turn_events
                .iter()
                .rev()
                .find_map(|e| {
                    if let SessionEvent::ToolCall { id: cid, name, .. } = e {
                        if cid == &id { Some(name.clone()) } else { None }
                    } else {
                        None
                    }
                })
                .unwrap_or_default();

            self.pending_turn_events.push(SessionEvent::ToolResult {
                id,
                name,
                content: "failure: aborted by user".to_string(),
                is_error: true,
                display_range: None,
                timestamp: Self::now_ts(),
            });
        }
    }

    fn clear_abort_status_notice(&mut self) {
        if matches!(
            self.streaming_status,
            Some(StreamingStatus::CompletedMessage(ref s)) if s == "[agent loop aborted]"
        ) {
            self.streaming_status = None;
        }
    }

    pub fn abort_agent_loop(&mut self) {
        if let Some(handle) = self.runtime.agent_task.take() {
            // Signal cooperative cancellation first; hard-abort as fallback.
            if let Some(tx) = self.runtime.cancel_tx.take() {
                let _ = tx.send(true);
            }
            handle.abort();
            self.streaming_status = Some(StreamingStatus::CompletedMessage(
                "[agent loop aborted]".to_string(),
            ));
            self.last_output_at = None;
            self.runtime.steering_tx = None;
            self.runtime.queued_steering.clear();
            self.append_abort_results_for_pending_tool_calls();
            self.finalise_assistant_turn_event();
            self.flush_turn_events();
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

    /// Current wall-clock time as seconds since UNIX epoch.
    fn now_ts() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }

    /// Flush `pending_turn_events` to the event log and clear the buffer.
    ///
    /// Called at every turn-completion boundary (`TurnEnd`, `Done`, `Error`).
    fn flush_turn_events(&mut self) {
        if self.pending_turn_events.is_empty() {
            return;
        }
        let batch: Vec<SessionEvent> = std::mem::take(&mut self.pending_turn_events);
        if let Some(ss) = self.session_state.as_mut() {
            if let Err(e) = ss.append_batch(&batch) {
                log::debug!("failed to append turn events to session state: {e}");
            }
            // append_batch rebuilds the committed display projection from durable events.
            // Clear the in-flight turn fields (assistant content, tool entries) from
            // LiveTurnState — they are now represented in committed display state.
            // Notices are preserved (they survive turn boundaries).
            self.live_turn.clear_turn();
        }
    }

    /// Append a single event to the event log immediately (for events that are
    /// complete units on their own: `UserMessage`, `ModelChanged`,
    /// `ThinkingLevelChanged`).
    fn append_event_immediate(&mut self, ev: SessionEvent) {
        if let Some(ss) = self.session_state.as_mut()
            && let Err(e) = ss.append_immediate(ev)
        {
            log::debug!("failed to append event to session state: {e}");
        }
    }

    pub fn apply_app_event(&mut self, ev: AppEvent) {
        match ev {
            AppEvent::Agent(ev) => {
                self.apply_agent_event(ev);
                self.drain_app_events();
            }
            AppEvent::ModelsReady(result) => self.apply_model_list(result),
            AppEvent::Login(ev) => self.apply_login_event(ev),
            AppEvent::AskUser(req) => self.receive_ask_request(req),
        }
    }

    pub fn apply_agent_event(&mut self, ev: AgentEvent) {
        match ev {
            AgentEvent::ThinkingToken(token) => {
                if !token.trim().is_empty() {
                    self.last_output_at = Some(std::time::Instant::now());
                }
                self.live_turn
                    .assistant_thinking
                    .get_or_insert_with(String::new)
                    .push_str(&token);
                self.bump_log_revision();
            }
            AgentEvent::Usage(usage) => {
                self.latest_usage = Some(usage);
            }
            AgentEvent::TextToken { text, phase } => {
                if !text.trim().is_empty() {
                    self.last_output_at = Some(std::time::Instant::now());
                }
                self.live_turn.assistant_content.push_str(&text);
                if phase != AssistantPhase::Unknown {
                    self.live_turn.assistant_phase = phase;
                }
                self.bump_log_revision();
            }
            AgentEvent::ToolIntentStart => {
                self.live_turn.assistant_phase = AssistantPhase::Provisional;
                self.bump_log_revision();
            }
            AgentEvent::SteeringConsumed { text } => {
                self.last_output_at = Some(std::time::Instant::now());
                if let Some(pos) = self.runtime.queued_steering.iter().position(|m| m == &text) {
                    self.runtime.queued_steering.remove(pos);
                }
                // Steering messages are user messages — append immediately.
                self.append_user_message(text);
                self.bump_log_revision();
            }
            AgentEvent::StatusUpdate(msg) => {
                if !msg.is_empty() {
                    self.last_output_at = Some(std::time::Instant::now());
                }
                self.streaming_status = if msg.is_empty() {
                    Some(StreamingStatus::Waiting)
                } else {
                    Some(StreamingStatus::Message(msg))
                };
                self.bump_log_revision();
            }
            AgentEvent::Compacting => {
                self.last_output_at = Some(std::time::Instant::now());
                self.streaming_status = Some(StreamingStatus::Message("compacting…".to_string()));
                self.bump_log_revision();
            }
            AgentEvent::CompactionDone {
                summary,
                trigger_reason,
                context_window,
                reserve_tokens,
                keep_recent_tokens,
                tokens_before,
                tokens_after,
                retained_event_count,
                read_files,
                modified_files,
            } => {
                let ev = SessionEvent::CompactionSummary {
                    summary,
                    trigger_reason,
                    context_window,
                    reserve_tokens,
                    keep_recent_tokens,
                    tokens_before,
                    tokens_after,
                    retained_event_count: Some(retained_event_count),
                    read_files,
                    modified_files,
                    timestamp: Self::now_ts(),
                };
                self.append_event_immediate(ev);
                // append_immediate already updates display incrementally via SessionState.
                self.latest_usage = Some(UsageStats {
                    input_tokens: Some(tokens_after),
                    output_tokens: None,
                    total_tokens: Some(tokens_after),
                });
                self.auto_scroll = true;
                self.bump_log_revision();
                self.persist_messages();
            }
            AgentEvent::ToolCallStart { id, name, args } => {
                self.last_output_at = Some(std::time::Instant::now());
                self.live_turn.tool_entries.push(LiveToolEntry {
                    id: id.clone(),
                    name: name.clone(),
                    args: args.clone(),
                    result: None,
                });
                // ToolCall is buffered; only flushed together with its result.
                self.pending_turn_events.push(SessionEvent::ToolCall {
                    id,
                    name,
                    args,
                    timestamp: Self::now_ts(),
                });
                self.bump_log_revision();
            }
            AgentEvent::ToolCallEnd { id, result } => {
                self.last_output_at = Some(std::time::Instant::now());
                let display_range = result.truncation.as_ref().map(|tr| DisplayRange {
                    first_line: tr.first_kept_line,
                    last_line: tr.first_kept_line + tr.output_lines - 1,
                    total_lines: tr.total_lines,
                });
                // Update the matching live tool entry with its result.
                if let Some(entry) = self.live_turn.find_tool_entry_mut(&id) {
                    entry.result = Some(LiveToolResult {
                        content: result.content.clone(),
                        is_error: result.is_error,
                        display_range: display_range.clone(),
                    });
                }
                // Resolve tool name from the matching pending ToolCall.
                let name = self
                    .pending_turn_events
                    .iter()
                    .rev()
                    .find_map(|e| {
                        if let SessionEvent::ToolCall { id: cid, name, .. } = e {
                            if cid == &id { Some(name.clone()) } else { None }
                        } else {
                            None
                        }
                    })
                    .unwrap_or_default();
                self.pending_turn_events.push(SessionEvent::ToolResult {
                    id,
                    name,
                    content: result.content,
                    is_error: result.is_error,
                    display_range,
                    timestamp: Self::now_ts(),
                });
                self.bump_log_revision();
            }
            AgentEvent::ExternalFileChange {
                paths: _,
                notification,
            } => {
                self.last_output_at = Some(std::time::Instant::now());
                // External file change notifications are user-visible context
                // injected into the conversation — treat as UserMessage.
                self.append_user_message(notification);
                self.bump_log_revision();
            }
            AgentEvent::TurnEnd => {
                self.streaming_status = Some(StreamingStatus::Waiting);
                // Finalise the assistant message in the pending buffer before
                // flushing, using the current in-memory messages state.
                self.finalise_assistant_turn_event();
                self.flush_turn_events();
                self.persist_messages();
            }
            AgentEvent::Done => {
                self.streaming_status = None;
                self.last_output_at = None;
                self.runtime.agent_task = None;
                self.runtime.cancel_tx = None;
                self.runtime.steering_tx = None;
                self.runtime.queued_steering.clear();
                self.bump_log_revision();
                // The final TurnEnd already flushed the turn buffer.
                // Done only cleans up live streaming state.
                self.persist_messages();
            }
            AgentEvent::Error(e) => {
                self.streaming_status = None;
                self.last_output_at = None;
                self.runtime.agent_task = None;
                self.runtime.cancel_tx = None;
                self.runtime.steering_tx = None;
                self.runtime.queued_steering.clear();
                self.bump_log_revision();

                let is_unauthorized = e.kind == crate::llm::ProviderErrorKind::Unauthorized;

                if is_unauthorized
                    && self.login.auth_retry_budget > 0
                    && self.trigger_auth_refresh(RetryTarget::AgentTurn)
                {
                    log::debug!(
                        "received 401, refresh triggered: provider={} remaining_budget= {}",
                        self.current_provider,
                        self.login.auth_retry_budget
                    );
                    self.login.auth_retry_budget -= 1;
                    self.streaming_status = None;
                    // Refresh triggered; retry will happen automatically after refresh completes.
                    // Discard pending events and in-flight turn state — the turn will be retried.
                    self.pending_turn_events.clear();
                    self.live_turn.clear_turn();
                } else {
                    let provider_label = active_provider_display_name(
                        &self.current_provider,
                        &self.provider_instances,
                    );
                    let rendered = format_provider_error_for_display(&provider_label, &e);
                    self.live_turn
                        .notices
                        .push(Message::assistant(format!("[Error: {rendered}]")));
                    self.bump_log_revision();
                    self.streaming_status = None;
                    // Discard any partially accumulated assistant/tool events
                    // and append a TurnError instead.
                    self.pending_turn_events.clear();
                    self.live_turn.clear_turn();
                    self.append_event_immediate(SessionEvent::TurnError {
                        message: format!("[Error: {rendered}]"),
                        timestamp: Self::now_ts(),
                    });
                    self.persist_messages();
                }
            }
        }
    }

    /// Assemble the `AssistantMessage` session event from `LiveTurnState` fields
    /// and insert it into `pending_turn_events`.
    ///
    /// Called just before flushing the turn buffer so that the final content,
    /// thinking, phase, and usage are captured directly from `live_turn` —
    /// not read back from committed display state.
    fn finalise_assistant_turn_event(&mut self) {
        let content = self.live_turn.assistant_content.clone();
        let thinking = self.live_turn.assistant_thinking.clone();
        let phase = self.live_turn.assistant_phase;

        // Don't record a completely empty assistant turn with no tools either.
        let has_tools = self
            .pending_turn_events
            .iter()
            .any(|e| matches!(e, SessionEvent::ToolCall { .. }));
        if content.is_empty() && thinking.is_none() && !has_tools {
            return;
        }

        let ev = SessionEvent::AssistantMessage {
            content,
            thinking,
            phase: if phase == AssistantPhase::Unknown {
                AssistantPhase::Final
            } else {
                phase
            },
            usage: self.latest_usage,
            timestamp: Self::now_ts(),
        };

        // Replace existing AssistantMessage in the buffer or prepend.
        if let Some(pos) = self
            .pending_turn_events
            .iter()
            .position(|e| matches!(e, SessionEvent::AssistantMessage { .. }))
        {
            self.pending_turn_events[pos] = ev;
        } else {
            self.pending_turn_events.insert(0, ev);
        }
    }

    pub fn drain_app_events(&mut self) {
        loop {
            match self.runtime.app_event_rx.try_recv() {
                Ok(AppEvent::Agent(ev)) => self.apply_agent_event(ev),
                Ok(other) => self.apply_app_event(other),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{App, StreamingStatus, format_provider_error_for_display};
    use crate::{
        agent::{
            AgentLoopConfig,
            types::{AskRequest, AskUserOption, AskUserResponse},
        },
        llm::{Message, ProviderError, Role},
        provider_instance::{ApiType, BackendPreset, ProviderInstance},
        thinking::ThinkingLevel,
    };

    fn make_app() -> App {
        App::new(
            "gpt-4o",
            "openai",
            ThinkingLevel::Off,
            AgentLoopConfig {
                tools: Default::default(),
                file_tracker: std::sync::Arc::new(std::sync::Mutex::new(
                    crate::agent::FileTracker::new(),
                )),
                tool_output_log: std::sync::Arc::new(std::sync::Mutex::new(
                    crate::agent::ToolOutputLog::new("test-session"),
                )),
                session_events: vec![],
                current_model: "gpt-4o".to_string(),
                auto_compaction_enabled: true,
                manual_compaction_instructions: None,
                before_tool_call: None,
                after_tool_call: None,
            },
        )
    }

    fn install_test_agent_task(app: &mut App) {
        app.runtime.agent_task = Some(tokio::spawn(async {
            std::future::pending::<()>().await;
        }));
    }

    #[test]
    fn setup_input_kind_uses_service_specific_prompts() {
        assert_eq!(
            super::SetupInputKind::Name.prompt_label(None),
            "provider instance name: "
        );

        let mut open_webui = ProviderInstance::new("work-webui", BackendPreset::OpenWebUi);
        open_webui.api_type = ApiType::OpenAiCompatible;
        assert_eq!(
            super::SetupInputKind::BaseUrl.prompt_label(Some(&open_webui)),
            "open-webui URL: "
        );
        assert_eq!(
            super::SetupInputKind::ApiKey.prompt_label(Some(&open_webui)),
            "open-webui token: "
        );

        let mut openrouter = ProviderInstance::new("router", BackendPreset::OpenRouter);
        openrouter.api_type = ApiType::OpenAiCompatible;
        assert_eq!(
            super::SetupInputKind::BaseUrl.prompt_label(Some(&openrouter)),
            "URL: "
        );
        assert_eq!(
            super::SetupInputKind::ApiKey.prompt_label(Some(&openrouter)),
            "OpenRouter API key: "
        );

        let mut ollama = ProviderInstance::new("gpu-box", BackendPreset::Ollama);
        ollama.api_type = ApiType::OllamaChatApi;
        assert_eq!(
            super::SetupInputKind::BaseUrl.prompt_label(Some(&ollama)),
            "ollama URL: "
        );

        let mut compat = ProviderInstance::new("test", BackendPreset::OpenAiCompatible);
        compat.api_type = ApiType::OpenAiCompatible;
        assert_eq!(
            super::SetupInputKind::BaseUrl.prompt_label(Some(&compat)),
            "URL: "
        );
        assert_eq!(
            super::SetupInputKind::ApiKey.prompt_label(Some(&compat)),
            "API key: "
        );
    }

    #[test]
    fn enter_provider_selection_mode_lists_add_and_provider_entries() {
        let mut app = make_app();
        let providers = vec![
            ProviderInstance::new("copilot", BackendPreset::Copilot),
            ProviderInstance::new("gpu-box", BackendPreset::Ollama),
            ProviderInstance::new("work-webui", BackendPreset::OpenWebUi),
        ];

        app.enter_provider_selection_mode(&providers);

        let items: Vec<_> = app
            .selection
            .items
            .iter()
            .map(|item| item.complete_to.as_str())
            .collect();

        assert!(items.contains(&"/provider_add"));
        assert!(items.contains(&"/provider copilot"));
        assert!(items.contains(&"/provider gpu-box"));
        assert!(items.contains(&"/provider work-webui"));
    }

    #[test]
    fn enter_provider_removal_confirmation_mode_tracks_target_provider() {
        let mut app = make_app();
        let instance = ProviderInstance::new("gpu-box", BackendPreset::Ollama);

        app.enter_provider_removal_confirmation_mode(&instance);

        assert_eq!(
            app.selection.kind,
            Some(super::SelectionKind::ConfirmProviderRemoval)
        );
        assert_eq!(app.selection.title, "  Remove provider?  ");
        assert_eq!(
            app.pending_provider_removal
                .as_ref()
                .map(|pending| pending.id.as_str()),
            Some("gpu-box")
        );
        let items: Vec<_> = app
            .selection
            .items
            .iter()
            .map(|item| item.complete_to.as_str())
            .collect();
        assert_eq!(
            items,
            vec!["/provider_remove_confirm", "/provider_remove_cancel"]
        );
    }

    #[test]
    fn apply_selection_returns_remove_provider_confirmation() {
        let mut app = make_app();
        let instance = ProviderInstance::new("gpu-box", BackendPreset::Ollama);
        app.enter_provider_removal_confirmation_mode(&instance);
        app.selection.selected = 0;

        let result = app.apply_selection();

        assert!(matches!(
            result,
            Some(super::SelectionResult::RemoveProvider(id)) if id == "gpu-box"
        ));
    }

    #[test]
    fn apply_selection_returns_cancel_provider_removal() {
        let mut app = make_app();
        let instance = ProviderInstance::new("gpu-box", BackendPreset::Ollama);
        app.enter_provider_removal_confirmation_mode(&instance);
        app.selection.selected = 1;

        let result = app.apply_selection();

        assert!(matches!(
            result,
            Some(super::SelectionResult::CancelProviderRemoval)
        ));
    }

    #[test]
    fn clear_pending_provider_setup_clears_pending_provider_removal() {
        let mut app = make_app();
        let instance = ProviderInstance::new("gpu-box", BackendPreset::Ollama);
        app.enter_provider_removal_confirmation_mode(&instance);

        app.clear_pending_provider_setup();

        assert!(app.pending_provider_removal.is_none());
    }

    #[test]
    fn enter_provider_backend_preset_selection_mode_uses_backend_type_title() {
        let mut app = make_app();
        app.enter_provider_backend_preset_selection_mode();
        assert_eq!(app.selection.title, "  Select backend type  ");
    }

    #[test]
    fn enter_provider_backend_preset_selection_mode_lists_only_user_addable_backends() {
        let mut app = make_app();
        app.enter_provider_backend_preset_selection_mode();

        let labels: Vec<_> = app
            .selection
            .items
            .iter()
            .map(|item| item.label.as_str())
            .collect();

        assert_eq!(
            labels,
            vec!["Ollama", "Open WebUI", "OpenAI-compatible endpoint"]
        );
    }

    #[test]
    fn submit_pending_provider_base_url_stores_openai_compatible_endpoint() {
        let mut app = make_app();
        app.pending_provider_setup = Some(super::PendingProviderSetup::new("test".to_string()));
        app.set_pending_provider_backend_preset(BackendPreset::OpenAiCompatible);
        app.set_pending_provider_api_type(ApiType::OpenAiCompatible);
        app.enter_provider_base_url_input_mode();
        app.textarea.insert_str("test");

        let url = app
            .submit_pending_provider_base_url()
            .expect("normalized endpoint url");
        assert_eq!(url, "https://test");
        assert_eq!(
            app.pending_provider_instance()
                .as_ref()
                .and_then(|p| p.base_url.as_deref()),
            Some("https://test")
        );
    }

    #[test]
    fn submit_pending_provider_base_url_stores_openrouter_endpoint() {
        let mut app = make_app();
        app.pending_provider_setup = Some(super::PendingProviderSetup::new("router".to_string()));
        app.set_pending_provider_backend_preset(BackendPreset::OpenRouter);
        app.set_pending_provider_api_type(ApiType::OpenAiCompatible);
        app.enter_provider_base_url_input_mode();
        app.textarea.insert_str("openrouter.ai/api/v1");

        let url = app
            .submit_pending_provider_base_url()
            .expect("normalized openrouter url");
        assert_eq!(url, "https://openrouter.ai/api/v1");
        assert_eq!(
            app.pending_provider_instance()
                .as_ref()
                .and_then(|p| p.base_url.as_deref()),
            Some("https://openrouter.ai/api/v1")
        );
    }

    #[test]
    fn submit_pending_provider_api_key_stores_token() {
        let mut app = make_app();
        app.pending_provider_setup = Some(super::PendingProviderSetup::new("test".to_string()));
        app.enter_provider_api_key_input_mode();
        app.textarea.insert_str("sk-test");

        let token = app
            .submit_pending_provider_api_key()
            .expect("provider token");
        assert_eq!(token, "sk-test");
        assert_eq!(
            app.pending_provider_setup
                .as_ref()
                .and_then(|p| p.api_key.as_deref()),
            Some("sk-test")
        );
    }

    #[test]
    fn provider_selection_mode_reports_selected_provider_id() {
        let mut app = make_app();
        app.enter_provider_selection_mode(&[
            ProviderInstance::new("copilot", BackendPreset::Copilot),
            ProviderInstance::new("gpu-box", BackendPreset::Ollama),
        ]);
        app.selection.selected = app
            .selection
            .items
            .iter()
            .position(|item| item.complete_to == "/provider gpu-box")
            .expect("provider item present");

        assert!(app.in_provider_selection_mode());
        assert_eq!(app.selected_provider_id(), Some("gpu-box"));
    }

    #[test]
    fn enter_ollama_endpoint_freeform_mode_prefills_default_for_new_provider() {
        let mut app = make_app();
        app.begin_new_provider_setup();
        app.set_pending_provider_backend_preset(BackendPreset::Ollama);

        app.enter_ollama_endpoint_freeform_mode();

        assert_eq!(
            app.textarea.lines().join(""),
            super::DEFAULT_OLLAMA_ENDPOINT
        );
    }

    #[test]
    fn enter_provider_edit_mode_prefills_existing_base_url() {
        let mut app = make_app();
        let mut instance = ProviderInstance::new("gpu-box", BackendPreset::Ollama);
        instance.base_url = Some("http://gpu-box:11434".to_string());

        app.enter_provider_edit_mode(&instance);

        assert!(app.pending_provider_setup_is_edit());
        assert_eq!(app.textarea.lines().join(""), "http://gpu-box:11434");
    }

    #[test]
    fn submit_pending_provider_api_key_keeps_existing_value_when_editing() {
        let mut app = make_app();
        let mut instance = ProviderInstance::new("work-webui", BackendPreset::OpenWebUi);
        instance.api_key = Some("sk-existing".to_string());
        app.pending_provider_setup = Some(super::PendingProviderSetup::from_instance(&instance));
        app.enter_provider_api_key_input_mode();

        let token = app
            .submit_pending_provider_api_key()
            .expect("provider token");
        assert_eq!(token, "sk-existing");
        assert_eq!(
            app.pending_provider_setup
                .as_ref()
                .and_then(|p| p.api_key.as_deref()),
            Some("sk-existing")
        );
    }

    #[test]
    fn submit_provider_name_input_slugifies_and_rejects_duplicates() {
        let mut app = make_app();
        let providers = vec![crate::provider_instance::ProviderInstance::new(
            "work-webui",
            BackendPreset::OpenWebUi,
        )];

        app.begin_new_provider_setup();
        app.set_pending_provider_backend_preset(BackendPreset::OpenWebUi);
        if let Some(setup) = app.pending_provider_setup.as_mut() {
            setup.base_url = Some("https://work.example.com".to_string());
        }
        app.enter_provider_name_input_mode();
        assert_eq!(app.textarea.lines().join(""), "work.example.com-open-webui");
        app.textarea = App::make_textarea();
        app.textarea.insert_str("Work WebUI");
        assert!(app.submit_provider_name_input(&providers).is_none());

        app.enter_provider_name_input_mode();
        app.textarea = App::make_textarea();
        app.textarea.insert_str("GPU Box");
        let id = app
            .submit_provider_name_input(&providers)
            .expect("new provider id");
        assert_eq!(id, "gpu-box");
        assert_eq!(
            app.pending_provider_setup.as_ref().map(|p| p.id.as_str()),
            Some("gpu-box")
        );
    }

    #[test]
    fn pending_provider_instance_uses_suggested_id_when_name_not_confirmed_yet() {
        let mut app = make_app();
        app.begin_new_provider_setup();
        app.set_pending_provider_backend_preset(BackendPreset::Ollama);
        app.set_pending_provider_api_type(ApiType::AnthropicCompatible);
        if let Some(setup) = app.pending_provider_setup.as_mut() {
            setup.base_url = Some("http://mydomain.com:11434".to_string());
        }

        let instance = app
            .pending_provider_instance()
            .expect("pending provider instance");
        assert_eq!(instance.id, "ollama-mydomain.com");
        assert_eq!(instance.backend_preset, BackendPreset::Ollama);
        assert_eq!(instance.api_type, ApiType::AnthropicCompatible);
    }

    #[test]
    fn enter_provider_name_input_mode_prefills_existing_name_when_editing() {
        let mut app = make_app();
        let mut instance = ProviderInstance::new("gpu-box", BackendPreset::Ollama);
        instance.base_url = Some("http://gpu-box:11434".to_string());

        app.pending_provider_setup = Some(super::PendingProviderSetup::from_instance(&instance));
        app.enter_provider_name_input_mode();

        assert_eq!(app.textarea.lines().join(""), "gpu-box");
    }

    #[test]
    fn enter_provider_name_input_mode_prefills_ollama_name_from_endpoint() {
        let mut app = make_app();
        app.begin_new_provider_setup();
        app.set_pending_provider_backend_preset(BackendPreset::Ollama);
        if let Some(setup) = app.pending_provider_setup.as_mut() {
            setup.base_url = Some("http://localhost:11434".to_string());
        }

        app.enter_provider_name_input_mode();

        assert_eq!(app.textarea.lines().join(""), "ollama-localhost");
    }

    #[test]
    fn pending_provider_instance_uses_backend_based_placeholder_id_before_url_is_known() {
        let mut app = make_app();
        app.begin_new_provider_setup();
        app.set_pending_provider_backend_preset(BackendPreset::Ollama);
        app.set_pending_provider_api_type(ApiType::AnthropicCompatible);

        let instance = app
            .pending_provider_instance()
            .expect("pending provider instance");
        assert_eq!(instance.id, "ollama-ollama");
        assert_eq!(instance.backend_preset, BackendPreset::Ollama);
        assert_eq!(instance.api_type, ApiType::AnthropicCompatible);
    }

    #[test]
    fn pending_provider_instance_uses_selected_service_and_api() {
        let mut app = make_app();
        app.pending_provider_setup = Some(super::PendingProviderSetup::new("gpu-box".to_string()));
        app.set_pending_provider_backend_preset(BackendPreset::Ollama);
        app.set_pending_provider_api_type(ApiType::AnthropicCompatible);

        let instance = app
            .pending_provider_instance()
            .expect("pending provider instance");
        assert_eq!(instance.id, "gpu-box");
        assert_eq!(instance.backend_preset, BackendPreset::Ollama);
        assert_eq!(instance.api_type, ApiType::AnthropicCompatible);
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
            !app.selection.active,
            "selection mode should NOT be active for no-options"
        );
        assert!(
            app.ask_user_freeform_mode(),
            "freeform mode should be active"
        );
        assert_eq!(
            app.ask_user_question(),
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

        assert!(app.selection.active);
        assert_eq!(app.selection.items.len(), 3); // 2 options + freeform sentinel
        assert_eq!(app.selection.items[2].complete_to, "/ask_user_freeform");
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

        assert!(app.selection.active);
        assert_eq!(app.selection.items.len(), 2); // only the 2 options
        assert!(
            app.selection
                .items
                .iter()
                .all(|i| i.complete_to != "/ask_user_freeform")
        );
    }

    #[test]
    fn format_provider_error_uses_natural_english_and_message() {
        let err = ProviderError::server_error("OpenAI", 524, "error code: 524");
        let rendered = format_provider_error_for_display("Open WebUI", &err);
        assert_eq!(
            rendered,
            "Open WebUI timed out on the backend (524).\nProvider message: error code: 524"
        );
    }

    #[test]
    fn format_provider_error_handles_network_failures() {
        let err = ProviderError::network("Ollama", "connection refused");
        let rendered = format_provider_error_for_display("Open WebUI", &err);
        assert_eq!(
            rendered,
            "Could not reach Open WebUI.\nProvider message: connection refused"
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
            app.streaming_status = Some(StreamingStatus::Waiting);
            install_test_agent_task(&mut app);
            app.textarea.insert_str("/model gpt");

            app.handle_escape_in_chat_mode();

            assert!(
                app.streaming(),
                "streaming should remain active when ESC cancels slash input"
            );
            assert!(
                app.runtime.agent_task.is_some(),
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
                !app.live_turn
                    .notices
                    .iter()
                    .any(|m| m.content == "[agent loop aborted]"),
                "ESC slash cancel should not append an abort notice"
            );

            if let Some(handle) = app.runtime.agent_task.take() {
                handle.abort();
            }
        });
    }

    #[test]
    fn handle_escape_in_chat_mode_aborts_stream_when_not_in_slash_mode() {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        rt.block_on(async {
            let mut app = make_app();
            app.streaming_status = Some(StreamingStatus::Waiting);
            install_test_agent_task(&mut app);
            app.textarea.insert_str("hello");

            app.handle_escape_in_chat_mode();

            assert!(
                !app.streaming(),
                "streaming should stop when ESC is used outside slash mode"
            );
            assert!(
                app.runtime.agent_task.is_none(),
                "agent task should be removed when stream is aborted"
            );
            assert!(matches!(
                app.streaming_status,
                Some(StreamingStatus::CompletedMessage(ref s)) if s == "[agent loop aborted]"
            ));
        });
    }

    #[test]
    fn abort_agent_loop_appends_error_result_for_pending_tool_call() {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        rt.block_on(async {
            let tmp = tempfile::tempdir().expect("tempdir");
            let path = tmp.path().join("session.jsonl");

            let mut app = make_app();
            app.session_state = Some(crate::session_state::SessionState::from_event_log(
                crate::event_log::EventLog::load(&path).expect("load event log"),
            ));
            app.streaming_status = Some(StreamingStatus::Waiting);
            install_test_agent_task(&mut app);
            // Simulate an in-flight tool call via pending_turn_events.
            app.pending_turn_events
                .push(crate::session_event::SessionEvent::ToolCall {
                    id: "call_1".to_string(),
                    name: "powershell".to_string(),
                    args: serde_json::json!({"command": "git diff"}),
                    timestamp: 1,
                });
            app.live_turn
                .tool_entries
                .push(crate::live_turn::LiveToolEntry {
                    id: "call_1".to_string(),
                    name: "powershell".to_string(),
                    args: serde_json::json!({"command": "git diff"}),
                    result: None,
                });

            app.abort_agent_loop();

            let tool_result = app
                .session_state
                .as_ref()
                .expect("session state")
                .display_messages()
                .iter()
                .find(|m| m.role == Role::ToolResult && m.tool_call_id.as_deref() == Some("call_1"))
                .expect("expected abort tool result");
            assert!(tool_result.is_error, "abort tool result should be an error");
            assert_eq!(tool_result.content, "failure: aborted by user");
        });
    }

    #[test]
    fn abort_agent_loop_does_not_duplicate_existing_tool_result() {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        rt.block_on(async {
            let tmp = tempfile::tempdir().expect("tempdir");
            let path = tmp.path().join("session.jsonl");

            let mut app = make_app();
            app.session_state = Some(crate::session_state::SessionState::from_event_log(
                crate::event_log::EventLog::load(&path).expect("load event log"),
            ));
            app.streaming_status = Some(StreamingStatus::Waiting);
            install_test_agent_task(&mut app);
            // Simulate a ToolCall with its result already in pending_turn_events.
            app.pending_turn_events
                .push(crate::session_event::SessionEvent::ToolCall {
                    id: "call_1".to_string(),
                    name: "powershell".to_string(),
                    args: serde_json::json!({"command": "git diff"}),
                    timestamp: 1,
                });
            app.pending_turn_events
                .push(crate::session_event::SessionEvent::ToolResult {
                    id: "call_1".to_string(),
                    name: "powershell".to_string(),
                    content: "done".to_string(),
                    is_error: false,
                    display_range: None,
                    timestamp: 1,
                });

            app.abort_agent_loop();

            let matching_results = app
                .session_state
                .as_ref()
                .expect("session state")
                .display_messages()
                .iter()
                .filter(|m| {
                    m.role == Role::ToolResult && m.tool_call_id.as_deref() == Some("call_1")
                })
                .count();
            assert_eq!(
                matching_results, 1,
                "should not append abort result for already-completed tool call"
            );
        });
    }

    #[test]
    fn abort_agent_loop_commits_partial_turn_and_next_turn_clears_abort_status() {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        rt.block_on(async {
            let tmp = tempfile::tempdir().expect("tempdir");
            let path = tmp.path().join("session.jsonl");

            let mut app = make_app();
            app.session_state = Some(crate::session_state::SessionState::from_event_log(
                crate::event_log::EventLog::load(&path).expect("load event log"),
            ));
            app.live_turn.assistant_content = "partial".to_string();
            app.streaming_status = Some(StreamingStatus::Waiting);
            install_test_agent_task(&mut app);

            app.abort_agent_loop();

            let display = app
                .session_state
                .as_ref()
                .expect("session state")
                .display_messages();
            assert!(
                display
                    .iter()
                    .any(|m| m.role == Role::Assistant && m.content == "partial")
            );
            assert!(matches!(
                app.streaming_status,
                Some(StreamingStatus::CompletedMessage(ref s)) if s == "[agent loop aborted]"
            ));

            let provider: std::sync::Arc<dyn crate::llm::LlmProvider + Send + Sync> =
                std::sync::Arc::new(crate::llm::test_provider::TestProvider::new());
            app.launch_turn(&provider);

            assert!(matches!(
                app.streaming_status,
                Some(StreamingStatus::Waiting)
            ));
        });
    }

    #[test]
    fn submit_does_not_duplicate_user_message_in_display_projection() {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        rt.block_on(async {
            let tmp = tempfile::tempdir().expect("tempdir");
            let path = tmp.path().join("session.jsonl");

            let mut app = make_app();
            app.session_state = Some(crate::session_state::SessionState::from_event_log(
                crate::event_log::EventLog::load(&path).expect("load event log"),
            ));
            app.textarea.insert_str("hello");

            let provider: std::sync::Arc<dyn crate::llm::LlmProvider + Send + Sync> =
                std::sync::Arc::new(crate::llm::test_provider::TestProvider::new());

            app.submit(&provider);

            let combined = app.display_messages_combined();
            let matching = combined
                .iter()
                .filter(|m| m.role == Role::User && m.content == "hello")
                .count();
            assert_eq!(matching, 1, "user message should appear once in display");

            if let Some(handle) = app.runtime.agent_task.take() {
                handle.abort();
            }
        });
    }

    #[test]
    fn turn_end_rebuild_replaces_transient_output_without_duplication() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("session.jsonl");

        let mut app = make_app();
        app.session_state = Some(crate::session_state::SessionState::from_event_log(
            crate::event_log::EventLog::load(&path).expect("load event log"),
        ));

        app.append_event_immediate(crate::session_event::SessionEvent::UserMessage {
            content: "hello".to_string(),
            timestamp: 1,
        });

        app.apply_agent_event(crate::agent::types::AgentEvent::TextToken {
            text: "hi".to_string(),
            phase: crate::llm::AssistantPhase::Final,
        });
        app.apply_agent_event(crate::agent::types::AgentEvent::ToolCallStart {
            id: "call_1".to_string(),
            name: "read_file".to_string(),
            args: serde_json::json!({"path": "src/main.rs"}),
        });
        app.apply_agent_event(crate::agent::types::AgentEvent::ToolCallEnd {
            id: "call_1".to_string(),
            result: crate::agent::types::ToolResult {
                content: "ok".to_string(),
                is_error: false,
                is_truncated: false,
                truncation: None,
                raw_stdout: None,
                raw_stderr: None,
            },
        });
        app.apply_agent_event(crate::agent::types::AgentEvent::TurnEnd);

        let combined = app.display_messages_combined();
        let assistant_matching = combined
            .iter()
            .filter(|m| m.role == Role::Assistant && m.content == "hi")
            .count();
        assert_eq!(assistant_matching, 1, "assistant output should appear once");

        let tool_call_matching = combined
            .iter()
            .filter(|m| m.role == Role::ToolCall && m.tool_call_id.as_deref() == Some("call_1"))
            .count();
        assert_eq!(tool_call_matching, 1, "tool call should appear once");

        let tool_result_matching = combined
            .iter()
            .filter(|m| m.role == Role::ToolResult && m.tool_call_id.as_deref() == Some("call_1"))
            .count();
        assert_eq!(tool_result_matching, 1, "tool result should appear once");
    }

    #[test]
    fn done_does_not_duplicate_assistant_turn_after_turn_end() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("session.jsonl");

        let mut app = make_app();
        app.session_state = Some(crate::session_state::SessionState::from_event_log(
            crate::event_log::EventLog::load(&path).expect("load event log"),
        ));

        // Seed a user message (normally appended on submit).
        app.append_event_immediate(crate::session_event::SessionEvent::UserMessage {
            content: "hello".to_string(),
            timestamp: 1,
        });

        // Simulate a simple assistant turn with no tools.
        app.apply_agent_event(crate::agent::types::AgentEvent::TextToken {
            text: "hi".to_string(),
            phase: crate::llm::AssistantPhase::Final,
        });
        app.apply_agent_event(crate::agent::types::AgentEvent::TurnEnd);
        app.apply_agent_event(crate::agent::types::AgentEvent::Done);

        let log_events = app
            .session_state
            .as_ref()
            .expect("session state")
            .events()
            .to_vec();
        let assistant_count = log_events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    crate::session_event::SessionEvent::AssistantMessage { .. }
                )
            })
            .count();
        assert_eq!(assistant_count, 1, "assistant turn should be written once");
    }

    // ── prepare_llm_messages ─────────────────────────────────────────────────

    #[test]
    fn prepare_llm_messages_prepends_system_prompt() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("session.jsonl");
        let mut app = make_app();
        app.system_prompt = Some("be helpful".to_string());
        app.session_state = Some(crate::session_state::SessionState::from_event_log(
            crate::event_log::EventLog::load(&path).expect("load event log"),
        ));
        app.append_event_immediate(crate::session_event::SessionEvent::UserMessage {
            content: "hello".to_string(),
            timestamp: 1,
        });
        let msgs = app.prepare_llm_messages();
        assert_eq!(msgs[0].role, Role::System);
        assert_eq!(msgs[0].content, "be helpful");
        assert_eq!(msgs[1].role, Role::User);
    }

    #[test]
    fn prepare_llm_messages_filters_include_in_llm_false() {
        // TurnError events are not included in LLM messages.
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("session.jsonl");
        let mut app = make_app();
        app.session_state = Some(crate::session_state::SessionState::from_event_log(
            crate::event_log::EventLog::load(&path).expect("load event log"),
        ));
        app.append_event_immediate(crate::session_event::SessionEvent::TurnError {
            message: "[Error: rate limit]".to_string(),
            timestamp: 1,
        });
        app.append_event_immediate(crate::session_event::SessionEvent::UserMessage {
            content: "visible".to_string(),
            timestamp: 2,
        });
        let msgs = app.prepare_llm_messages();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "visible");
    }

    #[test]
    fn prepare_llm_messages_pops_trailing_empty_assistant() {
        use crate::llm::AssistantPhase;
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("session.jsonl");
        let mut app = make_app();
        app.session_state = Some(crate::session_state::SessionState::from_event_log(
            crate::event_log::EventLog::load(&path).expect("load event log"),
        ));
        app.append_event_immediate(crate::session_event::SessionEvent::UserMessage {
            content: "hello".to_string(),
            timestamp: 1,
        });
        app.append_event_immediate(crate::session_event::SessionEvent::AssistantMessage {
            content: String::new(),
            thinking: None,
            phase: AssistantPhase::Provisional,
            usage: None,
            timestamp: 2,
        });
        let msgs = app.prepare_llm_messages();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, Role::User);
    }

    #[test]
    fn prepare_llm_messages_excludes_live_turn_assistant_and_tools() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("session.jsonl");
        let mut app = make_app();
        app.session_state = Some(crate::session_state::SessionState::from_event_log(
            crate::event_log::EventLog::load(&path).expect("load event log"),
        ));
        app.append_event_immediate(crate::session_event::SessionEvent::UserMessage {
            content: "committed".to_string(),
            timestamp: 1,
        });

        // Live turn state should not reach the LLM.
        app.live_turn.assistant_content = "live assistant".to_string();
        app.live_turn
            .tool_entries
            .push(crate::live_turn::LiveToolEntry {
                id: "c1".to_string(),
                name: "read_file".to_string(),
                args: serde_json::json!({"path": "src/main.rs"}),
                result: Some(crate::live_turn::LiveToolResult {
                    content: "tool output".to_string(),
                    is_error: false,
                    display_range: None,
                }),
            });
        app.live_turn.notices.push(Message::assistant("[notice]"));

        let msgs = app.prepare_llm_messages();
        assert_eq!(
            msgs.len(),
            1,
            "only committed user message should be present"
        );
        assert_eq!(msgs[0].content, "committed");
    }

    #[test]
    #[should_panic(expected = "prepare_llm_messages called before session_state was initialised")]
    fn prepare_llm_messages_panics_without_session_state() {
        let mut app = make_app();
        let _ = app.prepare_llm_messages();
    }

    // ── Step 6: resume/export/integration paths ─────────────────────────────

    #[test]
    fn submit_initialises_session_state_even_without_session_store() {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        rt.block_on(async {
            let mut app = make_app();
            app.session_store = None; // persistence unavailable
            app.textarea.insert_str("hello");

            let provider: std::sync::Arc<dyn crate::llm::LlmProvider + Send + Sync> =
                std::sync::Arc::new(crate::llm::test_provider::TestProvider::new());

            app.submit(&provider);

            assert!(
                app.session_state.is_some(),
                "submit should always initialise session_state before launching a turn"
            );

            if let Some(handle) = app.runtime.agent_task.take() {
                handle.abort();
            }
        });
    }

    #[test]
    fn resume_clears_live_turn_overlay_and_notices() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cwd = tmp.path().to_string_lossy().to_string();
        let mut store = crate::session::SessionStore::open_at(tmp.path().join("sessions"))
            .expect("open session store");
        let session_id = store.create_session(&cwd).expect("create session");
        let mut log = store.load_events(&session_id).expect("load events");
        log.append_batch(&[crate::session_event::SessionEvent::UserMessage {
            content: "hello".to_string(),
            timestamp: 1,
        }])
        .expect("append event");

        let mut app = make_app();
        app.session_store = Some(store);
        app.live_turn.assistant_content = "streaming".to_string();
        app.live_turn.notices.push(Message::assistant("[notice]"));

        app.resume_session_by_id(&session_id);

        assert!(app.live_turn.assistant_content.is_empty());
        assert!(app.live_turn.tool_entries.is_empty());
        assert!(app.live_turn.notices.is_empty());
        let combined = app.display_messages_combined();
        assert_eq!(combined.len(), 1);
        assert_eq!(combined[0].content, "hello");
    }

    #[test]
    fn export_uses_committed_state_not_live_overlay() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("session.jsonl");
        let export_path = tmp.path().join("export.html");

        let mut app = make_app();
        app.current_cwd = tmp.path().to_string_lossy().to_string();
        app.session_state = Some(crate::session_state::SessionState::from_event_log(
            crate::event_log::EventLog::load(&path).expect("load event log"),
        ));
        app.append_event_immediate(crate::session_event::SessionEvent::UserMessage {
            content: "committed".to_string(),
            timestamp: 1,
        });
        app.live_turn.assistant_content = "live assistant".to_string();
        app.live_turn.notices.push(Message::assistant("[notice]"));

        app.export_session_html(Some(export_path.to_str().expect("utf8 path")));

        let html = std::fs::read_to_string(&export_path).expect("read export html");
        assert!(html.contains("committed"));
        assert!(!html.contains("live assistant"));
        assert!(!html.contains("[notice]"));
    }

    #[test]
    fn provider_error_clears_live_turn_and_commits_turn_error_event() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("session.jsonl");
        let mut app = make_app();
        app.session_state = Some(crate::session_state::SessionState::from_event_log(
            crate::event_log::EventLog::load(&path).expect("load event log"),
        ));
        app.streaming_status = Some(StreamingStatus::Waiting);
        app.live_turn.assistant_content = "partial".to_string();
        app.pending_turn_events
            .push(crate::session_event::SessionEvent::ToolCall {
                id: "c1".to_string(),
                name: "read_file".to_string(),
                args: serde_json::json!({"path": "src/main.rs"}),
                timestamp: 1,
            });

        app.apply_agent_event(crate::agent::types::AgentEvent::Error(ProviderError {
            message: "boom".to_string(),
            kind: crate::llm::ProviderErrorKind::Other,
            status_code: None,
            source: "test".to_string(),
        }));

        assert!(app.live_turn.assistant_content.is_empty());
        assert!(app.pending_turn_events.is_empty());

        let events = app.session_state.as_ref().expect("session state").events();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, crate::session_event::SessionEvent::TurnError { .. })),
            "TurnError should be committed"
        );
    }

    #[test]
    fn empty_status_update_keeps_throbber_visible_while_waiting() {
        let mut app = make_app();
        app.streaming_status = Some(StreamingStatus::Waiting);
        app.last_output_at = None;

        assert!(app.throbber_visible());

        app.apply_agent_event(crate::agent::types::AgentEvent::StatusUpdate(String::new()));

        assert!(app.throbber_visible());
    }

    #[test]
    fn non_empty_status_update_temporarily_hides_throbber() {
        let mut app = make_app();
        app.streaming_status = Some(StreamingStatus::Waiting);
        app.last_output_at = None;

        assert!(app.throbber_visible());

        app.apply_agent_event(crate::agent::types::AgentEvent::StatusUpdate(
            "retrying in 1s…".to_string(),
        ));

        assert!(!app.throbber_visible());
    }

    #[test]
    fn whitespace_text_token_keeps_throbber_visible_while_waiting() {
        let mut app = make_app();
        app.streaming_status = Some(StreamingStatus::Waiting);
        app.last_output_at = None;

        app.apply_agent_event(crate::agent::types::AgentEvent::TextToken {
            text: "   \n".to_string(),
            phase: crate::llm::AssistantPhase::Unknown,
        });

        assert!(app.throbber_visible());
    }

    #[test]
    fn whitespace_thinking_token_keeps_throbber_visible_while_waiting() {
        let mut app = make_app();
        app.streaming_status = Some(StreamingStatus::Waiting);
        app.last_output_at = None;

        app.apply_agent_event(crate::agent::types::AgentEvent::ThinkingToken(
            "\n\n".to_string(),
        ));

        assert!(app.throbber_visible());
    }

    #[test]
    fn provider_status_visibility_follows_status_messages() {
        let mut app = make_app();
        assert!(!app.provider_status_visible());

        app.streaming_status = Some(StreamingStatus::Message("compacting…".to_string()));
        assert!(app.provider_status_visible());

        app.streaming_status = None;
        assert!(!app.provider_status_visible());
    }

    #[test]
    fn notices_survive_turn_boundary_but_are_not_committed_or_sent_to_llm() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("session.jsonl");
        let mut app = make_app();
        app.session_state = Some(crate::session_state::SessionState::from_event_log(
            crate::event_log::EventLog::load(&path).expect("load event log"),
        ));

        app.live_turn.notices.push(Message::assistant("[notice]"));
        app.live_turn.assistant_content = "hi".to_string();
        app.apply_agent_event(crate::agent::types::AgentEvent::TurnEnd);

        assert_eq!(
            app.live_turn.notices.len(),
            1,
            "notice should survive turn boundary"
        );
        assert_eq!(app.live_turn.notices[0].content, "[notice]");
        assert!(
            app.live_turn.assistant_content.is_empty(),
            "turn content should clear"
        );

        let events = app.session_state.as_ref().expect("session state").events();
        assert!(
            !events.iter().any(|e| matches!(
                e,
                crate::session_event::SessionEvent::AssistantMessage { content, .. } if content == "[notice]"
            )),
            "notice must not be committed as a session event"
        );

        let llm = app.prepare_llm_messages();
        assert!(
            !llm.iter().any(|m| m.content == "[notice]"),
            "notice must not appear in LLM input"
        );
    }

    #[test]
    fn shell_output_is_ui_only_and_excluded_from_event_log_and_llm() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("session.jsonl");
        let mut app = make_app();
        app.current_cwd = tmp.path().to_string_lossy().to_string();
        app.session_state = Some(crate::session_state::SessionState::from_event_log(
            crate::event_log::EventLog::load(&path).expect("load event log"),
        ));
        app.shell_textarea.insert_str("printf 'hello'");

        app.submit_shell_command();

        assert!(
            app.live_turn
                .notices
                .iter()
                .any(|m| m.role == Role::ToolCall && m.tool_call_id.as_deref().is_some()),
            "shell tool call should appear in UI notices"
        );
        assert!(
            app.live_turn
                .notices
                .iter()
                .any(|m| m.role == Role::ToolResult && m.content.contains("hello")),
            "shell tool result should appear in UI notices"
        );

        let events = app.session_state.as_ref().expect("session state").events();
        assert!(
            events.is_empty(),
            "shell output must not enter the event log"
        );

        let llm = app.prepare_llm_messages();
        assert!(llm.is_empty(), "shell output must not enter LLM history");
    }

    #[test]
    fn finalise_assistant_turn_event_uses_live_turn_state_fields() {
        let mut app = make_app();
        app.live_turn.assistant_content = "answer".to_string();
        app.live_turn.assistant_thinking = Some("thinking".to_string());
        app.live_turn.assistant_phase = crate::llm::AssistantPhase::Provisional;
        app.latest_usage = Some(crate::llm::UsageStats {
            input_tokens: Some(1),
            output_tokens: Some(2),
            total_tokens: Some(3),
        });

        app.finalise_assistant_turn_event();

        let ev = app
            .pending_turn_events
            .iter()
            .find_map(|e| match e {
                crate::session_event::SessionEvent::AssistantMessage {
                    content,
                    thinking,
                    phase,
                    usage,
                    ..
                } => Some((content, thinking, phase, usage)),
                _ => None,
            })
            .expect("assistant event should be present");

        assert_eq!(ev.0, "answer");
        assert_eq!(ev.1.as_deref(), Some("thinking"));
        assert_eq!(*ev.2, crate::llm::AssistantPhase::Provisional);
        assert_eq!(
            *ev.3,
            Some(crate::llm::UsageStats {
                input_tokens: Some(1),
                output_tokens: Some(2),
                total_tokens: Some(3),
            })
        );
    }

    #[test]
    fn live_overlay_does_not_mutate_committed_history() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("session.jsonl");
        let mut app = make_app();
        app.session_state = Some(crate::session_state::SessionState::from_event_log(
            crate::event_log::EventLog::load(&path).expect("load event log"),
        ));
        app.append_event_immediate(crate::session_event::SessionEvent::UserMessage {
            content: "committed".to_string(),
            timestamp: 1,
        });

        let committed_before = app
            .session_state
            .as_ref()
            .expect("session state")
            .display_messages()
            .to_vec();

        app.live_turn.assistant_content = "live".to_string();
        app.live_turn.notices.push(Message::assistant("[notice]"));

        let committed_after = app
            .session_state
            .as_ref()
            .expect("session state")
            .display_messages()
            .to_vec();
        let before_contents: Vec<_> = committed_before.iter().map(|m| m.content.clone()).collect();
        let after_contents: Vec<_> = committed_after.iter().map(|m| m.content.clone()).collect();
        assert_eq!(
            before_contents, after_contents,
            "live overlay must not mutate committed history"
        );

        let combined = app.display_messages_combined();
        assert!(
            combined.len() > committed_after.len(),
            "combined view should include live overlay"
        );
    }
}
