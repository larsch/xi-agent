use tokio::task::JoinHandle;

use crate::app_event::{AppEvent, AppEventTx};

/// Owns the agent task handle, event channels, steering queue, and
/// cancellation plumbing for one agent session.
///
/// # Orchestration methods that remain on `App`
///
/// The higher-level coordination methods were not moved here because they
/// also touch session state, live-turn state, and provider handles:
///
/// - `start_agent_task` — builds `AgentLoopConfig`, spawns the task,
///   writes to `session_state`, `live_turn`, `current_provider`
/// - `abort_agent_loop` — also clears `live_turn`, updates UI state
/// - `steer_agent` — also pushes steering into the log display
/// - `try_recv_app_event` — also drives the full event-dispatch loop
///
/// They remain on `App` accessing runtime fields via `self.runtime.*`.
pub(crate) struct AgentRuntime {
    /// Receives background app events forwarded from tasks targeting the UI.
    pub(crate) app_event_rx: tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    pub(crate) app_event_tx: AppEventTx,
    pub(crate) steering_tx: Option<tokio::sync::mpsc::UnboundedSender<String>>,
    /// User steering messages queued while a loop is running; rendered pinned
    /// at the bottom of the log with a 🕹️ icon until consumed.
    pub(crate) queued_steering: Vec<String>,
    /// JoinHandle for the currently running agent loop task (if any).
    pub(crate) agent_task: Option<JoinHandle<()>>,
    /// Cancellation sender for the active agent loop task.
    /// Sending `true` signals the loop to exit at its next cooperative checkpoint.
    pub(crate) cancel_tx: Option<tokio::sync::watch::Sender<bool>>,
}

impl AgentRuntime {
    pub fn new() -> Self {
        let (app_event_tx, app_event_rx) = tokio::sync::mpsc::unbounded_channel();
        Self {
            app_event_rx,
            app_event_tx,
            steering_tx: None,
            queued_steering: Vec::new(),
            agent_task: None,
            cancel_tx: None,
        }
    }

    /// Returns a clone of the sender side of the app-event channel.
    pub fn app_event_tx(&self) -> AppEventTx {
        self.app_event_tx.clone()
    }

    /// Receive the next app event, waiting asynchronously.
    pub async fn recv_app_event(&mut self) -> Option<AppEvent> {
        self.app_event_rx.recv().await
    }

    /// Non-blocking poll for the next app event.
    pub fn try_recv_app_event(
        &mut self,
    ) -> Result<AppEvent, tokio::sync::mpsc::error::TryRecvError> {
        self.app_event_rx.try_recv()
    }

    /// Queued steering messages waiting to be consumed by the agent loop.
    pub fn queued_steering(&self) -> &[String] {
        &self.queued_steering
    }

    /// Returns true when an agent task is currently running.
    #[allow(dead_code)]
    pub(crate) fn is_running(&self) -> bool {
        self.agent_task.is_some()
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
        assert!(rt.steering_tx.is_none());
        assert!(rt.cancel_tx.is_none());
    }

    #[test]
    fn default_equals_new() {
        let a = AgentRuntime::new();
        let b = AgentRuntime::default();
        assert_eq!(a.is_running(), b.is_running());
        assert_eq!(a.queued_steering(), b.queued_steering());
    }

    #[test]
    fn app_event_tx_can_send_and_runtime_receives() {
        let mut rt = AgentRuntime::new();
        let tx = rt.app_event_tx();
        tx.send(AppEvent::ModelsReady(Ok(vec!["gpt-4".to_string()])))
            .unwrap();
        let received = rt.try_recv_app_event();
        assert!(matches!(received, Ok(AppEvent::ModelsReady(Ok(_)))));
    }

    #[test]
    fn is_running_reflects_agent_task_presence() {
        let mut rt = AgentRuntime::new();
        assert!(!rt.is_running());
        rt.agent_task = Some(tokio::runtime::Runtime::new().unwrap().spawn(async {}));
        assert!(rt.is_running());
        rt.agent_task = None;
        assert!(!rt.is_running());
    }
}
