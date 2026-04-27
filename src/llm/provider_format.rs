//! Canonical per-protocol message serialization for LLM providers.
//!
//! Each function converts a slice of tau [`Message`]s into the wire format
//! required by one protocol family.  All functions apply
//! [`normalize_tool_name`] consistently so that emoji shorthand tool names
//! are resolved before being sent to any provider.
//!
//! Protocol families:
//! - [`to_openai_wire`]   — OpenAI Chat Completions
//! - [`to_anthropic_wire`] — Anthropic Messages API
//! - [`to_gemini_wire`]   — Google Gemini `contents` array
//! - [`to_codex_wire`]    — OpenAI Responses API (Codex / GPT-5 style)
//! - [`to_ollama_wire`]   — Ollama `/api/chat` (OpenAI-like with `thinking` and object `arguments`)

use super::common::normalize_tool_name;
use super::{ImageData, Message, Role};

// ── Shared helpers ────────────────────────────────────────────────────────────

/// Build the `content` value for an OpenAI-style tool result message.
///
/// When the message carries image data, the content is a two-element array:
/// a text block with the placeholder and an `image_url` block using a
/// `data:` URI.  Otherwise the content is the plain text string.
fn openai_tool_result_content(tr: &Message) -> serde_json::Value {
    if let Some(ImageData { base64, mime_type }) = &tr.image_data {
        serde_json::json!([
            { "type": "text", "text": &tr.content },
            { "type": "image_url", "image_url": { "url": format!("data:{mime_type};base64,{base64}") } }
        ])
    } else {
        serde_json::Value::String(tr.content.clone())
    }
}

/// Build the `content` array for an Anthropic tool result message.
///
/// For image results the content carries both a `text` block (placeholder)
/// and an `image` block with base64 data.  For text results it is a single
/// `text` block.
fn anthropic_tool_result_content(tr: &Message) -> serde_json::Value {
    if let Some(ImageData { base64, mime_type }) = &tr.image_data {
        serde_json::json!([
            { "type": "text", "text": &tr.content },
            { "type": "image", "source": { "type": "base64", "media_type": mime_type, "data": base64 } }
        ])
    } else {
        serde_json::Value::String(tr.content.clone())
    }
}

///
/// The OpenAI API requires that tool calls and their accompanying text live in
/// *one* assistant message, followed by one `"role":"tool"` message per result.
/// Tau stores them as separate `Role::Assistant` + `Role::ToolCall` +
/// `Role::ToolResult` messages, interleaved when there are multiple calls in a
/// single turn.  This function:
///
/// 1. Merges a `Role::Assistant` message with any immediately following
///    `Role::ToolCall` messages into a single assistant message that carries
///    both `content` and `tool_calls`.
/// 2. Collects the corresponding `Role::ToolResult` messages and emits them
///    after the merged assistant message, preserving order.
/// 3. Skips empty assistant messages that have no content and no tool calls.
pub fn to_openai_wire(messages: &[Message]) -> Vec<serde_json::Value> {
    let mut result: Vec<serde_json::Value> = Vec::new();
    let mut i = 0;

    while i < messages.len() {
        let msg = &messages[i];

        match msg.role {
            Role::Assistant => {
                let mut j = i + 1;
                let mut tool_calls: Vec<serde_json::Value> = Vec::new();
                let mut tool_results: Vec<serde_json::Value> = Vec::new();

                while j < messages.len() && messages[j].role == Role::ToolCall {
                    let tc = &messages[j];
                    let call_idx = tool_calls.len();
                    tool_calls.push(serde_json::json!({
                        "id": tc.tool_call_id.clone().unwrap_or_else(|| format!("call_{call_idx}")),
                        "type": "function",
                        "function": {
                            "name": normalize_tool_name(tc.tool_name.as_deref().unwrap_or_default()).to_string(),
                            "arguments": tc.tool_args.as_ref().map(|v| v.to_string()).unwrap_or_else(|| "{}".to_string()),
                        }
                    }));
                    j += 1;

                    if j < messages.len() && messages[j].role == Role::ToolResult {
                        let tr = &messages[j];
                        let tr_content = openai_tool_result_content(tr);
                        tool_results.push(serde_json::json!({
                            "role": "tool",
                            "content": tr_content,
                            "tool_call_id": tr.tool_call_id,
                        }));
                        j += 1;
                    }
                }

                let content = if msg.content.is_empty() {
                    None
                } else {
                    Some(&msg.content)
                };
                let tool_calls_opt = if tool_calls.is_empty() {
                    None
                } else {
                    Some(&tool_calls)
                };

                if content.is_some() || tool_calls_opt.is_some() {
                    result.push(serde_json::json!({
                        "role": "assistant",
                        "content": content,
                        "tool_calls": tool_calls_opt,
                    }));
                    result.extend(tool_results);
                }

                i = j;
            }

            Role::ToolCall => {
                let tc = msg;
                result.push(serde_json::json!({
                    "role": "assistant",
                    "content": serde_json::Value::Null,
                    "tool_calls": [{
                        "id": tc.tool_call_id.clone().unwrap_or_else(|| "call_0".to_string()),
                        "type": "function",
                        "function": {
                            "name": normalize_tool_name(tc.tool_name.as_deref().unwrap_or_default()).to_string(),
                            "arguments": tc.tool_args.as_ref().map(|v| v.to_string()).unwrap_or_else(|| "{}".to_string()),
                        }
                    }],
                }));
                i += 1;
            }

            Role::ToolResult => {
                let tr = msg;
                let tr_content = openai_tool_result_content(tr);
                result.push(serde_json::json!({
                    "role": "tool",
                    "content": tr_content,
                    "tool_call_id": tr.tool_call_id,
                }));
                i += 1;
            }

            Role::System => {
                result.push(serde_json::json!({
                    "role": "system",
                    "content": msg.content,
                }));
                i += 1;
            }

            Role::User => {
                result.push(serde_json::json!({
                    "role": "user",
                    "content": msg.content,
                }));
                i += 1;
            }
        }
    }

    result
}

// ── Anthropic Messages API ────────────────────────────────────────────────────

/// Convert a tau `Message` history to the Anthropic Messages API wire format.
///
/// System messages are skipped here (they must be extracted separately and
/// passed as the top-level `system` field in the request body).
/// Tool calls are grouped with their parent assistant turn into a single
/// `content` array entry; tool results are emitted as a `"role":"user"` block.
pub fn to_anthropic_wire(messages: &[Message]) -> Vec<serde_json::Value> {
    let mut result: Vec<serde_json::Value> = Vec::new();
    let mut i = 0;

    while i < messages.len() {
        let msg = &messages[i];

        match msg.role {
            Role::System => {
                i += 1;
            }

            Role::User => {
                result.push(serde_json::json!({
                    "role": "user",
                    "content": msg.content,
                }));
                i += 1;
            }

            Role::Assistant => {
                let mut content: Vec<serde_json::Value> = Vec::new();

                if !msg.content.is_empty() {
                    content.push(serde_json::json!({
                        "type": "text",
                        "text": msg.content,
                    }));
                }

                i += 1;

                let mut tool_results: Vec<serde_json::Value> = Vec::new();
                while i < messages.len() && messages[i].role == Role::ToolCall {
                    let tc = &messages[i];
                    content.push(serde_json::json!({
                        "type": "tool_use",
                        "id": tc.tool_call_id.as_deref().unwrap_or("call_0"),
                        "name": normalize_tool_name(tc.tool_name.as_deref().unwrap_or("")),
                        "input": tc.tool_args.clone().unwrap_or_default(),
                    }));
                    i += 1;

                    if i < messages.len() && messages[i].role == Role::ToolResult {
                        let tr = &messages[i];
                        let tr_content = anthropic_tool_result_content(tr);
                        tool_results.push(serde_json::json!({
                            "type": "tool_result",
                            "tool_use_id": tr.tool_call_id.as_deref().unwrap_or("call_0"),
                            "content": tr_content,
                            "is_error": tr.is_error,
                        }));
                        i += 1;
                    }
                }

                if content.is_empty() {
                    continue;
                }

                result.push(serde_json::json!({
                    "role": "assistant",
                    "content": content,
                }));

                if !tool_results.is_empty() {
                    result.push(serde_json::json!({
                        "role": "user",
                        "content": tool_results,
                    }));
                }
            }

            Role::ToolCall => {
                let tc = msg;
                result.push(serde_json::json!({
                    "role": "assistant",
                    "content": [{
                        "type": "tool_use",
                        "id": tc.tool_call_id.as_deref().unwrap_or("call_0"),
                        "name": normalize_tool_name(tc.tool_name.as_deref().unwrap_or("")),
                        "input": tc.tool_args.clone().unwrap_or_default(),
                    }],
                }));
                i += 1;
            }

            Role::ToolResult => {
                let tr = msg;
                let tr_content = anthropic_tool_result_content(tr);
                result.push(serde_json::json!({
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": tr.tool_call_id.as_deref().unwrap_or("call_0"),
                        "content": tr_content,
                        "is_error": tr.is_error,
                    }],
                }));
                i += 1;
            }
        }
    }

    result
}

// ── Google Gemini ─────────────────────────────────────────────────────────────

/// Convert a tau `Message` history to the Gemini `contents` array wire format.
///
/// System messages are skipped (passed separately as `systemInstruction`).
/// Tool calls use `functionCall` parts; tool results use `functionResponse`
/// parts.  A side-table maps tool-call IDs to names so that `ToolResult`
/// messages can include the correct `name` field even when `tool_name` is
/// absent on the result message.
pub fn to_gemini_wire(messages: &[Message]) -> Vec<serde_json::Value> {
    let mut contents = Vec::new();
    let mut tool_names_by_id: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();

    for msg in messages {
        match msg.role {
            Role::System => {}
            Role::User => {
                contents.push(serde_json::json!({
                    "role": "user",
                    "parts": [{ "text": msg.content }],
                }));
            }
            Role::Assistant => {
                if msg.content.trim().is_empty() {
                    continue;
                }
                contents.push(serde_json::json!({
                    "role": "model",
                    "parts": [{ "text": msg.content }],
                }));
            }
            Role::ToolCall => {
                let name =
                    normalize_tool_name(msg.tool_name.as_deref().unwrap_or_default()).to_string();
                let id = msg
                    .tool_call_id
                    .clone()
                    .unwrap_or_else(|| "call_0".to_string());
                let args = msg
                    .tool_args
                    .clone()
                    .unwrap_or_else(|| serde_json::json!({}));
                tool_names_by_id.insert(id.clone(), name.clone());
                contents.push(serde_json::json!({
                    "role": "model",
                    "parts": [{
                        "functionCall": {
                            "name": name,
                            "id": id,
                            "args": args,
                        }
                    }],
                }));
            }
            Role::ToolResult => {
                let tool_call_id = msg
                    .tool_call_id
                    .clone()
                    .unwrap_or_else(|| "call_0".to_string());
                let tool_name = msg
                    .tool_name
                    .clone()
                    .or_else(|| tool_names_by_id.get(&tool_call_id).cloned())
                    .unwrap_or_else(|| "tool".to_string());
                contents.push(serde_json::json!({
                    "role": "user",
                    "parts": [{
                        "functionResponse": {
                            "name": tool_name,
                            "id": tool_call_id,
                            "response": if msg.is_error {
                                serde_json::json!({"error": msg.content})
                            } else {
                                serde_json::json!({"output": msg.content})
                            },
                        }
                    }],
                }));
            }
        }
    }

    contents
}

// ── OpenAI Responses API (Codex / GPT-5) ─────────────────────────────────────

/// Convert a tau `Message` history to the OpenAI Responses API wire format.
///
/// System messages are skipped (passed as the `instructions` field).
/// This format differs from Chat Completions: assistant messages use
/// `"type":"message"` with `output_text` content; tool calls are
/// `"type":"function_call"` items; tool results are
/// `"type":"function_call_output"` items.
pub fn to_codex_wire(messages: &[Message]) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    let mut msg_idx = 0usize;

    for msg in messages {
        match msg.role {
            Role::System => {}

            Role::User => {
                out.push(serde_json::json!({
                    "role": "user",
                    "content": [{ "type": "input_text", "text": msg.content }]
                }));
                msg_idx += 1;
            }

            Role::Assistant => {
                out.push(serde_json::json!({
                    "type": "message",
                    "role": "assistant",
                    "status": "completed",
                    "id": format!("msg_{msg_idx}"),
                    "content": [{ "type": "output_text", "text": msg.content, "annotations": [] }]
                }));
                msg_idx += 1;
            }

            Role::ToolCall => {
                let call_id = msg.tool_call_id.as_deref().unwrap_or("call_0");
                let name = normalize_tool_name(msg.tool_name.as_deref().unwrap_or(""));
                let args = msg
                    .tool_args
                    .as_ref()
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "{}".to_string());
                out.push(serde_json::json!({
                    "type": "function_call",
                    "call_id": call_id,
                    "name": name,
                    "arguments": args,
                }));
            }

            Role::ToolResult => {
                let call_id = msg.tool_call_id.as_deref().unwrap_or("call_0");
                out.push(serde_json::json!({
                    "type": "function_call_output",
                    "call_id": call_id,
                    "output": msg.content,
                }));
            }
        }
    }

    out
}

// ── Ollama /api/chat ──────────────────────────────────────────────────────────

/// Convert a tau `Message` history to the Ollama `/api/chat` wire format.
///
/// Ollama's format is OpenAI Chat Completions-like with two differences:
/// - Assistant messages may carry a `thinking` field (chain-of-thought tokens
///   from a previous turn that should be echoed back).
/// - Tool-call `arguments` must be a JSON **object**, not a JSON-encoded
///   string (unlike OpenAI Chat Completions).
pub fn to_ollama_wire(messages: &[Message]) -> Vec<serde_json::Value> {
    let mut result: Vec<serde_json::Value> = Vec::new();
    let mut i = 0;

    while i < messages.len() {
        let msg = &messages[i];

        match msg.role {
            Role::Assistant => {
                let mut j = i + 1;
                let mut tool_calls: Vec<serde_json::Value> = Vec::new();
                let mut tool_results: Vec<serde_json::Value> = Vec::new();

                while j < messages.len() && messages[j].role == Role::ToolCall {
                    let tc = &messages[j];
                    let call_idx = tool_calls.len();
                    tool_calls.push(serde_json::json!({
                        "id": tc.tool_call_id.clone().unwrap_or_else(|| format!("call_{call_idx}")),
                        "type": "function",
                        "function": {
                            "name": normalize_tool_name(tc.tool_name.as_deref().unwrap_or_default()).to_string(),
                            // Ollama requires an object, not a JSON string.
                            "arguments": tc.tool_args.clone().unwrap_or_else(|| serde_json::json!({})),
                        }
                    }));
                    j += 1;

                    if j < messages.len() && messages[j].role == Role::ToolResult {
                        let tr = &messages[j];
                        let tr_content = openai_tool_result_content(tr);
                        tool_results.push(serde_json::json!({
                            "role": "tool",
                            "content": tr_content,
                            "tool_call_id": tr.tool_call_id,
                        }));
                        j += 1;
                    }
                }

                let content = if msg.content.is_empty() {
                    None
                } else {
                    Some(&msg.content)
                };
                let tool_calls_opt = if tool_calls.is_empty() {
                    None
                } else {
                    Some(&tool_calls)
                };

                if content.is_some() || tool_calls_opt.is_some() {
                    let mut entry = serde_json::json!({
                        "role": "assistant",
                        "content": content,
                        "tool_calls": tool_calls_opt,
                    });
                    // Echo thinking tokens back when present.
                    if let Some(thinking) = msg.thinking.as_deref().filter(|t| !t.is_empty()) {
                        entry["thinking"] = serde_json::Value::String(thinking.to_string());
                    }
                    result.push(entry);
                    result.extend(tool_results);
                }

                i = j;
            }

            Role::ToolCall => {
                let tc = msg;
                result.push(serde_json::json!({
                    "role": "assistant",
                    "content": serde_json::Value::Null,
                    "tool_calls": [{
                        "id": tc.tool_call_id.clone().unwrap_or_else(|| "call_0".to_string()),
                        "type": "function",
                        "function": {
                            "name": normalize_tool_name(tc.tool_name.as_deref().unwrap_or_default()).to_string(),
                            "arguments": tc.tool_args.clone().unwrap_or_else(|| serde_json::json!({})),
                        }
                    }],
                }));
                i += 1;
            }

            Role::ToolResult => {
                let tr = msg;
                let tr_content = openai_tool_result_content(tr);
                result.push(serde_json::json!({
                    "role": "tool",
                    "content": tr_content,
                    "tool_call_id": tr.tool_call_id,
                }));
                i += 1;
            }

            Role::System => {
                result.push(serde_json::json!({
                    "role": "system",
                    "content": msg.content,
                }));
                i += 1;
            }

            Role::User => {
                result.push(serde_json::json!({
                    "role": "user",
                    "content": msg.content,
                }));
                i += 1;
            }
        }
    }

    result
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::Message;

    // ── to_openai_wire ────────────────────────────────────────────────────────

    #[test]
    fn openai_wire_normalizes_emoji_tool_name() {
        let messages = vec![
            Message::assistant(""),
            Message::tool_call("id-1", "👀", serde_json::json!({"path": "foo"})),
            Message::tool_result("id-1", "content", false),
        ];
        let wire = to_openai_wire(&messages);
        let assistant = &wire[0];
        let name = &assistant["tool_calls"][0]["function"]["name"];
        assert_eq!(name, "read_file");
    }

    #[test]
    fn openai_wire_merges_assistant_with_tool_calls_and_results() {
        let messages = vec![
            Message::assistant("thinking"),
            Message::tool_call("id-1", "bash", serde_json::json!({"command": "ls"})),
            Message::tool_result("id-1", "output", false),
        ];
        let wire = to_openai_wire(&messages);
        assert_eq!(wire.len(), 2);
        assert_eq!(wire[0]["role"], "assistant");
        assert_eq!(wire[0]["content"], "thinking");
        assert_eq!(wire[1]["role"], "tool");
    }

    #[test]
    fn openai_wire_skips_empty_assistant_without_tool_calls() {
        let messages = vec![Message::assistant("")];
        let wire = to_openai_wire(&messages);
        assert!(wire.is_empty());
    }

    #[test]
    fn openai_wire_standalone_tool_call_fallback_id() {
        let mut tc = Message::tool_call("", "bash", serde_json::json!({}));
        tc.tool_call_id = None;
        let wire = to_openai_wire(&[tc]);
        assert_eq!(wire[0]["tool_calls"][0]["id"], "call_0");
    }

    // ── to_anthropic_wire ─────────────────────────────────────────────────────

    #[test]
    fn anthropic_wire_normalizes_emoji_tool_name() {
        let messages = vec![
            Message::assistant(""),
            Message::tool_call("id-1", "✏️", serde_json::json!({})),
            Message::tool_result("id-1", "ok", false),
        ];
        let wire = to_anthropic_wire(&messages);
        let tc_block = &wire[0]["content"][0];
        assert_eq!(tc_block["name"], "write_file");
    }

    #[test]
    fn anthropic_wire_skips_system_messages() {
        let messages = vec![Message::system("be helpful"), Message::user("hello")];
        let wire = to_anthropic_wire(&messages);
        assert_eq!(wire.len(), 1);
        assert_eq!(wire[0]["role"], "user");
    }

    #[test]
    fn anthropic_wire_tool_results_emitted_as_user_block() {
        let messages = vec![
            Message::assistant("ok"),
            Message::tool_call("id-1", "bash", serde_json::json!({})),
            Message::tool_result("id-1", "done", false),
        ];
        let wire = to_anthropic_wire(&messages);
        assert_eq!(wire.len(), 2);
        assert_eq!(wire[1]["role"], "user");
        assert_eq!(wire[1]["content"][0]["type"], "tool_result");
    }

    // ── to_gemini_wire ────────────────────────────────────────────────────────

    #[test]
    fn gemini_wire_normalizes_emoji_tool_name() {
        let messages = vec![Message::tool_call("id-1", "📝", serde_json::json!({}))];
        let wire = to_gemini_wire(&messages);
        assert_eq!(wire[0]["parts"][0]["functionCall"]["name"], "edit_file");
    }

    #[test]
    fn gemini_wire_skips_system_messages() {
        let messages = vec![Message::system("instructions"), Message::user("hi")];
        let wire = to_gemini_wire(&messages);
        assert_eq!(wire.len(), 1);
        assert_eq!(wire[0]["role"], "user");
    }

    #[test]
    fn gemini_wire_tool_result_resolves_name_from_side_table() {
        let messages = vec![
            Message::tool_call("id-1", "bash", serde_json::json!({})),
            Message::tool_result("id-1", "output", false),
        ];
        let wire = to_gemini_wire(&messages);
        assert_eq!(wire[1]["parts"][0]["functionResponse"]["name"], "bash");
    }

    // ── to_codex_wire ─────────────────────────────────────────────────────────

    #[test]
    fn codex_wire_normalizes_emoji_tool_name() {
        let messages = vec![Message::tool_call("id-1", "💻", serde_json::json!({}))];
        let wire = to_codex_wire(&messages);
        assert_eq!(wire[0]["name"], "bash");
    }

    #[test]
    fn codex_wire_skips_system_messages() {
        let messages = vec![Message::system("instructions"), Message::user("do it")];
        let wire = to_codex_wire(&messages);
        assert_eq!(wire.len(), 1);
        assert_eq!(wire[0]["role"], "user");
    }

    #[test]
    fn codex_wire_assistant_gets_type_message() {
        let messages = vec![Message::assistant("reply")];
        let wire = to_codex_wire(&messages);
        assert_eq!(wire[0]["type"], "message");
        assert_eq!(wire[0]["role"], "assistant");
    }

    // ── to_ollama_wire ────────────────────────────────────────────────────────

    #[test]
    fn ollama_wire_normalizes_emoji_tool_name() {
        let messages = vec![
            Message::assistant(""),
            Message::tool_call("id-1", "👀", serde_json::json!({"path": "foo"})),
            Message::tool_result("id-1", "content", false),
        ];
        let wire = to_ollama_wire(&messages);
        assert_eq!(wire[0]["tool_calls"][0]["function"]["name"], "read_file");
    }

    #[test]
    fn ollama_wire_arguments_are_object_not_string() {
        let messages = vec![
            Message::assistant(""),
            Message::tool_call("id-1", "bash", serde_json::json!({"command": "ls"})),
            Message::tool_result("id-1", "output", false),
        ];
        let wire = to_ollama_wire(&messages);
        // Must be an object, not a JSON-encoded string.
        assert!(wire[0]["tool_calls"][0]["function"]["arguments"].is_object());
        assert_eq!(
            wire[0]["tool_calls"][0]["function"]["arguments"]["command"],
            "ls"
        );
    }

    #[test]
    fn ollama_wire_thinking_echoed_when_present() {
        let mut msg = Message::assistant("reply");
        msg.thinking = Some("chain of thought".to_string());
        let wire = to_ollama_wire(&[msg]);
        assert_eq!(wire[0]["thinking"], "chain of thought");
    }

    #[test]
    fn ollama_wire_thinking_absent_when_empty() {
        let mut msg = Message::assistant("reply");
        msg.thinking = Some(String::new());
        let wire = to_ollama_wire(&[msg]);
        assert!(wire[0].get("thinking").is_none() || wire[0]["thinking"].is_null());
    }
}
