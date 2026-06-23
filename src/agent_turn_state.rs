use crate::app::StreamingStatus;
use std::cell::Cell;

/// Groups the three fields that track the progress of a live agent turn.
///
/// Writes go through methods to keep the invariants clear:
/// `start()` / `end()` for turn lifecycle, `record_output()` for visible
/// output arriving, `set_status()` for mid-turn status updates.
/// Fields remain readable (`pub(crate)`) for pattern matches in UI/tests.
pub(crate) struct AgentTurnState {
    /// Current streaming state; `None` when no turn is active.
    pub(crate) status: Option<StreamingStatus>,
    /// Throbber animation frame index, advanced on every UI tick while streaming.
    pub(crate) tick: u8,
    /// Instant of the last visible agent output (text/thinking tokens, tool
    /// calls, tool results, etc.); used to suppress the throbber while output
    /// is actively arriving and re-show it after a short idle time.
    pub(crate) last_output_at: Option<std::time::Instant>,
    /// Track the last reported visible state so we only log transitions.
    last_reported_visible: Cell<Option<bool>>,
}

impl AgentTurnState {
    pub(crate) fn new() -> Self {
        Self {
            status: None,
            tick: 0,
            last_output_at: None,
            last_reported_visible: Cell::new(None),
        }
    }

    /// Returns true when a turn is active (streaming or waiting for first token).
    pub(crate) fn is_active(&self) -> bool {
        matches!(
            self.status,
            Some(StreamingStatus::Waiting | StreamingStatus::Message(_))
        )
    }

    /// Begin a new agent turn: set status to Waiting and clear last_output_at.
    pub(crate) fn start(&mut self) {
        log::debug!(
            "[THROB] start() — status → Waiting, last_output_at cleared | was_active={}",
            self.is_active()
        );
        self.status = Some(StreamingStatus::Waiting);
        self.last_output_at = None;
    }

    /// End the current turn: clear status and last_output_at.
    pub(crate) fn end(&mut self) {
        log::debug!(
            "[THROB] end() — status → None, last_output_at cleared | was_active={} last_output_age={:?}",
            self.is_active(),
            self.last_output_at.map(|t| t.elapsed()),
        );
        self.status = None;
        self.last_output_at = None;
    }

    /// Update the mid-turn status message without touching last_output_at.
    pub(crate) fn set_status(&mut self, status: Option<StreamingStatus>) {
        log::debug!(
            "[THROB] set_status({:?}) | was_active={}",
            status.as_ref().map(|s| match s {
                StreamingStatus::Waiting => "Waiting",
                StreamingStatus::Message(_) => "Message(..)",
                StreamingStatus::CompletedMessage(_) => "CompletedMessage(..)",
            }),
            self.is_active()
        );
        self.status = status;
    }

    /// Record that visible output has just arrived.
    pub(crate) fn record_output(&mut self, caller: &str) {
        let prev_age = self.last_output_at.map(|t| t.elapsed());
        log::debug!("[THROB] record_output({caller}) last_output_age={prev_age:?}");
        self.last_output_at = Some(std::time::Instant::now());
    }

    /// Advance the throbber animation frame.  Called on every UI tick while active.
    pub(crate) fn advance_tick(&mut self) {
        if self.is_active() {
            self.tick = self.tick.wrapping_add(1);
        }
    }

    /// Returns true when the throbber should be visible.
    ///
    /// Caller must supply whether there is a pending ask or freeform mode
    /// active (those come from other parts of `App`).
    pub(crate) fn throbber_visible(&self, has_pending_ask: bool) -> bool {
        if !self.is_active() {
            return self.report_visible(false, "!is_active");
        }
        if has_pending_ask {
            return self.report_visible(false, "has_pending_ask");
        }
        match self.last_output_at {
            None => self.report_visible(true, "last_output_at=None"),
            Some(t) => {
                let elapsed = t.elapsed();
                let visible = elapsed >= std::time::Duration::from_millis(240);
                if !visible {
                    return self.report_visible(false, &format!("recent_output({elapsed:?})"));
                }
                self.report_visible(true, &format!("idle_since_output({elapsed:?})"))
            }
        }
    }

    /// Log state transitions in throbber visibility so we can trace
    /// the exact moment it changes without noise from repeated checks.
    fn report_visible(&self, visible: bool, reason: &str) -> bool {
        let prev = self.last_reported_visible.get();
        if prev != Some(visible) {
            log::debug!("[THROB] visible → {visible} ({reason})");
            self.last_reported_visible.set(Some(visible));
        }
        visible
    }
}
