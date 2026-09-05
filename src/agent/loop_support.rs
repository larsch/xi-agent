use std::sync::Arc;

use tokio::sync::mpsc::UnboundedReceiver;

use crate::agent::compaction;
use crate::agent::events::{AgentEventSink, send_agent_event};
use crate::agent::types::{AgentEvent, AgentLoopConfig};

use crate::hooks::{HookConfig, HookPoint, empty_payload};
use crate::llm::LlmProvider;
use crate::session_event::{CompactionTrigger, SessionEvent};

pub(crate) struct HookDispatchContext<'a> {
    pub(crate) hooks: &'a std::collections::HashMap<HookPoint, Vec<HookConfig>>,
    pub(crate) hook_ipc: &'a crate::hooks::HookIpcPublisherHandle,
    pub(crate) session_id: &'a str,
}

// ── Helpers ───────────────────────────────────────────────────────────────────

pub(crate) async fn drain_steering_messages(
    steering_rx: &mut UnboundedReceiver<String>,
    session_events: &mut Vec<SessionEvent>,
    sink: &dyn AgentEventSink,
    hook_ctx: &HookDispatchContext<'_>,
) -> bool {
    let mut consumed = false;
    while let Ok(text) = steering_rx.try_recv() {
        send_agent_event(sink, AgentEvent::SteeringConsumed { text: text.clone() });
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

pub(crate) fn send_compaction_failed_status(sink: &dyn AgentEventSink, message: &str) {
    send_agent_event(
        sink,
        AgentEvent::StatusUpdate(format!(
            "compaction failed: {message}; continuing without compaction."
        )),
    );
}

pub(crate) async fn emit_compaction(
    provider: Arc<dyn LlmProvider>,
    sink: &dyn AgentEventSink,
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

    send_agent_event(sink, AgentEvent::Compacting);
    let outcome = compaction::compact_events(
        provider,
        session_events,
        &config.current_model,
        trigger_reason,
        user_instructions,
    )
    .await?;
    send_agent_event(sink, AgentEvent::CompactionDone(outcome.clone()));
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
