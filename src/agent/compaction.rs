use std::collections::BTreeSet;
use std::sync::Arc;

use futures_util::StreamExt;

use crate::context_window::{context_window_for_model, scaled_token_budget};
use crate::llm::{AssistantPhase, LlmEvent, LlmProvider, Message};
use crate::projection::project_llm_messages;
use crate::session_event::{CompactionTrigger, SessionEvent};

const DEFAULT_CONTEXT_WINDOW: usize = 200_000;
const TOOL_RESULT_SNIPPET_LIMIT: usize = 4_000;

#[derive(Debug, Clone)]
pub struct CompactionOutcome {
    pub summary: String,
    pub trigger_reason: CompactionTrigger,
    pub context_window: usize,
    pub reserve_tokens: usize,
    pub keep_recent_tokens: usize,
    pub tokens_before: usize,
    pub tokens_after: usize,
    pub retained_event_count: usize,
    pub read_files: Vec<String>,
    pub modified_files: Vec<String>,
}

#[derive(Debug, Clone)]
struct Unit {
    events: Vec<SessionEvent>,
    messages: Vec<Message>,
}

#[derive(Debug, Clone)]
struct FileSets {
    read_files: Vec<String>,
    modified_files: Vec<String>,
}

pub fn estimate_message_tokens(msg: &Message) -> usize {
    let mut chars = 0usize;
    chars = chars.saturating_add(msg.content.chars().count());
    if let Some(thinking) = &msg.thinking {
        chars = chars.saturating_add(thinking.chars().count());
    }
    if let Some(name) = &msg.tool_name {
        chars = chars.saturating_add(name.chars().count());
    }
    if let Some(args) = &msg.tool_args {
        chars = chars.saturating_add(args.to_string().chars().count());
    }
    chars.div_ceil(4)
}

pub fn estimate_messages_tokens(messages: &[Message]) -> usize {
    messages.iter().map(estimate_message_tokens).sum()
}

pub fn estimate_session_tokens(events: &[SessionEvent]) -> usize {
    estimate_messages_tokens(&project_llm_messages(events))
}

pub fn is_context_overflow_error(err: &crate::llm::ProviderError) -> bool {
    let msg = err.message.to_ascii_lowercase();
    msg.contains("context window")
        || msg.contains("maximum context")
        || msg.contains("context length")
        || msg.contains("context limit")
        || msg.contains("too many tokens")
        || msg.contains("prompt is too long")
        || msg.contains("input is too long")
        || msg.contains("token limit exceeded")
        || msg.contains("context overflow")
}

pub fn context_window_and_budgets(model: &str) -> (usize, usize, usize) {
    let context_window = context_window_for_model(model).unwrap_or(DEFAULT_CONTEXT_WINDOW);
    let reserve_tokens = scaled_token_budget(context_window, 16_000, 16_000);
    let keep_recent_tokens = scaled_token_budget(context_window, 20_000, 20_000);
    (context_window, reserve_tokens, keep_recent_tokens)
}

pub async fn compact_events(
    provider: Arc<dyn LlmProvider>,
    events: &[SessionEvent],
    model: &str,
    trigger_reason: CompactionTrigger,
    user_instructions: Option<String>,
) -> Result<CompactionOutcome, crate::llm::ProviderError> {
    let (context_window, reserve_tokens, keep_recent_tokens) = context_window_and_budgets(model);
    let tokens_before = estimate_session_tokens(events);

    let (previous_summary, previous_files, tail_events) = split_at_latest_compaction(events);
    let units = build_units(&tail_events);
    if units.is_empty() && previous_summary.is_none() {
        return Err(crate::llm::ProviderError::other(
            "compaction",
            "Nothing to compact.",
        ));
    }

    let cut = choose_cut_index(&units, keep_recent_tokens);

    let summarized_units = &units[..cut];
    let summarized_events = summarized_units
        .iter()
        .flat_map(|u| u.events.iter().cloned())
        .collect::<Vec<_>>();
    let retained_event_count = units[cut..].iter().map(|u| u.events.len()).sum::<usize>();
    let kept_messages = units[cut..]
        .iter()
        .flat_map(|u| u.messages.iter().cloned())
        .collect::<Vec<_>>();

    let new_files = derive_files_from_events(&summarized_events);
    let merged_files = merge_file_sets(previous_files, new_files);

    let summary_prompt = build_summary_prompt(
        previous_summary.as_deref(),
        &summarized_events,
        user_instructions.as_deref(),
    );
    let summary_core = collect_summary(provider, summary_prompt).await?;
    let summary = append_file_sections(summary_core.trim(), &merged_files);

    let mut after_messages = Vec::new();
    after_messages.push(Message::user(summary.clone()));
    after_messages.extend(kept_messages);
    let tokens_after = estimate_messages_tokens(&after_messages);

    Ok(CompactionOutcome {
        summary,
        trigger_reason,
        context_window,
        reserve_tokens,
        keep_recent_tokens,
        tokens_before,
        tokens_after,
        retained_event_count,
        read_files: merged_files.read_files,
        modified_files: merged_files.modified_files,
    })
}

fn split_at_latest_compaction(
    events: &[SessionEvent],
) -> (Option<String>, FileSets, Vec<SessionEvent>) {
    if let Some(idx) = events
        .iter()
        .rposition(|e| matches!(e, SessionEvent::CompactionSummary { .. }))
        && let SessionEvent::CompactionSummary {
            summary,
            read_files,
            modified_files,
            ..
        } = &events[idx]
    {
        return (
            Some(summary.clone()),
            FileSets {
                read_files: read_files.clone(),
                modified_files: modified_files.clone(),
            },
            events[idx + 1..].to_vec(),
        );
    }

    (
        None,
        FileSets {
            read_files: vec![],
            modified_files: vec![],
        },
        events.to_vec(),
    )
}

fn build_units(events: &[SessionEvent]) -> Vec<Unit> {
    let mut units = Vec::new();
    let mut idx = 0usize;
    while idx < events.len() {
        match &events[idx] {
            SessionEvent::UserMessage { content, .. } => {
                units.push(Unit {
                    events: vec![events[idx].clone()],
                    messages: vec![Message::user(content.clone())],
                });
                idx += 1;
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
                units.push(Unit {
                    events: vec![events[idx].clone()],
                    messages: vec![msg],
                });
                idx += 1;
            }
            SessionEvent::ToolCall { id, name, args, .. } => {
                if let Some(SessionEvent::ToolResult {
                    id: result_id,
                    content,
                    is_error,
                    display_range,
                    ..
                }) = events.get(idx + 1)
                    && result_id == id
                {
                    let mut result_msg =
                        Message::tool_result(id.clone(), content.clone(), *is_error);
                    result_msg.display_range = display_range.clone();
                    units.push(Unit {
                        events: vec![events[idx].clone(), events[idx + 1].clone()],
                        messages: vec![
                            Message::tool_call(id.clone(), name.clone(), args.clone()),
                            result_msg,
                        ],
                    });
                    idx += 2;
                    continue;
                }

                units.push(Unit {
                    events: vec![events[idx].clone()],
                    messages: vec![Message::tool_call(id.clone(), name.clone(), args.clone())],
                });
                idx += 1;
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
                units.push(Unit {
                    events: vec![events[idx].clone()],
                    messages: vec![msg],
                });
                idx += 1;
            }
            SessionEvent::TurnError { .. }
            | SessionEvent::CompactionSummary { .. }
            | SessionEvent::ModelChanged { .. }
            | SessionEvent::ThinkingLevelChanged { .. } => {
                idx += 1;
            }
        }
    }
    units
}

fn choose_cut_index(units: &[Unit], keep_recent_tokens: usize) -> usize {
    let mut kept_tokens = 0usize;
    let mut cut = units.len();

    for (idx, unit) in units.iter().enumerate().rev() {
        let unit_tokens = estimate_messages_tokens(&unit.messages);
        if kept_tokens.saturating_add(unit_tokens) <= keep_recent_tokens {
            kept_tokens = kept_tokens.saturating_add(unit_tokens);
            cut = idx;
        } else {
            break;
        }
    }

    cut
}

fn derive_files_from_events(events: &[SessionEvent]) -> FileSets {
    let mut read_files = BTreeSet::new();
    let mut modified_files = BTreeSet::new();

    for ev in events {
        let SessionEvent::ToolCall { name, args, .. } = ev else {
            continue;
        };
        let Some(path) = args.get("path").and_then(|v| v.as_str()) else {
            continue;
        };
        match name.as_str() {
            "read_file" => {
                read_files.insert(path.to_string());
            }
            "write_file" | "edit_file" => {
                modified_files.insert(path.to_string());
            }
            _ => {}
        }
    }

    FileSets {
        read_files: read_files.into_iter().collect(),
        modified_files: modified_files.into_iter().collect(),
    }
}

fn merge_file_sets(previous: FileSets, new: FileSets) -> FileSets {
    let mut read_files = previous.read_files.into_iter().collect::<BTreeSet<_>>();
    read_files.extend(new.read_files);

    let mut modified_files = previous.modified_files.into_iter().collect::<BTreeSet<_>>();
    modified_files.extend(new.modified_files);

    FileSets {
        read_files: read_files.into_iter().collect(),
        modified_files: modified_files.into_iter().collect(),
    }
}

fn build_summary_prompt(
    previous_summary: Option<&str>,
    summarized_events: &[SessionEvent],
    user_instructions: Option<&str>,
) -> Vec<Message> {
    let mut prompt = String::from("Update the session compaction summary.\n\n");
    prompt.push_str("Return only the summary body using this exact structure:\n\n");
    prompt.push_str("## Goal\n[What the user is trying to accomplish]\n\n");
    prompt.push_str("## Constraints & Preferences\n- [Requirements mentioned by user]\n\n");
    prompt.push_str(
        "## Progress\n### Done\n- [x] [Completed tasks]\n\n### In Progress\n- [ ] [Current work]\n\n### Blocked\n- [Issues, if any]\n\n");
    prompt.push_str("## Key Decisions\n- **[Decision]**: [Rationale]\n\n");
    prompt.push_str("## Next Steps\n1. [What should happen next]\n\n");
    prompt.push_str("## Critical Context\n- [Data needed to continue]\n\n");
    prompt.push_str(
        "Keep the summary concise. Preserve exact file paths, function names, and relevant error messages. Do not include <read-files> or <modified-files> sections; they will be appended separately.\n\n",
    );

    if let Some(instructions) = user_instructions.filter(|s| !s.trim().is_empty()) {
        prompt.push_str("User compaction instructions:\n");
        prompt.push_str(instructions.trim());
        prompt.push_str("\n\n");
    }

    prompt.push_str("Previous compaction summary:\n");
    prompt.push_str(previous_summary.unwrap_or("(none)"));
    prompt.push_str("\n\nHistory to summarize:\n\n");
    prompt.push_str(&serialize_events_for_summary(summarized_events));

    vec![
        Message::system(
            "You create compact, structured continuation summaries for xi-agent coding sessions.",
        ),
        Message::user(prompt),
    ]
}

fn serialize_events_for_summary(events: &[SessionEvent]) -> String {
    let mut out = String::new();
    for ev in events {
        match ev {
            SessionEvent::UserMessage { content, .. } => {
                out.push_str("[user]\n");
                out.push_str(content);
                out.push_str("\n\n");
            }
            SessionEvent::AssistantMessage {
                content,
                thinking,
                phase,
                ..
            } => {
                out.push_str("[assistant");
                if *phase == AssistantPhase::Provisional {
                    out.push_str(" provisional");
                }
                out.push_str("]\n");
                if let Some(thinking) = thinking {
                    out.push_str("<thinking>\n");
                    out.push_str(thinking);
                    out.push_str("\n</thinking>\n");
                }
                out.push_str(content);
                out.push_str("\n\n");
            }
            SessionEvent::ToolCall { name, args, .. } => {
                out.push_str("[tool-call ");
                out.push_str(name);
                out.push_str("]\n");
                out.push_str(&args.to_string());
                out.push_str("\n\n");
            }
            SessionEvent::ToolResult { name, content, .. } => {
                out.push_str("[tool-result ");
                out.push_str(name);
                out.push_str("]\n");
                if content.chars().count() > TOOL_RESULT_SNIPPET_LIMIT {
                    let truncated = content
                        .chars()
                        .take(TOOL_RESULT_SNIPPET_LIMIT)
                        .collect::<String>();
                    out.push_str(&truncated);
                    out.push_str("\n[truncated]\n\n");
                } else {
                    out.push_str(content);
                    out.push_str("\n\n");
                }
            }
            SessionEvent::TurnError { message, .. } => {
                out.push_str("[turn-error]\n");
                out.push_str(message);
                out.push_str("\n\n");
            }
            SessionEvent::ModelChanged {
                model, provider, ..
            } => {
                out.push_str("[model-changed]\n");
                out.push_str(provider);
                out.push_str(" / ");
                out.push_str(model);
                out.push_str("\n\n");
            }
            SessionEvent::ThinkingLevelChanged { level, .. } => {
                out.push_str("[thinking-level]\n");
                out.push_str(level.as_str());
                out.push_str("\n\n");
            }
            SessionEvent::CompactionSummary { .. } => {}
        }
    }
    out
}

async fn collect_summary(
    provider: Arc<dyn LlmProvider>,
    messages: Vec<Message>,
) -> Result<String, crate::llm::ProviderError> {
    let mut stream = provider.stream_chat(messages);
    let mut summary = String::new();

    while let Some(ev) = stream.next().await {
        match ev {
            LlmEvent::Token { text, .. } => summary.push_str(&text),
            LlmEvent::ThinkingToken(_) => {}
            LlmEvent::Usage(_) => {}
            LlmEvent::ToolCallStart { .. } => {}
            LlmEvent::ToolCallArgsDelta { .. } => {}
            LlmEvent::ToolCall { .. } => {}
            LlmEvent::StatusUpdate(_) => {}
            LlmEvent::Done => break,
            LlmEvent::Error(e) => return Err(e),
        }
    }

    let trimmed = summary.trim();
    if trimmed.is_empty() {
        Err(crate::llm::ProviderError::other(
            "compaction",
            "Compaction summary was empty.",
        ))
    } else {
        Ok(trimmed.to_string())
    }
}

fn append_file_sections(summary: &str, files: &FileSets) -> String {
    let mut out = summary.trim().to_string();
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push('\n');
    out.push_str("<read-files>\n");
    for path in &files.read_files {
        out.push_str(path);
        out.push('\n');
    }
    out.push_str("</read-files>\n\n");
    out.push_str("<modified-files>\n");
    for path in &files.modified_files {
        out.push_str(path);
        out.push('\n');
    }
    out.push_str("</modified-files>");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{DisplayRange, UsageStats};
    use crate::session_event::CompactionTrigger;

    fn user(content: &str) -> SessionEvent {
        SessionEvent::UserMessage {
            content: content.to_string(),
            timestamp: 1,
        }
    }

    fn assistant(content: &str) -> SessionEvent {
        SessionEvent::AssistantMessage {
            content: content.to_string(),
            thinking: None,
            phase: AssistantPhase::Final,
            usage: Some(UsageStats::default()),
            timestamp: 2,
        }
    }

    #[test]
    fn estimates_message_tokens_from_all_payload_fields() {
        let mut msg = Message::assistant("abcd");
        msg.thinking = Some("1234".to_string());
        msg.tool_name = Some("tool".to_string());
        msg.tool_args = Some(serde_json::json!({"path":"src/main.rs"}));
        assert!(estimate_message_tokens(&msg) >= 4);
    }

    #[test]
    fn cut_selection_keeps_tool_call_and_result_together() {
        let units = build_units(&[
            user("u1"),
            assistant("a1"),
            SessionEvent::ToolCall {
                id: "c1".to_string(),
                name: "read_file".to_string(),
                args: serde_json::json!({"path":"src/main.rs"}),
                timestamp: 3,
            },
            SessionEvent::ToolResult {
                id: "c1".to_string(),
                name: "read_file".to_string(),
                content: "content".to_string(),
                is_error: false,
                display_range: Some(DisplayRange {
                    first_line: 1,
                    last_line: 2,
                    total_lines: 2,
                }),
                timestamp: 4,
            },
        ]);
        assert_eq!(units.len(), 3);
        assert_eq!(units[2].events.len(), 2);
        assert_eq!(units[2].messages.len(), 2);
    }

    #[test]
    fn cut_selection_can_compact_everything_when_budget_is_tiny() {
        let units = build_units(&[user("u1"), assistant("a1"), user("u2"), assistant("a2")]);

        // Force a tiny keep budget so even the newest unit cannot fit.
        let cut = choose_cut_index(&units, 0);
        assert_eq!(cut, units.len());
    }

    #[test]
    fn derives_file_sets_from_tool_calls() {
        let files = derive_files_from_events(&[
            SessionEvent::ToolCall {
                id: "1".to_string(),
                name: "read_file".to_string(),
                args: serde_json::json!({"path":"src/app.rs"}),
                timestamp: 1,
            },
            SessionEvent::ToolCall {
                id: "2".to_string(),
                name: "edit_file".to_string(),
                args: serde_json::json!({"path":"src/main.rs"}),
                timestamp: 2,
            },
        ]);
        assert_eq!(files.read_files, vec!["src/app.rs".to_string()]);
        assert_eq!(files.modified_files, vec!["src/main.rs".to_string()]);
    }

    #[test]
    fn appends_file_sections() {
        let summary = append_file_sections(
            "## Goal\nDo work",
            &FileSets {
                read_files: vec!["a".to_string()],
                modified_files: vec!["b".to_string()],
            },
        );
        assert!(summary.contains("<read-files>\na\n</read-files>"));
        assert!(summary.contains("<modified-files>\nb\n</modified-files>"));
    }

    #[test]
    fn splits_after_latest_compaction_boundary() {
        let (summary, files, tail) = split_at_latest_compaction(&[
            user("old"),
            SessionEvent::CompactionSummary {
                summary: "older".to_string(),
                trigger_reason: CompactionTrigger::Threshold,
                context_window: 10,
                reserve_tokens: 1,
                keep_recent_tokens: 1,
                tokens_before: 10,
                tokens_after: 2,
                retained_event_count: None,
                read_files: vec!["x".to_string()],
                modified_files: vec!["y".to_string()],
                timestamp: 3,
            },
            user("new"),
        ]);
        assert_eq!(summary.as_deref(), Some("older"));
        assert_eq!(files.read_files, vec!["x".to_string()]);
        assert_eq!(tail.len(), 1);
    }

    #[test]
    fn detects_context_overflow_messages() {
        let err = crate::llm::ProviderError::other("x", "maximum context length exceeded");
        assert!(is_context_overflow_error(&err));
    }
}
