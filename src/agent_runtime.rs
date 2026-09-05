use crate::agent::runner::AgentHandle;
use crate::agent::types::CancelLevel;
use crate::app_event::{AppEvent, AppEventTx};

/// Owns application event delivery and the active agent runner, while keeping
/// UI-only steering display and abort state separate from runner lifecycle.
pub(crate) struct AgentRuntime {
    pub(crate) app_event_rx: tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    pub(crate) app_event_tx: AppEventTx,
    pub(crate) agent_handle: Option<AgentHandle>,
    pub(crate) queued_steering: Vec<String>,
    pub(crate) abort_stage: CancelLevel,
    pub(crate) ctrl_d_last_press: Option<std::time::Instant>,
    pub(crate) pending_finalize: bool,
    pub(crate) pending_shell_handle: Option<tokio::task::JoinHandle<()>>,
}

impl AgentRuntime {
    pub fn new() -> Self {
        let (app_event_tx, app_event_rx) = tokio::sync::mpsc::unbounded_channel();
        Self {
            app_event_rx,
            app_event_tx,
            agent_handle: None,
            queued_steering: Vec::new(),
            abort_stage: CancelLevel::None,
            ctrl_d_last_press: None,
            pending_finalize: false,
            pending_shell_handle: None,
        }
    }

    pub(crate) fn reset_abort_stages(&mut self) {
        self.abort_stage = CancelLevel::None;
        self.ctrl_d_last_press = None;
    }

    pub fn app_event_tx(&self) -> AppEventTx {
        self.app_event_tx.clone()
    }

    pub async fn recv_app_event(&mut self) -> Option<AppEvent> {
        self.app_event_rx.recv().await
    }

    pub fn try_recv_app_event(
        &mut self,
    ) -> Result<AppEvent, tokio::sync::mpsc::error::TryRecvError> {
        self.app_event_rx.try_recv()
    }

    pub fn queued_steering(&self) -> &[String] {
        &self.queued_steering
    }

    pub(crate) fn set_agent_handle(&mut self, handle: AgentHandle) {
        self.agent_handle = Some(handle);
    }

    pub(crate) fn steering_tx(&self) -> Option<&tokio::sync::mpsc::UnboundedSender<String>> {
        self.agent_handle.as_ref().map(|h| h.steering_sender())
    }

    pub(crate) fn cancel_tx(&self) -> Option<&tokio::sync::watch::Sender<CancelLevel>> {
        self.agent_handle.as_ref().map(|h| h.cancel_sender())
    }

    pub(crate) fn take_agent_handle(&mut self) -> Option<AgentHandle> {
        self.agent_handle.take()
    }

    pub(crate) fn clear_agent_handle(&mut self) {
        self.agent_handle = None;
    }

    #[cfg(test)]
    pub(crate) fn take_agent_task(&mut self) -> Option<tokio::task::JoinHandle<()>> {
        self.agent_handle.take().map(|handle| handle.task)
    }

    pub(crate) fn is_running(&self) -> bool {
        self.agent_handle.is_some()
    }
}

impl Default for AgentRuntime {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_produces_idle_runtime() {
        let rt = AgentRuntime::new();
        assert!(!rt.is_running());
        assert!(rt.queued_steering().is_empty());
        assert_eq!(rt.abort_stage, CancelLevel::None);
    }

    #[test]
    fn reset_abort_stages_clears_state() {
        let mut rt = AgentRuntime::new();
        rt.abort_stage = CancelLevel::SoftStop;
        rt.ctrl_d_last_press = Some(std::time::Instant::now());
        rt.reset_abort_stages();
        assert_eq!(rt.abort_stage, CancelLevel::None);
        assert!(rt.ctrl_d_last_press.is_none());
    }
}
