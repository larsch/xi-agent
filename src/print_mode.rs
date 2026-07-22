//! Non-interactive (headless / `--print`) mode support.
//!
//! Handles the agent loop when xi is invoked with `--print` / `-p`, streaming
//! the response to stdout and exiting.

use std::io::{self, ErrorKind, Write};
use std::sync::{Arc, Mutex};

use crate::agent::AgentLoopConfig;
use crate::agent::tools::custom::{custom_tool_dirs, load_custom_tools};
use crate::agent::tools::register_builtin_tools;
use crate::agent::types::CancelLevel;
use crate::agent::{AgentEvent, ToolOutputLog, build_system_prompt};
use crate::app_event::AppEvent;
use crate::auth;
use crate::hook_ipc::HookIpcPublisherHandle;
use crate::llm;
use crate::provider::build_provider_for_instance;
use crate::provider_instance::ProviderInstance;
use crate::provider_manager::format_provider_error_for_display;
use crate::provider_setup::{
    resolve_provider_instance, resolve_thinking_level_for_model, with_resolved_model,
};
use crate::skills;
use crate::thinking::ThinkingLevel;
use crate::tool_presentation;

use super::build_file_tracker;

// ── Shared helpers ────────────────────────────────────────────────────────

/// Parameters needed to rebuild a provider after a reactive token refresh.
struct PrintModeProviderCtx<'a> {
    instance: &'a ProviderInstance,
    thinking: ThinkingLevel,
    xi_config: &'a crate::config::XiConfig,
    name: &'a str,
}

pub(crate) fn provider_display_name(instance: &ProviderInstance) -> String {
    instance.backend_preset.label().to_string()
}

/// Returns `true` if `provider` is one of the OAuth providers that support
/// token refresh (copilot, codex, gemini).
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
            let refresh_result = match auth::real_backend_for(provider) {
                Ok(backend) => auth::refresh_token(provider, backend).await,
                Err(e) => Err(e),
            };
            match refresh_result {
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

// ── Main entry ────────────────────────────────────────────────────────────

pub(crate) async fn run_print_mode(
    prompt: String,
    provider_override: &str,
    model_override: Option<&str>,
    config: &crate::config::XiConfig,
) -> io::Result<()> {
    let resolved_instance = with_resolved_model(
        model_override,
        &resolve_provider_instance(Some(provider_override), config)
            .map_err(|e| io::Error::new(ErrorKind::InvalidInput, e))?,
    );
    let current_thinking =
        resolve_thinking_level_for_model(config, resolved_instance.effective_model());
    let provider_name = resolved_instance.backend_preset.id().to_string();

    // Proactive preflight: refresh the token before building the provider so
    // that build_provider reads fresh credentials from the auth store.
    preflight_token_refresh(&provider_name).await;

    let provider = build_provider_for_instance(&resolved_instance, current_thinking, config)
        .map_err(|e| io::Error::other(format!("provider error: {e}")))?;

    let custom_tools = load_custom_tools(&custom_tool_dirs());
    let headless_tracker = Arc::new(Mutex::new(build_file_tracker()));
    let loaded_skills = Arc::new(skills::load_skills());
    let tools = register_builtin_tools(
        None,
        Arc::clone(&headless_tracker),
        Arc::clone(&loaded_skills),
        custom_tools,
    )
    .await;
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".to_string());
    let headless_log = Arc::new(std::sync::Mutex::new(ToolOutputLog::new("headless")));
    let system_prompt = build_system_prompt(&tools, &cwd, &loaded_skills, None);

    let session_events = vec![crate::session_event::SessionEvent::UserMessage {
        content: prompt.clone(),
        timestamp: crate::app_agent_handlers::now_ts(),
    }];

    let loop_config = AgentLoopConfig {
        tools,
        file_tracker: headless_tracker,
        tool_output_log: headless_log,
        session_events,
        current_model: resolved_instance.effective_model().to_string(),
        auto_compaction_enabled: true,
        manual_compaction_instructions: None,
        executor: std::sync::Arc::new(crate::agent::DefaultToolExecutor::new()),
        system_prompt: Some(system_prompt),
        hooks: std::collections::HashMap::new(),
        hook_ipc: HookIpcPublisherHandle::disabled(),
        session_id: String::new(),
    };

    let provider_ctx = PrintModeProviderCtx {
        instance: &resolved_instance,
        thinking: current_thinking,
        xi_config: config,
        name: &provider_name,
    };

    let exit_code = run_print_mode_loop(loop_config, provider, &provider_ctx).await;

    std::process::exit(exit_code);
}

// ── Agent event loops ─────────────────────────────────────────────────────

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
    let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(CancelLevel::None);

    tokio::spawn(async move {
        crate::agent::run_agent_loop(config, provider, tx, steering_rx, cancel_rx).await;
    });

    while let Some(ev) = rx.recv().await {
        let AppEvent::Agent(ev) = ev else {
            continue;
        };
        match ev {
            AgentEvent::TextToken { text, .. } => {
                print!("{text}");
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
            AgentEvent::CompactionDone(outcome) => {
                eprintln!(
                    "compacted: {}k → {}k tokens",
                    outcome.tokens_before / 1000,
                    outcome.tokens_after / 1000
                );
            }
            AgentEvent::ToolCallStart { name, args, .. } => {
                let (label, _) = tool_presentation::tool_invocation_label(
                    &name,
                    &args,
                    None,
                    &crate::config::DisplayConfig::default(),
                );
                eprintln!("{label}");
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
                    let refresh_result = match auth::real_backend_for(ctx.name) {
                        Ok(backend) => auth::refresh_token(ctx.name, backend).await,
                        Err(e) => Err(e),
                    };
                    match refresh_result {
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
    let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(CancelLevel::None);

    // AgentLoopConfig is not Clone; rebuild a minimal headless one for the retry.
    let retry_tracker = Arc::new(Mutex::new(build_file_tracker()));
    let retry_log = Arc::new(std::sync::Mutex::new(ToolOutputLog::new("headless-retry")));
    let custom_tools = load_custom_tools(&custom_tool_dirs());
    let retry_skills = Arc::new(skills::load_skills());
    let retry_tools = register_builtin_tools(
        None,
        Arc::clone(&retry_tracker),
        Arc::clone(&retry_skills),
        custom_tools,
    )
    .await;
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
        hooks: std::collections::HashMap::new(),
        hook_ipc: HookIpcPublisherHandle::disabled(),
        session_id: String::new(),
    };

    tokio::spawn(async move {
        crate::agent::run_agent_loop(retry_config, provider, tx, steering_rx, cancel_rx).await;
    });

    while let Some(ev) = rx.recv().await {
        let AppEvent::Agent(ev) = ev else {
            continue;
        };
        match ev {
            AgentEvent::TextToken { text, .. } => {
                print!("{text}");
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
            AgentEvent::CompactionDone(outcome) => {
                eprintln!(
                    "compacted: {}k → {}k tokens",
                    outcome.tokens_before / 1000,
                    outcome.tokens_after / 1000
                );
            }
            AgentEvent::ToolCallStart { name, args, .. } => {
                let (label, _) = tool_presentation::tool_invocation_label(
                    &name,
                    &args,
                    None,
                    &crate::config::DisplayConfig::default(),
                );
                eprintln!("{label}");
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
