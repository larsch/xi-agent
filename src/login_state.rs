use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use crate::{
    app_event::AppEventTx,
    auth::{self, AuthFlow, LoginEvent},
    completion::CompletionItem,
    llm::Message,
    log_view_state::LogCache,
    selection_state::{SelectionKind, SelectionState},
    session_manager::SessionManager,
};

// Internal complete_to tokens for login action items.
pub(crate) const LOGIN_ACTION_OPEN_BROWSER: &str = "/login_action open_browser";
pub(crate) const LOGIN_ACTION_COPY_URL: &str = "/login_action copy_url";
pub(crate) const LOGIN_ACTION_COPY_CODE: &str = "/login_action copy_code";
pub(crate) const LOGIN_ACTION_CANCEL: &str = "/login_action cancel";

/// Actions available in the login action menu.
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

    // ── Login helpers ─────────────────────────────────────────────────────────

    /// Build a single login-action `CompletionItem`.
    fn login_action_item(label: &str, detail: &str, token: &str) -> CompletionItem {
        CompletionItem {
            label: label.to_string(),
            detail: detail.to_string(),
            complete_to: token.to_string(),
            loading: false,
            error: false,
            match_range: None,
        }
    }

    /// Copy `text` to the clipboard using the persistent `self.clipboard`
    /// instance. Lazily initialises it on first call. Returns an error
    /// string on failure.
    pub fn clipboard_set(&mut self, text: String) -> Result<(), String> {
        // Lazily open the clipboard and keep it alive for the whole login
        // session. On Linux the clipboard is owner-based: dropping the
        // Clipboard instance clears the content for other applications.
        if self.clipboard.is_none() {
            match arboard::Clipboard::new() {
                Ok(cb) => self.clipboard = Some(cb),
                Err(e) => return Err(e.to_string()),
            }
        }
        self.clipboard
            .as_mut()
            .unwrap()
            .set_text(text)
            .map_err(|e| e.to_string())
    }

    // ── Login flow actions ────────────────────────────────────────────────────

    /// Cancel the running auth task if one is active.
    pub fn cancel_login(&mut self) {
        if let Some(cancel) = &self.cancel {
            log::debug!("login cancel requested");
            cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Spawn the auth task for `provider`.
    pub fn start_login(&mut self, provider: &str, tx: AppEventTx) {
        if self.active {
            return;
        }

        log::debug!("login start requested: provider={provider}");

        self.active = true;
        self.provider = Some(provider.to_string());
        self.info = format!("Starting login for {provider}...");
        self.url = None;
        self.code = None;
        self.auth_flow = None;

        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Some(cancel.clone());
        let provider = provider.to_string();

        tokio::spawn(async move {
            auth::login_provider(&provider, tx, cancel).await;
        });
    }

    /// Open provider picker for `/login` command.
    pub fn enter_login_selection_mode(&mut self, selection: &mut SelectionState) {
        let items = ["copilot", "codex", "gemini"]
            .iter()
            .map(|p| CompletionItem {
                label: (*p).to_string(),
                detail: String::new(),
                complete_to: format!("/login {p}"),
                loading: false,
                error: false,
                match_range: None,
            })
            .collect();
        selection.activate(SelectionKind::LoginProvider, "  Login provider  ", items);
    }

    /// Open the action selection menu for the active login panel.
    ///
    /// Items are populated based on what is currently available:
    /// - "Open browser" and "Copy URL" only when a URL has arrived
    /// - "Copy code" only when a device code is present (Copilot flow)
    /// - "Cancel" always
    pub fn enter_login_action_menu(&mut self, selection: &mut SelectionState) {
        if !self.active {
            return;
        }

        let mut items: Vec<CompletionItem> = Vec::new();
        if self.url.is_some() {
            items.push(Self::login_action_item(
                "Open browser",
                "Launch the authentication URL in your default browser",
                LOGIN_ACTION_OPEN_BROWSER,
            ));
            items.push(Self::login_action_item(
                "Copy URL",
                "Copy the authentication URL to the clipboard",
                LOGIN_ACTION_COPY_URL,
            ));
        }
        if self.code.is_some() {
            items.push(Self::login_action_item(
                "Copy code",
                "Copy the device code to the clipboard",
                LOGIN_ACTION_COPY_CODE,
            ));
        }
        items.push(Self::login_action_item(
            "Cancel",
            "Abort the login flow",
            LOGIN_ACTION_CANCEL,
        ));

        selection.activate(SelectionKind::LoginAction, "  Login actions  ", items);
    }

    /// Execute a login action chosen from the action menu.
    pub fn apply_login_action(&mut self, action: LoginActionKind, selection: &mut SelectionState) {
        // Always close the menu first so the login panel is visible behind
        // the feedback message written to login_info.
        selection.reset();

        match action {
            LoginActionKind::OpenBrowser => {
                let Some(url) = self.url.clone() else {
                    return;
                };
                match auth::open_url::open_url(&url) {
                    Ok(()) => {
                        log::debug!("login: opened browser for {url}");
                        self.info = "Browser opened.".to_string();
                    }
                    Err(e) => {
                        log::debug!("login: failed to open browser: {e}");
                        self.info = format!("Could not open browser: {e}. Copy the URL manually.");
                    }
                }
            }
            LoginActionKind::CopyUrl => {
                let Some(url) = self.url.clone() else {
                    return;
                };
                match self.clipboard_set(url) {
                    Ok(()) => {
                        log::debug!("login: copied URL to clipboard");
                        self.info = "URL copied to clipboard.".to_string();
                    }
                    Err(e) => {
                        log::debug!("login: clipboard unavailable: {e}");
                        let _ = e;
                        self.info =
                            "Clipboard unavailable — select the URL above to copy.".to_string();
                    }
                }
            }
            LoginActionKind::CopyCode => {
                let Some(code) = self.code.clone() else {
                    return;
                };
                match self.clipboard_set(code) {
                    Ok(()) => {
                        log::debug!("login: copied device code to clipboard");
                        self.info = "Code copied to clipboard.".to_string();
                    }
                    Err(e) => {
                        log::debug!("login: clipboard unavailable: {e}");
                        let _ = e;
                        self.info = "Clipboard unavailable — type the code shown above manually."
                            .to_string();
                    }
                }
            }
            LoginActionKind::Cancel => {
                self.cancel_login();
            }
        }
    }

    /// Handle a `LoginEvent` from the auth task, updating state and calling
    /// into `session` / `log_cache` for cross-struct side effects.
    pub fn apply_login_event(
        &mut self,
        ev: LoginEvent,
        session: &mut SessionManager,
        selection: &mut SelectionState,
        log_cache: &mut LogCache,
    ) {
        match ev {
            LoginEvent::Info(msg) => {
                log::debug!("login info: {msg}");
                self.info = msg;
            }
            LoginEvent::AuthCode { url, code, flow } => {
                log::debug!("login auth prompt: url={} has_code={}", url, code.is_some());
                self.url = Some(url);
                self.code = code;
                self.auth_flow = Some(flow);
                // Automatically open the action menu so the user can choose
                // how to proceed without needing to know any keyboard shortcuts.
                self.enter_login_action_menu(selection);
            }
            LoginEvent::Success { provider } => {
                log::debug!("login success: provider={provider}");
                session.live_turn.notices.push(Message::assistant(format!(
                    "[login successful: {provider}]"
                )));
                log_cache.invalidate();
                session.refresh_persistence();
                self.needs_rebuild = true;
            }
            LoginEvent::Error { provider, message } => {
                log::debug!("login error: provider={} err={}", provider, message);
                session.live_turn.notices.push(Message::assistant(format!(
                    "[login failed for {provider}: {message}]"
                )));
                log_cache.invalidate();
                session.refresh_persistence();
            }
            LoginEvent::RefreshResult {
                provider,
                success,
                message,
            } => {
                log::debug!(
                    "token refresh result: provider={} success={} msg={}",
                    provider,
                    success,
                    message
                );
                self.refresh_in_progress = false;
                if success {
                    // Silently refresh — no message added to the chat log or
                    // LLM history; the retry will continue seamlessly.
                    self.needs_rebuild = true;
                } else {
                    self.retry_after_refresh = false;
                    // Still signal a rebuild so the event loop exits and the
                    // error notice becomes visible — without this, the UI
                    // would be stuck if the refresh happened during a turn.
                    self.needs_rebuild = true;
                    session.live_turn.notices.push(Message::assistant(format!(
                        "[token refresh failed for {provider}: {message}. Run /login {provider}]"
                    )));
                    log_cache.invalidate();
                    session.refresh_persistence();
                }
            }
            LoginEvent::Finished => {
                log::debug!("login flow finished");
                self.active = false;
                self.provider = None;
                self.cancel = None;
                self.auth_flow = None;
                selection.reset();
                // Drop the clipboard instance; on Linux this releases clipboard
                // ownership so the content is no longer served by this process.
                self.clipboard = None;
            }
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

    #[test]
    fn cancel_login_sets_flag() {
        let mut s = LoginState::new();
        let flag = Arc::new(AtomicBool::new(false));
        s.cancel = Some(flag.clone());
        s.cancel_login();
        assert!(flag.load(Ordering::Relaxed));
    }

    #[test]
    fn cancel_login_no_panic_when_inactive() {
        let mut s = LoginState::new();
        s.cancel_login(); // should not panic
    }
}
