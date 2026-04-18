//! Transient in-flight state for the currently active agent turn.
//!
//! # Ownership boundaries
//!
//! [`LiveTurnState`] owns everything that is **not yet committed** to the
//! durable session event log:
//!
//! - Streaming assistant text accumulated so far (`assistant_content`,
//!   `assistant_thinking`, `assistant_phase`)
//! - In-progress tool call/result pairs (`tool_entries`)
//! - UI-only notices — errors, export confirmations, session warnings — that
//!   are never forwarded to the LLM and never backed by a `SessionEvent`
//!   (`notices`)
//!
//! ## What belongs here
//! - Anything produced during a streaming turn that hasn't been flushed yet
//! - UI-only feedback messages with no conversation meaning
//! - Local shell execution output (never enters the event log or LLM history)
//!
//! ## What does NOT belong here
//! - Committed conversation history (lives in `SessionState`)
//! - Events (belong in the `SessionEvent` log)
//!
//! ## Lifecycle
//! - `assistant_content`, `assistant_thinking`, `assistant_phase`, and
//!   `tool_entries` are cleared at each turn boundary (after `flush_turn_events`).
//! - `notices` persist until the next `new_conversation` reset — they survive
//!   turn boundaries because they are user-visible session-level feedback.

use crate::llm::{AssistantPhase, DisplayRange, Message};

/// A single in-progress tool call, optionally with its result.
#[derive(Debug, Clone)]
pub struct LiveToolEntry {
    pub id: String,
    pub name: String,
    pub args: serde_json::Value,
    pub result: Option<LiveToolResult>,
}

/// The result side of an in-progress tool call.
#[derive(Debug, Clone)]
pub struct LiveToolResult {
    pub content: String,
    pub is_error: bool,
    pub display_range: Option<DisplayRange>,
}

/// Transient state for the currently active (or most recently completed but
/// not yet flushed) agent turn.
#[derive(Debug, Default)]
pub struct LiveTurnState {
    /// Streaming assistant text accumulated so far.
    pub assistant_content: String,
    /// Streaming thinking content, if any.
    pub assistant_thinking: Option<String>,
    /// Current assistant phase marker.
    pub assistant_phase: AssistantPhase,
    /// In-progress tool call/result pairs for this turn.
    pub tool_entries: Vec<LiveToolEntry>,
    /// UI-only notices (errors, export confirmations, warnings).
    /// Not backed by `SessionEvent`; never forwarded to the LLM.
    /// Persist across turn boundaries until `new_conversation`.
    pub notices: Vec<Message>,
}

impl LiveTurnState {
    /// Create a new, empty live turn state.
    pub fn new() -> Self {
        Self::default()
    }

    /// True when there is any in-flight assistant content (text or thinking).
    pub fn has_assistant_content(&self) -> bool {
        !self.assistant_content.is_empty() || self.assistant_thinking.is_some()
    }

    /// True when there are any active or completed tool entries.
    pub fn has_tool_entries(&self) -> bool {
        !self.tool_entries.is_empty()
    }

    /// Clear the turn-scoped fields after flushing to committed state.
    ///
    /// `notices` are preserved — they survive turn boundaries.
    pub fn clear_turn(&mut self) {
        self.assistant_content.clear();
        self.assistant_thinking = None;
        self.assistant_phase = AssistantPhase::default();
        self.tool_entries.clear();
    }

    /// Clear everything including notices (called on `new_conversation`).
    pub fn clear_all(&mut self) {
        self.clear_turn();
        self.notices.clear();
    }

    /// Build the overlay messages to compose with committed display history
    /// at render time.
    ///
    /// `streaming` should be `true` while an agent turn is active so that an
    /// empty assistant entry is included even before the first token arrives
    /// (this lets the UI render the waiting cursor `▋`).
    ///
    /// Returns messages in display order:
    /// 1. Assistant message (if any content/thinking present, or streaming)
    /// 2. Tool call/result pairs (in insertion order)
    /// 3. UI-only notices
    pub fn render_overlay(&self, streaming: bool) -> Vec<Message> {
        let mut msgs: Vec<Message> = Vec::new();

        let show_assistant = self.has_assistant_content()
            || self.assistant_phase != AssistantPhase::default()
            || streaming;

        if show_assistant {
            let mut asst = Message::assistant(self.assistant_content.clone());
            asst.thinking = self.assistant_thinking.clone();
            asst.assistant_phase = Some(self.assistant_phase);
            msgs.push(asst);
        }

        for entry in &self.tool_entries {
            msgs.push(Message::tool_call(
                entry.id.clone(),
                entry.name.clone(),
                entry.args.clone(),
            ));
            if let Some(result) = &entry.result {
                let mut msg =
                    Message::tool_result(entry.id.clone(), result.content.clone(), result.is_error);
                if let Some(dr) = result.display_range.clone() {
                    msg = msg.with_display_range(dr);
                }
                msgs.push(msg);
            }
        }

        msgs.extend(self.notices.iter().cloned());
        msgs
    }

    /// Find a mutable reference to the tool entry with the given call ID.
    pub fn find_tool_entry_mut(&mut self, id: &str) -> Option<&mut LiveToolEntry> {
        self.tool_entries.iter_mut().find(|e| e.id == id)
    }
}

/// Build display messages for the UI: committed history + live overlay.
pub fn compose_display(
    committed: &[Message],
    live: &LiveTurnState,
    streaming: bool,
) -> Vec<Message> {
    let mut msgs = committed.to_vec();
    msgs.extend(live.render_overlay(streaming));
    msgs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::Role;

    #[test]
    fn render_overlay_empty_when_nothing_active() {
        let live = LiveTurnState::new();
        assert!(live.render_overlay(false).is_empty());
    }

    #[test]
    fn render_overlay_includes_assistant_content() {
        let mut live = LiveTurnState::new();
        live.assistant_content = "hello".to_string();
        let overlay = live.render_overlay(false);
        assert_eq!(overlay.len(), 1);
        assert_eq!(overlay[0].role, Role::Assistant);
        assert_eq!(overlay[0].content, "hello");
    }

    #[test]
    fn render_overlay_includes_thinking() {
        let mut live = LiveTurnState::new();
        live.assistant_thinking = Some("...".to_string());
        let overlay = live.render_overlay(false);
        assert_eq!(overlay.len(), 1);
        assert_eq!(overlay[0].thinking.as_deref(), Some("..."));
    }

    #[test]
    fn render_overlay_includes_tool_entries_in_order() {
        let mut live = LiveTurnState::new();
        live.assistant_content = "ok".to_string();
        live.tool_entries.push(LiveToolEntry {
            id: "c1".to_string(),
            name: "read_file".to_string(),
            args: serde_json::json!({}),
            result: Some(LiveToolResult {
                content: "content".to_string(),
                is_error: false,
                display_range: None,
            }),
        });
        let overlay = live.render_overlay(false);
        assert_eq!(overlay.len(), 3); // assistant + tool_call + tool_result
        assert_eq!(overlay[0].role, Role::Assistant);
        assert_eq!(overlay[1].role, Role::ToolCall);
        assert_eq!(overlay[2].role, Role::ToolResult);
    }

    #[test]
    fn render_overlay_includes_notices_after_turn_entries() {
        let mut live = LiveTurnState::new();
        live.notices.push(Message::assistant("[exported]"));
        let overlay = live.render_overlay(false);
        assert_eq!(overlay.len(), 1);
        assert_eq!(overlay[0].content, "[exported]");
    }

    #[test]
    fn clear_turn_preserves_notices() {
        let mut live = LiveTurnState::new();
        live.assistant_content = "hi".to_string();
        live.notices.push(Message::assistant("[notice]"));
        live.clear_turn();
        assert!(live.assistant_content.is_empty());
        assert_eq!(live.notices.len(), 1);
    }

    #[test]
    fn clear_all_removes_notices() {
        let mut live = LiveTurnState::new();
        live.notices.push(Message::assistant("[notice]"));
        live.clear_all();
        assert!(live.notices.is_empty());
    }

    #[test]
    fn compose_display_combines_committed_and_overlay() {
        let committed = vec![Message::user("hello")];
        let mut live = LiveTurnState::new();
        live.assistant_content = "hi".to_string();
        let combined = compose_display(&committed, &live, false);
        assert_eq!(combined.len(), 2);
        assert_eq!(combined[0].role, Role::User);
        assert_eq!(combined[1].role, Role::Assistant);
    }
}
