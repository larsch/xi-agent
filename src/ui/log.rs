use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use unicode_width::UnicodeWidthStr;

use crate::{
    llm::{AssistantPhase, Message, Role},
    tool_presentation,
};

use super::input::{normalize_terminal_segment, wrap_str};

pub(super) const USER_BG: Color = Color::Rgb(50, 50, 64);
const ASK_USER_BG: Color = Color::Rgb(27, 71, 31);

// ── ToolBodyConfig ────────────────────────────────────────────────────────────

/// Display configuration for tool body rendering.
///
/// All line-count limits apply to the visible window; when a body exceeds
/// the limit the overflow is replaced by a `... (N lines total)` marker.
/// Setting `full_output = true` disables all limits.
#[derive(Debug, Clone)]
pub struct ToolBodyConfig {
    /// Show untruncated output for all tools.
    pub full_output: bool,
    /// Max lines shown for head-truncated bodies (read_file, write_file, find_files).
    pub head_lines: usize,
    /// Max lines shown for tail-truncated bodies (bash, exec, custom).
    pub tail_lines: usize,
    /// Max lines per side for edit_file diff body.
    pub diff_lines: usize,
    /// Max lines shown for shell command intent (bash/cmd/powershell).
    /// Reserved for future use when streaming command intent body is implemented.
    #[allow(dead_code)]
    pub intent_shell_lines: usize,
}

impl Default for ToolBodyConfig {
    fn default() -> Self {
        Self {
            full_output: false,
            head_lines: 8,
            tail_lines: 8,
            diff_lines: 4,
            intent_shell_lines: 5,
        }
    }
}

// ── Main entry point ──────────────────────────────────────────────────────────

/// Apply uniform dim styling to all spans in a set of pre-rendered lines.
///
/// Used to render the "to be discarded" portion of the conversation log when
/// the user is in step-back mode.
/// Dim a colour by blending it toward a dark background.
fn dim_color(c: Color) -> Color {
    match c {
        Color::Rgb(r, g, b) => {
            // Blend 60 % toward a near-black neutral to reduce brightness while
            // keeping hue.  The result stays noticeably darker than normal but
            // not invisible.
            let blend = |v: u8| -> u8 { ((v as u16 * 40) / 100) as u8 };
            Color::Rgb(blend(r), blend(g), blend(b))
        }
        // For named colours fall back to a fixed muted grey.
        _ => Color::Rgb(80, 80, 90),
    }
}

pub(super) fn dim_lines(lines: Vec<Line<'static>>) -> Vec<Line<'static>> {
    lines
        .into_iter()
        .map(|line| {
            Line::from(
                line.spans
                    .into_iter()
                    .map(|span| {
                        let mut style = span.style;
                        // Dim fg: scale explicit colours down; for default-fg spans
                        // (plain text, model responses) apply a fixed muted grey so
                        // they are visibly dimmed rather than left at full brightness.
                        style = match style.fg {
                            Some(fg) => style.fg(dim_color(fg)),
                            None => style.fg(Color::Rgb(110, 110, 120)),
                        };
                        // Dim bg so user-message background blocks match the bar lines.
                        if let Some(bg) = style.bg {
                            style = style.bg(dim_color(bg));
                        }
                        Span::styled(span.content, style)
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect()
}

pub(super) fn build_log_lines(
    messages: &[Message],
    streaming: bool,
    width: usize,
    cfg: &ToolBodyConfig,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();

    for (idx, msg) in messages.iter().enumerate() {
        let is_last = idx == messages.len() - 1;

        match msg.role {
            Role::User => {
                if msg.hidden {
                    continue;
                }
                append_message(
                    &mut lines,
                    &sanitize_for_display(&msg.content),
                    "",
                    width,
                    true,
                );
            }
            Role::System => {}
            Role::Assistant => {
                let thinking = msg.thinking.as_deref().unwrap_or("");
                let is_streaming_last = streaming && is_last;
                let content = trim_assistant_block_edges(&msg.content);
                let has_answer = !content.is_empty();

                if !thinking.is_empty() {
                    let thinking_display = {
                        let sanitized = sanitize_for_display(thinking);
                        let all_lines: Vec<&str> = sanitized.lines().collect();
                        let skip = all_lines.len().saturating_sub(5);
                        format!("🧠 {}", all_lines[skip..].join("\n"))
                    };
                    append_message_dim(&mut lines, &thinking_display, "", width);
                    if has_answer {
                        lines.push(Line::default());
                    }
                }

                let effective_phase = match msg.assistant_phase {
                    Some(p) => p,
                    None if is_streaming_last => AssistantPhase::Unknown,
                    None => AssistantPhase::Final,
                };
                let answer_icon = match effective_phase {
                    AssistantPhase::Provisional => "💭",
                    AssistantPhase::Final => "💬",
                    AssistantPhase::Unknown if is_streaming_last => "💭",
                    AssistantPhase::Unknown => "💬",
                };

                if has_answer {
                    let prefix = format!("{answer_icon} ");
                    let md_lines = crate::markdown::render(&content, width, &prefix);
                    append_markdown_answer(&mut lines, md_lines, is_streaming_last);
                }
            }
            Role::ToolCall => {
                render_tool_call(messages, idx, width, cfg, &mut lines);
            }
            Role::ToolResult => {
                render_tool_result(messages, idx, width, cfg, &mut lines);
            }
        }
    }

    lines
}

// ── Tool call rendering ───────────────────────────────────────────────────────

fn render_tool_call(
    messages: &[Message],
    idx: usize,
    width: usize,
    cfg: &ToolBodyConfig,
    out: &mut Vec<Line<'static>>,
) {
    let msg = &messages[idx];
    let name = msg.tool_name.as_deref().unwrap_or("unknown");

    if name == "ask_user" {
        let args = msg.tool_args.as_ref();
        let context = args
            .and_then(|a| a.get("context"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let question = args
            .and_then(|a| a.get("question"))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        // Context: green bg, dimmed text, no icon.
        if let Some(ctx) = context {
            append_ask_user_block_dim(out, ctx, width, ASK_USER_BG);
        }

        // Question: green bg, normal text, ❓ icon.
        if !question.is_empty() {
            let md_lines = crate::markdown::render(question, width, "❓ ");
            append_ask_user_block_normal(out, md_lines, width, ASK_USER_BG);
        }

        // Response is rendered in render_tool_result; nothing more here.
        return;
    }

    // Regular tool call intent line.
    let label = if name == "local_shell" {
        let prefix = msg
            .tool_args
            .as_ref()
            .and_then(|a| a.get("prefix"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let command = msg
            .tool_args
            .as_ref()
            .and_then(|a| a.get("command"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if prefix.is_empty() {
            format!("⚙ {command}")
        } else {
            format!("⚙ {prefix} {command}")
        }
    } else if let Some(snapshot) = msg.tool_partial_snapshot.as_ref() {
        tool_presentation::tool_invocation_label(name, snapshot)
    } else if let Some(partial) = msg.tool_partial_args.as_deref() {
        tool_presentation::tool_invocation_label_partial(
            name,
            partial,
            msg.tool_streaming_field.as_deref(),
        )
    } else {
        match msg.tool_args.as_ref() {
            Some(args) => tool_presentation::tool_invocation_label(name, args),
            None => tool_presentation::tool_invocation_label(name, &serde_json::Value::Null),
        }
    };

    // For write_file: show the content body from tool args while streaming
    // (before result arrives). This is the intent body streaming case.
    // We only show it when there is NO following ToolResult yet; once the
    // result arrives the ToolResult handler shows the content.
    let show_write_intent_body = matches!(name, "write_file" | "write")
        && !matches!(
            messages.get(idx + 1),
            Some(next) if next.role == Role::ToolResult
        );

    // For edit_file: show the diff body while streaming, before the result
    // arrives. Same dual-source pattern as write_file.
    let show_edit_intent_body = matches!(name, "edit_file" | "edit")
        && !matches!(
            messages.get(idx + 1),
            Some(next) if next.role == Role::ToolResult
        );

    // Append read_file range suffix when result is available.
    let mut intent_label = label;
    if matches!(name, "read" | "read_file")
        && let Some(next) = messages.get(idx + 1)
        && next.role == Role::ToolResult
        && let Some(ref dr) = next.display_range
    {
        intent_label.push_str(&format!(
            " [{}-{}/{}]",
            dr.first_line, dr.last_line, dr.total_lines
        ));
    }

    let color = if name == "local_shell" {
        Color::LightBlue
    } else {
        Color::Cyan
    };
    append_message_colored(out, &intent_label, width, color);

    // Show streaming write_file intent body.
    // Content is available either from finalized tool_args or, while still
    // streaming, extracted from tool_partial_args so the body is visible
    // throughout streaming without any disappear/reappear flicker.
    if show_write_intent_body {
        let streaming_content = msg
            .tool_partial_snapshot
            .as_ref()
            .and_then(|a| a.get("content"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| {
                msg.tool_partial_args
                    .as_deref()
                    .and_then(|p| tool_presentation::extract_partial_field(p, "content"))
            });
        let content = msg
            .tool_args
            .as_ref()
            .and_then(|a| a.get("content"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or(streaming_content);
        if let Some(content) = content {
            let body_color = Color::Cyan;
            render_head_truncated_body(
                out,
                &content,
                cfg.head_lines,
                cfg.full_output,
                body_color,
                width,
            );
        }
    }

    // Show streaming edit_file diff body.
    // old_text and new_text are extracted from tool_partial_args during
    // streaming and from tool_args once finalized, so the diff is visible
    // throughout the entire stream without flicker.
    if show_edit_intent_body {
        let extract = |field: &str| -> Option<String> {
            msg.tool_args
                .as_ref()
                .and_then(|a| a.get(field))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .or_else(|| {
                    msg.tool_partial_snapshot
                        .as_ref()
                        .and_then(|a| a.get(field))
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                })
                .or_else(|| {
                    msg.tool_partial_args
                        .as_deref()
                        .and_then(|p| tool_presentation::extract_partial_field(p, field))
                })
        };
        let old_text = extract("old_text").unwrap_or_default();
        let new_text = extract("new_text").unwrap_or_default();
        if !old_text.is_empty() || !new_text.is_empty() {
            render_diff_body(
                out,
                &old_text,
                &new_text,
                cfg.diff_lines,
                cfg.full_output,
                width,
            );
        }
    }
}

// ── Tool result rendering ─────────────────────────────────────────────────────

fn render_tool_result(
    messages: &[Message],
    idx: usize,
    width: usize,
    cfg: &ToolBodyConfig,
    out: &mut Vec<Line<'static>>,
) {
    let msg = &messages[idx];
    let prev = messages.get(idx.saturating_sub(1));
    let prev_name = prev
        .filter(|p| p.role == Role::ToolCall)
        .and_then(|p| p.tool_name.as_deref())
        .unwrap_or("unknown");

    // ask_user: response is committed as part of the ToolCall rendering above.
    // Here we just append the response block (green bg, italic).
    if prev_name == "ask_user" {
        append_ask_user_response(out, &msg.content, width, ASK_USER_BG);
        return;
    }

    // local_shell: existing color treatment, tail-truncated.
    if prev_name == "local_shell" {
        let color = if msg.is_error {
            Color::LightRed
        } else {
            Color::LightBlue
        };
        let content = sanitize_for_display(&msg.content);
        render_tail_truncated_body(out, &content, cfg.tail_lines, cfg.full_output, color, width);
        return;
    }

    // edit_file: compact diff from tool args old_text/new_text.
    if matches!(prev_name, "edit" | "edit_file") {
        // If error, fall through to plain content rendering.
        if !msg.is_error {
            let old_text = prev
                .and_then(|p| p.tool_args.as_ref())
                .and_then(|a| a.get("old_text"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let new_text = prev
                .and_then(|p| p.tool_args.as_ref())
                .and_then(|a| a.get("new_text"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if !old_text.is_empty() || !new_text.is_empty() {
                render_diff_body(
                    out,
                    old_text,
                    new_text,
                    cfg.diff_lines,
                    cfg.full_output,
                    width,
                );
                return;
            }
        }
        // Fallthrough to plain content on error or missing args.
        let color = if msg.is_error {
            Color::Red
        } else {
            Color::Green
        };
        let content = sanitize_for_display(&msg.content);
        render_tail_truncated_body(out, &content, cfg.tail_lines, cfg.full_output, color, width);
        return;
    }

    // write_file: show written content from tool args (head-truncated).
    if matches!(prev_name, "write" | "write_file") && !msg.is_error {
        let content = prev
            .and_then(|p| p.tool_args.as_ref())
            .and_then(|a| a.get("content"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let color = Color::Green;
        render_head_truncated_body(out, content, cfg.head_lines, cfg.full_output, color, width);
        return;
    }

    // read_file / find_files: head-truncated.
    if matches!(prev_name, "read" | "read_file" | "find" | "find_files") {
        let color = if msg.is_error {
            Color::Red
        } else {
            Color::Green
        };
        let content = sanitize_for_display(&msg.content);
        render_head_truncated_body(out, &content, cfg.head_lines, cfg.full_output, color, width);
        return;
    }

    // bash / cmd / powershell / exec: tail-truncated.
    if matches!(prev_name, "bash" | "cmd" | "powershell" | "exec") {
        let color = if msg.is_error {
            Color::LightRed
        } else {
            Color::LightBlue
        };
        let content = sanitize_for_display(&msg.content);
        render_tail_truncated_body(out, &content, cfg.tail_lines, cfg.full_output, color, width);
        return;
    }

    // Custom / unknown tools: tail-truncated, green/red.
    let color = if msg.is_error {
        Color::Red
    } else {
        Color::Green
    };
    let content = sanitize_for_display(&msg.content);
    render_tail_truncated_body(out, &content, cfg.tail_lines, cfg.full_output, color, width);
}

// ── Body rendering helpers ────────────────────────────────────────────────────

/// Render head-truncated body: show first `max_lines` lines, then marker.
fn render_head_truncated_body(
    out: &mut Vec<Line<'static>>,
    content: &str,
    max_lines: usize,
    full_output: bool,
    color: Color,
    width: usize,
) {
    if content.trim().is_empty() {
        return;
    }
    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len();
    let limit = if full_output { total } else { max_lines };
    let shown = lines.iter().take(limit);
    for line in shown {
        let normalized = normalize_terminal_segment(line, 1);
        let chunks = wrap_str(&normalized, width.saturating_sub(1).max(1));
        for chunk in chunks {
            out.push(tool_result_line(chunk, color));
        }
    }
    if !full_output && total > max_lines {
        out.push(tool_result_line(
            format!("... ({total} lines total)"),
            color,
        ));
    }
}

/// Render tail-truncated body: show marker then last `max_lines` lines.
fn render_tail_truncated_body(
    out: &mut Vec<Line<'static>>,
    content: &str,
    max_lines: usize,
    full_output: bool,
    color: Color,
    width: usize,
) {
    if content.trim().is_empty() {
        return;
    }
    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len();
    let limit = if full_output { total } else { max_lines };
    if !full_output && total > max_lines {
        out.push(tool_result_line(
            format!("... ({total} lines total)"),
            color,
        ));
    }
    let start = if full_output || total <= max_lines {
        0
    } else {
        total - max_lines
    };
    for line in &lines[start..] {
        let normalized = normalize_terminal_segment(line, 1);
        let chunks = wrap_str(&normalized, width.saturating_sub(1).max(1));
        for chunk in chunks {
            out.push(tool_result_line(chunk, color));
        }
    }
    let _ = limit;
}

/// Render a compact diff body for edit_file.
fn render_diff_body(
    out: &mut Vec<Line<'static>>,
    old_text: &str,
    new_text: &str,
    max_lines_per_side: usize,
    full_output: bool,
    width: usize,
) {
    let old_lines: Vec<&str> = old_text.lines().collect();
    let new_lines: Vec<&str> = new_text.lines().collect();

    // Compute common head length.
    let common_head = old_lines
        .iter()
        .zip(new_lines.iter())
        .take_while(|(a, b)| a == b)
        .count();

    // Compute common tail length (must not overlap with head).
    let old_tail_max = old_lines.len().saturating_sub(common_head);
    let new_tail_max = new_lines.len().saturating_sub(common_head);
    let common_tail = old_lines[old_lines.len().saturating_sub(old_tail_max)..]
        .iter()
        .rev()
        .zip(
            new_lines[new_lines.len().saturating_sub(new_tail_max)..]
                .iter()
                .rev(),
        )
        .take_while(|(a, b)| a == b)
        .count();

    let old_diff = &old_lines[common_head..old_lines.len() - common_tail];
    let new_diff = &new_lines[common_head..new_lines.len() - common_tail];

    let old_total = old_diff.len();
    let new_total = new_diff.len();
    let old_limit = if full_output {
        old_total
    } else {
        max_lines_per_side
    };
    let new_limit = if full_output {
        new_total
    } else {
        max_lines_per_side
    };

    // Show hidden head marker.
    if common_head > 0 {
        out.push(Line::from(Span::styled(
            format!("  ... ({common_head} common lines hidden)"),
            Style::default().fg(Color::DarkGray),
        )));
    }

    // Old side (red, - prefix).
    for line in old_diff.iter().take(old_limit) {
        let text = format!("- {line}");
        let normalized = normalize_terminal_segment(&text, 0);
        let chunks = wrap_str(&normalized, width);
        for chunk in chunks {
            out.push(Line::from(Span::styled(
                chunk,
                Style::default().fg(Color::LightRed),
            )));
        }
    }
    if !full_output && old_total > max_lines_per_side {
        out.push(Line::from(Span::styled(
            format!("... ({old_total} lines total)"),
            Style::default().fg(Color::LightRed),
        )));
    }

    // New side (green, + prefix).
    for line in new_diff.iter().take(new_limit) {
        let text = format!("+ {line}");
        let normalized = normalize_terminal_segment(&text, 0);
        let chunks = wrap_str(&normalized, width);
        for chunk in chunks {
            out.push(Line::from(Span::styled(
                chunk,
                Style::default().fg(Color::LightGreen),
            )));
        }
    }
    if !full_output && new_total > max_lines_per_side {
        out.push(Line::from(Span::styled(
            format!("... ({new_total} lines total)"),
            Style::default().fg(Color::LightGreen),
        )));
    }

    // Show hidden tail marker.
    if common_tail > 0 {
        out.push(Line::from(Span::styled(
            format!("  ... ({common_tail} common lines hidden)"),
            Style::default().fg(Color::DarkGray),
        )));
    }
}

/// Build a single tool-result line with `│` marker.
fn tool_result_line(content: impl Into<String>, color: Color) -> Line<'static> {
    let style = Style::default().fg(color);
    Line::from(vec![
        Span::styled("│", style),
        Span::styled(content.into(), style),
    ])
}

// ── ask_user block helpers ────────────────────────────────────────────────────

/// Context block: green background, dimmed text, no icon.
fn append_ask_user_block_dim(out: &mut Vec<Line<'static>>, content: &str, width: usize, bg: Color) {
    let dim_bg_style = Style::default().bg(bg).add_modifier(Modifier::DIM);
    let padding_style = Style::default().bg(bg);
    let md_lines = crate::markdown::render(content, width, "");
    for line in md_lines {
        // Re-render with bg color and dim applied to each span.
        let dimmed: Vec<Span<'static>> = line
            .spans
            .into_iter()
            .map(|s| Span::styled(s.content, dim_bg_style.patch(s.style)))
            .collect();
        let text_width: usize = dimmed.iter().map(|s| s.content.width()).sum();
        let padding = width.saturating_sub(text_width);
        let mut spans = dimmed;
        if padding > 0 {
            spans.push(Span::styled(" ".repeat(padding), padding_style));
        }
        out.push(Line::from(spans));
    }
}

/// Question block: green background, normal text, icon already in md prefix.
fn append_ask_user_block_normal(
    out: &mut Vec<Line<'static>>,
    md_lines: Vec<Line<'static>>,
    width: usize,
    bg: Color,
) {
    let bg_style = Style::default().bg(bg);
    for line in md_lines {
        let colored: Vec<Span<'static>> = line
            .spans
            .into_iter()
            .map(|s| Span::styled(s.content, bg_style.patch(s.style)))
            .collect();
        let text_width: usize = colored.iter().map(|s| s.content.width()).sum();
        let padding = width.saturating_sub(text_width);
        let mut spans = colored;
        if padding > 0 {
            spans.push(Span::styled(" ".repeat(padding), bg_style));
        }
        out.push(Line::from(spans));
    }
}

/// Response block: green background, italic text.
fn append_ask_user_response(out: &mut Vec<Line<'static>>, content: &str, width: usize, bg: Color) {
    let bg_italic_style = Style::default().bg(bg).add_modifier(Modifier::ITALIC);
    let bg_style = Style::default().bg(bg);
    let sanitized = sanitize_for_display(content);
    let segments: Vec<&str> = sanitized.split('\n').collect();
    for seg in &segments {
        if seg.is_empty() {
            continue;
        }
        let chunks = wrap_str(seg, width);
        for chunk in chunks {
            let text_cols = chunk.as_str().width();
            let padding = width.saturating_sub(text_cols);
            let padded = format!("{chunk}{}", " ".repeat(padding));
            out.push(Line::from(Span::styled(padded, bg_italic_style)));
        }
    }
    let _ = bg_style;
}

// ── Shared rendering primitives ───────────────────────────────────────────────

fn trim_assistant_block_edges(text: &str) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let Some(start) = lines.iter().position(|line| !line.trim().is_empty()) else {
        return String::new();
    };
    let end = lines
        .iter()
        .rposition(|line| !line.trim().is_empty())
        .unwrap_or(start);

    let mut out = String::new();
    for (idx, line) in lines[start..=end].iter().enumerate() {
        if idx > 0 {
            out.push('\n');
        }
        let is_first = idx == 0;
        let is_last = start + idx == end;
        let rendered = if is_first && is_last {
            line.trim()
        } else if is_first {
            line.trim_start()
        } else if is_last {
            line.trim_end()
        } else {
            line
        };
        out.push_str(rendered);
    }

    out
}

fn halfblock_line(width: usize, ch: char, color: Color) -> Line<'static> {
    Line::from(Span::styled(
        ch.to_string().repeat(width),
        Style::default().fg(color),
    ))
}

fn append_message_colored(out: &mut Vec<Line<'static>>, content: &str, width: usize, color: Color) {
    let style = Style::default().fg(color);
    let segments: Vec<&str> = if content.is_empty() {
        vec![""]
    } else {
        content.split('\n').collect()
    };
    let visible: Vec<usize> = if content.is_empty() {
        vec![0]
    } else {
        segments
            .iter()
            .enumerate()
            .filter_map(|(idx, seg)| {
                let has_nonempty_after = segments.iter().skip(idx + 1).any(|s| !s.is_empty());
                if seg.is_empty() && !has_nonempty_after {
                    None
                } else {
                    Some(idx)
                }
            })
            .collect()
    };

    for seg_idx in visible {
        let normalized = normalize_terminal_segment(segments[seg_idx], 0);
        let chunks = wrap_str(&normalized, width);
        for chunk in chunks {
            out.push(Line::from(vec![Span::styled(chunk, style)]));
        }
    }
}

#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn append_tool_result_block(
    out: &mut Vec<Line<'static>>,
    content: &str,
    width: usize,
    color: Color,
) {
    let marker_style = Style::default().fg(color);
    let text_style = Style::default().fg(color);

    if content.is_empty() {
        let no_output_style = Style::default()
            .fg(Color::Rgb(100, 100, 120))
            .add_modifier(Modifier::ITALIC);
        out.push(Line::from(vec![
            Span::styled("│", marker_style),
            Span::styled("(no output)", no_output_style),
        ]));
        return;
    }

    if width == 0 {
        out.push(Line::from(vec![Span::styled(
            "│".to_string(),
            marker_style,
        )]));
        return;
    }

    let content_width = width.saturating_sub(1).max(1);
    let segments: Vec<&str> = content.split('\n').collect();
    let visible: Vec<usize> = segments
        .iter()
        .enumerate()
        .filter_map(|(idx, seg)| {
            let has_nonempty_after = segments.iter().skip(idx + 1).any(|s| !s.is_empty());
            if seg.is_empty() && !has_nonempty_after {
                None
            } else {
                Some(idx)
            }
        })
        .collect();

    for seg_idx in visible {
        let segment = segments[seg_idx];
        let normalized = normalize_terminal_segment(segment, 1);
        let chunks = wrap_str(&normalized, content_width);
        for chunk in chunks {
            out.push(Line::from(vec![
                Span::styled("│", marker_style),
                Span::styled(chunk, text_style),
            ]));
        }
    }
}

fn append_message_dim(
    out: &mut Vec<Line<'static>>,
    content: &str,
    suffix: &'static str,
    width: usize,
) {
    let dim_style = Style::default().fg(Color::DarkGray);
    let segments: Vec<&str> = if content.is_empty() {
        vec![""]
    } else {
        content.split('\n').collect()
    };
    let visible: Vec<usize> = if content.is_empty() {
        vec![0]
    } else {
        segments
            .iter()
            .enumerate()
            .filter_map(|(idx, seg)| {
                let has_nonempty_after = segments.iter().skip(idx + 1).any(|s| !s.is_empty());
                if seg.is_empty() && !has_nonempty_after {
                    None
                } else {
                    Some(idx)
                }
            })
            .collect()
    };
    let last_visible = visible.last().copied();

    for seg_idx in visible {
        let segment = segments[seg_idx];
        let is_last_visible_seg = Some(seg_idx) == last_visible;
        let normalized = normalize_terminal_segment(segment, 0);
        let chunks = wrap_str(&normalized, width);
        let last_chunk = chunks.len() - 1;

        for (chunk_idx, chunk) in chunks.iter().enumerate() {
            let is_last_chunk = chunk_idx == last_chunk;
            let show_suffix = !suffix.is_empty() && is_last_visible_seg && is_last_chunk;
            let mut spans: Vec<Span<'static>> = vec![Span::styled(chunk.clone(), dim_style)];
            if show_suffix {
                spans.push(Span::styled(suffix, dim_style));
            }
            out.push(Line::from(spans));
        }
    }
}

fn append_markdown_answer(
    out: &mut Vec<Line<'static>>,
    mut md_lines: Vec<Line<'static>>,
    streaming: bool,
) {
    if md_lines.is_empty() {
        if streaming {
            out.push(Line::from(Span::styled(
                "▋",
                Style::default().fg(Color::Yellow),
            )));
        }
        return;
    }

    if streaming {
        let last = md_lines.last_mut().unwrap();
        last.spans
            .push(Span::styled("▋", Style::default().fg(Color::Yellow)));
    }

    out.extend(md_lines);
}

fn append_message(
    out: &mut Vec<Line<'static>>,
    content: &str,
    suffix: &'static str,
    width: usize,
    user: bool,
) {
    let user_bg_style = Style::default().bg(USER_BG);
    let segments: Vec<&str> = if content.is_empty() {
        vec![""]
    } else {
        content.split('\n').collect()
    };
    let visible: Vec<usize> = if content.is_empty() {
        vec![0]
    } else {
        segments
            .iter()
            .enumerate()
            .filter_map(|(idx, seg)| {
                let has_nonempty_after = segments.iter().skip(idx + 1).any(|s| !s.is_empty());
                if seg.is_empty() && !has_nonempty_after {
                    None
                } else {
                    Some(idx)
                }
            })
            .collect()
    };
    let last_visible = visible.last().copied();

    if user {
        out.push(halfblock_line(width, '▄', USER_BG));
    }

    for seg_idx in visible {
        let segment = segments[seg_idx];
        let is_last_visible_seg = Some(seg_idx) == last_visible;
        let normalized = normalize_terminal_segment(segment, 0);
        let chunks = wrap_str(&normalized, width);
        let last_chunk = chunks.len() - 1;

        for (chunk_idx, chunk) in chunks.iter().enumerate() {
            let is_last_chunk = chunk_idx == last_chunk;
            let show_suffix = !suffix.is_empty() && is_last_visible_seg && is_last_chunk;

            if user {
                let text_cols = chunk.as_str().width();
                let padding = width.saturating_sub(text_cols);
                let padded = format!("{}{}", chunk, " ".repeat(padding));
                out.push(Line::from(Span::styled(padded, user_bg_style)));
            } else {
                let mut spans: Vec<Span<'static>> = vec![Span::raw(chunk.clone())];
                if show_suffix {
                    spans.push(Span::styled(suffix, Style::default().fg(Color::Yellow)));
                }
                out.push(Line::from(spans));
            }
        }
    }

    if user {
        out.push(halfblock_line(width, '▀', USER_BG));
    }
}

pub(super) fn sanitize_for_display(text: &str) -> String {
    let mut s = String::with_capacity(text.len());
    for line in text.split('\n') {
        s.push_str(line.trim_end());
        s.push('\n');
    }
    if s.ends_with('\n') {
        s.pop();
    }

    let s = s.trim_matches('\n');
    let mut result = String::with_capacity(s.len());
    let mut newline_run = 0usize;
    for ch in s.chars() {
        if ch == '\n' {
            newline_run += 1;
        } else {
            for _ in 0..newline_run.min(2) {
                result.push('\n');
            }
            newline_run = 0;
            result.push(ch);
        }
    }
    for _ in 0..newline_run.min(2) {
        result.push('\n');
    }
    result
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::{ToolBodyConfig, build_log_lines, dim_lines, trim_assistant_block_edges};
    use crate::llm::{AssistantPhase, DisplayRange, Message};
    use ratatui::{
        style::Color,
        text::{Line, Span},
    };

    #[test]
    fn dim_lines_dims_fg_proportionally() {
        use ratatui::style::Style;
        let lines = vec![Line::from(vec![Span::styled(
            "hello",
            Style::default().fg(Color::Rgb(200, 100, 50)),
        )])];
        let dimmed = dim_lines(lines);
        // 200 * 40/100 = 80, 100 * 40/100 = 40, 50 * 40/100 = 20
        assert_eq!(dimmed[0].spans[0].style.fg, Some(Color::Rgb(80, 40, 20)));
    }

    #[test]
    fn dim_lines_dims_bg_proportionally() {
        use ratatui::style::Style;
        // USER_BG = Rgb(50, 50, 64)
        let bg = Color::Rgb(50, 50, 64);
        let lines = vec![Line::from(vec![Span::styled(
            "hello",
            Style::default().bg(bg),
        )])];
        let dimmed = dim_lines(lines);
        // 50*40/100=20, 50*40/100=20, 64*40/100=25
        assert_eq!(dimmed[0].spans[0].style.bg, Some(Color::Rgb(20, 20, 25)));
    }

    #[test]
    fn dim_lines_bar_fg_and_text_bg_match() {
        use ratatui::style::Style;
        // Bar line: fg = USER_BG, no bg
        let user_bg = Color::Rgb(50, 50, 64);
        let bar_line = Line::from(vec![Span::styled("▄▄▄", Style::default().fg(user_bg))]);
        // Text line: bg = USER_BG, no fg
        let text_line = Line::from(vec![Span::styled("hello", Style::default().bg(user_bg))]);
        let dimmed = dim_lines(vec![bar_line, text_line]);
        let bar_fg = dimmed[0].spans[0].style.fg.unwrap();
        let text_bg = dimmed[1].spans[0].style.bg.unwrap();
        assert_eq!(
            bar_fg, text_bg,
            "bar fg and text bg must match after dimming"
        );
    }

    #[test]
    fn dim_lines_dims_plain_spans_with_fallback_grey() {
        let lines = vec![Line::from(vec![Span::raw("hello")])];
        let dimmed = dim_lines(lines);
        assert_eq!(
            dimmed[0].spans[0].style.fg,
            Some(Color::Rgb(110, 110, 120)),
            "plain spans must get fallback muted grey"
        );
    }

    #[test]
    fn dim_lines_preserves_span_content() {
        let lines = vec![Line::from(vec![Span::raw("hello")])];
        let dimmed = dim_lines(lines);
        assert_eq!(dimmed[0].spans[0].content, "hello");
    }

    fn cfg() -> ToolBodyConfig {
        ToolBodyConfig::default()
    }

    #[test]
    fn trim_assistant_block_edges_hides_outer_whitespace() {
        let rendered = trim_assistant_block_edges("\n  hello\n\n");
        assert_eq!(rendered, "hello");
    }

    #[test]
    fn trim_assistant_block_edges_preserves_interior_whitespace() {
        let rendered = trim_assistant_block_edges("\n\nfirst\n\n\nlast\n\n");
        assert_eq!(rendered, "first\n\n\nlast");
    }

    #[test]
    fn build_log_lines_hides_whitespace_only_streaming_assistant() {
        let mut msg = Message::assistant("\n   \n".to_string());
        msg.assistant_phase = Some(AssistantPhase::Provisional);
        let lines = build_log_lines(&[msg], true, 80, &cfg());
        assert!(lines.is_empty());
    }

    // ── read_file ─────────────────────────────────────────────────────────────

    #[test]
    fn read_file_result_head_truncated_to_8_lines() {
        let call = { Message::tool_call("c1", "read_file", serde_json::json!({"path": "foo.rs"})) };
        let content = (1..=20)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let result = Message::tool_result("c1", &content, false);
        let lines = build_log_lines(&[call, result], false, 120, &cfg());
        // 8 content lines + 1 marker = 9 body lines, plus 1 intent line = 10 total
        assert_eq!(lines.len(), 10);
        let text: Vec<String> = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        assert!(text.last().unwrap().contains("20 lines total"));
    }

    #[test]
    fn read_file_result_no_truncation_marker_when_within_limit() {
        let call = Message::tool_call("c1", "read_file", serde_json::json!({"path": "foo.rs"}));
        let content = "line1\nline2\nline3";
        let result = Message::tool_result("c1", content, false);
        let lines = build_log_lines(&[call, result], false, 120, &cfg());
        let text: Vec<String> = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        assert!(!text.iter().any(|t| t.contains("lines total")));
    }

    #[test]
    fn read_file_range_suffix_shown_when_display_range_present() {
        let call = Message::tool_call("c1", "read_file", serde_json::json!({"path": "foo.rs"}));
        let mut result = Message::tool_result("c1", "content", false);
        result.display_range = Some(DisplayRange {
            first_line: 1,
            last_line: 5,
            total_lines: 100,
        });
        let lines = build_log_lines(&[call, result], false, 120, &cfg());
        let intent = lines[0]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<String>();
        assert!(
            intent.contains("[1-5/100]"),
            "expected range suffix, got: {intent}"
        );
    }

    // ── find_files ────────────────────────────────────────────────────────────

    #[test]
    fn find_files_result_head_truncated() {
        let call = Message::tool_call("c1", "find_files", serde_json::json!({"pattern": "*.rs"}));
        let content = (1..=12)
            .map(|i| format!("src/file{i}.rs"))
            .collect::<Vec<_>>()
            .join("\n");
        let result = Message::tool_result("c1", &content, false);
        let lines = build_log_lines(&[call, result], false, 120, &cfg());
        let text: Vec<String> = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        assert!(text.last().unwrap().contains("12 lines total"));
    }

    // ── edit_file diff ────────────────────────────────────────────────────────

    #[test]
    fn edit_file_renders_diff_body() {
        let call = Message::tool_call(
            "c1",
            "edit_file",
            serde_json::json!({"path": "foo.rs", "old_text": "old line", "new_text": "new line"}),
        );
        let result = Message::tool_result("c1", "Successfully edited foo.rs", false);
        let lines = build_log_lines(&[call, result], false, 120, &cfg());
        let text: Vec<String> = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        assert!(text.iter().any(|t| t.starts_with("- ")), "expected - line");
        assert!(text.iter().any(|t| t.starts_with("+ ")), "expected + line");
    }

    #[test]
    fn edit_file_diff_truncated_per_side() {
        let old = (1..=6)
            .map(|i| format!("old{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let new = (1..=6)
            .map(|i| format!("new{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let call = Message::tool_call(
            "c1",
            "edit_file",
            serde_json::json!({"path": "foo.rs", "old_text": old, "new_text": new}),
        );
        let result = Message::tool_result("c1", "ok", false);
        let lines = build_log_lines(&[call, result], false, 120, &cfg());
        let text: Vec<String> = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        // Two truncation markers: one per side
        let marker_count = text.iter().filter(|t| t.contains("lines total")).count();
        assert_eq!(marker_count, 2);
    }

    #[test]
    fn edit_file_error_shows_plain_content() {
        let call = Message::tool_call(
            "c1",
            "edit_file",
            serde_json::json!({"path": "foo.rs", "old_text": "x", "new_text": "y"}),
        );
        let result = Message::tool_result("c1", "old_text not found", true);
        let lines = build_log_lines(&[call, result], false, 120, &cfg());
        let text: Vec<String> = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        assert!(text.iter().any(|t| t.contains("old_text not found")));
        assert!(!text.iter().any(|t| t.starts_with("- ")));
    }

    // ── bash tail truncation ──────────────────────────────────────────────────

    #[test]
    fn bash_result_tail_truncated() {
        let call = Message::tool_call("c1", "bash", serde_json::json!({"command": "seq 20"}));
        let content = (1..=20)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let result = Message::tool_result("c1", &content, false);
        let lines = build_log_lines(&[call, result], false, 120, &cfg());
        let text: Vec<String> = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        // Marker should be first body line (tail-truncated)
        let body: Vec<&String> = text.iter().skip(1).collect();
        assert!(
            body[0].contains("20 lines total"),
            "expected marker first, got: {}",
            body[0]
        );
        assert!(
            body.last().unwrap().contains("20"),
            "expected last line to be 20"
        );
    }

    // ── full_output toggle ────────────────────────────────────────────────────

    #[test]
    fn full_output_disables_truncation() {
        let call = Message::tool_call("c1", "read_file", serde_json::json!({"path": "foo.rs"}));
        let content = (1..=20)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let result = Message::tool_result("c1", &content, false);
        let full_cfg = ToolBodyConfig {
            full_output: true,
            ..ToolBodyConfig::default()
        };
        let lines = build_log_lines(&[call, result], false, 120, &full_cfg);
        let text: Vec<String> = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        assert!(!text.iter().any(|t| t.contains("lines total")));
        // 20 content lines + 1 intent = 21
        assert_eq!(lines.len(), 21);
    }

    // ── ask_user ──────────────────────────────────────────────────────────────

    #[test]
    fn ask_user_renders_while_pending() {
        let call = Message::tool_call(
            "c1",
            "ask_user",
            serde_json::json!({"question": "What do you want?"}),
        );
        // No following ToolResult.
        let lines = build_log_lines(&[call], false, 120, &cfg());
        let text: Vec<String> = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        assert!(
            text.iter().any(|t| t.contains("What do you want?")),
            "pending question not rendered"
        );
    }

    #[test]
    fn ask_user_renders_after_answer() {
        let call = Message::tool_call(
            "c1",
            "ask_user",
            serde_json::json!({"question": "What do you want?"}),
        );
        let result = Message::tool_result("c1", "Option A", false);
        let lines = build_log_lines(&[call, result], false, 120, &cfg());
        let text: Vec<String> = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        assert!(
            text.iter().any(|t| t.contains("What do you want?")),
            "question not rendered"
        );
        assert!(
            text.iter().any(|t| t.contains("Option A")),
            "response not rendered"
        );
    }
}
