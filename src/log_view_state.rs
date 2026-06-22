use ratatui::text::Line;

/// Tracks the monotonic log revision and its pre-wrapped line cache.
///
/// Call `invalidate()` whenever log content changes. Call `ensure_cached()` in
/// the render path to populate or reuse the wrapped-line cache.
pub struct LogCache {
    /// Monotonic counter bumped on every log-content change.
    pub(crate) revision: u64,
    /// Pre-wrapped lines: `(revision, width, step_cursor, lines)`. Invalidated on bump.
    pub(crate) cached_lines: Option<(u64, usize, Option<usize>, Vec<Line<'static>>)>,
}

/// Tracks the maximum rendered line count observed during the current streaming
/// turn. When the total shrinks (e.g. an edit_file diff recalculates with fewer
/// lines), this lets us pad the visible output with blank lines so the viewport
/// doesn't pull old content down.
///
/// Cleared on user scroll, resize, and streaming end.
#[derive(Clone, Copy)]
pub(crate) struct PaddingState {
    /// Maximum total rendered line count observed.
    pub(crate) max_total_lines: usize,
}

impl LogCache {
    pub fn new() -> Self {
        Self {
            revision: 0,
            cached_lines: None,
        }
    }

    /// Bump the revision counter and drop the cached lines.
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

/// Scroll and cache state for the log pane.
pub struct LogViewState {
    /// Tracks log-revision and pre-wrapped line cache; call `log_cache.invalidate()`
    /// whenever visible log content changes.
    pub(crate) log_cache: LogCache,
    pub(crate) log_scroll: usize,
    /// When true, the view always follows the bottom (auto-scrolls).
    pub(crate) auto_scroll: bool,
    /// Height of the log pane from the last draw — used as page-size scrolling.
    pub(crate) last_log_height: usize,
    /// Width of the log pane from the last draw — used to detect resize.
    pub(crate) last_log_width: usize,
    /// When true, tool bodies are rendered without truncation.
    pub(crate) full_output: bool,
    /// Padding state for the last message block during streaming.
    /// Cleared on user scroll, invalidated on resize (via [`LogViewState::invalidate`]).
    pub(crate) last_block_padding: Option<PaddingState>,
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
            last_block_padding: None,
        }
    }

    pub fn invalidate(&mut self) {
        self.log_cache.invalidate();
    }

    /// Clear the block-padding state (on resize, scroll, or streaming end).
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

    /// Toggle untruncated tool body display and invalidate the line cache.
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
