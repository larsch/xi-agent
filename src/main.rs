use clap::Parser;
use crossterm::{
    event::{Event, EventStream, MouseButton, MouseEventKind},
    execute,
    terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate},
};
use futures_util::StreamExt;
use ratatui::Terminal;
use std::{
    io,
    io::ErrorKind,
    sync::{Arc, Mutex},
};

mod agent;
mod agent_runtime;
mod agents;
mod at_file;

mod atomic_file;

mod agent_turn_state;
mod app;
mod app_agent_handlers;
mod app_event;
mod app_interaction;
mod app_submission;
mod ask_user_state;
mod auth;
mod clipboard;
mod commands;
mod completion;
mod completion_state;
mod config;
mod context_window;
mod debug_log;
mod desktop_notification;
mod dirs;
mod event_log;
mod export;
mod hook_ipc;
mod hooks;
mod input;
mod keybindings;
mod live_turn;
mod llm;
mod log_view_state;
mod login_state;
mod markdown;
mod migrate;
mod mouse_select;
mod print_mode;
mod process;
mod projection;
mod provider;
mod provider_instance;
mod provider_manager;
mod provider_setup;
#[cfg(feature = "restart")]
mod restart;
mod selection_state;
mod session;
mod session_event;
mod session_ipc;
mod session_manager;
mod session_state;
mod shell;
mod shell_state;
mod skills;
mod step_back_state;
mod terminal;
mod theme;
mod theme_demo;
mod thinking;
mod tool_presentation;
mod tracked;
pub(crate) mod ui;

use agent::tools::custom::custom_tool_dirs;
use agent::{
    AgentLoopConfig, FileTracker, ToolOutputLog,
    tools::{custom::load_custom_tools, register_builtin_tools},
};
use agents::load_agents;
use app::App;
use app_event::{AppEvent, SendIgnore};

use config::XiConfig;
use hook_ipc::HookIpcPublisherHandle;
use llm::{LlmProvider, Message};
use provider::{ThinkingSupport, build_provider_for_instance, thinking_support_for_instance};
use provider_instance::AuthMode;
use provider_instance::BackendPreset;
use provider_instance::ProviderInstance;
use provider_manager::PendingProviderSetup;
use provider_manager::ProviderSetupStep;
use thinking::ThinkingLevel;

// ── CLI definition ────────────────────────────────────────────────────────────

/// xi — a terminal-based AI coding agent
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
    #[arg(long, short = 'p', value_name = "PROMPT", num_args = 1.., conflicts_with = "theme_demo")]
    print: Option<Vec<String>>,

    /// Start the interactive UI and automatically submit PROMPT.
    /// Accepts multiple words without shell quoting.
    #[arg(
        long,
        value_name = "PROMPT",
        num_args = 1..,
        conflicts_with_all = ["prompt_file", "print"]
    )]
    prompt: Option<Vec<String>>,

    /// Start the interactive UI and automatically submit the UTF-8 contents
    /// of PATH as the initial prompt.
    #[arg(
        long,
        value_name = "PATH",
        conflicts_with = "prompt",
        conflicts_with = "print"
    )]
    prompt_file: Option<std::path::PathBuf>,

    /// Auto-resume the most recent session for the current working directory.
    #[arg(long)]
    resume: bool,

    /// Resume a specific session by ID (used by the `restart_host` tool).
    #[arg(long, value_name = "SESSION_ID")]
    resume_session: Option<String>,

    /// Enable the worktree session IPC control socket.
    #[arg(long)]
    enable_session_ipc: bool,

    /// Print the file-system paths xi uses and exit.
    #[arg(long)]
    print_dirs: bool,

    /// Path to a theme.toml file. Overrides the `theme` key in config.toml.
    #[arg(long, value_name = "PATH")]
    theme: Option<std::path::PathBuf>,

    /// Start with a representative transcript using the local test provider.
    #[arg(long)]
    theme_demo: bool,
}

// ── Entry point ───────────────────────────────────────────────────────────────

/// Build a [`FileTracker`] pre-configured to ignore xi-agent's own generated files:
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

fn read_initial_prompt(
    prompt: Option<Vec<String>>,
    prompt_file: Option<std::path::PathBuf>,
) -> io::Result<Option<String>> {
    match (prompt, prompt_file) {
        (Some(words), None) => Ok(Some(words.join(" "))),
        (None, Some(path)) => std::fs::read_to_string(&path).map(Some).map_err(|e| {
            io::Error::new(
                ErrorKind::InvalidInput,
                format!("failed to read initial prompt file {}: {e}", path.display()),
            )
        }),
        (None, None) => Ok(None),
        (Some(_), Some(_)) => Err(io::Error::new(
            ErrorKind::InvalidInput,
            "--prompt and --prompt-file cannot be used together",
        )),
    }
}

/// Minimal phase timer for startup profiling. Each `mark` logs the elapsed
/// time since the previous mark (and since process start) via `log::debug!`,
/// so it is a no-op unless `XI_DEBUG` enables the debug log.
struct StartupTimer {
    start: std::time::Instant,
    last: std::time::Instant,
}

impl StartupTimer {
    fn new() -> Self {
        let now = std::time::Instant::now();
        Self {
            start: now,
            last: now,
        }
    }

    fn mark(&mut self, name: &str) {
        let now = std::time::Instant::now();
        log::debug!(
            target: "startup",
            "phase={name} elapsed_ms={:.2} total_ms={:.2}",
            (now - self.last).as_secs_f64() * 1000.0,
            (now - self.start).as_secs_f64() * 1000.0,
        );
        self.last = now;
    }
}

#[tokio::main]
async fn main() -> io::Result<()> {
    let mut timer = StartupTimer::new();
    migrate::run();
    timer.mark("migrate::run");
    debug_log::init_logging();
    timer.mark("debug_log::init_logging");

    let cli = Cli::parse();
    timer.mark("Cli::parse");

    if cli.print_dirs {
        dirs::print_dirs();
        return Ok(());
    }

    let initial_prompt = read_initial_prompt(cli.prompt, cli.prompt_file)?;
    timer.mark("read_initial_prompt");

    let mut config = XiConfig::load().map_err(|e| {
        eprintln!(
            "error: failed to load config.toml: {e}\n\
             Refusing to start with default config to prevent data loss.\n\
             Fix or restore ~/.config/xi/config.toml and try again."
        );
        io::Error::other("config load failed")
    })?;
    timer.mark("XiConfig::load");

    // --theme flag overrides config.toml theme path
    if let Some(theme_path) = cli.theme {
        config.theme = Some(theme_path);
    }

    // Load theme (missing file → built-in defaults)
    let theme_path = config.theme.clone().unwrap_or_else(|| {
        crate::dirs::project_dirs()
            .map(|d| d.config_dir().join("theme.toml"))
            .unwrap_or_else(|_| std::path::PathBuf::from("theme.toml"))
    });
    let theme = match crate::theme::Theme::load(&theme_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("warning: failed to load theme: {e}");
            crate::theme::Theme::default()
        }
    };
    timer.mark("theme load");

    // Built-in hosted providers are always available from the static catalog.
    // Config only stores user-configured instances and overrides.

    // ── Non-interactive (--print / -p) mode ───────────────────────────────────
    if let Some(words) = cli.print {
        let prompt = words.join(" ");
        let provider_override = cli.provider.as_deref().ok_or_else(|| {
            io::Error::new(
                ErrorKind::InvalidInput,
                "--print requires --provider <name>",
            )
        })?;
        return print_mode::run_print_mode(
            prompt,
            provider_override,
            cli.model.as_deref(),
            &config,
        )
        .await;
    }

    // Priority: --provider flag > config.toml > default.
    let initial_instance = if cli.theme_demo {
        provider_instance::ProviderInstance::new("test", BackendPreset::Test)
    } else {
        provider_setup::resolve_provider_instance(cli.provider.as_deref(), &config)
            .map_err(|e| io::Error::new(ErrorKind::InvalidInput, e))?
    };

    // Priority: --model flag > config.toml > provider default. Theme demo
    // deliberately overrides both configured provider and model.
    let initial_instance = if cli.theme_demo {
        provider_setup::with_resolved_model(Some("test"), &initial_instance)
    } else {
        provider_setup::with_resolved_model(cli.model.as_deref(), &initial_instance)
    };
    timer.mark("resolve_provider_instance");
    let initial_model = initial_instance.effective_model().to_string();
    let initial_thinking =
        provider_setup::resolve_thinking_level_for_model(&config, &initial_model);
    let window_folder = std::env::current_dir()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| ".".to_string());
    let window_title = format!("ξ - {window_folder}");
    let initial_session_events = if cli.theme_demo {
        theme_demo::demo_events()
    } else {
        Vec::new()
    };

    let (mut terminal, mut keyboard_enhancements_enabled) = terminal::init_terminal(&window_title)?;
    timer.mark("terminal::init_terminal");

    let file_tracker = Arc::new(Mutex::new(build_file_tracker()));
    timer.mark("build_file_tracker");
    let tool_output_log = Arc::new(std::sync::Mutex::new(ToolOutputLog::new("init")));
    let hook_ipc = HookIpcPublisherHandle::new(&config.hook_ipc);

    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".to_string());

    let mut app = App::new(
        initial_instance,
        &initial_model,
        initial_thinking,
        AgentLoopConfig {
            tools: std::collections::HashMap::new(),
            file_tracker: Arc::clone(&file_tracker),
            tool_output_log: Arc::clone(&tool_output_log),
            session_events: initial_session_events.clone(),
            current_model: initial_model.clone(),
            auto_compaction_enabled: true,
            manual_compaction_requested: false,
            manual_compaction_instructions: None,
            executor: std::sync::Arc::new(crate::agent::DefaultToolExecutor::new()),
            system_prompt: None,
            hooks: crate::hooks::load_hooks(&cwd, &config.hooks),
            hook_ipc: hook_ipc.clone(),
            session_id: String::new(),
        },
        config.display.clone(),
        config.throbber.clone(),
    );
    timer.mark("App::new (incl. load_hooks)");
    app.theme = theme;

    let app_event_tx = app.app_event_tx();

    // Record the desired agent now; the actual system-prompt rebuild happens
    // once the context (tools, skills, agents) has been loaded, either by the
    // background task below or synchronously on first submit.
    app.active_agent = config.agent.clone();

    // Load tools, skills, and agents on a background thread so the prompt
    // appears instantly. The result is applied when the `ContextLoaded` event
    // is received by the event loop. If the user submits before that happens
    // (e.g. `--prompt`), the submit path loads the context synchronously.
    {
        let tx = app_event_tx.clone();
        let ft = Arc::clone(&file_tracker);
        tokio::task::spawn_blocking(move || {
            let ctx = load_context(Some(tx.clone()), &ft);
            tx.send_ignore(AppEvent::ContextLoaded(ctx));
        });
    }
    timer.mark("spawn background context load");

    app.init_session_persistence(cwd.clone());
    timer.mark("init_session_persistence");
    if cli.resume {
        app.resume_latest_for_current_cwd();
    }
    #[cfg(feature = "restart")]
    if let Some(session_id) = cli.resume_session.as_deref() {
        app.resume_session_by_id(session_id);
        app.pending_restart_continue = app.complete_pending_restart();
    }
    if !initial_session_events.is_empty() {
        let session_id = app.session.ensure_session_id();
        if let Some(store) = app.session.session_store.as_ref()
            && let Ok(path) = store.resolve_event_log_path(&session_id)
            && let Ok(log) =
                crate::event_log::EventLog::new_from_events(path, &initial_session_events)
        {
            app.session.session_state =
                Some(crate::session_state::SessionState::from_event_log(log));
        }
    }
    let _ipc_server = if cli.enable_session_ipc || config.enable_session_ipc {
        match session_ipc::IpcServer::bind(std::path::Path::new(&cwd), app_event_tx.clone()) {
            Ok(server) => server,
            Err(error) => {
                log::debug!("session IPC unavailable: {error}");
                None
            }
        }
    } else {
        None
    };
    app.provider.instances = config.resolve_effective_providers();
    timer.mark("resolve_effective_providers");
    // Mark provider as explicitly selected when a provider was configured
    // (from config.toml or --provider flag), as opposed to the fallback.
    if config.provider.is_some() || cli.provider.is_some() || cli.theme_demo {
        app.provider.provider_selected = true;
    }
    provider_setup::maybe_warn_thinking_unsupported(&mut app);

    let mut initial_prompt = initial_prompt;

    loop {
        // Build (or re-build) the provider for the current instance.
        // When no provider has been explicitly selected, skip the build
        // to avoid spurious "not authenticated" notices on fresh install.
        timer.mark("pre provider build");
        let provider = if !app.provider.provider_selected {
            Arc::new(provider_setup::UnavailableProvider {
                message: String::new(),
            }) as Arc<dyn LlmProvider + Send + Sync>
        } else {
            match build_provider_for_instance(
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
                    Arc::new(provider_setup::UnavailableProvider { message: msg })
                        as Arc<dyn LlmProvider + Send + Sync>
                }
            }
        };
        timer.mark("build_provider_for_instance");

        if app.login.retry_after_refresh {
            app.login.retry_after_refresh = false;
            app.retry_last_request(&provider);
        }

        if app.login.retry_model_fetch_after_refresh {
            app.login.retry_model_fetch_after_refresh = false;
            app.start_model_fetch(&provider);
        }

        if app.should_auto_query_model() {
            app.start_model_fetch(&provider);
        }

        // On clean install with no provider selected, show the login menu
        // automatically so the user can connect to a service.
        if !app.provider.provider_selected
            && !app.selection.active
            && app.provider.setup_step == ProviderSetupStep::Idle
        {
            app.enter_login_selection_mode();
        }

        if initial_prompt.is_some() && app.provider.provider_selected {
            let prompt = initial_prompt.take().expect("initial prompt was present");
            app.textarea = ratatui_textarea::TextArea::from(vec![prompt]);
            app.submit_chat_message(&provider);
        }

        #[cfg(feature = "restart")]
        if app.pending_restart_continue && app.provider.provider_selected {
            app.pending_restart_continue = false;
            app.launch_turn(&provider);
        }

        match run(&mut terminal, &mut app, &provider, &config, &mut timer).await {
            Ok(RunResult::Quit) | Err(_) => break,

            #[cfg(feature = "restart")]
            Ok(RunResult::Restart) => {
                // The restart_host tool requested a re-exec.  Restore the
                // terminal, then exec the current binary resuming the session.
                let _ = terminal::shutdown_terminal(&mut terminal, keyboard_enhancements_enabled);
                let session_id = app.session.current_session_id.clone().unwrap_or_default();
                let err = crate::restart::exec_self(&session_id);
                // exec() returns only on failure.
                eprintln!("restart failed: {err}");
                std::process::exit(1);
            }

            #[cfg(unix)]
            Ok(RunResult::Terminate(code)) => {
                // OS signal received — completed turns are already persisted
                // via the event log.  Install a last-resort guard against hung
                // cleanup, then restore the terminal and exit.
                terminal::install_termination_guard();
                let _ = terminal::shutdown_terminal(&mut terminal, keyboard_enhancements_enabled);
                std::process::exit(code);
            }

            Ok(RunResult::Suspend) => {
                terminal::suspend_interactive_ui(&mut terminal, keyboard_enhancements_enabled)?;
                terminal = terminal::recreate_terminal(
                    terminal,
                    &mut keyboard_enhancements_enabled,
                    &window_title,
                )?;
            }

            Ok(RunResult::RebuildProvider) => {}

            Ok(RunResult::ReloadContext) => {
                handle_reload_context(&mut app, &file_tracker, app_event_tx.clone());
            }

            Ok(RunResult::NewSession) => {
                handle_new_session(&mut app, &file_tracker, app_event_tx.clone());
            }

            Ok(RunResult::ChangeModel {
                name,
                prompt_thinking_selection,
            }) => {
                handle_change_model(&mut app, &mut config, name, prompt_thinking_selection);
            }

            Ok(RunResult::ChangeProvider(id)) => {
                if handle_change_provider(&mut app, &mut config, id) {
                    continue;
                }
            }

            Ok(RunResult::AddProvider(instance)) => {
                handle_add_provider(&mut app, &mut config, instance);
            }

            Ok(RunResult::UpdateProvider {
                original_id,
                instance,
            }) => {
                handle_update_provider(&mut app, &mut config, original_id, instance);
            }

            Ok(RunResult::RemoveProvider(id)) => {
                handle_remove_provider(&mut app, &mut config, id);
            }

            Ok(RunResult::ChangeThinking(level)) => {
                handle_change_thinking(&mut app, &mut config, level);
            }

            Ok(RunResult::ConfigureProvider {
                instance,
                url,
                api_key,
            }) => {
                handle_configure_provider(&mut app, &mut config, instance, url, api_key);
            }
        }
    }

    terminal::shutdown_terminal(&mut terminal, keyboard_enhancements_enabled)?;

    Ok(())
}

use input::{RunResult, apply_paste, handle_key_event, provider_setup_requires_api_key};

// ── Signal event abstraction ─────────────────────────────────────────────

/// A signal event delivered from the OS to the event loop.
enum SignalEvent {
    /// Process should terminate with the given exit code.
    #[cfg(unix)]
    Terminate(i32),
    /// Process should suspend (SIGTSTP).
    #[cfg(unix)]
    Suspend,
}

/// Wait for the next OS signal that requires action.
///
/// Registers handlers for SIGTERM, SIGINT, SIGHUP, SIGQUIT, and SIGTSTP,
/// then races them against each other on Unix. On other platforms, the future
/// never resolves.
async fn next_signal() -> SignalEvent {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut sigterm =
            signal(SignalKind::terminate()).expect("failed to register SIGTERM handler");
        let mut sigint =
            signal(SignalKind::interrupt()).expect("failed to register SIGINT handler");
        let mut sighup = signal(SignalKind::hangup()).expect("failed to register SIGHUP handler");
        let mut sigquit = signal(SignalKind::quit()).expect("failed to register SIGQUIT handler");
        let mut sigtstp = signal(SignalKind::from_raw(libc::SIGTSTP))
            .expect("failed to register SIGTSTP handler");

        tokio::select! {
            _ = sigterm.recv() => SignalEvent::Terminate(0),
            _ = sigint.recv() => SignalEvent::Terminate(130),   // 128 + SIGINT(2)
            _ = sighup.recv() => SignalEvent::Terminate(129),   // 128 + SIGHUP(1)
            _ = sigquit.recv() => SignalEvent::Terminate(131),  // 128 + SIGQUIT(3)
            _ = sigtstp.recv() => SignalEvent::Suspend,
        }
    }
    #[cfg(not(unix))]
    {
        // On non-Unix, never resolve — signals don't apply.
        std::future::pending().await
    }
}

// ── Inner event loop ──────────────────────────────────────────────────────────

async fn run(
    terminal: &mut Terminal<terminal::Backend>,
    app: &mut App,
    provider: &Arc<dyn LlmProvider + Send + Sync>,
    config: &XiConfig,
    timer: &mut StartupTimer,
) -> io::Result<RunResult> {
    let mut crossterm_events = EventStream::new();
    // The throbber animations are designed for 8 FPS (125 ms per frame).
    let mut tick_interval = tokio::time::interval(std::time::Duration::from_millis(125));
    tick_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    // Timestamp of the most recent key Press event other than Enter itself.
    // Used on Windows to detect paste-injected Enter events (see above).
    #[cfg(windows)]
    let mut last_key_at: Option<std::time::Instant> = None;

    // Draw unconditionally on the first iteration; subsequent draws are only
    // performed when something actually changed (dirty flag).
    let mut needs_redraw = true;

    let draw_frame = |terminal: &mut Terminal<_>, app: &mut App| -> io::Result<()> {
        execute!(io::stdout(), BeginSynchronizedUpdate)?;
        terminal.draw(|f| ui::draw(f, app))?;
        execute!(io::stdout(), EndSynchronizedUpdate)?;
        Ok(())
    };

    let mut first_draw = true;

    loop {
        if needs_redraw {
            draw_frame(&mut *terminal, app)?;
            if first_draw {
                first_draw = false;
                timer.mark("first_draw (prompt visible)");
            }
            needs_redraw = false;
            if app.runtime.pending_finalize {
                app.finalize_submission(provider);
            }
        }

        tokio::select! {
            // ── Terminal input ────────────────────────────────────────────────
            Some(Ok(ev)) = crossterm_events.next() => {
                match ev {
                    Event::Key(key) => {
                        needs_redraw = true;
                        if let Some(result) = handle_key_event(
                            app,
                            provider,
                            config,
                            key,
                            #[cfg(windows)]
                            &mut last_key_at,
                        ) {
                            if matches!(result, RunResult::Suspend) {
                                drop(crossterm_events);
                            }
                            return Ok(result);
                        }
                    }
                    Event::Mouse(mouse) => {
                        match mouse.kind {
                            MouseEventKind::ScrollUp => {
                                app.scroll_up_lines(3);
                                needs_redraw = true;
                            }
                            MouseEventKind::ScrollDown => {
                                app.scroll_down_lines(3);
                                needs_redraw = true;
                            }
                            MouseEventKind::Down(MouseButton::Left) => {
                                let col = mouse.column;
                                let row = mouse.row;
                                if app.mouse_select.handle_mouse_down(
                                    col, row,
                                    app.log_view.auto_scroll,
                                ) {
                                    app.log_view.auto_scroll = false;
                                    needs_redraw = true;
                                }
                            }
                            MouseEventKind::Moved => {
                                if app.mouse_select.handle_mouse_move(mouse.column, mouse.row) {
                                    needs_redraw = true;
                                }
                            }
                            MouseEventKind::Drag(MouseButton::Left) => {
                                app.mouse_select.handle_mouse_drag(
                                    mouse.column, mouse.row,
                                );
                                needs_redraw = true;
                            }
                            MouseEventKind::Up(MouseButton::Left) => {
                                let text = app.mouse_select.handle_mouse_up();
                                let clicked_block = app.mouse_select.clicked_block();
                                if let Some(text) = text {
                                    let _ = crate::clipboard::set_clipboard(&text);
                                } else if let Some(identity) = clicked_block {
                                    if !app.log_view.auto_scroll {
                                        let line_idx = app.mouse_select.log_scroll
                                            + app.mouse_select.drag_start_row
                                                .saturating_sub(app.mouse_select.log_area_top)
                                                as usize;
                                        let block_top = app
                                            .mouse_select
                                            .hit_map
                                            .iter()
                                            .position(|source| {
                                                source.block_identity.as_ref() == Some(&identity)
                                            })
                                            .unwrap_or(line_idx);
                                        let block_screen_top =
                                            block_top.saturating_sub(app.mouse_select.log_scroll);
                                        app.log_view.pending_anchor =
                                            Some((identity.clone(), block_screen_top));
                                    }
                                    app.log_view.toggle_expanded(identity);
                                }
                                if let Some(saved) = app.mouse_select.take_saved_auto_scroll() {
                                    app.log_view.auto_scroll = saved;
                                }
                                needs_redraw = true;
                            }
                            _ => {}
                        }
                    },
                    Event::Paste(text)
                        if !app.login.active => {
                            apply_paste(app, provider, &text);
                            needs_redraw = true;
                        },
                    Event::Resize(_, _) => {
                        needs_redraw = true;
                    }
                    _ => {}
                }

                // If submit() prepared a user message, draw it immediately
                // so the user sees the message appear in the log before we
                // do the disk I/O in finalize_submission().
                if app.runtime.pending_finalize {
                    draw_frame(&mut *terminal, app)?;
                    // Prevent a redundant redraw on the next loop iteration.
                    needs_redraw = false;
                    app.finalize_submission(provider);
                }
            }

            // ── Background app events ───────────────────────────────────────
            Some(ev) = app.recv_app_event() => {
                needs_redraw = true;
                app.apply_app_event(ev);
                if let Some(text) = app.ipc_notifications.first().cloned() {
                    app.ipc_notifications.remove(0);
                    app.submit_with_text(text, provider);
                }
                if let Some((_connection_id, text, reply)) = app.take_ipc_prompt() {
                    app.submit_with_text(text, provider);
                    let accepted = app.runtime.pending_finalize;
                    let _ = reply.send(Ok(serde_json::json!({"accepted": accepted, "mode": "prompt"})));
                }
                if app.login.needs_rebuild {
                    app.login.needs_rebuild = false;
                    return Ok(RunResult::RebuildProvider);
                }
                #[cfg(feature = "restart")]
                if app.pending_restart {
                    return Ok(RunResult::Restart);
                }
            }

            // ── Throbber animation tick ───────────────────────────────────────
            _ = tick_interval.tick() => {
                app.tick();
                // Redraw when the turn is active or when a token refresh is in
                // flight — the throbber should animate in both cases.
                if app.streaming() || app.login.refresh_in_progress {
                    needs_redraw = true;
                }
            }

            // ── OS signals (Unix) ─────────────────────────────────────────────
            sig = next_signal() => {
                match sig {
                    #[cfg(unix)]
                    SignalEvent::Terminate(code) => return Ok(RunResult::Terminate(code)),
                    #[cfg(unix)]
                    SignalEvent::Suspend => return Ok(RunResult::Suspend),
                }
            }
        }
    }
}

// ── Context loading ──────────────────────────────────────────────────────

/// The result of loading the agent context (tools, skills, agents) from disk.
///
/// The system prompt is deliberately *not* included: it is rebuilt from the
/// active agent via [`App::apply_loaded_context`] so that agent filtering is
/// applied consistently regardless of which path loaded the context (startup
/// background task, first-submit fallback, or a reload).
struct LoadedContext {
    tools: agent::ToolRegistry,
    skills: Vec<skills::SkillMeta>,
    agents: Vec<agents::AgentMeta>,
    custom_tool_count: usize,
}

impl std::fmt::Debug for LoadedContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoadedContext")
            .field("tools", &self.tools.len())
            .field("skills", &self.skills.len())
            .field("agents", &self.agents.len())
            .field("custom_tool_count", &self.custom_tool_count)
            .finish()
    }
}

/// Load tools, skills, and agents from disk.  Blocking (file I/O + subprocess
/// spawns for custom-tool `--describe` and Python detection), so callers should
/// run this on a blocking thread where startup latency matters.
fn load_context(
    app_event_tx: Option<tokio::sync::mpsc::UnboundedSender<AppEvent>>,
    file_tracker: &Arc<Mutex<FileTracker>>,
) -> LoadedContext {
    let custom_tools = load_custom_tools(&custom_tool_dirs());
    let custom_tool_count = custom_tools.len();
    let loaded_skills = Arc::new(skills::load_skills());
    let tools = register_builtin_tools(
        app_event_tx,
        Arc::clone(file_tracker),
        Arc::clone(&loaded_skills),
        custom_tools,
    );
    let agents = load_agents();
    LoadedContext {
        tools,
        skills: (*loaded_skills).clone(),
        agents,
        custom_tool_count,
    }
}

// ── Event-loop handlers ──────────────────────────────────────────────────

fn handle_reload_context(
    app: &mut App,
    file_tracker: &Arc<Mutex<FileTracker>>,
    app_event_tx: tokio::sync::mpsc::UnboundedSender<AppEvent>,
) {
    let ctx = load_context(Some(app_event_tx), file_tracker);
    let skills_count = ctx.skills.len();
    let custom_count = ctx.custom_tool_count;
    app.apply_loaded_context(ctx);
    app.push_notice(Message::assistant(format!(
        "[reloaded context: {} skill{}, {} custom tool{}]",
        skills_count,
        if skills_count == 1 { "" } else { "s" },
        custom_count,
        if custom_count == 1 { "" } else { "s" },
    )));
    app.completion.available_models = None;
}

fn handle_new_session(
    app: &mut App,
    file_tracker: &Arc<Mutex<FileTracker>>,
    app_event_tx: tokio::sync::mpsc::UnboundedSender<AppEvent>,
) {
    file_tracker.lock().unwrap().reset();
    app.clear_session_state();

    let ctx = load_context(Some(app_event_tx), file_tracker);
    let skills_count = ctx.skills.len();
    let custom_count = ctx.custom_tool_count;
    app.apply_loaded_context(ctx);
    app.push_notice(Message::assistant(format!(
        "[new session: {} skill{}, {} custom tool{}]",
        skills_count,
        if skills_count == 1 { "" } else { "s" },
        custom_count,
        if custom_count == 1 { "" } else { "s" },
    )));
    app.completion.available_models = None;
}

fn handle_change_model(
    app: &mut App,
    config: &mut XiConfig,
    name: String,
    prompt_thinking_selection: bool,
) {
    app.provider.current_instance.model = Some(name.clone());
    app.provider.current_model = name.clone();
    app.provider.current_model = name;
    app.provider.current_thinking =
        provider_setup::resolve_thinking_level_for_model(config, &app.provider.current_model);
    app.record_model_changed();
    app.record_thinking_level_changed();
    app.completion.available_models = None;
    provider_setup::persist_provider_model_selection_v2(config, app);
    app.provider.instances = config.resolve_effective_providers();
    provider_setup::maybe_warn_thinking_unsupported(app);
    if prompt_thinking_selection
        && thinking_support_for_instance(
            &app.provider.current_instance,
            &app.provider.current_model,
        ) == ThinkingSupport::Applied
    {
        app.enter_thinking_selection_mode();
    }
}

/// Returns `true` if the outer loop should `continue` (skip provider rebuild).
fn handle_change_provider(app: &mut App, config: &mut XiConfig, id: String) -> bool {
    if let Some(inst) = config.resolve_provider(&id) {
        app.provider.provider_selected = true;

        let requires_api_key = provider_setup_requires_api_key(&inst);
        if requires_api_key && inst.api_key.as_deref().unwrap_or("").is_empty() {
            app.provider.pending_setup = Some(PendingProviderSetup::from_instance(&inst));
            app.enter_provider_api_key_input_mode();
            return true;
        }

        if inst.backend_preset.def().auth_mode == AuthMode::OAuthLogin {
            let has_creds = match inst.backend_preset {
                BackendPreset::Copilot => auth::AuthStore::load_default()
                    .ok()
                    .and_then(|s| s.get_copilot())
                    .is_some(),
                BackendPreset::Codex => auth::AuthStore::load_default()
                    .ok()
                    .and_then(|s| s.get_codex())
                    .is_some(),
                BackendPreset::Gemini => auth::AuthStore::load_default()
                    .ok()
                    .and_then(|s| s.get_gemini())
                    .is_some(),
                _ => false,
            };
            if !has_creds {
                app.provider.current_instance = inst;
                app.provider.current_model = provider_setup::resolve_model_for_instance(
                    None,
                    &app.provider.current_instance,
                );
                app.provider.current_thinking = provider_setup::resolve_thinking_level_for_model(
                    config,
                    &app.provider.current_model,
                );
                app.start_login(&id);
                return true;
            }
        }

        app.provider.current_instance = inst;
        app.provider.current_model =
            provider_setup::resolve_model_for_instance(None, &app.provider.current_instance);
        app.provider.current_thinking =
            provider_setup::resolve_thinking_level_for_model(config, &app.provider.current_model);
        app.record_model_changed();
        app.record_thinking_level_changed();
        app.completion.available_models = None;
        provider_setup::persist_provider_model_selection_v2(config, app);
        app.provider.instances = config.resolve_effective_providers();
        provider_setup::maybe_warn_thinking_unsupported(app);
    }
    false
}

fn handle_add_provider(app: &mut App, config: &mut XiConfig, instance: ProviderInstance) {
    app.clear_pending_provider_setup();
    let instance_id = instance.id.clone();
    let current_model_for_instance = provider_setup::resolve_model_for_instance(None, &instance);
    config.upsert_provider(instance.clone());
    config.provider = Some(instance_id);
    app.provider.provider_selected = true;
    if let Err(e) = config.save() {
        log::debug!("failed to persist new provider config: {e}");
        app.push_notice(Message::assistant(format!(
            "[failed to persist config.toml: {e}]"
        )));
    }
    app.provider.current_instance = config.resolve_provider(&instance.id).unwrap_or(instance);
    app.provider.current_model = current_model_for_instance;
    app.provider.current_thinking =
        provider_setup::resolve_thinking_level_for_model(config, &app.provider.current_model);
    app.record_model_changed();
    app.record_thinking_level_changed();
    app.provider.instances = config.resolve_effective_providers();
    app.completion.available_models = None;
    provider_setup::maybe_warn_thinking_unsupported(app);
    app.push_notice(Message::assistant(format!(
        "[added provider {} ({})]",
        app.provider.current_instance.id,
        app.provider.current_instance.backend_preset.label(),
    )));
}

fn handle_update_provider(
    app: &mut App,
    config: &mut XiConfig,
    original_id: Option<String>,
    instance: ProviderInstance,
) {
    app.clear_pending_provider_setup();
    let instance_id = instance.id.clone();
    app.provider.provider_selected = true;
    let current_model_for_instance = provider_setup::resolve_model_for_instance(None, &instance);
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
    }
    app.provider.current_instance = config.resolve_provider(&instance.id).unwrap_or(instance);
    app.provider.current_model = current_model_for_instance;
    app.provider.current_thinking =
        provider_setup::resolve_thinking_level_for_model(config, &app.provider.current_model);
    app.record_model_changed();
    app.record_thinking_level_changed();
    app.provider.instances = config.resolve_effective_providers();
    app.completion.available_models = None;
    provider_setup::maybe_warn_thinking_unsupported(app);
    app.push_notice(Message::assistant(format!(
        "[edited provider {} ({})]",
        app.provider.current_instance.id,
        app.provider.current_instance.backend_preset.label(),
    )));
}

fn handle_remove_provider(app: &mut App, config: &mut XiConfig, id: String) {
    app.clear_pending_provider_setup();
    app.clear_pending_provider_removal();
    if config.remove_provider(&id) {
        if config.provider.as_deref() == Some(id.as_str()) {
            config.provider = config
                .resolve_effective_providers()
                .first()
                .map(|p| p.id.clone());
        }
        if let Err(e) = config.save() {
            log::debug!("failed to persist provider removal: {e}");
            app.push_notice(Message::assistant(format!(
                "[failed to persist config.toml: {e}]"
            )));
        }
        app.provider.current_instance = provider_setup::resolve_default_provider_instance(config);
        app.provider.current_model =
            provider_setup::resolve_model_for_instance(None, &app.provider.current_instance);
        app.provider.current_thinking =
            provider_setup::resolve_thinking_level_for_model(config, &app.provider.current_model);
        app.record_model_changed();
        app.record_thinking_level_changed();
        app.provider.instances = config.resolve_effective_providers();
        app.completion.available_models = None;
        provider_setup::maybe_warn_thinking_unsupported(app);
        app.push_notice(Message::assistant(format!("[removed provider {id}]")));
    }
}

fn handle_change_thinking(app: &mut App, config: &mut XiConfig, level: ThinkingLevel) {
    app.provider.current_thinking = level;
    app.provider.current_thinking = level;
    app.record_thinking_level_changed();
    provider_setup::persist_provider_model_selection_v2(config, app);
    app.provider.instances = config.resolve_effective_providers();
    provider_setup::maybe_warn_thinking_unsupported(app);
}

fn handle_configure_provider(
    app: &mut App,
    config: &mut XiConfig,
    instance: ProviderInstance,
    url: Option<String>,
    api_key: Option<String>,
) {
    app.clear_pending_provider_setup();
    let mut inst = config.resolve_provider(&instance.id).unwrap_or(instance);
    if let Some(url) = url.as_deref() {
        inst.base_url = Some(url.to_string());
    }
    if let Some(api_key) = api_key {
        inst.api_key = Some(api_key.clone());
    }
    config.upsert_provider(inst.clone());
    config.provider = Some(inst.id.clone());
    app.provider.provider_selected = true;
    if let Err(e) = config.save() {
        log::debug!("failed to persist provider config: {e}");
        app.push_notice(Message::assistant(format!(
            "[failed to persist config.toml: {e}]"
        )));
    }
    app.provider.current_instance = inst;
    app.provider.instances = config.resolve_effective_providers();
    app.provider.current_model =
        provider_setup::resolve_model_for_instance(None, &app.provider.current_instance);
    app.provider.current_thinking =
        provider_setup::resolve_thinking_level_for_model(config, &app.provider.current_model);
    app.record_model_changed();
    app.record_thinking_level_changed();
    app.completion.available_models = None;
    provider_setup::maybe_warn_thinking_unsupported(app);
    let endpoint_msg = url
        .map(|u| format!(" endpoint set to {u}"))
        .unwrap_or_default();
    app.push_notice(Message::assistant(format!(
        "[provider {}{endpoint_msg}]",
        app.provider.current_instance.id,
    )));
}

#[cfg(test)]
mod tests {
    use super::print_mode::provider_display_name;
    use super::provider_setup::{self, with_resolved_model};
    use crate::input::normalize_paste_text;
    use crate::{
        config::XiConfig,
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
    fn read_initial_prompt_joins_inline_words() {
        assert_eq!(
            super::read_initial_prompt(Some(vec!["fix".into(), "tests".into()]), None).unwrap(),
            Some("fix tests".into())
        );
    }

    #[test]
    fn read_initial_prompt_rejects_both_sources() {
        let err = super::read_initial_prompt(
            Some(vec!["inline".into()]),
            Some(std::path::PathBuf::from("prompt.txt")),
        )
        .unwrap_err();
        assert!(err.to_string().contains("cannot be used together"));
    }

    #[test]
    fn resolve_provider_instance_accepts_exact_configured_provider_id() {
        let mut cfg = XiConfig::default();
        cfg.providers.push(ProviderInstance::new(
            "work-webui",
            BackendPreset::OpenWebUi,
        ));

        let instance = provider_setup::resolve_provider_instance(Some("work-webui"), &cfg)
            .expect("provider should resolve");

        assert_eq!(instance.id, "work-webui");
        assert_eq!(instance.backend_preset, BackendPreset::OpenWebUi);
    }

    #[test]
    fn resolve_provider_instance_accepts_hidden_test_provider() {
        let cfg = XiConfig::default();

        let instance = provider_setup::resolve_provider_instance(Some("test"), &cfg)
            .expect("test should resolve");

        assert_eq!(instance.id, "test");
        assert_eq!(instance.backend_preset, BackendPreset::Test);
    }

    #[test]
    fn resolve_provider_instance_rejects_unknown_cli_provider() {
        let mut cfg = XiConfig::default();
        cfg.providers
            .push(ProviderInstance::new("copilot", BackendPreset::Copilot));
        cfg.providers.push(ProviderInstance::new(
            "work-webui",
            BackendPreset::OpenWebUi,
        ));

        let err = provider_setup::resolve_provider_instance(Some("does-not-exist"), &cfg)
            .expect_err("unknown provider should be rejected");

        assert_eq!(
            err,
            "unknown provider 'does-not-exist'. Expected one of: codex, copilot, gemini, ollama-com, openai, openrouter, work-webui, test"
        );
    }

    #[test]
    fn resolve_default_provider_instance_prefers_configured_default() {
        let mut cfg = XiConfig {
            provider: Some("work-webui".to_string()),
            ..XiConfig::default()
        };
        cfg.providers
            .push(ProviderInstance::new("copilot", BackendPreset::Copilot));
        cfg.providers.push(ProviderInstance::new(
            "work-webui",
            BackendPreset::OpenWebUi,
        ));

        let instance = provider_setup::resolve_default_provider_instance(&cfg);

        assert_eq!(instance.id, "work-webui");
        assert_eq!(instance.backend_preset, BackendPreset::OpenWebUi);
    }

    #[test]
    fn resolve_default_provider_instance_falls_back_to_first_effective() {
        let cfg = XiConfig::default();

        let instance = provider_setup::resolve_default_provider_instance(&cfg);

        // First effective provider is the first built-in alphabetically: codex.
        assert_eq!(instance.id, "codex");
        assert_eq!(instance.backend_preset, BackendPreset::Codex);
    }

    #[test]
    fn resolve_model_uses_instance_model() {
        let mut inst = ProviderInstance::new("copilot", BackendPreset::Copilot);
        inst.model = Some("gpt-5.3-codex".to_string());
        let model = provider_setup::resolve_model_for_instance(None, &inst);
        assert_eq!(model, "gpt-5.3-codex");
    }

    #[test]
    fn resolve_model_falls_back_to_service_default() {
        let inst = ProviderInstance::new("copilot", BackendPreset::Copilot);
        let model = provider_setup::resolve_model_for_instance(None, &inst);
        assert_eq!(model, BackendPreset::Copilot.default_model());
    }

    #[test]
    fn with_resolved_model_applies_cli_override() {
        let mut inst = ProviderInstance::new("copilot", BackendPreset::Copilot);
        inst.model = Some("gpt-4o".to_string());

        let resolved = with_resolved_model(Some("gpt-5"), &inst);

        assert_eq!(resolved.model.as_deref(), Some("gpt-5"));
        assert_eq!(resolved.effective_model(), "gpt-5");
    }

    #[test]
    fn with_resolved_model_preserves_instance_model_without_override() {
        let mut inst = ProviderInstance::new("copilot", BackendPreset::Copilot);
        inst.model = Some("gpt-4o".to_string());

        let resolved = with_resolved_model(None, &inst);

        assert_eq!(resolved.model.as_deref(), Some("gpt-4o"));
        assert_eq!(resolved.effective_model(), "gpt-4o");
    }

    #[test]
    fn resolve_thinking_uses_model_specific_config() {
        let mut cfg = XiConfig {
            thinking: Some("minimal".to_string()),
            ..XiConfig::default()
        };
        cfg.thinking_by_model
            .insert("gpt-5".to_string(), "high".to_string());

        let level = provider_setup::resolve_thinking_level_for_model(&cfg, "gpt-5");
        assert_eq!(level, ThinkingLevel::High);
    }

    #[test]
    fn resolve_thinking_falls_back_to_global_config() {
        let cfg = XiConfig {
            thinking: Some("minimal".to_string()),
            ..XiConfig::default()
        };
        let level = provider_setup::resolve_thinking_level_for_model(&cfg, "gpt-4o");
        assert_eq!(level, ThinkingLevel::Minimal);
    }

    #[test]
    fn resolve_thinking_defaults_to_off() {
        let cfg = XiConfig::default();
        let level = provider_setup::resolve_thinking_level_for_model(&cfg, "gpt-4o");
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
