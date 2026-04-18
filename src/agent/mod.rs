use std::sync::Arc;

use futures_util::StreamExt;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::app_event::AppEvent;
use crate::llm::{AssistantPhase, LlmEvent, LlmProvider, Message, ToolDefinition};
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
pub use types::{AgentEvent, AgentLoopConfig, ToolResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolBatchInterruption {
    None,
    Cancelled,
    SteeringQueued,
}

fn drain_steering_messages(
    steering_rx: &mut UnboundedReceiver<String>,
    messages: &mut Vec<Message>,
    tx: &UnboundedSender<AppEvent>,
) -> bool {
    let mut consumed = false;
    while let Ok(text) = steering_rx.try_recv() {
        let _ = tx.send(AppEvent::Agent(AgentEvent::SteeringConsumed {
            text: text.clone(),
        }));
        messages.push(Message::user(text));
        consumed = true;
    }
    consumed
}

fn record_tool_call_result(
    messages: &mut Vec<Message>,
    session_events: &mut Vec<SessionEvent>,
    id: &str,
    name: &str,
    args: serde_json::Value,
    result: ToolResult,
) {
    messages.push(Message::tool_call(id, name, args.clone()));
    messages.push(Message::tool_result(id, &result.content, result.is_error));
    session_events.push(SessionEvent::ToolCall {
        id: id.to_string(),
        name: name.to_string(),
        args,
        timestamp: 0,
    });
    session_events.push(SessionEvent::ToolResult {
        id: id.to_string(),
        name: name.to_string(),
        content: result.content,
        is_error: result.is_error,
        display_range: None,
        timestamp: 0,
    });
}

fn skip_remaining_tool_calls(
    pending_tool_calls: &[(String, String, serde_json::Value)],
    next_idx: usize,
    tx: &UnboundedSender<AppEvent>,
    messages: &mut Vec<Message>,
    session_events: &mut Vec<SessionEvent>,
    reason: &'static str,
) {
    for (skip_id, skip_name, skip_args) in pending_tool_calls.iter().skip(next_idx).cloned() {
        let _ = tx.send(AppEvent::Agent(AgentEvent::ToolCallStart {
            id: skip_id.clone(),
            name: skip_name.clone(),
            args: skip_args.clone(),
        }));
        let skipped = ToolResult::err(reason);
        let _ = tx.send(AppEvent::Agent(AgentEvent::ToolCallEnd {
            id: skip_id.clone(),
            result: skipped.clone(),
        }));
        record_tool_call_result(
            messages,
            session_events,
            &skip_id,
            &skip_name,
            skip_args,
            skipped,
        );
    }
}

fn resolve_tool_batch_interruption(
    pending_tool_calls: &[(String, String, serde_json::Value)],
    next_idx: usize,
    cancel_rx: &tokio::sync::watch::Receiver<bool>,
    steering_rx: &mut UnboundedReceiver<String>,
    messages: &mut Vec<Message>,
    session_events: &mut Vec<SessionEvent>,
    tx: &UnboundedSender<AppEvent>,
) -> ToolBatchInterruption {
    if *cancel_rx.borrow() {
        skip_remaining_tool_calls(
            pending_tool_calls,
            next_idx,
            tx,
            messages,
            session_events,
            "Interrupted by user",
        );
        return ToolBatchInterruption::Cancelled;
    }

    if drain_steering_messages(steering_rx, messages, tx) {
        skip_remaining_tool_calls(
            pending_tool_calls,
            next_idx,
            tx,
            messages,
            session_events,
            "Skipped due to queued user message.",
        );
        return ToolBatchInterruption::SteeringQueued;
    }

    ToolBatchInterruption::None
}

async fn emit_compaction(
    provider: Arc<dyn LlmProvider>,
    tx: &UnboundedSender<AppEvent>,
    session_events: &[SessionEvent],
    model: &str,
    trigger_reason: CompactionTrigger,
    user_instructions: Option<String>,
) -> Result<compaction::CompactionOutcome, crate::llm::ProviderError> {
    let _ = tx.send(AppEvent::Agent(AgentEvent::Compacting));
    let outcome = compaction::compact_events(
        provider,
        session_events.to_vec(),
        model,
        trigger_reason,
        user_instructions,
    )
    .await?;
    let _ = tx.send(AppEvent::Agent(AgentEvent::CompactionDone {
        summary: outcome.summary.clone(),
        trigger_reason: outcome.trigger_reason.clone(),
        context_window: outcome.context_window,
        reserve_tokens: outcome.reserve_tokens,
        keep_recent_tokens: outcome.keep_recent_tokens,
        tokens_before: outcome.tokens_before,
        tokens_after: outcome.tokens_after,
        retained_event_count: outcome.retained_event_count,
        read_files: outcome.read_files.clone(),
        modified_files: outcome.modified_files.clone(),
    }));
    Ok(outcome)
}

/// Run the agent loop: call the LLM, execute tool calls, repeat until the
/// model gives a final text answer.
///
/// All activity is reported back to `App` via `AppEvent::Agent(...)` values sent on `tx`.
pub async fn run_agent_loop(
    mut messages: Vec<Message>,
    config: AgentLoopConfig,
    provider: Arc<dyn LlmProvider>,
    tx: UnboundedSender<AppEvent>,
    mut steering_rx: UnboundedReceiver<String>,
    cancel_rx: tokio::sync::watch::Receiver<bool>,
) {
    // Build the tool definitions once from the registry.
    let tool_defs: Vec<ToolDefinition> = config
        .tools
        .values()
        .map(|t| ToolDefinition {
            name: t.name().to_string(),
            description: t.description().to_string(),
            parameters: t.parameters_schema(),
        })
        .collect();

    let mut session_events = config.session_events.clone();
    let mut overflow_retry_remaining = 1usize;

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
            Ok(_) => {
                let _ = tx.send(AppEvent::Agent(AgentEvent::Done));
            }
            Err(e) => {
                let _ = tx.send(AppEvent::Agent(AgentEvent::Error(e)));
            }
        }
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
            messages.push(Message::user(notification.clone()));
            session_events.push(SessionEvent::UserMessage {
                content: notification.clone(),
                timestamp: 0,
            });
            let _ = tx.send(AppEvent::Agent(AgentEvent::ExternalFileChange {
                paths,
                notification,
            }));
        }

        // Insert queued steering messages before the next assistant turn.
        let _ = drain_steering_messages(&mut steering_rx, &mut messages, &tx);

        // ── Stream the assistant response ─────────────────────────────────────
        let mut stream = provider.stream_chat_with_tools(messages.clone(), tool_defs.clone());

        // Accumulate text/thinking for the assistant message we'll push to
        // the display and to `messages` for history.
        let mut assistant_text = String::new();
        let mut assistant_thinking: Option<String> = None;
        let mut assistant_phase = AssistantPhase::Unknown;
        let mut pending_tool_calls: Vec<(String, String, serde_json::Value)> = Vec::new(); // (id, name, args)
        let mut tool_intent_seen = false;

        let mut latest_usage = None;
        let mut stream_error: Option<crate::llm::ProviderError> = None;

        while let Some(ev) = stream.next().await {
            match ev {
                LlmEvent::Token { text, phase } => {
                    let _ = tx.send(AppEvent::Agent(AgentEvent::TextToken {
                        text: text.clone(),
                        phase,
                    }));
                    assistant_text.push_str(&text);
                    if phase != AssistantPhase::Unknown {
                        assistant_phase = phase;
                    }
                }
                LlmEvent::ThinkingToken(t) => {
                    let _ = tx.send(AppEvent::Agent(AgentEvent::ThinkingToken(t.clone())));
                    assistant_thinking
                        .get_or_insert_with(String::new)
                        .push_str(&t);
                }
                LlmEvent::Usage(usage) => {
                    latest_usage = Some(usage);
                    let _ = tx.send(AppEvent::Agent(AgentEvent::Usage(usage)));
                }
                LlmEvent::ToolIntentStart => {
                    let _ = tx.send(AppEvent::Agent(AgentEvent::ToolIntentStart));
                    assistant_phase = AssistantPhase::Provisional;
                    tool_intent_seen = true;
                }
                LlmEvent::ToolCall { id, name, args } => {
                    pending_tool_calls.push((id, name, args));
                }
                LlmEvent::Done => break,
                LlmEvent::Error(e) => {
                    stream_error = Some(e);
                    break;
                }
                LlmEvent::StatusUpdate(msg) => {
                    let _ = tx.send(AppEvent::Agent(AgentEvent::StatusUpdate(msg)));
                }
            }
        }

        if let Some(e) = stream_error {
            if overflow_retry_remaining > 0 && compaction::is_context_overflow_error(&e) {
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
                        messages = crate::projection::project_llm_messages(&session_events);
                        continue;
                    }
                    Err(compaction_error) => {
                        let _ = tx.send(AppEvent::Agent(AgentEvent::Error(compaction_error)));
                        return;
                    }
                }
            }
            let _ = tx.send(AppEvent::Agent(AgentEvent::Error(e)));
            return;
        }

        // Guard: if the model signalled a tool call was coming but no complete
        // tool call arrived (e.g. truncated by max_tokens), treat it as an
        // error rather than silently accepting an empty assistant turn.
        if tool_intent_seen && pending_tool_calls.is_empty() {
            let _ = tx.send(AppEvent::Agent(AgentEvent::Error(
                crate::llm::ProviderError::other(
                    "agent",
                    "Tool call was indicated but not completed (response may have been truncated).",
                ),
            )));
            return;
        }

        // Append assistant message to history (even if empty when tools were called).
        let mut asst_msg = Message::assistant(&assistant_text);
        asst_msg.thinking = assistant_thinking.clone();
        let final_phase = if pending_tool_calls.is_empty() {
            AssistantPhase::Final
        } else if assistant_phase == AssistantPhase::Unknown {
            AssistantPhase::Provisional
        } else {
            assistant_phase
        };
        asst_msg.assistant_phase = Some(final_phase);
        messages.push(asst_msg);
        session_events.push(SessionEvent::AssistantMessage {
            content: assistant_text.clone(),
            thinking: assistant_thinking.clone(),
            phase: final_phase,
            usage: latest_usage,
            timestamp: 0,
        });

        // ── No tool calls → final answer ──────────────────────────────────────
        if pending_tool_calls.is_empty() {
            // Refresh baselines before returning to user input so that any
            // file changes the agent made during this run are absorbed.
            config.file_tracker.lock().unwrap().refresh_baselines();
            // Check for steering messages that arrived while the LLM was
            // generating its final response. If any are present, keep the
            // loop alive so they are processed rather than silently dropped.
            if drain_steering_messages(&mut steering_rx, &mut messages, &tx) {
                let _ = tx.send(AppEvent::Agent(AgentEvent::TurnEnd));
                continue;
            }
            let _ = tx.send(AppEvent::Agent(AgentEvent::TurnEnd));

            if config.auto_compaction_enabled {
                let (context_window, reserve_tokens, _keep_recent_tokens) =
                    compaction::context_window_and_budgets(&config.current_model);
                let used_tokens = latest_usage
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
                        Err(e) => {
                            let _ = tx.send(AppEvent::Agent(AgentEvent::Error(e)));
                            return;
                        }
                    }
                }
            }

            let _ = tx.send(AppEvent::Agent(AgentEvent::Done));
            return;
        }

        // ── Execute tool calls sequentially ───────────────────────────────────
        let mut tool_batch_interruption = ToolBatchInterruption::None;
        for (idx, (id, name, args)) in pending_tool_calls.iter().cloned().enumerate() {
            let _ = tx.send(AppEvent::Agent(AgentEvent::ToolCallStart {
                id: id.clone(),
                name: name.clone(),
                args: args.clone(),
            }));

            // before_tool_call hook
            let blocked = config
                .before_tool_call
                .as_ref()
                .map(|f| !f(&name, &args))
                .unwrap_or(false);

            let mut result = if blocked {
                ToolResult::err(format!("Tool call '{name}' was blocked"))
            } else {
                match config.tools.get(&name) {
                    Some(tool) => {
                        let r = tool.execute(args.clone()).await;
                        if tool.saves_output() {
                            let cmd_summary = args.get("command").and_then(|v| v.as_str());
                            r.with_log_notice(
                                &id,
                                cmd_summary,
                                &mut config.tool_output_log.lock().unwrap(),
                            )
                        } else {
                            r
                        }
                    }
                    None => ToolResult::err(format!("Unknown tool: '{name}'")),
                }
            };

            // after_tool_call hook
            if let Some(f) = &config.after_tool_call
                && let Some(override_result) = f(&name, &result)
            {
                result = override_result;
            }

            let _ = tx.send(AppEvent::Agent(AgentEvent::ToolCallEnd {
                id: id.clone(),
                result: result.clone(),
            }));

            record_tool_call_result(&mut messages, &mut session_events, &id, &name, args, result);

            tool_batch_interruption = resolve_tool_batch_interruption(
                &pending_tool_calls,
                idx + 1,
                &cancel_rx,
                &mut steering_rx,
                &mut messages,
                &mut session_events,
                &tx,
            );
            if tool_batch_interruption != ToolBatchInterruption::None {
                break;
            }
        }

        config.file_tracker.lock().unwrap().refresh_baselines();

        let _ = tx.send(AppEvent::Agent(AgentEvent::TurnEnd));

        match tool_batch_interruption {
            ToolBatchInterruption::None => {}
            ToolBatchInterruption::Cancelled => return,
            ToolBatchInterruption::SteeringQueued => continue,
        }
    }
}
