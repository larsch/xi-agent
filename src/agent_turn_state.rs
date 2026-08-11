use crate::agent::types::AgentActivity;
use crate::app::StreamingStatus;
use std::cell::Cell;
use std::time::{Duration, Instant};

/// How the rendered log changed during one draw preparation pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VisualUpdate {
    /// Net change in rendered visual lines across all affected blocks.
    Delta(isize),
    /// The layout changed without a content-line delta.
    NonContentLayoutChange,
}

/// Groups the three fields that track the progress of a live agent turn.
///
/// Writes go through methods to keep the invariants clear:
/// `start()` / `end()` for turn lifecycle, `update_visual_state()` for
/// renderer-confirmed visual changes, and `set_status()` for mid-turn status
/// updates.
/// Fields remain readable (`pub(crate)`) for pattern matches in UI/tests.
pub(crate) struct AgentTurnState {
    /// Current streaming state; `None` when no turn is active.
    pub(crate) status: Option<StreamingStatus>,
    /// Throbber animation frame index, advanced on every UI tick while streaming.
    pub(crate) tick: u8,
    /// Current agent-loop activity, used only to select throbber visuals.
    pub(crate) activity: AgentActivity,
    /// Current throbber visibility, independent of the hold-off timer.
    pub(crate) activity_visible: bool,
    /// Start of the current hidden-state hold-off.
    pub(crate) holdoff_started_at: Option<Instant>,
    /// Generation of the current agent turn.
    turn_generation: u64,
    /// Track the last reported visible state so we only log transitions.
    last_reported_visible: Cell<Option<bool>>,
}

impl AgentTurnState {
    pub(crate) fn new() -> Self {
        Self {
            status: None,
            tick: 0,
            activity: AgentActivity::ModelRequest,
            activity_visible: false,
            holdoff_started_at: None,
            turn_generation: 0,
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

    /// Set the agent-loop activity used to select throbber visuals.
    pub(crate) fn set_activity(&mut self, activity: AgentActivity) {
        self.activity = activity;
    }

    pub(crate) fn turn_generation(&self) -> u64 {
        self.turn_generation
    }

    /// Begin a new agent turn: set status to Waiting and reset visual state.
    pub(crate) fn start(&mut self) {
        self.turn_generation = self.turn_generation.wrapping_add(1);
        self.activity = AgentActivity::ModelRequest;
        self.status = Some(StreamingStatus::Waiting);
        self.activity_visible = false;
        self.holdoff_started_at = Some(Instant::now());
    }

    /// End the current turn and hide the activity row.
    pub(crate) fn end(&mut self) {
        self.status = None;
        self.activity = AgentActivity::ModelRequest;
        self.activity_visible = false;
        self.holdoff_started_at = None;
    }

    /// Update the mid-turn status message without touching visual timing.
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

    /// Apply one renderer-confirmed visual update and poll the hidden hold-off.
    pub(crate) fn update_visual_state(&mut self, update: Option<VisualUpdate>, now: Instant) {
        self.update_visual_state_with_padding(update, usize::MAX, now);
    }

    /// Apply a visual delta after accounting for anchor padding. The activity
    /// row is consumed only by growth that exceeds the available padding.
    pub(crate) fn update_visual_state_with_padding(
        &mut self,
        update: Option<VisualUpdate>,
        anchor_padding: usize,
        now: Instant,
    ) {
        let before_visible = self.activity_visible;
        let before_holdoff = self.holdoff_started_at;
        if !self.is_active() {
            log::debug!(
                target: "throbber.trace",
                "visual update ignored: inactive update={update:?} visible={before_visible}"
            );
            return;
        }
        if let Some(update) = update {
            match update {
                VisualUpdate::Delta(delta)
                    if anchor_padding != usize::MAX
                        && delta > anchor_padding as isize
                        && self.activity_visible =>
                {
                    self.activity_visible = false;
                    self.holdoff_started_at = Some(now);
                }
                VisualUpdate::Delta(_) | VisualUpdate::NonContentLayoutChange => {
                    if !self.activity_visible {
                        self.holdoff_started_at.get_or_insert(now);
                    }
                }
            }
        }
        if !self.activity_visible
            && self
                .holdoff_started_at
                .is_some_and(|started| now.duration_since(started) >= Duration::from_millis(240))
        {
            self.activity_visible = true;
            self.holdoff_started_at = None;
        }
        log::debug!(
            target: "throbber.trace",
            "visual update: update={update:?} visible={before_visible}->{:?} holdoff={before_holdoff:?}->{:?} active={} now={now:?}",
            self.activity_visible,
            self.holdoff_started_at,
            self.is_active()
        );
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
        self.report_visible(self.activity_visible, "activity_state")
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

#[cfg(test)]
mod tests {
    use super::{AgentTurnState, VisualUpdate};
    use std::time::{Duration, Instant};

    #[test]
    fn holdoff_expires_only_during_visual_state_update() {
        let mut state = AgentTurnState::new();
        state.start();
        assert!(!state.throbber_visible(false));

        let now = Instant::now();
        assert!(!state.throbber_visible(false));
        state.update_visual_state(None, now + Duration::from_millis(239));
        assert!(!state.throbber_visible(false));
        state.update_visual_state(None, now + Duration::from_millis(241));
        assert!(state.throbber_visible(false));
    }

    #[test]
    fn visible_throbber_survives_non_growth_and_activity_changes() {
        let mut state = AgentTurnState::new();
        state.start();
        let visible_at = Instant::now() + Duration::from_secs(1);
        state.update_visual_state(None, visible_at);
        state.set_activity(crate::agent::types::AgentActivity::LocalWork);
        state.update_visual_state(Some(VisualUpdate::Delta(0)), visible_at);
        assert!(state.throbber_visible(false));
    }

    #[test]
    fn output_growth_hides_visible_throbber_and_restarts_holdoff() {
        let mut state = AgentTurnState::new();
        state.start();
        let now = Instant::now();
        state.update_visual_state(None, now + Duration::from_millis(241));
        assert!(state.throbber_visible(false));

        state.update_visual_state_with_padding(Some(VisualUpdate::Delta(1)), 0, now);
        assert!(!state.throbber_visible(false));
        state.update_visual_state(None, now + Duration::from_millis(239));
        assert!(!state.throbber_visible(false));
        state.update_visual_state(None, now + Duration::from_millis(241));
        assert!(state.throbber_visible(false));
    }
}
