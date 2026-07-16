use std::sync::Arc;

use futures_util::StreamExt;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::app_event::{AppEvent, SendIgnore};
use crate::hooks::{
    HookConfig, HookPoint, empty_payload, ipc_external_change_payload, ipc_on_error_payload,
    ipc_pre_tool_payload, ipc_status_update_payload, ipc_tool_intent_payload, maybe_run_hook,
    post_tool_json, tool_json,
};
use crate::llm::{
    AssistantPhase, LlmEvent, LlmProvider, LlmRequestContext, Message, ToolDefinition, UsageStats,
};
use crate::projection::LlmProjection;
use crate::session_event::{CompactionTrigger, SessionEvent};
use file_tracker::build_notification;

pub mod compaction;
pub mod file_tracker;
pub mod system_prompt;
pub mod tool_output_log;
pub mod tools;
pub mod types;

#[cfg(test)]
mod tests;

pub use file_tracker::FileTracker;
pub use system_prompt::build_system_prompt;
pub use tool_output_log::ToolOutputLog;
pub use types::{
    AgentEvent, AgentLoopConfig, DefaultToolExecutor, ToolExecutor, ToolRegistry, ToolResult,
};

// ── TurnOutcome ───────────────────────────────────────────────────────────────

/// The result of one LLM streaming turn.
#[derive(Debug)]
enum TurnOutcome {
    /// The model produced a final answer with no tool calls.
    FinalAnswer {
        text: String,
        thinking: Option<String>,
        phase: AssistantPhase,
        usage: Option<UsageStats>,
    },
    /// The model produced tool calls that must be executed.
    ToolCalls {
        text: String,
        thinking: Option<String>,
        phase: AssistantPhase,
        usage: Option<UsageStats>,
        calls: Vec<(String, String, serde_json::Value)>,
    },
    /// The stream failed with a context-overflow error eligible for retry.
    ContextOverflow(crate::llm::ProviderError),
    /// The stream failed with a non-recoverable error.
    Error(crate::llm::ProviderError),
    /// The model indicated a tool call was coming but no call arrived
    /// (e.g. truncated by max_tokens).
    ToolIntentWithNoCall,
}

// ── BatchOutcome ──────────────────────────────────────────────────────────────

/// The result of executing a batch of tool calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BatchOutcome {
    /// All tool calls completed normally.
    Completed,
    /// The user cancelled; the loop should stop.
    Cancelled,
}

struct HookDispatchContext<'a> {
    hooks: &'a std::collections::HashMap<HookPoint, Vec<HookConfig>>,
    hook_ipc: &'a crate::hooks::HookIpcPublisherHandle,
    session_id: &'a str,
}

// ── Helpers ───────────────────────────────────────────────────────────────────

async fn drain_steering_messages(
    steering_rx: &mut UnboundedReceiver<String>,
    session_events: &mut Vec<SessionEvent>,
    tx: &UnboundedSender<AppEvent>,
    hook_ctx: &HookDispatchContext<'_>,
) -> bool {
    let mut consumed = false;
    while let Ok(text) = steering_rx.try_recv() {
        tx.send_ignore(AppEvent::Agent(AgentEvent::SteeringConsumed {
            text: text.clone(),
        }));
        session_events.push(SessionEvent::UserMessage {
            content: text.clone(),
            timestamp: 0,
        });
        hook_ctx.hook_ipc.publish(
            hook_ctx.session_id,
            HookPoint::OnSteeringConsumed,
            None,
            crate::hooks::ipc_steering_consumed_payload(&text),
        );
        crate::hooks::maybe_run_hook(
            hook_ctx.hooks,
            HookPoint::OnSteeringConsumed,
            hook_ctx.session_id,
            Some(crate::hooks::on_steering_consumed_json(&text)),
            None,
        )
        .await;
        consumed = true;
    }
    consumed
}

fn record_tool_call_result(
    session_events: &mut Vec<SessionEvent>,
    id: &str,
    name: &str,
    args: serde_json::Value,
    result: ToolResult,
) {
    session_events.push(SessionEvent::ToolCall {
        id: id.to_string(),
        name: name.to_string(),
        args,
        include_in_llm: true,
        timestamp: 0,
    });
    session_events.push(SessionEvent::ToolResult {
        id: id.to_string(),
        name: name.to_string(),
        content: result.content.as_text().to_string(),
        is_error: result.is_error,
        display_range: None,
        include_in_llm: true,
        timestamp: 0,
    });
}

fn send_compaction_failed_status(tx: &UnboundedSender<AppEvent>, message: &str) {
    tx.send_ignore(AppEvent::Agent(AgentEvent::StatusUpdate(format!(
        "compaction failed: {message}; continuing without compaction."
    ))));
}

async fn emit_compaction(
    provider: Arc<dyn LlmProvider>,
    tx: &UnboundedSender<AppEvent>,
    session_events: &[SessionEvent],
    config: &AgentLoopConfig,
    trigger_reason: CompactionTrigger,
    user_instructions: Option<String>,
) -> Result<compaction::CompactionOutcome, crate::llm::ProviderError> {
    // ── Pre-turn hook equivalent for compaction ────────────────────────────
    config.hook_ipc.publish(
        &config.session_id,
        HookPoint::OnCompacting,
        None,
        empty_payload(),
    );
    crate::hooks::maybe_run_hook(
        &config.hooks,
        HookPoint::OnCompacting,
        &config.session_id,
        None,
        None,
    )
    .await;

    tx.send_ignore(AppEvent::Agent(AgentEvent::Compacting));
    let outcome = compaction::compact_events(
        provider,
        session_events,
        &config.current_model,
        trigger_reason,
        user_instructions,
    )
    .await?;
    tx.send_ignore(AppEvent::Agent(AgentEvent::CompactionDone(outcome.clone())));
    config.hook_ipc.publish(
        &config.session_id,
        HookPoint::OnCompactionDone,
        None,
        crate::hooks::ipc_compaction_done_payload(
            outcome.tokens_before,
            outcome.tokens_after,
            outcome.retained_event_count,
        ),
    );
    crate::hooks::maybe_run_hook(
        &config.hooks,
        HookPoint::OnCompactionDone,
        &config.session_id,
        Some(crate::hooks::on_compaction_done_json(
            outcome.tokens_before,
            outcome.tokens_after,
            outcome.retained_event_count,
        )),
        None,
    )
    .await;
    Ok(outcome)
}

// ── stream_assistant_turn ─────────────────────────────────────────────────────

/// Drive one LLM streaming turn and return a typed [`TurnOutcome`].
///
/// Streams all events from the provider, accumulates text/thinking/tool-calls,
/// and returns the appropriate outcome variant. No session state is mutated.
async fn stream_assistant_turn(
    provider: Arc<dyn LlmProvider>,
    messages: Vec<Message>,
    tool_defs: Vec<ToolDefinition>,
    tx: &UnboundedSender<AppEvent>,
    overflow_retry_remaining: usize,
    hook_ctx: HookDispatchContext<'_>,
) -> TurnOutcome {
    // Build a lookup from tool name → streaming_field for intent events.
    let streaming_fields: std::collections::HashMap<String, Option<String>> = tool_defs
        .iter()
        .map(|t| (t.name.clone(), t.streaming_field.clone()))
        .collect();

    let mut stream = provider.stream_chat_with_tools(
        messages,
        tool_defs,
        LlmRequestContext {
            prompt_cache_key: Some(hook_ctx.session_id.to_string()),
        },
    );

    let mut assistant_text = String::new();
    let mut assistant_thinking: Option<String> = None;
    let mut assistant_phase = AssistantPhase::Unknown;
    let mut pending_tool_calls: Vec<(String, String, serde_json::Value)> = Vec::new();
    let mut tool_intent_seen = false;
    let mut latest_usage = None;
    let mut first_thinking_token = true;
    let mut first_text_token = true;

    while let Some(ev) = stream.next().await {
        match ev {
            LlmEvent::Token { text, phase } => {
                if first_text_token {
                    first_text_token = false;
                    hook_ctx.hook_ipc.publish(
                        hook_ctx.session_id,
                        HookPoint::OnFirstTextToken,
                        None,
                        empty_payload(),
                    );
                    maybe_run_hook(
                        hook_ctx.hooks,
                        HookPoint::OnFirstTextToken,
                        hook_ctx.session_id,
                        None,
                        None,
                    )
                    .await;
                }
                tx.send_ignore(AppEvent::Agent(AgentEvent::TextToken {
                    text: text.clone(),
                    phase,
                }));
                assistant_text.push_str(&text);
                if phase != AssistantPhase::Unknown {
                    assistant_phase = phase;
                }
            }
            LlmEvent::ThinkingToken(t) => {
                if first_thinking_token {
                    first_thinking_token = false;
                    hook_ctx.hook_ipc.publish(
                        hook_ctx.session_id,
                        HookPoint::OnFirstThinkingToken,
                        None,
                        empty_payload(),
                    );
                    maybe_run_hook(
                        hook_ctx.hooks,
                        HookPoint::OnFirstThinkingToken,
                        hook_ctx.session_id,
                        None,
                        None,
                    )
                    .await;
                }
                tx.send_ignore(AppEvent::Agent(AgentEvent::ThinkingToken(t.clone())));
                assistant_thinking
                    .get_or_insert_with(String::new)
                    .push_str(&t);
            }
            LlmEvent::Usage(usage) => {
                latest_usage = Some(usage);
                tx.send_ignore(AppEvent::Agent(AgentEvent::Usage(usage)));
            }
            LlmEvent::ToolCallStart { id, name } => {
                let streaming_field = streaming_fields.get(&name).and_then(|f| f.clone());
                tx.send_ignore(AppEvent::Agent(AgentEvent::ToolCallIntent {
                    id,
                    name: name.clone(),
                    streaming_field,
                }));
                assistant_phase = AssistantPhase::Provisional;
                tool_intent_seen = true;
                hook_ctx.hook_ipc.publish(
                    hook_ctx.session_id,
                    HookPoint::OnToolIntent,
                    Some(&name),
                    ipc_tool_intent_payload(&name),
                );
                crate::hooks::maybe_run_hook(
                    hook_ctx.hooks,
                    HookPoint::OnToolIntent,
                    hook_ctx.session_id,
                    Some(crate::hooks::tool_json(&name, &serde_json::Value::Null)),
                    Some(&name),
                )
                .await;
            }
            LlmEvent::ToolCallArgsDelta { id, partial_json } => {
                tx.send_ignore(AppEvent::Agent(AgentEvent::ToolCallArgsDelta {
                    id,
                    partial_json,
                }));
            }
            LlmEvent::ToolCall { id, name, args } => {
                pending_tool_calls.push((id, name, args));
            }
            LlmEvent::Done => break,
            LlmEvent::Error(e) => {
                if overflow_retry_remaining > 0 && compaction::is_context_overflow_error(&e) {
                    return TurnOutcome::ContextOverflow(e);
                }
                return TurnOutcome::Error(e);
            }
            LlmEvent::StatusUpdate(msg) => {
                tx.send_ignore(AppEvent::Agent(AgentEvent::StatusUpdate(msg.clone())));
                hook_ctx.hook_ipc.publish(
                    hook_ctx.session_id,
                    HookPoint::OnStatusUpdate,
                    None,
                    ipc_status_update_payload(&msg),
                );
                maybe_run_hook(
                    hook_ctx.hooks,
                    HookPoint::OnStatusUpdate,
                    hook_ctx.session_id,
                    Some(serde_json::json!({"status": msg})),
                    None,
                )
                .await;
            }
        }
    }

    if tool_intent_seen && pending_tool_calls.is_empty() {
        return TurnOutcome::ToolIntentWithNoCall;
    }

    let final_phase = if pending_tool_calls.is_empty() {
        AssistantPhase::Final
    } else if assistant_phase == AssistantPhase::Unknown {
        AssistantPhase::Provisional
    } else {
        assistant_phase
    };

    if pending_tool_calls.is_empty() {
        TurnOutcome::FinalAnswer {
            text: assistant_text,
            thinking: assistant_thinking,
            phase: final_phase,
            usage: latest_usage,
        }
    } else {
        TurnOutcome::ToolCalls {
            text: assistant_text,
            thinking: assistant_thinking,
            phase: final_phase,
            usage: latest_usage,
            calls: pending_tool_calls,
        }
    }
}

// ── execute_tool_batch ────────────────────────────────────────────────────────

/// Execute a batch of tool calls sequentially and return a [`BatchOutcome`].
///
/// Sends `ToolCallStart`/`ToolCallEnd` events and appends `ToolCall`/`ToolResult`
/// entries to `session_events` for each call. Checks for cancellation between
/// calls, but queued steering is deferred until the current turn boundary so
/// already-emitted tool calls complete in order.
async fn execute_tool_batch(
    config: &AgentLoopConfig,
    pending_tool_calls: &[(String, String, serde_json::Value)],
    tx: &UnboundedSender<AppEvent>,
    cancel_rx: &tokio::sync::watch::Receiver<bool>,
    session_events: &mut Vec<SessionEvent>,
) -> BatchOutcome {
    for (idx, (id, name, args)) in pending_tool_calls.iter().cloned().enumerate() {
        // ── Pre-tool hook ────────────────────────────────────────────────────
        config.hook_ipc.publish(
            &config.session_id,
            HookPoint::PreTool,
            Some(&name),
            ipc_pre_tool_payload(&name, &args),
        );
        crate::hooks::maybe_run_hook(
            &config.hooks,
            HookPoint::PreTool,
            &config.session_id,
            Some(tool_json(&name, &args)),
            Some(&name),
        )
        .await;

        tx.send_ignore(AppEvent::Agent(AgentEvent::ToolCallStart {
            id: id.clone(),
            name: name.clone(),
            args: args.clone(),
        }));

        let result = config
            .executor
            .execute_tool(
                &id,
                &name,
                args.clone(),
                &config.tools,
                &config.tool_output_log,
                Some(tx.clone()),
            )
            .await;

        // ── Post-tool hook ───────────────────────────────────────────────────
        crate::hooks::maybe_run_hook(
            &config.hooks,
            HookPoint::PostTool,
            &config.session_id,
            Some(post_tool_json(
                &name,
                &args,
                result.is_error,
                result.is_truncated,
            )),
            Some(&name),
        )
        .await;

        tx.send_ignore(AppEvent::Agent(AgentEvent::ToolCallEnd {
            id: id.clone(),
            result: result.clone(),
        }));
        record_tool_call_result(session_events, &id, &name, args, result);

        // Check for cancellation before the next call.
        if *cancel_rx.borrow() {
            for (skip_id, skip_name, skip_args) in pending_tool_calls.iter().skip(idx + 1).cloned()
            {
                tx.send_ignore(AppEvent::Agent(AgentEvent::ToolCallStart {
                    id: skip_id.clone(),
                    name: skip_name.clone(),
                    args: skip_args.clone(),
                }));
                let interrupted = ToolResult::err("Interrupted by user");
                tx.send_ignore(AppEvent::Agent(AgentEvent::ToolCallEnd {
                    id: skip_id.clone(),
                    result: interrupted.clone(),
                }));
                record_tool_call_result(
                    session_events,
                    &skip_id,
                    &skip_name,
                    skip_args,
                    interrupted,
                );
            }
            return BatchOutcome::Cancelled;
        }
    }

    BatchOutcome::Completed
}

// ── Tool definition helpers ───────────────────────────────────────────────────

/// Build a sorted list of [`ToolDefinition`]s from the tool registry.
///
/// Sorted alphabetically by name so the serialized request body is
/// deterministic across process restarts, keeping the LLM provider's
/// prompt cache stable.
pub(crate) fn build_sorted_tool_defs(tools: &ToolRegistry) -> Vec<ToolDefinition> {
    let mut defs: Vec<ToolDefinition> = tools
        .values()
        .map(|t| ToolDefinition {
            name: t.name().to_string(),
            description: t.description().to_string(),
            parameters: t.parameters_schema(),
            streaming_field: t.streaming_field().map(str::to_owned),
        })
        .collect();
    defs.sort_by(|a, b| a.name.cmp(&b.name));
    defs
}

// ── run_agent_loop ────────────────────────────────────────────────────────────

/// Run the agent loop: call the LLM, execute tool calls, repeat until the
/// model gives a final text answer.
///
/// All activity is reported back to `App` via `AppEvent::Agent(...)` values sent on `tx`.
pub async fn run_agent_loop(
    config: AgentLoopConfig,
    provider: Arc<dyn LlmProvider>,
    tx: UnboundedSender<AppEvent>,
    mut steering_rx: UnboundedReceiver<String>,
    cancel_rx: tokio::sync::watch::Receiver<bool>,
) {
    let tool_defs: Vec<ToolDefinition> = build_sorted_tool_defs(&config.tools);

    let mut session_events = config.session_events.clone();
    let mut projection = LlmProjection::new();
    let mut overflow_retry_remaining = 1usize;

    // ── Manual compaction shortcut ────────────────────────────────────────────
    if config.manual_compaction_instructions.is_some() {
        match emit_compaction(
            Arc::clone(&provider),
            &tx,
            &session_events,
            &config,
            CompactionTrigger::Threshold,
            config.manual_compaction_instructions.clone(),
        )
        .await
        {
            Ok(_) => {}
            Err(e) => send_compaction_failed_status(&tx, &e.message),
        }
        // ── On-done hook (manual compaction) ─────────────────────────────────
        config
            .hook_ipc
            .publish(&config.session_id, HookPoint::OnDone, None, empty_payload());
        crate::hooks::maybe_run_hook(
            &config.hooks,
            HookPoint::OnDone,
            &config.session_id,
            None,
            None,
        )
        .await;
        config
            .hook_ipc
            .publish(&config.session_id, HookPoint::OnIdle, None, empty_payload());
        crate::hooks::maybe_run_hook(
            &config.hooks,
            HookPoint::OnIdle,
            &config.session_id,
            None,
            None,
        )
        .await;
        tx.send_ignore(AppEvent::Agent(AgentEvent::Done));
        return;
    }

    loop {
        // ── Cancellation check ────────────────────────────────────────────────
        if *cancel_rx.borrow() {
            config.hook_ipc.publish(
                &config.session_id,
                HookPoint::OnCancel,
                None,
                empty_payload(),
            );
            crate::hooks::maybe_run_hook(
                &config.hooks,
                HookPoint::OnCancel,
                &config.session_id,
                None,
                None,
            )
            .await;
            return;
        }

        // ── Check for external file modifications ─────────────────────────────
        let changes = {
            let mut tracker = config.file_tracker.lock().unwrap();
            let all_changes = tracker.check_modified();
            // Only report changes to git-tracked files. Gitignored and
            // untracked files (databases, build artifacts, etc.) are
            // protected by the per-tool staleness guard instead.
            let (tracked, _untracked): (Vec<_>, Vec<_>) = all_changes
                .into_iter()
                .partition(|c| tracker.is_git_tracked(&c.path));
            // Absorb only git-tracked changes so they won't re-fire.
            // Untracked changes stay in the old snapshot so the staleness
            // guard in edit/write tools can detect them.
            let paths: Vec<_> = tracked.iter().map(|c| c.path.clone()).collect();
            tracker.accept_changes(&paths);
            tracked
        };
        if !changes.is_empty() {
            let paths: Vec<std::path::PathBuf> = changes.iter().map(|c| c.path.clone()).collect();
            let notification = build_notification(&changes);
            session_events.push(SessionEvent::UserMessage {
                content: notification.clone(),
                timestamp: 0,
            });
            config.hook_ipc.publish(
                &config.session_id,
                HookPoint::OnExternalChange,
                None,
                ipc_external_change_payload(&paths),
            );
            crate::hooks::maybe_run_hook(
                &config.hooks,
                HookPoint::OnExternalChange,
                &config.session_id,
                None,
                None,
            )
            .await;
            tx.send_ignore(AppEvent::Agent(AgentEvent::ExternalFileChange {
                paths,
                notification,
            }));
        }

        // ── Insert queued steering messages ───────────────────────────────────
        let _ = drain_steering_messages(
            &mut steering_rx,
            &mut session_events,
            &tx,
            &HookDispatchContext {
                hooks: &config.hooks,
                hook_ipc: &config.hook_ipc,
                session_id: &config.session_id,
            },
        )
        .await;

        // ── Build message list ────────────────────────────────────────────────
        projection.ensure_current(&session_events);
        let mut messages: Vec<Message> = config.system_prompt.iter().map(Message::system).collect();
        messages.extend_from_slice(projection.messages());

        // ── Pre-turn hook ────────────────────────────────────────────────────
        config.hook_ipc.publish(
            &config.session_id,
            HookPoint::PreTurn,
            None,
            empty_payload(),
        );
        crate::hooks::maybe_run_hook(
            &config.hooks,
            HookPoint::PreTurn,
            &config.session_id,
            None,
            None,
        )
        .await;

        // ── Stream assistant turn ─────────────────────────────────────────────
        let turn = stream_assistant_turn(
            Arc::clone(&provider),
            messages,
            tool_defs.clone(),
            &tx,
            overflow_retry_remaining,
            HookDispatchContext {
                hooks: &config.hooks,
                hook_ipc: &config.hook_ipc,
                session_id: &config.session_id,
            },
        )
        .await;

        match turn {
            TurnOutcome::Error(e) => {
                config.hook_ipc.publish(
                    &config.session_id,
                    HookPoint::OnError,
                    None,
                    ipc_on_error_payload(&e.message, None, None),
                );
                crate::hooks::maybe_run_hook(
                    &config.hooks,
                    HookPoint::OnError,
                    &config.session_id,
                    Some(crate::hooks::on_error_json(&e.message, None, None)),
                    None,
                )
                .await;
                tx.send_ignore(AppEvent::Agent(AgentEvent::Error(e)));
                return;
            }

            TurnOutcome::ToolIntentWithNoCall => {
                let error_message =
                    "Tool call was indicated but not completed (response may have been truncated).";
                config.hook_ipc.publish(
                    &config.session_id,
                    HookPoint::OnError,
                    None,
                    ipc_on_error_payload(error_message, None, None),
                );
                crate::hooks::maybe_run_hook(
                    &config.hooks,
                    HookPoint::OnError,
                    &config.session_id,
                    Some(crate::hooks::on_error_json(error_message, None, None)),
                    None,
                )
                .await;
                tx.send_ignore(AppEvent::Agent(AgentEvent::Error(
                    crate::llm::ProviderError::other(
                        "agent",
                        "Tool call was indicated but not completed \
                         (response may have been truncated).",
                    ),
                )));
                return;
            }

            TurnOutcome::ContextOverflow(e) => {
                overflow_retry_remaining -= 1;
                match emit_compaction(
                    Arc::clone(&provider),
                    &tx,
                    &session_events,
                    &config,
                    CompactionTrigger::OverflowRetry,
                    None,
                )
                .await
                {
                    Ok(outcome) => {
                        session_events.push(SessionEvent::CompactionSummary {
                            summary: outcome.summary.clone(),
                            trigger_reason: outcome.trigger_reason,
                            context_window: outcome.context_window,
                            reserve_tokens: outcome.reserve_tokens,
                            keep_recent_tokens: outcome.keep_recent_tokens,
                            tokens_before: outcome.tokens_before,
                            tokens_after: outcome.tokens_after,
                            retained_event_count: Some(outcome.retained_event_count),
                            read_files: outcome.read_files,
                            modified_files: outcome.modified_files,
                            timestamp: 0,
                        });
                        continue;
                    }
                    Err(compaction_error) => {
                        send_compaction_failed_status(&tx, &compaction_error.message);
                        tx.send_ignore(AppEvent::Agent(AgentEvent::Error(e)));
                        return;
                    }
                }
            }

            TurnOutcome::FinalAnswer {
                text,
                thinking,
                phase,
                usage,
            } => {
                session_events.push(SessionEvent::AssistantMessage {
                    content: text,
                    thinking,
                    phase,
                    usage,
                    timestamp: 0,
                });

                config.file_tracker.lock().unwrap().refresh_baselines();

                tx.send_ignore(AppEvent::Agent(AgentEvent::TurnEnd));

                // ── Post-turn hook ───────────────────────────────────────────
                config.hook_ipc.publish(
                    &config.session_id,
                    HookPoint::PostTurn,
                    None,
                    empty_payload(),
                );
                crate::hooks::maybe_run_hook(
                    &config.hooks,
                    HookPoint::PostTurn,
                    &config.session_id,
                    None,
                    None,
                )
                .await;

                // If a steering message arrived while the LLM was generating,
                // consume it only after the completed assistant turn has been
                // committed via TurnEnd so transcript order remains natural.
                if drain_steering_messages(
                    &mut steering_rx,
                    &mut session_events,
                    &tx,
                    &HookDispatchContext {
                        hooks: &config.hooks,
                        hook_ipc: &config.hook_ipc,
                        session_id: &config.session_id,
                    },
                )
                .await
                {
                    continue;
                }

                // Threshold-based auto-compaction after a completed turn.
                if config.auto_compaction_enabled {
                    let (context_window, reserve_tokens, _keep_recent_tokens) =
                        compaction::context_window_and_budgets(&config.current_model);
                    let used_tokens = usage
                        .and_then(|u| u.used_tokens())
                        .unwrap_or_else(|| compaction::estimate_session_tokens(&session_events));
                    if used_tokens > context_window.saturating_sub(reserve_tokens) {
                        match emit_compaction(
                            Arc::clone(&provider),
                            &tx,
                            &session_events,
                            &config,
                            CompactionTrigger::Threshold,
                            None,
                        )
                        .await
                        {
                            Ok(_) => {}
                            Err(e) => send_compaction_failed_status(&tx, &e.message),
                        }
                    }
                }

                // ── On-done hook (final answer) ──────────────────────────────
                config.hook_ipc.publish(
                    &config.session_id,
                    HookPoint::OnDone,
                    None,
                    empty_payload(),
                );
                crate::hooks::maybe_run_hook(
                    &config.hooks,
                    HookPoint::OnDone,
                    &config.session_id,
                    None,
                    None,
                )
                .await;
                config.hook_ipc.publish(
                    &config.session_id,
                    HookPoint::OnIdle,
                    None,
                    empty_payload(),
                );
                crate::hooks::maybe_run_hook(
                    &config.hooks,
                    HookPoint::OnIdle,
                    &config.session_id,
                    None,
                    None,
                )
                .await;
                tx.send_ignore(AppEvent::Agent(AgentEvent::Done));
                return;
            }

            TurnOutcome::ToolCalls {
                text,
                thinking,
                phase,
                usage,
                calls,
            } => {
                session_events.push(SessionEvent::AssistantMessage {
                    content: text,
                    thinking,
                    phase,
                    usage,
                    timestamp: 0,
                });

                // ── Execute tool batch ────────────────────────────────────────
                let batch_outcome =
                    execute_tool_batch(&config, &calls, &tx, &cancel_rx, &mut session_events).await;

                config.file_tracker.lock().unwrap().refresh_baselines();
                tx.send_ignore(AppEvent::Agent(AgentEvent::TurnEnd));

                // ── Post-turn hook (tool calls) ──────────────────────────────
                config.hook_ipc.publish(
                    &config.session_id,
                    HookPoint::PostTurn,
                    None,
                    empty_payload(),
                );
                crate::hooks::maybe_run_hook(
                    &config.hooks,
                    HookPoint::PostTurn,
                    &config.session_id,
                    None,
                    None,
                )
                .await;

                if let BatchOutcome::Cancelled = batch_outcome {
                    config.hook_ipc.publish(
                        &config.session_id,
                        HookPoint::OnCancel,
                        None,
                        empty_payload(),
                    );
                    crate::hooks::maybe_run_hook(
                        &config.hooks,
                        HookPoint::OnCancel,
                        &config.session_id,
                        None,
                        None,
                    )
                    .await;
                    return;
                }

                if drain_steering_messages(
                    &mut steering_rx,
                    &mut session_events,
                    &tx,
                    &HookDispatchContext {
                        hooks: &config.hooks,
                        hook_ipc: &config.hook_ipc,
                        session_id: &config.session_id,
                    },
                )
                .await
                {
                    continue;
                }
            }
        }
    }
}
