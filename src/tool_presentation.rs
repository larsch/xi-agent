use serde_json::Value;

const MAX_MULTILINE_SHELL_COMMAND_LINES: usize = 5;
const MAX_ONE_LINE_CHARS: usize = 120;

/// Whether a tool uses head-truncation (show end while streaming, snap to
/// beginning when done) or tail-truncation (always show the trailing window).
fn uses_head_truncation(name: &str) -> bool {
    matches!(name, "bash" | "cmd" | "powershell" | "exec" | "ask_user")
}

/// Extract the display string for a partial JSON argument object.
///
/// Uses `jawohl::complete_json` to complete the partial JSON into a valid
/// document, parses it with `serde_json`, then extracts the target field.
/// Returns `None` if the field is not yet present or the JSON can't be
/// completed/parsed.
pub fn extract_partial_field(partial_json: &str, field: &str) -> Option<String> {
    let completed = jawohl::complete_json(partial_json).ok()?;
    let value: Value = serde_json::from_str(&completed).ok()?;
    value.get(field)?.as_str().map(|s| s.to_string())
}

/// Return a short pending action label shown before argument streaming begins.
///
/// Uses an action verb (e.g. "reading…") rather than the raw internal tool
/// name so the intent is clear and it visually reads as "in progress".
pub fn tool_pending_label(name: &str) -> String {
    let emoji = tool_emoji(name);
    let action = match name {
        "bash" | "cmd" | "powershell" | "exec" | "python" => "running…",
        "ask_user" => "asking…",
        "read" | "read_file" => "reading…",
        "write" | "write_file" => "writing…",
        "edit" | "edit_file" => "editing…",
        "find" | "find_files" => "finding…",
        _ => "working…",
    };
    format!("{emoji} {action}")
}

/// Build a display label for a tool call whose arguments are still streaming.
///
/// `partial_json` is the accumulated raw argument JSON so far.
/// `streaming_field` is the field name to extract for display (from
/// `ToolDefinition::streaming_field`). If `None`, falls back to the
/// completed-args display.
///
/// Returns `(label, is_placeholder)` where `is_placeholder` is `true` when
/// the target field has not yet arrived and the label is a pending action hint.
pub fn tool_invocation_label_partial(
    name: &str,
    partial_json: &str,
    streaming_field: Option<&str>,
) -> (String, bool) {
    let emoji = tool_emoji(name);

    let Some(field) = streaming_field else {
        return (tool_pending_label(name), true);
    };

    let text = match extract_partial_field(partial_json, field) {
        Some(t) => t,
        None => return (tool_pending_label(name), true),
    };

    if text.is_empty() {
        return (tool_pending_label(name), true);
    }

    let detail = if uses_head_truncation(name) {
        // Head-truncation: show the trailing N lines (newest content).
        head_truncate(&text)
    } else {
        // Tail-truncation: show the trailing N lines.
        tail_truncate(&text)
    };

    if detail.is_empty() {
        (tool_pending_label(name), true)
    } else {
        (format!("{emoji} {detail}"), false)
    }
}

/// Truncate to the last N lines for streaming display, prepending "…" if truncated.
/// Used for shell tools during streaming where total is not yet known.
fn head_truncate(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= MAX_MULTILINE_SHELL_COMMAND_LINES {
        return text.trim_end_matches('\n').to_string();
    }
    let start = lines.len() - MAX_MULTILINE_SHELL_COMMAND_LINES;
    let mut result = String::from("…\n");
    result.push_str(&lines[start..].join("\n"));
    result
}

/// Truncate to the last N lines with no leading marker.
fn tail_truncate(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= MAX_MULTILINE_SHELL_COMMAND_LINES {
        return text.trim_end_matches('\n').to_string();
    }
    let start = lines.len() - MAX_MULTILINE_SHELL_COMMAND_LINES;
    lines[start..].join("\n")
}

/// Return the display emoji for a tool name.
pub fn tool_emoji(name: &str) -> &'static str {
    match name {
        "read" | "read_file" => "👀",
        "write" | "write_file" => "📄",
        "edit" | "edit_file" => "📝",
        "bash" | "cmd" | "powershell" | "exec" => "💻",
        "find" | "find_files" => "🔍",
        "ask_user" => "❓",
        "read_skill" => "🎓",
        _ => "⚙️",
    }
}

/// Extract a short, human-readable tool argument summary.
///
/// Preference order matches the most meaningful fields shown to users.
pub fn tool_detail(name: &str, args: &Value) -> String {
    // Shell tool calls preserve newlines so users can see the full command,
    // up to a small fixed number of lines.
    if matches!(name, "bash" | "cmd" | "powershell")
        && let Some(command) = args.get("command").and_then(|v| v.as_str())
    {
        return multiline_shell_command(command);
    }

    // exec tool calls should display the full argv-style command rendered as a
    // shell-quoted string for readability.
    if name == "exec" {
        return exec_command_detail(args);
    }

    // ask_user questions must be shown in full (wrapped by the UI), never
    // truncated with an ellipsis.
    if name == "ask_user"
        && let Some(question) = args.get("question").and_then(|v| v.as_str())
    {
        return one_line(question);
    }

    // read_skill: show only the skill name.
    if name == "read_skill" {
        return args
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
    }

    // find_files: render pattern and/or path meaningfully.
    if matches!(name, "find" | "find_files") {
        let pattern = args.get("pattern").and_then(|v| v.as_str());
        let path = args.get("path").and_then(|v| v.as_str());
        return match (pattern, path) {
            (Some(p), Some(d)) => format!("{} in {}", compact(p), d),
            (Some(p), None) => compact(p),
            (None, Some(d)) => format!("in {}", d),
            (None, None) => String::new(),
        };
    }

    for key in ["command", "pattern", "path", "question", "prompt"] {
        if let Some(s) = args.get(key).and_then(|v| v.as_str()) {
            return compact(s);
        }
    }

    if let Some(obj) = args.as_object() {
        for value in obj.values() {
            if let Some(s) = value.as_str() {
                return compact(s);
            }
        }
    }

    String::new()
}

/// Build the user-facing one-line tool invocation label.
pub fn tool_invocation_label(name: &str, args: &Value) -> String {
    let emoji = tool_emoji(name);
    let detail = tool_detail(name, args);
    if detail.is_empty() {
        format!("{emoji} {name}")
    } else {
        format!("{emoji} {detail}")
    }
}

fn one_line(input: &str) -> String {
    input.replace('\n', " ").trim().to_string()
}

fn compact(input: &str) -> String {
    let one_line = one_line(input);
    if one_line.chars().count() <= MAX_ONE_LINE_CHARS {
        return one_line;
    }
    one_line
        .chars()
        .take(MAX_ONE_LINE_CHARS)
        .collect::<String>()
        + "…"
}

fn multiline_shell_command(input: &str) -> String {
    let lines: Vec<&str> = input.lines().collect();
    if lines.is_empty() {
        return String::new();
    }

    let mut shown: Vec<String> = lines
        .iter()
        .take(MAX_MULTILINE_SHELL_COMMAND_LINES)
        .map(|line| (*line).to_string())
        .collect();

    if lines.len() > MAX_MULTILINE_SHELL_COMMAND_LINES {
        shown.push(format!("… {} total lines", lines.len()));
    }

    shown.join("\n")
}

fn exec_command_detail(args: &Value) -> String {
    let Some(program) = args.get("program").and_then(|v| v.as_str()) else {
        return String::new();
    };

    let mut words = Vec::new();
    words.push(program);

    if let Some(argv) = args.get("args").and_then(|v| v.as_array()) {
        for arg in argv {
            if let Some(arg) = arg.as_str() {
                words.push(arg);
            }
        }
    }

    match shlex::try_join(words.iter().copied()) {
        Ok(command) => compact(&command),
        Err(_) => compact(&words.join(" ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn label_prefers_command() {
        let label = tool_invocation_label("bash", &json!({"command": "rg -n tool src"}));
        assert_eq!(label, "💻 rg -n tool src");
    }

    #[test]
    fn shell_label_preserves_newlines_up_to_five_lines() {
        let label = tool_invocation_label(
            "bash",
            &json!({"command": "printf 'one\ntwo\nthree\nfour\nfive'"}),
        );
        assert_eq!(label, "💻 printf 'one\ntwo\nthree\nfour\nfive'");
    }

    #[test]
    fn shell_label_truncates_after_five_lines_with_line_count() {
        let label = tool_invocation_label("bash", &json!({"command": "l1\nl2\nl3\nl4\nl5\nl6"}));
        assert_eq!(label, "💻 l1\nl2\nl3\nl4\nl5\n… 6 total lines");
    }

    #[test]
    fn exec_label_shows_full_shell_quoted_command() {
        let label = tool_invocation_label(
            "exec",
            &json!({
                "program": "printf",
                "args": ["%s %s", "hello world", "$PATH"]
            }),
        );
        assert_eq!(label, "💻 printf '%s %s' 'hello world' '$PATH'");
    }

    #[test]
    fn exec_label_uses_program_when_no_args() {
        let label = tool_invocation_label("exec", &json!({"program": "git"}));
        assert_eq!(label, "💻 git");
    }

    #[test]
    fn label_shows_pattern_and_path_for_find_files() {
        let label = tool_invocation_label(
            "find_files",
            &json!({"pattern": "src/**/*.rs", "path": "."}),
        );
        assert_eq!(label, "🔍 src/**/*.rs in .");
    }

    #[test]
    fn ask_user_label_shows_full_question_without_ellipsis() {
        let question = "How do you want to run this triage session? Please choose Quick pass or Full pass, and optionally specify: item limit, include blocked items, and owner filter.";
        let label = tool_invocation_label("ask_user", &json!({"question": question}));
        assert_eq!(label, format!("❓ {question}"));
        assert!(!label.contains('…'));
    }

    #[test]
    fn label_avoids_raw_json() {
        let label =
            tool_invocation_label("read_file", &json!({"path": "src/main.rs", "limit": 20}));
        assert!(!label.contains('{'));
        assert!(!label.contains('}'));
        assert_eq!(label, "👀 src/main.rs");
    }
}
