//! Append-only session event log.
//!
//! An [`EventLog`] is the in-memory representation of a session.  It is
//! backed by a JSONL file on disk where each line is one serialized
//! [`SessionEvent`].  All writes are append-only: existing lines are never
//! modified or deleted.
//!
//! # Loading
//!
//! [`EventLog::load`] reads a JSONL file line by line.  Each line is first
//! attempted as a [`SessionEvent`]; if that fails the line is retried as a
//! legacy [`Message`] and converted to a [`SessionEvent`] equivalent.  Lines
//! that cannot be parsed under either format are silently skipped with a debug
//! log.  Unknown `type` tags that deserialize successfully are preserved as
//! [`SessionEvent`] values and round-trip through save without data loss.
//!
//! This means existing session files written by older versions of tau are read
//! transparently.  On the first [`append_batch`] call after loading a legacy
//! file, the session is re-persisted in the new format as part of the normal
//! write path — no explicit migration step is required.
//!
//! [`append_batch`]: EventLog::append_batch

use std::{
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::PathBuf,
};

use anyhow::Context;

use crate::{
    llm::{AssistantPhase, Message, Role},
    session_event::SessionEvent,
};

// ── EventLog ──────────────────────────────────────────────────────────────────

/// In-memory session event log backed by an append-only JSONL file.
#[derive(Debug)]
pub struct EventLog {
    /// Path to the JSONL file on disk.
    path: PathBuf,
    /// All events loaded from (or appended to) the log, in order.
    pub events: Vec<SessionEvent>,
    /// True when the file on disk contains legacy `Message` lines that have
    /// not yet been rewritten in the new format.
    legacy: bool,
}

impl EventLog {
    /// Load an event log from a JSONL file.
    ///
    /// Each line is attempted as a [`SessionEvent`] first; if that fails it is
    /// retried as a legacy [`Message`] and converted.  Lines that cannot be
    /// parsed under either format are skipped with a debug log.
    ///
    /// If the file does not exist an empty log is returned (the file will be
    /// created on the first [`append_batch`] call).
    ///
    /// [`append_batch`]: EventLog::append_batch
    pub fn load(path: impl Into<PathBuf>) -> anyhow::Result<Self> {
        let path = path.into();

        if !path.exists() {
            return Ok(Self {
                path,
                events: Vec::new(),
                legacy: false,
            });
        }

        let file = File::open(&path)
            .with_context(|| format!("Failed to open event log: {}", path.display()))?;
        let reader = BufReader::new(file);

        let mut events = Vec::new();
        let mut legacy = false;

        for line in reader.lines() {
            let line =
                line.with_context(|| format!("Failed to read event log line: {}", path.display()))?;
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            // Try new format first.
            if let Ok(ev) = serde_json::from_str::<SessionEvent>(line) {
                events.push(ev);
                continue;
            }

            // Fall back to legacy Message format.
            match serde_json::from_str::<Message>(line) {
                Ok(msg) => {
                    legacy = true;
                    if let Some(ev) = message_to_event(&msg) {
                        events.push(ev);
                    }
                }
                Err(e) => {
                    log::debug!(
                        "skipping unparseable event log line in {}: {e}",
                        path.display()
                    );
                }
            }
        }

        Ok(Self {
            path,
            events,
            legacy,
        })
    }

    /// Append a batch of events to the log.
    ///
    /// When [`legacy`] is true (the file was loaded from a legacy format),
    /// the entire log is rewritten in the new format before appending.  After
    /// that write the log is no longer considered legacy.
    ///
    /// All events in `batch` are serialized and written as a single contiguous
    /// block.  The write uses a single [`write_all`] call to minimize the
    /// window for partial writes.
    ///
    /// [`legacy`]: EventLog::legacy
    /// [`write_all`]: std::io::Write::write_all
    pub fn append_batch(&mut self, batch: &[SessionEvent]) -> anyhow::Result<()> {
        if batch.is_empty() {
            return Ok(());
        }

        if self.legacy {
            self.rewrite_as_new_format().with_context(|| {
                format!("Failed to rewrite legacy log: {}", self.path.display())
            })?;
            self.legacy = false;
        }

        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create event log directory: {}", parent.display())
            })?;
        }

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .with_context(|| {
                format!(
                    "Failed to open event log for append: {}",
                    self.path.display()
                )
            })?;

        let mut buf = String::new();
        for ev in batch {
            buf.push_str(
                &serde_json::to_string(ev).with_context(|| "Failed to serialize session event")?,
            );
            buf.push('\n');
        }

        file.write_all(buf.as_bytes())
            .with_context(|| format!("Failed to append to event log: {}", self.path.display()))?;

        self.events.extend_from_slice(batch);
        Ok(())
    }

    /// True when the on-disk file contains legacy-format lines that will be
    /// rewritten on the next [`append_batch`] call.
    ///
    /// [`append_batch`]: EventLog::append_batch
    #[allow(dead_code)]
    pub fn is_legacy(&self) -> bool {
        self.legacy
    }

    /// Rewrite the entire in-memory event list to the file in the new format.
    /// Called automatically by [`append_batch`] when [`legacy`] is true.
    ///
    /// [`append_batch`]: EventLog::append_batch
    /// [`legacy`]: EventLog::legacy
    fn rewrite_as_new_format(&self) -> anyhow::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create event log directory: {}", parent.display())
            })?;
        }

        let mut buf = String::new();
        for ev in &self.events {
            buf.push_str(
                &serde_json::to_string(ev)
                    .with_context(|| "Failed to serialize session event during rewrite")?,
            );
            buf.push('\n');
        }

        std::fs::write(&self.path, buf.as_bytes())
            .with_context(|| format!("Failed to rewrite event log: {}", self.path.display()))?;

        Ok(())
    }
}

// ── Legacy conversion ─────────────────────────────────────────────────────────

/// Convert a legacy [`Message`] to the equivalent [`SessionEvent`].
///
/// Returns `None` for message roles that do not map to a durable event
/// (e.g. [`Role::System`], which is injected at projection time and is not
/// stored in the event log).
///
/// Uses the current wall-clock time as the timestamp for legacy messages,
/// since the old format did not record timestamps.
fn message_to_event(msg: &Message) -> Option<SessionEvent> {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    match msg.role {
        Role::User => Some(SessionEvent::UserMessage {
            content: msg.content.clone(),
            timestamp,
        }),

        Role::Assistant => {
            // Error messages surfaced by the old code as synthetic assistant
            // content become TurnError events.
            if msg.content.starts_with("[Error:") || msg.content.starts_with("[token refresh") {
                Some(SessionEvent::TurnError {
                    message: msg.content.clone(),
                    timestamp,
                })
            } else {
                Some(SessionEvent::AssistantMessage {
                    content: msg.content.clone(),
                    thinking: msg.thinking.clone(),
                    phase: msg.assistant_phase.unwrap_or(AssistantPhase::Final),
                    usage: None,
                    timestamp,
                })
            }
        }

        Role::ToolCall => Some(SessionEvent::ToolCall {
            id: msg
                .tool_call_id
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
            name: msg
                .tool_name
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
            args: msg.tool_args.clone().unwrap_or(serde_json::Value::Null),
            timestamp,
        }),

        Role::ToolResult => Some(SessionEvent::ToolResult {
            id: msg
                .tool_call_id
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
            // Tool name is not stored on the result in the legacy format;
            // leave it empty — the projection can tolerate this.
            name: String::new(),
            content: msg.content.clone(),
            is_error: msg.is_error,
            display_range: msg.display_range.clone(),
            timestamp,
        }),

        // System messages are injected at projection time; not stored in log.
        Role::System => None,
    }
}

// ── SessionStore extension ────────────────────────────────────────────────────

/// Extension methods on [`crate::session::SessionStore`] for event-log I/O.
///
/// These sit alongside the existing `load_messages` / `save_messages` API
/// and will replace it once all consumers are migrated (phase 1, step 5).
impl crate::session::SessionStore {
    /// Load the event log for `session_id`.
    ///
    /// Returns an empty [`EventLog`] (pointing at the correct path) if no
    /// file exists yet.
    pub fn load_events(&self, session_id: &str) -> anyhow::Result<EventLog> {
        let path = self.event_log_path(session_id)?;
        EventLog::load(path)
    }

    /// Append a batch of events to the event log for `session_id`.
    ///
    /// Creates the file if it does not exist.  If the session file currently
    /// contains legacy [`Message`] lines, the file is transparently rewritten
    /// in the new format before appending.
    #[allow(dead_code)]
    pub fn append_events(&self, session_id: &str, batch: &[SessionEvent]) -> anyhow::Result<()> {
        let path = self.event_log_path(session_id)?;
        let mut log = EventLog::load(&path)?;
        log.append_batch(batch)
    }

    /// Resolve the JSONL file path for a session's event log.
    ///
    /// Delegates to the existing [`SessionStore`] path resolution so that
    /// event logs live alongside (and eventually replace) the legacy session
    /// files.
    fn event_log_path(&self, session_id: &str) -> anyhow::Result<PathBuf> {
        // `find_session_file_by_id` is private; we replicate its lookup via
        // the public `load_messages` path for now.  Once the migration is
        // complete this will be the only path and can be simplified.
        //
        // For new sessions that have no file yet, we still need a path to
        // write to.  We derive one from the session id by asking for the
        // messages of an unknown session (which returns an empty vec and gives
        // us nothing), so we fall back to re-deriving the path from the
        // session store directory.
        self.resolve_event_log_path(session_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        llm::{AssistantPhase, UsageStats},
        session_event::SessionEvent,
        thinking::ThinkingLevel,
    };
    use tempfile::tempdir;

    fn ts() -> u64 {
        1_713_000_000
    }

    fn user_ev() -> SessionEvent {
        SessionEvent::UserMessage {
            content: "hello".to_string(),
            timestamp: ts(),
        }
    }

    fn assistant_ev() -> SessionEvent {
        SessionEvent::AssistantMessage {
            content: "hi".to_string(),
            thinking: None,
            phase: AssistantPhase::Final,
            usage: Some(UsageStats {
                input_tokens: Some(10),
                output_tokens: Some(5),
                total_tokens: Some(15),
            }),
            timestamp: ts(),
        }
    }

    fn tool_call_ev() -> SessionEvent {
        SessionEvent::ToolCall {
            id: "call_1".to_string(),
            name: "read_file".to_string(),
            args: serde_json::json!({"path": "src/main.rs"}),
            timestamp: ts(),
        }
    }

    fn tool_result_ev() -> SessionEvent {
        SessionEvent::ToolResult {
            id: "call_1".to_string(),
            name: "read_file".to_string(),
            content: "fn main() {}".to_string(),
            is_error: false,
            display_range: None,
            timestamp: ts(),
        }
    }

    // ── load / append round-trip ──────────────────────────────────────────────

    #[test]
    fn load_returns_empty_for_nonexistent_file() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("session.jsonl");
        let log = EventLog::load(&path).unwrap();
        assert!(log.events.is_empty());
        assert!(!log.is_legacy());
    }

    #[test]
    fn append_batch_creates_file_and_persists_events() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("session.jsonl");
        let mut log = EventLog::load(&path).unwrap();

        log.append_batch(&[user_ev(), assistant_ev()]).unwrap();

        assert_eq!(log.events.len(), 2);
        assert!(path.exists());

        // Reload and verify.
        let reloaded = EventLog::load(&path).unwrap();
        assert_eq!(reloaded.events.len(), 2);
        assert!(matches!(
            reloaded.events[0],
            SessionEvent::UserMessage { .. }
        ));
        assert!(matches!(
            reloaded.events[1],
            SessionEvent::AssistantMessage { .. }
        ));
    }

    #[test]
    fn append_batch_is_incremental() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("session.jsonl");
        let mut log = EventLog::load(&path).unwrap();

        log.append_batch(&[user_ev()]).unwrap();
        log.append_batch(&[assistant_ev()]).unwrap();

        // Two separate appends — reload should see both.
        let reloaded = EventLog::load(&path).unwrap();
        assert_eq!(reloaded.events.len(), 2);
    }

    #[test]
    fn append_batch_preserves_existing_events_across_reloads() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("session.jsonl");

        // First session: write two events.
        let mut log = EventLog::load(&path).unwrap();
        log.append_batch(&[user_ev(), assistant_ev()]).unwrap();
        drop(log);

        // Second session: load and append more.
        let mut log2 = EventLog::load(&path).unwrap();
        log2.append_batch(&[tool_call_ev(), tool_result_ev()])
            .unwrap();

        let reloaded = EventLog::load(&path).unwrap();
        assert_eq!(reloaded.events.len(), 4);
    }

    #[test]
    fn empty_batch_is_a_noop() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("session.jsonl");
        let mut log = EventLog::load(&path).unwrap();
        log.append_batch(&[]).unwrap();
        assert!(!path.exists(), "empty batch should not create file");
    }

    // ── legacy read support ───────────────────────────────────────────────────

    #[test]
    fn loads_legacy_user_message() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("session.jsonl");
        let msg = Message::user("legacy hello");
        std::fs::write(&path, format!("{}\n", serde_json::to_string(&msg).unwrap())).unwrap();

        let log = EventLog::load(&path).unwrap();
        assert!(log.is_legacy());
        assert_eq!(log.events.len(), 1);
        assert!(
            matches!(&log.events[0], SessionEvent::UserMessage { content, .. } if content == "legacy hello")
        );
    }

    #[test]
    fn loads_legacy_assistant_message() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("session.jsonl");
        let msg = Message::assistant("legacy reply");
        std::fs::write(&path, format!("{}\n", serde_json::to_string(&msg).unwrap())).unwrap();

        let log = EventLog::load(&path).unwrap();
        assert!(log.is_legacy());
        assert!(
            matches!(&log.events[0], SessionEvent::AssistantMessage { content, .. } if content == "legacy reply")
        );
    }

    #[test]
    fn loads_legacy_tool_call_and_result() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("session.jsonl");
        let tc = Message::tool_call("c1", "bash", serde_json::json!({"command": "ls"}));
        let tr = Message::tool_result("c1", "file.txt", false);
        std::fs::write(
            &path,
            format!(
                "{}\n{}\n",
                serde_json::to_string(&tc).unwrap(),
                serde_json::to_string(&tr).unwrap(),
            ),
        )
        .unwrap();

        let log = EventLog::load(&path).unwrap();
        assert!(log.is_legacy());
        assert_eq!(log.events.len(), 2);
        assert!(matches!(&log.events[0], SessionEvent::ToolCall { name, .. } if name == "bash"));
        assert!(
            matches!(&log.events[1], SessionEvent::ToolResult { content, .. } if content == "file.txt")
        );
    }

    #[test]
    fn legacy_system_messages_are_skipped() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("session.jsonl");
        let sys = Message::system("you are helpful");
        let usr = Message::user("hi");
        std::fs::write(
            &path,
            format!(
                "{}\n{}\n",
                serde_json::to_string(&sys).unwrap(),
                serde_json::to_string(&usr).unwrap(),
            ),
        )
        .unwrap();

        let log = EventLog::load(&path).unwrap();
        // System message skipped; only user message survives.
        assert_eq!(log.events.len(), 1);
        assert!(matches!(&log.events[0], SessionEvent::UserMessage { .. }));
    }

    #[test]
    fn legacy_error_assistant_message_becomes_turn_error() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("session.jsonl");
        let msg = Message::assistant("[Error: rate limit exceeded]");
        std::fs::write(&path, format!("{}\n", serde_json::to_string(&msg).unwrap())).unwrap();

        let log = EventLog::load(&path).unwrap();
        assert!(
            matches!(&log.events[0], SessionEvent::TurnError { message, .. } if message.contains("rate limit"))
        );
    }

    #[test]
    fn first_append_after_legacy_load_rewrites_file() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("session.jsonl");

        // Write a legacy file.
        let msg = Message::user("legacy");
        std::fs::write(&path, format!("{}\n", serde_json::to_string(&msg).unwrap())).unwrap();

        // Load and append one new event.
        let mut log = EventLog::load(&path).unwrap();
        assert!(log.is_legacy());
        log.append_batch(&[assistant_ev()]).unwrap();
        assert!(!log.is_legacy());

        // Reload — should now be entirely new format (2 events, no legacy flag).
        let reloaded = EventLog::load(&path).unwrap();
        assert!(!reloaded.is_legacy());
        assert_eq!(reloaded.events.len(), 2);
    }

    #[test]
    fn skips_unparseable_lines_silently() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("session.jsonl");
        // A valid new-format event, a garbage line, and another valid event.
        std::fs::write(
            &path,
            format!(
                "{}\nnot valid json at all\n{}\n",
                serde_json::to_string(&user_ev()).unwrap(),
                serde_json::to_string(&assistant_ev()).unwrap(),
            ),
        )
        .unwrap();

        let log = EventLog::load(&path).unwrap();
        assert_eq!(log.events.len(), 2);
    }

    // ── ThinkingLevelChanged round-trip ───────────────────────────────────────

    #[test]
    fn thinking_level_changed_round_trips_through_file() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("session.jsonl");
        let mut log = EventLog::load(&path).unwrap();
        log.append_batch(&[SessionEvent::ThinkingLevelChanged {
            level: ThinkingLevel::Medium,
            timestamp: ts(),
        }])
        .unwrap();

        let reloaded = EventLog::load(&path).unwrap();
        assert!(
            matches!(&reloaded.events[0], SessionEvent::ThinkingLevelChanged { level, .. } if *level == ThinkingLevel::Medium)
        );
    }

    // ── ModelChanged round-trip ───────────────────────────────────────────────

    #[test]
    fn model_changed_round_trips_through_file() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("session.jsonl");
        let mut log = EventLog::load(&path).unwrap();
        log.append_batch(&[SessionEvent::ModelChanged {
            model: "gpt-4o".to_string(),
            provider: "openai".to_string(),
            timestamp: ts(),
        }])
        .unwrap();

        let reloaded = EventLog::load(&path).unwrap();
        assert!(
            matches!(&reloaded.events[0], SessionEvent::ModelChanged { model, .. } if model == "gpt-4o")
        );
    }
}
