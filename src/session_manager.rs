//! Session state management.
//!
//! `SessionManager` groups the fields that track session persistence,
//! the committed session state, the in-flight live turn overlay, and
//! the pending turn event buffer.
//!
//! Pure session methods live here; methods that also need `log_cache` or
//! `textarea` remain on `App` as thin wrappers.

use crate::live_turn::LiveTurnState;
use crate::session::SessionStore;
use crate::session_event::SessionEvent;
use crate::session_state::SessionState;

/// All session-related state owned by the application.
pub(crate) struct SessionManager {
    /// Open session store (file-backed). `None` if persistence is unavailable.
    pub session_store: Option<SessionStore>,

    /// ID of the currently active session. `None` until the first session is
    /// created or resumed.
    pub current_session_id: Option<String>,

    /// Working directory this session is associated with.
    pub current_cwd: String,

    /// Whether a resumable session exists for the current working directory.
    pub resume_available_for_cwd: bool,

    /// Committed session state: durable event log plus derived read models.
    /// `None` until the first session is created or resumed.
    pub session_state: Option<SessionState>,

    /// Transient in-flight state for the current (or most recently flushed)
    /// agent turn. Streaming assistant text, tool call/result pairs, and
    /// UI-only notices all live here until committed or cleared.
    pub live_turn: LiveTurnState,

    /// Buffer of events accumulated during the current in-flight turn.
    /// Flushed to session state as a batch on `TurnEnd`, `Done`, or `Error`.
    pub pending_turn_events: Vec<SessionEvent>,

    /// Optional manual compaction instructions for the next launched
    /// compaction-only task.
    pub pending_manual_compaction_instructions: Option<String>,
}

impl SessionManager {
    pub(crate) fn new() -> Self {
        Self {
            session_store: None,
            current_session_id: None,
            current_cwd: String::new(),
            resume_available_for_cwd: false,
            session_state: None,
            live_turn: LiveTurnState::new(),
            pending_turn_events: Vec::new(),
            pending_manual_compaction_instructions: None,
        }
    }

    /// Update `resume_available_for_cwd` from the session store.
    pub fn refresh_resume_availability(&mut self) {
        self.resume_available_for_cwd = self
            .session_store
            .as_ref()
            .and_then(|s| s.latest_for_cwd(&self.current_cwd))
            .is_some();
    }

    /// Return the current session ID, creating a new session if needed.
    /// Falls back to `"unknown"` if persistence is unavailable.
    pub fn ensure_session_id(&mut self) -> String {
        if let Some(ref id) = self.current_session_id {
            return id.clone();
        }
        if let Some(ref mut store) = self.session_store {
            match store.create_session(&self.current_cwd) {
                Ok(id) => {
                    self.current_session_id = Some(id.clone());
                    return id;
                }
                Err(e) => {
                    log::debug!("failed to create session for tool output log: {e}");
                }
            }
        }
        "unknown".to_string()
    }

    /// Ensure a `SessionState` exists for the current session before submitting
    /// a user message.  Creates the session and loads (or initialises) the
    /// state if needed.  No-op when session state is already populated.
    ///
    /// Falls back to an ephemeral event log in the system temp directory when
    /// persistent session storage is unavailable.
    pub fn ensure_event_log_for_submit(&mut self) {
        if self.session_state.is_some() {
            return;
        }
        let session_id = self.ensure_session_id();
        if let Some(store) = &self.session_store {
            match store.load_events(&session_id) {
                Ok(log) => {
                    self.session_state = Some(SessionState::from_event_log(log));
                    return;
                }
                Err(e) => {
                    log::debug!("failed to load event log for session {session_id}: {e}");
                }
            }
        }

        // Persistence unavailable: create an ephemeral event log.
        let path = std::env::temp_dir().join(format!("xi-ephemeral-session-{session_id}.jsonl"));
        match crate::event_log::EventLog::load(&path) {
            Ok(log) => {
                self.session_state = Some(SessionState::from_event_log(log));
            }
            Err(e) => {
                log::debug!("failed to create ephemeral event log for session {session_id}: {e}");
            }
        }
    }

    /// Refresh the resume hint.  Call this after any persistent session
    /// mutation (replaces the old `persist_messages` call sites).
    pub fn refresh_persistence(&mut self) {
        self.refresh_resume_availability();
    }

    /// Append a user-visible user message to the active session.
    pub fn append_user_message(&mut self, content: String, timestamp: u64) {
        self.ensure_event_log_for_submit();
        assert!(
            self.session_state.is_some(),
            "append_user_message called before session_state was initialised"
        );
        self.append_event_immediate(SessionEvent::UserMessage { content, timestamp });
    }

    /// Flush all buffered pending turn events to the session state.
    pub fn flush_turn_events(&mut self) {
        if self.pending_turn_events.is_empty() {
            return;
        }
        let batch: Vec<SessionEvent> = std::mem::take(&mut self.pending_turn_events);
        if let Some(ss) = self.session_state.as_mut() {
            if let Err(e) = ss.append_batch(&batch) {
                log::debug!("failed to append turn events to session state: {e}");
            }
            // Clear the in-flight turn fields — they are now represented in
            // committed display state.  Notices are preserved.
            self.live_turn.clear_turn();
        }
    }

    /// Append a single event to the event log immediately.
    pub fn append_event_immediate(&mut self, ev: SessionEvent) {
        if let Some(ss) = self.session_state.as_mut()
            && let Err(e) = ss.append_immediate(ev)
        {
            log::debug!("failed to append event to session state: {e}");
        }
    }
}
