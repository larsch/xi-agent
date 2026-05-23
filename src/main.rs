use clap::Parser;
use crossterm::{
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
        KeyboardEnhancementFlags, MouseEventKind, PopKeyboardEnhancementFlags,
        PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{
        BeginSynchronizedUpdate, EndSynchronizedUpdate, EnterAlternateScreen, LeaveAlternateScreen,
        SetTitle, disable_raw_mode, enable_raw_mode,
    },
};
use futures_util::StreamExt;
use ratatui::{Terminal, backend::CrosstermBackend};
use std::{
    io,
    io::ErrorKind,
    sync::{Arc, Mutex},
};

mod agent;
mod agent_runtime;
mod app;
mod app_agent_handlers;
mod app_event;
mod app_interaction;
mod app_submission;
mod ask_user_state;
mod auth;
mod commands;
mod completion;
mod completion_state;
mod config;
mod debug_log;
mod dirs;
mod event_log;
mod export;
mod live_turn;
mod llm;
mod log_view_state;
mod login_state;
mod markdown;
mod process;
mod projection;
mod provider;
mod provider_instance;
mod provider_manager;
mod selection_state;
mod session;
mod session_event;
mod session_manager;
mod session_state;
mod shell;
mod shell_state;
mod skills;
mod thinking;
mod tool_presentation;
mod ui;

use agent::tools::custom::custom_tool_dirs;
use agent::{
    AgentEvent, AgentLoopConfig, FileTracker, ToolOutputLog, build_system_prompt,
    tools::{custom::load_custom_tools, register_builtin_tools},
};
use app::{App, InputMode, SelectionResult};
use app_event::AppEvent;
use commands::CommandAction;
use config::TauConfig;
use llm::{LlmEvent, LlmProvider, LlmStream, Message, ModelListFuture};
use provider::{ThinkingSupport, build_provider_for_instance, thinking_support_for_instance};
use provider_instance::{AuthMode, EndpointBehavior, ProviderInstance};
use provider_manager::format_provider_error_for_display;
use provider_manager::{PendingProviderSetup, ProviderSetupStep};
use thinking::ThinkingLevel;

// ── CLI definition ────────────────────────────────────────────────────────────

/// tau — a terminal-based AI coding agent
#[derive(Debug, Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// LLM provider to use (must match a configured provider instance id).
    #[arg(long, short = 'P', value_name = "PROVIDER")]
    provider: Option<String>,

    /// Model name to use (e.g. gpt-4o, llama3.1).
    #[arg(long, short = 'm', value_name = "MODEL")]
    model: Option<String>,

    /// Run in non-interactive mode: send PROMPT, stream the response to
    /// stdout, and exit.  Accepts multiple words without shell quoting.
    #[arg(long, short = 'p', value_name = "PROMPT", num_args = 1..)]
    print: Option<Vec<String>>,

    /// Print the file-system paths tau uses and exit.
    #[arg(long)]
    print_dirs: bool,
}

// ── Entry point ───────────────────────────────────────────────────────────────

/// Build a [`FileTracker`] pre-configured to ignore tau's own generated files:
///
/// - Session files (data dir `sessions/` subtree).
/// - Debug logs (cache dir).
/// - Instruction files named `AGENTS.md` or `SKILL.md` (matched by filename).
fn build_file_tracker() -> FileTracker {
    let excluded_prefixes: Vec<std::path::PathBuf> = dirs::PROJECT_DIRS
        .as_ref()
        .map(|d| vec![d.data_dir().join("sessions"), d.cache_dir().to_path_buf()])
        .unwrap_or_default();

    FileTracker::with_exclusions(excluded_prefixes, &["AGENTS.md", "SKILL.md"])
}

#[tokio::main]
async fn main() -> io::Result<()> {
    debug_log::init_logging();

    let cli = Cli::parse();

    if cli.print_dirs {
        dirs::print_dirs();
        return Ok(());
    }

    let mut config = match TauConfig::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("warning: failed to load config.toml: {e}");
            TauConfig::default()
        }
    };

    // ── Non-interactive (--print / -p) mode ───────────────────────────────────
    if let Some(words) = cli.print {
        let prompt = words.join(" ");
        let provider_override = cli.provider.as_deref().ok_or_else(|| {
            io::Error::new(
                ErrorKind::InvalidInput,
                "--print requires --provider <name>",
            )
        })?;
        return run_print_mode(prompt, provider_override, cli.model.as_deref(), &config).await;
    }

    // Priority: --provider flag > config.toml > default.
    let initial_instance = resolve_provider_instance(cli.provider.as_deref(), &config)
        .map_err(|e| io::Error::new(ErrorKind::InvalidInput, e))?;

    // Priority: --model flag > config.toml > provider default.
    let initial_model = resolve_model_for_instance(cli.model.as_deref(), &initial_instance);
    let initial_thinking = resolve_thinking_level_for_model(&config, &initial_model);
    let window_folder = std::env::current_dir()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| ".".to_string());
    let window_title = format!("𝜏 - {window_folder}");

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        SetTitle(&window_title),
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )?;

    let mut keyboard_enhancements_enabled = false;
    match execute!(
        stdout,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES),
    ) {
        Ok(()) => keyboard_enhancements_enabled = true,
        Err(e) if e.kind() == ErrorKind::Unsupported => {
            log::debug!(
                "keyboard progressive enhancement unsupported on this terminal; continuing without it"
            );
        }
        Err(e) => return Err(e),
    }

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let file_tracker = Arc::new(Mutex::new(build_file_tracker()));
    let tool_output_log = Arc::new(std::sync::Mutex::new(ToolOutputLog::new("init")));

    let mut app = App::new(
        initial_instance,
        &initial_model,
        initial_thinking,
        AgentLoopConfig {
            tools: std::collections::HashMap::new(),
            file_tracker: Arc::clone(&file_tracker),
            tool_output_log: Arc::clone(&tool_output_log),
            session_events: vec![],
            current_model: initial_model.clone(),
            auto_compaction_enabled: true,
            manual_compaction_instructions: None,
            executor: std::sync::Arc::new(crate::agent::DefaultToolExecutor::new()),
            system_prompt: None,
        },
    );

    let app_event_tx = app.app_event_tx();
    let custom_tools = load_custom_tools(&custom_tool_dirs());
    let tools = register_builtin_tools(
        Some(app_event_tx.clone()),
        Arc::clone(&file_tracker),
        custom_tools,
    )
    .await;
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".to_string());
    app.init_session_persistence(cwd.clone());
    let loaded_skills = skills::load_skills();
    let system_prompt = build_system_prompt(&tools, &cwd, &loaded_skills);
    app.agent_config.tools = tools;
    app.system_prompt = Some(system_prompt);
    app.loaded_skills = loaded_skills;
    app.provider.instances = config.providers.clone();
    maybe_warn_thinking_unsupported(&mut app);

    loop {
        // Build (or re-build) the provider for the current instance.
        let provider = match build_provider_for_instance(
            &app.provider.current_instance,
            app.provider.current_thinking,
            &config,
        ) {
            Ok(p) => p,
            Err(e) => {
                let msg = format!("[provider unavailable: {e}]");
                log::debug!(
                    "provider build failed: provider={} model={} err={}",
                    app.provider.current_instance.id,
                    app.provider.current_model,
                    e
                );
                app.push_notice(llm::Message::assistant(msg.clone()));
                app.mark_log_dirty();
                Arc::new(UnavailableProvider { message: msg }) as Arc<dyn LlmProvider + Send + Sync>
            }
        };

        if app.login.retry_after_refresh {
            app.login.retry_after_refresh = false;
            app.retry_last_request(&provider);
        }

        if app.login.retry_model_fetch_after_refresh {
            app.login.retry_model_fetch_after_refresh = false;
            app.start_model_fetch(&provider);
        }

        match run(&mut terminal, &mut app, &provider, &config).await {
            Ok(RunResult::Quit) | Err(_) => break,

            Ok(RunResult::RebuildProvider) => {}

            Ok(RunResult::ReloadContext) => {
                let custom_tools = load_custom_tools(&custom_tool_dirs());
                let custom_count = custom_tools.len();
                let tools = register_builtin_tools(
                    Some(app_event_tx.clone()),
                    Arc::clone(&file_tracker),
                    custom_tools,
                )
                .await;
                let loaded_skills = skills::load_skills();
                let system_prompt = build_system_prompt(&tools, &cwd, &loaded_skills);
                let skills_count = loaded_skills.len();
                app.agent_config.tools = tools;
                app.system_prompt = Some(system_prompt);
                app.loaded_skills = loaded_skills;
                app.push_notice(Message::assistant(format!(
                    "[reloaded context: {} skill{}, {} custom tool{}]",
                    skills_count,
                    if skills_count == 1 { "" } else { "s" },
                    custom_count,
                    if custom_count == 1 { "" } else { "s" },
                )));
                app.mark_log_dirty();
                app.completion.available_models = None;
            }

            Ok(RunResult::ChangeModel {
                name,
                prompt_thinking_selection,
            }) => {
                // Update the current instance's model.
                app.provider.current_instance.model = Some(name.clone());
                app.provider.current_model = name.clone();
                app.provider.current_model = name;
                app.provider.current_thinking =
                    resolve_thinking_level_for_model(&config, &app.provider.current_model);
                app.record_model_changed();
                app.record_thinking_level_changed();
                // Invalidate cached model list so the next fetch is fresh.
                app.completion.available_models = None;
                persist_provider_model_selection_v2(&mut config, &mut app);
                app.provider.instances = config.providers.clone();
                maybe_warn_thinking_unsupported(&mut app);
                if prompt_thinking_selection
                    && thinking_support_for_instance(
                        &app.provider.current_instance,
                        &app.provider.current_model,
                    ) == ThinkingSupport::Applied
                {
                    app.enter_thinking_selection_mode();
                }
            }

            Ok(RunResult::ChangeProvider(id)) => {
                if let Some(inst) = config.find_provider(&id).cloned() {
                    let requires_api_key = provider_setup_requires_api_key(&inst);
                    if requires_api_key && inst.api_key.as_deref().unwrap_or("").is_empty() {
                        app.provider.pending_setup =
                            Some(PendingProviderSetup::from_instance(&inst));
                        app.enter_provider_api_key_input_mode();
                        continue;
                    }

                    app.provider.current_instance = inst;
                    app.provider.current_model =
                        resolve_model_for_instance(None, &app.provider.current_instance);
                    app.provider.current_thinking =
                        resolve_thinking_level_for_model(&config, &app.provider.current_model);
                    app.record_model_changed();
                    app.record_thinking_level_changed();
                    app.completion.available_models = None;
                    persist_provider_model_selection_v2(&mut config, &mut app);
                    app.provider.instances = config.providers.clone();
                    maybe_warn_thinking_unsupported(&mut app);
                }
                // Unknown id: silently ignore and loop (provider unchanged).
            }

            Ok(RunResult::AddProvider(instance)) => {
                app.clear_pending_provider_setup();
                let instance_id = instance.id.clone();
                let current_model_for_instance = resolve_model_for_instance(None, &instance);
                config.upsert_provider(instance.clone());
                config.provider = Some(instance_id);
                if let Err(e) = config.save() {
                    log::debug!("failed to persist new provider config: {e}");
                    app.push_notice(Message::assistant(format!(
                        "[failed to persist config.toml: {e}]"
                    )));
                    app.mark_log_dirty();
                }
                app.provider.current_instance = config
                    .find_provider(&instance.id)
                    .cloned()
                    .unwrap_or(instance);
                app.provider.current_model = current_model_for_instance;
                app.provider.current_thinking =
                    resolve_thinking_level_for_model(&config, &app.provider.current_model);
                app.record_model_changed();
                app.record_thinking_level_changed();
                app.provider.instances = config.providers.clone();
                app.completion.available_models = None;
                maybe_warn_thinking_unsupported(&mut app);
                app.push_notice(Message::assistant(format!(
                    "[added provider {} ({})]",
                    app.provider.current_instance.id,
                    app.provider.current_instance.backend_preset.label(),
                )));
                app.mark_log_dirty();
            }

            Ok(RunResult::UpdateProvider {
                original_id,
                instance,
            }) => {
                app.clear_pending_provider_setup();
                let instance_id = instance.id.clone();
                let current_model_for_instance = resolve_model_for_instance(None, &instance);
                if let Some(original_id) = original_id.as_deref()
                    && original_id != instance.id
                {
                    config.remove_provider(original_id);
                }
                config.upsert_provider(instance.clone());
                config.provider = Some(instance_id);
                if let Err(e) = config.save() {
                    log::debug!("failed to persist updated provider config: {e}");
                    app.push_notice(Message::assistant(format!(
                        "[failed to persist config.toml: {e}]"
                    )));
                    app.mark_log_dirty();
                }
                app.provider.current_instance = config
                    .find_provider(&instance.id)
                    .cloned()
                    .unwrap_or(instance);
                app.provider.current_model = current_model_for_instance;
                app.provider.current_thinking =
                    resolve_thinking_level_for_model(&config, &app.provider.current_model);
                app.record_model_changed();
                app.record_thinking_level_changed();
                app.provider.instances = config.providers.clone();
                app.completion.available_models = None;
                maybe_warn_thinking_unsupported(&mut app);
                app.push_notice(Message::assistant(format!(
                    "[edited provider {} ({})]",
                    app.provider.current_instance.id,
                    app.provider.current_instance.backend_preset.label(),
                )));
                app.mark_log_dirty();
            }

            Ok(RunResult::RemoveProvider(id)) => {
                app.clear_pending_provider_setup();
                app.clear_pending_provider_removal();
                if config.remove_provider(&id) {
                    if config.provider.as_deref() == Some(id.as_str()) {
                        config.provider = config.providers.first().map(|p| p.id.clone());
                    }
                    if let Err(e) = config.save() {
                        log::debug!("failed to persist provider removal: {e}");
                        app.push_notice(Message::assistant(format!(
                            "[failed to persist config.toml: {e}]"
                        )));
                        app.mark_log_dirty();
                    }
                    app.provider.current_instance = resolve_default_provider_instance(&config);
                    app.provider.current_model =
                        resolve_model_for_instance(None, &app.provider.current_instance);
                    app.provider.current_thinking =
                        resolve_thinking_level_for_model(&config, &app.provider.current_model);
                    app.record_model_changed();
                    app.record_thinking_level_changed();
                    app.provider.instances = config.providers.clone();
                    app.completion.available_models = None;
                    maybe_warn_thinking_unsupported(&mut app);
                    app.push_notice(Message::assistant(format!("[removed provider {id}]")));
                    app.mark_log_dirty();
                }
            }

            Ok(RunResult::ChangeThinking(level)) => {
                app.provider.current_thinking = level;
                app.provider.current_thinking = level;
                app.record_thinking_level_changed();
                persist_provider_model_selection_v2(&mut config, &mut app);
                app.provider.instances = config.providers.clone();
                maybe_warn_thinking_unsupported(&mut app);
            }

            Ok(RunResult::ConfigureProvider {
                instance,
                url,
                api_key,
            }) => {
                app.clear_pending_provider_setup();
                let mut inst = config
                    .find_provider(&instance.id)
                    .cloned()
                    .unwrap_or(instance);
                if let Some(url) = url.as_deref() {
                    inst.base_url = Some(url.to_string());
                    // Keep legacy per-preset config in sync.
                    match inst.backend_preset {
                        provider_instance::BackendPreset::Ollama => {
                            config.ollama.record_endpoint(url.to_string());
                        }
                        provider_instance::BackendPreset::OpenWebUi => {
                            config.open_webui.record_endpoint(url.to_string());
                        }
                        _ => {}
                    }
                }
                if let Some(api_key) = api_key {
                    inst.api_key = Some(api_key.clone());
                    if inst.backend_preset == provider_instance::BackendPreset::OpenWebUi {
                        config.open_webui.api_key = Some(api_key);
                    }
                }
                config.upsert_provider(inst.clone());
                config.provider = Some(inst.id.clone());
                if let Err(e) = config.save() {
                    log::debug!("failed to persist provider config: {e}");
                    app.push_notice(Message::assistant(format!(
                        "[failed to persist config.toml: {e}]"
                    )));
                    app.mark_log_dirty();
                }
                app.provider.current_instance = inst;
                app.provider.instances = config.providers.clone();
                app.provider.current_model =
                    resolve_model_for_instance(None, &app.provider.current_instance);
                app.provider.current_thinking =
                    resolve_thinking_level_for_model(&config, &app.provider.current_model);
                app.record_model_changed();
                app.record_thinking_level_changed();
                app.completion.available_models = None;
                maybe_warn_thinking_unsupported(&mut app);
                let endpoint_msg = url
                    .map(|u| format!(" endpoint set to {u}"))
                    .unwrap_or_default();
                app.push_notice(Message::assistant(format!(
                    "[provider {}{endpoint_msg}]",
                    app.provider.current_instance.id,
                )));
                app.mark_log_dirty();
            }
        }
    }

    disable_raw_mode()?;
    if keyboard_enhancements_enabled {
        execute!(terminal.backend_mut(), PopKeyboardEnhancementFlags)?;
    }
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        DisableBracketedPaste
    )?;
    terminal.show_cursor()?;

    Ok(())
}

// ── Result type returned by the inner run loop ────────────────────────────────

enum RunResult {
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

fn normalize_paste_text(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn provider_setup_requires_endpoint(instance: &ProviderInstance) -> bool {
    matches!(
        instance.backend_preset.def().endpoint_behavior,
        EndpointBehavior::UserSupplied
    )
}

fn provider_setup_requires_api_key(instance: &ProviderInstance) -> bool {
    instance.backend_preset.def().auth_mode == AuthMode::ApiKey
}

fn enter_provider_endpoint_input(app: &mut App, _instance: &ProviderInstance) {
    app.enter_provider_endpoint_input_mode();
}

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

struct UnavailableProvider {
    message: String,
}

impl LlmProvider for UnavailableProvider {
    fn stream_chat(&self, _messages: Vec<Message>) -> LlmStream {
        let msg = self.message.clone();
        Box::pin(async_stream::stream! {
            yield LlmEvent::Error(llm::ProviderError::other("unavailable", msg));
        })
    }

    fn stream_chat_with_tools(
        &self,
        _messages: Vec<Message>,
        _tools: Vec<llm::ToolDefinition>,
    ) -> LlmStream {
        self.stream_chat(vec![])
    }

    fn list_models(&self) -> ModelListFuture {
        Box::pin(async { Ok(vec![]) })
    }
}

// ── Inner event loop ──────────────────────────────────────────────────────────

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

enum KeyDispatch {
    NotHandled,
    Continue,
    Return(RunResult),
}

async fn run(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    provider: &Arc<dyn LlmProvider + Send + Sync>,
    config: &TauConfig,
) -> io::Result<RunResult> {
    let mut crossterm_events = EventStream::new();
    let mut tick_interval = tokio::time::interval(std::time::Duration::from_millis(320));
    tick_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    // Timestamp of the most recent key Press event other than Enter itself.
    // Used on Windows to detect paste-injected Enter events (see above).
    #[cfg(windows)]
    let mut last_key_at: Option<std::time::Instant> = None;

    // Draw unconditionally on the first iteration; subsequent draws are only
    // performed when something actually changed (dirty flag).
    let mut needs_redraw = true;

    loop {
        if needs_redraw {
            execute!(io::stdout(), BeginSynchronizedUpdate)?;
            terminal.draw(|f| ui::draw(f, app))?;
            execute!(io::stdout(), EndSynchronizedUpdate)?;
            needs_redraw = false;
        }

        tokio::select! {
            // ── Terminal input ────────────────────────────────────────────────
            Some(Ok(ev)) = crossterm_events.next() => {
                needs_redraw = true;
                match ev {
                    Event::Key(key) => {
                        if let Some(result) = handle_key_event(
                            app,
                            provider,
                            config,
                            key,
                            #[cfg(windows)]
                            &mut last_key_at,
                        ) {
                            return Ok(result);
                        }
                    }
                    Event::Mouse(mouse) => match mouse.kind {
                        MouseEventKind::ScrollUp   => app.scroll_up_lines(3),
                        MouseEventKind::ScrollDown => app.scroll_down_lines(3),
                        _ => {}
                    },
                    Event::Paste(text)
                        if !app.login.active => {
                            apply_paste(app, provider, &text);
                        },
                    _ => {}
                }
            }

            // ── Background app events ───────────────────────────────────────
            Some(ev) = app.recv_app_event() => {
                needs_redraw = true;
                app.apply_app_event(ev);
                if app.login.needs_rebuild {
                    app.login.needs_rebuild = false;
                    return Ok(RunResult::RebuildProvider);
                }
            }

            // ── Throbber animation tick ───────────────────────────────────────
            _ = tick_interval.tick() => {
                app.tick();
                // Only mark dirty when the throbber is actually animating;
                // when idle the tick fires but nothing visible changes.
                if app.streaming() {
                    needs_redraw = true;
                }
            }
        }
    }
}

fn handle_key_event(
    app: &mut App,
    provider: &Arc<dyn LlmProvider + Send + Sync>,
    config: &TauConfig,
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

fn handle_selection_mode_key(app: &mut App, config: &TauConfig, key: KeyEvent) -> KeyDispatch {
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
                    == provider_instance::BackendClass::UserSuppliedService
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
                    == provider_instance::BackendClass::UserSuppliedService
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
    config: &TauConfig,
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
    config: &TauConfig,
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
                            provider_instance::EndpointBehavior::UserSupplied
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
                    .map(|i| i.backend_preset == provider_instance::BackendPreset::Ollama)
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
    config: &TauConfig,
) -> KeyDispatch {
    let input = app.slash_submit_text().unwrap_or_default();
    app.reset_textarea();

    match commands::parse(&input) {
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
            let thinking_supported = config
                .find_provider(&app.provider.current_instance.id)
                .map(|inst| {
                    thinking_support_for_instance(inst, &app.provider.current_model)
                        == ThinkingSupport::Applied
                })
                .unwrap_or(false);
            if !thinking_supported {
                return KeyDispatch::Continue;
            }
            match ThinkingLevel::parse(&raw) {
                Some(level) => return KeyDispatch::Return(RunResult::ChangeThinking(level)),
                None => {
                    app.push_notice(llm::Message::assistant(format!(
                        "[invalid thinking level: '{raw}' (use off|minimal|low|medium|high|xhigh)]"
                    )));
                    app.mark_log_dirty();
                }
            }
        }
        Some(CommandAction::ThinkingNoArg) => {
            let thinking_supported = config
                .find_provider(&app.provider.current_instance.id)
                .map(|inst| {
                    thinking_support_for_instance(inst, &app.provider.current_model)
                        == ThinkingSupport::Applied
                })
                .unwrap_or(false);
            if thinking_supported {
                app.enter_thinking_selection_mode();
            }
        }
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
                Some(skill) => match skills::expand_skill(skill, &args) {
                    Ok(expanded) => {
                        app.submit_with_text(expanded, provider);
                    }
                    Err(e) => {
                        app.push_notice(llm::Message::assistant(format!("[skill error: {e}]")));
                        app.mark_log_dirty();
                    }
                },
                None => {
                    app.push_notice(llm::Message::assistant(format!(
                        "[unknown skill: '{name}']"
                    )));
                    app.mark_log_dirty();
                }
            }
        }
        None => {}
    }

    KeyDispatch::Continue
}

fn cancel_ask_freeform_if_off_sentinel(app: &mut App) {
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

fn apply_paste(app: &mut App, provider: &Arc<dyn LlmProvider + Send + Sync>, text: &str) {
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

/// Resolve the default active [`ProviderInstance`] from config.
///
/// Resolution order:
/// 1. `config.provider` matched against instance ids
/// 2. First instance in `config.providers`
/// 3. Synthetic copilot default
fn resolve_default_provider_instance(config: &TauConfig) -> ProviderInstance {
    if let Some(ref id) = config.provider
        && let Some(inst) = config.find_provider(id)
    {
        return inst.clone();
    }

    config.providers.first().cloned().unwrap_or_else(|| {
        ProviderInstance::new("copilot", provider_instance::BackendPreset::Copilot)
    })
}

fn resolve_provider_instance(
    cli_override: Option<&str>,
    config: &TauConfig,
) -> Result<ProviderInstance, String> {
    if let Some(id) = cli_override {
        if id == "test" {
            return Ok(ProviderInstance::new(
                "test",
                provider_instance::BackendPreset::Test,
            ));
        }
        if let Some(inst) = config.find_provider(id) {
            return Ok(inst.clone());
        }

        let mut allowed = config
            .providers
            .iter()
            .map(|instance| instance.id.as_str())
            .collect::<Vec<_>>();
        allowed.push("test");
        return Err(format!(
            "unknown provider '{id}'. Expected one of: {}",
            allowed.join(", ")
        ));
    }

    Ok(resolve_default_provider_instance(config))
}

fn resolve_current_run_instance(app: &App, config: &TauConfig) -> ProviderInstance {
    config
        .find_provider(&app.provider.current_instance.id)
        .cloned()
        .unwrap_or_else(|| resolve_default_provider_instance(config))
}

/// Resolve the effective model for a provider instance.
fn resolve_model_for_instance(cli_override: Option<&str>, instance: &ProviderInstance) -> String {
    cli_override
        .map(ToString::to_string)
        .or_else(|| instance.model.clone())
        .unwrap_or_else(|| instance.backend_preset.default_model().to_string())
}

/// Instance-based variant of `persist_provider_model_selection`.
///
/// Updates the named instance's model in the providers list and persists config.
fn persist_provider_model_selection_v2(config: &mut TauConfig, app: &mut App) {
    let instance = &app.provider.current_instance;
    let model = &app.provider.current_model;
    let thinking = app.provider.current_thinking;
    // Never persist the test provider.
    if instance.backend_preset == provider_instance::BackendPreset::Test {
        return;
    }
    config.provider = Some(instance.id.clone());
    config.thinking = Some(thinking.as_str().to_string());
    config
        .thinking_by_model
        .insert(model.to_string(), thinking.as_str().to_string());

    // Update the model on the stored instance.
    if let Some(stored) = config.find_provider_mut(&instance.id) {
        stored.model = Some(model.to_string());
    }

    if let Err(e) = config.save() {
        log::debug!("failed to persist provider/model config: {}", e);
        app.push_notice(Message::assistant(format!(
            "[failed to persist config.toml: {e}]"
        )));
        app.mark_log_dirty();
    }
}

fn resolve_thinking_level_for_model(config: &TauConfig, model: &str) -> ThinkingLevel {
    config
        .thinking_by_model
        .get(model)
        .and_then(|raw| ThinkingLevel::parse(raw))
        .or_else(|| config.thinking.as_deref().and_then(ThinkingLevel::parse))
        .unwrap_or(ThinkingLevel::Off)
}

fn maybe_warn_thinking_unsupported(app: &mut App) {
    let instance = &app.provider.current_instance;
    let model = &app.provider.current_model;
    let thinking = app.provider.current_thinking;
    // Always keep app.provider.thinking_supported in sync regardless of the level.
    app.provider.thinking_supported =
        thinking_support_for_instance(instance, model) == ThinkingSupport::Applied;

    if thinking == ThinkingLevel::Off {
        return;
    }
    if let ThinkingSupport::Ignored(reason) = thinking_support_for_instance(instance, model) {
        log::debug!(
            "thinking '{}' ignored for provider={} model={}: {}",
            thinking.as_str(),
            instance.id,
            model,
            reason
        );
    }
}
// ── Non-interactive helpers ───────────────────────────────────────────────────

/// Parameters needed to rebuild a provider after a reactive token refresh.
struct PrintModeProviderCtx<'a> {
    instance: &'a ProviderInstance,
    thinking: ThinkingLevel,
    tau_config: &'a TauConfig,
    name: &'a str,
}

fn provider_display_name(instance: &ProviderInstance) -> String {
    instance.backend_preset.label().to_string()
}

/// Non-interactive mode: run the agent loop for `prompt`, stream output to
/// stdout, and exit when the loop finishes.
/// Returns `true` if `provider` is one of the three OAuth providers that
/// support token refresh (copilot, codex, gemini).
fn provider_supports_token_refresh(provider: &str) -> bool {
    matches!(provider, "copilot" | "codex" | "gemini")
}

/// Proactively refresh the token for `provider` if it is expired or expiring
/// soon. Does nothing (and returns `false`) for providers that do not support
/// refresh. Returns `true` when a refresh was performed successfully.
async fn preflight_token_refresh(provider: &str) -> bool {
    if !provider_supports_token_refresh(provider) {
        return false;
    }

    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    let state = match auth::token_state(provider, now_secs, auth::AUTH_REFRESH_LEEWAY_SECS) {
        Ok(s) => s,
        Err(e) => {
            log::warn!("preflight token check failed: {e}");
            return false;
        }
    };

    match state {
        auth::AuthTokenState::Expired | auth::AuthTokenState::ExpiringSoon => {
            log::debug!("preflight: token {state:?}, refreshing before request");
            match auth::refresh_token(provider).await {
                Ok(()) => {
                    log::debug!("preflight: token refreshed successfully");
                    true
                }
                Err(e) => {
                    log::warn!("preflight: token refresh failed: {e}");
                    false
                }
            }
        }
        _ => false,
    }
}

async fn run_print_mode(
    prompt: String,
    provider_override: &str,
    model_override: Option<&str>,
    config: &TauConfig,
) -> io::Result<()> {
    let current_instance = resolve_provider_instance(Some(provider_override), config)
        .map_err(|e| io::Error::new(ErrorKind::InvalidInput, e))?;
    let current_model = resolve_model_for_instance(model_override, &current_instance);
    let current_thinking = resolve_thinking_level_for_model(config, &current_model);
    let provider_name = current_instance.backend_preset.id().to_string();

    // Proactive preflight: refresh the token before building the provider so
    // that build_provider reads fresh credentials from the auth store.
    preflight_token_refresh(&provider_name).await;

    let provider = build_provider_for_instance(&current_instance, current_thinking, config)
        .map_err(|e| io::Error::other(format!("provider error: {e}")))?;

    let custom_tools = load_custom_tools(&custom_tool_dirs());
    let headless_tracker = Arc::new(Mutex::new(build_file_tracker()));
    let tools = register_builtin_tools(None, Arc::clone(&headless_tracker), custom_tools).await;
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".to_string());
    let loaded_skills = skills::load_skills();
    let headless_log = Arc::new(std::sync::Mutex::new(ToolOutputLog::new("headless")));
    let system_prompt = build_system_prompt(&tools, &cwd, &loaded_skills);

    let session_events = vec![crate::session_event::SessionEvent::UserMessage {
        content: prompt.clone(),
        timestamp: crate::app_agent_handlers::now_ts(),
    }];

    let loop_config = AgentLoopConfig {
        tools,
        file_tracker: headless_tracker,
        tool_output_log: headless_log,
        session_events,
        current_model: current_instance.effective_model().to_string(),
        auto_compaction_enabled: true,
        manual_compaction_instructions: None,
        executor: std::sync::Arc::new(crate::agent::DefaultToolExecutor::new()),
        system_prompt: Some(system_prompt),
    };

    let provider_ctx = PrintModeProviderCtx {
        instance: &current_instance,
        thinking: current_thinking,
        tau_config: config,
        name: &provider_name,
    };

    let exit_code = run_print_mode_loop(loop_config, provider, &provider_ctx).await;

    std::process::exit(exit_code);
}

/// Drive the agent event loop for `--print` mode, handling one reactive token
/// refresh + retry on a 401 Unauthorized error. Returns the process exit code.
async fn run_print_mode_loop(
    config: AgentLoopConfig,
    provider: std::sync::Arc<dyn llm::LlmProvider + Send + Sync>,
    ctx: &PrintModeProviderCtx<'_>,
) -> i32 {
    // Keep a copy of what we need for the retry path.
    let session_events_for_retry = config.session_events.clone();
    let system_prompt_for_retry = config.system_prompt.clone();

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AppEvent>();
    let (_steering_tx, steering_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);

    tokio::spawn(async move {
        agent::run_agent_loop(config, provider, tx, steering_rx, cancel_rx).await;
    });

    while let Some(ev) = rx.recv().await {
        let AppEvent::Agent(ev) = ev else {
            continue;
        };
        match ev {
            AgentEvent::TextToken { text, .. } => {
                print!("{text}");
                use std::io::Write;
                let _ = io::stdout().flush();
            }
            AgentEvent::ThinkingToken(_) => {
                // Suppress thinking tokens in print mode.
            }
            AgentEvent::Usage(_) => {
                // Suppress usage events in print mode.
            }
            AgentEvent::ToolCallIntent { .. } => {
                // No-op in print mode.
            }
            AgentEvent::ToolCallArgsDelta { .. } => {
                // No-op in print mode.
            }
            AgentEvent::SteeringConsumed { .. } => {
                // No-op in print mode.
            }
            AgentEvent::StatusUpdate(msg) => {
                eprintln!("{msg}");
            }
            AgentEvent::Compacting => {
                eprintln!("compacting…");
            }
            AgentEvent::CompactionDone {
                tokens_before,
                tokens_after,
                ..
            } => {
                eprintln!(
                    "compacted: {}k → {}k tokens",
                    tokens_before / 1000,
                    tokens_after / 1000
                );
            }
            AgentEvent::ToolCallStart { name, args, .. } => {
                eprintln!("{}", tool_presentation::tool_invocation_label(&name, &args));
            }
            AgentEvent::ToolCallEnd { result, .. } => {
                if result.is_error {
                    eprintln!(
                        "  ✗ {}",
                        result.content.as_text().lines().next().unwrap_or("error")
                    );
                }
            }
            AgentEvent::TurnEnd => {}
            AgentEvent::ExternalFileChange { paths, .. } => {
                // Print the file change notification to stderr in headless mode.
                for path in &paths {
                    eprintln!("⚠️  {} was modified externally", path.display());
                }
            }
            AgentEvent::Done => {
                println!(); // final newline after streamed output
                return 0;
            }
            AgentEvent::Error(e) => {
                // Reactive 401 handling: refresh the token once and retry.
                if e.kind == llm::ProviderErrorKind::Unauthorized
                    && provider_supports_token_refresh(ctx.name)
                {
                    log::debug!("received 401 in print mode, attempting token refresh");
                    match auth::refresh_token(ctx.name).await {
                        Ok(()) => {
                            log::debug!(
                                "reactive refresh succeeded, rebuilding provider and retrying"
                            );
                            match build_provider_for_instance(
                                ctx.instance,
                                ctx.thinking,
                                ctx.tau_config,
                            ) {
                                Ok(new_provider) => {
                                    // Run the loop a second time with the refreshed provider.
                                    // `retried = true` prevents further recursive retries.
                                    return run_print_mode_loop_inner(
                                        session_events_for_retry,
                                        system_prompt_for_retry,
                                        new_provider,
                                        &provider_display_name(ctx.instance),
                                    )
                                    .await;
                                }
                                Err(build_err) => {
                                    eprintln!(
                                        "error: token refreshed but failed to rebuild provider: {build_err}"
                                    );
                                    return 1;
                                }
                            }
                        }
                        Err(refresh_err) => {
                            log::warn!("reactive refresh failed: {refresh_err}");
                            let rendered = format_provider_error_for_display(
                                &provider_display_name(ctx.instance),
                                &e,
                            );
                            eprintln!(
                                "error: {rendered} (token refresh also failed: {refresh_err})"
                            );
                            return 1;
                        }
                    }
                }

                let rendered =
                    format_provider_error_for_display(&provider_display_name(ctx.instance), &e);
                eprintln!("error: {rendered}");
                return 1;
            }
        }
    }

    0
}

/// Inner agent loop used for the single retry after a reactive token refresh.
/// Identical event handling to `run_print_mode_loop` but without a further
/// retry on 401 (budget is exhausted after one attempt).
async fn run_print_mode_loop_inner(
    session_events: Vec<crate::session_event::SessionEvent>,
    system_prompt: Option<String>,
    provider: std::sync::Arc<dyn llm::LlmProvider + Send + Sync>,
    provider_label: &str,
) -> i32 {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AppEvent>();
    let (_steering_tx, steering_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);

    // AgentLoopConfig is not Clone; rebuild a minimal headless one for the retry.
    let retry_tracker = Arc::new(Mutex::new(build_file_tracker()));
    let retry_log = Arc::new(std::sync::Mutex::new(ToolOutputLog::new("headless-retry")));
    let custom_tools = load_custom_tools(&custom_tool_dirs());
    let retry_tools = register_builtin_tools(None, Arc::clone(&retry_tracker), custom_tools).await;
    let retry_config = AgentLoopConfig {
        tools: retry_tools,
        file_tracker: retry_tracker,
        tool_output_log: retry_log,
        session_events,
        current_model: String::new(),
        auto_compaction_enabled: true,
        manual_compaction_instructions: None,
        executor: std::sync::Arc::new(crate::agent::DefaultToolExecutor::new()),
        system_prompt,
    };

    tokio::spawn(async move {
        agent::run_agent_loop(retry_config, provider, tx, steering_rx, cancel_rx).await;
    });

    while let Some(ev) = rx.recv().await {
        let AppEvent::Agent(ev) = ev else {
            continue;
        };
        match ev {
            AgentEvent::TextToken { text, .. } => {
                print!("{text}");
                use std::io::Write;
                let _ = io::stdout().flush();
            }
            AgentEvent::ThinkingToken(_)
            | AgentEvent::Usage(_)
            | AgentEvent::ToolCallIntent { .. }
            | AgentEvent::ToolCallArgsDelta { .. }
            | AgentEvent::SteeringConsumed { .. }
            | AgentEvent::TurnEnd => {}
            AgentEvent::StatusUpdate(msg) => {
                eprintln!("{msg}");
            }
            AgentEvent::Compacting => {
                eprintln!("compacting…");
            }
            AgentEvent::CompactionDone {
                tokens_before,
                tokens_after,
                ..
            } => {
                eprintln!(
                    "compacted: {}k → {}k tokens",
                    tokens_before / 1000,
                    tokens_after / 1000
                );
            }
            AgentEvent::ToolCallStart { name, args, .. } => {
                eprintln!("{}", tool_presentation::tool_invocation_label(&name, &args));
            }
            AgentEvent::ToolCallEnd { result, .. } => {
                if result.is_error {
                    eprintln!(
                        "  ✗ {}",
                        result.content.as_text().lines().next().unwrap_or("error")
                    );
                }
            }
            AgentEvent::ExternalFileChange { paths, .. } => {
                for path in &paths {
                    eprintln!("⚠️  {} was modified externally", path.display());
                }
            }
            AgentEvent::Done => {
                println!();
                return 0;
            }
            AgentEvent::Error(e) => {
                let rendered = format_provider_error_for_display(provider_label, &e);
                eprintln!("error: {rendered}");
                return 1;
            }
        }
    }

    0
}

#[cfg(test)]
mod tests {
    use super::{
        normalize_paste_text, provider_display_name, resolve_default_provider_instance,
        resolve_model_for_instance, resolve_provider_instance, resolve_thinking_level_for_model,
    };
    use crate::{
        config::TauConfig,
        llm::ProviderError,
        provider_instance::{BackendPreset, ProviderInstance},
        provider_manager::format_provider_error_for_display,
        thinking::ThinkingLevel,
    };

    #[test]
    fn normalize_paste_text_converts_crlf_and_cr_to_lf() {
        let pasted = "a\r\nb\rc\n";
        assert_eq!(normalize_paste_text(pasted), "a\nb\nc\n");
    }

    #[test]
    fn resolve_provider_instance_accepts_exact_configured_provider_id() {
        let mut cfg = TauConfig::default();
        cfg.providers.push(ProviderInstance::new(
            "work-webui",
            BackendPreset::OpenWebUi,
        ));

        let instance =
            resolve_provider_instance(Some("work-webui"), &cfg).expect("provider should resolve");

        assert_eq!(instance.id, "work-webui");
        assert_eq!(instance.backend_preset, BackendPreset::OpenWebUi);
    }

    #[test]
    fn resolve_provider_instance_accepts_hidden_test_provider() {
        let cfg = TauConfig::default();

        let instance = resolve_provider_instance(Some("test"), &cfg).expect("test should resolve");

        assert_eq!(instance.id, "test");
        assert_eq!(instance.backend_preset, BackendPreset::Test);
    }

    #[test]
    fn resolve_provider_instance_rejects_unknown_cli_provider() {
        let mut cfg = TauConfig::default();
        cfg.providers
            .push(ProviderInstance::new("copilot", BackendPreset::Copilot));
        cfg.providers.push(ProviderInstance::new(
            "work-webui",
            BackendPreset::OpenWebUi,
        ));

        let err = resolve_provider_instance(Some("does-not-exist"), &cfg)
            .expect_err("unknown provider should be rejected");

        assert_eq!(
            err,
            "unknown provider 'does-not-exist'. Expected one of: copilot, work-webui, test"
        );
    }

    #[test]
    fn resolve_default_provider_instance_prefers_configured_default() {
        let mut cfg = TauConfig {
            provider: Some("work-webui".to_string()),
            ..TauConfig::default()
        };
        cfg.providers
            .push(ProviderInstance::new("copilot", BackendPreset::Copilot));
        cfg.providers.push(ProviderInstance::new(
            "work-webui",
            BackendPreset::OpenWebUi,
        ));

        let instance = resolve_default_provider_instance(&cfg);

        assert_eq!(instance.id, "work-webui");
        assert_eq!(instance.backend_preset, BackendPreset::OpenWebUi);
    }

    #[test]
    fn resolve_default_provider_instance_falls_back_to_synthetic_copilot() {
        let cfg = TauConfig::default();

        let instance = resolve_default_provider_instance(&cfg);

        assert_eq!(instance.id, "copilot");
        assert_eq!(instance.backend_preset, BackendPreset::Copilot);
    }

    #[test]
    fn resolve_model_uses_instance_model() {
        let mut inst = ProviderInstance::new("copilot", BackendPreset::Copilot);
        inst.model = Some("gpt-5.3-codex".to_string());
        let model = resolve_model_for_instance(None, &inst);
        assert_eq!(model, "gpt-5.3-codex");
    }

    #[test]
    fn resolve_model_falls_back_to_service_default() {
        let inst = ProviderInstance::new("copilot", BackendPreset::Copilot);
        let model = resolve_model_for_instance(None, &inst);
        assert_eq!(model, BackendPreset::Copilot.default_model());
    }

    #[test]
    fn resolve_thinking_uses_model_specific_config() {
        let mut cfg = TauConfig {
            thinking: Some("minimal".to_string()),
            ..TauConfig::default()
        };
        cfg.thinking_by_model
            .insert("gpt-5".to_string(), "high".to_string());

        let level = resolve_thinking_level_for_model(&cfg, "gpt-5");
        assert_eq!(level, ThinkingLevel::High);
    }

    #[test]
    fn resolve_thinking_falls_back_to_global_config() {
        let cfg = TauConfig {
            thinking: Some("minimal".to_string()),
            ..TauConfig::default()
        };
        let level = resolve_thinking_level_for_model(&cfg, "gpt-4o");
        assert_eq!(level, ThinkingLevel::Minimal);
    }

    #[test]
    fn resolve_thinking_defaults_to_off() {
        let cfg = TauConfig::default();
        let level = resolve_thinking_level_for_model(&cfg, "gpt-4o");
        assert_eq!(level, ThinkingLevel::Off);
    }

    #[test]
    fn provider_display_name_uses_backend_label() {
        let instance = ProviderInstance::new("work-webui", BackendPreset::OpenWebUi);
        assert_eq!(provider_display_name(&instance), "Open WebUI");
    }

    #[test]
    fn print_mode_error_format_uses_backend_label() {
        let instance = ProviderInstance::new("work-webui", BackendPreset::OpenWebUi);
        let err = ProviderError::server_error("OpenAI", 524, "error code: 524");

        let rendered = format_provider_error_for_display(&provider_display_name(&instance), &err);

        assert_eq!(
            rendered,
            "Open WebUI timed out on the backend (524).\nProvider message: error code: 524"
        );
    }
}
