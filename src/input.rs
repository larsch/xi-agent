use std::sync::Arc;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::{
    app::{App, InputMode, SelectionResult},
    commands::CommandAction,
    config::XiConfig,
    llm::{LlmProvider, Message},
    provider::{ThinkingSupport, thinking_support_for_instance},
    provider_instance::{AuthMode, BackendPreset, EndpointBehavior, ProviderInstance},
    provider_manager::ProviderSetupStep,
    thinking::ThinkingLevel,
};

// ── Result types ─────────────────────────────────────────────────────────────

pub(crate) enum RunResult {
    Quit,
    RebuildProvider,
    ReloadContext,
    ChangeModel {
        name: String,
        prompt_thinking_selection: bool,
    },
    ChangeProvider(String),
    AddProvider(ProviderInstance),
    UpdateProvider {
        original_id: Option<String>,
        instance: ProviderInstance,
    },
    RemoveProvider(String),
    ChangeThinking(ThinkingLevel),
    /// Switch to (or stay on) a specific provider instance with optional new base URL and API key.
    ConfigureProvider {
        instance: ProviderInstance,
        url: Option<String>,
        api_key: Option<String>,
    },
}

pub(crate) enum KeyDispatch {
    NotHandled,
    Continue,
    Return(RunResult),
}

// ── Windows-only paste heuristic ─────────────────────────────────────────────

/// Timing threshold (milliseconds) used to distinguish paste-injected Enter
/// events from real Enter keypresses on Windows terminals that don't support
/// bracketed paste.  Paste events arrive in sub-millisecond bursts while human
/// typing always has gaps >20 ms.
///
/// Set to `Some(10)` to enable the heuristic, or `None` to disable it.
/// `ENABLE_VIRTUAL_TERMINAL_INPUT` has no effect on crossterm's Windows event
/// source because it uses `ReadConsoleInputW` (raw INPUT_RECORDs), not
/// `ReadFile`/`ReadConsole` (VT sequences).  The timing heuristic is therefore
/// the correct fallback for terminals that deliver paste as individual key events.
#[cfg(windows)]
const PASTE_ENTER_THRESHOLD_MS: Option<u128> = Some(10);

// ── Clipboard (Windows only) ─────────────────────────────────────────────────

/// Read text from the system clipboard, or return `None` on error.
///
/// On Windows, terminals that do not support bracketed paste (e.g. conhost)
/// deliver paste events as individual key records — including `VK_RETURN` for
/// every newline — so `Event::Paste` is never emitted and newlines in pasted
/// text inadvertently submit the input.  Reading the clipboard directly and
/// calling `insert_str` sidesteps the Win32 console input path entirely.
#[cfg(windows)]
fn read_clipboard_text() -> Option<String> {
    arboard::Clipboard::new()
        .ok()
        .and_then(|mut cb| cb.get_text().ok())
}

// ── Helper functions ──────────────────────────────────────────────────────────

pub(crate) fn normalize_paste_text(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn provider_setup_requires_endpoint(instance: &ProviderInstance) -> bool {
    matches!(
        instance.backend_preset.def().endpoint_behavior,
        EndpointBehavior::UserSupplied
    )
}

pub(crate) fn provider_setup_requires_api_key(instance: &ProviderInstance) -> bool {
    instance.backend_preset.def().auth_mode == AuthMode::ApiKey
}

fn enter_provider_endpoint_input(app: &mut App, _instance: &ProviderInstance) {
    app.enter_provider_endpoint_input_mode();
}

fn resolve_current_run_instance(app: &App, config: &XiConfig) -> ProviderInstance {
    config
        .find_provider(&app.provider.current_instance.id)
        .cloned()
        .unwrap_or_else(|| resolve_default_provider_instance(config))
}

fn resolve_default_provider_instance(config: &XiConfig) -> ProviderInstance {
    if let Some(ref id) = config.provider
        && let Some(inst) = config.find_provider(id)
    {
        return inst.clone();
    }

    config
        .providers
        .first()
        .cloned()
        .unwrap_or_else(|| ProviderInstance::new("copilot", BackendPreset::Copilot))
}

fn thinking_supported_for_current_provider(app: &App, config: &XiConfig) -> bool {
    config
        .find_provider(&app.provider.current_instance.id)
        .map(|inst| {
            thinking_support_for_instance(inst, &app.provider.current_model)
                == ThinkingSupport::Applied
        })
        .unwrap_or(false)
}

pub(crate) fn cancel_ask_freeform_if_off_sentinel(app: &mut App) {
    if app.ask_user_freeform_mode() {
        let on_sentinel = app
            .selection
            .items
            .get(app.selection.selected)
            .map(|i| i.complete_to == "/ask_user_freeform")
            .unwrap_or(false);
        if !on_sentinel {
            app.cancel_ask_freeform_typing();
        }
    }
}

pub(crate) fn apply_paste(
    app: &mut App,
    provider: &Arc<dyn LlmProvider + Send + Sync>,
    text: &str,
) {
    let normalized = normalize_paste_text(text);
    if app.selection.active {
        app.exit_selection_mode();
    }
    if app.input_mode == InputMode::Shell {
        app.shell.textarea.insert_str(normalized);
    } else {
        app.textarea.insert_str(normalized);
        app.update_completions();
        if app.should_fetch_models() {
            app.start_model_fetch(provider);
        }
    }
}

// ── Key dispatch ──────────────────────────────────────────────────────────────

pub(crate) fn handle_key_event(
    app: &mut App,
    provider: &Arc<dyn LlmProvider + Send + Sync>,
    config: &XiConfig,
    key: KeyEvent,
    #[cfg(windows)] last_key_at: &mut Option<std::time::Instant>,
) -> Option<RunResult> {
    // On Windows with keyboard enhancement flags enabled,
    // Crossterm can emit both Press and Release key events.
    // Ignore Release so one shortcut maps to one action.
    if key.kind == KeyEventKind::Release {
        return None;
    }

    // Record the time of every non-Enter key press so we
    // can detect paste-injected Enter events below.
    #[cfg(windows)]
    if key.code != KeyCode::Enter {
        *last_key_at = Some(std::time::Instant::now());
    }

    match handle_global_key_shortcuts(
        app,
        provider,
        key,
        #[cfg(windows)]
        last_key_at,
    ) {
        KeyDispatch::NotHandled => {}
        KeyDispatch::Continue => return None,
        KeyDispatch::Return(result) => return Some(result),
    }

    if app.input_mode == InputMode::Shell {
        return match handle_shell_mode_key(
            app,
            key,
            #[cfg(windows)]
            last_key_at.as_ref(),
        ) {
            KeyDispatch::NotHandled | KeyDispatch::Continue => None,
            KeyDispatch::Return(result) => Some(result),
        };
    }

    if app.selection.active {
        return match handle_selection_mode_key(app, config, key) {
            KeyDispatch::NotHandled | KeyDispatch::Continue => None,
            KeyDispatch::Return(result) => Some(result),
        };
    }

    match handle_chat_mode_key(
        app,
        provider,
        config,
        key,
        #[cfg(windows)]
        last_key_at.as_ref(),
    ) {
        KeyDispatch::NotHandled | KeyDispatch::Continue => None,
        KeyDispatch::Return(result) => Some(result),
    }
}

fn handle_global_key_shortcuts(
    app: &mut App,
    _provider: &Arc<dyn LlmProvider + Send + Sync>,
    key: KeyEvent,
    #[cfg(windows)] _last_key_at: &mut Option<std::time::Instant>,
) -> KeyDispatch {
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        if app.input_mode == InputMode::Shell {
            app.exit_shell_mode();
            return KeyDispatch::Continue;
        }
        return KeyDispatch::Return(RunResult::Quit);
    }

    if key.code == KeyCode::Char('d') && key.modifiers.contains(KeyModifiers::CONTROL) {
        if app.input_mode == InputMode::Shell {
            if app.shell_input_is_empty() {
                app.exit_shell_mode();
            }
            return KeyDispatch::Continue;
        }

        let input_is_empty = app
            .textarea
            .lines()
            .iter()
            .all(|line| line.trim().is_empty());
        if input_is_empty {
            return KeyDispatch::Return(RunResult::Quit);
        }
        return KeyDispatch::Continue;
    }

    if key.code == KeyCode::Char('i') && key.modifiers.contains(KeyModifiers::CONTROL) {
        app.toggle_info();
        return KeyDispatch::Continue;
    }

    if key.code == KeyCode::Char('f') && key.modifiers.contains(KeyModifiers::CONTROL) {
        app.log_view.toggle_full_output();
        return KeyDispatch::Continue;
    }

    if key.code == KeyCode::Char('r')
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && !app.selection.active
    {
        app.resume_latest_for_current_cwd();
        return KeyDispatch::Continue;
    }

    #[cfg(windows)]
    if key.code == KeyCode::Char('v')
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && !app.login.active
    {
        if let Some(text) = read_clipboard_text() {
            apply_paste(app, _provider, &text);
        }
        return KeyDispatch::Continue;
    }

    KeyDispatch::NotHandled
}

fn handle_shell_mode_key(
    app: &mut App,
    key: KeyEvent,
    #[cfg(windows)] last_key_at: Option<&std::time::Instant>,
) -> KeyDispatch {
    if key.code == KeyCode::Char('s') && key.modifiers.contains(KeyModifiers::CONTROL) {
        app.cycle_shell();
        return KeyDispatch::Continue;
    }

    match key.code {
        KeyCode::Esc => app.exit_shell_mode(),
        KeyCode::Backspace if app.shell_input_is_empty() => app.exit_shell_mode(),
        KeyCode::Enter if key.modifiers.is_empty() => {
            #[cfg(windows)]
            if let (Some(threshold), Some(t)) = (PASTE_ENTER_THRESHOLD_MS, last_key_at)
                && t.elapsed().as_millis() < threshold
            {
                app.shell.textarea.insert_newline();
                return KeyDispatch::Continue;
            }
            app.submit_shell_command();
        }
        _ => {
            app.shell.textarea.input(Event::Key(key));
        }
    }

    KeyDispatch::Continue
}

fn handle_selection_mode_key(app: &mut App, config: &XiConfig, key: KeyEvent) -> KeyDispatch {
    match key.code {
        KeyCode::Up => {
            app.selection_select_prev();
            cancel_ask_freeform_if_off_sentinel(app);
        }
        KeyCode::Down => {
            app.selection_select_next();
            cancel_ask_freeform_if_off_sentinel(app);
        }
        KeyCode::PageDown => app.selection_page_down(),
        KeyCode::PageUp => app.selection_page_up(),
        KeyCode::Backspace => {
            if app.ask_user_freeform_mode() {
                app.textarea.delete_char();
                if app.textarea.lines().iter().all(|l| l.is_empty()) {
                    app.cancel_ask_freeform_typing();
                }
            } else {
                app.selection_backspace();
            }
        }
        KeyCode::Char(c)
            if !key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT) =>
        {
            if app.pending_ask_allows_freeform() {
                app.begin_ask_freeform_typing();
                app.textarea.insert_char(c);
            } else {
                app.selection_add_char(c);
            }
        }
        KeyCode::Char('e')
            if key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT)
                && app.in_provider_selection_mode() =>
        {
            if let Some(id) = app.selected_provider_id()
                && let Some(instance) = config.find_provider(id)
                && instance.backend_preset.def().backend_class
                    == crate::provider_instance::BackendClass::UserSuppliedService
            {
                app.enter_provider_edit_mode(instance);
            }
        }
        KeyCode::Char('r')
            if key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT)
                && app.in_provider_selection_mode() =>
        {
            if let Some(id) = app.selected_provider_id()
                && let Some(instance) = config.find_provider(id)
                && instance.backend_preset.def().backend_class
                    == crate::provider_instance::BackendClass::UserSuppliedService
            {
                app.enter_provider_removal_confirmation_mode(instance);
            }
        }
        KeyCode::Enter if key.modifiers.is_empty() => {
            return handle_selection_enter(app);
        }
        KeyCode::Esc => {
            if app.has_pending_ask() {
                app.cancel_pending_ask();
            } else if app.in_slash_mode() {
                app.reset_textarea();
            } else if app.streaming() {
                app.abort_agent_loop();
            } else {
                if app.in_provider_removal_confirmation_mode() {
                    app.clear_pending_provider_removal();
                }
                app.exit_selection_mode();
            }
        }
        _ => {}
    }

    KeyDispatch::Continue
}

fn handle_selection_enter(app: &mut App) -> KeyDispatch {
    match app.apply_selection() {
        Some(SelectionResult::Model(m)) => KeyDispatch::Return(RunResult::ChangeModel {
            name: m,
            prompt_thinking_selection: true,
        }),
        Some(SelectionResult::Thinking(level)) => {
            KeyDispatch::Return(RunResult::ChangeThinking(level))
        }
        Some(SelectionResult::Provider(p)) => KeyDispatch::Return(RunResult::ChangeProvider(p)),
        Some(SelectionResult::AddProvider) => {
            app.begin_new_provider_setup();
            app.enter_provider_backend_preset_selection_mode();
            KeyDispatch::Continue
        }
        Some(SelectionResult::CancelProviderRemoval) => {
            app.clear_pending_provider_removal();
            KeyDispatch::Continue
        }
        Some(SelectionResult::RemoveProvider(id)) => {
            KeyDispatch::Return(RunResult::RemoveProvider(id))
        }
        Some(SelectionResult::ProviderBackendPreset(backend_preset)) => {
            let service_def = backend_preset.def();
            let default_api = service_def.default_api.clone();
            app.set_pending_provider_backend_preset(backend_preset.clone());
            if service_def.user_selects_api {
                app.enter_provider_api_type_selection_mode(&backend_preset);
            } else {
                app.set_pending_provider_api_type(default_api);
                if let Some(instance) = app.pending_provider_instance() {
                    if provider_setup_requires_endpoint(&instance) {
                        enter_provider_endpoint_input(app, &instance);
                    } else if provider_setup_requires_api_key(&instance) {
                        app.enter_provider_api_key_input_mode();
                    } else {
                        app.enter_provider_name_input_mode();
                    }
                }
            }
            KeyDispatch::Continue
        }
        Some(SelectionResult::ProviderApiType(api_type)) => {
            app.set_pending_provider_api_type(api_type);
            if let Some(instance) = app.pending_provider_instance() {
                if provider_setup_requires_endpoint(&instance) {
                    enter_provider_endpoint_input(app, &instance);
                } else if provider_setup_requires_api_key(&instance) {
                    app.enter_provider_api_key_input_mode();
                } else {
                    app.enter_provider_name_input_mode();
                }
            }
            KeyDispatch::Continue
        }
        Some(SelectionResult::LoginProvider(p)) => {
            app.start_login(&p);
            KeyDispatch::Continue
        }
        Some(SelectionResult::ResumeSession(id)) => {
            app.resume_session_by_id(&id);
            KeyDispatch::Continue
        }
        Some(SelectionResult::AskOption(answer)) => {
            app.select_pending_ask_option(answer);
            KeyDispatch::Continue
        }
        Some(SelectionResult::AskFreeform) => {
            if app.ask_user_freeform_mode() {
                app.submit_pending_ask_answer();
            } else {
                app.enter_ask_freeform_mode();
            }
            KeyDispatch::Continue
        }
        Some(SelectionResult::LoginAction(action)) => {
            app.apply_login_action(action);
            KeyDispatch::Continue
        }
        None => KeyDispatch::Continue,
    }
}

fn handle_chat_mode_key(
    app: &mut App,
    provider: &Arc<dyn LlmProvider + Send + Sync>,
    config: &XiConfig,
    key: KeyEvent,
    #[cfg(windows)] last_key_at: Option<&std::time::Instant>,
) -> KeyDispatch {
    if key.code == KeyCode::Esc {
        app.handle_escape_in_chat_mode();
        return KeyDispatch::Continue;
    }

    if app.login.active {
        if key.code == KeyCode::Enter && key.modifiers.is_empty() {
            app.enter_login_action_menu();
        }
        return KeyDispatch::Continue;
    }

    if key.code == KeyCode::Char('!')
        && (key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT)
    {
        let chat_input_is_empty = app
            .textarea
            .lines()
            .iter()
            .all(|line| line.trim().is_empty());
        if chat_input_is_empty {
            app.enter_shell_mode();
            return KeyDispatch::Continue;
        }
    }

    match key.code {
        KeyCode::PageUp => app.scroll_up(),
        KeyCode::PageDown => app.scroll_down(),
        KeyCode::Up => {
            if key.modifiers.contains(KeyModifiers::ALT) {
                app.step_back();
                return KeyDispatch::Continue;
            }
            if !app.completion.completions.is_empty() {
                app.completion_select_prev();
            } else {
                app.textarea.input(Event::Key(key));
            }
        }
        KeyCode::Down => {
            if key.modifiers.contains(KeyModifiers::ALT) {
                app.step_forward();
                return KeyDispatch::Continue;
            }
            if !app.completion.completions.is_empty() {
                app.completion_select_next();
            } else {
                app.textarea.input(Event::Key(key));
            }
        }
        KeyCode::Tab => {
            if !app.completion.completions.is_empty() {
                app.apply_completion();
            }
        }
        KeyCode::Enter if key.modifiers.is_empty() => {
            #[cfg(windows)]
            if let (Some(threshold), Some(t)) = (PASTE_ENTER_THRESHOLD_MS, last_key_at)
                && t.elapsed().as_millis() < threshold
            {
                app.textarea.insert_newline();
                app.update_completions();
                return KeyDispatch::Continue;
            }

            return handle_chat_submit(app, provider, config);
        }
        KeyCode::Enter if key.modifiers == KeyModifiers::SHIFT => {
            app.textarea.insert_newline();
            app.update_completions();
        }
        _ => {
            app.textarea.input(Event::Key(key));
            app.update_completions();
            if app.should_fetch_models() {
                app.start_model_fetch(provider);
            }
        }
    }

    KeyDispatch::Continue
}

fn handle_chat_submit(
    app: &mut App,
    provider: &Arc<dyn LlmProvider + Send + Sync>,
    config: &XiConfig,
) -> KeyDispatch {
    match app.provider.setup_step.clone() {
        ProviderSetupStep::Endpoint => {
            // Determine whether this is the two-step URL→token flow (e.g. OpenWebUI)
            // or a single-step URL entry (e.g. Ollama, generic endpoint).
            let needs_api_key = app
                .pending_provider_instance()
                .as_ref()
                .map(provider_setup_requires_api_key)
                .unwrap_or(false);

            let is_two_step = needs_api_key
                && app
                    .pending_provider_instance()
                    .as_ref()
                    .map(|i| {
                        matches!(
                            i.backend_preset.def().endpoint_behavior,
                            crate::provider_instance::EndpointBehavior::UserSupplied
                        )
                    })
                    .unwrap_or(false);

            if is_two_step {
                // Submit URL and transition to ApiKey step (carries pending_url).
                app.submit_open_webui_url_input();
                return KeyDispatch::Continue;
            }

            // Single-step: for Ollama use the Ollama-specific normalizer, otherwise generic.
            let url_opt = {
                let instance_opt = app.pending_provider_instance();
                let is_ollama = instance_opt
                    .as_ref()
                    .map(|i| i.backend_preset == crate::provider_instance::BackendPreset::Ollama)
                    .unwrap_or(false);
                if is_ollama {
                    app.take_ollama_endpoint_input()
                } else {
                    app.submit_pending_provider_base_url()
                }
            };

            if let Some(url) = url_opt {
                if app.pending_provider_setup_is_edit() {
                    let instance = app
                        .pending_provider_instance()
                        .unwrap_or_else(|| resolve_current_run_instance(app, config));
                    return KeyDispatch::Return(RunResult::ConfigureProvider {
                        instance,
                        url: Some(url),
                        api_key: None,
                    });
                }
                if let Some(setup) = app.provider.pending_setup.as_mut() {
                    setup.base_url = Some(url);
                }
                app.enter_provider_name_input_mode();
            }
            return KeyDispatch::Continue;
        }

        ProviderSetupStep::ApiKey { .. } => {
            if let Some((url, token)) = app.take_open_webui_token_input() {
                let instance = app
                    .pending_provider_instance()
                    .unwrap_or_else(|| resolve_current_run_instance(app, config));
                return KeyDispatch::Return(RunResult::ConfigureProvider {
                    instance,
                    url: Some(url),
                    api_key: Some(token),
                });
            }
            // Generic ApiKey step (edit flow without two-step URL→token).
            if app.submit_pending_provider_api_key().is_some() {
                app.enter_provider_name_input_mode();
            }
            return KeyDispatch::Continue;
        }

        ProviderSetupStep::Name => {
            let was_edit = app.pending_provider_setup_is_edit();
            let original_id = app.pending_provider_original_id().map(ToOwned::to_owned);
            if app.submit_provider_name_input(&config.providers).is_some()
                && let Some(instance) = app.finish_pending_provider_setup()
            {
                if was_edit {
                    return KeyDispatch::Return(RunResult::UpdateProvider {
                        original_id,
                        instance,
                    });
                }
                return KeyDispatch::Return(RunResult::AddProvider(instance));
            }
            return KeyDispatch::Continue;
        }

        ProviderSetupStep::Idle => {}
    }

    if app.has_pending_ask() {
        app.submit_pending_ask_answer();
        return KeyDispatch::Continue;
    }

    if app.in_slash_mode() {
        return handle_slash_submit(app, provider, config);
    }

    if app.is_stepping() {
        // Commit the branch: create a new session from the kept events and
        // the (possibly edited) message, then submit normally.
        if app.commit_step_branch().is_some() {
            app.submit(provider);
        }
        return KeyDispatch::Continue;
    }

    if app.streaming() {
        app.enqueue_steering_from_input();
    } else {
        app.submit(provider);
    }
    KeyDispatch::Continue
}

fn handle_slash_submit(
    app: &mut App,
    provider: &Arc<dyn LlmProvider + Send + Sync>,
    config: &XiConfig,
) -> KeyDispatch {
    let input = app.slash_submit_text().unwrap_or_default();
    app.reset_textarea();

    match crate::commands::parse(&input) {
        Some(CommandAction::New) => app.new_conversation(),
        Some(CommandAction::Export(path)) => app.export_session_html(path.as_deref()),
        Some(CommandAction::Reload) => return KeyDispatch::Return(RunResult::ReloadContext),
        Some(CommandAction::Quit) => return KeyDispatch::Return(RunResult::Quit),
        Some(CommandAction::Model(name)) => {
            return KeyDispatch::Return(RunResult::ChangeModel {
                name,
                prompt_thinking_selection: true,
            });
        }
        Some(CommandAction::ModelNoArg) => {
            app.enter_model_selection_mode();
            if app.should_fetch_models_for_selection() {
                app.start_model_fetch(provider);
            }
        }
        Some(CommandAction::Provider(name)) => {
            return KeyDispatch::Return(RunResult::ChangeProvider(name));
        }
        Some(CommandAction::ProviderNoArg) => {
            app.enter_provider_selection_mode(&config.providers);
        }
        Some(CommandAction::Thinking(raw)) => {
            if !thinking_supported_for_current_provider(app, config) {
                app.push_notice(Message::assistant(
                    "[thinking is not supported by the current provider/model]".to_string(),
                ));
                return KeyDispatch::Continue;
            }
            match ThinkingLevel::parse(&raw) {
                Some(level) => return KeyDispatch::Return(RunResult::ChangeThinking(level)),
                None => {
                    app.push_notice(Message::assistant(format!(
                        "[invalid thinking level: '{raw}' (use off|minimal|low|medium|high|xhigh)]"
                    )));
                }
            }
        }
        Some(CommandAction::ThinkingNoArg)
            if thinking_supported_for_current_provider(app, config) =>
        {
            app.enter_thinking_selection_mode();
        }
        Some(CommandAction::ThinkingNoArg) => {}
        Some(CommandAction::Login(provider_name)) => {
            app.start_login(&provider_name);
        }
        Some(CommandAction::LoginNoArg) => {
            app.enter_login_selection_mode();
        }
        Some(CommandAction::Resume(session_id)) => {
            app.resume_session_by_id(&session_id);
        }
        Some(CommandAction::ResumeNoArg) => {
            app.enter_resume_selection_mode();
        }
        Some(CommandAction::Compact(instructions)) => {
            app.trigger_manual_compaction(instructions, provider);
        }
        Some(CommandAction::Skill { name, args }) => {
            match app.loaded_skills.iter().find(|s| s.name == name) {
                Some(skill) => match crate::skills::expand_skill(skill, &args) {
                    Ok(expanded) => {
                        app.submit_with_text(expanded, provider);
                    }
                    Err(e) => {
                        app.push_notice(Message::assistant(format!("[skill error: {e}]")));
                    }
                },
                None => {
                    app.push_notice(Message::assistant(format!("[unknown skill: '{name}']")));
                }
            }
        }
        None => {}
    }

    KeyDispatch::Continue
}
