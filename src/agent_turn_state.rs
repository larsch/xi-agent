use crate::agent::types::AgentActivity;
use crate::app::StreamingStatus;
use crate::config::ThrobberConfig;
use std::cell::Cell;
use std::time::Instant;

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
    config: ThrobberConfig,
    expected_interval_ms: f64,
    last_growth_at: Option<Instant>,
    /// Generation of the current agent turn.
    turn_generation: u64,
    /// Track the last reported visible state so we only log transitions.
    last_reported_visible: Cell<Option<bool>>,
}

impl AgentTurnState {
    pub(crate) fn new(config: ThrobberConfig) -> Self {
        let config = config.normalized();
        Self {
            status: None,
            tick: 0,
            activity: AgentActivity::ModelRequest,
            activity_visible: false,
            holdoff_started_at: None,
            expected_interval_ms: config.low_confidence_target_ms as f64,
            last_growth_at: None,
            config,
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
        self.activity_visible = true;
        self.holdoff_started_at = None;
        self.expected_interval_ms = self.config.low_confidence_target_ms as f64;
        self.last_growth_at = None;
    }

    /// Continue an active turn after a tool batch without resetting activity
    /// visibility. A tool-result boundary starts another model request, but it
    /// is not a new user turn and must preserve a throbber that became visible
    /// while the tool was running.
    pub(crate) fn continue_turn(&mut self) {
        self.turn_generation = self.turn_generation.wrapping_add(1);
        self.activity = AgentActivity::ModelRequest;
        if !self.is_active() {
            self.status = Some(StreamingStatus::Waiting);
            self.activity_visible = true;
            self.holdoff_started_at = None;
            self.expected_interval_ms = self.config.low_confidence_target_ms as f64;
            self.last_growth_at = None;
        } else {
            self.activity_visible = true;
            self.holdoff_started_at = None;
            self.expected_interval_ms = self.config.low_confidence_target_ms as f64;
            self.last_growth_at = None;
        }
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

    /// Record one streamed chunk and restart the hidden hold-off.
    ///
    /// Chunk timing is deliberately separate from renderer layout timing:
    /// several chunks can update an existing rendered line without producing a
    /// positive line delta.
    pub(crate) fn record_chunk(&mut self, now: Instant) {
        if !self.is_active() {
            return;
        }
        if let Some(previous) = self.last_growth_at {
            let interval_ms = now.duration_since(previous).as_secs_f64() * 1000.0;
            let lower = self.config.lower_bound_ms as f64;
            let upper = self.config.upper_bound_ms as f64;
            if (lower..=upper).contains(&interval_ms) {
                let alpha = self.config.alpha;
                self.expected_interval_ms =
                    alpha * interval_ms + (1.0 - alpha) * self.expected_interval_ms;
            }
        }
        self.last_growth_at = Some(now);
        if !self.activity_visible {
            self.holdoff_started_at = Some(now);
        }
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
                    if delta > 0
                        && (anchor_padding == usize::MAX || delta > anchor_padding as isize)
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
        let timeout_ms = (2.0 * self.expected_interval_ms).clamp(
            self.config.lower_bound_ms as f64,
            self.config.upper_bound_ms as f64,
        );
        if !self.activity_visible
            && self.holdoff_started_at.is_some_and(|started| {
                now.duration_since(started).as_secs_f64() * 1000.0 >= timeout_ms
            })
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
    use crate::config::ThrobberConfig;
    use std::time::{Duration, Instant};

    #[test]
    fn request_starts_with_visible_throbber() {
        let mut state = AgentTurnState::new(ThrobberConfig::default());
        state.start();
        assert!(state.throbber_visible(false));
    }

    #[test]
    fn visible_throbber_survives_non_growth_and_activity_changes() {
        let mut state = AgentTurnState::new(ThrobberConfig::default());
        state.start();
        let visible_at = Instant::now() + Duration::from_millis(4_001);
        state.update_visual_state(None, visible_at);
        state.set_activity(crate::agent::types::AgentActivity::LocalWork);
        state.update_visual_state(Some(VisualUpdate::Delta(0)), visible_at);
        assert!(state.throbber_visible(false));
    }

    #[test]
    fn tool_continuation_preserves_visible_throbber() {
        let mut state = AgentTurnState::new(ThrobberConfig::default());
        state.start();
        let visible_at = Instant::now() + Duration::from_millis(4_001);
        state.update_visual_state(None, visible_at);
        assert!(state.throbber_visible(false));

        state.continue_turn();
        assert!(state.is_active());
        assert!(state.throbber_visible(false));
    }

    #[test]
    fn output_growth_hides_visible_throbber_and_restarts_holdoff() {
        let mut state = AgentTurnState::new(ThrobberConfig::default());
        state.start();
        let now = Instant::now();
        state.update_visual_state(None, now + Duration::from_millis(4_001));
        assert!(state.throbber_visible(false));

        state.update_visual_state_with_padding(
            Some(VisualUpdate::Delta(1)),
            0,
            now + Duration::from_millis(1),
        );
        assert!(!state.throbber_visible(false));
        state.update_visual_state(None, now + Duration::from_millis(3_999));
        assert!(!state.throbber_visible(false));
        state.update_visual_state(None, now + Duration::from_millis(4_001));
        assert!(state.throbber_visible(false));
    }

    #[test]
    fn regular_growth_keeps_throbber_hidden_after_interval_estimation() {
        let mut state = AgentTurnState::new(ThrobberConfig::default());
        state.start();
        let now = Instant::now();

        for i in 0..40 {
            state.record_chunk(now + Duration::from_millis(i * 100));
            if i == 0 {
                state.update_visual_state_with_padding(Some(VisualUpdate::Delta(1)), 0, now);
            }
            assert!(!state.throbber_visible(false));
        }
    }

    #[test]
    fn regular_content_updates_reset_holdoff_and_update_interval() {
        let mut state = AgentTurnState::new(ThrobberConfig::default());
        state.start();
        let now = Instant::now();

        state.update_visual_state(Some(VisualUpdate::Delta(1)), now);
        assert!(!state.throbber_visible(false));
        state.record_chunk(now + Duration::from_millis(600));
        state.record_chunk(now + Duration::from_millis(1_200));
        state.record_chunk(now + Duration::from_millis(1_800));

        // A chunk resets the holdoff. It must not expire based on the
        // original hide time while chunks continue to arrive.
        state.update_visual_state(None, now + Duration::from_millis(4_700));
        assert!(!state.throbber_visible(false));
        state.update_visual_state(None, now + Duration::from_millis(5_000));
        assert!(state.throbber_visible(false));
    }
}
