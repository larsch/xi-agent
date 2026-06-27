use crate::completion::CompletionItem;

/// Maximum number of rows shown in the selection menu before scrolling.
pub const MAX_SELECTION_VISIBLE: usize = 12;

/// Filters `all_items` by `query` and updates `items`, `selected`, and `scroll`.
fn filter_and_clip(
    query: &str,
    all_items: &[CompletionItem],
    items: &mut Vec<CompletionItem>,
    selected: &mut usize,
    scroll: &mut usize,
) {
    let q = query.trim();
    if q.is_empty() {
        *items = all_items.to_vec();
    } else {
        let needle = q.to_lowercase();
        *items = all_items
            .iter()
            .filter(|item| {
                item.label.to_lowercase().contains(&needle)
                    || item.detail.to_lowercase().contains(&needle)
            })
            .cloned()
            .collect();
    }

    if items.is_empty() {
        *selected = 0;
        *scroll = 0;
        return;
    }

    if *selected >= items.len() {
        *selected = 0;
    }
    ensure_visible_impl(*selected, scroll, items.len());
}

fn ensure_visible_impl(selected: usize, scroll: &mut usize, len: usize) {
    if len == 0 {
        *scroll = 0;
        return;
    }
    if selected < *scroll {
        *scroll = selected;
    }
    if selected >= *scroll + MAX_SELECTION_VISIBLE {
        *scroll = selected + 1 - MAX_SELECTION_VISIBLE;
    }
}

/// Discriminates what kind of selection menu is currently open.
///
/// # Methods that remain on `App`
///
/// All selection-manipulation methods were not moved here because they
/// require fields owned by `App` beyond this struct's scope:
///
/// - `set_selection_items` — resets items and calls `apply_selection_filter`
/// - `select_current_default` — reads `current_model`, `current_thinking`,
///   `current_provider`
/// - `apply_selection_filter` — reads `selection.query` to filter items
/// - `ensure_selection_visible` — reads scroll bounds
/// - `enter_*_selection_mode` — reads provider instances, session list, etc.
/// - `exit_selection_mode` — clears the selection and triggers completions update
/// - `selection_select_next/prev/page_*` — coordinate with scroll bounds
/// - `apply_selection` — dispatches results into model/provider/login/ask flows
///
/// They remain on `App` accessing selection fields via `self.selection.*`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SelectionKind {
    Model,
    Thinking,
    Provider,
    ProviderBackendPreset,
    ProviderApiType,
    LoginProvider,
    ResumeSession,
    AskUser,
    LoginAction,
    ConfirmProviderRemoval,
    KeybindingHelp,
}

/// All state for the selection menu panel.
pub struct SelectionState {
    /// True when the selection picker is active.
    pub(crate) active: bool,
    /// Header title shown in the selection menu.
    pub(crate) title: &'static str,
    /// Items currently visible (after filtering).
    pub(crate) items: Vec<CompletionItem>,
    /// Unfiltered source items for search filtering.
    pub(crate) all_items: Vec<CompletionItem>,
    /// Current free-text search query.
    pub(crate) query: String,
    /// Kind of selection currently being displayed.
    pub(crate) kind: Option<SelectionKind>,
    /// Index of the currently highlighted row.
    pub(crate) selected: usize,
    /// First visible item index (scroll offset).
    pub(crate) scroll: usize,
}

impl SelectionState {
    pub fn new() -> Self {
        Self {
            active: false,
            title: "",
            items: Vec::new(),
            all_items: Vec::new(),
            query: String::new(),
            kind: None,
            selected: 0,
            scroll: 0,
        }
    }

    /// Activate the picker with a given kind, title, and item list.
    ///
    /// Clears the query, resets scroll, and applies the (empty) filter so
    /// `items` is fully populated and `selected`/`scroll` are consistent.
    pub fn activate(
        &mut self,
        kind: SelectionKind,
        title: &'static str,
        all_items: Vec<CompletionItem>,
    ) {
        self.active = true;
        self.kind = Some(kind);
        self.title = title;
        self.query.clear();
        self.all_items = all_items;
        self.selected = 0;
        self.scroll = 0;
        self.apply_filter();
    }

    /// Reset the picker to inactive/empty state.
    pub fn reset(&mut self) {
        self.active = false;
        self.kind = None;
        self.items.clear();
        self.all_items.clear();
        self.query.clear();
        self.selected = 0;
        self.scroll = 0;
    }

    /// Re-filter `all_items` using `query` and update `items`, `selected`, `scroll`.
    pub fn apply_filter(&mut self) {
        filter_and_clip(
            &self.query.clone(),
            &self.all_items.clone(),
            &mut self.items,
            &mut self.selected,
            &mut self.scroll,
        );
    }

    /// Ensure `selected` is within the visible scroll window.
    pub fn ensure_visible(&mut self) {
        ensure_visible_impl(self.selected, &mut self.scroll, self.items.len());
    }

    /// Replace the item list and reapply the current filter.
    pub fn set_items(&mut self, all_items: Vec<CompletionItem>) {
        self.all_items = all_items;
        self.selected = 0;
        self.scroll = 0;
        self.apply_filter();
    }
}

impl Default for SelectionState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_produces_inactive_empty_state() {
        let s = SelectionState::new();
        assert!(!s.active);
        assert_eq!(s.title, "");
        assert!(s.items.is_empty());
        assert!(s.all_items.is_empty());
        assert_eq!(s.query, "");
        assert!(s.kind.is_none());
        assert_eq!(s.selected, 0);
        assert_eq!(s.scroll, 0);
    }

    #[test]
    fn default_equals_new() {
        let a = SelectionState::new();
        let b = SelectionState::default();
        assert_eq!(a.active, b.active);
        assert_eq!(a.title, b.title);
        assert_eq!(a.selected, b.selected);
        assert_eq!(a.scroll, b.scroll);
        assert!(b.kind.is_none());
    }

    #[test]
    fn max_selection_visible_is_twelve() {
        assert_eq!(MAX_SELECTION_VISIBLE, 12);
    }
}
