use clap::Parser;
use crossterm::{
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, EventStream, KeyboardEnhancementFlags, MouseEventKind, PopKeyboardEnhancementFlags,
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
mod agent_turn_state;
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
mod context_window;
mod debug_log;
mod dirs;
mod event_log;
mod export;
mod input;
mod live_turn;
mod llm;
mod log_view_state;
mod login_state;
mod markdown;
mod migrate;
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
mod step_back_state;
mod thinking;
mod tool_presentation;
mod tracked;
mod ui;

use agent::tools::custom::custom_tool_dirs;
use agent::{
    AgentEvent, AgentLoopConfig, FileTracker, ToolOutputLog, build_system_prompt,
    tools::{custom::load_custom_tools, register_builtin_tools},
};
use app::App;
use app_event::AppEvent;

use config::XiConfig;
use llm::{LlmEvent, LlmProvider, LlmStream, Message, ModelListFuture};
use provider::{ThinkingSupport, build_provider_for_instance, thinking_support_for_instance};
use provider_instance::ProviderInstance;
use provider_manager::PendingProviderSetup;
use provider_manager::format_provider_error_for_display;
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
    #[arg(long, short = 'p', value_name = "PROMPT", num_args = 1..)]
    print: Option<Vec<String>>,

    /// Print the file-system paths xi uses and exit.
    #[arg(long)]
    print_dirs: bool,
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

#[tokio::main]
async fn main() -> io::Result<()> {
    migrate::run();
    debug_log::init_logging();

    let cli = Cli::parse();

    if cli.print_dirs {
        dirs::print_dirs();
        return Ok(());
    }

    let mut config = match XiConfig::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("warning: failed to load config.toml: {e}");
            XiConfig::default()
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
    let window_title = format!("ξ - {window_folder}");

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
    app.agent_config.system_prompt = Some(system_prompt);
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

        if app.should_auto_query_model() {
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
                app.agent_config.system_prompt = Some(system_prompt);
                app.loaded_skills = loaded_skills;
                app.push_notice(Message::assistant(format!(
                    "[reloaded context: {} skill{}, {} custom tool{}]",
                    skills_count,
                    if skills_count == 1 { "" } else { "s" },
                    custom_count,
                    if custom_count == 1 { "" } else { "s" },
                )));
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
                    if app.should_auto_query_model() {
                        app.start_model_fetch(&provider);
                    }
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
                }
                if let Some(api_key) = api_key {
                    inst.api_key = Some(api_key.clone());
                }
                config.upsert_provider(inst.clone());
                config.provider = Some(inst.id.clone());
                if let Err(e) = config.save() {
                    log::debug!("failed to persist provider config: {e}");
                    app.push_notice(Message::assistant(format!(
                        "[failed to persist config.toml: {e}]"
                    )));
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

use input::{RunResult, apply_paste, handle_key_event, provider_setup_requires_api_key};

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
    config: &XiConfig,
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

/// Resolve the default active [`ProviderInstance`] from config.
///
/// Resolution order:
/// 1. `config.provider` matched against instance ids
/// 2. First instance in `config.providers`
/// 3. Synthetic copilot default
fn resolve_default_provider_instance(config: &XiConfig) -> ProviderInstance {
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
    config: &XiConfig,
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
fn persist_provider_model_selection_v2(config: &mut XiConfig, app: &mut App) {
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
    }
}

fn resolve_thinking_level_for_model(config: &XiConfig, model: &str) -> ThinkingLevel {
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
    xi_config: &'a XiConfig,
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
    config: &XiConfig,
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
        xi_config: config,
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
            AgentEvent::ToolOutputChunk { .. } => {}
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
                                ctx.xi_config,
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
            AgentEvent::ToolOutputChunk { .. } => {}
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
        provider_display_name, resolve_default_provider_instance, resolve_model_for_instance,
        resolve_provider_instance, resolve_thinking_level_for_model,
    };
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
    fn resolve_provider_instance_accepts_exact_configured_provider_id() {
        let mut cfg = XiConfig::default();
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
        let cfg = XiConfig::default();

        let instance = resolve_provider_instance(Some("test"), &cfg).expect("test should resolve");

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

        let err = resolve_provider_instance(Some("does-not-exist"), &cfg)
            .expect_err("unknown provider should be rejected");

        assert_eq!(
            err,
            "unknown provider 'does-not-exist'. Expected one of: copilot, work-webui, test"
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

        let instance = resolve_default_provider_instance(&cfg);

        assert_eq!(instance.id, "work-webui");
        assert_eq!(instance.backend_preset, BackendPreset::OpenWebUi);
    }

    #[test]
    fn resolve_default_provider_instance_falls_back_to_synthetic_copilot() {
        let cfg = XiConfig::default();

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
        let mut cfg = XiConfig {
            thinking: Some("minimal".to_string()),
            ..XiConfig::default()
        };
        cfg.thinking_by_model
            .insert("gpt-5".to_string(), "high".to_string());

        let level = resolve_thinking_level_for_model(&cfg, "gpt-5");
        assert_eq!(level, ThinkingLevel::High);
    }

    #[test]
    fn resolve_thinking_falls_back_to_global_config() {
        let cfg = XiConfig {
            thinking: Some("minimal".to_string()),
            ..XiConfig::default()
        };
        let level = resolve_thinking_level_for_model(&cfg, "gpt-4o");
        assert_eq!(level, ThinkingLevel::Minimal);
    }

    #[test]
    fn resolve_thinking_defaults_to_off() {
        let cfg = XiConfig::default();
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
