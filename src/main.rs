use clap::Parser;
use crossterm::{
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, EventStream, KeyCode, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags,
        MouseEventKind, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{
        EnterAlternateScreen, LeaveAlternateScreen, SetTitle, disable_raw_mode, enable_raw_mode,
    },
};
use futures_util::StreamExt;
use ratatui::{Terminal, backend::CrosstermBackend};
use std::{io, io::ErrorKind, sync::Arc};

mod agent;
mod app;
mod auth;
mod commands;
mod config;
mod debug_log;
mod dirs;
mod llm;
mod provider;
mod session;
mod shell;
mod skills;
mod thinking;
mod tool_presentation;
mod ui;

use agent::{AgentEvent, AgentLoopConfig, build_system_prompt, tools::register_builtin_tools};
use app::{App, InputMode, SelectionResult};
use commands::CommandAction;
use config::TauConfig;
use llm::{LlmEvent, LlmProvider, LlmStream, Message, ModelListFuture};
use provider::{ProviderKind, ThinkingSupport, build_provider, thinking_support_for};
use thinking::ThinkingLevel;

// ── CLI definition ────────────────────────────────────────────────────────────

/// tau — a terminal-based AI coding agent
#[derive(Debug, Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// LLM provider to use (copilot, openai, codex, gemini, ollama).
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
        return run_print_mode(
            prompt,
            cli.provider.as_deref(),
            cli.model.as_deref(),
            &config,
        )
        .await;
    }

    // Priority: --provider flag > config.toml > default.
    let mut current_kind = resolve_provider_kind(cli.provider.as_deref(), &config);

    // Priority: --model flag > config.toml > provider default.
    let mut current_model = resolve_model(cli.model.as_deref(), &current_kind, &config);
    let mut current_thinking = resolve_thinking_level_for_model(&config, &current_model);

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

    let mut app = App::new(
        &current_model,
        &current_kind,
        current_thinking,
        AgentLoopConfig {
            tools: std::collections::HashMap::new(),
            before_tool_call: None,
            after_tool_call: None,
        },
    );

    let tools = register_builtin_tools(Some(app.ask_request_tx()));
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".to_string());
    app.init_session_persistence(cwd.clone());
    let loaded_skills = skills::load_skills();
    let system_prompt = build_system_prompt(&tools, &cwd, &loaded_skills);
    app.agent_config.tools = tools;
    app.system_prompt = Some(system_prompt);
    app.loaded_skills = loaded_skills;
    maybe_warn_thinking_unsupported(&mut app, &current_kind, &current_model, current_thinking);

    loop {
        // Build (or re-build) the provider for the current kind + model.
        let provider =
            match build_provider(&current_kind, &current_model, current_thinking, &config) {
                Ok(p) => p,
                Err(e) => {
                    let msg = format!("[provider unavailable: {e}]");
                    log::debug!(
                        "provider build failed: provider={} model={} err={}",
                        current_kind.name(),
                        current_model,
                        e
                    );
                    app.messages.push(llm::Message::assistant(msg.clone()));
                    app.mark_log_dirty();
                    Arc::new(UnavailableProvider { message: msg })
                        as Arc<dyn LlmProvider + Send + Sync>
                }
            };

        if app.retry_after_refresh {
            app.retry_after_refresh = false;
            app.retry_last_request(&provider);
        }

        if app.retry_model_fetch_after_refresh {
            app.retry_model_fetch_after_refresh = false;
            app.start_model_fetch(&provider);
        }

        match run(&mut terminal, &mut app, &provider).await {
            Ok(RunResult::Quit) | Err(_) => break,

            Ok(RunResult::RebuildProvider) => {}

            Ok(RunResult::ReloadContext) => {
                let loaded_skills = skills::load_skills();
                let system_prompt =
                    build_system_prompt(&app.agent_config.tools, &cwd, &loaded_skills);
                let skills_count = loaded_skills.len();
                app.system_prompt = Some(system_prompt);
                app.loaded_skills = loaded_skills;
                app.messages.push(Message::assistant(format!(
                    "[reloaded context: {} skill{}]",
                    skills_count,
                    if skills_count == 1 { "" } else { "s" }
                )));
                app.mark_log_dirty();
                app.available_models = None;
            }

            Ok(RunResult::ChangeModel {
                name,
                prompt_thinking_selection,
            }) => {
                app.current_model = name.clone();
                current_model = name;
                current_thinking = resolve_thinking_level_for_model(&config, &current_model);
                app.current_thinking = current_thinking;
                // Invalidate cached model list so the next fetch is fresh.
                app.available_models = None;
                persist_provider_model_selection(
                    &mut config,
                    &current_kind,
                    &current_model,
                    current_thinking,
                    &mut app,
                );
                maybe_warn_thinking_unsupported(
                    &mut app,
                    &current_kind,
                    &current_model,
                    current_thinking,
                );
                if prompt_thinking_selection
                    && thinking_support_for(&current_kind, &current_model)
                        == ThinkingSupport::Applied
                {
                    app.enter_thinking_selection_mode();
                }
            }

            Ok(RunResult::ChangeProvider(name)) => {
                if let Some(kind) = ProviderKind::from_name(&name) {
                    current_kind = kind;
                    current_model = resolve_model(None, &current_kind, &config);
                    app.current_model = current_model.clone();
                    current_thinking = resolve_thinking_level_for_model(&config, &current_model);
                    app.current_thinking = current_thinking;
                    app.current_provider = current_kind.name().to_string();
                    app.available_models = None;
                    persist_provider_model_selection(
                        &mut config,
                        &current_kind,
                        &current_model,
                        current_thinking,
                        &mut app,
                    );
                    maybe_warn_thinking_unsupported(
                        &mut app,
                        &current_kind,
                        &current_model,
                        current_thinking,
                    );
                }
                // Unknown name: silently ignore and loop (provider unchanged).
            }

            Ok(RunResult::ChangeThinking(level)) => {
                current_thinking = level;
                app.current_thinking = level;
                persist_provider_model_selection(
                    &mut config,
                    &current_kind,
                    &current_model,
                    current_thinking,
                    &mut app,
                );
                maybe_warn_thinking_unsupported(
                    &mut app,
                    &current_kind,
                    &current_model,
                    current_thinking,
                );
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
    ChangeThinking(ThinkingLevel),
}

fn normalize_paste_text(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
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

async fn run(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    provider: &Arc<dyn LlmProvider + Send + Sync>,
) -> io::Result<RunResult> {
    let mut crossterm_events = EventStream::new();

    loop {
        terminal.draw(|f| ui::draw(f, app))?;

        tokio::select! {
            // ── Terminal input ────────────────────────────────────────────────
            Some(Ok(ev)) = crossterm_events.next() => {
                match ev {
                    Event::Key(key) => {
                        // On Windows with keyboard enhancement flags enabled,
                        // Crossterm can emit both Press and Release key events.
                        // Ignore Release so one shortcut maps to one action.
                        if key.kind == KeyEventKind::Release {
                            continue;
                        }

                        if key.code == KeyCode::Char('c')
                            && key.modifiers.contains(KeyModifiers::CONTROL)
                        {
                            if app.input_mode == InputMode::Shell {
                                app.exit_shell_mode();
                                continue;
                            }
                            return Ok(RunResult::Quit);
                        }

                        if key.code == KeyCode::Char('d')
                            && key.modifiers.contains(KeyModifiers::CONTROL)
                        {
                            if app.input_mode == InputMode::Shell {
                                if app.shell_input_is_empty() {
                                    app.exit_shell_mode();
                                }
                                continue;
                            }

                            let input_is_empty = app
                                .textarea
                                .lines()
                                .iter()
                                .all(|line| line.trim().is_empty());
                            if input_is_empty {
                                return Ok(RunResult::Quit);
                            }
                            continue;
                        }

                        if key.code == KeyCode::Char('i')
                            && key.modifiers.contains(KeyModifiers::CONTROL)
                        {
                            app.toggle_info();
                            continue;
                        }

                        if key.code == KeyCode::Char('r')
                            && key.modifiers.contains(KeyModifiers::CONTROL)
                        {
                            app.resume_latest_for_current_cwd();
                            continue;
                        }

                        // ── Shell input mode ─────────────────────────────────
                        if app.input_mode == InputMode::Shell {
                            if key.code == KeyCode::Char('s')
                                && key.modifiers.contains(KeyModifiers::CONTROL)
                            {
                                app.cycle_shell();
                                continue;
                            }

                            match key.code {
                                KeyCode::Esc => {
                                    app.exit_shell_mode();
                                }
                                KeyCode::Backspace if app.shell_input_is_empty() => {
                                    app.exit_shell_mode();
                                }
                                KeyCode::Enter if key.modifiers.is_empty() => {
                                    app.submit_shell_command();
                                }
                                _ => {
                                    app.shell_textarea.input(Event::Key(key));
                                }
                            }
                            continue;
                        }

                        // ── Selection menu mode ───────────────────────────────
                        if app.selection_mode {
                            match key.code {
                                KeyCode::Up => app.selection_select_prev(),
                                KeyCode::Down => app.selection_select_next(),
                                KeyCode::Backspace => app.selection_backspace(),
                                KeyCode::Char(c)
                                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                                        && !key.modifiers.contains(KeyModifiers::ALT) =>
                                {
                                    app.selection_add_char(c);
                                }
                                KeyCode::Enter if key.modifiers.is_empty() => {
                                    match app.apply_selection() {
                                        Some(SelectionResult::Model(m)) => {
                                            return Ok(RunResult::ChangeModel {
                                                name: m,
                                                prompt_thinking_selection: true,
                                            });
                                        }
                                        Some(SelectionResult::Thinking(level)) => {
                                            return Ok(RunResult::ChangeThinking(level));
                                        }
                                        Some(SelectionResult::Provider(p)) => {
                                            return Ok(RunResult::ChangeProvider(p));
                                        }
                                        Some(SelectionResult::LoginProvider(p)) => {
                                            app.start_login(&p);
                                        }
                                        Some(SelectionResult::ResumeSession(id)) => {
                                            app.resume_session_by_id(&id);
                                        }
                                        Some(SelectionResult::AskOption(answer)) => {
                                            app.select_pending_ask_option(answer);
                                        }
                                        Some(SelectionResult::AskFreeform) => {
                                            app.enter_ask_freeform_mode();
                                        }
                                        Some(SelectionResult::LoginAction(action)) => {
                                            app.apply_login_action(action);
                                        }
                                        None => {}
                                    }
                                }
                                KeyCode::Esc => {
                                    if app.streaming {
                                        if app.has_pending_ask() {
                                            app.cancel_pending_ask();
                                        }
                                        app.abort_agent_loop();
                                    } else if app.has_pending_ask() {
                                        app.cancel_pending_ask();
                                    } else {
                                        app.exit_selection_mode();
                                    }
                                }
                                _ => {}
                            }
                            continue;
                        }

                        if key.code == KeyCode::Esc {
                            if app.streaming {
                                if app.has_pending_ask() {
                                    app.cancel_pending_ask();
                                }
                                app.abort_agent_loop();
                            } else if app.has_pending_ask() {
                                app.cancel_pending_ask();
                            } else if app.login_active {
                                app.cancel_login();
                            } else if app.in_slash_mode() {
                                app.reset_textarea();
                            }
                            continue;
                        }

                        if app.login_active {
                            if key.code == KeyCode::Enter && key.modifiers.is_empty() {
                                app.enter_login_action_menu();
                            }
                            continue;
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
                                continue;
                            }
                        }

                        match key.code {
                            KeyCode::PageUp   => app.scroll_up(),
                            KeyCode::PageDown => app.scroll_down(),

                            KeyCode::Up => {
                                if !app.completions.is_empty() {
                                    app.completion_select_prev();
                                } else {
                                    app.textarea.input(Event::Key(key));
                                }
                            }
                            KeyCode::Down => {
                                if !app.completions.is_empty() {
                                    app.completion_select_next();
                                } else {
                                    app.textarea.input(Event::Key(key));
                                }
                            }

                            KeyCode::Tab => {
                                if !app.completions.is_empty() {
                                    app.apply_completion();
                                }
                            }

                            KeyCode::Enter if key.modifiers.is_empty() => {
                                if app.has_pending_ask() {
                                    app.submit_pending_ask_answer();
                                } else if app.in_slash_mode() {
                                    let input = app.textarea.lines().first().cloned().unwrap_or_default();
                                    let input = input.trim().to_string();
                                    app.reset_textarea();

                                    match commands::parse(&input) {
                                        Some(CommandAction::New) => {
                                            app.new_conversation();
                                        }
                                        Some(CommandAction::Reload) => {
                                            return Ok(RunResult::ReloadContext);
                                        }
                                        Some(CommandAction::Quit) => {
                                            return Ok(RunResult::Quit);
                                        }
                                        Some(CommandAction::Model(name)) => {
                                            return Ok(RunResult::ChangeModel {
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
                                            return Ok(RunResult::ChangeProvider(name));
                                        }
                                        Some(CommandAction::ProviderNoArg) => {
                                            app.enter_provider_selection_mode();
                                        }
                                        Some(CommandAction::Thinking(raw)) => {
                                            let thinking_supported =
                                                ProviderKind::from_name(&app.current_provider)
                                                    .map(|kind| {
                                                        thinking_support_for(
                                                            &kind,
                                                            &app.current_model,
                                                        ) == ThinkingSupport::Applied
                                                    })
                                                    .unwrap_or(false);
                                            if !thinking_supported {
                                                continue;
                                            }
                                            match ThinkingLevel::parse(&raw) {
                                                Some(level) => {
                                                    return Ok(RunResult::ChangeThinking(level));
                                                }
                                                None => {
                                                    app.messages.push(llm::Message::assistant(format!(
                                                        "[invalid thinking level: '{raw}' (use off|minimal|low|medium|high|xhigh)]"
                                                    )));
                                                    app.mark_log_dirty();
                                                }
                                            }
                                        }
                                        Some(CommandAction::ThinkingNoArg) => {
                                            let thinking_supported =
                                                ProviderKind::from_name(&app.current_provider)
                                                    .map(|kind| {
                                                        thinking_support_for(
                                                            &kind,
                                                            &app.current_model,
                                                        ) == ThinkingSupport::Applied
                                                    })
                                                    .unwrap_or(false);
                                            if thinking_supported {
                                                app.enter_thinking_selection_mode();
                                            }
                                        }
                                        Some(CommandAction::Login(provider)) => {
                                            app.start_login(&provider);
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
                                        Some(CommandAction::Skill { name, args }) => {
                                            match app.loaded_skills.iter().find(|s| s.name == name) {
                                                Some(skill) => {
                                                    match skills::expand_skill(skill, &args) {
                                                        Ok(expanded) => {
                                                            app.submit_with_text(expanded, provider);
                                                        }
                                                        Err(e) => {
                                                            app.messages.push(llm::Message::assistant(
                                                                format!("[skill error: {e}]"),
                                                            ));
                                                            app.mark_log_dirty();
                                                        }
                                                    }
                                                }
                                                None => {
                                                    app.messages.push(llm::Message::assistant(
                                                        format!("[unknown skill: '{name}']"),
                                                    ));
                                                    app.mark_log_dirty();
                                                }
                                            }
                                        }
                                        None => {}
                                    }
                                } else if app.streaming {
                                    app.enqueue_steering_from_input();
                                } else {
                                    app.submit(provider);
                                }
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
                    }
                    Event::Mouse(mouse) => match mouse.kind {
                        MouseEventKind::ScrollUp   => app.scroll_up_lines(3),
                        MouseEventKind::ScrollDown => app.scroll_down_lines(3),
                        _ => {}
                    },
                    Event::Paste(text) => {
                        if !app.login_active {
                            if app.selection_mode {
                                app.exit_selection_mode();
                            }
                            if app.input_mode == InputMode::Shell {
                                app.shell_textarea.insert_str(normalize_paste_text(&text));
                            } else {
                                app.textarea.insert_str(normalize_paste_text(&text));
                                app.update_completions();
                                if app.should_fetch_models() {
                                    app.start_model_fetch(provider);
                                }
                            }
                        }
                    },
                    _ => {}
                }
            }

            // ── LLM streaming events ──────────────────────────────────────────
            Some(ev) = app.event_rx.recv() => {
                app.apply_event(ev);
                app.apply_events();
            }

            // ── Model list fetched ────────────────────────────────────────────
            Some(models) = app.models_rx.recv() => {
                app.apply_model_list(models);
            }

            // ── Login flow events ─────────────────────────────────────────────
            Some(ev) = app.login_rx.recv() => {
                app.apply_login_event(ev);
                if app.login_needs_rebuild {
                    app.login_needs_rebuild = false;
                    return Ok(RunResult::RebuildProvider);
                }
            }

            // ── ask_user tool requests ─────────────────────────────────────────
            Some(req) = app.ask_rx.recv() => {
                app.receive_ask_request(req);
            }
        }
    }
}

fn resolve_provider_kind(cli_override: Option<&str>, config: &TauConfig) -> ProviderKind {
    cli_override
        .and_then(ProviderKind::from_name)
        .or_else(|| config.provider.as_deref().and_then(ProviderKind::from_name))
        .unwrap_or(ProviderKind::Copilot)
}

fn resolve_model(cli_override: Option<&str>, kind: &ProviderKind, config: &TauConfig) -> String {
    cli_override
        .map(ToString::to_string)
        .or_else(|| match kind {
            ProviderKind::Copilot => config.copilot.model.clone(),
            ProviderKind::OpenAi => config.openai.model.clone(),
            ProviderKind::Codex => config.codex.model.clone(),
            ProviderKind::Gemini => config.gemini.model.clone(),
            ProviderKind::Ollama => config.ollama.model.clone(),
        })
        .unwrap_or_else(|| kind.default_model().to_string())
}

fn persist_provider_model_selection(
    config: &mut TauConfig,
    kind: &ProviderKind,
    model: &str,
    thinking: ThinkingLevel,
    app: &mut App,
) {
    config.provider = Some(kind.name().to_string());
    config.thinking = Some(thinking.as_str().to_string());
    config
        .thinking_by_model
        .insert(model.to_string(), thinking.as_str().to_string());
    match kind {
        ProviderKind::Copilot => config.copilot.model = Some(model.to_string()),
        ProviderKind::OpenAi => config.openai.model = Some(model.to_string()),
        ProviderKind::Codex => config.codex.model = Some(model.to_string()),
        ProviderKind::Gemini => config.gemini.model = Some(model.to_string()),
        ProviderKind::Ollama => config.ollama.model = Some(model.to_string()),
    }

    if let Err(e) = config.save() {
        log::debug!("failed to persist provider/model config: {}", e);
        app.messages.push(Message::assistant(format!(
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

fn maybe_warn_thinking_unsupported(
    _app: &mut App,
    kind: &ProviderKind,
    model: &str,
    thinking: ThinkingLevel,
) {
    if thinking == ThinkingLevel::Off {
        return;
    }
    if let ThinkingSupport::Ignored(reason) = thinking_support_for(kind, model) {
        log::debug!(
            "thinking '{}' ignored for provider={} model={}: {}",
            thinking.as_str(),
            kind.name(),
            model,
            reason
        );
    }
}

// ── Non-interactive helpers ───────────────────────────────────────────────────

/// Non-interactive mode: run the agent loop for `prompt`, stream output to
/// stdout, and exit when the loop finishes.
async fn run_print_mode(
    prompt: String,
    provider_override: Option<&str>,
    model_override: Option<&str>,
    config: &TauConfig,
) -> io::Result<()> {
    let current_kind = resolve_provider_kind(provider_override, config);
    let current_model = resolve_model(model_override, &current_kind, config);
    let current_thinking = resolve_thinking_level_for_model(config, &current_model);

    let provider = build_provider(&current_kind, &current_model, current_thinking, config)
        .map_err(|e| io::Error::other(format!("provider error: {e}")))?;

    let tools = register_builtin_tools(None);
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".to_string());
    let loaded_skills = skills::load_skills();
    let system_prompt = build_system_prompt(&tools, &cwd, &loaded_skills);

    let messages: Vec<Message> = vec![Message::system(&system_prompt), Message::user(&prompt)];

    let config = AgentLoopConfig {
        tools,
        before_tool_call: None,
        after_tool_call: None,
    };

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
    let (_steering_tx, steering_rx) = tokio::sync::mpsc::unbounded_channel::<String>();

    tokio::spawn(async move {
        agent::run_agent_loop(messages, config, provider, tx, steering_rx).await;
    });

    let mut exit_code = 0i32;

    while let Some(ev) = rx.recv().await {
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
            AgentEvent::ToolIntentStart => {
                // No-op in print mode.
            }
            AgentEvent::SteeringConsumed { .. } => {
                // No-op in print mode.
            }
            AgentEvent::ToolCallStart { name, args, .. } => {
                eprintln!("{}", tool_presentation::tool_invocation_label(&name, &args));
            }
            AgentEvent::ToolCallEnd {
                name: _, result, ..
            } => {
                if result.is_error {
                    eprintln!("  ✗ {}", result.content.lines().next().unwrap_or("error"));
                }
            }
            AgentEvent::TurnEnd => {}
            AgentEvent::Done => {
                println!(); // final newline after streamed output
                break;
            }
            AgentEvent::Error(e) => {
                eprintln!("error: {e}");
                exit_code = 1;
                break;
            }
        }
    }

    std::process::exit(exit_code);
}

#[cfg(test)]
mod tests {
    use super::{normalize_paste_text, resolve_model, resolve_thinking_level_for_model};
    use crate::{config::TauConfig, provider::ProviderKind, thinking::ThinkingLevel};

    #[test]
    fn normalize_paste_text_converts_crlf_and_cr_to_lf() {
        let pasted = "a\r\nb\rc\n";
        assert_eq!(normalize_paste_text(pasted), "a\nb\nc\n");
    }

    #[test]
    fn resolve_model_prefers_cli_over_config() {
        let cfg = TauConfig {
            openai: crate::config::OpenAiConfig {
                model: Some("gpt-4.1".to_string()),
                ..Default::default()
            },
            ..TauConfig::default()
        };

        let model = resolve_model(Some("gpt-4.1-mini"), &ProviderKind::OpenAi, &cfg);
        assert_eq!(model, "gpt-4.1-mini");
    }

    #[test]
    fn resolve_model_uses_selected_provider_config() {
        let cfg = TauConfig {
            openai: crate::config::OpenAiConfig {
                model: Some("gpt-4.1".to_string()),
                ..Default::default()
            },
            copilot: crate::config::CopilotConfig {
                model: Some("gpt-5.3-codex".to_string()),
            },
            ..TauConfig::default()
        };

        let model = resolve_model(None, &ProviderKind::Copilot, &cfg);
        assert_eq!(model, "gpt-5.3-codex");
    }

    #[test]
    fn resolve_model_falls_back_to_provider_default() {
        let cfg = TauConfig {
            openai: crate::config::OpenAiConfig {
                model: Some("gpt-4.1".to_string()),
                ..Default::default()
            },
            ..TauConfig::default()
        };

        let model = resolve_model(None, &ProviderKind::Copilot, &cfg);
        assert_eq!(model, ProviderKind::Copilot.default_model());
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
}
