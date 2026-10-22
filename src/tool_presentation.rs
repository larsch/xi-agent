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
        "bash" | "cmd" | "powershell" | "exec" | "run_python" | "python_repl_execute" => "running…",
        "ask_user" => "asking…",
        "read" | "read_file" => "reading…",
        "write" | "write_file" => "writing…",
        "edit" | "edit_file" => "editing…",
        "find" | "find_files" => "finding…",
        "restart_host" => "restarting…",
        "agent_session" => "contacting worker…",
        _ if !is_builtin_tool(name) => return format!("{emoji} {name} · working…"),
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

/// Return whether a tool name belongs to the built-in presentation set.
fn is_builtin_tool(name: &str) -> bool {
    matches!(
        name,
        "read"
            | "read_file"
            | "write"
            | "write_file"
            | "edit"
            | "edit_file"
            | "bash"
            | "cmd"
            | "powershell"
            | "exec"
            | "run_python"
            | "python_repl_execute"
            | "find"
            | "find_files"
            | "ask_user"
            | "learn"
            | "restart_host"
            | "local_shell"
            | "agent_session"
    )
}

/// Return the display emoji for a tool name.
pub fn tool_emoji(name: &str) -> &'static str {
    match name {
        "read" | "read_file" => "👀",
        "write" | "write_file" => "📄",
        "edit" | "edit_file" => "📝",
        "bash" | "cmd" | "powershell" | "exec" => "💻",
        "run_python" | "python_repl_execute" => "🐍",
        "find" | "find_files" => "🔍",
        "ask_user" => "❓",
        "learn" => "🎓",
        "restart_host" => "🔄",
        "agent_session" => "🤝",
        _ => "🔧",
    }
}

/// Return the canonical display field name for a tool, matching its
/// [`Tool::streaming_field()`](crate::agent::types::Tool::streaming_field).
/// Returns `None` for custom tools that don't declare a streaming field.
pub fn tool_streaming_field(name: &str) -> Option<&'static str> {
    match name {
        "bash" | "cmd" | "powershell" => Some("command"),
        "run_python" => Some("script"),
        "python_repl_execute" => Some("code"),
        "ask_user" => Some("question"),
        "read" | "read_file" => Some("path"),
        "write" | "write_file" => Some("path"),
        "edit" | "edit_file" => Some("path"),
        "find" | "find_files" => Some("pattern"),
        "learn" => Some("name"),
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
/// Applies per-tool formatting (multiline shell and shell-quoted exec, find_files
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
    if matches!(
        name,
        "bash" | "cmd" | "powershell" | "run_python" | "python_repl_execute" | "exec"
    ) {
        let detail = if name == "exec" {
            exec_command_detail(args)
        } else {
            let field = streaming_field.unwrap_or("command");
            args.get(field)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        };
        if detail.is_empty() {
            return (tool_pending_label(name), true);
        }
        let detail = multiline_head_truncated(&detail, display);
        return (format!("{emoji} {detail}"), false);
    }

    if name == "agent_session" {
        let action = match args.get("action").and_then(Value::as_str) {
            Some("inspect") => "Inspect worker",
            Some("state") => "Check worker",
            Some("post_prompt") => "Send task",
            Some(_) => "Contact worker",
            None => return (tool_pending_label(name), true),
        };
        let cwd = args.get("cwd").and_then(Value::as_str).unwrap_or("");
        if cwd.is_empty() {
            return (format!("{emoji} {action}…"), true);
        }
        let mut label = format!(
            "{emoji} {action} · {}",
            crate::session_ipc::display_path(cwd)
        );
        if let Some(prompt) = args.get("prompt").and_then(Value::as_str)
            && !prompt.trim().is_empty()
        {
            label.push_str(&format!("\n{}", compact(prompt, display)));
        }
        return (label, false);
    }

    // ask_user: full question on one line, never truncated.
    if name == "ask_user" {
        let question = args.get("question").and_then(|v| v.as_str()).unwrap_or("");
        if question.is_empty() {
            return (tool_pending_label(name), true);
        }
        return (format!("{emoji} {}", one_line(question)), false);
    }

    // learn: just the skill name.
    if name == "learn" {
        let skill_name = args
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if skill_name.is_empty() {
            return (tool_pending_label(name), true);
        }
        return (format!("{emoji} {skill_name}"), false);
    }

    // find_files: pattern and/or path.
    if matches!(name, "find" | "find_files") {
        let pattern = args.get("pattern").and_then(|v| v.as_str()).map(str::trim);
        let path = args.get("path").and_then(|v| v.as_str()).map(str::trim);
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

    // Preserve the existing streaming-field behavior for built-ins that do not
    // have a specialized formatter above.
    if is_builtin_tool(name) {
        let Some(field) = streaming_field else {
            return (tool_pending_label(name), true);
        };
        let text = match args.get(field).and_then(|v| v.as_str()) {
            Some(t) if !t.is_empty() => t,
            _ => return (tool_pending_label(name), true),
        };
        return (format!("{emoji} {}", compact(text, display)), false);
    }

    // Generic tools: show their actual name and readable parameter values.
    let detail = generic_tool_detail(name, args, display);
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

/// Summarize session results without changing the model-visible tool output.
/// Unknown responses fall back to the original text rather than hiding information.
pub fn session_result_summary(action: &str, content: &str, is_error: bool) -> String {
    if is_error {
        let (code, _) = content.split_once(':').unwrap_or(("", content));
        let message = match code {
            "unavailable" => "No running worker found in this workspace",
            "timeout" => "No acknowledgement · the instruction may have been accepted; not retried",
            "connection_timeout" => "Timed out connecting to the worker",
            "permission_denied" => "Permission denied connecting to the worker",
            "connection_error" => "Could not connect to the worker",
            "control_owned" => "Worker is controlled by another client · instruction not accepted",
            _ => return content.to_string(),
        };
        return message.to_string();
    }
    let Ok(value) = serde_json::from_str::<Value>(content) else {
        return content.to_string();
    };
    match action {
        "inspect" if value.get("session_id").is_some() => {
            let mut summary = "Connected".to_string();
            if let Some(capabilities) = value.get("capabilities").and_then(Value::as_array) {
                let mut features = Vec::new();
                for (capability, label) in [
                    ("get_state", "status checks"),
                    ("post_prompt", "delegated prompts"),
                    ("completion_events", "completion notifications"),
                ] {
                    if capabilities.iter().any(|v| v.as_str() == Some(capability)) {
                        features.push(label);
                    }
                }
                if !features.is_empty() {
                    summary.push_str(&format!(" · supports {}", features.join(", ")));
                }
            }
            summary
        }
        "state" => {
            let status = match value.get("status").and_then(Value::as_str) {
                Some("idle") => "Idle",
                Some("looping") => "Working",
                _ => return content.to_string(),
            };
            let mut parts = vec![status.to_string()];
            if value.get("pending_ask").and_then(Value::as_bool) == Some(true) {
                parts.push("waiting for user input".to_string());
            } else if let Some(activity) = value.get("activity").and_then(Value::as_str) {
                parts.push(match activity {
                    "modelrequest" => "waiting for model".to_string(),
                    "localwork" => "executing tools".to_string(),
                    other => one_line(other),
                });
            }
            if let Some(count) = value.get("queued_steering_count").and_then(Value::as_u64)
                && count > 0
            {
                parts.push(format!(
                    "{count} queued instruction{}",
                    if count == 1 { "" } else { "s" }
                ));
            }
            parts.join(" · ")
        }
        "post_prompt" => match value.get("accepted").and_then(Value::as_bool) {
            Some(false) => "Instruction not accepted".to_string(),
            Some(true) => match value.get("mode").and_then(Value::as_str) {
                Some("prompt") => "Accepted · started a new task".to_string(),
                Some("steering") => {
                    "Accepted · queued instruction for the current task".to_string()
                }
                _ => "Instruction accepted".to_string(),
            },
            None => content.to_string(),
        },
        _ => content.to_string(),
    }
}

/// Format the existing IPC input envelope for display only. The original input
/// retains its turn identifier and payload for model correlation and persistence.
/// ponytail: recognizes the current text envelope; switch to structured display
/// metadata if IPC notifications gain another envelope or need richer interaction.
pub fn session_notification_display(content: &str) -> Option<String> {
    let (header, rest) = content.split_once('\n')?;
    let rest = rest.strip_prefix("Workspace: ")?;
    let (cwd, rest) = rest.split_once("\nTurn: ")?;
    let (_, body) = rest.split_once('\n')?;
    if header == "External worker event: control.revoked." {
        let payload: Value = serde_json::from_str(body.strip_prefix("Details: ")?).ok()?;
        let reason = match payload.get("reason").and_then(Value::as_str) {
            Some("user_took_control") => "🔔 Worker control returned to the user",
            _ => "🔔 Worker control revoked",
        };
        let mut text = format!("{reason} · {cwd}");
        if payload
            .get("pending_input_dropped")
            .and_then(Value::as_bool)
            == Some(true)
        {
            text.push_str("\n\nPending delegated input was dropped.");
        }
        return Some(text);
    }
    if let Some(status) = header
        .strip_prefix("External worker completed (")?
        .strip_suffix(").")
    {
        let label = match status {
            "success" => "✅ Worker finished",
            "interrupted" => "🔔 Worker interrupted",
            "error" => "🔔 Worker failed",
            _ => {
                return Some(format!(
                    "🔔 Worker ended ({status}) · {cwd}\n\n{}",
                    body.trim_start()
                ));
            }
        };
        return Some(format!("{label} · {cwd}\n\n{}", body.trim_start()));
    }
    None
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

fn generic_tool_detail(name: &str, args: &Value, display: &DisplayConfig) -> String {
    let Some(parameters) = args.as_object() else {
        return name.to_string();
    };

    let details = parameters
        .iter()
        .filter_map(|(key, value)| generic_parameter_value(value).map(|value| (key, value)))
        .map(|(key, value)| format!("{key}: {}", compact(&value, display)))
        .collect::<Vec<_>>()
        .join(", ");

    if details.is_empty() {
        name.to_string()
    } else {
        format!("{name} · {details}")
    }
}

/// Convert JSON parameter values to readable text without JSON syntax.
/// Nested object fields are omitted rather than displaying raw JSON.
fn generic_parameter_value(value: &Value) -> Option<String> {
    match value {
        Value::String(text) if !text.trim().is_empty() => Some(one_line(text)),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(boolean) => Some(boolean.to_string()),
        Value::Array(values) => {
            let values = values
                .iter()
                .filter_map(generic_parameter_value)
                .collect::<Vec<_>>();
            (!values.is_empty()).then(|| values.join(", "))
        }
        Value::Null | Value::Object(_) | Value::String(_) => None,
    }
}

fn multiline_head_truncated(input: &str, display: &DisplayConfig) -> String {
    let max_lines = max_shell_command_lines(display);
    let mut lines: Vec<&str> = input.lines().collect();

    // Universal rule: all rendered output is trimmed.
    // Strip leading and trailing empty lines so the emoji never sits alone.
    while lines.first() == Some(&"") {
        lines.remove(0);
    }
    while lines.last() == Some(&"") {
        lines.pop();
    }

    if lines.is_empty() {
        return String::new();
    }

    let total = lines.len();
    let mut shown: Vec<String> = lines
        .iter()
        .take(max_lines)
        .map(|line| (*line).to_string())
        .collect();

    if total > max_lines {
        shown.push(format!("… {total} total lines"));
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
        Ok(command) => command,
        Err(_) => words.join(" "),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn session_intents_are_readable_and_stream_incrementally() {
        let display = DisplayConfig::default();
        assert_eq!(tool_pending_label("agent_session"), "🤝 contacting worker…");
        for (action, verb) in [
            ("inspect", "Inspect worker"),
            ("state", "Check worker"),
            ("post_prompt", "Send task"),
        ] {
            let (label, placeholder) = tool_invocation_label(
                "agent_session",
                &json!({"action": action, "cwd": "D:\\work\\worker", "prompt": "Review\nthe changes"}),
                None,
                &display,
            );
            assert!(!placeholder);
            assert!(label.starts_with(&format!("🤝 {verb} · D:\\work\\worker")));
            assert!(label.ends_with("Review the changes"));
            assert!(!label.contains("agent_session"));
        }
        let (label, placeholder) = tool_invocation_label_from_partial(
            "agent_session",
            r#"{"action":"post_prompt","cwd":"/work/worker","prompt":"Review"#,
            None,
            &display,
        );
        assert!(!placeholder);
        assert!(label.contains("Send task · /work/worker"));
        assert!(label.contains("Review"));
    }

    #[test]
    fn session_results_distinguish_acceptance_state_and_errors() {
        for (action, value, expected) in [
            (
                "inspect",
                json!({"session_id": "secret", "pid": 1, "capabilities": ["get_state", "post_prompt"]}),
                "Connected · supports status checks, delegated prompts",
            ),
            (
                "state",
                json!({"status": "idle", "activity": null, "pending_ask": false}),
                "Idle",
            ),
            (
                "state",
                json!({"status": "looping", "activity": "localwork", "pending_ask": false, "queued_steering_count": 1}),
                "Working · executing tools · 1 queued instruction",
            ),
            (
                "state",
                json!({"status": "looping", "activity": "modelrequest", "pending_ask": true}),
                "Working · waiting for user input",
            ),
            (
                "post_prompt",
                json!({"accepted": true, "mode": "prompt", "turn_id": "secret"}),
                "Accepted · started a new task",
            ),
            (
                "post_prompt",
                json!({"accepted": true, "mode": "steering"}),
                "Accepted · queued instruction for the current task",
            ),
            (
                "post_prompt",
                json!({"accepted": false}),
                "Instruction not accepted",
            ),
        ] {
            assert_eq!(
                session_result_summary(action, &value.to_string(), false),
                expected
            );
        }
        assert_eq!(
            session_result_summary("inspect", "unavailable: secret pipe", true),
            "No running worker found in this workspace"
        );
        assert!(
            session_result_summary("post_prompt", "timeout: raw", true)
                .contains("may have been accepted")
        );
        assert_eq!(
            session_result_summary("state", "not JSON", false),
            "not JSON"
        );
        assert_eq!(
            session_result_summary("state", r#"{"status":"new"}"#, false),
            r#"{"status":"new"}"#
        );
        assert_eq!(
            session_result_summary("inspect", "unexpected failure", true),
            "unexpected failure"
        );
    }

    #[test]
    fn session_notifications_hide_metadata_but_keep_answers_and_failure_status() {
        for (status, label) in [
            ("success", "✅ Worker finished"),
            ("error", "🔔 Worker failed"),
            ("interrupted", "🔔 Worker interrupted"),
        ] {
            let raw = format!(
                "External worker completed ({status}).\nWorkspace: /work/worker\nTurn: secret\n\n**Answer**"
            );
            let shown = session_notification_display(&raw).unwrap();
            assert_eq!(shown, format!("{label} · /work/worker\n\n**Answer**"));
            assert!(raw.contains("secret"));
        }
        let raw = "External worker event: control.revoked.\nWorkspace: /work/worker\nTurn: secret\nDetails: {\"reason\":\"user_took_control\",\"pending_input_dropped\":true}";
        assert_eq!(
            session_notification_display(raw).unwrap(),
            "🔔 Worker control returned to the user · /work/worker\n\nPending delegated input was dropped."
        );
        assert!(session_notification_display("ordinary user input").is_none());
        assert!(session_notification_display("External worker event: unknown.\nWorkspace: /work/worker\nTurn: secret\nDetails: {}").is_none());
    }

    #[test]
    fn session_icons_have_two_column_width() {
        use unicode_width::UnicodeWidthStr;
        for icon in ["🤝", "🔔", "✅"] {
            assert_eq!(icon.width(), 2);
        }
    }

    fn sf(name: &str) -> Option<&'static str> {
        // streaming_field per tool, matching Tool implementations
        match name {
            "bash" | "cmd" | "powershell" => Some("command"),
            "exec" => Some("program"),
            "run_python" => Some("script"),
            "python_repl_execute" => Some("code"),
            "ask_user" => Some("question"),
            "read" | "read_file" => Some("path"),
            "write" | "write_file" => Some("path"),
            "edit" | "edit_file" => Some("path"),
            "find" | "find_files" => Some("pattern"),
            "learn" => Some("name"),
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
    fn exec_label_preserves_multiline_arguments_up_to_five_lines() {
        let (lbl, ph) = label(
            "exec",
            &json!({
                "program": "printf",
                "args": ["one\ntwo\nthree\nfour\nfive"]
            }),
        );
        assert!(!ph);
        assert_eq!(lbl, "💻 printf 'one\ntwo\nthree\nfour\nfive'");
    }

    #[test]
    fn exec_label_truncates_after_five_lines_with_line_count() {
        let (lbl, ph) = label(
            "exec",
            &json!({"program": "printf", "args": ["l1\nl2\nl3\nl4\nl5\nl6"]}),
        );
        assert!(!ph);
        assert_eq!(lbl, "💻 printf 'l1\nl2\nl3\nl4\nl5\n… 6 total lines");
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
    fn find_files_trims_path() {
        let (lbl, ph) = label("find_files", &json!({"path": "\n src\n"}));
        assert!(!ph);
        assert_eq!(lbl, "🔍 in src");
    }

    #[test]
    fn find_files_trims_pattern_and_path() {
        let (lbl, ph) = label(
            "find_files",
            &json!({"pattern": "\n *.rs \n", "path": "\n src \n"}),
        );
        assert!(!ph);
        assert_eq!(lbl, "🔍 *.rs in src");
    }

    #[test]
    fn find_files_placeholder_when_both_empty() {
        let (lbl, ph) = label("find_files", &json!({"pattern": "", "path": ""}));
        assert!(ph);
        assert_eq!(lbl, "🔍 finding…");
    }

    // ── learn ────────────────────────────────────────────────────────────

    #[test]
    fn learn_shows_name() {
        let (lbl, ph) = label("learn", &json!({"name": "workflow"}));
        assert!(!ph);
        assert_eq!(lbl, "🎓 workflow");
    }

    #[test]
    fn learn_trims_name() {
        let (lbl, ph) = label("learn", &json!({"name": "\n workflow\n"}));
        assert!(!ph);
        assert_eq!(lbl, "🎓 workflow");
    }

    #[test]
    fn learn_placeholder_when_name_empty() {
        let (lbl, ph) = label("learn", &json!({"name": ""}));
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
        let (lbl, ph) = label("run_python", &json!({"script": "print('hello')"}));
        assert!(!ph);
        assert!(lbl.starts_with("🐍"), "expected 🐍 prefix, got: {lbl}");
    }

    #[test]
    fn python_multiline_preserves_newlines() {
        let (lbl, ph) = label(
            "run_python",
            &json!({"script": "import time\nfor i in range(3):\n    print(i)"}),
        );
        assert!(!ph);
        assert_eq!(lbl, "🐍 import time\nfor i in range(3):\n    print(i)");
    }

    #[test]
    fn python_multiline_truncates_with_line_count() {
        let (lbl, ph) = label(
            "run_python",
            &json!({"script": "l1\nl2\nl3\nl4\nl5\nl6\nl7"}),
        );
        assert!(!ph);
        assert!(lbl.contains("… 7 total lines"), "got: {lbl}");
    }

    #[test]
    fn python_placeholder_when_script_empty() {
        let (lbl, ph) = label("run_python", &json!({"script": ""}));
        assert!(ph);
        assert_eq!(lbl, "🐍 running…");
    }

    #[test]
    fn python_placeholder_when_script_missing() {
        let (lbl, ph) = label("run_python", &json!({}));
        assert!(ph);
        assert_eq!(lbl, "🐍 running…");
    }

    #[test]
    fn python_trims_leading_newlines() {
        // Regression: scripts that start with \n would put the emoji alone
        // on the first line. Leading empty lines must be trimmed.
        let (lbl, ph) = label(
            "run_python",
            &json!({"script": "\nimport time\nprint('ok')"}),
        );
        assert!(!ph);
        assert_eq!(lbl, "🐍 import time\nprint('ok')");
    }

    #[test]
    fn python_trims_trailing_newlines() {
        // Trailing empty lines should also be stripped so no dangling empty
        // line appears after the last visible content.
        let (lbl, ph) = label(
            "run_python",
            &json!({"script": "print('ok')\nimport time\n\n"}),
        );
        assert!(!ph);
        assert_eq!(lbl, "🐍 print('ok')\nimport time");
    }

    #[test]
    fn shell_trims_leading_newlines() {
        let (lbl, ph) = label("bash", &json!({"command": "\n\necho hello"}));
        assert!(!ph);
        assert_eq!(lbl, "💻 echo hello");
    }

    // ── Custom / no streaming_field ───────────────────────────────────────────

    #[test]
    fn custom_tool_shows_name_and_readable_parameters() {
        let (lbl, ph) = tool_invocation_label(
            "web-search",
            &json!({"query": "weather in Paris", "limit": 3, "options": {"safe": true}}),
            None,
            &DisplayConfig::default(),
        );
        assert!(!ph);
        assert_eq!(lbl, "🔧 web-search · query: weather in Paris, limit: 3");
        assert!(!lbl.contains('{'));
        assert!(!lbl.contains('}'));
    }

    #[test]
    fn custom_tool_without_arguments_shows_name() {
        let (lbl, ph) =
            tool_invocation_label("web-search", &json!({}), None, &DisplayConfig::default());
        assert!(!ph);
        assert_eq!(lbl, "🔧 web-search");
    }

    #[test]
    fn custom_tool_pending_label_includes_name() {
        assert_eq!(tool_pending_label("web-search"), "🔧 web-search · working…");
    }

    #[test]
    fn partial_custom_tool_shows_readable_parameters() {
        let (lbl, ph) = tool_invocation_label_from_partial(
            "web-search",
            r#"{"query":"weather in Paris"}"#,
            None,
            &DisplayConfig::default(),
        );
        assert!(!ph);
        assert_eq!(lbl, "🔧 web-search · query: weather in Paris");
    }

    #[test]
    fn restart_host_shows_restarting_label() {
        let (lbl, ph) =
            tool_invocation_label("restart_host", &json!({}), None, &DisplayConfig::default());
        assert!(ph);
        assert_eq!(lbl, "🔄 restarting…");
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
            "run_python",
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
        let (icon, text) = split_icon_from_label("🔧");
        assert_eq!(icon, "🔧");
        assert_eq!(text, "");
    }

    #[test]
    fn split_icon_from_label_wide_symbol() {
        let (icon, text) = split_icon_from_label("🔧 running…");
        assert_eq!(icon, "🔧");
        assert_eq!(text, "running…");
    }
}
