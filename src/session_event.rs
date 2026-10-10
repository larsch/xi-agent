//! Durable session event types for the append-only session log.
//!
//! A session is stored on disk as a JSONL file where each line is one
//! serialized [`SessionEvent`].  The in-memory representation is a
//! `Vec<SessionEvent>`.
//!
//! Observable state (LLM message list, UI display, export) is always derived
//! by projecting the event log — never by mutating it directly.
//!
//! # Serialization
//!
//! The `type` discriminant is serialized as a snake_case string tag, e.g.:
//!
//! ```json
//! {"type":"user_message","content":"hello","timestamp":1713000000}
//! {"type":"assistant_message","content":"hi","thinking":null,"phase":"final","usage":null,"timestamp":1713000001}
//! {"type":"tool_call","id":"call_1","name":"read_file","args":{"path":"src/main.rs"},"timestamp":1713000002}
//! {"type":"tool_result","id":"call_1","name":"read_file","content":"fn main() {}","is_error":false,"display_range":null,"timestamp":1713000003}
//! {"type":"turn_error","message":"rate limit exceeded","timestamp":1713000004}
//! {"type":"model_changed","model":"claude-opus-4","provider":"copilot","timestamp":1713000005}
//! {"type":"thinking_level_changed","level":"high","timestamp":1713000006}
//! ```
//!
//! Rust variant names are PascalCase; the serialized `type` tag is
//! snake_case via `#[serde(rename_all = "snake_case")]`.
//!
//! # Append policy
//!
//! Events are only written to disk as **complete units**:
//!
//! - [`UserMessage`] — appended immediately on submission.
//! - [`AssistantMessage`] + any [`ToolCall`]/[`ToolResult`] pairs for that
//!   turn — buffered in memory and appended as a single batch when the full
//!   turn completes.  [`ToolCall`] is never written without its matching
//!   [`ToolResult`].
//! - [`TurnError`] — appended when a turn fails.
//! - [`ModelChanged`], [`ThinkingLevelChanged`] — appended immediately.
//! - [`CompactionSummary`] — appended when the compaction summary is fully
//!   generated (phase 2).
//!
//! [`UserMessage`]: SessionEvent::UserMessage
//! [`AssistantMessage`]: SessionEvent::AssistantMessage
//! [`ToolCall`]: SessionEvent::ToolCall
//! [`ToolResult`]: SessionEvent::ToolResult
//! [`TurnError`]: SessionEvent::TurnError
//! [`ModelChanged`]: SessionEvent::ModelChanged
//! [`ThinkingLevelChanged`]: SessionEvent::ThinkingLevelChanged
//! [`CompactionSummary`]: SessionEvent::CompactionSummary

use crate::{
    llm::{AssistantPhase, DisplayRange, UsageStats},
    thinking::ThinkingLevel,
};

/// The last file operation that established an observation baseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileInteraction {
    Read,
    Write,
    Edit,
}

impl FileInteraction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Edit => "edit",
        }
    }
}

/// A file observed by the agent and subsequently changed outside the agent.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EnvironmentChange {
    pub path: String,
    pub baseline: FileInteraction,
}

fn default_true() -> bool {
    true
}

/// Translate the internal skill-learning tool name at the session-file
/// compatibility boundary. The application uses `learn`, while the durable
/// event format retains `read_skill`.
fn serialize_tool_name<S>(name: &str, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_str(if name == "learn" { "read_skill" } else { name })
}

fn deserialize_tool_name<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let name = <String as serde::Deserialize>::deserialize(deserializer)?;
    Ok(if name == "read_skill" {
        "learn".to_string()
    } else {
        name
    })
}

/// A single durable event in the session log.
///
/// See the [module documentation](self) for serialization format and append
/// policy details.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEvent {
    /// A message submitted by the user.
    UserMessage { content: String, timestamp: u64 },

    /// A completed assistant response, including optional thinking content
    /// and token usage for the turn.
    AssistantMessage {
        content: String,
        thinking: Option<String>,
        phase: AssistantPhase,
        usage: Option<UsageStats>,
        timestamp: u64,
    },

    /// A tool call requested by the assistant.
    ///
    /// Always followed immediately in the log by the matching [`ToolResult`]
    /// with the same `id`.  The two are always appended together as a batch
    /// — a [`ToolCall`] is never persisted without its result.
    ///
    /// [`ToolResult`]: SessionEvent::ToolResult
    /// [`ToolCall`]: SessionEvent::ToolCall
    ToolCall {
        /// Opaque identifier linking this call to its result.
        id: String,
        /// Name of the tool invoked.
        #[serde(
            serialize_with = "serialize_tool_name",
            deserialize_with = "deserialize_tool_name"
        )]
        name: String,
        /// Arguments passed to the tool (JSON object).
        args: serde_json::Value,
        /// When false, excluded from the LLM projection.  Used for local shell
        /// commands that are visible in the session log but never sent to the model.
        #[serde(default = "default_true")]
        include_in_llm: bool,
        timestamp: u64,
    },

    /// The result of a tool call.
    ///
    /// Always preceded in the log by the matching [`ToolCall`] with the same
    /// `id`.
    ///
    /// [`ToolCall`]: SessionEvent::ToolCall
    ToolResult {
        /// Opaque identifier matching the preceding [`ToolCall`].
        ///
        /// [`ToolCall`]: SessionEvent::ToolCall
        id: String,
        /// Name of the tool that was invoked (denormalized for readability).
        #[serde(
            serialize_with = "serialize_tool_name",
            deserialize_with = "deserialize_tool_name"
        )]
        name: String,
        /// Output returned by the tool.
        content: String,
        /// True when the tool returned an error.
        is_error: bool,
        /// Line-range metadata when only a window of a file was returned.
        display_range: Option<DisplayRange>,
        /// When false, excluded from the LLM projection.  Used for local shell
        /// commands that are visible in the session log but never sent to the model.
        #[serde(default = "default_true")]
        include_in_llm: bool,
        timestamp: u64,
    },

    /// Files previously observed by the agent changed outside the agent.
    EnvironmentNotification {
        changes: Vec<EnvironmentChange>,
        timestamp: u64,
    },

    /// A provider/API-level error that terminated a turn.
    ///
    /// Not included in the LLM projection — the model does not see it.
    /// Shown in the UI and export so the user knows what went wrong on
    /// resume.
    TurnError { message: String, timestamp: u64 },

    /// A compaction was performed.
    ///
    /// The `project_llm_messages` projection treats this as a boundary:
    /// events before the most recent [`CompactionSummary`] are excluded from
    /// the LLM context; the summary itself is injected as a single synthetic
    /// context message.
    ///
    /// Defined here for forward-compatibility; used in phase 2.
    ///
    /// [`CompactionSummary`]: SessionEvent::CompactionSummary
    CompactionSummary {
        /// Structured summary of the compacted history.
        summary: String,
        /// What triggered the compaction.
        trigger_reason: CompactionTrigger,
        /// Context window size of the active model at compaction time.
        context_window: usize,
        /// Token budget reserved for the model's response.
        reserve_tokens: usize,
        /// Token budget kept as recent verbatim history.
        keep_recent_tokens: usize,
        /// Estimated context tokens before compaction.
        tokens_before: usize,
        /// Estimated context tokens after compaction.
        tokens_after: usize,
        /// Number of trailing events from pre-compaction history that must be
        /// retained verbatim after injecting this summary.
        ///
        /// When present, projection keeps that many events from immediately
        /// before this summary event and appends any events written after this
        /// summary. When absent (legacy sessions), projection falls back to the
        /// old boundary behavior and only keeps events after this summary.
        #[serde(default)]
        retained_event_count: Option<usize>,
        /// Files read during the compacted history span.
        read_files: Vec<String>,
        /// Files written or edited during the compacted history span.
        modified_files: Vec<String>,
        timestamp: u64,
    },

    /// The active model and/or provider was changed during the session.
    ModelChanged {
        model: String,
        provider: String,
        timestamp: u64,
    },

    /// The active agent profile was changed during the session. An empty
    /// name represents the default agent.
    AgentChanged { agent: String, timestamp: u64 },

    /// The thinking level was changed during the session.
    ThinkingLevelChanged {
        level: ThinkingLevel,
        timestamp: u64,
    },
}

/// What triggered a compaction.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionTrigger {
    /// Context usage crossed the threshold after a completed turn.
    Threshold,
    /// The provider returned a context-overflow error; compaction was
    /// performed and the request retried.
    OverflowRetry,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts() -> u64 {
        1_713_000_000
    }

    // ── Round-trip serialization ──────────────────────────────────────────────

    #[test]
    fn environment_notification_round_trips() {
        let ev = SessionEvent::EnvironmentNotification {
            changes: vec![EnvironmentChange {
                path: "src/main.rs".to_string(),
                baseline: FileInteraction::Edit,
            }],
            timestamp: ts(),
        };
        let json = serde_json::to_string(&ev).unwrap();
        let decoded: SessionEvent = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(decoded, SessionEvent::EnvironmentNotification { changes, .. }
            if changes[0].path == "src/main.rs" && changes[0].baseline == FileInteraction::Edit)
        );
    }

    #[test]
    fn user_message_round_trips() {
        let ev = SessionEvent::UserMessage {
            content: "hello".to_string(),
            timestamp: ts(),
        };
        let json = serde_json::to_string(&ev).unwrap();
        let decoded: SessionEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(decoded, SessionEvent::UserMessage { content, .. } if content == "hello"));
    }

    #[test]
    fn assistant_message_round_trips() {
        let ev = SessionEvent::AssistantMessage {
            content: "hi".to_string(),
            thinking: Some("let me think".to_string()),
            phase: AssistantPhase::Final,
            usage: Some(UsageStats {
                input_tokens: Some(10),
                output_tokens: Some(5),
                total_tokens: Some(15),
                cached_tokens: None,
            }),
            timestamp: ts(),
        };
        let json = serde_json::to_string(&ev).unwrap();
        let decoded: SessionEvent = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(decoded, SessionEvent::AssistantMessage { ref content, ref thinking, phase, .. }
                if content == "hi" && thinking.as_deref() == Some("let me think") && phase == AssistantPhase::Final)
        );
    }

    #[test]
    fn tool_call_round_trips() {
        let ev = SessionEvent::ToolCall {
            id: "call_1".to_string(),
            name: "learn".to_string(),
            args: serde_json::json!({"path": "src/main.rs"}),
            include_in_llm: true,
            timestamp: ts(),
        };
        let json = serde_json::to_string(&ev).unwrap();
        let decoded: SessionEvent = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(decoded, SessionEvent::ToolCall { ref id, ref name, .. }
            if id == "call_1" && name == "learn")
        );
        assert!(json.contains(r#""name":"read_skill""#));
    }

    #[test]
    fn legacy_skill_tool_name_deserializes_to_learn() {
        let json = r#"{"type":"tool_call","id":"call_1","name":"read_skill","args":{},"timestamp":1713000000}"#;
        let decoded: SessionEvent = serde_json::from_str(json).unwrap();
        assert!(matches!(decoded, SessionEvent::ToolCall { name, .. } if name == "learn"));
    }

    #[test]
    fn tool_result_round_trips() {
        let ev = SessionEvent::ToolResult {
            id: "call_1".to_string(),
            name: "read_file".to_string(),
            content: "fn main() {}".to_string(),
            is_error: false,
            display_range: None,
            include_in_llm: true,
            timestamp: ts(),
        };
        let json = serde_json::to_string(&ev).unwrap();
        let decoded: SessionEvent = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(decoded, SessionEvent::ToolResult { ref content, is_error, .. }
                if content == "fn main() {}" && !is_error)
        );
    }

    #[test]
    fn turn_error_round_trips() {
        let ev = SessionEvent::TurnError {
            message: "rate limit exceeded".to_string(),
            timestamp: ts(),
        };
        let json = serde_json::to_string(&ev).unwrap();
        let decoded: SessionEvent = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(decoded, SessionEvent::TurnError { ref message, .. }
                if message == "rate limit exceeded")
        );
    }

    #[test]
    fn model_changed_round_trips() {
        let ev = SessionEvent::ModelChanged {
            model: "claude-opus-4".to_string(),
            provider: "copilot".to_string(),
            timestamp: ts(),
        };
        let json = serde_json::to_string(&ev).unwrap();
        let decoded: SessionEvent = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(decoded, SessionEvent::ModelChanged { ref model, .. }
            if model == "claude-opus-4")
        );
    }

    #[test]
    fn agent_changed_round_trips_including_default() {
        for agent in ["specialist", ""] {
            let ev = SessionEvent::AgentChanged {
                agent: agent.to_string(),
                timestamp: ts(),
            };
            let json = serde_json::to_string(&ev).unwrap();
            let decoded: SessionEvent = serde_json::from_str(&json).unwrap();
            assert!(
                matches!(decoded, SessionEvent::AgentChanged { agent: decoded, .. } if decoded == agent)
            );
        }
    }

    #[test]
    fn thinking_level_changed_round_trips() {
        let ev = SessionEvent::ThinkingLevelChanged {
            level: ThinkingLevel::High,
            timestamp: ts(),
        };
        let json = serde_json::to_string(&ev).unwrap();
        let decoded: SessionEvent = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(decoded, SessionEvent::ThinkingLevelChanged { level, .. }
                if level == ThinkingLevel::High)
        );
    }

    #[test]
    fn compaction_summary_round_trips() {
        let ev = SessionEvent::CompactionSummary {
            summary: "## Goal\nfix bugs".to_string(),
            trigger_reason: CompactionTrigger::Threshold,
            context_window: 200_000,
            reserve_tokens: 16_000,
            keep_recent_tokens: 20_000,
            tokens_before: 184_000,
            tokens_after: 22_000,
            retained_event_count: Some(42),
            read_files: vec!["src/main.rs".to_string()],
            modified_files: vec![],
            timestamp: ts(),
        };
        let json = serde_json::to_string(&ev).unwrap();
        let decoded: SessionEvent = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(decoded, SessionEvent::CompactionSummary { tokens_before, tokens_after, .. }
                if tokens_before == 184_000 && tokens_after == 22_000)
        );
    }

    #[test]
    fn compaction_summary_missing_retained_event_count_defaults_to_none() {
        let json = r#"{"type":"compaction_summary","summary":"s","trigger_reason":"threshold","context_window":200000,"reserve_tokens":16000,"keep_recent_tokens":20000,"tokens_before":100,"tokens_after":50,"read_files":[],"modified_files":[],"timestamp":1713000000}"#;
        let decoded: SessionEvent = serde_json::from_str(json).unwrap();
        assert!(matches!(
            decoded,
            SessionEvent::CompactionSummary {
                retained_event_count: None,
                ..
            }
        ));
    }

    // ── Type tag format ───────────────────────────────────────────────────────

    #[test]
    fn type_tags_are_snake_case() {
        let cases: &[(SessionEvent, &str)] = &[
            (
                SessionEvent::UserMessage {
                    content: String::new(),
                    timestamp: ts(),
                },
                "user_message",
            ),
            (
                SessionEvent::AssistantMessage {
                    content: String::new(),
                    thinking: None,
                    phase: AssistantPhase::Final,
                    usage: None,
                    timestamp: ts(),
                },
                "assistant_message",
            ),
            (
                SessionEvent::ToolCall {
                    id: String::new(),
                    name: String::new(),
                    args: serde_json::Value::Null,
                    include_in_llm: true,
                    timestamp: ts(),
                },
                "tool_call",
            ),
            (
                SessionEvent::ToolResult {
                    id: String::new(),
                    name: String::new(),
                    content: String::new(),
                    is_error: false,
                    display_range: None,
                    include_in_llm: true,
                    timestamp: ts(),
                },
                "tool_result",
            ),
            (
                SessionEvent::TurnError {
                    message: String::new(),
                    timestamp: ts(),
                },
                "turn_error",
            ),
            (
                SessionEvent::ModelChanged {
                    model: String::new(),
                    provider: String::new(),
                    timestamp: ts(),
                },
                "model_changed",
            ),
            (
                SessionEvent::ThinkingLevelChanged {
                    level: ThinkingLevel::Off,
                    timestamp: ts(),
                },
                "thinking_level_changed",
            ),
        ];

        for (ev, expected_tag) in cases {
            let json = serde_json::to_string(ev).unwrap();
            let v: serde_json::Value = serde_json::from_str(&json).unwrap();
            assert_eq!(
                v["type"].as_str(),
                Some(*expected_tag),
                "wrong type tag for {expected_tag}"
            );
        }
    }

    // ── CompactionTrigger ─────────────────────────────────────────────────────

    #[test]
    fn compaction_trigger_serializes_as_snake_case() {
        assert_eq!(
            serde_json::to_string(&CompactionTrigger::Threshold).unwrap(),
            "\"threshold\""
        );
        assert_eq!(
            serde_json::to_string(&CompactionTrigger::OverflowRetry).unwrap(),
            "\"overflow_retry\""
        );
    }
}
