use ratatui_textarea::{CursorMove, TextArea};
use std::sync::Arc;

use crate::{
    agent::AgentLoopConfig,
    app_event::{AppEvent, AppEventTx},
    auth::{self},
    completion::{self, CompletionItem},
    config::DisplayConfig,
    keybindings::{BindingContext, KEYBINDINGS},
    live_turn::compose_display,
    llm::{LlmProvider, Message, UsageStats},
    provider_instance::{ApiType, ProviderInstance},
    session::SessionStore,
    session_state::SessionState,
    skills::SkillMeta,
    theme::Theme,
    thinking::ThinkingLevel,
};

use crate::agent_runtime::AgentRuntime;
use crate::agent_turn_state::AgentTurnState;
use crate::ask_user_state::AskUserState;
use crate::completion_state::CompletionState;
use crate::log_view_state::LogViewState;
use crate::login_state::{LoginActionKind, LoginState};
use crate::mouse_select::MouseSelectState;
use crate::provider_manager::{
    ProviderManager, ProviderSetupStep, active_provider_display_name,
    format_provider_error_for_display,
};
use crate::selection_state::{SelectionKind, SelectionState};
use crate::session_event::SessionEvent;
use crate::session_manager::SessionManager;
use crate::shell::ShellKind;
use crate::shell_state::ShellState;
use crate::step_back_state::StepBackState;
use crate::tracked::Tracked;

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
    /// The user cancelled a pending provider removal confirmation.
    CancelProviderRemoval,
    /// The user confirmed removing a custom provider instance.
    RemoveProvider(String),
    /// A provider API type was chosen during add-provider setup.
    ProviderApiType(ApiType),
    /// The user selected an agent name from the picker.
    Agent(String),
}

/// Target operation to retry after token refresh completes.
#[derive(Debug, Clone, Copy)]
pub(crate) enum RetryTarget {
    /// Retry the last agent turn (chat request).
    AgentTurn,
    /// Retry the model list fetch.
    ModelFetch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputMode {
    Chat,
    Shell,
}

// ── Login state ───────────────────────────────────────────────────────────────
// ── Log cache ─────────────────────────────────────────────────────────────────

// ── App state ─────────────────────────────────────────────────────────────────

pub struct App {
    pub(crate) textarea: TextArea<'static>,
    /// Shell mode state (textarea, selected shell, available shells).
    /// Shell mode state (textarea, selected shell, available shells).
    pub(crate) shell: ShellState,
    pub(crate) input_mode: InputMode,
    /// Vertical scroll offset for the input panel (in wrapped lines).
    pub(crate) input_scroll: usize,
    /// Log pane scroll and cache state.
    pub(crate) log_view: LogViewState,
    /// Active agent turn state: streaming status, throbber tick, last output time.
    pub(crate) agent_turn: AgentTurnState,
    /// All provider-related state: instances, active instance/model/thinking,
    /// and transient setup-flow state.
    pub(crate) provider: ProviderManager,
    /// Agent loop configuration (tools, hooks).
    pub(crate) agent_config: AgentLoopConfig,
    /// Skills loaded from all supported skill roots.
    pub(crate) loaded_skills: Vec<SkillMeta>,
    /// User-definable agent profiles loaded from filesystem.
    pub(crate) agents: Vec<crate::agents::AgentMeta>,
    /// Name of the currently active agent, or `None` for the default.
    pub(crate) active_agent: Option<String>,

    // ── Completion popup + model fetch ────────────────────────────────────────
    pub(crate) completion: CompletionState,

    // ── Generic selection menu ────────────────────────────────────────────────
    pub(crate) selection: SelectionState,

    // ── Info bar ──────────────────────────────────────────────────────────────
    /// When true, the info bar (provider / model / context window) is shown
    /// below the input panel.  Toggled by Ctrl+I.
    pub(crate) show_info: bool,
    /// Best-effort token usage reported for the latest completed turn.
    pub(crate) latest_usage: Option<UsageStats>,
    /// Set when the previous turn should have populated the prompt cache but
    /// the current response shows zero cached tokens.  Cleared when the user
    /// submits a new message.
    pub(crate) cache_miss_warning: bool,

    // ── Login panel ───────────────────────────────────────────────────────────
    pub(crate) login: LoginState,

    // ── Session persistence + state ───────────────────────────────────────────
    /// All session-related state: persistence store, committed state, live
    /// turn overlay, and pending event buffer.
    pub(crate) session: Tracked<SessionManager>,

    // ── Ask-user interaction state ──────────────────────────────────────────
    pub(crate) ask_user: AskUserState,

    // ── Runtime/task state ───────────────────────────────────────────────────
    pub(crate) runtime: AgentRuntime,

    // ── Step-back state ──────────────────────────────────────────────────────
    pub(crate) step_back: StepBackState,

    // ── Theme ─────────────────────────────────────────────────────────────────
    pub(crate) theme: Theme,
    // ── Display thresholds ────────────────────────────────────────────────────
    pub(crate) display: DisplayConfig,

    // ── Mouse selection ───────────────────────────────────────────────────────
    pub(crate) mouse_select: MouseSelectState,
}

// Convenience alias used throughout this module.
pub(crate) type DynProvider = Arc<dyn LlmProvider + Send + Sync + 'static>;

impl App {
    pub fn new(
        initial_instance: ProviderInstance,
        initial_model: impl Into<String>,
        initial_thinking: ThinkingLevel,
        agent_config: AgentLoopConfig,
        display: DisplayConfig,
    ) -> Self {
        let initial_model = initial_model.into();
        Self {
            textarea: Self::make_textarea(),
            shell: ShellState::new(),
            input_mode: InputMode::Chat,
            input_scroll: 0,
            log_view: LogViewState::new(),
            agent_turn: AgentTurnState::new(),
            provider: ProviderManager::new(initial_instance, initial_model, initial_thinking),
            agent_config,
            loaded_skills: Vec::new(),
            agents: Vec::new(),
            active_agent: None,
            completion: CompletionState::new(),
            selection: SelectionState::new(),
            show_info: false,
            latest_usage: None,
            cache_miss_warning: false,
            login: LoginState::new(),
            session: Tracked::new(SessionManager::new()),
            ask_user: AskUserState::new(),
            runtime: AgentRuntime::new(),
            step_back: StepBackState::default(),
            theme: Theme::default(),
            display,
            mouse_select: MouseSelectState::new(),
        }
    }

    /// Returns true when an agent turn is active (streaming or waiting for first token).
    pub fn streaming(&self) -> bool {
        self.agent_turn.is_active()
    }

    /// Advance the throbber animation frame.  Called on every UI tick.
    pub fn tick(&mut self) {
        if self.login.refresh_in_progress {
            self.agent_turn.tick = self.agent_turn.tick.wrapping_add(1);
        } else {
            self.agent_turn.advance_tick();
        }
    }

    /// Record a model/provider change in the event log.
    ///
    /// Call this whenever `current_model` or `current_provider` is updated so
    /// that the change is preserved in the session history.
    pub fn record_model_changed(&mut self) {
        self.append_event_immediate(SessionEvent::ModelChanged {
            model: self.provider.current_model.clone(),
            provider: self.provider.current_instance.id.clone(),
            timestamp: Self::now_ts(),
        });
    }

    /// Record a thinking-level change in the event log.
    ///
    /// Call this whenever `current_thinking` is updated.
    pub fn record_thinking_level_changed(&mut self) {
        self.append_event_immediate(SessionEvent::ThinkingLevelChanged {
            level: self.provider.current_thinking,
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
    /// - Token refresh in progress: throbber visible regardless of turn state,
    ///   so the user sees activity during the ~500ms refresh window.
    pub fn throbber_visible(&self) -> bool {
        self.login.refresh_in_progress
            || self
                .agent_turn
                .throbber_visible(self.has_pending_ask() || self.ask_user_freeform_mode())
    }

    /// Returns true when provider/system status text should be visible.
    pub fn provider_status_visible(&self) -> bool {
        if self.login.active {
            return false;
        }
        matches!(
            self.agent_turn.status,
            Some(StreamingStatus::Message(_) | StreamingStatus::CompletedMessage(_))
        )
    }

    pub fn ask_user_freeform_mode(&self) -> bool {
        self.ask_user.freeform_mode
    }

    pub fn queued_steering(&self) -> &[String] {
        self.runtime.queued_steering()
    }

    /// Toggle the info bar visibility.
    pub fn toggle_info(&mut self) {
        self.show_info = !self.show_info;
    }

    // ── Agent switching ───────────────────────────────────────────────────────

    /// Switch to the named agent, rebuilding tools, skills, and system prompt.
    /// Passing an empty string clears the active agent and restores defaults.
    /// Persists the choice to config.toml.
    pub fn switch_agent(&mut self, name: &str, cwd: &str) {
        if name.is_empty() {
            self.active_agent = None;
        } else if self.agents.iter().any(|a| a.name == name) {
            self.active_agent = Some(name.to_string());
        } else {
            return; // unknown agent name — ignore
        }
        self.rebuild_agent_system_prompt(cwd);

        // Persist to config
        if let Ok(mut config) = crate::config::XiConfig::load() {
            config.agent = self.active_agent.clone();
            let _ = config.save();
        }
    }

    /// Rebuild `agent_config.system_prompt` from the currently active agent,
    /// or the default when none is active.  Skills and tools in the prompt
    /// are filtered according to the active agent's include/exclude rules.
    pub(crate) fn rebuild_agent_system_prompt(&mut self, cwd: &str) {
        let agent = self.resolve_current_agent();
        let skills: Vec<crate::skills::SkillMeta> = if let Some(a) = agent {
            crate::agents::filter_skills(&self.loaded_skills, &a.include_skills, &a.exclude_skills)
        } else {
            self.loaded_skills.clone()
        };
        let system_prompt =
            crate::agent::build_system_prompt(&self.agent_config.tools, cwd, &skills, agent);
        self.agent_config.system_prompt = Some(system_prompt);
    }

    /// Return a reference to the currently active agent.
    ///
    /// Falls back to the "default" agent (if present in the agents list) when
    /// no agent has been explicitly selected.
    pub fn resolve_current_agent(&self) -> Option<&crate::agents::AgentMeta> {
        if let Some(name) = self.active_agent.as_deref() {
            self.agents.iter().find(|a| a.name == name)
        } else {
            self.agents.iter().find(|a| a.name == "default")
        }
    }

    /// Return the list of primary agents for the picker.
    pub fn primary_agents(&self) -> Vec<&crate::agents::AgentMeta> {
        self.agents
            .iter()
            .filter(|a| matches!(a.mode, crate::agents::AgentMode::Primary))
            .collect()
    }

    pub(crate) async fn recv_app_event(&mut self) -> Option<AppEvent> {
        self.runtime.recv_app_event().await
    }

    pub fn app_event_tx(&self) -> AppEventTx {
        self.runtime.app_event_tx()
    }

    pub fn init_session_persistence(&mut self, cwd: String) {
        self.session.current_cwd = cwd;
        match SessionStore::open() {
            Ok(store) => {
                self.session.session_store = Some(store);
                self.refresh_resume_availability();
            }
            Err(e) => {
                log::debug!("session persistence disabled: {}", e);
                self.session
                    .live_turn
                    .notices
                    .push(Message::assistant(format!(
                        "[session persistence unavailable: {e}]"
                    )));
            }
        }
    }

    /// Return all messages to display in the chat log: committed session
    /// messages followed by the live turn overlay (streaming assistant,
    /// in-flight tools, and UI-only notices).
    pub fn display_messages_combined(&self) -> Vec<Message> {
        let committed = self
            .session
            .session_state
            .as_ref()
            .map(|s| s.display_messages())
            .unwrap_or(&[]);
        compose_display(committed, &self.session.live_turn, self.streaming())
    }

    /// When in step-back mode, returns `(kept_messages, discarded_messages)`.
    /// `kept_messages` covers events before the step cursor (rendered normally).
    /// `discarded_messages` covers events from the step cursor onward (rendered dimmed).
    /// Returns `None` when not stepping.
    pub fn display_messages_split(&self) -> Option<(Vec<Message>, Vec<Message>)> {
        let idx = self.step_back.cursor?;
        let ss = self.session.session_state.as_ref()?;
        let events = ss.events();
        let kept = crate::projection::project_display_messages(&events[..idx]);
        let discarded = crate::projection::project_display_messages(&events[idx..]);
        Some((kept, discarded))
    }

    /// Push a transient UI-only notice (not backed by a `SessionEvent`).
    pub fn push_notice(&mut self, msg: Message) {
        self.session.live_turn.notices.push(msg);
    }

    /// Whether there are no committed display messages and no live overlay.
    pub fn display_is_empty(&self) -> bool {
        self.session
            .session_state
            .as_ref()
            .map(|s| s.display_is_empty())
            .unwrap_or(true)
            && self.session.live_turn.notices.is_empty()
            && !self.session.live_turn.has_assistant_content()
            && !self.session.live_turn.has_tool_entries()
    }

    /// Number of displayed messages (committed + live overlay).
    pub fn display_len(&self) -> usize {
        let committed = self
            .session
            .session_state
            .as_ref()
            .map(|s| s.display_len())
            .unwrap_or(0);
        // Use streaming=false for counting purposes (we don't want the
        // waiting-cursor empty slot to affect the count used for shell IDs).
        committed + self.session.live_turn.render_overlay(false).len()
    }

    pub fn should_show_resume_hint(&self) -> bool {
        self.session.resume_available_for_cwd
            && self.display_is_empty()
            && !self.selection.active
            && !self.login.active
            && !self.streaming()
    }

    pub fn resume_latest_for_current_cwd(&mut self) {
        let Some(store) = self.session.session_store.as_ref() else {
            return;
        };
        let Some(meta) = store.latest_for_cwd(&self.session.current_cwd) else {
            self.session.live_turn.notices.push(Message::assistant(
                "[no resumable session in this working folder]",
            ));
            return;
        };
        self.resume_session_by_id(&meta.id);
    }

    pub fn resume_session_by_id(&mut self, session_id: &str) {
        let Some(store) = self.session.session_store.as_ref() else {
            return;
        };
        match store.load_events(session_id) {
            Ok(log) => {
                // Capture last known token usage from the loaded events
                // before moving the event log into session_state.
                self.latest_usage = Self::find_last_usage_from_events(&log.events);
                self.session.session_state = Some(SessionState::from_event_log(log));
                self.session.live_turn.clear_all();
                self.session.current_session_id = Some(session_id.to_string());
                self.log_view.auto_scroll = true;
                self.log_view.log_scroll = 0;
            }
            Err(e) => {
                self.session
                    .live_turn
                    .notices
                    .push(Message::assistant(format!(
                        "[failed to resume session: {e}]"
                    )));
            }
        }
        self.refresh_resume_availability();
    }

    /// Scan session events for the last known token usage data.
    ///
    /// Checks the most recent [`AssistantMessage`] that has `usage` data,
    /// then falls back to the most recent [`CompactionSummary`]'s
    /// `tokens_after` so that the info bar can show a meaningful context
    /// utilisation value immediately on session resume.
    ///
    /// [`AssistantMessage`]: SessionEvent::AssistantMessage
    /// [`CompactionSummary`]: SessionEvent::CompactionSummary
    fn find_last_usage_from_events(events: &[SessionEvent]) -> Option<UsageStats> {
        for ev in events.iter().rev() {
            match ev {
                SessionEvent::AssistantMessage { usage: Some(u), .. } => return Some(*u),
                SessionEvent::CompactionSummary { tokens_after, .. } => {
                    return Some(UsageStats {
                        input_tokens: Some(*tokens_after),
                        output_tokens: None,
                        total_tokens: Some(*tokens_after),
                        cached_tokens: None,
                    });
                }
                _ => {}
            }
        }
        None
    }

    pub fn enter_resume_selection_mode(&mut self) {
        self.reset_textarea();
        self.session.live_turn.notices.clear();

        let items = if let Some(store) = self.session.session_store.as_ref() {
            let current_folder = std::path::Path::new(&self.session.current_cwd)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("");

            let mut sessions = store.list_sessions();
            sessions.sort_by(|a, b| {
                let a_scope = session_scope(&a.cwd, &self.session.current_cwd, current_folder);
                let b_scope = session_scope(&b.cwd, &self.session.current_cwd, current_folder);
                a_scope
                    .cmp(&b_scope)
                    .then_with(|| b.updated_at_ms.cmp(&a.updated_at_ms))
            });

            if sessions.is_empty() {
                vec![CompletionItem {
                    label: "no saved sessions yet".to_string(),
                    detail: String::new(),
                    complete_to: String::new(),
                    loading: true,
                    error: false,
                    match_range: None,
                }]
            } else {
                sessions
                    .iter()
                    .map(|meta| {
                        let scope = session_scope_label(
                            &meta.cwd,
                            &self.session.current_cwd,
                            current_folder,
                        );
                        let when = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(
                            meta.updated_at_ms,
                        )
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
                    .collect()
            }
        } else {
            vec![CompletionItem {
                label: "session persistence unavailable".to_string(),
                detail: String::new(),
                complete_to: String::new(),
                loading: true,
                error: false,
                match_range: None,
            }]
        };
        self.selection
            .activate(SelectionKind::ResumeSession, "  Resume session  ", items);
    }

    pub fn enter_keybinding_help_mode(&mut self) {
        self.session.live_turn.notices.clear();

        let contexts = [
            BindingContext::Global,
            BindingContext::Chat,
            BindingContext::Selection,
            BindingContext::Shell,
            BindingContext::ProviderPicker,
            BindingContext::Mouse,
        ];

        let mut items = Vec::new();
        for context in contexts {
            let bindings: Vec<_> = KEYBINDINGS
                .iter()
                .filter(|binding| binding.context == context)
                .collect();
            if bindings.is_empty() {
                continue;
            }

            items.push(CompletionItem {
                label: context.label().to_string(),
                detail: String::new(),
                complete_to: String::new(),
                loading: true,
                error: false,
                match_range: None,
            });

            items.extend(bindings.into_iter().map(|binding| CompletionItem {
                label: binding.shortcut.to_string(),
                detail: binding.description.to_string(),
                complete_to: String::new(),
                loading: false,
                error: false,
                match_range: None,
            }));
        }

        self.selection.activate(
            SelectionKind::KeybindingHelp,
            "  Keyboard shortcuts  ",
            items,
        );
        self.selection_select_next();
    }

    pub(crate) fn make_textarea() -> TextArea<'static> {
        TextArea::default()
    }

    fn provider_supports_token_refresh(&self) -> bool {
        self.provider
            .current_instance
            .backend_preset
            .def()
            .auth_mode
            == crate::provider_instance::AuthMode::OAuthLogin
    }

    /// Trigger a token refresh for unauthorized errors and set up automatic retry.
    ///
    /// Returns `true` if refresh was triggered, `false` if conditions weren't met
    /// (already refreshing, provider doesn't support refresh, etc.).
    pub(crate) fn trigger_auth_refresh(&mut self, target: RetryTarget) -> bool {
        if !self.provider_supports_token_refresh() || self.login.refresh_in_progress {
            return false;
        }

        log::debug!(
            "triggering token refresh: provider={} target={:?}",
            self.provider.current_instance.id,
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

        let provider = self.provider.current_instance.id.clone();
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
    pub(crate) fn check_token_preflight(&mut self, target: RetryTarget) -> bool {
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
            &self.provider.current_instance.id,
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
        self.completion.clear();
    }

    pub fn shell_input_is_empty(&self) -> bool {
        self.shell.input_is_empty()
    }

    pub fn enter_shell_mode(&mut self) {
        self.input_mode = InputMode::Shell;
        self.shell.reset_textarea();
        self.completion.clear();
    }

    pub fn exit_shell_mode(&mut self) {
        self.input_mode = InputMode::Chat;
        self.shell.reset_textarea();
    }

    pub fn cycle_shell(&mut self) {
        self.shell.cycle();
    }

    pub fn submit_shell_command(&mut self) {
        let lines: Vec<String> = self.shell.textarea.lines().to_vec();
        let command = lines.join("\n").trim().to_string();
        if command.is_empty() || self.streaming() || self.login.active {
            return;
        }

        // Ensure a session exists so the appended events are persisted.
        self.ensure_event_log_for_submit();

        let cwd = if self.session.current_cwd.is_empty() {
            ".".to_string()
        } else {
            self.session.current_cwd.clone()
        };
        let prompt = self.shell.selected.prompt_char();

        let cmd_prefix = if self.shell.available.len() > 1 {
            format!("[{}] {}{}", self.shell.selected.label(), cwd, prompt)
        } else {
            format!("{}{}", cwd, prompt)
        };

        let call_id = format!("local-shell-{}", self.display_len());

        // Push a live tool entry so the UI renders the tool-call header and
        // live streaming output (via ToolOutputChunk events forwarded from the
        // subprocess).
        self.session
            .live_turn
            .tool_entries
            .push(crate::live_turn::LiveToolEntry {
                id: call_id.clone(),
                name: "local_shell".to_string(),
                args: serde_json::json!({
                    "prefix": cmd_prefix,
                    "command": command,
                }),
                partial_args: String::new(),
                partial_snapshot: None,
                streaming_field: Some("command".to_string()),
                running_output: String::new(),
                result: None,
            });

        self.exit_shell_mode();
        self.log_view.auto_scroll = true;

        // Spawn the subprocess asynchronously, reusing the same execution
        // infrastructure as agent tools (SubprocessCommand + ToolCallContext).
        let tx = self.app_event_tx();
        let ctx = crate::agent::types::ToolCallContext {
            id: call_id.clone(),
            tx: Some(tx.clone()),
            hooks: std::collections::HashMap::new(),
            hook_ipc: crate::hooks::HookIpcPublisherHandle::disabled(),
            session_id: String::new(),
            cancel_rx: None,
        };

        let selected_shell = self.shell.selected;
        self.runtime.pending_shell_handle = Some(tokio::spawn(async move {
            let cmd = match selected_shell {
                ShellKind::Bash => crate::agent::tools::subprocess::SubprocessCommand::new("sh")
                    .arg("-c")
                    .arg(&command),
                #[cfg(windows)]
                ShellKind::Cmd => {
                    crate::agent::tools::subprocess::SubprocessCommand::new("cmd.exe")
                        .arg("/D")
                        .arg("/S")
                        .arg("/C")
                        .raw_arg(format!("\"{command}\""))
                }
                #[cfg(windows)]
                ShellKind::PowerShell => {
                    crate::agent::tools::powershell::powershell_subprocess(&command)
                }
            };

            let result = cmd.current_dir(&cwd).run(ctx).await;
            let _ = tx.send(AppEvent::ShellComplete { call_id, result });
        }));
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

        if let Some(item) = self
            .completion
            .completions
            .get(self.completion.completion_selected)
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
    /// 7) when streaming, show a Ctrl-C hint instead of aborting
    /// 8) when idle, clear non-empty input
    pub fn handle_escape_in_chat_mode(&mut self) {
        if self.is_stepping() {
            // Clean up any in-flight ask_user UI state before cancelling.
            if self.has_pending_ask() {
                self.ask_user.pending = None;
                self.ask_user.freeform_mode = false;
                self.exit_selection_mode();
            }
            self.cancel_stepping();
        } else if self.has_pending_ask() {
            self.cancel_pending_ask();
        } else if self.in_slash_mode() {
            self.reset_textarea();
        } else if self.selection.kind == Some(SelectionKind::ConfirmProviderRemoval) {
            self.exit_selection_mode();
            self.clear_pending_provider_removal();
        } else if self.provider.setup_step != ProviderSetupStep::Idle {
            self.cancel_setup_input();
        } else if self.login.active {
            self.cancel_login();
        } else if self.streaming() {
            // Esc no longer aborts the agent loop — use Ctrl-C for that.
            self.push_notice(Message::assistant(
                "[Use Ctrl-C to abort the agent loop]".to_string(),
            ));
        } else {
            // Idle: clear input if non-empty, no-op if empty (exception to
            // the feedback invariant — base state with nothing to dismiss).
            let input_is_empty = self
                .textarea
                .lines()
                .iter()
                .all(|line| line.trim().is_empty());
            if !input_is_empty {
                self.reset_textarea();
            }
        }
    }

    // ── Completion helpers ────────────────────────────────────────────────────

    /// Recompute the completion list from the current textarea content and
    /// cached model list. Call this after every keystroke.
    pub fn update_completions(&mut self) {
        let cwd = self.session.current_cwd.clone();
        self.completion.update(
            &self.textarea,
            &self.loaded_skills,
            self.provider.thinking_supported,
            &self.provider.instances,
            &cwd,
        );
    }

    /// Returns true if a model-list fetch should be triggered now.
    /// Returns true when a model-list fetch should be triggered automatically.
    ///
    /// Fires when no fetch is already in-flight and the model list has not yet
    /// been populated.  This covers two cases:
    /// 1. No model configured — the list is needed so the user can pick one.
    /// 2. Model configured — the fetch is still beneficial because it populates
    ///    the Copilot model metadata cache (context-window size, vendor) from
    ///    the live API, which otherwise falls back to the hard-coded table.
    ///
    /// Does not trigger when no provider has been selected — on a clean
    /// install the login menu is shown instead.
    pub fn should_auto_query_model(&self) -> bool {
        self.provider.provider_selected
            && !self.completion.models_loading
            && self.completion.available_models.is_none()
            && self.selection.kind != Some(SelectionKind::Model)
    }

    pub fn should_fetch_models(&self) -> bool {
        if self.completion.available_models.is_some()
            || self.completion.models_loading
            || self.completion.model_fetch_error.is_some()
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

        self.completion.models_loading = true;
        self.completion.model_fetch_error = None;
        let future = provider.list_models();
        let tx = self.app_event_tx();
        tokio::spawn(async move {
            let result = future.await;
            let _ = tx.send(AppEvent::ModelsReady(result));
        });
    }

    /// Store a freshly fetched model list (or error) and refresh completions.
    pub fn apply_model_list(&mut self, result: Result<Vec<String>, crate::llm::ProviderError>) {
        self.completion.models_loading = false;
        match result {
            Ok(models) => {
                self.completion.available_models = Some(models);
                self.completion.model_fetch_error = None;
            }
            Err(e) => {
                let is_unauthorized = e.kind == crate::llm::ProviderErrorKind::Unauthorized;

                if is_unauthorized && self.trigger_auth_refresh(RetryTarget::ModelFetch) {
                    // Refresh triggered; retry will happen automatically after refresh completes
                } else {
                    let provider_label = active_provider_display_name(
                        &self.provider.current_instance.id,
                        &self.provider.instances,
                    );
                    self.completion.model_fetch_error =
                        Some(format_provider_error_for_display(&provider_label, &e));
                }
            }
        }
        self.update_completions();

        // If no model is configured and the fetch succeeded, open the model
        // picker automatically so the user can choose one.  On a fresh install
        // (no provider selected), redirect to the login menu instead.
        if self.provider.current_instance.model.is_none()
            && self.completion.available_models.is_some()
            && !self.selection.active
        {
            if !self.provider.provider_selected {
                self.enter_login_selection_mode();
            } else {
                self.enter_model_selection_mode();
            }
            return;
        }

        if self.selection.active && self.selection.kind == Some(SelectionKind::Model) {
            if let Some(err) = &self.completion.model_fetch_error {
                let items = vec![completion::CompletionItem::error_indicator(err)];
                self.set_selection_items(items);
            } else {
                let items: Vec<_> = self
                    .completion
                    .available_models
                    .as_deref()
                    .unwrap_or(&[])
                    .iter()
                    .map(|m| completion::CompletionItem::from_model(m))
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
        self.completion.select_next();
    }

    /// Navigate the completion selection up (wraps around).
    pub fn completion_select_prev(&mut self) {
        self.completion.select_prev();
    }

    /// Replace the textarea with the selected item's `complete_to` text and
    /// move the cursor to the end of the line. No-ops on loading indicators.
    ///
    /// For `@<file>` completions, replaces only the `@` token portion of the
    /// input rather than the entire textarea, preserving surrounding text.
    pub fn apply_completion(&mut self) {
        let item = match self
            .completion
            .completions
            .get(self.completion.completion_selected)
        {
            Some(i) if !i.loading && !i.complete_to.is_empty() => i,
            _ => return,
        };

        let lines: Vec<String> = self.textarea.lines().to_vec();
        let input = lines.join("\n");

        // Check if the textarea contains an @ token that triggered file completions.
        if let Some(range) = Self::find_at_token(&input) {
            // Replace just the @token portion with @ + completed path.
            let completed_path = &item.complete_to;
            let new_text = format!(
                "{}@{}{}",
                &input[..range.0],
                completed_path,
                &input[range.1..]
            );
            self.textarea = TextArea::new(new_text.lines().map(|s| s.to_string()).collect());
        } else {
            // Standard completion: replace entire textarea.
            let text = item.complete_to.clone();
            self.textarea = TextArea::new(vec![text]);
        }

        self.textarea.move_cursor(CursorMove::End);
        self.update_completions();
    }

    /// Find the byte range of the last `@<path>` token in `input`.
    ///
    /// Returns `(start, end)` where `start` is the position of `@` and `end`
    /// is the position after the end of the path fragment.  The token must be
    /// preceded by start-of-string or ASCII whitespace.
    fn find_at_token(input: &str) -> Option<(usize, usize)> {
        let bytes = input.as_bytes();
        let len = bytes.len();
        let mut i = 0;
        let mut result: Option<(usize, usize)> = None;

        while i < len {
            if bytes[i] == b'@' {
                let preceded_by_space = i == 0 || bytes[i - 1].is_ascii_whitespace();
                if preceded_by_space && i + 1 < len {
                    let start = i;
                    let mut end = i + 1;
                    while end < len && !bytes[end].is_ascii_whitespace() && bytes[end] != b'"' {
                        end += 1;
                    }
                    result = Some((start, end));
                    i = end;
                    continue;
                }
            }
            i += 1;
        }

        result
    }

    // ── Step-back navigation ──────────────────────────────────────────────────

    /// Returns the event indices (into the committed event log) of all
    /// step-back boundaries: `UserMessage` events and `ToolResult` events
    /// for `ask_user` (user answers to in-turn questions), in order.
    pub(crate) fn step_boundaries(&self) -> Vec<usize> {
        let Some(ss) = self.session.session_state.as_ref() else {
            return Vec::new();
        };
        ss.events()
            .iter()
            .enumerate()
            .filter_map(|(i, ev)| match ev {
                SessionEvent::UserMessage { .. } => Some(i),
                SessionEvent::ToolResult { name, .. } if name == "ask_user" => Some(i),
                _ => None,
            })
            .collect()
    }

    /// Returns true when step-back navigation is currently active.
    pub(crate) fn is_stepping(&self) -> bool {
        self.step_back.is_stepping()
    }

    /// Step back to the previous step boundary (user message or ask_user answer).
    ///
    /// On the first call, saves the current input field content so it can be
    /// restored on cancel.  No-op when the agent loop is active or there are no
    /// boundaries.
    pub(crate) fn step_back(&mut self) {
        if self.runtime.is_running() {
            return;
        }
        let boundaries = self.step_boundaries();
        if boundaries.is_empty() {
            return;
        }
        // Clear any in-flight ask_user UI from a previous step before
        // repopulating with the new boundary.
        if self.has_pending_ask() && self.ask_user.reply.is_none() {
            self.ask_user.pending = None;
            self.ask_user.freeform_mode = false;
            self.exit_selection_mode();
        }
        let current = self.step_back.cursor;
        let new_cursor = match current {
            None => {
                // Save current input before first step
                self.step_back.save_input(self.textarea.lines().join("\n"));
                // Step to the last UserMessage
                *boundaries.last().unwrap()
            }
            Some(cur) => {
                // Find the boundary strictly before cur
                match boundaries.iter().rev().find(|&&i| i < cur) {
                    Some(&i) => i,
                    None => return, // Already at the earliest boundary
                }
            }
        };
        self.step_back.cursor = Some(new_cursor);
        self.repopulate_input_from_cursor();
        self.scroll_to_step_cursor();
        self.log_view.invalidate();
    }

    /// Step forward toward the next step boundary.
    ///
    /// When stepping past the end, clears step state and restores the saved
    /// input.  No-op when the agent loop is active.
    pub(crate) fn step_forward(&mut self) {
        if self.runtime.is_running() {
            return;
        }
        let Some(cur) = self.step_back.cursor else {
            return;
        };
        // Clear any in-flight ask_user UI from a previous step before
        // repopulating with the new boundary.
        if self.has_pending_ask() && self.ask_user.reply.is_none() {
            self.ask_user.pending = None;
            self.ask_user.freeform_mode = false;
            self.exit_selection_mode();
        }
        let boundaries = self.step_boundaries();
        match boundaries.iter().find(|&&i| i > cur) {
            Some(&next) => {
                self.step_back.cursor = Some(next);
                self.repopulate_input_from_cursor();
                self.scroll_to_step_cursor();
                self.log_view.invalidate();
            }
            None => {
                // Past the end — cancel stepping
                self.cancel_stepping();
            }
        }
    }

    /// Cancel step-back mode, restoring the original input and full view.
    pub(crate) fn cancel_stepping(&mut self) {
        if let Some(saved) = self.step_back.cancel() {
            self.textarea = TextArea::new(vec![saved]);
            self.textarea.move_cursor(CursorMove::End);
        }
        self.log_view.auto_scroll = true;
        self.log_view.invalidate();
    }

    /// Repopulate the input field with the user-provided content at the
    /// current step cursor position (user message text or ask_user answer).
    ///
    /// For `ask_user` tool results, instead of populating the textarea,
    /// restores the full ask_user prompt UI so the user can answer fresh.
    fn repopulate_input_from_cursor(&mut self) {
        let Some(idx) = self.step_back.cursor else {
            return;
        };
        let Some(ss) = self.session.session_state.as_ref() else {
            return;
        };
        match ss.events().get(idx) {
            Some(SessionEvent::UserMessage { content, .. }) => {
                let text = content.clone();
                self.textarea = TextArea::new(vec![text]);
                self.textarea.move_cursor(CursorMove::End);
            }
            Some(SessionEvent::ToolResult { id, name, .. }) if name == "ask_user" => {
                // Find the preceding ToolCall with matching id to get the
                // question and options.
                let tool_call_args = ss.events()[..idx].iter().rev().find_map(|ev| match ev {
                    SessionEvent::ToolCall {
                        id: tid,
                        name: tname,
                        args,
                        ..
                    } if tid == id && tname == "ask_user" => Some(args.clone()),
                    _ => None,
                });
                if let Some(args) = tool_call_args {
                    self.restore_ask_user_from_step(&args);
                }
            }
            _ => {}
        }
    }

    /// Adjust the scroll position so the step cursor boundary is visible with
    /// context on both sides.
    fn scroll_to_step_cursor(&mut self) {
        self.log_view.auto_scroll = false;
        // The exact line count for the kept portion is not known until render
        // time; we set auto_scroll to false and let the render path handle
        // final clamping.  For now, a coarse approximation: scroll to the end
        // of the kept portion.  The render path will center it properly.
        self.log_view.log_scroll = usize::MAX; // will be clamped in draw
    }

    /// Commit the step: create a new branched session from events up to (but
    /// not including) the step cursor, plus the current textarea content as a
    /// new `UserMessage`.  Switches the active session to the branch.
    ///
    /// Returns the new `UserMessage` content, or `None` if not in step mode or
    /// session state is unavailable.
    pub(crate) fn commit_step_branch(&mut self) -> Option<String> {
        let idx = self.step_back.cursor?;
        let new_content = self.textarea.lines().join("\n");
        if new_content.trim().is_empty() {
            return None;
        }

        let mut events: Vec<SessionEvent> = {
            let ss = self.session.session_state.as_ref()?;
            ss.events()[..idx].to_vec()
        };

        // Strip trailing incomplete turn: when stepping back to an ask_user
        // ToolResult boundary, the slice `[..idx]` includes the preceding
        // ToolCall but not its result — leaving an unpaired ToolCall that
        // violates the API invariant "every assistant message with tool_calls
        // must be followed by tool result messages".
        trim_incomplete_turn(&mut events);

        let cwd = self.session.current_cwd.clone();
        let new_session_id = self
            .session
            .session_store
            .as_mut()
            .and_then(|store| store.create_session_from_events(&cwd, &events).ok());

        // Switch active session
        if let Some(ref session_id) = new_session_id
            && let Some(store) = &self.session.session_store
            && let Ok(log) = store.load_events(session_id)
        {
            self.session.current_session_id = Some(session_id.clone());
            self.session.session_state =
                Some(crate::session_state::SessionState::from_event_log(log));
            self.session.live_turn = crate::live_turn::LiveTurnState::new();
            self.session.pending_turn_events.clear();
        }

        // Clear step state
        self.step_back.clear();
        self.log_view.auto_scroll = true;
        self.log_view.invalidate();

        Some(new_content)
    }

    /// Copy the last assistant response to the system clipboard.
    ///
    /// Prefers the currently streaming (live turn) assistant content; falls
    /// back to the last committed assistant message.  Silently does nothing
    /// if there is no assistant content to copy.
    pub fn copy_last_assistant_response(&mut self) {
        let text = if self.session.live_turn.has_assistant_content() {
            self.session.live_turn.assistant_content.clone()
        } else if let Some(ss) = self.session.session_state.as_ref() {
            let msgs = ss.display_messages();
            msgs.iter()
                .rev()
                .find(|m| m.role == crate::llm::Role::Assistant)
                .map(|m| m.content.clone())
                .unwrap_or_default()
        } else {
            String::new()
        };

        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }

        match crate::clipboard::set_clipboard(&text) {
            Ok(()) => {
                log::debug!("copied {} bytes to clipboard via OSC 52", text.len());
                self.agent_turn
                    .set_status(Some(StreamingStatus::CompletedMessage(
                        "📋 Copied to clipboard.".to_string(),
                    )));
            }
            Err(e) => {
                log::debug!("clipboard copy failed: {e}");
            }
        }
    }
}

/// Strip trailing unpaired [`ToolCall`](SessionEvent::ToolCall) events and the
/// [`AssistantMessage`](SessionEvent::AssistantMessage) that started their
/// turn from a step-back branch event slice.
///
/// When `commit_step_branch` slices events at a step boundary that points to
/// an `ask_user` [`ToolResult`](SessionEvent::ToolResult), the preceding
/// `ToolCall` is included but its result is excluded.  This leaves an
/// unpaired `ToolCall` that violates the API invariant "every assistant
/// message with `tool_calls` must be followed by tool result messages".
///
/// This function walks backwards from the end of `events`, finds any trailing
/// `ToolCall` events that have no matching `ToolResult` in the slice, and
/// removes them together with the `AssistantMessage` that started their turn
/// (if all tool calls belonging to that turn are unpaired).
fn trim_incomplete_turn(events: &mut Vec<SessionEvent>) {
    // Walk backwards collecting unpaired ToolCall IDs.
    let mut unpaired: Vec<String> = Vec::new();
    for i in (0..events.len()).rev() {
        match &events[i] {
            SessionEvent::ToolCall { id, .. } => {
                // Check whether a matching ToolResult exists anywhere
                // in the slice (results always follow their calls).
                let has_result = events
                    .iter()
                    .any(|e| matches!(e, SessionEvent::ToolResult { id: rid, .. } if rid == id));
                if has_result {
                    // This ToolCall is paired — stop; everything before it is
                    // part of completed turns.
                    break;
                }
                unpaired.push(id.clone());
            }
            SessionEvent::AssistantMessage { .. } => {
                // If every ToolCall after this AssistantMessage is in
                // `unpaired`, this AssistantMessage starts the incomplete
                // turn.  Truncate here — the incomplete turn is stripped.
                let all_after_unpaired = events[i + 1..].iter().all(|e| match e {
                    SessionEvent::ToolCall { id, .. } => unpaired.contains(id),
                    _ => false,
                });
                if all_after_unpaired && !events[i + 1..].is_empty() {
                    events.truncate(i);
                }
                // Stop regardless — we only strip one trailing incomplete turn.
                break;
            }
            _ => {}
        }
    }
}

/// Return a sort key for session scope: 0 = local, 1 = similar, 2 = foreign.
fn session_scope(session_cwd: &str, current_cwd: &str, current_folder: &str) -> u8 {
    if session_cwd == current_cwd {
        0 // local
    } else if !current_folder.is_empty() && folder_name(session_cwd) == Some(current_folder) {
        1 // similar
    } else {
        2 // foreign
    }
}

/// Return a human-readable scope label for a session.
fn session_scope_label(session_cwd: &str, current_cwd: &str, current_folder: &str) -> &'static str {
    if session_cwd == current_cwd {
        "local"
    } else if !current_folder.is_empty() && folder_name(session_cwd) == Some(current_folder) {
        "similar"
    } else {
        "foreign"
    }
}

/// Extract the final path component as `Some(&str)`, or `None` for empty/"." paths.
fn folder_name(path: &str) -> Option<&str> {
    let p = std::path::Path::new(path);
    let name = p.file_name()?.to_str()?;
    if name.is_empty() || name == "." || name == ".." {
        None
    } else {
        Some(name)
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
        app_event::AppEvent,
        llm::{Message, ProviderError, Role},
        provider_instance::{ApiType, BackendPreset, ProviderInstance},
        provider_manager::{PendingProviderSetup, SetupInputKind},
        session_event::SessionEvent,
        thinking::ThinkingLevel,
    };

    fn make_app() -> App {
        let instance =
            crate::provider_instance::ProviderInstance::new("openai", BackendPreset::OpenAi);
        App::new(
            instance,
            "gpt-4o",
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
                executor: std::sync::Arc::new(crate::agent::DefaultToolExecutor::new()),
                system_prompt: None,
                hooks: std::collections::HashMap::new(),
                hook_ipc: crate::hooks::HookIpcPublisherHandle::disabled(),
                session_id: String::new(),
            },
            crate::config::DisplayConfig::default(),
        )
    }

    fn install_test_agent_task(app: &mut App) {
        app.runtime.agent_task = Some(tokio::spawn(async {
            std::future::pending::<()>().await;
        }));
    }

    #[test]
    fn fresh_app_has_no_provider_selected() {
        let app = make_app();
        assert!(
            !app.provider.provider_selected,
            "fresh App should not have a provider selected"
        );
    }

    #[test]
    fn submit_chat_message_blocked_when_no_provider_selected() {
        let mut app = make_app();
        assert!(!app.provider.provider_selected);

        let provider = std::sync::Arc::new(crate::llm::test_provider::TestProvider::new())
            as std::sync::Arc<dyn crate::llm::LlmProvider + Send + Sync>;
        // Populate the textarea with a non-empty message.
        app.textarea.insert_str("hello");

        // Should push a notice rather than submit.
        let before = app.session.live_turn.notices.len();
        app.submit_chat_message(&provider);
        assert_eq!(
            app.session.live_turn.notices.len(),
            before + 1,
            "should have pushed a notice"
        );
        assert!(
            app.session
                .live_turn
                .notices
                .last()
                .unwrap()
                .content
                .contains("no provider selected"),
            "notice should mention no provider selected"
        );
        // Pending finalise should NOT be set (no real submission happened).
        assert!(!app.runtime.pending_finalize);

        // Now set provider_selected and verify submission proceeds.
        app.provider.provider_selected = true;
        app.submit_chat_message(&provider);
        // The textarea was cleared by the first call; refill.
        app.textarea.insert_str("hello again");
        app.submit_chat_message(&provider);
        assert!(
            app.runtime.pending_finalize,
            "should have triggered submission"
        );
    }

    #[test]
    fn setup_input_kind_uses_service_specific_prompts() {
        assert_eq!(
            SetupInputKind::Name.prompt_label(None),
            "provider instance name: "
        );

        let mut open_webui = ProviderInstance::new("work-webui", BackendPreset::OpenWebUi);
        open_webui.api_type = ApiType::OpenAiCompatible;
        assert_eq!(
            SetupInputKind::BaseUrl.prompt_label(Some(&open_webui)),
            "open-webui URL: "
        );
        assert_eq!(
            SetupInputKind::ApiKey.prompt_label(Some(&open_webui)),
            "open-webui token: "
        );

        let mut openrouter = ProviderInstance::new("router", BackendPreset::OpenRouter);
        openrouter.api_type = ApiType::OpenAiCompatible;
        assert_eq!(
            SetupInputKind::BaseUrl.prompt_label(Some(&openrouter)),
            "URL: "
        );
        assert_eq!(
            SetupInputKind::ApiKey.prompt_label(Some(&openrouter)),
            "OpenRouter API key: "
        );

        let mut ollama = ProviderInstance::new("gpu-box", BackendPreset::Ollama);
        ollama.api_type = ApiType::OllamaChatApi;
        assert_eq!(
            SetupInputKind::BaseUrl.prompt_label(Some(&ollama)),
            "ollama URL: "
        );

        let mut compat = ProviderInstance::new("test", BackendPreset::OpenAiCompatible);
        compat.api_type = ApiType::OpenAiCompatible;
        assert_eq!(SetupInputKind::BaseUrl.prompt_label(Some(&compat)), "URL: ");
        assert_eq!(
            SetupInputKind::ApiKey.prompt_label(Some(&compat)),
            "API key: "
        );
    }

    #[test]
    fn enter_provider_selection_mode_lists_providers_and_login_entry() {
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

        assert!(items.contains(&"/login"));
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
            app.provider
                .pending_removal
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

        assert!(app.provider.pending_removal.is_none());
    }

    #[test]
    fn submit_pending_provider_base_url_stores_openai_compatible_endpoint() {
        let mut app = make_app();
        app.provider.pending_setup = Some(PendingProviderSetup::new("test".to_string()));
        app.set_pending_provider_backend_preset(BackendPreset::OpenAiCompatible);
        app.set_pending_provider_api_type(ApiType::OpenAiCompatible);
        app.enter_provider_endpoint_input_mode();
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
        app.provider.pending_setup = Some(PendingProviderSetup::new("router".to_string()));
        app.set_pending_provider_backend_preset(BackendPreset::OpenRouter);
        app.set_pending_provider_api_type(ApiType::OpenAiCompatible);
        app.enter_provider_endpoint_input_mode();
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
        app.provider.pending_setup = Some(PendingProviderSetup::new("test".to_string()));
        app.enter_provider_api_key_input_mode();
        app.textarea.insert_str("sk-test");

        let token = app
            .submit_pending_provider_api_key()
            .expect("provider token");
        assert_eq!(token, "sk-test");
        assert_eq!(
            app.provider
                .pending_setup
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
        app.provider.pending_setup = Some(PendingProviderSetup::new(String::new()));
        app.set_pending_provider_backend_preset(BackendPreset::Ollama);

        app.enter_provider_endpoint_input_mode();

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
        app.provider.pending_setup = Some(PendingProviderSetup::from_instance(&instance));
        app.enter_provider_api_key_input_mode();

        let token = app
            .submit_pending_provider_api_key()
            .expect("provider token");
        assert_eq!(token, "sk-existing");
        assert_eq!(
            app.provider
                .pending_setup
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

        app.provider.pending_setup = Some(PendingProviderSetup::new(String::new()));
        app.set_pending_provider_backend_preset(BackendPreset::OpenWebUi);
        if let Some(setup) = app.provider.pending_setup.as_mut() {
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
            app.provider.pending_setup.as_ref().map(|p| p.id.as_str()),
            Some("gpu-box")
        );
    }

    #[test]
    fn pending_provider_instance_uses_suggested_id_when_name_not_confirmed_yet() {
        let mut app = make_app();
        app.provider.pending_setup = Some(PendingProviderSetup::new(String::new()));
        app.set_pending_provider_backend_preset(BackendPreset::Ollama);
        app.set_pending_provider_api_type(ApiType::AnthropicCompatible);
        if let Some(setup) = app.provider.pending_setup.as_mut() {
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

        app.provider.pending_setup = Some(PendingProviderSetup::from_instance(&instance));
        app.enter_provider_name_input_mode();

        assert_eq!(app.textarea.lines().join(""), "gpu-box");
    }

    #[test]
    fn enter_provider_name_input_mode_prefills_ollama_name_from_endpoint() {
        let mut app = make_app();
        app.provider.pending_setup = Some(PendingProviderSetup::new(String::new()));
        app.set_pending_provider_backend_preset(BackendPreset::Ollama);
        if let Some(setup) = app.provider.pending_setup.as_mut() {
            setup.base_url = Some("http://localhost:11434".to_string());
        }

        app.enter_provider_name_input_mode();

        assert_eq!(app.textarea.lines().join(""), "ollama-localhost");
    }

    #[test]
    fn pending_provider_instance_uses_backend_based_placeholder_id_before_url_is_known() {
        let mut app = make_app();
        app.provider.pending_setup = Some(PendingProviderSetup::new(String::new()));
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
        app.provider.pending_setup = Some(PendingProviderSetup::new("gpu-box".to_string()));
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
        let norm = BackendPreset::Ollama
            .def()
            .url_normalization
            .as_ref()
            .unwrap();
        assert_eq!(
            norm.normalize("gpu-box:8080"),
            Some("http://gpu-box:8080".to_string())
        );
    }

    #[test]
    fn normalize_ollama_endpoint_adds_default_port_when_scheme_present() {
        let norm = BackendPreset::Ollama
            .def()
            .url_normalization
            .as_ref()
            .unwrap();
        assert_eq!(
            norm.normalize("https://gpu-box"),
            Some("https://gpu-box:11434".to_string())
        );
    }

    #[test]
    fn normalize_ollama_endpoint_keeps_existing_scheme_and_port() {
        let norm = BackendPreset::Ollama
            .def()
            .url_normalization
            .as_ref()
            .unwrap();
        assert_eq!(
            norm.normalize("http://gpu-box:8080"),
            Some("http://gpu-box:8080".to_string())
        );
    }

    #[test]
    fn normalize_ollama_endpoint_rejects_empty_input() {
        let norm = BackendPreset::Ollama
            .def()
            .url_normalization
            .as_ref()
            .unwrap();
        assert_eq!(norm.normalize("   "), None);
    }

    // ── receive_ask_request ───────────────────────────────────────────────────

    /// When ask_user has no options, receive_ask_request must go directly into
    /// freeform mode (not selection mode).  The question is visible in the log
    /// tool call, so no selection header is needed.
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
        assert!(app.has_pending_ask(), "pending ask should be set");
        assert_eq!(
            app.ask_user.pending.as_ref().map(|p| p.question.as_str()),
            Some("What is your name?")
        );
    }

    /// When ask_user has options and allow_freeform is true, the freeform
    /// sentinel should appear after the option items.
    #[test]
    fn receive_ask_request_with_options_and_freeform_includes_sentinel() {
        let mut app = make_app();
        let (reply_tx, _reply_rx) = tokio::sync::oneshot::channel::<AskUserResponse>();
        app.receive_ask_request(AskRequest {
            question: "Choose one".to_string(),
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
            question: "Choose one".to_string(),
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
            .completion
            .completions
            .get(app.completion.completion_selected)
            .expect("expected at least one completion");
        assert_eq!(selected.complete_to, "/model ");
        assert_eq!(app.slash_submit_text().as_deref(), Some("/model"));
    }

    #[test]
    fn slash_submit_text_falls_back_to_raw_input_when_no_completion() {
        let mut app = make_app();
        app.textarea.insert_str("/unknown");
        app.update_completions();
        assert!(app.completion.completions.is_empty());

        assert_eq!(app.slash_submit_text().as_deref(), Some("/unknown"));
    }

    #[test]
    fn handle_escape_in_chat_mode_prefers_slash_cancel_over_stream_abort() {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        rt.block_on(async {
            let mut app = make_app();
            app.agent_turn.start();
            install_test_agent_task(&mut app);
            app.textarea.insert_str("/model gpt");

            app.handle_escape_in_chat_mode();

            assert!(
                app.streaming(),
                "streaming should remain active when ESC cancels slash input"
            );
            assert!(
                app.runtime.is_running(),
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
                !app.session
                    .live_turn
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
    fn handle_escape_in_chat_mode_shows_notice_when_streaming() {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        rt.block_on(async {
            let mut app = make_app();
            app.agent_turn.start();
            install_test_agent_task(&mut app);
            app.textarea.insert_str("hello");

            app.handle_escape_in_chat_mode();

            // Esc no longer aborts the agent loop — it shows a notice.
            assert!(
                app.streaming(),
                "streaming should remain active when Esc is pressed"
            );
            assert!(
                app.runtime.is_running(),
                "agent task should not be removed by Esc"
            );
            assert!(
                app.session
                    .live_turn
                    .notices
                    .iter()
                    .any(|m| { m.content.contains("Use Ctrl-C to abort") })
            );

            // Clean up.
            if let Some(handle) = app.runtime.agent_task.take() {
                handle.abort();
            }
        });
    }

    #[test]
    fn enter_and_exit_keybinding_help_leaves_draft_input_untouched() {
        let mut app = make_app();
        app.textarea =
            ratatui_textarea::TextArea::new(vec!["hello".to_string(), "world".to_string()]);

        app.enter_keybinding_help_mode();
        assert!(
            app.selection.active,
            "help should open as a selection modal"
        );
        assert_eq!(
            app.textarea.lines(),
            &["hello".to_string(), "world".to_string()]
        );

        app.exit_selection_mode();
        assert_eq!(
            app.textarea.lines(),
            &["hello".to_string(), "world".to_string()]
        );
    }

    #[test]
    fn keybinding_help_does_not_enable_selection_filter_mode() {
        let mut app = make_app();
        app.textarea = ratatui_textarea::TextArea::new(vec!["draft".to_string()]);

        app.enter_keybinding_help_mode();

        assert_eq!(
            app.selection.kind,
            Some(crate::selection_state::SelectionKind::KeybindingHelp)
        );
        assert!(
            !app.selection_filter_enabled(),
            "keyboard help should not replace the input field with a filter query"
        );
        assert_eq!(app.textarea.lines(), &["draft".to_string()]);
    }

    #[test]
    fn f1_toggles_keybinding_help_without_clearing_draft() {
        let provider: std::sync::Arc<dyn crate::llm::LlmProvider + Send + Sync> =
            std::sync::Arc::new(crate::llm::test_provider::TestProvider::new());
        let mut app = make_app();
        app.textarea = ratatui_textarea::TextArea::new(vec!["draft".to_string()]);

        let f1 = crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::F(1),
            crossterm::event::KeyModifiers::NONE,
        );

        let first = crate::input::handle_key_event(
            &mut app,
            &provider,
            &crate::config::XiConfig::default(),
            f1,
        );
        assert!(first.is_none());
        assert_eq!(
            app.selection.kind,
            Some(crate::selection_state::SelectionKind::KeybindingHelp)
        );
        assert_eq!(app.textarea.lines(), &["draft".to_string()]);

        let second = crate::input::handle_key_event(
            &mut app,
            &provider,
            &crate::config::XiConfig::default(),
            f1,
        );
        assert!(second.is_none());
        assert!(!app.selection.active, "second F1 should close help");
        assert_eq!(app.textarea.lines(), &["draft".to_string()]);
    }

    #[test]
    fn escape_closes_keybinding_help_while_streaming() {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        rt.block_on(async {
            let mut app = make_app();
            app.agent_turn.start();
            install_test_agent_task(&mut app);
            app.textarea = ratatui_textarea::TextArea::new(vec!["draft".to_string()]);
            app.enter_keybinding_help_mode();

            let key = crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Esc,
                crossterm::event::KeyModifiers::NONE,
            );
            let dispatch = crate::input::handle_key_event(
                &mut app,
                &(std::sync::Arc::new(crate::llm::test_provider::TestProvider::new())
                    as std::sync::Arc<dyn crate::llm::LlmProvider + Send + Sync>),
                &crate::config::XiConfig::default(),
                key,
            );

            assert!(dispatch.is_none());
            assert!(!app.selection.active, "Esc should close keybinding help");
            assert!(
                app.streaming(),
                "closing help should not stop the agent loop"
            );
            assert_eq!(app.textarea.lines(), &["draft".to_string()]);
            assert!(
                app.session
                    .live_turn
                    .notices
                    .iter()
                    .all(|m| !m.content.contains("Use Ctrl-C to abort")),
                "closing help should not show the streaming abort hint"
            );

            if let Some(handle) = app.runtime.agent_task.take() {
                handle.abort();
            }
        });
    }

    #[test]
    fn abort_agent_loop_appends_error_result_for_pending_tool_call() {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        rt.block_on(async {
            let tmp = tempfile::tempdir().expect("tempdir");
            let path = tmp.path().join("session.jsonl");

            let mut app = make_app();
            app.session.session_state = Some(crate::session_state::SessionState::from_event_log(
                crate::event_log::EventLog::load(&path).expect("load event log"),
            ));
            app.agent_turn.start();
            install_test_agent_task(&mut app);
            // Simulate an in-flight tool call via pending_turn_events.
            app.session
                .pending_turn_events
                .push(crate::session_event::SessionEvent::ToolCall {
                    id: "call_1".to_string(),
                    name: "powershell".to_string(),
                    args: serde_json::json!({"command": "git diff"}),
                    include_in_llm: true,
                    timestamp: 1,
                });
            app.session
                .live_turn
                .tool_entries
                .push(crate::live_turn::LiveToolEntry {
                    id: "call_1".to_string(),
                    name: "powershell".to_string(),
                    args: serde_json::json!({"command": "git diff"}),
                    partial_args: String::new(),
                    partial_snapshot: None,
                    streaming_field: None,
                    running_output: String::new(),
                    result: None,
                });

            app.abort_agent_loop();

            let tool_result = app
                .session
                .session_state
                .as_ref()
                .expect("session state")
                .display_messages()
                .iter()
                .find(|m| m.role == Role::ToolResult && m.tool_call_id.as_deref() == Some("call_1"))
                .expect("expected abort tool result");
            assert!(tool_result.is_error, "abort tool result should be an error");
            assert_eq!(tool_result.content, "Interrupted by user");
        });
    }

    #[test]
    fn abort_agent_loop_flushes_error_result_for_pending_tool_call_to_event_log() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("session.jsonl");

        let rt = tokio::runtime::Runtime::new().expect("runtime");
        rt.block_on(async {
            let mut app = make_app();
            app.session.session_state = Some(crate::session_state::SessionState::from_event_log(
                crate::event_log::EventLog::load(&path).expect("load event log"),
            ));
            app.agent_turn.start();
            install_test_agent_task(&mut app);
            app.session
                .pending_turn_events
                .push(crate::session_event::SessionEvent::ToolCall {
                    id: "call_1".to_string(),
                    name: "ask_user".to_string(),
                    args: serde_json::json!({"question": "Continue?"}),
                    include_in_llm: true,
                    timestamp: 1,
                });
            app.session
                .live_turn
                .tool_entries
                .push(crate::live_turn::LiveToolEntry {
                    id: "call_1".to_string(),
                    name: "ask_user".to_string(),
                    args: serde_json::json!({"question": "Continue?"}),
                    partial_args: String::new(),
                    partial_snapshot: None,
                    streaming_field: None,
                    running_output: String::new(),
                    result: None,
                });

            app.abort_agent_loop();

            let events = app
                .session
                .session_state
                .as_ref()
                .expect("session state")
                .events();
            let tool_results: Vec<_> = events
                .iter()
                .filter_map(|event| match event {
                    crate::session_event::SessionEvent::ToolResult {
                        id,
                        name,
                        content,
                        is_error,
                        ..
                    } if id == "call_1" => Some((name.clone(), content.clone(), *is_error)),
                    _ => None,
                })
                .collect();

            assert_eq!(tool_results.len(), 1, "expected exactly one tool result");
            assert_eq!(
                tool_results[0],
                (
                    "ask_user".to_string(),
                    "Interrupted by user".to_string(),
                    true
                )
            );
        });
    }

    #[test]
    fn abort_agent_loop_does_not_duplicate_existing_tool_result() {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        rt.block_on(async {
            let tmp = tempfile::tempdir().expect("tempdir");
            let path = tmp.path().join("session.jsonl");

            let mut app = make_app();
            app.session.session_state = Some(crate::session_state::SessionState::from_event_log(
                crate::event_log::EventLog::load(&path).expect("load event log"),
            ));
            app.agent_turn.start();
            install_test_agent_task(&mut app);
            // Simulate a ToolCall with its result already in pending_turn_events.
            app.session
                .pending_turn_events
                .push(crate::session_event::SessionEvent::ToolCall {
                    id: "call_1".to_string(),
                    name: "powershell".to_string(),
                    args: serde_json::json!({"command": "git diff"}),
                    include_in_llm: true,
                    timestamp: 1,
                });
            app.session
                .pending_turn_events
                .push(crate::session_event::SessionEvent::ToolResult {
                    id: "call_1".to_string(),
                    name: "powershell".to_string(),
                    content: "done".to_string(),
                    is_error: false,
                    display_range: None,
                    include_in_llm: true,
                    timestamp: 1,
                });

            app.abort_agent_loop();

            let matching_results = app
                .session
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
            app.session.session_state = Some(crate::session_state::SessionState::from_event_log(
                crate::event_log::EventLog::load(&path).expect("load event log"),
            ));
            app.session.live_turn.assistant_content = "partial".to_string();
            app.agent_turn.start();
            install_test_agent_task(&mut app);

            app.abort_agent_loop();

            let display = app
                .session
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
                app.agent_turn.status,
                Some(StreamingStatus::CompletedMessage(ref s)) if s == "[agent loop aborted]"
            ));

            let provider: std::sync::Arc<dyn crate::llm::LlmProvider + Send + Sync> =
                std::sync::Arc::new(crate::llm::test_provider::TestProvider::new());
            app.launch_turn(&provider);

            assert!(matches!(
                app.agent_turn.status,
                Some(StreamingStatus::Waiting)
            ));
        });
    }

    #[test]
    fn request_hard_abort_keeps_running_tool_alive_for_cooperative_cancel() {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        rt.block_on(async {
            let mut app = make_app();
            app.agent_turn.start();
            install_test_agent_task(&mut app);
            let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(crate::agent::CancelLevel::None);
            app.runtime.cancel_tx = Some(cancel_tx);
            app.session.live_turn.tool_entries.push(crate::live_turn::LiveToolEntry {
                id: "call_1".to_string(),
                name: "bash".to_string(),
                args: serde_json::json!({"command": "sleep 10"}),
                partial_args: String::new(),
                partial_snapshot: None,
                streaming_field: None,
                running_output: String::new(),
                result: None,
            });

            app.request_hard_abort();

            assert!(app.runtime.agent_task.is_some(), "tool-phase hard abort should keep agent task alive");
            assert_eq!(app.runtime.abort_stage, crate::agent::CancelLevel::HardAbort);
            assert_eq!(*cancel_rx.borrow(), crate::agent::CancelLevel::HardAbort);
            assert!(matches!(
                app.agent_turn.status,
                Some(StreamingStatus::Message(ref s)) if s == "[Aborting… Press Ctrl-C again to force kill]"
            ));

            if let Some(handle) = app.runtime.agent_task.take() {
                handle.abort();
            }
        });
    }

    #[test]
    fn request_hard_abort_without_running_tool_aborts_immediately() {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        rt.block_on(async {
            let tmp = tempfile::tempdir().expect("tempdir");
            let path = tmp.path().join("session.jsonl");

            let mut app = make_app();
            app.session.session_state = Some(crate::session_state::SessionState::from_event_log(
                crate::event_log::EventLog::load(&path).expect("load event log"),
            ));
            app.session.live_turn.assistant_content = "partial".to_string();
            app.agent_turn.start();
            install_test_agent_task(&mut app);
            let (cancel_tx, _cancel_rx) = tokio::sync::watch::channel(crate::agent::CancelLevel::None);
            app.runtime.cancel_tx = Some(cancel_tx);

            app.request_hard_abort();

            assert!(app.runtime.agent_task.is_none(), "model-phase hard abort should stop the task immediately");
            assert!(matches!(
                app.agent_turn.status,
                Some(StreamingStatus::CompletedMessage(ref s)) if s == "[Aborting… Press Ctrl-C again to force kill]"
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
            app.session.session_state = Some(crate::session_state::SessionState::from_event_log(
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
        app.session.session_state = Some(crate::session_state::SessionState::from_event_log(
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
            result: crate::agent::types::ToolResult::ok_str("ok"),
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
        app.session.session_state = Some(crate::session_state::SessionState::from_event_log(
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
            .session
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

    // ── Step 6: resume/export/integration paths ─────────────────────────────

    #[test]
    fn submit_initialises_session_state_even_without_session_store() {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        rt.block_on(async {
            let mut app = make_app();
            app.session.session_store = None; // persistence unavailable
            app.textarea.insert_str("hello");

            let provider: std::sync::Arc<dyn crate::llm::LlmProvider + Send + Sync> =
                std::sync::Arc::new(crate::llm::test_provider::TestProvider::new());

            app.submit(&provider);

            assert!(
                app.session.session_state.is_some(),
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
        app.session.session_store = Some(store);
        app.session.live_turn.assistant_content = "streaming".to_string();
        app.session
            .live_turn
            .notices
            .push(Message::assistant("[notice]"));

        app.resume_session_by_id(&session_id);

        assert!(app.session.live_turn.assistant_content.is_empty());
        assert!(app.session.live_turn.tool_entries.is_empty());
        assert!(app.session.live_turn.notices.is_empty());
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
        app.session.current_cwd = tmp.path().to_string_lossy().to_string();
        app.session.session_state = Some(crate::session_state::SessionState::from_event_log(
            crate::event_log::EventLog::load(&path).expect("load event log"),
        ));
        app.append_event_immediate(crate::session_event::SessionEvent::UserMessage {
            content: "committed".to_string(),
            timestamp: 1,
        });
        app.session.live_turn.assistant_content = "live assistant".to_string();
        app.session
            .live_turn
            .notices
            .push(Message::assistant("[notice]"));

        app.export_session_html(Some(export_path.to_str().expect("utf8 path")));

        let html = std::fs::read_to_string(&export_path).expect("read export html");
        assert!(html.contains("committed"));
        assert!(!html.contains("live assistant"));
        assert!(!html.contains("[notice]"));
    }

    #[test]
    fn submit_injects_attachment_events_after_user_message() {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        rt.block_on(async {
            let tmp = tempfile::tempdir().expect("tempdir");
            let path = tmp.path().join("note.txt");
            std::fs::write(&path, "attached contents\n").expect("write attachment");

            let mut app = make_app();
            app.session.current_cwd = tmp.path().to_string_lossy().to_string();
            app.textarea.insert_str("please inspect @note.txt");

            let provider: std::sync::Arc<dyn crate::llm::LlmProvider + Send + Sync> =
                std::sync::Arc::new(crate::llm::test_provider::TestProvider::new());

            app.submit(&provider);

            let events = app
                .session
                .session_state
                .as_ref()
                .expect("session state")
                .events();
            let user_idx = events
                .iter()
                .position(|event| {
                    matches!(
                        event,
                        crate::session_event::SessionEvent::UserMessage { content, .. }
                            if content == "please inspect `note.txt`"
                    )
                })
                .expect("submitted user message present");
            assert!(matches!(
                events.get(user_idx + 1),
                Some(crate::session_event::SessionEvent::ToolCall { id, name, .. })
                    if id == "attach_0" && name == "read_file"
            ));
            assert!(matches!(
                events.get(user_idx + 2),
                Some(crate::session_event::SessionEvent::ToolResult { id, content, is_error, .. })
                    if id == "attach_0" && content == "attached contents\n" && !is_error
            ));

            if let Some(handle) = app.runtime.agent_task.take() {
                handle.abort();
            }
        });
    }

    #[test]
    fn provider_error_clears_live_turn_and_commits_turn_error_event() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("session.jsonl");
        let mut app = make_app();
        app.session.session_state = Some(crate::session_state::SessionState::from_event_log(
            crate::event_log::EventLog::load(&path).expect("load event log"),
        ));
        app.agent_turn.start();
        app.session.live_turn.assistant_content = "partial".to_string();
        app.session
            .pending_turn_events
            .push(crate::session_event::SessionEvent::ToolCall {
                id: "c1".to_string(),
                name: "read_file".to_string(),
                args: serde_json::json!({"path": "src/main.rs"}),
                include_in_llm: true,
                timestamp: 1,
            });

        app.apply_agent_event(crate::agent::types::AgentEvent::Error(ProviderError {
            message: "boom".to_string(),
            kind: crate::llm::ProviderErrorKind::Other,
            status_code: None,
            source: "test".to_string(),
        }));

        assert!(app.session.live_turn.assistant_content.is_empty());
        assert!(app.session.pending_turn_events.is_empty());
        assert!(
            app.session.live_turn.notices.is_empty(),
            "provider errors should not accumulate as persistent live notices"
        );

        let events = app
            .session
            .session_state
            .as_ref()
            .expect("session state")
            .events();
        let turn_errors: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, crate::session_event::SessionEvent::TurnError { .. }))
            .collect();
        assert_eq!(
            turn_errors.len(),
            1,
            "TurnError should be committed exactly once"
        );
    }

    #[test]
    fn empty_status_update_keeps_throbber_visible_while_waiting() {
        let mut app = make_app();
        app.agent_turn.start();

        assert!(app.throbber_visible());

        app.apply_agent_event(crate::agent::types::AgentEvent::StatusUpdate(String::new()));

        assert!(app.throbber_visible());
    }

    #[test]
    fn non_empty_status_update_temporarily_hides_throbber() {
        let mut app = make_app();
        app.agent_turn.start();

        assert!(app.throbber_visible());

        app.apply_agent_event(crate::agent::types::AgentEvent::StatusUpdate(
            "retrying in 1s…".to_string(),
        ));

        assert!(!app.throbber_visible());
    }

    #[test]
    fn whitespace_text_token_keeps_throbber_visible_while_waiting() {
        let mut app = make_app();
        app.agent_turn.start();

        app.apply_agent_event(crate::agent::types::AgentEvent::TextToken {
            text: "   \n".to_string(),
            phase: crate::llm::AssistantPhase::Unknown,
        });

        assert!(app.throbber_visible());
    }

    #[test]
    fn whitespace_thinking_token_keeps_throbber_visible_while_waiting() {
        let mut app = make_app();
        app.agent_turn.start();

        app.apply_agent_event(crate::agent::types::AgentEvent::ThinkingToken(
            "\n\n".to_string(),
        ));

        assert!(app.throbber_visible());
    }

    #[test]
    fn provider_status_visibility_follows_status_messages() {
        let mut app = make_app();
        assert!(!app.provider_status_visible());

        app.agent_turn
            .set_status(Some(StreamingStatus::Message("compacting…".to_string())));
        assert!(app.provider_status_visible());

        app.agent_turn.set_status(None);
        assert!(!app.provider_status_visible());
    }

    #[test]
    fn notices_survive_turn_boundary_but_are_not_committed_or_sent_to_llm() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("session.jsonl");
        let mut app = make_app();
        app.session.session_state = Some(crate::session_state::SessionState::from_event_log(
            crate::event_log::EventLog::load(&path).expect("load event log"),
        ));

        app.session
            .live_turn
            .notices
            .push(Message::assistant("[notice]"));
        app.session.live_turn.assistant_content = "hi".to_string();
        app.apply_agent_event(crate::agent::types::AgentEvent::TurnEnd);

        assert_eq!(
            app.session.live_turn.notices.len(),
            1,
            "notice should survive turn boundary"
        );
        assert_eq!(app.session.live_turn.notices[0].content, "[notice]");
        assert!(
            app.session.live_turn.assistant_content.is_empty(),
            "turn content should clear"
        );

        let events = app
            .session
            .session_state
            .as_ref()
            .expect("session state")
            .events();
        assert!(
            !events.iter().any(|e| matches!(
                e,
                crate::session_event::SessionEvent::AssistantMessage { content, .. } if content == "[notice]"
            )),
            "notice must not be committed as a session event"
        );

        let llm = app
            .session
            .session_state
            .as_mut()
            .expect("session state")
            .llm_messages()
            .to_vec();
        assert!(
            !llm.iter().any(|m| m.content == "[notice]"),
            "notice must not appear in LLM input"
        );
    }

    #[tokio::test]
    async fn shell_output_is_ui_only_and_excluded_from_event_log_and_llm() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("session.jsonl");
        let mut app = make_app();
        app.session.current_cwd = tmp.path().to_string_lossy().to_string();
        app.session.session_state = Some(crate::session_state::SessionState::from_event_log(
            crate::event_log::EventLog::load(&path).expect("load event log"),
        ));
        #[cfg(windows)]
        app.shell.textarea.insert_str("Write-Output hello");
        #[cfg(not(windows))]
        app.shell.textarea.insert_str("printf 'hello'");

        app.submit_shell_command();

        // Async: wait for the ShellComplete event from the spawned task.
        // Don't dispatch through apply_app_event in the loop because
        // drain_app_events (called by Agent handling) would greedily
        // consume ShellComplete before this loop can observe it.
        let complete_ev = loop {
            match tokio::time::timeout(
                std::time::Duration::from_secs(5),
                app.runtime.recv_app_event(),
            )
            .await
            {
                Ok(Some(ev @ AppEvent::ShellComplete { .. })) => break ev,
                Ok(Some(_)) => continue,
                Ok(None) => panic!("shell channel closed unexpectedly"),
                Err(_) => panic!("shell command timed out after 5s"),
            }
        };

        // Dispatch the ShellComplete and any remaining buffered events.
        app.apply_app_event(complete_ev);

        // After completion, the live entry should be removed and events
        // persisted in the session state.
        assert!(
            app.session.live_turn.tool_entries.is_empty(),
            "shell live entry should be removed after completion"
        );

        // Shell events should be persisted in the event log with include_in_llm=false.
        let events = app
            .session
            .session_state
            .as_ref()
            .expect("session state")
            .events();
        let has_tool_call = events.iter().any(|e| {
            matches!(e, SessionEvent::ToolCall { name, include_in_llm, .. }
                if name == "local_shell" && !include_in_llm)
        });
        let has_tool_result = events.iter().any(|e| {
            matches!(e, SessionEvent::ToolResult { name, include_in_llm, .. }
                if name == "local_shell" && !include_in_llm)
        });
        assert!(has_tool_call, "shell tool call must be persisted");
        assert!(has_tool_result, "shell tool result must be persisted");

        // Shell events must NOT appear in the LLM projection.
        let llm = app
            .session
            .session_state
            .as_mut()
            .expect("session state")
            .llm_messages()
            .to_vec();
        assert!(llm.is_empty(), "shell output must not enter LLM history");
    }

    #[tokio::test]
    async fn shell_command_persists_in_fresh_session_without_prior_chat() {
        // In a fresh session with no session_state yet, shell commands should
        // still persist via ensure_event_log_for_submit.
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut app = make_app();
        app.session.current_cwd = tmp.path().to_string_lossy().to_string();
        // Don't set session_state — simulate fresh session.
        assert!(
            app.session.session_state.is_none(),
            "fresh session should have no session_state"
        );

        #[cfg(windows)]
        app.shell.textarea.insert_str("Write-Output fresh");
        #[cfg(not(windows))]
        app.shell.textarea.insert_str("printf 'fresh'");

        app.submit_shell_command();

        // Wait for completion.
        loop {
            match tokio::time::timeout(
                std::time::Duration::from_secs(5),
                app.runtime.recv_app_event(),
            )
            .await
            {
                Ok(Some(ev @ AppEvent::ShellComplete { .. })) => {
                    app.apply_app_event(ev);
                    break;
                }
                Ok(Some(_)) => continue,
                Ok(None) => panic!("channel closed"),
                Err(_) => panic!("timed out"),
            }
        }

        // Session state should now exist and contain the shell events.
        let ss = app
            .session
            .session_state
            .as_ref()
            .expect("session state should exist after shell command");
        let events = ss.events();
        assert!(
            events.iter().any(|e| matches!(e, SessionEvent::ToolCall {
                name, ..
            } if name == "local_shell")),
            "shell tool call must be persisted in fresh session"
        );
        assert!(
            events.iter().any(|e| matches!(e, SessionEvent::ToolResult {
                name, ..
            } if name == "local_shell")),
            "shell tool result must be persisted in fresh session"
        );
    }

    #[test]
    fn finalise_assistant_turn_event_uses_live_turn_state_fields() {
        let mut app = make_app();
        app.session.live_turn.assistant_content = "answer".to_string();
        app.session.live_turn.assistant_thinking = Some("thinking".to_string());
        app.session.live_turn.assistant_phase = crate::llm::AssistantPhase::Provisional;
        app.latest_usage = Some(crate::llm::UsageStats {
            input_tokens: Some(1),
            output_tokens: Some(2),
            total_tokens: Some(3),
            cached_tokens: None,
        });

        app.finalise_assistant_turn_event();

        let ev = app
            .session
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
                cached_tokens: None,
            })
        );
    }

    #[test]
    fn live_overlay_does_not_mutate_committed_history() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("session.jsonl");
        let mut app = make_app();
        app.session.session_state = Some(crate::session_state::SessionState::from_event_log(
            crate::event_log::EventLog::load(&path).expect("load event log"),
        ));
        app.append_event_immediate(crate::session_event::SessionEvent::UserMessage {
            content: "committed".to_string(),
            timestamp: 1,
        });

        let committed_before = app
            .session
            .session_state
            .as_ref()
            .expect("session state")
            .display_messages()
            .to_vec();

        app.session.live_turn.assistant_content = "live".to_string();
        app.session
            .live_turn
            .notices
            .push(Message::assistant("[notice]"));

        let committed_after = app
            .session
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

    // ── Step-back navigation ──────────────────────────────────────────────────

    fn make_app_with_events(events: Vec<crate::session_event::SessionEvent>) -> App {
        let mut app = make_app();
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("session.jsonl");
        let mut log = crate::event_log::EventLog::load(&path).expect("load");
        log.append_batch(&events).expect("append");
        app.session.session_state = Some(crate::session_state::SessionState::from_event_log(log));
        // Keep tempdir alive inside app so the path is valid for the test duration.
        // We don't need the store for these unit tests.
        app
    }

    fn ts() -> u64 {
        1_713_000_000
    }

    fn user_ev(content: &str) -> crate::session_event::SessionEvent {
        crate::session_event::SessionEvent::UserMessage {
            content: content.to_string(),
            timestamp: ts(),
        }
    }

    fn assistant_ev(content: &str) -> crate::session_event::SessionEvent {
        crate::session_event::SessionEvent::AssistantMessage {
            content: content.to_string(),
            thinking: None,
            phase: crate::llm::AssistantPhase::Final,
            usage: None,
            timestamp: ts(),
        }
    }

    fn ask_user_result_ev(id: &str, answer: &str) -> crate::session_event::SessionEvent {
        crate::session_event::SessionEvent::ToolResult {
            id: id.to_string(),
            name: "ask_user".to_string(),
            content: answer.to_string(),
            is_error: false,
            display_range: None,
            include_in_llm: true,
            timestamp: ts(),
        }
    }

    fn ask_user_call_ev(question: &str) -> crate::session_event::SessionEvent {
        crate::session_event::SessionEvent::ToolCall {
            id: "ask_1".to_string(),
            name: "ask_user".to_string(),
            args: serde_json::json!({"question": question}),
            include_in_llm: true,
            timestamp: ts(),
        }
    }

    fn ask_user_call_with_options_ev(
        question: &str,
        options: &[&str],
    ) -> crate::session_event::SessionEvent {
        let opts: Vec<serde_json::Value> = options
            .iter()
            .map(|o| serde_json::Value::String(o.to_string()))
            .collect();
        crate::session_event::SessionEvent::ToolCall {
            id: "ask_2".to_string(),
            name: "ask_user".to_string(),
            args: serde_json::json!({
                "question": question,
                "options": opts,
                "allowFreeform": true,
            }),
            include_in_llm: true,
            timestamp: ts(),
        }
    }

    fn other_tool_result_ev() -> crate::session_event::SessionEvent {
        crate::session_event::SessionEvent::ToolResult {
            id: "t1".to_string(),
            name: "bash".to_string(),
            content: "output".to_string(),
            is_error: false,
            display_range: None,
            include_in_llm: true,
            timestamp: ts(),
        }
    }

    #[test]
    fn step_boundaries_empty() {
        let app = make_app();
        assert!(app.step_boundaries().is_empty());
    }

    #[test]
    fn step_boundaries_user_messages() {
        let app = make_app_with_events(vec![
            user_ev("first"),
            assistant_ev("reply1"),
            user_ev("second"),
            assistant_ev("reply2"),
        ]);
        assert_eq!(app.step_boundaries(), vec![0, 2]);
    }

    #[test]
    fn step_boundaries_includes_ask_user_results() {
        let app = make_app_with_events(vec![
            user_ev("do it"),
            assistant_ev(""),
            other_tool_result_ev(), // non-ask_user tool result should be ignored
            ask_user_call_ev("which?"),
            ask_user_result_ev("ask_1", "my answer"),
            assistant_ev("done"),
            user_ev("next"),
        ]);
        // Boundaries: UserMessage at 0, ask_user ToolResult at 4, UserMessage at 6
        // The other_tool_result at 2 should NOT be a boundary.
        assert_eq!(app.step_boundaries(), vec![0, 4, 6]);
    }

    #[test]
    fn step_back_restores_ask_user_ui_with_options() {
        let mut app = make_app_with_events(vec![
            user_ev("do it"),
            assistant_ev(""),
            ask_user_call_with_options_ev("which?", &["Option A", "Option B"]),
            ask_user_result_ev("ask_2", "my answer"),
            assistant_ev("done"),
            user_ev("next"),
        ]);
        app.textarea = ratatui_textarea::TextArea::new(vec!["current input".to_string()]);

        // Step back to the last boundary (last UserMessage at index 5), then
        // again to the ask_user ToolResult at index 3.
        app.step_back();
        assert_eq!(app.step_back.cursor, Some(5));
        app.step_back();
        assert_eq!(app.step_back.cursor, Some(3));
        assert!(app.selection.active);
        assert!(app.has_pending_ask());
        assert!(
            app.ask_user.reply.is_none(),
            "no reply channel in step mode"
        );
    }

    #[test]
    fn step_back_restores_ask_user_freeform_when_no_options() {
        let mut app = make_app_with_events(vec![
            user_ev("do it"),
            assistant_ev(""),
            ask_user_call_ev("what do you think?"),
            ask_user_result_ev("ask_1", "my answer"),
            assistant_ev("done"),
            user_ev("next"),
        ]);
        app.textarea = ratatui_textarea::TextArea::new(vec!["current input".to_string()]);

        // Step back past the last UserMessage to the ask_user ToolResult.
        app.step_back();
        assert_eq!(app.step_back.cursor, Some(5));
        app.step_back();
        assert_eq!(app.step_back.cursor, Some(3));
        // No options → freeform-only mode; selection is inactive.
        assert!(!app.selection.active);
        assert!(app.has_pending_ask());
        assert!(app.ask_user_freeform_mode());
        assert!(
            app.ask_user.reply.is_none(),
            "no reply channel in step mode"
        );
    }

    #[test]
    fn step_back_saves_input_and_repopulates() {
        let mut app = make_app_with_events(vec![
            user_ev("first"),
            assistant_ev("reply1"),
            user_ev("second"),
            assistant_ev("reply2"),
        ]);
        app.textarea = ratatui_textarea::TextArea::new(vec!["current input".to_string()]);

        app.step_back();

        assert_eq!(app.step_back.saved_input.as_deref(), Some("current input"));
        assert_eq!(app.step_back.cursor, Some(2));
        assert_eq!(app.textarea.lines().join(""), "second");
    }

    #[test]
    fn step_back_twice_reaches_first_boundary() {
        let mut app = make_app_with_events(vec![
            user_ev("first"),
            assistant_ev("reply1"),
            user_ev("second"),
            assistant_ev("reply2"),
        ]);
        app.step_back();
        app.step_back();

        assert_eq!(app.step_back.cursor, Some(0));
        assert_eq!(app.textarea.lines().join(""), "first");
    }

    #[test]
    fn step_back_noop_at_earliest_boundary() {
        let mut app = make_app_with_events(vec![user_ev("first"), assistant_ev("reply1")]);
        app.step_back();
        app.step_back(); // Should not go further

        assert_eq!(app.step_back.cursor, Some(0));
    }

    #[test]
    fn step_forward_restores_and_clears_at_end() {
        let mut app = make_app_with_events(vec![
            user_ev("first"),
            assistant_ev("reply1"),
            user_ev("second"),
            assistant_ev("reply2"),
        ]);
        app.textarea = ratatui_textarea::TextArea::new(vec!["current input".to_string()]);

        app.step_back(); // cursor -> 2
        app.step_back(); // cursor -> 0
        app.step_forward(); // cursor -> 2
        assert_eq!(app.step_back.cursor, Some(2));
        assert_eq!(app.textarea.lines().join(""), "second");

        app.step_forward(); // past end -> clear
        assert!(app.step_back.cursor.is_none());
        assert!(app.step_back.saved_input.is_none());
        assert_eq!(app.textarea.lines().join(""), "current input");
    }

    #[test]
    fn cancel_stepping_restores_input() {
        let mut app = make_app_with_events(vec![user_ev("first"), assistant_ev("reply1")]);
        app.textarea = ratatui_textarea::TextArea::new(vec!["my draft".to_string()]);

        app.step_back();
        assert_eq!(app.step_back.cursor, Some(0));

        app.cancel_stepping();
        assert!(app.step_back.cursor.is_none());
        assert_eq!(app.textarea.lines().join(""), "my draft");
    }

    #[tokio::test]
    async fn step_back_noop_when_runtime_running() {
        let mut app = make_app_with_events(vec![user_ev("first")]);
        install_test_agent_task(&mut app);

        app.step_back();
        assert!(app.step_back.cursor.is_none());
    }

    // ── trim_incomplete_turn / commit_step_branch ask_user fix ─────────────

    #[test]
    fn trim_incomplete_turn_removes_trailing_unpaired_tool_call_and_assistant() {
        let mut events = vec![
            user_ev("hello"),
            assistant_ev("hi"),
            // Incomplete turn: AssistantMessage + unpaired ToolCall.
            assistant_ev("which one?"),
            ask_user_call_ev("pick"),
        ];
        super::trim_incomplete_turn(&mut events);
        // The incomplete turn (asst + unpaired tool call) must be stripped.
        assert_eq!(events.len(), 2);
        assert!(matches!(
            events[0],
            crate::session_event::SessionEvent::UserMessage { .. }
        ));
        assert!(matches!(
            events[1],
            crate::session_event::SessionEvent::AssistantMessage { .. }
        ));
    }

    #[test]
    fn trim_incomplete_turn_preserves_paired_tool_calls() {
        let mut events = vec![
            user_ev("run"),
            assistant_ev(""),
            crate::session_event::SessionEvent::ToolCall {
                id: "t1".to_string(),
                name: "bash".to_string(),
                args: serde_json::json!({"command": "ls"}),
                include_in_llm: true,
                timestamp: ts(),
            },
            crate::session_event::SessionEvent::ToolResult {
                id: "t1".to_string(),
                name: "bash".to_string(),
                content: "ok".to_string(),
                is_error: false,
                display_range: None,
                include_in_llm: true,
                timestamp: ts(),
            },
        ];
        let expected_len = events.len();
        super::trim_incomplete_turn(&mut events);
        // All tool calls have results — nothing should be removed.
        assert_eq!(events.len(), expected_len);
        assert!(events.iter().any(|e| {
            matches!(e, crate::session_event::SessionEvent::ToolCall { id, name, .. } if id == "t1" && name == "bash")
        }));
        assert!(events.iter().any(|e| {
            matches!(e, crate::session_event::SessionEvent::ToolResult { id, name, .. } if id == "t1" && name == "bash")
        }));
    }

    #[test]
    fn trim_incomplete_turn_noop_when_clean() {
        let mut events = vec![
            user_ev("a"),
            assistant_ev("b"),
            user_ev("c"),
            assistant_ev("d"),
        ];
        let expected_len = events.len();
        super::trim_incomplete_turn(&mut events);
        assert_eq!(events.len(), expected_len);
    }

    #[test]
    fn trim_incomplete_turn_empty_slice() {
        let mut events: Vec<crate::session_event::SessionEvent> = vec![];
        super::trim_incomplete_turn(&mut events);
        assert!(events.is_empty());
    }

    #[test]
    fn commit_step_branch_strips_incomplete_ask_user_turn() {
        // Simulate stepping back to an ask_user ToolResult and committing a
        // branch.  The resulting session must not contain unpaired ToolCalls.
        let tmp = tempfile::tempdir().expect("tempdir");
        let sessions_dir = tmp.path().join("sessions");
        let store = crate::session::SessionStore::open_at(sessions_dir).expect("open store");

        let mut app = make_app_with_events(vec![
            user_ev("start"),
            assistant_ev("thinking…"),
            // Completed tool call from the same turn (not ask_user).
            crate::session_event::SessionEvent::ToolCall {
                id: "bash1".to_string(),
                name: "bash".to_string(),
                args: serde_json::json!({"command": "ls"}),
                include_in_llm: true,
                timestamp: ts(),
            },
            crate::session_event::SessionEvent::ToolResult {
                id: "bash1".to_string(),
                name: "bash".to_string(),
                content: "output".to_string(),
                is_error: false,
                display_range: None,
                include_in_llm: true,
                timestamp: ts(),
            },
            // The ask_user turn:
            assistant_ev("which option?"),
            ask_user_call_ev("pick one"),
            ask_user_result_ev("ask_1", "old answer"),
            // More events after (should not be in branch):
            assistant_ev("thanks"),
            user_ev("next"),
        ]);
        app.session.session_store = Some(store);
        app.session.current_cwd = tmp.path().to_string_lossy().to_string();

        // Step back to the ask_user ToolResult boundary (index 6).
        // Boundaries: UserMsg(0), ask_user ToolResult(6), UserMsg(8)
        app.step_back(); // -> last UserMessage (8)
        app.step_back(); // -> ask_user ToolResult (6)
        assert_eq!(app.step_back.cursor, Some(6));

        // Set a new answer and commit the branch.
        app.textarea = ratatui_textarea::TextArea::new(vec!["new answer".to_string()]);
        let committed = app.commit_step_branch();
        assert!(committed.is_some());

        // commit_step_branch creates a new session from trimmed events.
        // append_user_message is called separately by finish_pending_ask.
        let ss = app.session.session_state.as_ref().unwrap();
        // After trimming: user(0), asst(1), bash_call(2), bash_result(3) = 4 events.
        assert_eq!(ss.events().len(), 4);

        // Verify no unpaired ToolCalls remain.
        for (i, ev) in ss.events().iter().enumerate() {
            if let crate::session_event::SessionEvent::ToolCall { id, .. } = ev {
                let has_result = ss.events()[i + 1..].iter().any(|e| {
                    matches!(e, crate::session_event::SessionEvent::ToolResult { id: rid, .. } if rid == id)
                });
                assert!(
                    has_result,
                    "ToolCall at index {i} has no matching ToolResult"
                );
            }
        }
    }

    #[test]
    fn trim_incomplete_turn_only_strips_last_incomplete_turn() {
        // A preceding incomplete turn should be left alone if it's not at the tail.
        let mut events = vec![
            user_ev("first"),
            assistant_ev("which?"),
            ask_user_call_ev("first q"),
            ask_user_result_ev("ask_1", "answer"), // paired — complete
            // Second ask_user turn — incomplete at tail (different call id).
            assistant_ev("another?"),
            crate::session_event::SessionEvent::ToolCall {
                id: "ask_2".to_string(),
                name: "ask_user".to_string(),
                args: serde_json::json!({"question": "second q"}),
                include_in_llm: true,
                timestamp: ts(),
            },
        ];
        super::trim_incomplete_turn(&mut events);
        // Only the second (trailing) incomplete turn is removed (asst at 4 + call at 5).
        assert_eq!(events.len(), 4);
        // Verify the first ask_user turn is intact.
        let has_ask_1_call = events.iter().any(|e| {
            matches!(e, crate::session_event::SessionEvent::ToolCall { id, name, .. } if id == "ask_1" && name == "ask_user")
        });
        assert!(has_ask_1_call, "first ask_user turn should be preserved");
        let has_ask_1_result = events.iter().any(|e| {
            matches!(e, crate::session_event::SessionEvent::ToolResult { id, name, .. } if id == "ask_1" && name == "ask_user")
        });
        assert!(
            has_ask_1_result,
            "first ask_user result should be preserved"
        );
        // The second incomplete turn must not be present.
        let has_second = events.iter().any(|e| {
            matches!(e, crate::session_event::SessionEvent::ToolCall { id, .. } if id == "ask_2")
        });
        assert!(
            !has_second,
            "second incomplete ask_user turn must be stripped"
        );
    }
}
