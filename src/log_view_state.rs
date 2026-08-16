use std::collections::HashSet;
use std::sync::Arc;

use crate::ui::log::{LogBlockCache, LogLayout};

/// Type alias for the cached log layout tuple: `(revision, width, step_cursor,
/// layout)`. The block layout is retained as the render source of truth; the
/// flattened line/source vectors are no longer cached (only the visible window
/// is materialized each frame).
pub(crate) type CachedLogLayout = (u64, usize, Option<usize>, LogLayout);

/// Tracks the monotonic log revision and its retained block layout.
pub struct LogCache {
    pub(crate) revision: u64,
    pub(crate) cached_layout: Option<CachedLogLayout>,
}

impl LogCache {
    pub fn new() -> Self {
        Self {
            revision: 0,
            cached_layout: None,
        }
    }

    pub fn invalidate(&mut self) {
        self.revision = self.revision.wrapping_add(1);
        self.cached_layout = None;
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
    /// Remaining anchor padding after applying visual line deltas.
    pub(crate) remaining: isize,
}

pub struct LogViewState {
    pub(crate) log_cache: LogCache,
    pub(crate) block_cache: LogBlockCache,
    pub(crate) log_scroll: usize,
    pub(crate) auto_scroll: bool,
    pub(crate) last_log_height: usize,
    pub(crate) last_log_width: usize,
    pub(crate) full_output: bool,
    pub(crate) expanded_blocks: HashSet<String>,
    pub(crate) pending_anchor: Option<(String, usize)>,
    pub(crate) last_block_padding: Option<PaddingState>,
    pub(crate) turn_generation: Option<u64>,
    pub(crate) visual_baseline: Option<Vec<(Arc<str>, usize)>>,
    pub(crate) visual_baseline_width: Option<usize>,
}

impl LogViewState {
    pub fn new() -> Self {
        Self {
            log_cache: LogCache::new(),
            block_cache: LogBlockCache::default(),
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
        // NOTE: this runs on every streaming token (via `take_dirty`). It must
        // NOT clear `block_cache`, which is fingerprint-keyed and handles
        // content growth itself; clearing it here would defeat the cache.
        self.log_cache.invalidate();
    }

    pub fn begin_turn(&mut self, generation: u64) {
        self.turn_generation = Some(generation);
        self.visual_baseline = None;
        self.visual_baseline_width = None;
        self.log_cache.invalidate();
        self.block_cache.clear();
        self.clear_padding();
    }

    pub fn clear_turn_baseline(&mut self) {
        self.turn_generation = None;
        self.visual_baseline = None;
        self.visual_baseline_width = None;
        self.log_cache.invalidate();
        self.block_cache.clear();
        self.clear_padding();
    }

    /// Reset visual comparison state after a tool batch is committed while
    /// preserving the active turn's activity-row state.
    pub(crate) fn begin_tool_continuation(&mut self) {
        self.reset_visual_comparison();
    }

    /// Invalidate the rendered log and drop any pending visual comparison
    /// state. The baseline is always reset together with the cache so the
    /// next draw cannot compare a user-initiated layout change (fold/unfold,
    /// full-output toggle) against the pre-change frame and misreport it as
    /// a streaming shrink that needs bottom anchor padding.
    fn reset_visual_comparison(&mut self) {
        self.visual_baseline = None;
        self.visual_baseline_width = None;
        self.invalidate();
        // `full_output` / `expanded_blocks` affect rendering but are not part
        // of the block cache key, so clear it on any toggle that changes them.
        self.block_cache.clear();
        self.clear_padding();
    }

    pub(crate) fn take_visual_baseline(&mut self) -> Option<Vec<(Arc<str>, usize)>> {
        self.visual_baseline.take()
    }

    pub(crate) fn set_visual_baseline(&mut self, baseline: Vec<(Arc<str>, usize)>) {
        self.visual_baseline = Some(baseline);
    }

    pub fn toggle_expanded(&mut self, identity: String) {
        if !self.expanded_blocks.remove(&identity) {
            self.expanded_blocks.insert(identity);
        }
        self.reset_visual_comparison();
    }

    pub fn clear_expanded(&mut self) {
        self.expanded_blocks.clear();
        self.pending_anchor = None;
        self.reset_visual_comparison();
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
        self.reset_visual_comparison();
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
    fn expansion_toggle_clears_visual_baseline() {
        let mut state = LogViewState::new();
        state.set_visual_baseline(vec![("message:0:tool".into(), 40)]);
        state.visual_baseline_width = Some(80);
        state.toggle_expanded("message:0:tool".into());
        assert!(state.visual_baseline.is_none());
        assert!(state.visual_baseline_width.is_none());
    }

    #[test]
    fn full_output_toggle_clears_visual_baseline() {
        let mut state = LogViewState::new();
        state.set_visual_baseline(vec![("message:0:tool".into(), 40)]);
        state.visual_baseline_width = Some(80);
        state.toggle_full_output();
        assert!(state.visual_baseline.is_none());
        assert!(state.visual_baseline_width.is_none());
    }

    #[test]
    fn clear_expanded_clears_visual_baseline() {
        let mut state = LogViewState::new();
        state.set_visual_baseline(vec![("message:0:tool".into(), 40)]);
        state.visual_baseline_width = Some(80);
        state.clear_expanded();
        assert!(state.visual_baseline.is_none());
        assert!(state.visual_baseline_width.is_none());
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
