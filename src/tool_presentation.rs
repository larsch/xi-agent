use serde_json::Value;

const MAX_MULTILINE_SHELL_COMMAND_LINES: usize = 5;
const MAX_ONE_LINE_CHARS: usize = 120;

/// Return the display emoji for a tool name.
pub fn tool_emoji(name: &str) -> &'static str {
    match name {
        "read" | "read_file" => "👀",
        "write" | "write_file" => "✏️",
        "edit" | "edit_file" => "📝",
        "bash" | "cmd" | "powershell" => "💻",
        "find" | "find_files" => "🔍",
        "ask_user" => "❓",
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

    // ask_user questions must be shown in full (wrapped by the UI), never
    // truncated with an ellipsis.
    if name == "ask_user"
        && let Some(question) = args.get("question").and_then(|v| v.as_str())
    {
        return one_line(question);
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
    let detail = tool_detail(name, args);
    if detail.is_empty() {
        format!("{} {name}", tool_emoji(name))
    } else {
        format!("{} {detail}", tool_emoji(name))
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
    one_line.chars().take(MAX_ONE_LINE_CHARS).collect::<String>() + "…"
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
        shown.push("…".to_string());
    }

    shown.join("\n")
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
    fn shell_label_truncates_after_five_lines_with_standalone_ellipsis() {
        let label = tool_invocation_label(
            "bash",
            &json!({"command": "l1\nl2\nl3\nl4\nl5\nl6"}),
        );
        assert_eq!(label, "💻 l1\nl2\nl3\nl4\nl5\n…");
    }

    #[test]
    fn label_prefers_pattern_before_path() {
        let label = tool_invocation_label(
            "find_files",
            &json!({"pattern": "src/**/*.rs", "path": "."}),
        );
        assert_eq!(label, "🔍 src/**/*.rs");
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
