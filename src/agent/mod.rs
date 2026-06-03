use std::sync::Arc;

use futures_util::StreamExt;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::app_event::{AppEvent, SendIgnore};
use crate::llm::{AssistantPhase, LlmEvent, LlmProvider, Message, ToolDefinition, UsageStats};
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
pub use types::{AgentEvent, AgentLoopConfig, DefaultToolExecutor, ToolExecutor, ToolResult};

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

// ── Helpers ───────────────────────────────────────────────────────────────────

fn drain_steering_messages(
    steering_rx: &mut UnboundedReceiver<String>,
    session_events: &mut Vec<SessionEvent>,
    tx: &UnboundedSender<AppEvent>,
) -> bool {
    let mut consumed = false;
    while let Ok(text) = steering_rx.try_recv() {
        tx.send_ignore(AppEvent::Agent(AgentEvent::SteeringConsumed {
            text: text.clone(),
        }));
        session_events.push(SessionEvent::UserMessage {
            content: text,
            timestamp: 0,
        });
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
        timestamp: 0,
    });
    session_events.push(SessionEvent::ToolResult {
        id: id.to_string(),
        name: name.to_string(),
        content: result.content.as_text().to_string(),
        is_error: result.is_error,
        display_range: None,
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
    model: &str,
    trigger_reason: CompactionTrigger,
    user_instructions: Option<String>,
) -> Result<compaction::CompactionOutcome, crate::llm::ProviderError> {
    tx.send_ignore(AppEvent::Agent(AgentEvent::Compacting));
    let outcome = compaction::compact_events(
        provider,
        session_events,
        model,
        trigger_reason,
        user_instructions,
    )
    .await?;
    tx.send_ignore(AppEvent::Agent(AgentEvent::CompactionDone(outcome.clone())));
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
) -> TurnOutcome {
    // Build a lookup from tool name → streaming_field for intent events.
    let streaming_fields: std::collections::HashMap<String, Option<String>> = tool_defs
        .iter()
        .map(|t| (t.name.clone(), t.streaming_field.clone()))
        .collect();

    let mut stream = provider.stream_chat_with_tools(messages, tool_defs);

    let mut assistant_text = String::new();
    let mut assistant_thinking: Option<String> = None;
    let mut assistant_phase = AssistantPhase::Unknown;
    let mut pending_tool_calls: Vec<(String, String, serde_json::Value)> = Vec::new();
    let mut tool_intent_seen = false;
    let mut latest_usage = None;

    while let Some(ev) = stream.next().await {
        match ev {
            LlmEvent::Token { text, phase } => {
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
                    name,
                    streaming_field,
                }));
                assistant_phase = AssistantPhase::Provisional;
                tool_intent_seen = true;
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
                tx.send_ignore(AppEvent::Agent(AgentEvent::StatusUpdate(msg)));
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
    let tool_defs: Vec<ToolDefinition> = config
        .tools
        .values()
        .map(|t| ToolDefinition {
            name: t.name().to_string(),
            description: t.description().to_string(),
            parameters: t.parameters_schema(),
            streaming_field: t.streaming_field().map(str::to_owned),
        })
        .collect();

    let mut session_events = config.session_events.clone();
    let mut projection = LlmProjection::new();
    let mut overflow_retry_remaining = 1usize;

    // ── Manual compaction shortcut ────────────────────────────────────────────
    if config.manual_compaction_instructions.is_some() {
        match emit_compaction(
            Arc::clone(&provider),
            &tx,
            &session_events,
            &config.current_model,
            CompactionTrigger::Threshold,
            config.manual_compaction_instructions.clone(),
        )
        .await
        {
            Ok(_) => {}
            Err(e) => send_compaction_failed_status(&tx, &e.message),
        }
        tx.send_ignore(AppEvent::Agent(AgentEvent::Done));
        return;
    }

    loop {
        // ── Cancellation check ────────────────────────────────────────────────
        if *cancel_rx.borrow() {
            return;
        }

        // ── Check for external file modifications ─────────────────────────────
        let changes = config.file_tracker.lock().unwrap().check_modified();
        if !changes.is_empty() {
            let paths: Vec<std::path::PathBuf> = changes.iter().map(|c| c.path.clone()).collect();
            let notification = build_notification(&changes);
            session_events.push(SessionEvent::UserMessage {
                content: notification.clone(),
                timestamp: 0,
            });
            tx.send_ignore(AppEvent::Agent(AgentEvent::ExternalFileChange {
                paths,
                notification,
            }));
        }

        // ── Insert queued steering messages ───────────────────────────────────
        let _ = drain_steering_messages(&mut steering_rx, &mut session_events, &tx);

        // ── Build message list ────────────────────────────────────────────────
        projection.ensure_current(&session_events);
        let mut messages: Vec<Message> = config.system_prompt.iter().map(Message::system).collect();
        messages.extend_from_slice(projection.messages());

        // ── Stream assistant turn ─────────────────────────────────────────────
        let turn = stream_assistant_turn(
            Arc::clone(&provider),
            messages,
            tool_defs.clone(),
            &tx,
            overflow_retry_remaining,
        )
        .await;

        match turn {
            TurnOutcome::Error(e) => {
                tx.send_ignore(AppEvent::Agent(AgentEvent::Error(e)));
                return;
            }

            TurnOutcome::ToolIntentWithNoCall => {
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
                    &config.current_model,
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

                // If a steering message arrived while the LLM was generating,
                // consume it only after the completed assistant turn has been
                // committed via TurnEnd so transcript order remains natural.
                if drain_steering_messages(&mut steering_rx, &mut session_events, &tx) {
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
                            &config.current_model,
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

                if let BatchOutcome::Cancelled = batch_outcome {
                    return;
                }

                if drain_steering_messages(&mut steering_rx, &mut session_events, &tx) {
                    continue;
                }
            }
        }
    }
}
