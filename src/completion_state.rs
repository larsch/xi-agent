use crate::completion::CompletionItem;

/// Holds the popup-completion and model-fetch state for the chat input.
///
/// # Methods that remain on `App`
///
/// The following methods from the original plan were not moved here because
/// they require fields owned by `App` beyond this struct's scope:
///
/// - `update_completions` — needs `textarea`, `thinking_supported`,
///   `loaded_skills`, `provider_instances`
/// - `should_fetch_models` — needs `textarea`
/// - `start_model_fetch` — needs `app_event_tx`, `check_token_preflight`
/// - `apply_model_list` — needs `trigger_auth_refresh`, `selection` state,
///   `set_selection_items`, `select_current_default`
/// - `apply_completion` — needs `textarea`
///
/// These stay on `App` but access completion fields via `self.completion.*`.
#[derive(Default)]
pub struct CompletionState {
    /// Items to display in the completion popup (empty = popup hidden).
    pub(crate) completions: Vec<CompletionItem>,
    /// Index of the currently highlighted completion row.
    pub(crate) completion_selected: usize,
    /// Cached model list from the provider; `None` until first successful fetch.
    pub(crate) available_models: Option<Vec<String>>,
    /// True while a `list_models` task is in flight.
    pub(crate) models_loading: bool,
    /// Set to the error message when the last model fetch failed.
    pub(crate) model_fetch_error: Option<String>,
}

impl CompletionState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Clear completions and reset selection index.
    pub fn clear(&mut self) {
        self.completions.clear();
        self.completion_selected = 0;
    }

    /// Navigate the completion selection down (wraps around).
    pub fn select_next(&mut self) {
        let len = self.completions.len();
        if len > 0 {
            self.completion_selected = (self.completion_selected + 1) % len;
        }
    }

    /// Navigate the completion selection up (wraps around).
    pub fn select_prev(&mut self) {
        let len = self.completions.len();
        if len > 0 {
            self.completion_selected = (self.completion_selected + len - 1) % len;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_items(n: usize) -> Vec<CompletionItem> {
        (0..n)
            .map(|i| CompletionItem {
                label: format!("item{i}"),
                detail: String::new(),
                complete_to: format!("/item{i} "),
                loading: false,
                error: false,
                match_range: None,
            })
            .collect()
    }

    #[test]
    fn clear_resets_completions_and_index() {
        let mut state = CompletionState::new();
        state.completions = make_items(3);
        state.completion_selected = 2;
        state.clear();
        assert!(state.completions.is_empty());
        assert_eq!(state.completion_selected, 0);
    }

    #[test]
    fn select_next_wraps_around() {
        let mut state = CompletionState::new();
        state.completions = make_items(3);
        state.completion_selected = 0;
        state.select_next();
        assert_eq!(state.completion_selected, 1);
        state.select_next();
        assert_eq!(state.completion_selected, 2);
        state.select_next();
        assert_eq!(state.completion_selected, 0); // wraps
    }

    #[test]
    fn select_prev_wraps_around() {
        let mut state = CompletionState::new();
        state.completions = make_items(3);
        state.completion_selected = 0;
        state.select_prev();
        assert_eq!(state.completion_selected, 2); // wraps
        state.select_prev();
        assert_eq!(state.completion_selected, 1);
    }

    #[test]
    fn select_next_noop_when_empty() {
        let mut state = CompletionState::new();
        state.select_next();
        assert_eq!(state.completion_selected, 0);
    }

    #[test]
    fn select_prev_noop_when_empty() {
        let mut state = CompletionState::new();
        state.select_prev();
        assert_eq!(state.completion_selected, 0);
    }

    #[test]
    fn clear_is_idempotent_on_empty_state() {
        let mut state = CompletionState::new();
        state.clear();
        assert!(state.completions.is_empty());
        assert_eq!(state.completion_selected, 0);
    }
}
