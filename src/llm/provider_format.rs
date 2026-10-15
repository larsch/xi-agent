//! Canonical per-protocol message serialization for LLM providers.
//!
//! Each function converts a slice of xi-agent [`Message`]s into the wire format
//! required by one protocol family.  All functions apply
//! [`normalize_tool_name`] consistently so that emoji shorthand tool names
//! are resolved before being sent to any provider.
//!
//! Protocol families:
//! - [`to_openai_wire`]    — OpenAI Chat Completions
//! - [`to_anthropic_wire`] — Anthropic Messages API
//! - [`to_gemini_wire`]    — Google Gemini `contents` array
//! - [`to_codex_wire`]     — OpenAI Responses API (Codex / GPT-5 style)
//! - [`to_ollama_wire`]    — Ollama `/api/chat` (OpenAI-like with `thinking` and object `arguments`)
//!
//! ## Per-provider deviations
//!
//! | Feature                        | OpenAI | Anthropic | Gemini | Codex | Ollama |
//! |-------------------------------|--------|-----------|--------|-------|--------|
//! | System messages               | kept   | skipped†  | skipped† | skipped† | kept |
//! | Tool-call arguments encoding  | string | object    | object | string | object |
//! | Image in tool result          | following user turn | `image` block (success); follow-up for errors | Gemini 3+: `functionResponse.parts`; older: follow-up turn | `input_image` in output | following user turn (`images`) |
//! | Thinking/reasoning echoed as  | `reasoning_content` | — | — | — | `thinking` |
//! | Standalone ToolCall fallback  | yes    | yes       | n/a‡   | n/a‡  | yes    |
//! | Standalone ToolResult fallback| yes    | yes       | n/a‡   | n/a‡  | yes    |
//! | Grouping model                | grouped | grouped  | flat   | flat  | grouped |
//!
//! † System messages are extracted by the caller and passed as a separate field.
//! ‡ Gemini and Codex iterate messages individually; standalone tool messages
//!   are emitted directly without grouping.

use super::common::normalize_tool_name;
use super::{ImageData, Message, Role};

// ── Shared helpers ─────────────────────────────────────────────────────────────

/// Return the tool args as a JSON object, falling back to `{}` when absent or
/// null.  Anthropic's API requires `input` to be a valid dictionary; a `null`
/// value (which can occur when the LLM emits a tool call with no arguments, or
/// when an old session file stores `"args": null`) causes a 400 error.
fn tool_args_object(msg: &Message) -> serde_json::Value {
    match &msg.tool_args {
        Some(v) if v.is_object() => v.clone(),
        _ => serde_json::json!({}),
    }
}

// ── Shared traversal ──────────────────────────────────────────────────────────

/// One logical conversation unit produced by [`group_messages`].
///
/// The `Assistant` variant groups an assistant message with any immediately
/// following interleaved `ToolCall`/`ToolResult` pairs.  Each pair is
/// `(call, result)` where `result` may be `None` when no `ToolResult`
/// immediately follows the call.
///
/// `StandaloneToolCall` and `StandaloneToolResult` cover orphaned messages that
/// appear without a surrounding `Assistant` turn — a defensive fallback for
/// histories that were not produced by the normal agent loop.
enum Turn<'a> {
    System(&'a Message),
    User(&'a Message),
    Assistant {
        msg: &'a Message,
        tool_pairs: Vec<(&'a Message, Option<&'a Message>)>,
    },
    StandaloneToolCall(&'a Message),
    StandaloneToolResult(&'a Message),
}

/// Group a flat message slice into logical [`Turn`]s.
///
/// The traversal consumes an `Assistant` message plus any immediately
/// following interleaved `ToolCall` / `ToolResult` pairs into a single
/// `Turn::Assistant`.  All other roles produce one turn each.
fn group_messages(messages: &[Message]) -> Vec<Turn<'_>> {
    let mut turns = Vec::new();
    let mut i = 0;

    while i < messages.len() {
        let msg = &messages[i];

        match msg.role {
            Role::System => {
                turns.push(Turn::System(msg));
                i += 1;
            }
            Role::User => {
                turns.push(Turn::User(msg));
                i += 1;
            }
            Role::Assistant => {
                let mut j = i + 1;
                let mut tool_pairs: Vec<(&Message, Option<&Message>)> = Vec::new();

                while j < messages.len() && messages[j].role == Role::ToolCall {
                    let call = &messages[j];
                    j += 1;
                    let result = if j < messages.len() && messages[j].role == Role::ToolResult {
                        let r = &messages[j];
                        j += 1;
                        Some(r)
                    } else {
                        None
                    };
                    tool_pairs.push((call, result));
                }

                turns.push(Turn::Assistant { msg, tool_pairs });
                i = j;
            }
            Role::ToolCall => {
                turns.push(Turn::StandaloneToolCall(msg));
                i += 1;
            }
            Role::ToolResult => {
                turns.push(Turn::StandaloneToolResult(msg));
                i += 1;
            }
        }
    }

    turns
}

// ── Shared helpers ────────────────────────────────────────────────────────────

/// Tool-result images are sent in a following user message for Chat
/// Completions and Ollama. Their tool-result messages remain text-only.
fn openai_tool_result_content(tr: &Message) -> serde_json::Value {
    serde_json::Value::String(tr.content.clone())
}

fn openai_tool_result_image_part(image: &ImageData) -> serde_json::Value {
    serde_json::json!({
        "type": "image_url",
        "image_url": { "url": format!("data:{};base64,{}", image.mime_type, image.base64) }
    })
}

/// Build the `content` array for an Anthropic tool result message.
///
/// For image results the content carries both a `text` block (placeholder)
/// and an `image` block with base64 data.  For text results it is a single
/// `text` block.
fn anthropic_tool_result_content(tr: &Message) -> serde_json::Value {
    if tr.is_error {
        return serde_json::Value::String(tr.content.clone());
    }
    if let Some(ImageData { base64, mime_type }) = &tr.image_data {
        serde_json::json!([
            { "type": "text", "text": &tr.content },
            { "type": "image", "source": { "type": "base64", "media_type": mime_type, "data": base64 } }
        ])
    } else {
        serde_json::Value::String(tr.content.clone())
    }
}

// ── OpenAI Chat Completions ───────────────────────────────────────────────────

/// Convert a xi-agent `Message` history to the OpenAI Chat Completions wire format.
///
/// The OpenAI API requires that tool calls and their accompanying text live in
/// *one* assistant message, followed by one `"role":"tool"` message per result.
/// xi-agent stores them as separate `Role::Assistant` + `Role::ToolCall` +
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

    for turn in group_messages(messages) {
        match turn {
            Turn::System(msg) => {
                result.push(serde_json::json!({
                    "role": "system",
                    "content": msg.content,
                }));
            }
            Turn::User(msg) => {
                result.push(serde_json::json!({
                    "role": "user",
                    "content": msg.content,
                }));
            }
            Turn::Assistant { msg, tool_pairs } => {
                let mut tool_calls: Vec<serde_json::Value> = Vec::new();
                let mut tool_results: Vec<serde_json::Value> = Vec::new();

                let mut tool_result_images: Vec<serde_json::Value> = Vec::new();

                for (call_idx, (tc, tr_opt)) in tool_pairs.iter().enumerate() {
                    tool_calls.push(serde_json::json!({
                        "id": tc.tool_call_id.clone().unwrap_or_else(|| format!("call_{call_idx}")),
                        "type": "function",
                        "function": {
                            "name": normalize_tool_name(tc.tool_name.as_deref().unwrap_or_default()).to_string(),
                            "arguments": tc.tool_args.as_ref().map(|v| v.to_string()).unwrap_or_else(|| "{}".to_string()),
                        }
                    }));
                    if let Some(tr) = tr_opt {
                        let tr_content = openai_tool_result_content(tr);
                        tool_results.push(serde_json::json!({
                            "role": "tool",
                            "content": tr_content,
                            "tool_call_id": tr.tool_call_id,
                        }));
                        if let Some(image) = &tr.image_data {
                            tool_result_images.push(openai_tool_result_image_part(image));
                        }
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
                        // Always include reasoning_content so reasoning
                        // models (e.g. DeepSeek-v4-pro) don't reject the
                        // request.  Use an empty string when there is no
                        // thinking content (matches the model's own
                        // initial delta pattern).
                        "reasoning_content": "",
                    });
                    if let Some(thinking) = msg.thinking.as_deref().filter(|t| !t.is_empty()) {
                        entry["reasoning_content"] =
                            serde_json::Value::String(thinking.to_string());
                    }
                    result.push(entry);
                    result.extend(tool_results);
                    if !tool_result_images.is_empty() {
                        let mut parts = vec![serde_json::json!({
                            "type": "text",
                            "text": "Attached image(s) from tool result:",
                        })];
                        parts.extend(tool_result_images);
                        result.push(serde_json::json!({ "role": "user", "content": parts }));
                    }
                }
            }
            Turn::StandaloneToolCall(tc) => {
                result.push(serde_json::json!({
                    "role": "assistant",
                    "content": serde_json::Value::Null,
                    "reasoning_content": "",
                    "tool_calls": [{
                        "id": tc.tool_call_id.clone().unwrap_or_else(|| "call_0".to_string()),
                        "type": "function",
                        "function": {
                            "name": normalize_tool_name(tc.tool_name.as_deref().unwrap_or_default()).to_string(),
                            "arguments": tc.tool_args.as_ref().map(|v| v.to_string()).unwrap_or_else(|| "{}".to_string()),
                        }
                    }],
                }));
            }
            Turn::StandaloneToolResult(tr) => {
                let tr_content = openai_tool_result_content(tr);
                result.push(serde_json::json!({
                    "role": "tool",
                    "content": tr_content,
                    "tool_call_id": tr.tool_call_id,
                }));
                if let Some(image) = &tr.image_data {
                    result.push(serde_json::json!({
                        "role": "user",
                        "content": [
                            { "type": "text", "text": "Attached image from tool result:" },
                            openai_tool_result_image_part(image),
                        ],
                    }));
                }
            }
        }
    }

    result
}

// ── Anthropic Messages API ────────────────────────────────────────────────────

/// Convert a xi-agent `Message` history to the Anthropic Messages API wire format.
///
/// System messages are skipped here (they must be extracted separately and
/// passed as the top-level `system` field in the request body).
/// Tool calls are grouped with their parent assistant turn into a single
/// `content` array entry; tool results are emitted as a `"role":"user"` block.
pub fn to_anthropic_wire(messages: &[Message]) -> Vec<serde_json::Value> {
    let mut result: Vec<serde_json::Value> = Vec::new();

    for turn in group_messages(messages) {
        match turn {
            Turn::System(_) => {}
            Turn::User(msg) => {
                result.push(serde_json::json!({
                    "role": "user",
                    "content": msg.content,
                }));
            }
            Turn::Assistant { msg, tool_pairs } => {
                let mut content: Vec<serde_json::Value> = Vec::new();

                if !msg.content.is_empty() {
                    content.push(serde_json::json!({
                        "type": "text",
                        "text": msg.content,
                    }));
                }

                let mut tool_results: Vec<serde_json::Value> = Vec::new();
                let mut error_images: Vec<serde_json::Value> = Vec::new();
                for (tc, tr_opt) in &tool_pairs {
                    content.push(serde_json::json!({
                        "type": "tool_use",
                        "id": tc.tool_call_id.as_deref().unwrap_or("call_0"),
                        "name": normalize_tool_name(tc.tool_name.as_deref().unwrap_or("")),
                        "input": tool_args_object(tc),
                    }));
                    if let Some(tr) = tr_opt {
                        let tr_content = anthropic_tool_result_content(tr);
                        tool_results.push(serde_json::json!({
                            "type": "tool_result",
                            "tool_use_id": tr.tool_call_id.as_deref().unwrap_or("call_0"),
                            "content": tr_content,
                            "is_error": tr.is_error,
                        }));
                        if tr.is_error
                            && let Some(ImageData { base64, mime_type }) = &tr.image_data
                        {
                            error_images.push(serde_json::json!({
                                "type": "image",
                                "source": { "type": "base64", "media_type": mime_type, "data": base64 }
                            }));
                        }
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
                    tool_results.extend(error_images);
                    result.push(serde_json::json!({
                        "role": "user",
                        "content": tool_results,
                    }));
                }
            }
            Turn::StandaloneToolCall(tc) => {
                result.push(serde_json::json!({
                    "role": "assistant",
                    "content": [{
                        "type": "tool_use",
                        "id": tc.tool_call_id.as_deref().unwrap_or("call_0"),
                        "name": normalize_tool_name(tc.tool_name.as_deref().unwrap_or("")),
                        "input": tool_args_object(tc),
                    }],
                }));
            }
            Turn::StandaloneToolResult(tr) => {
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
                if tr.is_error
                    && let Some(ImageData { base64, mime_type }) = &tr.image_data
                {
                    result.push(serde_json::json!({
                        "role": "user",
                        "content": [
                            { "type": "text", "text": "Image from error tool result:" },
                            { "type": "image", "source": { "type": "base64", "media_type": mime_type, "data": base64 } }
                        ],
                    }));
                }
            }
        }
    }

    result
}

// ── Google Gemini ─────────────────────────────────────────────────────────────

fn gemini_supports_multimodal_function_response(model: &str) -> bool {
    let lower = model.to_ascii_lowercase();
    let version = lower
        .strip_prefix("gemini-")
        .and_then(|suffix| suffix.split(['-', '.']).next())
        .and_then(|major| major.parse::<u32>().ok());
    version.is_none_or(|major| major >= 3)
}

/// Convert a xi-agent `Message` history to the Gemini `contents` array wire format.
///
/// System messages are skipped (passed separately as `systemInstruction`).
/// Tool calls use `functionCall` parts; tool results use `functionResponse`
/// parts.  A side-table maps tool-call IDs to names so that `ToolResult`
/// messages can include the correct `name` field even when `tool_name` is
/// absent on the result message.
///
/// Gemini iterates messages individually (flat model) rather than grouping
/// assistant turns — see the deviation table in the module doc.
pub fn to_gemini_wire(messages: &[Message], model: &str) -> Vec<serde_json::Value> {
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
                if let Some(ImageData { base64, mime_type }) = &msg.image_data {
                    let image_part = serde_json::json!({
                        "inlineData": { "mimeType": mime_type, "data": base64 }
                    });
                    if gemini_supports_multimodal_function_response(model) {
                        let response_parts = &mut contents
                            .last_mut()
                            .expect("function response was just appended")["parts"];
                        response_parts[0]["functionResponse"]["parts"] =
                            serde_json::json!([image_part]);
                    } else {
                        contents.push(serde_json::json!({
                            "role": "user",
                            "parts": [
                                { "text": "Tool result image:" },
                                image_part
                            ],
                        }));
                    }
                }
            }
        }
    }

    contents
}

// ── OpenAI Responses API (Codex / GPT-5) ─────────────────────────────────────

/// Convert a xi-agent `Message` history to the OpenAI Responses API wire format.
///
/// System messages are skipped (passed as the `instructions` field).
/// This format differs from Chat Completions: assistant messages use
/// `"type":"message"` with `output_text` content; tool calls are
/// `"type":"function_call"` items; tool results are
/// `"type":"function_call_output"` items.
///
/// Codex iterates messages individually (flat model) — see the deviation
/// table in the module doc.
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
                let output = if let Some(ImageData { base64, mime_type }) = &msg.image_data {
                    serde_json::json!([
                        { "type": "input_text", "text": msg.content },
                        { "type": "input_image", "detail": "auto", "image_url": format!("data:{mime_type};base64,{base64}") }
                    ])
                } else {
                    serde_json::Value::String(msg.content.clone())
                };
                out.push(serde_json::json!({
                    "type": "function_call_output",
                    "call_id": call_id,
                    "output": output,
                }));
            }
        }
    }

    out
}

// ── Ollama /api/chat ──────────────────────────────────────────────────────────

/// Convert a xi-agent `Message` history to the Ollama `/api/chat` wire format.
///
/// Ollama's format is OpenAI Chat Completions-like with two differences:
/// - Assistant messages may carry a `thinking` field (chain-of-thought tokens
///   from a previous turn that should be echoed back).
/// - Tool-call `arguments` must be a JSON **object**, not a JSON-encoded
///   string (unlike OpenAI Chat Completions).
pub fn to_ollama_wire(messages: &[Message]) -> Vec<serde_json::Value> {
    let mut result: Vec<serde_json::Value> = Vec::new();

    for turn in group_messages(messages) {
        match turn {
            Turn::System(msg) => {
                result.push(serde_json::json!({
                    "role": "system",
                    "content": msg.content,
                }));
            }
            Turn::User(msg) => {
                result.push(serde_json::json!({
                    "role": "user",
                    "content": msg.content,
                }));
            }
            Turn::Assistant { msg, tool_pairs } => {
                let mut tool_calls: Vec<serde_json::Value> = Vec::new();
                let mut tool_results: Vec<serde_json::Value> = Vec::new();
                let mut tool_result_images: Vec<String> = Vec::new();

                for (call_idx, (tc, tr_opt)) in tool_pairs.iter().enumerate() {
                    tool_calls.push(serde_json::json!({
                        "id": tc.tool_call_id.clone().unwrap_or_else(|| format!("call_{call_idx}")),
                        "type": "function",
                        "function": {
                            "name": normalize_tool_name(tc.tool_name.as_deref().unwrap_or_default()).to_string(),
                            // Ollama requires an object, not a JSON string.
                            "arguments": tc.tool_args.clone().unwrap_or_else(|| serde_json::json!({})),
                        }
                    }));
                    if let Some(tr) = tr_opt {
                        let tr_content = openai_tool_result_content(tr);
                        tool_results.push(serde_json::json!({
                            "role": "tool",
                            "content": tr_content,
                            "tool_call_id": tr.tool_call_id,
                        }));
                        if let Some(ImageData { base64, .. }) = &tr.image_data {
                            tool_result_images.push(base64.clone());
                        }
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
                    if !tool_result_images.is_empty() {
                        result.push(serde_json::json!({
                            "role": "user",
                            "content": "Attached image(s) from tool result:",
                            "images": tool_result_images,
                        }));
                    }
                }
            }
            Turn::StandaloneToolCall(tc) => {
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
            }
            Turn::StandaloneToolResult(tr) => {
                let tr_content = openai_tool_result_content(tr);
                result.push(serde_json::json!({
                    "role": "tool",
                    "content": tr_content,
                    "tool_call_id": tr.tool_call_id,
                }));
                if let Some(ImageData { base64, .. }) = &tr.image_data {
                    result.push(serde_json::json!({
                        "role": "user",
                        "content": "Attached image from tool result:",
                        "images": [base64],
                    }));
                }
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
    fn openai_tool_result_image_moves_to_following_user_message() {
        let mut result = Message::tool_result("id-1", "[image]", false);
        result.image_data = Some(ImageData {
            base64: "aW1n".into(),
            mime_type: "image/png".into(),
        });
        let wire = to_openai_wire(&[
            Message::assistant(""),
            Message::tool_call("id-1", "read_file", serde_json::json!({})),
            result,
        ]);
        assert_eq!(wire[1]["role"], "tool");
        assert_eq!(wire[1]["content"], "[image]");
        assert_eq!(wire[2]["role"], "user");
        assert_eq!(wire[2]["content"][1]["type"], "image_url");
        assert_eq!(
            wire[2]["content"][1]["image_url"]["url"],
            "data:image/png;base64,aW1n"
        );
    }

    #[test]
    fn openai_wire_reasoning_content_echoed_when_present() {
        let mut msg = Message::assistant("answer");
        msg.thinking = Some("chain of thought".to_string());
        let wire = to_openai_wire(&[msg]);
        assert_eq!(wire[0]["reasoning_content"], "chain of thought");
    }

    #[test]
    fn openai_wire_reasoning_content_absent_when_empty() {
        let mut msg = Message::assistant("answer");
        msg.thinking = Some(String::new());
        let wire = to_openai_wire(&[msg]);
        // reasoning_content is always present; empty string when no thinking.
        assert_eq!(wire[0]["reasoning_content"], "");
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
    fn anthropic_wire_keeps_image_in_successful_tool_result() {
        let mut result = Message::tool_result("id-1", "[image]", false);
        result.image_data = Some(ImageData {
            base64: "aW1n".into(),
            mime_type: "image/png".into(),
        });
        let wire = to_anthropic_wire(&[
            Message::assistant(""),
            Message::tool_call("id-1", "read_file", serde_json::json!({})),
            result,
        ]);
        assert_eq!(wire[1]["content"][0]["type"], "tool_result");
        assert_eq!(wire[1]["content"][0]["content"][1]["type"], "image");
    }

    #[test]
    fn anthropic_wire_moves_error_result_image_after_tool_result() {
        let mut result = Message::tool_result("id-1", "failed", true);
        result.image_data = Some(ImageData {
            base64: "aW1n".into(),
            mime_type: "image/png".into(),
        });
        let wire = to_anthropic_wire(&[
            Message::assistant(""),
            Message::tool_call("id-1", "read_file", serde_json::json!({})),
            result,
        ]);
        assert_eq!(wire[1]["content"][0]["type"], "tool_result");
        assert_eq!(wire[1]["content"][0]["content"], "failed");
        assert_eq!(wire[1]["content"][1]["type"], "image");
    }

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

    #[test]
    fn anthropic_wire_null_tool_args_become_empty_object() {
        // When the LLM emits a tool call with null args (a known model quirk),
        // the Anthropic wire format must still emit an object for `input`, not
        // null — Anthropic's API rejects null with a 400.
        // Case 1: tool_args is None
        let mut tc = Message::tool_call("id-1", "exec", serde_json::json!({}));
        tc.tool_args = None;
        let messages = vec![
            Message::assistant(""),
            tc,
            Message::tool_result("id-1", "err", true),
        ];
        let wire = to_anthropic_wire(&messages);
        assert!(wire[0]["content"][0]["input"].is_object());

        // Case 2: tool_args is Some(Value::Null) — from "args": null in session file
        let mut tc2 = Message::tool_call("id-2", "exec", serde_json::json!({}));
        tc2.tool_args = Some(serde_json::Value::Null);
        let messages2 = vec![
            Message::assistant(""),
            tc2,
            Message::tool_result("id-2", "err", true),
        ];
        let wire2 = to_anthropic_wire(&messages2);
        assert!(wire2[0]["content"][0]["input"].is_object());
    }

    #[test]
    fn anthropic_wire_standalone_null_tool_args_become_empty_object() {
        // Case 1: None
        let mut tc = Message::tool_call("id-1", "exec", serde_json::json!({}));
        tc.tool_args = None;
        let wire = to_anthropic_wire(&[tc]);
        assert!(wire[0]["content"][0]["input"].is_object());

        // Case 2: Some(Value::Null)
        let mut tc2 = Message::tool_call("id-2", "exec", serde_json::json!({}));
        tc2.tool_args = Some(serde_json::Value::Null);
        let wire2 = to_anthropic_wire(&[tc2]);
        assert!(wire2[0]["content"][0]["input"].is_object());
    }

    // ── to_gemini_wire ────────────────────────────────────────────────────────

    #[test]
    fn gemini_wire_attaches_image_after_function_response() {
        let mut result = Message::tool_result("id-1", "[image]", false);
        result.image_data = Some(ImageData {
            base64: "aW1n".into(),
            mime_type: "image/png".into(),
        });
        let wire = to_gemini_wire(
            &[
                Message::tool_call("id-1", "read_file", serde_json::json!({})),
                result,
            ],
            "gemini-3.0-flash",
        );
        assert!(wire[1]["parts"][0]["functionResponse"].is_object());
        assert_eq!(
            wire[1]["parts"][0]["functionResponse"]["parts"][0]["inlineData"]["mimeType"],
            "image/png"
        );
        assert_eq!(
            wire[1]["parts"][0]["functionResponse"]["parts"][0]["inlineData"]["data"],
            "aW1n"
        );
    }

    #[test]
    fn gemini_wire_normalizes_emoji_tool_name() {
        let messages = vec![Message::tool_call("id-1", "📝", serde_json::json!({}))];
        let wire = to_gemini_wire(&messages, "gemini-2.0-flash");
        assert_eq!(wire[0]["parts"][0]["functionCall"]["name"], "edit_file");
    }

    #[test]
    fn gemini_wire_skips_system_messages() {
        let messages = vec![Message::system("instructions"), Message::user("hi")];
        let wire = to_gemini_wire(&messages, "gemini-2.0-flash");
        assert_eq!(wire.len(), 1);
        assert_eq!(wire[0]["role"], "user");
    }

    #[test]
    fn gemini_wire_tool_result_resolves_name_from_side_table() {
        let messages = vec![
            Message::tool_call("id-1", "bash", serde_json::json!({})),
            Message::tool_result("id-1", "output", false),
        ];
        let wire = to_gemini_wire(&messages, "gemini-2.0-flash");
        assert_eq!(wire[1]["parts"][0]["functionResponse"]["name"], "bash");
    }

    // ── to_codex_wire ─────────────────────────────────────────────────────────

    #[test]
    fn codex_wire_includes_image_in_function_call_output() {
        let mut result = Message::tool_result("id-1", "[image]", false);
        result.image_data = Some(ImageData {
            base64: "aW1n".into(),
            mime_type: "image/png".into(),
        });
        let wire = to_codex_wire(&[result]);
        assert_eq!(wire[0]["output"][0]["type"], "input_text");
        assert_eq!(wire[0]["output"][1]["type"], "input_image");
        assert_eq!(
            wire[0]["output"][1]["image_url"],
            "data:image/png;base64,aW1n"
        );
    }

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
    fn ollama_wire_attaches_image_in_following_user_message() {
        let mut result = Message::tool_result("id-1", "[image]", false);
        result.image_data = Some(ImageData {
            base64: "aW1n".into(),
            mime_type: "image/png".into(),
        });
        let wire = to_ollama_wire(&[
            Message::assistant(""),
            Message::tool_call("id-1", "read_file", serde_json::json!({})),
            result,
        ]);
        assert_eq!(wire[1]["role"], "tool");
        assert_eq!(wire[2]["role"], "user");
        assert_eq!(wire[2]["images"][0], "aW1n");
    }

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
