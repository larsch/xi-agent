//! Projections over the session event log.
//!
//! A projection is a pure function (or stateful renderer) that derives
//! observable state from a `&[SessionEvent]`.  The three projections defined
//! here serve distinct consumers:
//!
//! | Projection | Consumer | Output |
//! |---|---|---|
//! | [`project_llm_messages`] | LLM provider requests | `Vec<Message>` |
//! | [`project_display_messages`] | UI chat log / export | `Vec<Message>` |
//! | [`DisplayProjection`] | `SessionState` committed display state | `Vec<Message>` |
//! | [`LlmProjection`] | `SessionState` committed LLM state | `Vec<Message>` |
//!
//! # Compaction boundary
//!
//! When a [`SessionEvent::CompactionSummary`] is present in the log,
//! `project_llm_messages` treats it as a boundary: the summary is injected as
//! a synthetic context message, and the post-compaction tail is retained.
//! For newer summaries this tail is reconstructed from
//! `retained_event_count`; legacy summaries fall back to events after the
//! summary line.
//!
//! The compaction boundary logic is fully exercised; both `retained_event_count`
//! (new sessions) and the legacy fallback (post-summary tail only) are handled.

use crate::{
    llm::{Message, Role},
    session_event::SessionEvent,
};

// ── LLM projection ────────────────────────────────────────────────────────────

/// Build the message list to send to the LLM from a session event log.
///
/// This is the replacement for `App::prepare_llm_messages`.  The system
/// prompt is **not** included here — callers prepend it as today.
///
/// # Compaction boundary
///
/// If the log contains one or more [`SessionEvent::CompactionSummary`] events,
/// only the most recent boundary is considered. The summary is injected as a
/// synthetic user message. For newer compactions that record
/// `retained_event_count`, that many events immediately preceding the summary
/// are also preserved verbatim, and events after the summary are projected as
/// normal. Legacy compactions without this metadata keep only post-summary
/// events.
///
/// # Trailing empty assistant message
///
/// A trailing [`Role::Assistant`] message with empty content is dropped, as in
/// the current `prepare_llm_messages` implementation.  Such messages are
/// created transiently by the streaming path to hold the `ToolIntentStart`
/// phase marker and should not be forwarded to the provider.
pub fn project_llm_messages(events: &[SessionEvent]) -> Vec<Message> {
    // Find the index of the most recent CompactionSummary, if any.
    let start = events
        .iter()
        .rposition(|e| matches!(e, SessionEvent::CompactionSummary { .. }));

    let mut msgs: Vec<Message> = Vec::new();

    if let Some(idx) = start {
        // Inject the compaction summary as a synthetic context message.
        if let SessionEvent::CompactionSummary {
            summary,
            retained_event_count,
            ..
        } = &events[idx]
        {
            msgs.push(Message::user(summary.clone()));

            // New sessions include retained tail metadata so we can keep the
            // requested recent span from immediately before the boundary while
            // staying append-only.
            if let Some(retained) = retained_event_count {
                let retained = (*retained).min(idx);
                let tail_start = idx.saturating_sub(retained);
                for ev in &events[tail_start..idx] {
                    push_llm_message(&mut msgs, ev);
                }
            }
        }
        // Project events after the boundary.
        for ev in &events[idx + 1..] {
            push_llm_message(&mut msgs, ev);
        }
    } else {
        for ev in events {
            push_llm_message(&mut msgs, ev);
        }
    }

    // Drop a trailing empty assistant message (streaming artefact).
    if matches!(msgs.last().map(|m| &m.role), Some(Role::Assistant))
        && msgs.last().map(|m| m.content.is_empty()).unwrap_or(false)
    {
        msgs.pop();
    }

    msgs
}

/// Push the LLM-visible [`Message`] for one event, if any.
fn push_llm_message(msgs: &mut Vec<Message>, ev: &SessionEvent) {
    match ev {
        SessionEvent::UserMessage { content, .. } => {
            msgs.push(Message::user(content.clone()));
        }
        SessionEvent::AssistantMessage {
            content,
            thinking,
            phase,
            ..
        } => {
            let mut msg = Message::assistant(content.clone());
            msg.thinking = thinking.clone();
            msg.assistant_phase = Some(*phase);
            msgs.push(msg);
        }
        SessionEvent::ToolCall { id, name, args, .. } => {
            msgs.push(Message::tool_call(id.clone(), name.clone(), args.clone()));
        }
        SessionEvent::ToolResult {
            id,
            content,
            is_error,
            display_range,
            ..
        } => {
            let mut msg = Message::tool_result(id.clone(), content.clone(), *is_error);
            msg.display_range = display_range.clone();
            msgs.push(msg);
        }
        // Not sent to the LLM.
        SessionEvent::TurnError { .. }
        | SessionEvent::CompactionSummary { .. }
        | SessionEvent::ModelChanged { .. }
        | SessionEvent::ThinkingLevelChanged { .. } => {}
    }
}

// ── Display / export projection ───────────────────────────────────────────────

/// Build the message list for UI rendering and HTML export from a session
/// event log.
///
/// This projection is unfiltered and full-fidelity — it includes every durable
/// event as a visible message.  The UI may later add filtering on top via
/// [`DisplayProjection`]; the export always uses the unfiltered output.
///
/// Mapping:
///
/// | Event | Message |
/// |---|---|
/// | `UserMessage` | `Role::User` |
/// | `AssistantMessage` | `Role::Assistant` (with thinking, phase) |
/// | `ToolCall` | `Role::ToolCall` |
/// | `ToolResult` | `Role::ToolResult` (with display_range) |
/// | `TurnError` | `Role::Assistant`, visible, `include_in_llm = false` |
/// | `CompactionSummary` | `Role::Assistant`, visible, `include_in_llm = false` |
/// | `ModelChanged` | `Role::Assistant`, hidden, `include_in_llm = false` |
/// | `ThinkingLevelChanged` | `Role::Assistant`, hidden, `include_in_llm = false` |
pub fn project_display_messages(events: &[SessionEvent]) -> Vec<Message> {
    let mut msgs: Vec<Message> = Vec::new();
    for ev in events {
        push_display_message(&mut msgs, ev);
    }
    msgs
}

/// Push the display-visible [`Message`] for one event.
fn push_display_message(msgs: &mut Vec<Message>, ev: &SessionEvent) {
    match ev {
        SessionEvent::UserMessage { content, .. } => {
            msgs.push(Message::user(content.clone()));
        }
        SessionEvent::AssistantMessage {
            content,
            thinking,
            phase,
            ..
        } => {
            let mut msg = Message::assistant(content.clone());
            msg.thinking = thinking.clone();
            msg.assistant_phase = Some(*phase);
            msgs.push(msg);
        }
        SessionEvent::ToolCall { id, name, args, .. } => {
            let mut msg = Message::tool_call(id.clone(), name.clone(), args.clone());
            if id.starts_with("attach_") {
                msg.hidden = true;
            }
            msgs.push(msg);
        }
        SessionEvent::ToolResult {
            id,
            content,
            is_error,
            display_range,
            ..
        } => {
            let mut msg = Message::tool_result(id.clone(), content.clone(), *is_error);
            msg.display_range = display_range.clone();
            if id.starts_with("attach_") {
                msg.hidden = true;
            }
            msgs.push(msg);
        }
        SessionEvent::TurnError { message, .. } => {
            // Visible in the UI so the user knows what went wrong on resume,
            // but not forwarded to the LLM.
            let mut msg = Message::assistant(message.clone());
            msg.include_in_llm = false;
            msgs.push(msg);
        }
        SessionEvent::CompactionSummary {
            summary,
            tokens_before,
            tokens_after,
            ..
        } => {
            // Show a compact one-line marker in the UI; the full summary is
            // sent to the LLM via project_llm_messages, not shown verbatim.
            let tokens_before_k = tokens_before / 1_000;
            let tokens_after_k = tokens_after / 1_000;
            let label =
                format!("[compacted: {tokens_before_k}k → {tokens_after_k}k tokens]\n\n{summary}");
            let mut msg = Message::assistant(label);
            msg.include_in_llm = false;
            msgs.push(msg);
        }
        SessionEvent::ModelChanged {
            model, provider, ..
        } => {
            let mut msg = Message::assistant(format!("[model changed: {provider} / {model}]"));
            msg.hidden = true;
            msg.include_in_llm = false;
            msgs.push(msg);
        }
        SessionEvent::ThinkingLevelChanged { level, .. } => {
            let mut msg =
                Message::assistant(format!("[thinking level changed: {}]", level.as_str()));
            msg.hidden = true;
            msg.include_in_llm = false;
            msgs.push(msg);
        }
    }
}

// ── DisplayProjection ─────────────────────────────────────────────────────────

/// Stateful incremental renderer for the UI chat log.
///
/// Wraps [`project_display_messages`] and maintains a cache of the last
/// projection result so that incremental updates (new events appended to the
/// log) are cheap.
///
/// The current UI also mutates the rendered message list temporarily while a
/// turn is still in flight. Once the corresponding session events are flushed,
/// callers should rebuild from the event log to replace those transient edits
/// with the durable projection.
#[derive(Debug, Default)]
pub struct DisplayProjection {
    /// Cached projection output.  Rebuilt on filter changes; extended
    /// incrementally on new events.
    messages: Vec<Message>,
    /// Number of events from the source log that are reflected in `messages`.
    processed: usize,
}

impl DisplayProjection {
    /// Create a new empty projection.
    pub fn new() -> Self {
        Self::default()
    }

    /// Return the current projected message list.
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// Return mutable access to the current rendered message list.
    ///
    /// Used for transient in-flight UI state while a turn is streaming.
    #[cfg(test)]
    pub fn messages_mut(&mut self) -> &mut Vec<Message> {
        &mut self.messages
    }

    /// True when no messages are currently rendered.
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// Number of currently rendered messages.
    pub fn len(&self) -> usize {
        self.messages.len()
    }

    /// Reset the projection to an empty state.
    #[cfg(test)]
    pub fn clear(&mut self) {
        self.messages.clear();
        self.processed = 0;
    }

    /// Extend the projection with newly appended events.
    ///
    /// `events` is the full event log (old + new).  Only events beyond
    /// `self.processed` are projected; existing messages are preserved.
    /// This is O(new events).
    pub fn apply_new_events(&mut self, events: &[SessionEvent]) {
        for ev in events.iter().skip(self.processed) {
            push_display_message(&mut self.messages, ev);
        }
        self.processed = events.len();
    }

    /// Rebuild the projection from scratch after a filter change.
    ///
    /// Currently a full rebuild since `DisplayFilter` is not yet implemented.
    /// O(log length) for the walk; render caches (phase 2) will make this
    /// cheaper.
    ///
    /// # Access restriction
    ///
    /// This method must only be called from within `session_state` (load and
    /// compaction paths). External callers should use `SessionState` ingestion
    /// methods instead.
    pub(crate) fn rebuild(&mut self, events: &[SessionEvent]) {
        self.messages = project_display_messages(events);
        self.processed = events.len();
    }
}

// ── LlmProjection ─────────────────────────────────────────────────────────────

/// Stateful incremental cache for the LLM-visible message list.
///
/// Maintains a lazily-validated view of [`project_llm_messages`] over the
/// session event log. The cache is invalidated when a compaction event is
/// ingested (because compaction changes what the LLM sees at the *start* of
/// history); on all other events the cache is extended incrementally.
#[derive(Debug, Default)]
pub struct LlmProjection {
    /// Cached message list.  `None` when the cache is invalid and must be
    /// rebuilt before use.
    messages: Option<Vec<Message>>,
    /// Number of events reflected in the current cache, valid only when
    /// `messages` is `Some`.
    processed: usize,
}

impl LlmProjection {
    /// Create a new, empty (invalid) projection.
    pub fn new() -> Self {
        Self::default()
    }

    /// Ensure the cache is current with respect to `events`.
    ///
    /// If the cache is invalid (e.g. after a compaction) a full rebuild is
    /// performed. Otherwise only new events are projected.
    pub fn ensure_current(&mut self, events: &[SessionEvent]) {
        match &self.messages {
            None => {
                // Full rebuild.
                self.messages = Some(project_llm_messages(events));
                self.processed = events.len();
            }
            Some(_) if events.len() > self.processed => {
                // Check whether any new events are compaction boundaries.
                let new_events = &events[self.processed..];
                let has_compaction = new_events
                    .iter()
                    .any(|e| matches!(e, SessionEvent::CompactionSummary { .. }));

                // The trailing-empty-assistant rule is not purely append-only:
                // an empty assistant that was previously dropped because it was
                // trailing may need to become visible again once subsequent
                // tool events arrive. Likewise, a newly-appended empty
                // assistant may need to be dropped if it becomes the new tail.
                // Rebuild whenever this rule could affect correctness.
                let previous_tail_was_empty_assistant = self.processed > 0
                    && matches!(
                        events.get(self.processed - 1),
                        Some(SessionEvent::AssistantMessage { content, .. }) if content.is_empty()
                    );
                let new_events_include_empty_assistant = new_events.iter().any(|e| {
                    matches!(
                        e,
                        SessionEvent::AssistantMessage { content, .. } if content.is_empty()
                    )
                });

                if has_compaction
                    || previous_tail_was_empty_assistant
                    || new_events_include_empty_assistant
                {
                    // These cases affect more than a simple append-only tail.
                    self.messages = Some(project_llm_messages(events));
                } else {
                    // Incremental: append messages for the new events only.
                    let msgs = self.messages.as_mut().unwrap();
                    for ev in new_events {
                        push_llm_message(msgs, ev);
                    }
                }
                self.processed = events.len();
            }
            Some(_) => {
                // Cache already up to date.
            }
        }
    }

    /// Return the cached messages, panicking if not current.
    ///
    /// Always call [`ensure_current`] before this.
    pub fn messages(&self) -> &[Message] {
        self.messages.as_deref().unwrap_or(&[])
    }

    /// Apply new events from the full event log, invalidating on compaction.
    ///
    /// This is the primary incremental update method for `SessionState`.
    pub fn apply_new_events(&mut self, events: &[SessionEvent]) {
        self.ensure_current(events);
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        llm::{AssistantPhase, DisplayRange, UsageStats},
        session_event::{CompactionTrigger, SessionEvent},
        thinking::ThinkingLevel,
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
            phase: AssistantPhase::Final,
            usage: None,
            timestamp: ts(),
        }
    }

    fn tool_call_ev(id: &str, name: &str) -> SessionEvent {
        SessionEvent::ToolCall {
            id: id.to_string(),
            name: name.to_string(),
            args: serde_json::json!({"path": "src/main.rs"}),
            timestamp: ts(),
        }
    }

    fn tool_result_ev(id: &str, content: &str) -> SessionEvent {
        SessionEvent::ToolResult {
            id: id.to_string(),
            name: "read_file".to_string(),
            content: content.to_string(),
            is_error: false,
            display_range: None,
            timestamp: ts(),
        }
    }

    fn compaction_ev(summary: &str, before: usize, after: usize) -> SessionEvent {
        SessionEvent::CompactionSummary {
            summary: summary.to_string(),
            trigger_reason: CompactionTrigger::Threshold,
            context_window: 200_000,
            reserve_tokens: 16_000,
            keep_recent_tokens: 20_000,
            tokens_before: before,
            tokens_after: after,
            retained_event_count: None,
            read_files: vec![],
            modified_files: vec![],
            timestamp: ts(),
        }
    }

    // ── project_llm_messages ──────────────────────────────────────────────────

    #[test]
    fn llm_user_and_assistant_project_correctly() {
        let events = vec![user_ev("hello"), assistant_ev("hi")];
        let msgs = project_llm_messages(&events);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, Role::User);
        assert_eq!(msgs[0].content, "hello");
        assert_eq!(msgs[1].role, Role::Assistant);
        assert_eq!(msgs[1].content, "hi");
    }

    #[test]
    fn llm_tool_call_and_result_project_correctly() {
        let events = vec![
            user_ev("do it"),
            assistant_ev(""),
            tool_call_ev("c1", "bash"),
            tool_result_ev("c1", "ok"),
        ];
        let msgs = project_llm_messages(&events);
        // Empty assistant is dropped by the trailing-empty rule.
        // Wait — the tool call follows it so it won't be last. Verify full chain.
        let roles: Vec<_> = msgs.iter().map(|m| &m.role).collect();
        assert!(roles.contains(&&Role::ToolCall));
        assert!(roles.contains(&&Role::ToolResult));
    }

    #[test]
    fn llm_turn_error_is_skipped() {
        let events = vec![
            user_ev("hello"),
            SessionEvent::TurnError {
                message: "rate limit".to_string(),
                timestamp: ts(),
            },
        ];
        let msgs = project_llm_messages(&events);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, Role::User);
    }

    #[test]
    fn llm_model_changed_and_thinking_changed_are_skipped() {
        let events = vec![
            user_ev("hello"),
            SessionEvent::ModelChanged {
                model: "gpt-4o".to_string(),
                provider: "openai".to_string(),
                timestamp: ts(),
            },
            SessionEvent::ThinkingLevelChanged {
                level: ThinkingLevel::High,
                timestamp: ts(),
            },
            assistant_ev("hi"),
        ];
        let msgs = project_llm_messages(&events);
        assert_eq!(msgs.len(), 2);
    }

    #[test]
    fn llm_trailing_empty_assistant_is_dropped() {
        let events = vec![
            user_ev("hello"),
            SessionEvent::AssistantMessage {
                content: String::new(),
                thinking: None,
                phase: AssistantPhase::Provisional,
                usage: None,
                timestamp: ts(),
            },
        ];
        let msgs = project_llm_messages(&events);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, Role::User);
    }

    #[test]
    fn llm_nonempty_assistant_is_not_dropped() {
        let events = vec![user_ev("hello"), assistant_ev("hi")];
        let msgs = project_llm_messages(&events);
        assert_eq!(msgs.len(), 2);
    }

    // ── LlmProjection incremental cache parity ───────────────────────────────

    #[test]
    fn llm_projection_rebuilds_when_new_empty_assistant_arrives() {
        let mut proj = LlmProjection::new();
        let events = vec![user_ev("hello")];
        proj.apply_new_events(&events);
        assert_eq!(proj.messages().len(), 1);

        let events = vec![
            user_ev("hello"),
            SessionEvent::AssistantMessage {
                content: String::new(),
                thinking: None,
                phase: AssistantPhase::Provisional,
                usage: None,
                timestamp: ts(),
            },
        ];
        proj.apply_new_events(&events);
        assert_eq!(
            proj.messages().len(),
            1,
            "trailing empty assistant should be dropped"
        );
    }

    #[test]
    fn llm_projection_rebuilds_when_dropped_empty_assistant_gains_following_tool_call() {
        let mut proj = LlmProjection::new();
        let events = vec![
            user_ev("hello"),
            SessionEvent::AssistantMessage {
                content: String::new(),
                thinking: None,
                phase: AssistantPhase::Provisional,
                usage: None,
                timestamp: ts(),
            },
        ];
        proj.apply_new_events(&events);
        assert_eq!(
            proj.messages().len(),
            1,
            "trailing empty assistant should be dropped"
        );

        let events = vec![
            user_ev("hello"),
            SessionEvent::AssistantMessage {
                content: String::new(),
                thinking: None,
                phase: AssistantPhase::Provisional,
                usage: None,
                timestamp: ts(),
            },
            tool_call_ev("c1", "bash"),
        ];
        proj.apply_new_events(&events);

        let roles: Vec<_> = proj.messages().iter().map(|m| m.role.clone()).collect();
        assert_eq!(roles, vec![Role::User, Role::Assistant, Role::ToolCall]);
    }

    #[test]
    fn llm_thinking_and_phase_are_preserved() {
        let events = vec![SessionEvent::AssistantMessage {
            content: "answer".to_string(),
            thinking: Some("thinking...".to_string()),
            phase: AssistantPhase::Final,
            usage: Some(UsageStats {
                input_tokens: Some(10),
                output_tokens: Some(5),
                total_tokens: Some(15),
                cached_tokens: None,
            }),
            timestamp: ts(),
        }];
        let msgs = project_llm_messages(&events);
        assert_eq!(msgs[0].thinking.as_deref(), Some("thinking..."));
        assert_eq!(msgs[0].assistant_phase, Some(AssistantPhase::Final));
    }

    #[test]
    fn llm_display_range_is_preserved_on_tool_result() {
        let events = vec![SessionEvent::ToolResult {
            id: "c1".to_string(),
            name: "read_file".to_string(),
            content: "content".to_string(),
            is_error: false,
            display_range: Some(DisplayRange {
                first_line: 1,
                last_line: 10,
                total_lines: 100,
            }),
            timestamp: ts(),
        }];
        let msgs = project_llm_messages(&events);
        assert!(msgs[0].display_range.is_some());
    }

    // ── compaction boundary ───────────────────────────────────────────────────

    #[test]
    fn llm_events_before_compaction_are_excluded() {
        let events = vec![
            user_ev("old"),
            assistant_ev("old reply"),
            compaction_ev("## Goal\nfix stuff", 50_000, 5_000),
            user_ev("new"),
            assistant_ev("new reply"),
        ];
        let msgs = project_llm_messages(&events);
        // Summary injected as user message, then the two post-compaction events.
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0].role, Role::User);
        assert!(msgs[0].content.contains("fix stuff"));
        assert_eq!(msgs[1].content, "new");
        assert_eq!(msgs[2].content, "new reply");
    }

    #[test]
    fn llm_compaction_with_retained_tail_keeps_recent_pre_summary_events() {
        let events = vec![
            user_ev("old"),
            assistant_ev("old reply"),
            user_ev("keep me"),
            assistant_ev("keep reply"),
            SessionEvent::CompactionSummary {
                summary: "## Goal\nfix stuff".to_string(),
                trigger_reason: CompactionTrigger::Threshold,
                context_window: 200_000,
                reserve_tokens: 16_000,
                keep_recent_tokens: 20_000,
                tokens_before: 50_000,
                tokens_after: 5_000,
                retained_event_count: Some(2),
                read_files: vec![],
                modified_files: vec![],
                timestamp: ts(),
            },
            user_ev("new"),
        ];
        let msgs = project_llm_messages(&events);
        assert_eq!(msgs.len(), 4);
        assert!(msgs[0].content.contains("fix stuff"));
        assert_eq!(msgs[1].content, "keep me");
        assert_eq!(msgs[2].content, "keep reply");
        assert_eq!(msgs[3].content, "new");
    }

    #[test]
    fn llm_uses_most_recent_compaction_boundary() {
        let events = vec![
            user_ev("very old"),
            compaction_ev("first summary", 40_000, 4_000),
            user_ev("old"),
            compaction_ev("second summary", 30_000, 3_000),
            user_ev("new"),
        ];
        let msgs = project_llm_messages(&events);
        assert_eq!(msgs.len(), 2);
        assert!(msgs[0].content.contains("second summary"));
        assert_eq!(msgs[1].content, "new");
    }

    #[test]
    fn llm_no_compaction_includes_all_events() {
        let events = vec![user_ev("a"), assistant_ev("b"), user_ev("c")];
        let msgs = project_llm_messages(&events);
        assert_eq!(msgs.len(), 3);
    }

    // ── project_display_messages ──────────────────────────────────────────────

    #[test]
    fn display_turn_error_is_visible_but_not_in_llm() {
        let events = vec![SessionEvent::TurnError {
            message: "[Error: rate limit]".to_string(),
            timestamp: ts(),
        }];
        let msgs = project_display_messages(&events);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, Role::Assistant);
        assert!(!msgs[0].include_in_llm);
        assert!(!msgs[0].hidden);
    }

    #[test]
    fn display_compaction_summary_is_visible_but_not_in_llm() {
        let events = vec![compaction_ev("## Goal\nfix stuff", 50_000, 5_000)];
        let msgs = project_display_messages(&events);
        assert_eq!(msgs.len(), 1);
        assert!(!msgs[0].include_in_llm);
        assert!(!msgs[0].hidden);
        assert!(msgs[0].content.contains("compacted:"));
        assert!(msgs[0].content.contains("fix stuff"));
    }

    #[test]
    fn display_model_changed_is_hidden() {
        let events = vec![SessionEvent::ModelChanged {
            model: "gpt-4o".to_string(),
            provider: "openai".to_string(),
            timestamp: ts(),
        }];
        let msgs = project_display_messages(&events);
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].hidden);
        assert!(!msgs[0].include_in_llm);
    }

    #[test]
    fn display_thinking_level_changed_is_hidden() {
        let events = vec![SessionEvent::ThinkingLevelChanged {
            level: ThinkingLevel::Medium,
            timestamp: ts(),
        }];
        let msgs = project_display_messages(&events);
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].hidden);
        assert!(!msgs[0].include_in_llm);
    }

    #[test]
    fn display_full_conversation_maps_correctly() {
        let events = vec![
            user_ev("hello"),
            assistant_ev("hi"),
            tool_call_ev("c1", "bash"),
            tool_result_ev("c1", "output"),
        ];
        let msgs = project_display_messages(&events);
        assert_eq!(msgs.len(), 4);
        assert_eq!(msgs[0].role, Role::User);
        assert_eq!(msgs[1].role, Role::Assistant);
        assert_eq!(msgs[2].role, Role::ToolCall);
        assert_eq!(msgs[3].role, Role::ToolResult);
    }

    // ── DisplayProjection ─────────────────────────────────────────────────────

    #[test]
    fn display_projection_starts_empty() {
        let proj = DisplayProjection::new();
        assert!(proj.messages().is_empty());
    }

    #[test]
    fn display_projection_apply_new_events_incremental() {
        let mut proj = DisplayProjection::new();
        let events = vec![user_ev("hello"), assistant_ev("hi")];

        proj.apply_new_events(&events[..1]);
        assert_eq!(proj.messages().len(), 1);

        proj.apply_new_events(&events);
        assert_eq!(proj.messages().len(), 2);
    }

    #[test]
    fn display_projection_apply_new_events_is_idempotent() {
        let mut proj = DisplayProjection::new();
        let events = vec![user_ev("hello")];
        proj.apply_new_events(&events);
        proj.apply_new_events(&events);
        assert_eq!(proj.messages().len(), 1);
    }

    #[test]
    fn display_projection_rebuild_replaces_messages() {
        let mut proj = DisplayProjection::new();
        let events = vec![user_ev("hello"), assistant_ev("hi")];
        proj.apply_new_events(&events);
        assert_eq!(proj.messages().len(), 2);

        // Rebuild with a different (shorter) log.
        let short = vec![user_ev("only this")];
        proj.rebuild(&short);
        assert_eq!(proj.messages().len(), 1);
        assert_eq!(proj.messages()[0].content, "only this");
    }

    #[test]
    fn display_projection_clear_resets_state() {
        let mut proj = DisplayProjection::new();
        proj.apply_new_events(&[user_ev("hello")]);
        assert_eq!(proj.len(), 1);

        proj.clear();
        assert!(proj.is_empty());

        proj.apply_new_events(&[assistant_ev("hi")]);
        assert_eq!(proj.len(), 1);
        assert_eq!(proj.messages()[0].content, "hi");
    }

    #[test]
    fn synthetic_attachment_events_are_hidden_in_display_projection() {
        let events = vec![
            user_ev("look at @src/main.rs"),
            tool_call_ev("attach_0", "read_file"),
            tool_result_ev("attach_0", "fn main() {}"),
        ];
        let msgs = project_display_messages(&events);
        // All three events produce a Message, but the two attach_ ones are hidden.
        let attach_msgs: Vec<_> = msgs
            .iter()
            .filter(|m| {
                m.tool_call_id
                    .as_deref()
                    .is_some_and(|id| id.starts_with("attach_"))
            })
            .collect();
        assert_eq!(
            attach_msgs.len(),
            2,
            "expected ToolCall + ToolResult messages"
        );
        for m in &attach_msgs {
            assert!(
                m.hidden,
                "attach_ message should be hidden: {:?}",
                m.content
            );
        }
        // The user message itself is not hidden.
        let user_msg = msgs
            .iter()
            .find(|m| m.role == crate::llm::Role::User)
            .unwrap();
        assert!(!user_msg.hidden);
    }

    #[test]
    fn synthetic_attachment_events_are_present_in_llm_projection() {
        let events = vec![
            user_ev("look at @src/main.rs"),
            tool_call_ev("attach_0", "read_file"),
            tool_result_ev("attach_0", "fn main() {}"),
        ];
        let msgs = project_llm_messages(&events);
        // LLM sees the user message first, then the synthetic tool call/result.
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0].role, crate::llm::Role::User);
        assert_eq!(msgs[1].role, crate::llm::Role::ToolCall);
        assert_eq!(msgs[2].role, crate::llm::Role::ToolResult);
    }
}
