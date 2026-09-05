use std::sync::Arc;

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::agent::events::{AgentEventSink, AppEventSink, send_agent_event};
use crate::agent::file_tracker::build_notification;
use crate::agent::{AgentActivity, AgentEvent, AgentLoopConfig, CancelLevel};
use crate::agent::{compaction, lifecycle, loop_support, tool_batch, tool_defs, turn};
use crate::app_event::AppEvent;
use crate::hooks::{HookPoint, empty_payload, ipc_external_change_payload};
use crate::llm::{LlmProvider, Message};
use crate::projection::LlmProjection;
use crate::session_event::{CompactionTrigger, SessionEvent};

// ── run_agent_loop ────────────────────────────────────────────────────────────

/// Run the agent loop: call the LLM, execute tool calls, repeat until the
/// model gives a final text answer.
///
/// All activity is reported back to `App` via `AppEvent::Agent(...)` values sent on `tx`.
pub async fn run_agent_loop(
    config: AgentLoopConfig,
    provider: Arc<dyn LlmProvider>,
    tx: UnboundedSender<AppEvent>,
    steering_rx: UnboundedReceiver<String>,
    cancel_rx: tokio::sync::watch::Receiver<crate::agent::types::CancelLevel>,
) {
    let sink = AppEventSink::new(tx.clone());
    run_agent_loop_inner(&config, provider, tx, &sink, steering_rx, cancel_rx).await;
    config.executor.shutdown().await;
}

async fn run_agent_loop_inner(
    config: &AgentLoopConfig,
    provider: Arc<dyn LlmProvider>,
    tx: UnboundedSender<AppEvent>,
    sink: &dyn AgentEventSink,
    mut steering_rx: UnboundedReceiver<String>,
    cancel_rx: tokio::sync::watch::Receiver<crate::agent::types::CancelLevel>,
) {
    let tool_defs = tool_defs::build_sorted_tool_defs(&config.tools);

    let mut session_events = config.session_events.clone();
    let mut projection = LlmProjection::new();
    let mut overflow_retry_remaining = 1usize;

    // ── Manual compaction shortcut ────────────────────────────────────────────
    if config.manual_compaction_requested {
        match loop_support::emit_compaction(
            Arc::clone(&provider),
            sink,
            &session_events,
            config,
            CompactionTrigger::Threshold,
            config.manual_compaction_instructions.clone(),
        )
        .await
        {
            Ok(_) => {}
            Err(e) => loop_support::send_compaction_failed_status(sink, &e.message),
        }
        // ── On-done hook (manual compaction) ─────────────────────────────────
        lifecycle::on_done(config).await;
        send_agent_event(sink, AgentEvent::Done);
        return;
    }

    let mut continuing_turn = false;
    loop {
        // ── Cancellation check ────────────────────────────────────────────────
        let cancel_level = *cancel_rx.borrow();
        if cancel_level >= CancelLevel::HardAbort {
            lifecycle::on_cancel(config).await;
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
            send_agent_event(
                sink,
                AgentEvent::ExternalFileChange {
                    paths,
                    notification,
                },
            );
        }

        // ── Insert queued steering messages ───────────────────────────────────
        let _ = loop_support::drain_steering_messages(
            &mut steering_rx,
            &mut session_events,
            sink,
            &loop_support::HookDispatchContext {
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
        send_agent_event(
            sink,
            AgentEvent::TurnStart {
                continuation: continuing_turn,
            },
        );
        continuing_turn = false;
        send_agent_event(
            sink,
            AgentEvent::ActivityChanged(AgentActivity::ModelRequest),
        );
        let turn = turn::stream_assistant_turn(
            Arc::clone(&provider),
            messages,
            tool_defs.clone(),
            sink,
            overflow_retry_remaining,
            loop_support::HookDispatchContext {
                hooks: &config.hooks,
                hook_ipc: &config.hook_ipc,
                session_id: &config.session_id,
            },
        )
        .await;

        send_agent_event(sink, AgentEvent::ActivityChanged(AgentActivity::LocalWork));

        match turn {
            turn::TurnOutcome::Error(e) => {
                let error_message = e.message.clone();
                lifecycle::on_error(config, sink, &error_message, e).await;
                return;
            }

            turn::TurnOutcome::ToolIntentWithNoCall => {
                let error_message =
                    "Tool call was indicated but not completed (response may have been truncated).";
                lifecycle::on_error(
                    config,
                    sink,
                    error_message,
                    crate::llm::ProviderError::other(
                        "agent",
                        "Tool call was indicated but not completed \
                         (response may have been truncated).",
                    ),
                )
                .await;
                return;
            }

            turn::TurnOutcome::ContextOverflow(e) => {
                overflow_retry_remaining -= 1;
                match loop_support::emit_compaction(
                    Arc::clone(&provider),
                    &tx,
                    &session_events,
                    config,
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
                        loop_support::send_compaction_failed_status(
                            sink,
                            &compaction_error.message,
                        );
                        send_agent_event(sink, AgentEvent::Error(e));
                        return;
                    }
                }
            }

            turn::TurnOutcome::FinalAnswer {
                text,
                thinking,
                phase,
                usage,
            } => {
                session_events.push(SessionEvent::AssistantMessage {
                    content: text.clone(),
                    thinking,
                    phase,
                    usage,
                    timestamp: 0,
                });

                config.file_tracker.lock().unwrap().refresh_baselines();

                send_agent_event(sink, AgentEvent::TurnEnd);

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
                if loop_support::drain_steering_messages(
                    &mut steering_rx,
                    &mut session_events,
                    sink,
                    &loop_support::HookDispatchContext {
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
                        match loop_support::emit_compaction(
                            Arc::clone(&provider),
                            &tx,
                            &session_events,
                            config,
                            CompactionTrigger::Threshold,
                            None,
                        )
                        .await
                        {
                            Ok(_) => {}
                            Err(e) => loop_support::send_compaction_failed_status(sink, &e.message),
                        }
                    }
                }

                // ── On-done hook (final answer) ──────────────────────────────
                lifecycle::on_done(config).await;
                send_agent_event(sink, AgentEvent::FinalResponse { text });
                send_agent_event(sink, AgentEvent::Done);
                return;
            }

            turn::TurnOutcome::ToolCalls {
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
                let batch_outcome = tool_batch::execute_tool_batch(
                    config,
                    &calls,
                    sink,
                    &tx,
                    &cancel_rx,
                    &mut session_events,
                )
                .await;

                config.file_tracker.lock().unwrap().refresh_baselines();
                send_agent_event(sink, AgentEvent::TurnEnd);
                continuing_turn = true;

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

                if let tool_batch::BatchOutcome::Cancelled = batch_outcome {
                    lifecycle::on_cancel_and_idle(config).await;
                    send_agent_event(sink, AgentEvent::Done);
                    return;
                }

                // ── Soft-stop check after turn completes ─────────────────────
                if *cancel_rx.borrow() >= CancelLevel::SoftStop {
                    lifecycle::on_done(config).await;
                    send_agent_event(sink, AgentEvent::Done);
                    return;
                }

                if loop_support::drain_steering_messages(
                    &mut steering_rx,
                    &mut session_events,
                    sink,
                    &loop_support::HookDispatchContext {
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
