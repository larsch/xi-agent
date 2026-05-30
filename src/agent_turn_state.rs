use crate::app::StreamingStatus;

/// Groups the three fields that track the progress of a live agent turn.
///
/// All three are set and cleared together (both set when a turn starts,
/// both `status`/`last_output_at` cleared when a turn ends, `tick`
/// wraps continuously).
pub(crate) struct AgentTurnState {
    /// Current streaming state; `None` when no turn is active.
    pub(crate) status: Option<StreamingStatus>,
    /// Throbber animation frame index, advanced on every UI tick while streaming.
    pub(crate) tick: u8,
    /// Instant of the last visible agent output (text/thinking tokens, tool
    /// calls, tool results, etc.); used to suppress the throbber while output
    /// is actively arriving and re-show it after a short idle time.
    pub(crate) last_output_at: Option<std::time::Instant>,
}

impl AgentTurnState {
    pub(crate) fn new() -> Self {
        Self {
            status: None,
            tick: 0,
            last_output_at: None,
        }
    }

    /// Returns true when a turn is active (streaming or waiting for first token).
    pub(crate) fn is_active(&self) -> bool {
        matches!(
            self.status,
            Some(StreamingStatus::Waiting | StreamingStatus::Message(_))
        )
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
            return false;
        }
        if has_pending_ask {
            return false;
        }
        match self.last_output_at {
            None => true,
            Some(t) => t.elapsed() >= std::time::Duration::from_millis(240),
        }
    }
}
