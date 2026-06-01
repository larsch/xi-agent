//! Append-only session event log.
//!
//! An [`EventLog`] is the in-memory representation of a session.  It is
//! backed by a JSONL file on disk where each line is one serialized
//! [`SessionEvent`].  All writes are append-only: existing lines are never
//! modified or deleted.
//!
//! # Loading
//!
//! [`EventLog::load`] reads a JSONL file line by line.  Each line is
//! deserialized as a [`SessionEvent`].  Lines that cannot be parsed are
//! silently skipped with a debug log.  Unknown `type` tags that deserialize
//! successfully are preserved as [`SessionEvent`] values and round-trip
//! through save without data loss.

use std::{
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::PathBuf,
};

use anyhow::Context;

use crate::session_event::SessionEvent;

// ── EventLog ──────────────────────────────────────────────────────────────────

/// In-memory session event log backed by an append-only JSONL file.
#[derive(Debug)]
pub struct EventLog {
    /// Path to the JSONL file on disk.
    path: PathBuf,
    /// All events loaded from (or appended to) the log, in order.
    pub events: Vec<SessionEvent>,
}

impl EventLog {
    /// Load an event log from a JSONL file.
    ///
    /// Each line is deserialized as a [`SessionEvent`].  Lines that cannot be
    /// parsed are skipped with a debug log.
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
            });
        }

        let file = File::open(&path)
            .with_context(|| format!("Failed to open event log: {}", path.display()))?;
        let reader = BufReader::new(file);

        let mut events = Vec::new();

        for line in reader.lines() {
            let line =
                line.with_context(|| format!("Failed to read event log line: {}", path.display()))?;
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            match serde_json::from_str::<SessionEvent>(line) {
                Ok(ev) => events.push(ev),
                Err(e) => {
                    log::debug!(
                        "skipping unparseable event log line in {}: {e}",
                        path.display()
                    );
                }
            }
        }

        Ok(Self { path, events })
    }

    /// Append a batch of events to the log.
    ///
    /// All events in `batch` are serialized and written as a single contiguous
    /// block.  The write uses a single [`write_all`] call to minimize the
    /// window for partial writes.
    ///
    /// [`write_all`]: std::io::Write::write_all
    pub fn append_batch(&mut self, batch: &[SessionEvent]) -> anyhow::Result<()> {
        if batch.is_empty() {
            return Ok(());
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

    /// Create a new event log at `path` pre-populated with `events`.
    ///
    /// The file is created (or overwritten) and all events are written
    /// immediately.  Returns the new [`EventLog`] with those events loaded.
    pub fn new_from_events(
        path: impl Into<PathBuf>,
        events: &[SessionEvent],
    ) -> anyhow::Result<Self> {
        let path = path.into();

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create event log directory: {}", parent.display())
            })?;
        }

        let mut buf = String::new();
        for ev in events {
            buf.push_str(
                &serde_json::to_string(ev).with_context(|| "Failed to serialize session event")?,
            );
            buf.push('\n');
        }

        std::fs::write(&path, buf.as_bytes())
            .with_context(|| format!("Failed to write event log: {}", path.display()))?;

        Ok(Self {
            path,
            events: events.to_vec(),
        })
    }
}

// ── SessionStore extension ────────────────────────────────────────────────────

/// Extension methods on [`crate::session::SessionStore`] for event-log I/O.
impl crate::session::SessionStore {
    /// Load the event log for `session_id`.
    ///
    /// Returns an empty [`EventLog`] (pointing at the correct path) if no
    /// file exists yet.
    pub fn load_events(&self, session_id: &str) -> anyhow::Result<EventLog> {
        let path = self.resolve_event_log_path(session_id)?;
        EventLog::load(path)
    }

    /// Create a new session pre-populated with `events` and return its ID.
    ///
    /// A fresh session entry is registered, then the event log file is written
    /// atomically with all provided events.
    pub fn create_session_from_events(
        &mut self,
        cwd: &str,
        events: &[SessionEvent],
    ) -> anyhow::Result<String> {
        let session_id = self.create_session(cwd)?;
        let path = self.resolve_event_log_path(&session_id)?;
        EventLog::new_from_events(path, events)?;
        Ok(session_id)
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
                cached_tokens: None,
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

    #[test]
    fn load_returns_empty_for_nonexistent_file() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("session.jsonl");
        let log = EventLog::load(&path).unwrap();
        assert!(log.events.is_empty());
    }

    #[test]
    fn append_batch_creates_file_and_persists_events() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("session.jsonl");
        let mut log = EventLog::load(&path).unwrap();

        log.append_batch(&[user_ev(), assistant_ev()]).unwrap();

        assert_eq!(log.events.len(), 2);
        assert!(path.exists());

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

        let reloaded = EventLog::load(&path).unwrap();
        assert_eq!(reloaded.events.len(), 2);
    }

    #[test]
    fn append_batch_preserves_existing_events_across_reloads() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("session.jsonl");

        let mut log = EventLog::load(&path).unwrap();
        log.append_batch(&[user_ev(), assistant_ev()]).unwrap();
        drop(log);

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

    #[test]
    fn skips_unparseable_lines_silently() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("session.jsonl");
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

    #[test]
    fn new_from_events_writes_and_loads_correctly() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("branch.jsonl");
        let events = vec![user_ev(), assistant_ev()];
        let log = EventLog::new_from_events(&path, &events).unwrap();
        assert_eq!(log.events.len(), 2);

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
    fn new_from_events_overwrites_existing_file() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("branch.jsonl");

        // Write 3 events first
        EventLog::new_from_events(&path, &[user_ev(), assistant_ev(), user_ev()]).unwrap();

        // Overwrite with 1 event
        let log = EventLog::new_from_events(&path, &[user_ev()]).unwrap();
        assert_eq!(log.events.len(), 1);

        let reloaded = EventLog::load(&path).unwrap();
        assert_eq!(reloaded.events.len(), 1);
    }

    #[test]
    fn create_session_from_events_creates_new_session() {
        let tmp = tempdir().unwrap();
        let mut store = crate::session::SessionStore::open_at(tmp.path().to_path_buf()).unwrap();
        let events = vec![user_ev(), assistant_ev()];
        let session_id = store.create_session_from_events("/repo", &events).unwrap();
        assert!(!session_id.is_empty());

        // The new session's event log contains the events
        let reloaded = store.load_events(&session_id).unwrap();
        assert_eq!(reloaded.events.len(), 2);
        assert!(matches!(
            reloaded.events[0],
            SessionEvent::UserMessage { .. }
        ));
    }
}
