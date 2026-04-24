//! Session state management.
//!
//! `SessionManager` groups the fields that track session persistence,
//! the committed session state, the in-flight live turn overlay, and
//! the pending turn event buffer.
//!
//! The session-related *methods* remain on `App` because they interact
//! with the textarea, log revision, and other App-level concerns.
//! `SessionManager` is a pure data holder.

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
}
