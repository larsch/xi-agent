use std::{
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
};

use chrono::{Local, Utc};
use pulldown_cmark::{Options, Parser, html};

use crate::llm::{AssistantPhase, Message, Role};

pub fn default_export_filename() -> String {
    format!("tau-session-{}.html", Utc::now().format("%Y%m%d-%H%M%S"))
}

pub fn resolve_export_path(cwd: &str, requested: Option<&str>) -> PathBuf {
    match requested {
        Some(raw) => {
            let p = PathBuf::from(raw.trim());
            if p.is_absolute() {
                p
            } else {
                Path::new(cwd).join(p)
            }
        }
        None => Path::new(cwd).join(default_export_filename()),
    }
}

pub fn write_export_file(path: &Path, html: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, html)
}

pub fn build_session_export_html(
    messages: &[Message],
    cwd: &str,
    provider: &str,
    model: &str,
    session_id: Option<&str>,
) -> String {
    let mut out = String::with_capacity(messages.len().saturating_mul(640) + 2048);
    let generated_local = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();

    out.push_str("<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    out.push_str("<title>tau session export</title>\n<style>\n");
    out.push_str("*{box-sizing:border-box}body{margin:0;padding:0;background:#0b1020;color:#e6edf3;font:15px/1.5 -apple-system,BlinkMacSystemFont,Segoe UI,Roboto,Inter,Helvetica,Arial,sans-serif}main{max-width:980px;margin:0 auto;padding:24px}h1{margin:0 0 8px;font-size:22px}header{position:sticky;top:0;background:linear-gradient(180deg,#0b1020 85%,rgba(11,16,32,0));padding-bottom:12px;margin-bottom:8px}.meta{color:#9fb0c3;font-size:13px}.list{display:flex;flex-direction:column;gap:12px}.msg{border:1px solid #243047;border-radius:10px;overflow:hidden;background:#11182b}.msg .head{display:flex;justify-content:space-between;gap:8px;padding:8px 12px;font-size:12px;text-transform:uppercase;letter-spacing:.08em;color:#9fb0c3;border-bottom:1px solid #243047}.msg .body{padding:12px;word-break:break-word}.msg.user{border-color:#355783}.msg.user .head{background:#12213a}.msg.assistant{border-color:#265b4b}.msg.assistant .head{background:#102821}.msg.system{border-color:#5e4f1a}.msg.system .head{background:#2a220d}.msg.tool-call{border-color:#61408a}.msg.tool-call .head{background:#25153d}.msg.tool-result{border-color:#4b5f23}.msg.tool-result .head{background:#1b260f}.thinking{margin:0 0 10px;padding:10px;border-radius:8px;background:#1a2239;color:#b9c8d8;white-space:pre-wrap}.badge{padding:2px 8px;border-radius:999px;background:#2b3a56;color:#dbe7f3;font-size:11px}.muted{color:#9fb0c3}.markdown{line-height:1.6}.markdown p{margin:.4em 0 .8em}.markdown pre,.tool-output{margin:.6em 0;background:#0f172a;border:1px solid #22304f;border-radius:8px;padding:10px;overflow:auto}.markdown code,.tool-output{font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace}.markdown :not(pre)>code{background:#16213b;padding:1px 4px;border-radius:4px}.markdown blockquote{margin:.5em 0;padding:.3em .8em;border-left:3px solid #37507a;color:#b9c8d8}.markdown table{border-collapse:collapse;display:block;overflow:auto}.markdown th,.markdown td{border:1px solid #32435f;padding:4px 8px}.markdown a{color:#8dc2ff}.tool-json{margin-top:6px}.tool-json pre{margin:.4em 0;background:#0f172a;border:1px solid #22304f;border-radius:8px;padding:10px;overflow:auto;white-space:pre-wrap}@media (max-width:640px){main{padding:14px}}\n");
    out.push_str("</style>\n</head>\n<body>\n<main>\n<header>\n");
    out.push_str("<h1>tau session export</h1>\n<div class=\"meta\">\n");
    let _ = writeln!(out, "Generated: {}<br>", escape_html(&generated_local));
    let _ = writeln!(out, "Working directory: {}<br>", escape_html(cwd));
    let _ = writeln!(
        out,
        "Provider / model: {} / {}<br>",
        escape_html(provider),
        escape_html(model)
    );
    if let Some(id) = session_id {
        let _ = writeln!(out, "Session id: {}<br>", escape_html(id));
    }
    let _ = writeln!(
        out,
        "Messages: {}",
        messages.iter().filter(|m| !m.hidden).count()
    );
    out.push_str("</div>\n</header>\n<section class=\"list\">\n");

    for (idx, msg) in messages.iter().enumerate() {
        if msg.hidden {
            continue;
        }

        let (class, label) = match msg.role {
            Role::User => ("user", "user"),
            Role::Assistant => ("assistant", "assistant"),
            Role::System => ("system", "system"),
            Role::ToolCall => ("tool-call", "tool call"),
            Role::ToolResult => ("tool-result", "tool result"),
        };

        let _ = writeln!(out, "<article class=\"msg {}\">", class);
        out.push_str("<div class=\"head\"><span>");
        out.push_str(label);
        out.push_str("</span><span class=\"muted\">#");
        out.push_str(&(idx + 1).to_string());
        out.push_str("</span></div>\n<div class=\"body\">\n");

        match msg.role {
            Role::Assistant => {
                if let Some(thinking) = msg.thinking.as_deref()
                    && !thinking.trim().is_empty()
                {
                    out.push_str("<div class=\"thinking\"><strong>thinking</strong>\n");
                    out.push_str(&escape_html(thinking));
                    out.push_str("</div>\n");
                }
                if let Some(phase) = msg.assistant_phase {
                    let label = match phase {
                        AssistantPhase::Unknown => "unknown",
                        AssistantPhase::Provisional => "provisional",
                        AssistantPhase::Final => "final",
                    };
                    let _ = writeln!(out, "<div class=\"badge\">phase: {}</div>", label);
                }
                out.push_str(&markdown_to_safe_html(&msg.content));
            }
            Role::User | Role::System => {
                out.push_str(&markdown_to_safe_html(&msg.content));
            }
            Role::ToolCall => {
                if let Some(name) = msg.tool_name.as_deref() {
                    let _ = writeln!(
                        out,
                        "<div><strong>name:</strong> {}</div>",
                        escape_html(name)
                    );
                }
                if let Some(call_id) = msg.tool_call_id.as_deref() {
                    let _ = writeln!(
                        out,
                        "<div><strong>id:</strong> {}</div>",
                        escape_html(call_id)
                    );
                }
                if let Some(args) = msg.tool_args.as_ref() {
                    out.push_str("<div class=\"tool-json\"><strong>args</strong><pre>");
                    match serde_json::to_string_pretty(args) {
                        Ok(json) => out.push_str(&escape_html(&json)),
                        Err(_) => out.push_str(&escape_html(&args.to_string())),
                    }
                    out.push_str("</pre></div>");
                }
            }
            Role::ToolResult => {
                if let Some(call_id) = msg.tool_call_id.as_deref() {
                    let _ = writeln!(
                        out,
                        "<div><strong>id:</strong> {}</div>",
                        escape_html(call_id)
                    );
                }
                let status = if msg.is_error { "error" } else { "ok" };
                let _ = writeln!(out, "<div class=\"badge\">status: {status}</div>");
                out.push_str("<pre class=\"tool-output\">");
                out.push_str(&escape_html(&msg.content));
                out.push_str("</pre>");
            }
        }

        out.push_str("\n</div>\n</article>\n");
    }

    out.push_str("</section>\n</main>\n</body>\n</html>\n");
    out
}

fn markdown_to_safe_html(input: &str) -> String {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_FOOTNOTES);

    let parser = Parser::new_ext(input, options);
    let mut rendered = String::new();
    html::push_html(&mut rendered, parser);

    let clean = ammonia::Builder::default().clean(&rendered).to_string();

    format!("<div class=\"markdown\">{clean}</div>")
}

fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_content_is_rendered() {
        let html = build_session_export_html(
            &[Message::assistant("# Title\n\n`code`")],
            "/tmp",
            "copilot",
            "gpt-4o",
            None,
        );
        assert!(html.contains("<h1>Title</h1>"));
        assert!(html.contains("<code>code</code>"));
    }

    #[test]
    fn markdown_html_is_sanitized() {
        let html = build_session_export_html(
            &[Message::user("<script>alert(1)</script>ok")],
            "/tmp",
            "copilot",
            "gpt-4o",
            None,
        );
        assert!(!html.contains("<script>"));
        assert!(html.contains("ok"));
    }

    #[test]
    fn tool_result_is_preformatted() {
        let html = build_session_export_html(
            &[Message::tool_result("1", "line 1\n    line 2", false)],
            "/tmp",
            "copilot",
            "gpt-4o",
            None,
        );
        assert!(html.contains("<pre class=\"tool-output\">line 1\n    line 2</pre>"));
    }

    #[test]
    fn resolve_export_path_uses_cwd_for_relative_paths() {
        let p = resolve_export_path("/work", Some("out/session.html"));
        assert_eq!(p, Path::new("/work").join("out/session.html"));
    }
}
