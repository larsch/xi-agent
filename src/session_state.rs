//! Committed session state: durable event log plus incrementally-maintained
//! read models.
//!
//! # Ownership boundaries
//!
//! [`SessionState`] owns:
//! - the durable [`EventLog`] (source of truth for committed conversation history)
//! - the committed display read model ([`DisplayProjection`])
//! - the committed LLM read model ([`LlmProjection`])
//!
//! ## What belongs here
//! - Committed conversation events (user messages, assistant messages, tool
//!   calls/results, errors, compaction summaries, model/thinking-level changes)
//! - Incremental read-model updates driven by those events
//! - Full rebuilds on load and compaction reset paths
//!
//! ## What does NOT belong here
//! - Transient in-flight streaming state (lives in `LiveTurnState`)
//! - UI-only notices (live in `LiveTurnState::notices`)
//! - Local shell execution output (live in `LiveTurnState`)
//!
//! ## `DisplayProjection::rebuild` access
//!
//! `DisplayProjection::rebuild` is `pub(crate)` and must only be called from
//! within this module (load and compaction paths). Normal appends must go
//! through the incremental `apply_new_events` path.

use crate::{
    event_log::EventLog,
    llm::Message,
    projection::{DisplayProjection, LlmProjection, project_display_messages},
    session_event::SessionEvent,
};

/// Committed session history plus derived read models.
///
/// This is the single authoritative owner of the durable event log and the
/// read models derived from it. All updates to committed conversation state
/// must go through this type.
#[derive(Debug)]
pub struct SessionState {
    event_log: EventLog,
    display: DisplayProjection,
    llm: LlmProjection,
}

impl SessionState {
    /// Build session state from an already-loaded event log.
    ///
    /// Performs a full rebuild of both read models from the durable events.
    /// This is the correct entry point for load and resume paths.
    pub fn from_event_log(event_log: EventLog) -> Self {
        let mut display = DisplayProjection::new();
        display.rebuild(&event_log.events);
        let mut llm = LlmProjection::new();
        llm.apply_new_events(&event_log.events);
        Self {
            event_log,
            display,
            llm,
        }
    }

    /// Return the durable event history.
    pub fn events(&self) -> &[SessionEvent] {
        &self.event_log.events
    }

    /// Append one complete, self-contained event (e.g. `UserMessage`,
    /// `ModelChanged`) and update read models incrementally.
    pub fn append_immediate(&mut self, ev: SessionEvent) -> anyhow::Result<()> {
        self.event_log.append_batch(std::slice::from_ref(&ev))?;
        self.display.apply_new_events(&self.event_log.events);
        self.llm.apply_new_events(&self.event_log.events);
        Ok(())
    }

    /// Append a completed turn batch (assistant message + tool call/result
    /// pairs) and update read models.
    ///
    /// The display projection is rebuilt here because the batch corresponds to
    /// in-flight state that was already shown transiently via `LiveTurnState`.
    /// Rebuilding ensures the transient entries are replaced by a single
    /// projected copy rather than appearing twice.
    pub fn append_batch(&mut self, batch: &[SessionEvent]) -> anyhow::Result<()> {
        self.event_log.append_batch(batch)?;
        // Rebuild display to reconcile any transient in-flight UI messages
        // that were shown before the turn completed.
        self.display.rebuild(&self.event_log.events);
        self.llm.apply_new_events(&self.event_log.events);
        Ok(())
    }

    /// Current committed display messages.
    pub fn display_messages(&self) -> &[Message] {
        self.display.messages()
    }

    /// Mutable access to committed display messages.
    ///
    /// Restricted to tests and the session_state module. External code must
    /// not mutate committed display state directly — use event ingestion
    /// methods instead.
    #[cfg(test)]
    pub(crate) fn display_messages_mut(&mut self) -> &mut Vec<Message> {
        self.display.messages_mut()
    }

    /// Whether the committed display projection has no messages.
    pub fn display_is_empty(&self) -> bool {
        self.display.is_empty()
    }

    /// Number of committed display messages.
    pub fn display_len(&self) -> usize {
        self.display.len()
    }

    /// Export-friendly display projection built directly from durable events.
    ///
    /// Unlike `display_messages()`, this does not include any transient
    /// in-flight mutations that may have been applied via `display_messages_mut`.
    pub fn projected_display_messages(&self) -> Vec<Message> {
        project_display_messages(&self.event_log.events)
    }

    /// Current LLM-visible message list.
    ///
    /// Used in tests to verify the LLM projection without going through
    /// the agent loop.
    #[cfg(test)]
    pub fn llm_messages(&mut self) -> &[Message] {
        self.llm.ensure_current(&self.event_log.events);
        self.llm.messages()
    }

    /// Remove the trailing `TurnError` event if the last event is one.
    /// Used by `/retry` to clean up before re-launching a turn.
    pub(crate) fn pop_trailing_turn_error(&mut self) {
        if let Some(last) = self.event_log.events.last()
            && matches!(last, SessionEvent::TurnError { .. })
        {
            self.event_log.events.pop();
            self.display.rebuild(&self.event_log.events);
            self.llm.apply_new_events(&self.event_log.events);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        event_log::EventLog,
        llm::{Message, Role},
        session_event::CompactionTrigger,
    };

    fn ts() -> u64 {
        1_713_000_000
    }

    fn user_ev(content: &str) -> SessionEvent {
        SessionEvent::UserMessage {
            content: content.to_string(),
            timestamp: ts(),
        }
    }

    fn assistant_ev(content: &str) -> SessionEvent {
        SessionEvent::AssistantMessage {
            content: content.to_string(),
            thinking: None,
            phase: crate::llm::AssistantPhase::Final,
            usage: None,
            timestamp: ts(),
        }
    }

    #[test]
    fn session_state_builds_both_projections_from_event_log() {
        let path =
            std::env::temp_dir().join(format!("xi-session-state-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&path);

        let mut state = SessionState::from_event_log(EventLog::load(&path).unwrap());
        state.append_immediate(user_ev("hello")).unwrap();
        state.append_immediate(assistant_ev("hi")).unwrap();

        assert_eq!(state.display_messages().len(), 2);
        assert_eq!(state.llm_messages().len(), 2);
        assert_eq!(state.llm_messages()[0].role, Role::User);
    }

    #[test]
    fn session_state_append_batch_reconciles_transient_display_state() {
        let path = std::env::temp_dir().join(format!(
            "xi-session-state-batch-reconcile-{}.jsonl",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);

        let mut state = SessionState::from_event_log(EventLog::load(&path).unwrap());
        state.append_immediate(user_ev("hello")).unwrap();

        // Simulate transient in-flight UI state before the completed turn is flushed.
        state.display_messages_mut().push(Message::assistant("hi"));

        state.append_batch(&[assistant_ev("hi")]).unwrap();

        let assistant_count = state
            .display_messages()
            .iter()
            .filter(|m| m.role == Role::Assistant && m.content == "hi")
            .count();
        assert_eq!(assistant_count, 1);
    }

    #[test]
    fn session_state_compaction_invalidates_llm_projection() {
        let path = std::env::temp_dir().join(format!(
            "xi-session-state-compaction-{}.jsonl",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);

        let mut state = SessionState::from_event_log(EventLog::load(&path).unwrap());
        state.append_immediate(user_ev("old")).unwrap();
        state.append_immediate(assistant_ev("reply")).unwrap();
        state
            .append_immediate(SessionEvent::CompactionSummary {
                summary: "summary".to_string(),
                trigger_reason: CompactionTrigger::Threshold,
                context_window: 200_000,
                reserve_tokens: 16_000,
                keep_recent_tokens: 20_000,
                tokens_before: 10,
                tokens_after: 5,
                retained_event_count: None,
                read_files: vec![],
                modified_files: vec![],
                timestamp: ts(),
            })
            .unwrap();
        state.append_immediate(user_ev("new")).unwrap();

        let msgs = state.llm_messages();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[1].content, "new");
    }

    // ── Step 2: event-driven projection update tests ──────────────────────────

    /// `append_immediate` must update display incrementally (no rebuild needed)
    /// for ordinary conversation events.
    #[test]
    fn append_immediate_updates_display_incrementally() {
        let path =
            std::env::temp_dir().join(format!("xi-ss-incr-display-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&path);

        let mut state = SessionState::from_event_log(EventLog::load(&path).unwrap());

        state.append_immediate(user_ev("a")).unwrap();
        assert_eq!(state.display_messages().len(), 1);

        state.append_immediate(assistant_ev("b")).unwrap();
        assert_eq!(state.display_messages().len(), 2);

        state.append_immediate(user_ev("c")).unwrap();
        assert_eq!(state.display_messages().len(), 3);

        // All messages are in order.
        assert_eq!(state.display_messages()[0].content, "a");
        assert_eq!(state.display_messages()[1].content, "b");
        assert_eq!(state.display_messages()[2].content, "c");
    }

    /// `append_immediate` must update the LLM projection incrementally for
    /// ordinary conversation events.
    #[test]
    fn append_immediate_updates_llm_incrementally() {
        let path =
            std::env::temp_dir().join(format!("xi-ss-incr-llm-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&path);

        let mut state = SessionState::from_event_log(EventLog::load(&path).unwrap());

        state.append_immediate(user_ev("q1")).unwrap();
        assert_eq!(state.llm_messages().len(), 1);

        state.append_immediate(assistant_ev("a1")).unwrap();
        assert_eq!(state.llm_messages().len(), 2);

        // Messages are in order.
        assert_eq!(state.llm_messages()[0].content, "q1");
        assert_eq!(state.llm_messages()[1].content, "a1");
    }

    /// A `CompactionSummary` appended via `append_immediate` must cause the
    /// LLM projection to reflect only the post-compaction tail (the compaction
    /// boundary is respected).
    #[test]
    fn compaction_via_append_immediate_updates_llm_projection() {
        let path =
            std::env::temp_dir().join(format!("xi-ss-compact-llm-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&path);

        let mut state = SessionState::from_event_log(EventLog::load(&path).unwrap());
        state.append_immediate(user_ev("old")).unwrap();
        state.append_immediate(assistant_ev("reply")).unwrap();
        state
            .append_immediate(SessionEvent::CompactionSummary {
                summary: "ctx".to_string(),
                trigger_reason: CompactionTrigger::Threshold,
                context_window: 200_000,
                reserve_tokens: 16_000,
                keep_recent_tokens: 20_000,
                tokens_before: 10,
                tokens_after: 5,
                retained_event_count: None,
                read_files: vec![],
                modified_files: vec![],
                timestamp: ts(),
            })
            .unwrap();
        state.append_immediate(user_ev("new")).unwrap();

        // LLM sees: compaction summary + "new".
        let msgs = state.llm_messages();
        assert_eq!(msgs.len(), 2, "expected summary + new message");
        assert!(
            msgs[0].content.contains("ctx"),
            "first msg should be summary"
        );
        assert_eq!(msgs[1].content, "new");
    }

    /// A `CompactionSummary` appended via `append_immediate` must appear in
    /// the display projection as a single notice message.
    #[test]
    fn compaction_via_append_immediate_appears_in_display() {
        let path = std::env::temp_dir().join(format!(
            "xi-ss-compact-display-{}.jsonl",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);

        let mut state = SessionState::from_event_log(EventLog::load(&path).unwrap());
        state.append_immediate(user_ev("q")).unwrap();
        state
            .append_immediate(SessionEvent::CompactionSummary {
                summary: "ctx".to_string(),
                trigger_reason: CompactionTrigger::Threshold,
                context_window: 200_000,
                reserve_tokens: 16_000,
                keep_recent_tokens: 20_000,
                tokens_before: 50_000,
                tokens_after: 5_000,
                retained_event_count: None,
                read_files: vec![],
                modified_files: vec![],
                timestamp: ts(),
            })
            .unwrap();

        // Display has user message + compaction marker.
        assert_eq!(state.display_messages().len(), 2);
        let compaction_msg = &state.display_messages()[1];
        assert!(
            compaction_msg.content.contains("compacted:"),
            "compaction display message should contain 'compacted:'"
        );
        assert!(!compaction_msg.include_in_llm);
    }

    /// `append_batch` (turn flush) must not leave transient turn entries
    /// duplicated alongside the newly committed messages.
    #[test]
    fn append_batch_does_not_duplicate_committed_messages() {
        let path =
            std::env::temp_dir().join(format!("xi-ss-batch-dedup-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&path);

        let mut state = SessionState::from_event_log(EventLog::load(&path).unwrap());
        state.append_immediate(user_ev("hello")).unwrap();

        // Simulate transient in-flight assistant message shown before commit.
        state.display_messages_mut().push(Message::assistant("hi"));
        state.display_messages_mut().push(Message::tool_call(
            "c1",
            "read_file",
            serde_json::json!({}),
        ));

        // Flush the turn as a batch.
        state
            .append_batch(&[
                assistant_ev("hi"),
                SessionEvent::ToolCall {
                    id: "c1".to_string(),
                    name: "read_file".to_string(),
                    args: serde_json::json!({}),
                    include_in_llm: true,
                    timestamp: ts(),
                },
            ])
            .unwrap();

        let assistant_count = state
            .display_messages()
            .iter()
            .filter(|m| m.role == Role::Assistant && m.content == "hi")
            .count();
        assert_eq!(
            assistant_count, 1,
            "assistant message must appear exactly once"
        );

        let tool_count = state
            .display_messages()
            .iter()
            .filter(|m| m.role == Role::ToolCall)
            .count();
        assert_eq!(tool_count, 1, "tool call must appear exactly once");
    }
}
