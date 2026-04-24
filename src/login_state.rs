use std::sync::{Arc, atomic::AtomicBool};

use crate::auth::AuthFlow;

/// Actions available in the login action menu.
///
/// # Methods that remain on `App`
///
/// All login-flow methods were not moved here because they require fields
/// owned by `App` beyond this struct's scope:
///
/// - `apply_login_event` — reads/writes `current_provider`, triggers model
///   fetch, calls `update_completions`, `trigger_auth_refresh`
/// - `enter_login_selection_mode` — builds selection items, calls
///   `set_selection_items`
/// - `enter_login_action_menu` — reads `login.url`, `login.code`, builds
///   `CompletionItem` list
/// - `check_token_preflight` / `trigger_auth_refresh` — orchestrate auth
///   retry across login and model-fetch flows
/// - `handle_login_action` — dispatches `LoginActionKind` into clipboard,
///   browser open, and cancel flows
///
/// They remain on `App` accessing login fields via `self.login.*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginActionKind {
    OpenBrowser,
    CopyUrl,
    CopyCode,
    Cancel,
}

/// All state for the login/authentication panel.
pub struct LoginState {
    pub(crate) active: bool,
    pub(crate) provider: Option<String>,
    pub(crate) info: String,
    pub(crate) url: Option<String>,
    pub(crate) code: Option<String>,
    /// Which OAuth flow is in use; drives the UI's instruction text and
    /// available keyboard actions.
    pub(crate) auth_flow: Option<AuthFlow>,
    pub(crate) needs_rebuild: bool,
    pub(crate) refresh_in_progress: bool,
    pub(crate) retry_after_refresh: bool,
    /// Set when a `list_models` call fails with a 401 so the fetch is
    /// re-issued automatically once the token refresh completes.
    pub(crate) retry_model_fetch_after_refresh: bool,
    pub(crate) auth_retry_budget: u8,
    pub(crate) cancel: Option<Arc<AtomicBool>>,
    /// Persistent clipboard instance used during the login flow.
    ///
    /// On Linux the clipboard is owned by the process: dropping the
    /// `arboard::Clipboard` instance releases ownership and the text
    /// disappears from other applications.  We therefore keep it alive for
    /// the entire duration of the login panel and only drop it once login
    /// finishes.
    pub(crate) clipboard: Option<arboard::Clipboard>,
}

impl LoginState {
    pub fn new() -> Self {
        Self {
            active: false,
            provider: None,
            info: String::new(),
            url: None,
            code: None,
            auth_flow: None,
            needs_rebuild: false,
            refresh_in_progress: false,
            retry_after_refresh: false,
            retry_model_fetch_after_refresh: false,
            auth_retry_budget: 0,
            cancel: None,
            clipboard: None,
        }
    }
}

impl Default for LoginState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_produces_inactive_empty_state() {
        let s = LoginState::new();
        assert!(!s.active);
        assert!(s.provider.is_none());
        assert_eq!(s.info, "");
        assert!(s.url.is_none());
        assert!(s.code.is_none());
        assert!(s.auth_flow.is_none());
        assert!(!s.needs_rebuild);
        assert!(!s.refresh_in_progress);
        assert!(!s.retry_after_refresh);
        assert!(!s.retry_model_fetch_after_refresh);
        assert_eq!(s.auth_retry_budget, 0);
        assert!(s.cancel.is_none());
        assert!(s.clipboard.is_none());
    }

    #[test]
    fn default_equals_new() {
        let a = LoginState::new();
        let b = LoginState::default();
        assert_eq!(a.active, b.active);
        assert_eq!(a.provider, b.provider);
        assert_eq!(a.info, b.info);
        assert_eq!(a.needs_rebuild, b.needs_rebuild);
        assert_eq!(a.auth_retry_budget, b.auth_retry_budget);
    }

    #[test]
    fn login_action_kind_variants_are_distinct() {
        assert_ne!(LoginActionKind::OpenBrowser, LoginActionKind::CopyUrl);
        assert_ne!(LoginActionKind::CopyUrl, LoginActionKind::CopyCode);
        assert_ne!(LoginActionKind::CopyCode, LoginActionKind::Cancel);
        // round-trip via copy
        let k = LoginActionKind::OpenBrowser;
        assert_eq!(k, LoginActionKind::OpenBrowser);
    }
}
