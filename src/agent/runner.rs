use std::sync::Arc;

use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use crate::agent::{AgentLoopConfig, CancelLevel, run_agent_loop};
use crate::app_event::AppEventTx;
use crate::llm::LlmProvider;

/// Handles one spawned agent loop.
///
/// The handle owns the channels used to steer and cancel the loop, together
/// with its task. Consumers can keep the handle without depending on the
/// details of channel construction or task spawning.
pub(crate) struct AgentHandle {
    pub(crate) steering_tx: mpsc::UnboundedSender<String>,
    pub(crate) cancel_tx: watch::Sender<CancelLevel>,
    pub(crate) task: JoinHandle<()>,
}

impl AgentHandle {
    pub(crate) fn steering_sender(&self) -> &mpsc::UnboundedSender<String> {
        &self.steering_tx
    }

    pub(crate) fn cancel_sender(&self) -> &watch::Sender<CancelLevel> {
        &self.cancel_tx
    }

    #[cfg(test)]
    pub(crate) fn pending_for_test(cancel_tx: watch::Sender<CancelLevel>) -> Self {
        let (steering_tx, _steering_rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(std::future::pending());
        Self {
            steering_tx,
            cancel_tx,
            task,
        }
    }

    /// Spawn using a cancellation channel whose receiver can also be embedded
    /// in the tool executor before the loop starts.
    pub(crate) fn spawn_with_cancel(
        config: AgentLoopConfig,
        provider: Arc<dyn LlmProvider>,
        app_event_tx: AppEventTx,
        cancel_tx: watch::Sender<CancelLevel>,
        cancel_rx: watch::Receiver<CancelLevel>,
    ) -> Self {
        let (steering_tx, steering_rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            run_agent_loop(config, provider, app_event_tx, steering_rx, cancel_rx).await;
        });
        Self {
            steering_tx,
            cancel_tx,
            task,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancel_channel_starts_at_none() {
        let (_tx, rx) = watch::channel(CancelLevel::None);
        assert_eq!(*rx.borrow(), CancelLevel::None);
    }

    #[test]
    fn steering_channel_accepts_messages() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        tx.send("continue".to_string()).unwrap();
        assert_eq!(rx.try_recv().unwrap(), "continue");
    }
}
