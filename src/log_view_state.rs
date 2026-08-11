use std::collections::HashSet;

use ratatui::text::Line;

use crate::mouse_select::LineSource;

/// Type alias for the cached log lines + hit map tuple.
pub(crate) type CachedLogLines = (
    u64,
    usize,
    Option<usize>,
    Vec<Line<'static>>,
    Vec<LineSource>,
);

/// Tracks the monotonic log revision and its pre-wrapped line cache.
pub struct LogCache {
    pub(crate) revision: u64,
    pub(crate) cached_lines: Option<CachedLogLines>,
}

impl LogCache {
    pub fn new() -> Self {
        Self {
            revision: 0,
            cached_lines: None,
        }
    }

    pub fn invalidate(&mut self) {
        self.revision = self.revision.wrapping_add(1);
        self.cached_lines = None;
    }
}

impl Default for LogCache {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy)]
pub(crate) struct PaddingState {
    pub(crate) max_total_lines: usize,
    pub(crate) inner_height_when_set: usize,
}

pub struct LogViewState {
    pub(crate) log_cache: LogCache,
    pub(crate) log_scroll: usize,
    pub(crate) auto_scroll: bool,
    pub(crate) last_log_height: usize,
    pub(crate) last_log_width: usize,
    pub(crate) full_output: bool,
    pub(crate) expanded_blocks: HashSet<String>,
    pub(crate) pending_anchor: Option<(String, usize)>,
    pub(crate) last_block_padding: Option<PaddingState>,
    pub(crate) turn_generation: Option<u64>,
    pub(crate) visual_baseline: Option<Vec<(String, usize, String)>>,
    pub(crate) visual_baseline_width: Option<usize>,
}

impl LogViewState {
    pub fn new() -> Self {
        Self {
            log_cache: LogCache::new(),
            log_scroll: 0,
            auto_scroll: true,
            last_log_height: 0,
            last_log_width: 0,
            full_output: false,
            expanded_blocks: HashSet::new(),
            pending_anchor: None,
            last_block_padding: None,
            turn_generation: None,
            visual_baseline: None,
            visual_baseline_width: None,
        }
    }

    pub fn invalidate(&mut self) {
        self.log_cache.invalidate();
    }

    pub fn begin_turn(&mut self, generation: u64) {
        self.turn_generation = Some(generation);
        self.visual_baseline = None;
        self.visual_baseline_width = None;
        self.log_cache.invalidate();
        self.clear_padding();
    }

    pub fn clear_turn_baseline(&mut self) {
        self.turn_generation = None;
        self.visual_baseline = None;
        self.visual_baseline_width = None;
        self.log_cache.invalidate();
        self.clear_padding();
    }

    pub(crate) fn take_visual_baseline(&mut self) -> Option<Vec<(String, usize, String)>> {
        self.visual_baseline.take()
    }

    pub(crate) fn set_visual_baseline(&mut self, baseline: Vec<(String, usize, String)>) {
        self.visual_baseline = Some(baseline);
    }

    pub fn toggle_expanded(&mut self, identity: String) {
        if !self.expanded_blocks.remove(&identity) {
            self.expanded_blocks.insert(identity);
        }
        self.invalidate();
        self.clear_padding();
    }

    pub fn clear_expanded(&mut self) {
        self.expanded_blocks.clear();
        self.pending_anchor = None;
        self.invalidate();
        self.clear_padding();
    }

    pub fn clear_padding(&mut self) {
        self.last_block_padding = None;
    }

    pub fn scroll_up(&mut self) {
        self.clear_padding();
        self.scroll_up_lines(self.last_log_height.max(1));
    }

    pub fn scroll_up_lines(&mut self, n: usize) {
        self.auto_scroll = false;
        self.log_scroll = self.log_scroll.saturating_sub(n);
    }

    pub fn scroll_down_lines(&mut self, n: usize) {
        self.log_scroll = self.log_scroll.saturating_add(n);
    }

    pub fn scroll_down(&mut self) {
        self.clear_padding();
        self.auto_scroll = false;
        self.log_scroll = self.log_scroll.saturating_add(self.last_log_height.max(1));
    }

    pub fn toggle_full_output(&mut self) {
        self.full_output = !self.full_output;
        self.log_cache.invalidate();
        self.clear_padding();
    }
}

impl Default for LogViewState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::LogViewState;

    #[test]
    fn expansion_toggles_and_invalidates() {
        let mut state = LogViewState::new();
        let revision = state.log_cache.revision;
        state.toggle_expanded("block:1".into());
        assert!(state.expanded_blocks.contains("block:1"));
        assert!(state.log_cache.revision > revision);
        state.toggle_expanded("block:1".into());
        assert!(!state.expanded_blocks.contains("block:1"));
    }

    #[test]
    fn clearing_expansion_removes_pending_anchor() {
        let mut state = LogViewState::new();
        state.expanded_blocks.insert("block:1".into());
        state.pending_anchor = Some(("block:1".into(), 4));
        state.clear_expanded();
        assert!(state.expanded_blocks.is_empty());
        assert!(state.pending_anchor.is_none());
    }

    #[test]
    fn pending_anchor_stores_screen_relative_top() {
        let mut state = LogViewState::new();
        state.pending_anchor = Some(("block:1".into(), 2));
        assert_eq!(state.pending_anchor, Some(("block:1".into(), 2)));
    }
}
