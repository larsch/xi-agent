use std::sync::Arc;

use futures_util::StreamExt;

use crate::agent::compaction;
use crate::agent::events::{AgentEventSink, send_agent_event};
use crate::agent::loop_support::HookDispatchContext;
use crate::agent::types::AgentEvent;
use crate::hooks::{
    HookPoint, empty_payload, ipc_status_update_payload, ipc_tool_intent_payload, maybe_run_hook,
};
use crate::llm::{
    AssistantPhase, LlmEvent, LlmProvider, LlmRequestContext, Message, ToolDefinition, UsageStats,
};

// ── TurnOutcome ───────────────────────────────────────────────────────────────

/// The result of one LLM streaming turn.
#[derive(Debug)]
pub(crate) enum TurnOutcome {
    /// The model produced a final answer with no tool calls.
    FinalAnswer {
        text: String,
        thinking: Option<String>,
        phase: AssistantPhase,
        usage: Option<UsageStats>,
    },
    /// The model produced tool calls that must be executed.
    ToolCalls {
        text: String,
        thinking: Option<String>,
        phase: AssistantPhase,
        usage: Option<UsageStats>,
        calls: Vec<(String, String, serde_json::Value)>,
    },
    /// The stream failed with a context-overflow error eligible for retry.
    ContextOverflow(crate::llm::ProviderError),
    /// The stream failed with a non-recoverable error.
    Error(crate::llm::ProviderError),
    /// The model indicated a tool call was coming but no call arrived
    /// (e.g. truncated by max_tokens).
    ToolIntentWithNoCall,
}

// ── stream_assistant_turn ─────────────────────────────────────────────────────

/// Drive one LLM streaming turn and return a typed [`TurnOutcome`].
///
/// Streams all events from the provider, accumulates text/thinking/tool-calls,
/// and returns the appropriate outcome variant. No session state is mutated.
pub(crate) async fn stream_assistant_turn(
    provider: Arc<dyn LlmProvider>,
    messages: Vec<Message>,
    tool_defs: Vec<ToolDefinition>,
    sink: &dyn AgentEventSink,
    overflow_retry_remaining: usize,
    hook_ctx: HookDispatchContext<'_>,
) -> TurnOutcome {
    // Build a lookup from tool name → streaming_field for intent events.
    let streaming_fields: std::collections::HashMap<String, Option<String>> = tool_defs
        .iter()
        .map(|t| (t.name.clone(), t.streaming_field.clone()))
        .collect();

    let mut stream = provider.stream_chat_with_tools(
        messages,
        tool_defs,
        LlmRequestContext {
            prompt_cache_key: Some(hook_ctx.session_id.to_string()),
        },
    );

    let mut assistant_text = String::new();
    let mut assistant_thinking: Option<String> = None;
    let mut assistant_phase = AssistantPhase::Unknown;
    let mut pending_tool_calls: Vec<(String, String, serde_json::Value)> = Vec::new();
    let mut tool_intent_seen = false;
    let mut latest_usage = None;
    let mut first_thinking_token = true;
    let mut first_text_token = true;

    while let Some(ev) = stream.next().await {
        match ev {
            LlmEvent::Token { text, phase } => {
                if first_text_token {
                    first_text_token = false;
                    hook_ctx.hook_ipc.publish(
                        hook_ctx.session_id,
                        HookPoint::OnFirstTextToken,
                        None,
                        empty_payload(),
                    );
                    maybe_run_hook(
                        hook_ctx.hooks,
                        HookPoint::OnFirstTextToken,
                        hook_ctx.session_id,
                        None,
                        None,
                    )
                    .await;
                }
                send_agent_event(
                    sink,
                    AgentEvent::TextToken {
                        text: text.clone(),
                        phase,
                    },
                );
                assistant_text.push_str(&text);
                if phase != AssistantPhase::Unknown {
                    assistant_phase = phase;
                }
            }
            LlmEvent::ThinkingToken(t) => {
                if first_thinking_token {
                    first_thinking_token = false;
                    hook_ctx.hook_ipc.publish(
                        hook_ctx.session_id,
                        HookPoint::OnFirstThinkingToken,
                        None,
                        empty_payload(),
                    );
                    maybe_run_hook(
                        hook_ctx.hooks,
                        HookPoint::OnFirstThinkingToken,
                        hook_ctx.session_id,
                        None,
                        None,
                    )
                    .await;
                }
                send_agent_event(sink, AgentEvent::ThinkingToken(t.clone()));
                assistant_thinking
                    .get_or_insert_with(String::new)
                    .push_str(&t);
            }
            LlmEvent::Usage(usage) => {
                latest_usage = Some(usage);
                send_agent_event(sink, AgentEvent::Usage(usage));
            }
            LlmEvent::ToolCallStart { id, name } => {
                let streaming_field = streaming_fields.get(&name).and_then(|f| f.clone());
                send_agent_event(
                    sink,
                    AgentEvent::ToolCallIntent {
                        id,
                        name: name.clone(),
                        streaming_field,
                    },
                );
                assistant_phase = AssistantPhase::Provisional;
                tool_intent_seen = true;
                hook_ctx.hook_ipc.publish(
                    hook_ctx.session_id,
                    HookPoint::OnToolIntent,
                    Some(&name),
                    ipc_tool_intent_payload(&name),
                );
                crate::hooks::maybe_run_hook(
                    hook_ctx.hooks,
                    HookPoint::OnToolIntent,
                    hook_ctx.session_id,
                    Some(crate::hooks::tool_json(&name, &serde_json::Value::Null)),
                    Some(&name),
                )
                .await;
            }
            LlmEvent::ToolCallArgsDelta { id, partial_json } => {
                send_agent_event(sink, AgentEvent::ToolCallArgsDelta { id, partial_json });
            }
            LlmEvent::ToolCall { id, name, args } => {
                pending_tool_calls.push((id, name, args));
            }
            LlmEvent::Done => break,
            LlmEvent::Error(e) => {
                if overflow_retry_remaining > 0 && compaction::is_context_overflow_error(&e) {
                    return TurnOutcome::ContextOverflow(e);
                }
                return TurnOutcome::Error(e);
            }
            LlmEvent::StatusUpdate(msg) => {
                send_agent_event(sink, AgentEvent::StatusUpdate(msg.clone()));
                hook_ctx.hook_ipc.publish(
                    hook_ctx.session_id,
                    HookPoint::OnStatusUpdate,
                    None,
                    ipc_status_update_payload(&msg),
                );
                maybe_run_hook(
                    hook_ctx.hooks,
                    HookPoint::OnStatusUpdate,
                    hook_ctx.session_id,
                    Some(serde_json::json!({"status": msg})),
                    None,
                )
                .await;
            }
        }
    }

    if tool_intent_seen && pending_tool_calls.is_empty() {
        return TurnOutcome::ToolIntentWithNoCall;
    }

    let final_phase = if pending_tool_calls.is_empty() {
        AssistantPhase::Final
    } else if assistant_phase == AssistantPhase::Unknown {
        AssistantPhase::Provisional
    } else {
        assistant_phase
    };

    if pending_tool_calls.is_empty() {
        TurnOutcome::FinalAnswer {
            text: assistant_text,
            thinking: assistant_thinking,
            phase: final_phase,
            usage: latest_usage,
        }
    } else {
        TurnOutcome::ToolCalls {
            text: assistant_text,
            thinking: assistant_thinking,
            phase: final_phase,
            usage: latest_usage,
            calls: pending_tool_calls,
        }
    }
}
