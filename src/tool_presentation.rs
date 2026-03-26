use serde_json::Value;

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
pub fn tool_detail(args: &Value) -> String {
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
    let detail = tool_detail(args);
    if detail.is_empty() {
        format!("{} {name}", tool_emoji(name))
    } else {
        format!("{} {detail}", tool_emoji(name))
    }
}

fn compact(input: &str) -> String {
    const MAX_CHARS: usize = 120;
    let one_line = input.replace('\n', " ").trim().to_string();
    if one_line.chars().count() <= MAX_CHARS {
        return one_line;
    }
    one_line.chars().take(MAX_CHARS).collect::<String>() + "…"
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
    fn label_prefers_pattern_before_path() {
        let label = tool_invocation_label(
            "find_files",
            &json!({"pattern": "src/**/*.rs", "path": "."}),
        );
        assert_eq!(label, "🔍 src/**/*.rs");
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
