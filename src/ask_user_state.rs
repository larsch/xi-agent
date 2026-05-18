use crate::agent::types::{AskUserOption, AskUserResponse};

/// A pending ask-user request from the agent, waiting for a user reply.
///
/// # Methods that remain on `App`
///
/// All ask-user interaction methods were not moved here because they require
/// fields owned by `App` beyond this struct's scope:
///
/// - `receive_ask_request` — calls `exit_selection_mode`, `reset_textarea`,
///   `set_selection_items`, writes to `selection.*`
/// - `enter_ask_freeform_mode` — calls `exit_selection_mode`, `reset_textarea`
/// - `begin_ask_freeform_typing` — calls `ensure_selection_visible`, reads
///   `selection.items`
/// - `cancel_ask_freeform_typing` — calls `reset_textarea`
/// - `submit_pending_ask_answer` — reads `textarea`
/// - `select_pending_ask_option` — calls `finish_pending_ask`
/// - `cancel_pending_ask` — calls `finish_pending_ask`, `abort_agent_loop`
/// - `finish_pending_ask` (private) — calls `exit_selection_mode`,
///   `reset_textarea`
///
/// They remain on `App` accessing ask-user fields via `self.ask_user.*`.
pub(crate) struct PendingAsk {
    pub(crate) question: String,
    pub(crate) options: Vec<AskUserOption>,
    pub(crate) allow_freeform: bool,
}

pub(crate) struct AskUserState {
    pub(crate) pending: Option<PendingAsk>,
    pub(crate) reply: Option<tokio::sync::oneshot::Sender<AskUserResponse>>,
    pub(crate) freeform_mode: bool,
}

impl AskUserState {
    pub fn new() -> Self {
        Self {
            pending: None,
            reply: None,
            freeform_mode: false,
        }
    }
    pub fn has_pending(&self) -> bool {
        self.pending.is_some()
    }

    pub fn allows_freeform(&self) -> bool {
        self.pending
            .as_ref()
            .map(|p| p.allow_freeform)
            .unwrap_or(false)
    }
}

impl Default for AskUserState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_produces_idle_state() {
        let s = AskUserState::new();
        assert!(s.pending.is_none());
        assert!(s.reply.is_none());
        assert!(!s.freeform_mode);
    }

    #[test]
    fn default_equals_new() {
        let a = AskUserState::new();
        let b = AskUserState::default();
        assert_eq!(a.freeform_mode, b.freeform_mode);
        assert!(b.pending.is_none());
        assert!(b.reply.is_none());
    }

    #[test]
    fn pending_ask_stores_fields() {
        let ask = PendingAsk {
            question: "Continue?".to_string(),
            options: vec![],
            allow_freeform: true,
        };
        assert_eq!(ask.question, "Continue?");
        assert!(ask.options.is_empty());
        assert!(ask.allow_freeform);
    }
}
