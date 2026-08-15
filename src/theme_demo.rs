use crate::{
    llm::{AssistantPhase, DisplayRange, UsageStats},
    session_event::SessionEvent,
};

/// Build a compact conversation containing the persistent log block variants.
/// The events deliberately use the ordinary session format so the demo follows
/// the same projection and rendering path as a real conversation.
pub(crate) fn demo_events() -> Vec<SessionEvent> {
    let ts = 1_735_689_600;
    let long_text =
        "This deliberately long message demonstrates wrapping across several lines. ".repeat(8);
    let long_output = (1..=18)
        .map(|n| format!("output line {n}: representative tool output for the theme demo"))
        .collect::<Vec<_>>()
        .join("\n");

    let mut events = vec![
        SessionEvent::UserMessage {
            content: "Short user message.".to_string(),
            timestamp: ts,
        },
        SessionEvent::AssistantMessage {
            content: "Short assistant response.".to_string(),
            thinking: None,
            phase: AssistantPhase::Final,
            usage: None,
            timestamp: ts,
        },
        SessionEvent::UserMessage {
            content: long_text.clone(),
            timestamp: ts,
        },
        SessionEvent::AssistantMessage {
            content: "A longer assistant response with visible reasoning, multiple paragraphs, and enough content to demonstrate the normal wrapped assistant block.\n\nThe second paragraph remains ordinary conversation history.".to_string(),
            thinking: Some("Thinking block: inspect the request, compare the alternatives, and choose a clear implementation.\nThis is deliberately multiline so its styling is visible.".to_string()),
            phase: AssistantPhase::Final,
            usage: Some(UsageStats {
                input_tokens: Some(420),
                output_tokens: Some(180),
                total_tokens: Some(600),
                cached_tokens: None,
            }),
            timestamp: ts,
        },
        SessionEvent::ToolCall {
            id: "demo_read".to_string(),
            name: "read_file".to_string(),
            args: serde_json::json!({"path": "src/main.rs", "offset": 1, "limit": 12}),
            include_in_llm: true,
            timestamp: ts,
        },
        SessionEvent::ToolResult {
            id: "demo_read".to_string(),
            name: "read_file".to_string(),
            content: "fn main() {\n    println!(\"hello from the demo\");\n}\n".to_string(),
            is_error: false,
            display_range: Some(DisplayRange {
                first_line: 1,
                last_line: 4,
                total_lines: 120,
            }),
            include_in_llm: true,
            timestamp: ts,
        },
        SessionEvent::ToolCall {
            id: "demo_bash".to_string(),
            name: "bash".to_string(),
            args: serde_json::json!({"command": "cargo test --all-features"}),
            include_in_llm: true,
            timestamp: ts,
        },
        SessionEvent::ToolResult {
            id: "demo_bash".to_string(),
            name: "bash".to_string(),
            content: long_output.clone(),
            is_error: false,
            display_range: None,
            include_in_llm: true,
            timestamp: ts,
        },
        SessionEvent::ToolCall {
            id: "demo_write".to_string(),
            name: "write_file".to_string(),
            args: serde_json::json!({
                "path": "demo.txt",
                "content": "line one\nline two\nline three\nline four\nline five\nline six\nline seven\nline eight"
            }),
            include_in_llm: true,
            timestamp: ts,
        },
        SessionEvent::ToolResult {
            id: "demo_write".to_string(),
            name: "write_file".to_string(),
            content: "Wrote 8 lines to demo.txt".to_string(),
            is_error: false,
            display_range: None,
            include_in_llm: true,
            timestamp: ts,
        },
        SessionEvent::ToolCall {
            id: "demo_diff".to_string(),
            name: "edit_file".to_string(),
            args: serde_json::json!({
                "path": "src/demo.rs",
                "old_text": "unchanged\nremoved one\nremoved two\nunchanged",
                "new_text": "unchanged\nadded one\nadded two\nunchanged"
            }),
            include_in_llm: true,
            timestamp: ts,
        },
        SessionEvent::ToolResult {
            id: "demo_diff".to_string(),
            name: "edit_file".to_string(),
            content: "File edited successfully".to_string(),
            is_error: false,
            display_range: None,
            include_in_llm: true,
            timestamp: ts,
        },
        SessionEvent::ToolCall {
            id: "demo_add".to_string(),
            name: "edit_file".to_string(),
            args: serde_json::json!({
                "path": "src/add.rs",
                "old_text": "same line",
                "new_text": "same line\nnew insertion"
            }),
            include_in_llm: true,
            timestamp: ts,
        },
        SessionEvent::ToolResult {
            id: "demo_add".to_string(),
            name: "edit_file".to_string(),
            content: "File edited successfully".to_string(),
            is_error: false,
            display_range: None,
            include_in_llm: true,
            timestamp: ts,
        },
        SessionEvent::ToolCall {
            id: "demo_remove".to_string(),
            name: "edit_file".to_string(),
            args: serde_json::json!({
                "path": "src/remove.rs",
                "old_text": "same line\nremoved line",
                "new_text": "same line"
            }),
            include_in_llm: true,
            timestamp: ts,
        },
        SessionEvent::ToolResult {
            id: "demo_remove".to_string(),
            name: "edit_file".to_string(),
            content: "File edited successfully".to_string(),
            is_error: false,
            display_range: None,
            include_in_llm: true,
            timestamp: ts,
        },
        SessionEvent::ToolCall {
            id: "demo_error_tool".to_string(),
            name: "bash".to_string(),
            args: serde_json::json!({"command": "false"}),
            include_in_llm: true,
            timestamp: ts,
        },
        SessionEvent::ToolResult {
            id: "demo_error_tool".to_string(),
            name: "bash".to_string(),
            content: "command failed: exit status 1".to_string(),
            is_error: true,
            display_range: None,
            include_in_llm: true,
            timestamp: ts,
        },
        SessionEvent::TurnError {
            message: "[demo turn error: provider request failed]".to_string(),
            timestamp: ts,
        },
        SessionEvent::ToolCall {
            id: "demo_ask".to_string(),
            name: "ask_user".to_string(),
            args: serde_json::json!({
                "question": "Which block style should you inspect next?",
                "options": ["Diffs", "Tool output", "Assistant blocks"]
            }),
            include_in_llm: true,
            timestamp: ts,
        },
        SessionEvent::ToolResult {
            id: "demo_ask".to_string(),
            name: "ask_user".to_string(),
            content: "Diffs".to_string(),
            is_error: false,
            display_range: None,
            include_in_llm: true,
            timestamp: ts,
        },
        SessionEvent::UserMessage {
            content: "An empty-looking follow-up with a blank line:\n\nAnd a final line.".to_string(),
            timestamp: ts,
        },
        SessionEvent::AssistantMessage {
            content: "The demo is ready. Type a message to continue with the test provider.".to_string(),
            thinking: None,
            phase: AssistantPhase::Final,
            usage: None,
            timestamp: ts,
        },
        SessionEvent::AssistantMessage {
            content: "## Markdown response showcase\n\nThis paragraph demonstrates a model response with **strong emphasis**, _subtle emphasis_, and an inline preformatted element such as `cargo test --all-features`.\n\nThe preformatted section below preserves source-like spacing:\n\n```rust\nfn greet(name: &str) -> String {\n    format!(\"hello, {name}\")\n}\n```\n\nThe main ingredients are:\n\n- A paragraph with inline formatting\n- A preformatted code section\n- A compact table of values\n\nThe review sequence is:\n\n1. Read the response headline.\n2. Inspect the formatted source.\n3. Compare the table values.\n\n| Block | Priority | Hue family |\n| --- | ---: | --- |\n| Model response | 1 | Neutral |\n| Tool intent | 2 | Family accent |\n| Tool output | 3 | Muted family |\n| Thinking | 4 | Dim violet |\n\nThis final paragraph closes the Markdown showcase.".to_string(),
            thinking: None,
            phase: AssistantPhase::Final,
            usage: None,
            timestamp: ts,
        },
    ];

    // Keep the transcript intentionally repetitive: the demo is a visual
    // catalogue, so every renderer gets both a compact and an overflowing
    // example rather than relying on one representative tool.
    let short_lines = "one\ntwo\nthree";
    let many_lines = (1..=14)
        .map(|n| format!("line {n}: deliberately visible demo content"))
        .collect::<Vec<_>>()
        .join("\n");
    let long_line =
        "wrapped content that is long enough to cross the terminal width many times ".repeat(6);

    push_tool_pair(
        &mut events,
        "demo_read_short",
        "read_file",
        serde_json::json!({"path": "README.md"}),
        short_lines,
        false,
        Some(DisplayRange {
            first_line: 1,
            last_line: 3,
            total_lines: 3,
        }),
        ts,
    );
    push_tool_pair(
        &mut events,
        "demo_read_long",
        "read_file",
        serde_json::json!({"path": "src/lib.rs"}),
        &many_lines,
        false,
        Some(DisplayRange {
            first_line: 20,
            last_line: 33,
            total_lines: 240,
        }),
        ts,
    );
    push_tool_pair(
        &mut events,
        "demo_find_short",
        "find_files",
        serde_json::json!({"pattern": "*.toml"}),
        "Cargo.toml\nCargo.lock",
        false,
        None,
        ts,
    );
    push_tool_pair(
        &mut events,
        "demo_find_long",
        "find_files",
        serde_json::json!({"pattern": "src/**/*.rs"}),
        &many_lines,
        false,
        None,
        ts,
    );
    push_tool_pair(
        &mut events,
        "demo_write_short",
        "write_file",
        serde_json::json!({"path": "short.txt", "content": short_lines}),
        "Wrote 3 lines to short.txt",
        false,
        None,
        ts,
    );
    push_tool_pair(
        &mut events,
        "demo_write_long",
        "write_file",
        serde_json::json!({"path": "long.txt", "content": many_lines}),
        "Wrote 14 lines to long.txt",
        false,
        None,
        ts,
    );

    for (id, name, args, content) in [
        (
            "demo_exec_short",
            "exec",
            serde_json::json!({"command": "pwd"}),
            "build\nclean".to_string(),
        ),
        (
            "demo_cmd_long",
            "cmd",
            serde_json::json!({"command": "dir /s"}),
            many_lines.clone(),
        ),
        (
            "demo_powershell_short",
            "powershell",
            serde_json::json!({"command": "Get-Date"}),
            "2025-01-01".to_string(),
        ),
        (
            "demo_python_long",
            "run_python",
            serde_json::json!({"script": "print output"}),
            many_lines.clone(),
        ),
        (
            "demo_custom_short",
            "custom_demo",
            serde_json::json!({"value": "small"}),
            "custom result".to_string(),
        ),
    ] {
        push_tool_pair(&mut events, id, name, args, &content, false, None, ts);
    }
    push_tool_pair(
        &mut events,
        "demo_bash_wrapped",
        "bash",
        serde_json::json!({"command": "printf long-line"}),
        &long_line,
        false,
        None,
        ts,
    );

    let diff_cases = [
        ("demo_diff_identical", "same\ntext", "same\ntext"),
        (
            "demo_diff_no_context",
            "old one\nold two",
            "new one\nnew two",
        ),
        ("demo_diff_head", "common\nold", "common\nnew"),
        ("demo_diff_tail", "old\ncommon", "new\ncommon"),
        (
            "demo_diff_multiple",
            "head\nold one\nmiddle\nold two\ntail",
            "head\nnew one\nmiddle\nnew two\ntail",
        ),
        ("demo_diff_empty_old", "", "new line"),
        ("demo_diff_empty_new", "removed line", ""),
        (
            "demo_diff_wrapped",
            &long_line,
            &format!("{long_line}\nnew wrapped line"),
        ),
    ];
    for (id, old_text, new_text) in diff_cases {
        push_diff_pair(&mut events, id, old_text, new_text, ts);
    }
    let long_diff_old = (1..=12)
        .map(|n| format!("removed {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    let long_diff_new = (1..=12)
        .map(|n| format!("added {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    push_diff_pair(
        &mut events,
        "demo_diff_long_sides",
        &long_diff_old,
        &long_diff_new,
        ts,
    );

    events
}

// The helper mirrors the durable ToolCall/ToolResult fields so demo cases stay
// readable at each call site.
#[allow(clippy::too_many_arguments)]
fn push_tool_pair(
    events: &mut Vec<SessionEvent>,
    id: &str,
    name: &str,
    args: serde_json::Value,
    content: &str,
    is_error: bool,
    display_range: Option<DisplayRange>,
    timestamp: u64,
) {
    events.push(SessionEvent::ToolCall {
        id: id.to_string(),
        name: name.to_string(),
        args,
        include_in_llm: true,
        timestamp,
    });
    events.push(SessionEvent::ToolResult {
        id: id.to_string(),
        name: name.to_string(),
        content: content.to_string(),
        is_error,
        display_range,
        include_in_llm: true,
        timestamp,
    });
}

fn push_diff_pair(
    events: &mut Vec<SessionEvent>,
    id: &str,
    old_text: &str,
    new_text: &str,
    timestamp: u64,
) {
    push_tool_pair(
        events,
        id,
        "edit_file",
        serde_json::json!({"path": format!("{id}.rs"), "old_text": old_text, "new_text": new_text}),
        "File edited successfully",
        false,
        None,
        timestamp,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_events_cover_requested_log_variants() {
        let events = demo_events();
        assert!(events.iter().any(|e| matches!(
            e,
            SessionEvent::AssistantMessage {
                thinking: Some(_),
                ..
            }
        )));
        assert!(
            events
                .iter()
                .any(|e| matches!(e, SessionEvent::ToolResult { is_error: true, .. }))
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, SessionEvent::TurnError { .. }))
        );
        let edits = events
            .iter()
            .filter(|e| matches!(e, SessionEvent::ToolCall { name, .. } if name == "edit_file"))
            .count();
        assert!(
            edits >= 12,
            "demo should show a broad diff catalogue, got {edits}"
        );
        assert!(events.iter().any(|e| matches!(e, SessionEvent::ToolResult { content, display_range: Some(_), .. } if content.contains("fn main"))));

        let tool_names: std::collections::HashSet<&str> = events
            .iter()
            .filter_map(|event| match event {
                SessionEvent::ToolCall { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        for name in [
            "read_file",
            "find_files",
            "write_file",
            "bash",
            "cmd",
            "powershell",
            "exec",
            "run_python",
            "custom_demo",
            "edit_file",
        ] {
            assert!(tool_names.contains(name), "demo is missing {name}");
        }

        let diff_ids: std::collections::HashSet<&str> = events
            .iter()
            .filter_map(|event| match event {
                SessionEvent::ToolCall { id, name, .. } if name == "edit_file" => Some(id.as_str()),
                _ => None,
            })
            .collect();
        for id in [
            "demo_diff_identical",
            "demo_diff_no_context",
            "demo_diff_head",
            "demo_diff_tail",
            "demo_diff_multiple",
            "demo_diff_empty_old",
            "demo_diff_empty_new",
            "demo_diff_wrapped",
            "demo_diff_long_sides",
        ] {
            assert!(diff_ids.contains(id), "demo is missing diff case {id}");
        }
    }
}
