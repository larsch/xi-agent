use futures_util::future::join_all;
use tokio::sync::mpsc::UnboundedSender;

use crate::agent::events::{AgentEventSink, send_agent_event};
use crate::agent::types::{AgentEvent, AgentLoopConfig, CancelLevel, ToolResult};
use crate::app_event::AppEvent;
use crate::hooks::{HookPoint, ipc_pre_tool_payload, post_tool_json, tool_json};
use crate::session_event::SessionEvent;

/// The result of executing a batch of tool calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BatchOutcome {
    Completed,
    Cancelled,
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

// ── execute_tool_batch ────────────────────────────────────────────────────────

/// Execute a batch of tool calls concurrently and return a [`BatchOutcome`].
///
/// Each call runs its pre-tool hook, execution, and post-tool hook in one
/// future. The futures are joined concurrently, but their results are emitted
/// and recorded in the model's original order.
pub(crate) async fn execute_tool_batch(
    config: &AgentLoopConfig,
    pending_tool_calls: &[(String, String, serde_json::Value)],
    sink: &dyn AgentEventSink,
    tx: &UnboundedSender<AppEvent>,
    cancel_rx: &tokio::sync::watch::Receiver<CancelLevel>,
    session_events: &mut Vec<SessionEvent>,
) -> BatchOutcome {
    let calls = pending_tool_calls
        .iter()
        .cloned()
        .map(|(id, name, args)| async move {
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

            send_agent_event(
                sink,
                AgentEvent::ToolCallStart {
                    id: id.clone(),
                    name: name.clone(),
                    args: args.clone(),
                },
            );

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

            (id, name, args, result)
        });

    let results = join_all(calls).await;
    let cancelled = *cancel_rx.borrow() >= CancelLevel::HardAbort;

    for (id, name, args, result) in results {
        send_agent_event(
            sink,
            AgentEvent::ToolCallEnd {
                id: id.clone(),
                result: result.clone(),
            },
        );
        record_tool_call_result(session_events, &id, &name, args, result);
    }

    if cancelled {
        BatchOutcome::Cancelled
    } else {
        BatchOutcome::Completed
    }
}
