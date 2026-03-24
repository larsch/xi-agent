use std::sync::Arc;

use futures_util::StreamExt;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::llm::{AssistantPhase, LlmEvent, LlmProvider, Message, ToolDefinition};

pub mod system_prompt;
pub mod tools;
pub mod types;

#[cfg(test)]
mod tests;

pub use system_prompt::build_system_prompt;
pub use types::{AgentEvent, AgentLoopConfig, ToolResult};

fn drain_steering_messages(
    steering_rx: &mut UnboundedReceiver<String>,
    messages: &mut Vec<Message>,
    tx: &UnboundedSender<AgentEvent>,
) -> bool {
    let mut consumed = false;
    while let Ok(text) = steering_rx.try_recv() {
        let _ = tx.send(AgentEvent::SteeringConsumed { text: text.clone() });
        messages.push(Message::user(text));
        consumed = true;
    }
    consumed
}

/// Run the agent loop: call the LLM, execute tool calls, repeat until the
/// model gives a final text answer.
///
/// All activity is reported back to `App` via `AgentEvent`s sent on `tx`.
pub async fn run_agent_loop(
    mut messages: Vec<Message>,
    config: AgentLoopConfig,
    provider: Arc<dyn LlmProvider>,
    tx: UnboundedSender<AgentEvent>,
    mut steering_rx: UnboundedReceiver<String>,
) {
    // Build the tool definitions once from the registry.
    let tool_defs: Vec<ToolDefinition> = config
        .tools
        .values()
        .map(|t| ToolDefinition {
            name: t.name().to_string(),
            description: t.description().to_string(),
            parameters: t.parameters_schema(),
        })
        .collect();

    loop {
        // Insert queued steering messages before the next assistant turn.
        let _ = drain_steering_messages(&mut steering_rx, &mut messages, &tx);

        // ── Stream the assistant response ─────────────────────────────────────
        let mut stream = provider.stream_chat_with_tools(messages.clone(), tool_defs.clone());

        // Accumulate text/thinking for the assistant message we'll push to
        // the display and to `messages` for history.
        let mut assistant_text = String::new();
        let mut assistant_thinking: Option<String> = None;
        let mut assistant_phase = AssistantPhase::Unknown;
        let mut pending_tool_calls: Vec<(String, String, serde_json::Value)> = Vec::new(); // (id, name, args)

        let mut stream_error: Option<crate::llm::ProviderError> = None;

        while let Some(ev) = stream.next().await {
            match ev {
                LlmEvent::Token { text, phase } => {
                    let _ = tx.send(AgentEvent::TextToken {
                        text: text.clone(),
                        phase,
                    });
                    assistant_text.push_str(&text);
                    if phase != AssistantPhase::Unknown {
                        assistant_phase = phase;
                    }
                }
                LlmEvent::ThinkingToken(t) => {
                    let _ = tx.send(AgentEvent::ThinkingToken(t.clone()));
                    assistant_thinking
                        .get_or_insert_with(String::new)
                        .push_str(&t);
                }
                LlmEvent::Usage(usage) => {
                    let _ = tx.send(AgentEvent::Usage(usage));
                }
                LlmEvent::ToolIntentStart => {
                    let _ = tx.send(AgentEvent::ToolIntentStart);
                    assistant_phase = AssistantPhase::Provisional;
                }
                LlmEvent::ToolCall { id, name, args } => {
                    pending_tool_calls.push((id, name, args));
                }
                LlmEvent::Done => break,
                LlmEvent::Error(e) => {
                    stream_error = Some(e);
                    break;
                }
            }
        }

        if let Some(e) = stream_error {
            let _ = tx.send(AgentEvent::Error(e));
            return;
        }

        // Append assistant message to history (even if empty when tools were called).
        let mut asst_msg = Message::assistant(&assistant_text);
        asst_msg.thinking = assistant_thinking;
        asst_msg.assistant_phase = Some(if pending_tool_calls.is_empty() {
            AssistantPhase::Final
        } else if assistant_phase == AssistantPhase::Unknown {
            AssistantPhase::Provisional
        } else {
            assistant_phase
        });
        messages.push(asst_msg);

        // ── No tool calls → final answer ──────────────────────────────────────
        if pending_tool_calls.is_empty() {
            let _ = tx.send(AgentEvent::TurnEnd);
            let _ = tx.send(AgentEvent::Done);
            return;
        }

        // ── Execute tool calls sequentially ───────────────────────────────────
        let mut stop_after_turn_for_steering = false;
        for (idx, (id, name, args)) in pending_tool_calls.iter().cloned().enumerate() {
            let _ = tx.send(AgentEvent::ToolCallStart {
                id: id.clone(),
                name: name.clone(),
                args: args.clone(),
            });

            // before_tool_call hook
            let blocked = config
                .before_tool_call
                .as_ref()
                .map(|f| !f(&name, &args))
                .unwrap_or(false);

            let mut result = if blocked {
                ToolResult::err(format!("Tool call '{name}' was blocked"))
            } else {
                match config.tools.get(&name) {
                    Some(tool) => tool.execute(args.clone()).await,
                    None => ToolResult::err(format!("Unknown tool: '{name}'")),
                }
            };

            // after_tool_call hook
            if let Some(f) = &config.after_tool_call
                && let Some(override_result) = f(&name, &result)
            {
                result = override_result;
            }

            let _ = tx.send(AgentEvent::ToolCallEnd {
                id: id.clone(),
                name: name.clone(),
                result: result.clone(),
            });

            // Append tool call + result to history for the next LLM turn.
            messages.push(Message::tool_call(&id, &name, args));
            messages.push(Message::tool_result(&id, &result.content, result.is_error));

            // If steering arrived, consume it now and skip remaining tool calls.
            if drain_steering_messages(&mut steering_rx, &mut messages, &tx) {
                for (skip_id, skip_name, skip_args) in
                    pending_tool_calls.iter().skip(idx + 1).cloned()
                {
                    let _ = tx.send(AgentEvent::ToolCallStart {
                        id: skip_id.clone(),
                        name: skip_name.clone(),
                        args: skip_args.clone(),
                    });
                    let skipped = ToolResult::err("Skipped due to queued user message.");
                    let _ = tx.send(AgentEvent::ToolCallEnd {
                        id: skip_id.clone(),
                        name: skip_name.clone(),
                        result: skipped.clone(),
                    });
                    messages.push(Message::tool_call(&skip_id, &skip_name, skip_args));
                    messages.push(Message::tool_result(&skip_id, &skipped.content, true));
                }
                stop_after_turn_for_steering = true;
                break;
            }
        }

        let _ = tx.send(AgentEvent::TurnEnd);

        if stop_after_turn_for_steering {
            continue;
        }
    }
}
