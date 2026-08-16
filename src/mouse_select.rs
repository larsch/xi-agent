//! Mouse click-drag text selection and copy for the log view.
//!
//! Click-drag in the log area selects rendered text in screen space.
//! On release, the selected text is copied to the system clipboard.
//! Decoration prefixes (icons, margin markers) are stripped from the
//! copied text.
//!
//! Streaming blocks are excluded from interaction.  Scroll is locked
//! during drag and restored on release.

// ── LineSource ────────────────────────────────────────────────────────────────

/// Maps one rendered log line back to its visual properties.
#[derive(Debug, Clone)]
pub struct LineSource {
    /// Visual width (columns) of the decoration prefix on this line
    /// (icon glyphs + margin markers like `│ `).  Columns strictly less
    /// than this value are considered decoration area.
    pub decoration_width: u16,
    /// `true` when this line belongs to a streaming / incomplete message.
    pub streaming: bool,
    /// Stable logical block identity, when this line belongs to a block.
    pub block_identity: Option<String>,
    /// Whether the block is currently foldable.
    pub foldable: bool,
}

/// Type alias for the hit map: one entry per rendered log line.
pub type HitMap = Vec<LineSource>;

// ── MouseSelectState ──────────────────────────────────────────────────────────

/// Transient state for mouse click-drag selection.
pub struct MouseSelectState {
    /// `true` while the left button is held and a drag is in progress.
    pub button_down: bool,
    /// `true` while the mouse has moved since button-down (actual drag).
    pub drag_active: bool,
    /// Terminal-absolute column of the drag origin.
    pub drag_start_col: u16,
    /// Terminal-absolute row of the drag origin.
    pub drag_start_row: u16,
    /// Terminal-absolute column of the current drag position.
    pub drag_end_col: u16,
    /// Terminal-absolute row of the current drag position.
    pub drag_end_row: u16,

    /// Saved auto-scroll state, restored on mouse-up.
    saved_auto_scroll: Option<bool>,

    /// Mirror of the latest hit map from the last frame, used for
    /// decoration width lookup and streaming exclusion.
    pub hit_map: HitMap,

    /// Mirror of the latest visible rendered lines (after padding/scroll).
    /// Used to extract text during drag.
    pub visible_lines: Vec<ratatui::text::Line<'static>>,

    /// Log area top row in terminal coordinates.
    pub log_area_top: u16,
    /// Log area width in columns (excludes scrollbar).
    pub log_area_width: u16,
    /// Current log scroll offset.
    pub log_scroll: usize,
    /// Latest pointer position in terminal coordinates, if inside the log.
    pub hover_col: u16,
    pub hover_row: u16,
    pub hover_active: bool,
}

impl MouseSelectState {
    pub fn new() -> Self {
        Self {
            button_down: false,
            drag_active: false,
            drag_start_col: 0,
            drag_start_row: 0,
            drag_end_col: 0,
            drag_end_row: 0,
            saved_auto_scroll: None,
            hit_map: Vec::new(),
            visible_lines: Vec::new(),
            log_area_top: 0,
            log_area_width: 0,
            log_scroll: 0,
            hover_col: 0,
            hover_row: 0,
            hover_active: false,
        }
    }

    // ── Screen-space helpers ───────────────────────────────────────────────

    /// Return the normalized selection range as `(start_row, end_row,
    /// start_col, end_col)` in terminal-absolute coordinates, or `None`
    /// if no drag is active.
    pub fn selection_range(&self) -> Option<(u16, u16, u16, u16)> {
        if !self.drag_active {
            return None;
        }
        let start_row = self.drag_start_row.min(self.drag_end_row);
        let end_row = self.drag_start_row.max(self.drag_end_row);
        // For single-row drag, handle inverted columns.
        let (start_col, end_col) = if start_row == end_row {
            (
                self.drag_start_col.min(self.drag_end_col),
                self.drag_start_col.max(self.drag_end_col),
            )
        } else {
            // Multi-row: start row uses start col, end row uses end col.
            if self.drag_start_row <= self.drag_end_row {
                (self.drag_start_col, self.drag_end_col)
            } else {
                (self.drag_end_col, self.drag_start_col)
            }
        };
        Some((start_row, end_row, start_col, end_col))
    }

    /// Return `true` if the given absolute column is in the scrollbar area.
    fn in_scrollbar(&self, col: u16) -> bool {
        self.log_area_width > 0 && col >= self.log_area_width.saturating_sub(1)
    }

    /// Check whether a screen position is within the log content area
    /// (not in scrollbar, not outside the log area).
    fn is_in_log_area(&self, col: u16, row: u16) -> bool {
        if row < self.log_area_top {
            return false;
        }
        if self.in_scrollbar(col) {
            return false;
        }
        true
    }

    /// Extract visible text from the selected screen region.
    ///
    /// Strips decoration prefixes from each line and joins with newlines.
    pub fn extract_selected_text(&self) -> Option<String> {
        let (start_row, end_row, start_col, end_col) = self.selection_range()?;

        let mut lines: Vec<String> = Vec::new();

        for abs_row in start_row..=end_row {
            let vis_idx = abs_row.saturating_sub(self.log_area_top) as usize;
            if vis_idx >= self.visible_lines.len() {
                break;
            }

            let line = &self.visible_lines[vis_idx];
            let deco = self
                .hit_map
                .get(vis_idx)
                .map(|ls| ls.decoration_width)
                .unwrap_or(0);

            // Determine column bounds for this row.
            // col_to is inclusive (mouse column is the character cell under cursor).
            let (col_from, col_to) = if abs_row == start_row && abs_row == end_row {
                (start_col.max(deco), end_col + 1)
            } else if abs_row == start_row {
                (start_col.max(deco), u16::MAX)
            } else if abs_row == end_row {
                (deco, end_col + 1)
            } else {
                (deco, u16::MAX)
            };

            // Build text from spans, clipping to [col_from, col_to).
            let mut text = String::new();
            let mut col: u16 = 0;

            for span in &line.spans {
                let content: &str = span.content.as_ref();
                let span_width = unicode_width::UnicodeWidthStr::width(content) as u16;
                let span_end = col + span_width;

                // Entirely before the selection window — skip.
                if span_end <= col_from {
                    col = span_end;
                    continue;
                }
                // Entirely past the selection window — done.
                if col >= col_to {
                    break;
                }

                // The visible portion of this span in columns.
                let keep_from = col.max(col_from);
                let keep_to = span_end.min(col_to);
                if keep_from >= keep_to {
                    col = span_end;
                    continue;
                }

                // Walk characters, accumulating columns, picking those
                // that fall inside [keep_from, keep_to).
                let mut char_col: u16 = col;
                for ch in content.chars() {
                    let chw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0) as u16;
                    let ch_end = char_col + chw;
                    if ch_end > keep_from && char_col < keep_to {
                        text.push(ch);
                    }
                    char_col = ch_end;
                    if char_col >= keep_to && chw == 0 {
                        // zero-width char after range — include it
                    } else if char_col > keep_to {
                        // Past selection; remaining chars in this span are
                        // unreachable but we still advance col.
                    }
                }

                col = span_end;
            }

            lines.push(text);
        }

        while lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }

        if lines.is_empty() {
            return None;
        }

        Some(lines.join("\n"))
    }

    // ── Mouse event handlers ──────────────────────────────────────────────────

    pub fn handle_mouse_move(&mut self, col: u16, row: u16) -> bool {
        let changed = self.hover_col != col || self.hover_row != row || !self.hover_active;
        self.hover_col = col;
        self.hover_row = row;
        self.hover_active = true;
        changed
    }

    /// Handle a mouse-down event.  Returns `true` if a drag has begun.
    pub fn handle_mouse_down(&mut self, col: u16, row: u16, auto_scroll: bool) -> bool {
        if !self.is_in_log_area(col, row) {
            return false;
        }

        // Exclude streaming lines.
        let vis_idx = row.saturating_sub(self.log_area_top) as usize;
        if let Some(src) = self.hit_map.get(vis_idx)
            && src.streaming
        {
            return false;
        }

        self.button_down = true;
        self.drag_active = false;
        self.drag_start_col = col;
        self.drag_start_row = row;
        self.drag_end_col = col;
        self.drag_end_row = row;
        self.saved_auto_scroll = Some(auto_scroll);

        true
    }

    /// Handle a mouse-drag (move while button held).
    pub fn handle_mouse_drag(&mut self, col: u16, row: u16) {
        if !self.button_down {
            return;
        }
        self.drag_active = true;
        self.drag_end_col = col;
        self.drag_end_row = row;
    }

    /// Return the logical block clicked, if the button was released without a drag.
    pub fn clicked_block(&self) -> Option<String> {
        if self.drag_active {
            return None;
        }
        let vis_idx = self.drag_start_row.saturating_sub(self.log_area_top) as usize;
        self.hit_map
            .get(vis_idx)
            .filter(|source| source.foldable && !source.streaming)
            .and_then(|source| source.block_identity.clone())
    }

    /// Handle a mouse-up event.  Returns the selected text to copy, if any.
    pub fn handle_mouse_up(&mut self) -> Option<String> {
        let text = if self.drag_active {
            self.extract_selected_text()
        } else {
            None
        };
        self.button_down = false;
        self.drag_active = false;
        text
    }

    /// Return the saved auto-scroll value (if any).
    pub fn take_saved_auto_scroll(&mut self) -> Option<bool> {
        self.saved_auto_scroll.take()
    }
}

impl Default for MouseSelectState {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::text::{Line, Span};

    fn make_hit_map() -> HitMap {
        vec![
            LineSource {
                decoration_width: 0,
                streaming: false,
                block_identity: None,
                foldable: false,
            },
            LineSource {
                decoration_width: 3, // "💬 "
                streaming: false,
                block_identity: None,
                foldable: false,
            },
            LineSource {
                decoration_width: 3, // " │ "
                streaming: false,
                block_identity: None,
                foldable: false,
            },
            LineSource {
                decoration_width: 0,
                streaming: true,
                block_identity: None,
                foldable: false,
            },
        ]
    }

    fn make_visible_lines() -> Vec<Line<'static>> {
        vec![
            Line::from(vec![Span::raw("hello world")]),
            Line::from(vec![Span::raw("💬 "), Span::raw("assistant reply")]),
            Line::from(vec![Span::raw(" │ "), Span::raw("tool output")]),
            Line::from(vec![Span::raw("streaming line")]),
        ]
    }

    #[test]
    fn drag_copies_selected_text() {
        let mut state = MouseSelectState::new();
        state.hit_map = make_hit_map();
        state.visible_lines = make_visible_lines();
        state.log_area_top = 0;
        state.log_area_width = 80;
        state.log_scroll = 0;

        // Drag across first line.
        state.handle_mouse_down(0, 0, true);
        state.handle_mouse_drag(10, 0);
        let text = state.handle_mouse_up().unwrap();
        assert_eq!(text, "hello world");
    }

    #[test]
    fn drag_strips_decoration() {
        let mut state = MouseSelectState::new();
        state.hit_map = make_hit_map();
        state.visible_lines = make_visible_lines();
        state.log_area_top = 0;
        state.log_area_width = 80;
        state.log_scroll = 0;

        // Drag across the assistant line.  decoration_width=3 strips "💬 ".
        state.handle_mouse_down(0, 1, true);
        state.handle_mouse_drag(20, 1);
        let text = state.handle_mouse_up().unwrap();
        assert_eq!(text, "assistant reply");
    }

    #[test]
    fn drag_multiple_rows() {
        let mut state = MouseSelectState::new();
        state.hit_map = make_hit_map();
        state.visible_lines = make_visible_lines();
        state.log_area_top = 0;
        state.log_area_width = 80;
        state.log_scroll = 0;

        // Drag across rows 1+2.
        state.handle_mouse_down(0, 1, true);
        state.handle_mouse_drag(20, 2);
        let text = state.handle_mouse_up().unwrap();
        assert_eq!(text, "assistant reply\ntool output");
    }

    #[test]
    fn drag_ignores_streaming_lines() {
        let mut state = MouseSelectState::new();
        state.hit_map = make_hit_map();
        state.visible_lines = make_visible_lines();
        state.log_area_top = 0;
        state.log_area_width = 80;
        state.log_scroll = 0;

        // Click on streaming line (row 3) should be ignored.
        assert!(!state.handle_mouse_down(0, 3, true));
    }

    #[test]
    fn drag_ignores_scrollbar() {
        let mut state = MouseSelectState::new();
        state.hit_map = make_hit_map();
        state.visible_lines = make_visible_lines();
        state.log_area_top = 0;
        state.log_area_width = 10; // narrow, scrollbar at col 9
        state.log_scroll = 0;

        // Click in scrollbar column.
        assert!(!state.handle_mouse_down(9, 0, true));
        assert!(!state.handle_mouse_down(10, 0, true));
    }

    #[test]
    fn scroll_is_locked_during_drag() {
        let mut state = MouseSelectState::new();
        state.hit_map = make_hit_map();
        state.visible_lines = make_visible_lines();
        state.log_area_top = 0;
        state.log_area_width = 80;
        state.log_scroll = 0;

        state.handle_mouse_down(0, 0, false);
        assert!(state.button_down);
        assert!(!state.drag_active);
        assert_eq!(state.saved_auto_scroll, Some(false));
    }

    #[test]
    fn no_op_on_mouse_down_outside_log_area() {
        let mut state = MouseSelectState::new();
        state.hit_map = make_hit_map();
        state.visible_lines = make_visible_lines();
        state.log_area_top = 5; // log area starts at row 5
        state.log_area_width = 80;
        state.log_scroll = 0;

        assert!(!state.handle_mouse_down(0, 0, true));
    }

    #[test]
    fn drag_tool_output_strips_decoration() {
        let mut state = MouseSelectState::new();
        state.hit_map = make_hit_map();
        state.visible_lines = make_visible_lines();
        state.log_area_top = 0;
        state.log_area_width = 80;
        state.log_scroll = 0;

        // Drag across the tool output line (row 2, decoration_width=3, " │ " prefix).
        state.handle_mouse_down(0, 2, true);
        state.handle_mouse_drag(20, 2);
        let text = state.handle_mouse_up().unwrap();
        assert_eq!(text, "tool output");
    }

    #[test]
    fn drag_user_message_no_decoration() {
        let mut state = MouseSelectState::new();
        state.hit_map = make_hit_map();
        state.visible_lines = make_visible_lines();
        state.log_area_top = 0;
        state.log_area_width = 80;
        state.log_scroll = 0;

        // Drag across the user message line (row 0, decoration_width=0).
        state.handle_mouse_down(0, 0, true);
        state.handle_mouse_drag(5, 0);
        let text = state.handle_mouse_up().unwrap();
        // Full row 0 text is "hello world", dragging 0..5+1 gives "hello "
        assert_eq!(text, "hello ");
    }
}
