use crate::agent::types::AgentEvent;
use crate::app_event::{AppEvent, AppEventTx, SendIgnore};

/// Consumer-facing event boundary for the reusable agent loop.
pub(crate) trait AgentEventSink: Send + Sync {
    fn emit(&self, event: AgentEvent);
}

pub(crate) struct AppEventSink {
    tx: AppEventTx,
}

impl AppEventSink {
    pub(crate) fn new(tx: AppEventTx) -> Self {
        Self { tx }
    }
}

impl AgentEventSink for AppEventSink {
    fn emit(&self, event: AgentEvent) {
        self.tx.send_ignore(AppEvent::Agent(event));
    }
}

impl AgentEventSink for AppEventTx {
    fn emit(&self, event: AgentEvent) {
        self.send_ignore(AppEvent::Agent(event));
    }
}

impl<T: AgentEventSink + ?Sized> AgentEventSink for &T {
    fn emit(&self, event: AgentEvent) {
        (*self).emit(event);
    }
}

/// Emit an event through any agent event sink.
pub(crate) fn send_agent_event<S: AgentEventSink + ?Sized>(sink: &S, event: AgentEvent) {
    sink.emit(event);
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc::unbounded_channel;

    #[test]
    fn app_event_adapter_forwards_agent_events() {
        let (tx, mut rx) = unbounded_channel();
        send_agent_event(&tx, AgentEvent::Done);
        assert!(matches!(
            rx.try_recv(),
            Ok(AppEvent::Agent(AgentEvent::Done))
        ));
    }
}
