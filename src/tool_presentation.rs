use serde_json::Value;

use crate::config::DisplayConfig;

fn max_shell_command_lines(cfg: &DisplayConfig) -> usize {
    cfg.max_shell_command_lines
}
fn max_one_line_chars(cfg: &DisplayConfig) -> usize {
    cfg.max_one_line_chars
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

/// Split a tool label into (icon, text) parts.
///
/// Tool labels are always formatted as `"{icon} {text}"` where icon is the
/// emoji from `tool_emoji`. This splits at the first space so callers can
/// render the icon and text with different styles.
pub fn split_icon_from_label(label: &str) -> (&str, &str) {
    match label.find(' ') {
        Some(pos) => (&label[..pos], &label[pos + 1..]),
        None => (label, ""),
    }
}

/// Return the display emoji for a tool name.
pub fn tool_emoji(name: &str) -> &'static str {
    match name {
        "read" | "read_file" => "👀",
        "write" | "write_file" => "📄",
        "edit" | "edit_file" => "📝",
        "bash" | "cmd" | "powershell" | "exec" => "💻",
        "python" => "🐍",
        "find" | "find_files" => "🔍",
        "ask_user" => "❓",
        "read_skill" => "🎓",
        _ => "⚙️",
    }
}

/// Return the canonical display field name for a tool, matching its
/// [`Tool::streaming_field()`](crate::agent::types::Tool::streaming_field).
/// Returns `None` for custom tools that don't declare a streaming field.
pub fn tool_streaming_field(name: &str) -> Option<&'static str> {
    match name {
        "bash" | "cmd" | "powershell" => Some("command"),
        "python" => Some("script"),
        "exec" => Some("program"),
        "ask_user" => Some("question"),
        "read" | "read_file" => Some("path"),
        "write" | "write_file" => Some("path"),
        "edit" | "edit_file" => Some("path"),
        "find" | "find_files" => Some("pattern"),
        "read_skill" => Some("name"),
        _ => None,
    }
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

/// Build the user-facing tool invocation label.
///
/// Uses the tool's `streaming_field` to select which argument to display.
/// Applies per-tool formatting (multiline shell, shell-quoted exec, find_files
/// pattern+path, ask_user no-truncation, 1-line compact for others).
/// Always shows the **head** (beginning) of the selected value.
///
/// Returns `(label, is_placeholder)` where `is_placeholder` is `true` when
/// the target field has not yet arrived and the label is a pending action hint.
pub fn tool_invocation_label(
    name: &str,
    args: &Value,
    streaming_field: Option<&str>,
    display: &DisplayConfig,
) -> (String, bool) {
    let emoji = tool_emoji(name);

    // Shell and script tools: multiline invocation, head-truncated with continuation marker.
    if matches!(name, "bash" | "cmd" | "powershell" | "python") {
        let field = streaming_field.unwrap_or("command");
        let text = args.get(field).and_then(|v| v.as_str()).unwrap_or("");
        if text.is_empty() {
            return (tool_pending_label(name), true);
        }
        let detail = multiline_head_truncated(text, display);
        return (format!("{emoji} {detail}"), false);
    }

    // exec: full shell-quoted command (program + args).
    if name == "exec" {
        let detail = exec_command_detail(args, display);
        if detail.is_empty() {
            return (tool_pending_label(name), true);
        }
        return (format!("{emoji} {detail}"), false);
    }

    // ask_user: full question on one line, never truncated.
    if name == "ask_user" {
        let question = args.get("question").and_then(|v| v.as_str()).unwrap_or("");
        if question.is_empty() {
            return (tool_pending_label(name), true);
        }
        return (format!("{emoji} {}", one_line(question)), false);
    }

    // read_skill: just the skill name.
    if name == "read_skill" {
        let skill_name = args.get("name").and_then(|v| v.as_str()).unwrap_or("");
        if skill_name.is_empty() {
            return (tool_pending_label(name), true);
        }
        return (format!("{emoji} {skill_name}"), false);
    }

    // find_files: pattern and/or path.
    if matches!(name, "find" | "find_files") {
        let pattern = args.get("pattern").and_then(|v| v.as_str());
        let path = args.get("path").and_then(|v| v.as_str());
        let detail = match (pattern, path) {
            (Some(p), Some(d)) if !p.is_empty() && !d.is_empty() => {
                format!("{} in {}", compact(p, display), d)
            }
            (Some(p), _) if !p.is_empty() => compact(p, display),
            (_, Some(d)) if !d.is_empty() => format!("in {}", d),
            _ => String::new(),
        };
        if detail.is_empty() {
            return (tool_pending_label(name), true);
        }
        return (format!("{emoji} {detail}"), false);
    }

    // All other tools: extract the streaming_field value, 1-line compact.
    let Some(field) = streaming_field else {
        return (tool_pending_label(name), true);
    };

    let text = match args.get(field).and_then(|v| v.as_str()) {
        Some(t) if !t.is_empty() => t,
        _ => return (tool_pending_label(name), true),
    };

    let detail = compact(text, display);
    (format!("{emoji} {detail}"), false)
}

/// Build a display label from a partial JSON argument string.
///
/// Completes and parses the partial JSON, then delegates to
/// [`tool_invocation_label`] for consistent display logic.
pub fn tool_invocation_label_from_partial(
    name: &str,
    partial_json: &str,
    streaming_field: Option<&str>,
    display: &DisplayConfig,
) -> (String, bool) {
    let args = match jawohl::complete_json(partial_json)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
    {
        Some(v) => v,
        None => return (tool_pending_label(name), true),
    };
    tool_invocation_label(name, &args, streaming_field, display)
}

// ── Private formatting helpers ────────────────────────────────────────────────

fn one_line(input: &str) -> String {
    input.replace('\n', " ").trim().to_string()
}

fn compact(input: &str, display: &DisplayConfig) -> String {
    let max_chars = max_one_line_chars(display);
    let one_line = one_line(input);
    if one_line.chars().count() <= max_chars {
        return one_line;
    }
    one_line.chars().take(max_chars).collect::<String>() + "…"
}

fn multiline_head_truncated(input: &str, display: &DisplayConfig) -> String {
    let max_lines = max_shell_command_lines(display);
    let lines: Vec<&str> = input.lines().collect();
    if lines.is_empty() {
        return String::new();
    }

    let mut shown: Vec<String> = lines
        .iter()
        .take(max_lines)
        .map(|line| (*line).to_string())
        .collect();

    if lines.len() > max_lines {
        shown.push(format!("… {} total lines", lines.len()));
    }

    shown.join("\n")
}

fn exec_command_detail(args: &Value, display: &DisplayConfig) -> String {
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
        Ok(command) => compact(&command, display),
        Err(_) => compact(&words.join(" "), display),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sf(name: &str) -> Option<&'static str> {
        // streaming_field per tool, matching Tool implementations
        match name {
            "bash" | "cmd" | "powershell" => Some("command"),
            "exec" => Some("program"),
            "python" => Some("script"),
            "ask_user" => Some("question"),
            "read" | "read_file" => Some("path"),
            "write" | "write_file" => Some("path"),
            "edit" | "edit_file" => Some("path"),
            "find" | "find_files" => Some("pattern"),
            "read_skill" => Some("name"),
            _ => None,
        }
    }

    fn label(name: &str, args: &Value) -> (String, bool) {
        tool_invocation_label(name, args, sf(name), &DisplayConfig::default())
    }

    // ── Shell tools ───────────────────────────────────────────────────────────

    #[test]
    fn label_prefers_command() {
        let (lbl, ph) = label("bash", &json!({"command": "rg -n tool src"}));
        assert!(!ph);
        assert_eq!(lbl, "💻 rg -n tool src");
    }

    #[test]
    fn shell_label_preserves_newlines_up_to_five_lines() {
        let (lbl, ph) = label(
            "bash",
            &json!({"command": "printf 'one\ntwo\nthree\nfour\nfive'"}),
        );
        assert!(!ph);
        assert_eq!(lbl, "💻 printf 'one\ntwo\nthree\nfour\nfive'");
    }

    #[test]
    fn shell_label_truncates_after_five_lines_with_line_count() {
        let (lbl, ph) = label("bash", &json!({"command": "l1\nl2\nl3\nl4\nl5\nl6"}));
        assert!(!ph);
        assert_eq!(lbl, "💻 l1\nl2\nl3\nl4\nl5\n… 6 total lines");
    }

    #[test]
    fn shell_placeholder_when_command_empty() {
        let (lbl, ph) = label("bash", &json!({"command": ""}));
        assert!(ph);
        assert_eq!(lbl, "💻 running…");
    }

    #[test]
    fn shell_placeholder_when_command_missing() {
        let (lbl, ph) = label("bash", &json!({}));
        assert!(ph);
        assert_eq!(lbl, "💻 running…");
    }

    // ── exec ──────────────────────────────────────────────────────────────────

    #[test]
    fn exec_label_shows_full_shell_quoted_command() {
        let (lbl, ph) = label(
            "exec",
            &json!({
                "program": "printf",
                "args": ["%s %s", "hello world", "$PATH"]
            }),
        );
        assert!(!ph);
        assert_eq!(lbl, "💻 printf '%s %s' 'hello world' '$PATH'");
    }

    #[test]
    fn exec_label_uses_program_when_no_args() {
        let (lbl, ph) = label("exec", &json!({"program": "git"}));
        assert!(!ph);
        assert_eq!(lbl, "💻 git");
    }

    #[test]
    fn exec_placeholder_when_program_missing() {
        let (lbl, ph) = label("exec", &json!({}));
        assert!(ph);
        assert_eq!(lbl, "💻 running…");
    }

    // ── ask_user ──────────────────────────────────────────────────────────────

    #[test]
    fn ask_user_label_shows_full_question_without_ellipsis() {
        let question = "How do you want to run this triage session? Please choose Quick pass or Full pass, and optionally specify: item limit, include blocked items, and owner filter.";
        let (lbl, ph) = label("ask_user", &json!({"question": question}));
        assert!(!ph);
        assert_eq!(lbl, format!("❓ {question}"));
        assert!(!lbl.contains('…'));
    }

    #[test]
    fn ask_user_placeholder_when_question_empty() {
        let (lbl, ph) = label("ask_user", &json!({"question": ""}));
        assert!(ph);
        assert_eq!(lbl, "❓ asking…");
    }

    // ── find_files ────────────────────────────────────────────────────────────

    #[test]
    fn label_shows_pattern_and_path_for_find_files() {
        let (lbl, ph) = label(
            "find_files",
            &json!({"pattern": "src/**/*.rs", "path": "."}),
        );
        assert!(!ph);
        assert_eq!(lbl, "🔍 src/**/*.rs in .");
    }

    #[test]
    fn find_files_shows_pattern_only() {
        let (lbl, ph) = label("find_files", &json!({"pattern": "*.rs"}));
        assert!(!ph);
        assert_eq!(lbl, "🔍 *.rs");
    }

    #[test]
    fn find_files_shows_path_only() {
        let (lbl, ph) = label("find_files", &json!({"path": "src"}));
        assert!(!ph);
        assert_eq!(lbl, "🔍 in src");
    }

    #[test]
    fn find_files_placeholder_when_both_empty() {
        let (lbl, ph) = label("find_files", &json!({"pattern": "", "path": ""}));
        assert!(ph);
        assert_eq!(lbl, "🔍 finding…");
    }

    // ── read_skill ────────────────────────────────────────────────────────────

    #[test]
    fn read_skill_shows_name() {
        let (lbl, ph) = label("read_skill", &json!({"name": "workflow"}));
        assert!(!ph);
        assert_eq!(lbl, "🎓 workflow");
    }

    #[test]
    fn read_skill_placeholder_when_name_empty() {
        let (lbl, ph) = label("read_skill", &json!({"name": ""}));
        assert!(ph);
        assert_eq!(lbl, "🎓 working…");
    }

    // ── Read/write/edit tools ─────────────────────────────────────────────────

    #[test]
    fn label_avoids_raw_json() {
        let (lbl, ph) = label("read_file", &json!({"path": "src/main.rs", "limit": 20}));
        assert!(!ph);
        assert!(!lbl.contains('{'));
        assert!(!lbl.contains('}'));
        assert_eq!(lbl, "👀 src/main.rs");
    }

    #[test]
    fn write_file_shows_path_not_content() {
        // Regression: content must never appear in the headline for write_file.
        let (lbl, ph) = label(
            "write_file",
            &json!({"path": "/tmp/out.rs", "content": "fn main() {}"}),
        );
        assert!(!ph);
        assert_eq!(lbl, "📄 /tmp/out.rs");
    }

    #[test]
    fn write_file_placeholder_when_path_missing_but_content_present() {
        // During streaming, content may arrive before path — must show placeholder.
        let (lbl, ph) = label("write_file", &json!({"content": "fn main() {}"}));
        assert!(ph);
        assert_eq!(lbl, "📄 writing…");
    }

    #[test]
    fn edit_file_shows_path_not_old_text() {
        let (lbl, ph) = label(
            "edit_file",
            &json!({"path": "src/main.rs", "old_text": "a", "new_text": "b"}),
        );
        assert!(!ph);
        assert_eq!(lbl, "📝 src/main.rs");
    }

    #[test]
    fn edit_file_placeholder_when_path_missing() {
        let (lbl, ph) = label("edit_file", &json!({"old_text": "a", "new_text": "b"}));
        assert!(ph);
        assert_eq!(lbl, "📝 editing…");
    }

    // ── python ────────────────────────────────────────────────────────────────

    #[test]
    fn python_emoji() {
        let (lbl, ph) = label("python", &json!({"script": "print('hello')"}));
        assert!(!ph);
        assert!(lbl.starts_with("🐍"), "expected 🐍 prefix, got: {lbl}");
    }

    #[test]
    fn python_multiline_preserves_newlines() {
        let (lbl, ph) = label(
            "python",
            &json!({"script": "import time\nfor i in range(3):\n    print(i)"}),
        );
        assert!(!ph);
        assert_eq!(lbl, "🐍 import time\nfor i in range(3):\n    print(i)");
    }

    #[test]
    fn python_multiline_truncates_with_line_count() {
        let (lbl, ph) = label("python", &json!({"script": "l1\nl2\nl3\nl4\nl5\nl6\nl7"}));
        assert!(!ph);
        assert!(lbl.contains("… 7 total lines"), "got: {lbl}");
    }

    #[test]
    fn python_placeholder_when_script_empty() {
        let (lbl, ph) = label("python", &json!({"script": ""}));
        assert!(ph);
        assert_eq!(lbl, "🐍 running…");
    }

    #[test]
    fn python_placeholder_when_script_missing() {
        let (lbl, ph) = label("python", &json!({}));
        assert!(ph);
        assert_eq!(lbl, "🐍 running…");
    }

    // ── Custom / no streaming_field ───────────────────────────────────────────

    #[test]
    fn custom_tool_shows_placeholder() {
        let (lbl, ph) = tool_invocation_label(
            "custom_tool",
            &json!({"some_field": "value"}),
            None,
            &DisplayConfig::default(),
        );
        assert!(ph);
        assert_eq!(lbl, "⚙️ working…");
    }

    // ── Partial JSON wrapper ──────────────────────────────────────────────────

    #[test]
    fn partial_delegates_to_unified_label() {
        let (lbl, ph) = tool_invocation_label_from_partial(
            "write_file",
            r#"{"path": "/tmp/x.rs"#,
            Some("path"),
            &DisplayConfig::default(),
        );
        assert!(!ph);
        assert_eq!(lbl, "📄 /tmp/x.rs");
    }

    #[test]
    fn partial_shows_placeholder_when_field_absent() {
        let (lbl, ph) = tool_invocation_label_from_partial(
            "write_file",
            r#"{"content": "hello"#,
            Some("path"),
            &DisplayConfig::default(),
        );
        assert!(ph);
        assert_eq!(lbl, "📄 writing…");
    }

    #[test]
    fn partial_shows_placeholder_when_json_invalid() {
        let (lbl, ph) = tool_invocation_label_from_partial(
            "read_file",
            "not json at all",
            Some("path"),
            &DisplayConfig::default(),
        );
        assert!(ph);
        assert_eq!(lbl, "👀 reading…");
    }

    #[test]
    fn partial_exec_shows_full_command_when_args_present() {
        let (lbl, ph) = tool_invocation_label_from_partial(
            "exec",
            r#"{"program": "git", "args": ["log", "--oneline"]}"#,
            Some("program"),
            &DisplayConfig::default(),
        );
        assert!(!ph);
        assert_eq!(lbl, "💻 git log --oneline");
    }

    #[test]
    fn partial_exec_shows_placeholder_when_program_missing() {
        let (lbl, ph) = tool_invocation_label_from_partial(
            "exec",
            r#"{"args": ["log"]}"#,
            Some("program"),
            &DisplayConfig::default(),
        );
        assert!(ph);
        assert_eq!(lbl, "💻 running…");
    }

    #[test]
    fn partial_find_files_shows_pattern_and_path() {
        let (lbl, ph) = tool_invocation_label_from_partial(
            "find_files",
            r#"{"pattern": "*.rs", "path": "src"}"#,
            Some("pattern"),
            &DisplayConfig::default(),
        );
        assert!(!ph);
        assert_eq!(lbl, "🔍 *.rs in src");
    }

    #[test]
    fn partial_find_files_shows_placeholder_when_both_missing() {
        let (lbl, ph) = tool_invocation_label_from_partial(
            "find_files",
            r#"{}"#,
            Some("pattern"),
            &DisplayConfig::default(),
        );
        assert!(ph);
        assert_eq!(lbl, "🔍 finding…");
    }

    #[test]
    fn partial_python_multiline() {
        let (lbl, ph) = tool_invocation_label_from_partial(
            "python",
            r#"{"script": "import time\nfor i in range(3):\n    print(i)"}"#,
            Some("script"),
            &DisplayConfig::default(),
        );
        assert!(!ph);
        assert_eq!(lbl, "🐍 import time\nfor i in range(3):\n    print(i)");
    }

    // ── split_icon_from_label ─────────────────────────────────────────────────

    #[test]
    fn split_icon_from_label_splits_on_first_space() {
        let (icon, text) = split_icon_from_label("👀 reading…");
        assert_eq!(icon, "👀");
        assert_eq!(text, "reading…");
    }

    #[test]
    fn split_icon_from_label_no_space_returns_whole_label() {
        let (icon, text) = split_icon_from_label("⚙️");
        assert_eq!(icon, "⚙️");
        assert_eq!(text, "");
    }

    #[test]
    fn split_icon_from_label_variation_selector_emoji() {
        let (icon, text) = split_icon_from_label("⚙️ running…");
        assert_eq!(icon, "⚙️");
        assert_eq!(text, "running…");
    }
}
