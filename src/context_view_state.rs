/// State for the deferred system-prompt reader.
///
/// The chat log remains owned by `LogViewState`; this type only saves the
/// viewport position needed to return to it and tracks reader scrolling.
pub(crate) struct ContextViewState {
    pub(crate) active: bool,
    pub(crate) scroll: usize,
    pub(crate) max_scroll: usize,
    saved_log_position: Option<(usize, bool)>,
}

impl ContextViewState {
    pub(crate) fn new() -> Self {
        Self {
            active: false,
            scroll: 0,
            max_scroll: 0,
            saved_log_position: None,
        }
    }

    pub(crate) fn open(&mut self, log_scroll: usize, auto_scroll: bool) {
        if self.active {
            return;
        }
        self.saved_log_position = Some((log_scroll, auto_scroll));
        self.active = true;
        self.scroll = 0;
        self.max_scroll = 0;
    }

    pub(crate) fn close(&mut self) -> Option<(usize, bool)> {
        if !self.active {
            return None;
        }
        self.active = false;
        self.scroll = 0;
        self.max_scroll = 0;
        self.saved_log_position.take()
    }

    pub(crate) fn scroll_up(&mut self, lines: usize) {
        self.scroll = self.scroll.saturating_sub(lines);
    }

    pub(crate) fn scroll_down(&mut self, lines: usize, max_scroll: usize) {
        self.max_scroll = max_scroll;
        self.scroll = self.scroll.saturating_add(lines).min(max_scroll);
    }

    pub(crate) fn jump_to_start(&mut self) {
        self.scroll = 0;
    }

    pub(crate) fn jump_to_end(&mut self, max_scroll: usize) {
        self.max_scroll = max_scroll;
        self.scroll = max_scroll;
    }

    pub(crate) fn clamp_scroll(&mut self, max_scroll: usize) {
        self.max_scroll = max_scroll;
        self.scroll = self.scroll.min(max_scroll);
    }
}

impl Default for ContextViewState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::ContextViewState;

    #[test]
    fn open_close_preserves_chat_position_and_resets_reader_scroll() {
        let mut state = ContextViewState::new();
        state.open(42, false);
        state.scroll_down(10, 20);
        state.open(9, true); // Reopening an active view must not replace saved state.
        assert_eq!(state.close(), Some((42, false)));
        assert!(!state.active);
        assert_eq!(state.scroll, 0);
        assert_eq!(state.close(), None);
    }

    #[test]
    fn reader_scroll_is_bounded_and_jumps_to_ends() {
        let mut state = ContextViewState::new();
        state.jump_to_end(4);
        assert_eq!(state.scroll, 4);
        state.scroll_down(5, 4);
        assert_eq!(state.scroll, 4);
        state.scroll_up(2);
        assert_eq!(state.scroll, 2);
        state.clamp_scroll(1);
        assert_eq!(state.scroll, 1);
        state.jump_to_start();
        assert_eq!(state.scroll, 0);
    }
}
