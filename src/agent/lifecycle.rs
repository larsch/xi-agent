use crate::agent::events::AgentEventSink;
use crate::agent::types::{AgentEvent, AgentLoopConfig};
use crate::hooks::{HookPoint, empty_payload, maybe_run_hook};

/// Run the normal completion hooks for a finished agent loop.
pub(crate) async fn on_done(config: &AgentLoopConfig) {
    run_hook(config, HookPoint::OnDone, None, None).await;
    run_hook(config, HookPoint::OnIdle, None, None).await;
}

/// Run cancellation hooks without marking the loop idle.
pub(crate) async fn on_cancel(config: &AgentLoopConfig) {
    run_hook(config, HookPoint::OnCancel, None, None).await;
}

/// Run cancellation hooks and transition the loop to idle.
pub(crate) async fn on_cancel_and_idle(config: &AgentLoopConfig) {
    on_cancel(config).await;
    run_hook(config, HookPoint::OnIdle, None, None).await;
}

/// Report a provider/loop error through hooks and the agent event stream.
pub(crate) async fn on_error(
    config: &AgentLoopConfig,
    sink: &dyn AgentEventSink,
    message: &str,
    error: crate::llm::ProviderError,
) {
    let payload = crate::hooks::ipc_on_error_payload(message, None, None);
    let json = crate::hooks::on_error_json(message, None, None);
    run_hook(config, HookPoint::OnError, Some(payload), Some(json)).await;
    sink.emit(AgentEvent::Error(error));
}

async fn run_hook(
    config: &AgentLoopConfig,
    point: HookPoint,
    ipc_payload: Option<serde_json::Value>,
    hook_payload: Option<serde_json::Value>,
) {
    config.hook_ipc.publish(
        &config.session_id,
        point,
        None,
        ipc_payload.unwrap_or_else(empty_payload),
    );
    maybe_run_hook(&config.hooks, point, &config.session_id, hook_payload, None).await;
}
