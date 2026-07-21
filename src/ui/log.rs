use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use unicode_width::UnicodeWidthStr;

use crate::{
    llm::{AssistantPhase, Message, Role},
    mouse_select::LineSource,
    theme::Theme,
    tool_presentation,
};

use crate::config::DisplayConfig;

use super::input::{normalize_terminal_segment, wrap_str};

// ── ToolBodyConfig ────────────────────────────────────────────────────────────

/// Display configuration for tool body rendering.
///
/// All line-count limits apply to the visible window; when a body exceeds
/// the limit the overflow is replaced by a `… N total lines` marker.
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
}

impl Default for ToolBodyConfig {
    fn default() -> Self {
        Self {
            full_output: false,
            head_lines: 8,
            tail_lines: 8,
            diff_lines: 4,
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
    theme: &Theme,
    display: &DisplayConfig,
) -> (Vec<Line<'static>>, Vec<LineSource>) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut sources: Vec<LineSource> = Vec::new();

    /// Push [`LineSource`] entries for all lines added since `prev_len`,
    /// assigning them to `msg_idx` with the given properties.
    fn push_sources(
        sources: &mut Vec<LineSource>,
        lines: &[Line<'static>],
        prev_len: usize,
        msg_idx: usize,
        decoration_width: u16,
        streaming: bool,
    ) {
        let _ = msg_idx; // keep for caller readability
        for _ in prev_len..lines.len() {
            sources.push(LineSource {
                decoration_width,
                streaming,
            });
        }
    }

    for (idx, msg) in messages.iter().enumerate() {
        let is_last = idx == messages.len() - 1;
        let is_static_assistant_notice = matches!(msg.role, Role::Assistant)
            && msg.thinking.as_deref().unwrap_or("").is_empty()
            && msg.assistant_phase.is_none()
            && msg.content.starts_with('[')
            && msg.content.ends_with(']');
        let msg_streaming = streaming && is_last && !is_static_assistant_notice;

        match msg.role {
            Role::User => {
                if msg.hidden {
                    continue;
                }
                let user_bg = theme.log.user.bg.unwrap_or(Color::Rgb(50, 50, 64));
                let prev = lines.len();
                append_message_markdown(&mut lines, &msg.content, width, user_bg, &theme.markdown);
                push_sources(&mut sources, &lines, prev, idx, 0, msg_streaming);
            }
            Role::System => {}
            Role::Assistant => {
                let thinking = msg.thinking.as_deref().unwrap_or("");
                let is_streaming_last = msg_streaming;
                let content = trim_assistant_block_edges(&msg.content);
                let has_answer = !content.is_empty();

                if !thinking.is_empty() {
                    let thinking_display = {
                        let sanitized = sanitize_for_display(thinking);
                        let all_lines: Vec<&str> = sanitized.lines().collect();
                        let wrap_width = width.saturating_sub(3).max(1);
                        let mut wrapped: Vec<String> = Vec::new();
                        for logical in all_lines {
                            if logical.is_empty() {
                                wrapped.push(String::new());
                            } else {
                                wrapped.extend(wrap_str(logical, wrap_width));
                            }
                        }
                        let skip = wrapped.len().saturating_sub(5);
                        let shown = trim_empty_edges(&wrapped[skip..], |s| s.is_empty());
                        shown.join("\n")
                    };
                    let prev = lines.len();
                    append_message_colored(
                        &mut lines,
                        &format!("🧠 {}", thinking_display),
                        width,
                        Color::DarkGray,
                        false,
                        is_streaming_last && !has_answer,
                    );
                    push_sources(&mut sources, &lines, prev, idx, 3, msg_streaming);
                }

                let effective_phase = match msg.assistant_phase {
                    Some(p) => p,
                    None if is_streaming_last => AssistantPhase::Unknown,
                    None => AssistantPhase::Final,
                };
                let answer_icon = match effective_phase {
                    AssistantPhase::Provisional => theme
                        .log
                        .assistant
                        .provisional
                        .prefix
                        .text
                        .as_deref()
                        .unwrap_or("💭 ")
                        .trim_end(),
                    AssistantPhase::Final => theme
                        .log
                        .assistant
                        .r#final
                        .prefix
                        .text
                        .as_deref()
                        .unwrap_or("💬 ")
                        .trim_end(),
                    AssistantPhase::Unknown if is_streaming_last => theme
                        .log
                        .assistant
                        .provisional
                        .prefix
                        .text
                        .as_deref()
                        .unwrap_or("💭 ")
                        .trim_end(),
                    AssistantPhase::Unknown => theme
                        .log
                        .assistant
                        .r#final
                        .prefix
                        .text
                        .as_deref()
                        .unwrap_or("💬 ")
                        .trim_end(),
                };
                let deco_width = unicode_width::UnicodeWidthStr::width(answer_icon) as u16 + 1;

                if has_answer {
                    let md_width = width.saturating_sub(3).max(1);
                    let md_lines =
                        crate::markdown::render_with_theme(&content, md_width, "", &theme.markdown);
                    let prev = lines.len();
                    append_markdown_answer(&mut lines, answer_icon, md_lines, is_streaming_last);
                    push_sources(&mut sources, &lines, prev, idx, deco_width, msg_streaming);
                }
            }
            Role::ToolCall => {
                let prev = lines.len();
                render_tool_call(messages, idx, width, cfg, theme, display, &mut lines);
                push_sources(&mut sources, &lines, prev, idx, 3, msg_streaming);
            }
            Role::ToolResult => {
                let prev = lines.len();
                render_tool_result(messages, idx, width, cfg, theme, display, &mut lines);
                push_sources(&mut sources, &lines, prev, idx, 3, msg_streaming);
            }
        }
    }

    (lines, sources)
}

// ── Tool call rendering ───────────────────────────────────────────────────────

fn render_tool_call(
    messages: &[Message],
    idx: usize,
    width: usize,
    cfg: &ToolBodyConfig,
    theme: &Theme,
    display: &DisplayConfig,
    out: &mut Vec<Line<'static>>,
) {
    let msg = &messages[idx];
    let name = msg.tool_name.as_deref().unwrap_or("unknown");

    if name == "ask_user" {
        // During streaming, tool_args is still empty; extract question and context
        // from partial streaming data (same pattern as write_file/edit_file).
        let streaming_context = msg
            .tool_partial_snapshot
            .as_ref()
            .and_then(|a| a.get("context"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| {
                msg.tool_partial_args
                    .as_deref()
                    .and_then(|p| tool_presentation::extract_partial_field(p, "context"))
            });
        let streaming_question = msg
            .tool_partial_snapshot
            .as_ref()
            .and_then(|a| a.get("question"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| {
                msg.tool_partial_args
                    .as_deref()
                    .and_then(|p| tool_presentation::extract_partial_field(p, "question"))
            });

        let args = msg.tool_args.as_ref();
        let context = args
            .and_then(|a| a.get("context"))
            .and_then(|v| v.as_str())
            .or(streaming_context.as_deref())
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let question = args
            .and_then(|a| a.get("question"))
            .and_then(|v| v.as_str())
            .or(streaming_question.as_deref())
            .unwrap_or("");

        // Context always renders in the log — the selection header only
        // shows the question, so there is no duplication risk.
        if let Some(ctx) = context {
            append_ask_user_context_block(
                out,
                ctx,
                width,
                theme.log.ask_user.bg.unwrap_or(Color::Rgb(27, 71, 31)),
                theme,
                "📋 ",
            );
        }

        // The question always renders in the log body.
        if !question.is_empty() {
            let md_width = width.saturating_sub(3).max(1);
            let md_lines =
                crate::markdown::render_with_theme(question, md_width, "", &theme.markdown);
            append_markdown_answer(out, "❓", md_lines, false);
        }

        // Response is rendered in render_tool_result; nothing more here.
        return;
    }

    // Regular tool call intent line.
    let sf = msg
        .tool_streaming_field
        .as_deref()
        .or_else(|| tool_presentation::tool_streaming_field(name));

    let (label, is_placeholder) = if name == "local_shell" {
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
        let lbl = if prefix.is_empty() {
            format!("⚙ {command}")
        } else {
            format!("⚙ {prefix} {command}")
        };
        (lbl, false)
    } else if let Some(partial) = msg.tool_partial_args.as_deref() {
        let (lbl, placeholder) =
            tool_presentation::tool_invocation_label_from_partial(name, partial, sf, display);
        if placeholder {
            if let Some(snapshot) = msg.tool_partial_snapshot.as_ref() {
                // The latest partial JSON chunk couldn't be completed, but we
                // still have a valid snapshot from a previous frame.  Use it
                // so the headline doesn't blink back to a placeholder.
                tool_presentation::tool_invocation_label(name, snapshot, sf, display)
            } else {
                (lbl, placeholder)
            }
        } else {
            (lbl, placeholder)
        }
    } else {
        match msg.tool_args.as_ref() {
            Some(args) => tool_presentation::tool_invocation_label(name, args, sf, display),
            None => (tool_presentation::tool_pending_label(name), true),
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
        theme
            .tools
            .get(name)
            .headline_color()
            .unwrap_or(Color::Cyan)
    };
    if is_placeholder {
        // Render icon normally but text in italic+dim so the placeholder
        // nature is conveyed without distorting the emoji icon itself.
        let (icon, text) = tool_presentation::split_icon_from_label(&intent_label);
        if !text.is_empty() {
            append_message_colored_dim_with_icon(out, icon, text, width, color);
        } else {
            append_message_colored(out, &intent_label, width, color, true, false);
        }
    } else {
        append_message_colored(out, &intent_label, width, color, false, false);
    }

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
            let body_color = theme
                .tools
                .get("write_file")
                .body_color()
                .unwrap_or(Color::Cyan);
            render_head_truncated_body(
                out,
                &content,
                cfg.head_lines,
                cfg.full_output,
                body_color,
                width,
                true, // streaming — intent body, result not yet available
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
                theme,
            );
        }
    }

    // Show live subprocess output while the tool is still running (no result yet).
    if let Some(output) = msg.tool_running_output.as_deref()
        && !output.is_empty()
        && !matches!(
            messages.get(idx + 1),
            Some(next) if next.role == Role::ToolResult
        )
    {
        let body_color = theme.log.diff.unchanged.fg.unwrap_or(Color::DarkGray);
        render_tail_truncated_body(
            out,
            output,
            cfg.tail_lines,
            cfg.full_output,
            body_color,
            width,
            true, // streaming — subprocess still running
        );
    }
}

// ── Tool result rendering ─────────────────────────────────────────────────────

fn render_tool_result(
    messages: &[Message],
    idx: usize,
    width: usize,
    cfg: &ToolBodyConfig,
    theme: &Theme,
    _display: &DisplayConfig,
    out: &mut Vec<Line<'static>>,
) {
    let msg = &messages[idx];
    let prev = messages.get(idx.saturating_sub(1));
    let prev_name = prev
        .filter(|p| p.role == Role::ToolCall)
        .and_then(|p| p.tool_name.as_deref())
        .unwrap_or("unknown");

    // ask_user: response is committed as part of the ToolCall rendering above.
    // Here we just append the response block.
    if prev_name == "ask_user" {
        append_ask_user_response(
            out,
            &msg.content,
            width,
            theme.log.user.bg.unwrap_or(Color::Rgb(50, 50, 64)),
        );
        return;
    }

    // local_shell: existing color treatment, tail-truncated.
    if prev_name == "local_shell" {
        let color = if msg.is_error {
            theme.log.diff.removed.fg.unwrap_or(Color::LightRed)
        } else {
            Color::LightBlue
        };
        let content = sanitize_for_display(&msg.content);
        render_tail_truncated_body(
            out,
            &content,
            cfg.tail_lines,
            cfg.full_output,
            color,
            width,
            false,
        );
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
                    theme,
                );
                return;
            }
        }
        // Fallthrough to plain content on error or missing args.
        let color = if msg.is_error {
            theme.log.diff.removed.fg.unwrap_or(Color::Red)
        } else {
            theme.log.diff.added.fg.unwrap_or(Color::Green)
        };
        let content = sanitize_for_display(&msg.content);
        render_tail_truncated_body(
            out,
            &content,
            cfg.tail_lines,
            cfg.full_output,
            color,
            width,
            false,
        );
        return;
    }

    // write_file: show written content from tool args (head-truncated).
    if matches!(prev_name, "write" | "write_file") && !msg.is_error {
        let content = prev
            .and_then(|p| p.tool_args.as_ref())
            .and_then(|a| a.get("content"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let color = theme
            .tools
            .get(prev_name)
            .body_color()
            .unwrap_or(Color::Green);
        render_head_truncated_body(
            out,
            content,
            cfg.head_lines,
            cfg.full_output,
            color,
            width,
            false,
        );
        return;
    }

    // read_file / find_files: head-truncated.
    if matches!(prev_name, "read" | "read_file" | "find" | "find_files") {
        let color = if msg.is_error {
            theme.log.diff.removed.fg.unwrap_or(Color::Red)
        } else {
            theme
                .tools
                .get(prev_name)
                .body_color()
                .unwrap_or(Color::Green)
        };
        let content = sanitize_for_display(&msg.content);
        render_head_truncated_body(
            out,
            &content,
            cfg.head_lines,
            cfg.full_output,
            color,
            width,
            false,
        );
        return;
    }

    // read_skill: show only the invocation label (already rendered), no body.
    if prev_name == "read_skill" {
        return;
    }

    // bash / cmd / powershell / exec / python: tail-truncated.
    if matches!(
        prev_name,
        "bash" | "cmd" | "powershell" | "exec" | "run_python"
    ) {
        let color = if msg.is_error {
            theme.log.diff.removed.fg.unwrap_or(Color::LightRed)
        } else {
            theme
                .tools
                .get(prev_name)
                .body_color()
                .unwrap_or(Color::LightBlue)
        };
        let content = sanitize_for_display(&msg.content);
        render_tail_truncated_body(
            out,
            &content,
            cfg.tail_lines,
            cfg.full_output,
            color,
            width,
            false,
        );
        return;
    }

    // Custom / unknown tools: tail-truncated, green/red.
    let color = if msg.is_error {
        theme.log.diff.removed.fg.unwrap_or(Color::Red)
    } else {
        theme
            .tools
            .get(prev_name)
            .body_color()
            .unwrap_or(Color::Green)
    };
    let content = sanitize_for_display(&msg.content);
    render_tail_truncated_body(
        out,
        &content,
        cfg.tail_lines,
        cfg.full_output,
        color,
        width,
        false,
    );
}

// ── Body rendering helpers ────────────────────────────────────────────────────

/// Trim leading and trailing items for which `is_empty` returns true.
///
/// Empty items between non-empty items are preserved — only the edges are
/// trimmed.  Returns an empty slice when all items are empty.
fn trim_empty_edges<T>(slice: &[T], is_empty: impl Fn(&T) -> bool) -> &[T] {
    let start = slice
        .iter()
        .position(|x| !is_empty(x))
        .unwrap_or(slice.len());
    let end = slice
        .iter()
        .rposition(|x| !is_empty(x))
        .map(|i| i + 1)
        .unwrap_or(start);
    &slice[start..end]
}

/// A single wrapped (visual) line produced from a logical line of content.
struct WrappedLine {
    text: String,
    /// Which logical line this chunk belongs to (0-indexed).
    logical_idx: usize,
    /// First wrapped chunk of its logical line.
    is_first_chunk: bool,
    /// Last wrapped chunk of its logical line.
    is_last_chunk: bool,
}

/// Wrap every logical line in `content` to `width` columns, returning a flat
/// list of `WrappedLine` entries with logical-line metadata.
fn wrap_content(content: &str, width: usize) -> Vec<WrappedLine> {
    let mut out: Vec<WrappedLine> = Vec::new();
    for (li, line) in content.lines().enumerate() {
        let normalized = normalize_terminal_segment(line, 3);
        let chunks = wrap_str(&normalized, width);
        let chunk_count = chunks.len();
        for (ci, chunk) in chunks.into_iter().enumerate() {
            out.push(WrappedLine {
                text: chunk,
                logical_idx: li,
                is_first_chunk: ci == 0,
                is_last_chunk: ci == chunk_count - 1,
            });
        }
    }
    out
}

/// Render head-truncated body: show first `max_lines` wrapped lines, then truncation marker.
///
/// The limit is enforced on wrapped (visual) lines, not logical lines, so very
/// long logical lines that wrap to many visual lines are still bounded.
///
/// The first visible content line uses `╭` (the true start is shown).
/// The last content line uses `╰` (confirmed) or `┆` (streaming) when the body
/// is not truncated; truncated bodies end with a truncation marker.
/// A single-line body uses `·` (self-contained, no continuation implied).
/// Wrapped chunks of the same logical line continue with `│`.
fn render_head_truncated_body(
    out: &mut Vec<Line<'static>>,
    content: &str,
    max_lines: usize,
    full_output: bool,
    color: Color,
    width: usize,
    is_streaming: bool,
) {
    if content.trim().is_empty() {
        return;
    }
    let content_width = width.saturating_sub(3).max(1);
    let total_logical = content.lines().count();
    let wrapped = wrap_content(content, content_width);
    let total_wrapped = wrapped.len();

    let limit = if full_output {
        total_wrapped
    } else {
        max_lines
    };
    let truncated = !full_output && total_wrapped > max_lines;
    let shown = trim_empty_edges(&wrapped[..limit.min(total_wrapped)], |wl| {
        wl.text.is_empty()
    });

    for wl in shown {
        let is_first_logical = wl.logical_idx == 0 && wl.is_first_chunk;
        // We say the last logical line is visible only when all wrapped chunks
        // through the very end are displayed (no truncation).
        let is_last_logical = !truncated && wl.logical_idx + 1 == total_logical && wl.is_last_chunk;

        let marker = if is_last_logical && is_streaming {
            '┆'
        } else if is_first_logical && is_last_logical {
            '·'
        } else if is_last_logical {
            '╰'
        } else if is_first_logical && wl.is_first_chunk {
            '╭'
        } else {
            '│'
        };

        out.push(tool_result_line(marker, &wl.text, color));
    }

    if truncated {
        out.push(placeholder_result_line(
            format!("… {total_logical} total lines"),
            color,
        ));
    }
}

/// Render tail-truncated body: show truncation marker then last `max_lines` wrapped lines.
///
/// The limit is enforced on wrapped (visual) lines, not logical lines, so very
/// long logical lines that wrap to many visual lines are still bounded.
///
/// When a truncation marker precedes, the first visible content line uses `│`
/// (the true start is hidden).  Otherwise it uses `╭`.  The last content line
/// always uses `╰` (confirmed) or `┆` (streaming) — the end is always visible.
/// A single-line body uses `·` (self-contained, no continuation implied).
/// Wrapped chunks of the same logical line continue with `│`.
fn render_tail_truncated_body(
    out: &mut Vec<Line<'static>>,
    content: &str,
    max_lines: usize,
    full_output: bool,
    color: Color,
    width: usize,
    is_streaming: bool,
) {
    if content.trim().is_empty() {
        return;
    }
    let content_width = width.saturating_sub(3).max(1);
    let total_logical = content.lines().count();
    let wrapped = wrap_content(content, content_width);
    let total_wrapped = wrapped.len();

    let truncated = !full_output && total_wrapped > max_lines;
    if truncated {
        out.push(placeholder_result_line(
            format!("… {total_logical} total lines"),
            color,
        ));
    }
    let start = if full_output || total_wrapped <= max_lines {
        0
    } else {
        total_wrapped - max_lines
    };
    let shown = trim_empty_edges(&wrapped[start..], |wl| wl.text.is_empty());

    for wl in shown {
        // `is_first_logical` is true only when the first wrapped chunk of the
        // very first logical line is visible AND no truncation hides any
        // earlier content.
        let is_first_logical = !truncated && wl.logical_idx == 0 && wl.is_first_chunk;
        // Tail-truncated always shows through the end, so the last logical
        // line's last chunk marks the true end.
        let is_last_logical = wl.logical_idx + 1 == total_logical && wl.is_last_chunk;

        let marker = if is_last_logical && is_streaming {
            '┆'
        } else if is_first_logical && is_last_logical {
            '·'
        } else if is_last_logical {
            '╰'
        } else if is_first_logical && wl.is_first_chunk {
            '╭'
        } else {
            '│'
        };

        out.push(tool_result_line(marker, &wl.text, color));
    }
}

/// Render a compact diff body for edit_file.
///
/// The per-side line limit is enforced on wrapped (visual) lines, not logical
/// lines, so very long logical lines that wrap to many visual lines are still
/// bounded.
fn render_diff_body(
    out: &mut Vec<Line<'static>>,
    old_text: &str,
    new_text: &str,
    max_lines_per_side: usize,
    full_output: bool,
    width: usize,
    theme: &Theme,
) {
    let removed_color = theme.log.diff.removed.fg.unwrap_or(Color::LightRed);
    let added_color = theme.log.diff.added.fg.unwrap_or(Color::LightGreen);
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

    let is_pure_addition = old_total == 0;
    let is_pure_removal = new_total == 0;

    let content_width = width.saturating_sub(3).max(1);

    // Helper: push a combined "total + common" filler when both apply.
    let push_total_common =
        |out: &mut Vec<Line<'static>>, total: usize, common: usize, color: Color| {
            if total > 0 && common > 0 {
                out.push(placeholder_result_line(
                    format!("… {total} total lines + {common} common lines"),
                    color,
                ));
            } else if total > 0 {
                out.push(placeholder_result_line(
                    format!("… {total} total lines"),
                    color,
                ));
            } else if common > 0 {
                out.push(placeholder_result_line(
                    format!("… {common} common lines"),
                    color,
                ));
            }
        };

    // Helper: render a slice of logical lines as wrapped lines, limiting at
    // the wrapped level. Pushes rendered lines to `out`.
    let render_diff_block = |out: &mut Vec<Line<'static>>, diff_lines: &[&str], color: Color| {
        for line in diff_lines {
            let normalized = normalize_terminal_segment(line, 3);
            let chunks = wrap_str(&normalized, content_width);
            for chunk in chunks {
                out.push(tool_result_line('│', chunk, color));
            }
        }
    };

    // Helper: render a slice of logical lines with a wrapped-line limit.
    // Stops once `max_wrapped` wrapped lines have been emitted.
    let render_diff_block_limited =
        |out: &mut Vec<Line<'static>>, diff_lines: &[&str], max_wrapped: usize, color: Color| {
            let mut emitted = 0usize;
            for line in diff_lines {
                if emitted >= max_wrapped {
                    break;
                }
                let normalized = normalize_terminal_segment(line, 3);
                let chunks = wrap_str(&normalized, content_width);
                for chunk in chunks {
                    if emitted >= max_wrapped {
                        break;
                    }
                    out.push(tool_result_line('│', chunk, color));
                    emitted += 1;
                }
            }
        };

    // Removed block (omit common-line placeholders when it's a pure addition).
    if old_total > 0 {
        if common_head > 0 && !is_pure_removal {
            out.push(placeholder_result_line(
                format!("… {common_head} common lines"),
                removed_color,
            ));
        }
        if full_output {
            render_diff_block(out, old_diff, removed_color);
        } else {
            render_diff_block_limited(out, old_diff, max_lines_per_side, removed_color);
        }
        let truncated = !full_output && old_total > max_lines_per_side;
        let total_filler = if truncated { old_total } else { 0 };
        let common_filler = if common_tail > 0 && !is_pure_removal {
            common_tail
        } else {
            0
        };
        push_total_common(out, total_filler, common_filler, removed_color);
    }

    // Added block (omit common-line placeholders when it's a pure removal).
    if new_total > 0 {
        if common_head > 0 && !is_pure_addition {
            out.push(placeholder_result_line(
                format!("… {common_head} common lines"),
                added_color,
            ));
        }
        if full_output {
            render_diff_block(out, new_diff, added_color);
        } else {
            render_diff_block_limited(out, new_diff, max_lines_per_side, added_color);
        }
        let truncated = !full_output && new_total > max_lines_per_side;
        let total_filler = if truncated { new_total } else { 0 };
        let common_filler = if common_tail > 0 && !is_pure_addition {
            common_tail
        } else {
            0
        };
        push_total_common(out, total_filler, common_filler, added_color);
    }
}

/// Build a body content line with a block-drawing margin marker at column 1.
///
/// Layout: `·` at column 0, marker at column 1, `·` at column 2, content from
/// column 3 onward.  Both the marker prefix and the content share `color`.
fn tool_result_line(marker: char, content: impl Into<String>, color: Color) -> Line<'static> {
    let style = Style::default().fg(color);
    Line::from(vec![
        Span::styled(format!(" {} ", marker), style),
        Span::styled(content.into(), style),
    ])
}

/// Build a truncation/context placeholder line with a `┆` margin marker at
/// column 1.  The marker is rendered in `color`; the text is rendered in
/// `color` + dim + italic.
fn placeholder_result_line(text: impl Into<String>, color: Color) -> Line<'static> {
    let marker_style = Style::default().fg(color);
    let text_style = Style::default()
        .fg(color)
        .add_modifier(Modifier::ITALIC | Modifier::DIM);
    Line::from(vec![
        Span::styled(" ┆ ", marker_style),
        Span::styled(text.into(), text_style),
    ])
}

// ── ask_user block helpers ────────────────────────────────────────────────────

/// Context block: green background, readable text, with an emoji prefix
/// (e.g. "📋 ").  No DIM — the green background alone distinguishes it from
/// surrounding content.
fn append_ask_user_context_block(
    out: &mut Vec<Line<'static>>,
    content: &str,
    width: usize,
    bg: Color,
    theme: &Theme,
    emoji: &str,
) {
    let bg_style = Style::default().bg(bg);
    let padding_style = Style::default().bg(bg);
    let md_lines = crate::markdown::render_with_theme(content, width, emoji, &theme.markdown);
    for line in md_lines {
        let styled: Vec<Span<'static>> = line
            .spans
            .into_iter()
            .map(|s| Span::styled(s.content, bg_style.patch(s.style)))
            .collect();
        let text_width: usize = styled.iter().map(|s| s.content.width()).sum();
        let padding = width.saturating_sub(text_width);
        let mut spans = styled;
        if padding > 0 {
            spans.push(Span::styled(" ".repeat(padding), padding_style));
        }
        out.push(Line::from(spans));
    }
}

/// Response block: rendered like a normal user message but with the ask_user background color.
fn append_ask_user_response(out: &mut Vec<Line<'static>>, content: &str, width: usize, bg: Color) {
    let bg_style = Style::default().bg(bg);
    let sanitized = sanitize_for_display(content);
    let segments: Vec<&str> = sanitized.split('\n').collect();
    let visible = visible_segments(&segments);

    out.push(halfblock_line(width, '▄', bg));

    for seg_idx in visible {
        let segment = segments[seg_idx];
        let normalized = normalize_terminal_segment(segment, 0);
        let chunks = wrap_str(&normalized, width);
        for chunk in chunks {
            let text_cols = chunk.as_str().width();
            let padding = width.saturating_sub(text_cols);
            let padded = format!("{}{}", chunk, " ".repeat(padding));
            out.push(Line::from(Span::styled(padded, bg_style)));
        }
    }

    out.push(halfblock_line(width, '▀', bg));
}

// ── Shared rendering primitives ───────────────────────────────────────────────

/// Return the indices of segments to keep: strip leading/trailing empty
/// lines while preserving interior empty lines. An empty input returns an
/// empty vector so that callers can iterate directly without a sentinel.
fn visible_segments(segments: &[&str]) -> Vec<usize> {
    segments
        .iter()
        .enumerate()
        .filter(|(idx, seg)| {
            if !seg.is_empty() {
                return true;
            }
            let has_nonempty_before = segments[..*idx].iter().any(|s| !s.is_empty());
            let has_nonempty_after = segments[idx + 1..].iter().any(|s| !s.is_empty());
            has_nonempty_before && has_nonempty_after
        })
        .map(|(idx, _)| idx)
        .collect()
}

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

fn append_message_colored(
    out: &mut Vec<Line<'static>>,
    content: &str,
    width: usize,
    color: Color,
    dim: bool,
    streaming: bool,
) {
    let mut style = Style::default().fg(color);
    if dim {
        style = style.add_modifier(Modifier::ITALIC | Modifier::DIM);
    }
    let segments: Vec<&str> = content.split('\n').collect();
    let visible = visible_segments(&segments);
    let content_width = width.saturating_sub(3).max(1);
    let last_visible_idx = visible.len() - 1;
    let ending = if streaming { " ┆ " } else { " ╰ " };

    for (vi, &seg_idx) in visible.iter().enumerate() {
        let normalized = normalize_terminal_segment(segments[seg_idx], 0);

        if vi == 0 {
            // First line: icon at cols 0-1, space at col 2, text at col 3+.
            let (icon, text) = tool_presentation::split_icon_from_label(&normalized);
            let prefix = format!("{icon} ");
            let chunks = wrap_str(text, content_width);
            let last_chunk = chunks.len() - 1;
            for (ci, chunk) in chunks.iter().enumerate() {
                if ci == 0 {
                    out.push(Line::from(vec![
                        Span::styled(prefix.clone(), style),
                        Span::styled(chunk.clone(), style),
                    ]));
                } else {
                    let marker = if ci == last_chunk && vi == last_visible_idx {
                        ending
                    } else {
                        " │ "
                    };
                    out.push(Line::from(vec![
                        Span::styled(marker, style),
                        Span::styled(chunk.clone(), style),
                    ]));
                }
            }
        } else {
            // Subsequent logical lines (multiline labels).
            let chunks = wrap_str(&normalized, content_width);
            let last_chunk = chunks.len() - 1;
            for (ci, chunk) in chunks.iter().enumerate() {
                let marker = if ci == last_chunk && vi == last_visible_idx {
                    ending
                } else {
                    " │ "
                };
                out.push(Line::from(vec![
                    Span::styled(marker, style),
                    Span::styled(chunk.clone(), style),
                ]));
            }
        }
    }
}

/// Like `append_message_colored` with dim=true but renders an icon prefix without
/// italic/dim so the emoji stays visually clean while the placeholder text
/// is still marked as provisional.  Content aligned to column 3.
fn append_message_colored_dim_with_icon(
    out: &mut Vec<Line<'static>>,
    icon: &str,
    text: &str,
    width: usize,
    color: Color,
) {
    let icon_style = Style::default().fg(color);
    let text_style = Style::default()
        .fg(color)
        .add_modifier(Modifier::ITALIC | Modifier::DIM);
    let prefix = format!("{icon} ");
    out.push(Line::from(vec![
        Span::styled(prefix, icon_style),
        Span::styled(text.to_string(), text_style),
    ]));
    let _ = width;
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
            Span::styled(" │ ", marker_style),
            Span::styled("(no output)", no_output_style),
        ]));
        return;
    }

    if width == 0 {
        out.push(Line::from(vec![Span::styled(
            " │ ".to_string(),
            marker_style,
        )]));
        return;
    }

    let content_width = width.saturating_sub(3).max(1);
    let segments: Vec<&str> = content.split('\n').collect();
    for seg_idx in visible_segments(&segments) {
        let segment = segments[seg_idx];
        let normalized = normalize_terminal_segment(segment, 3);
        let chunks = wrap_str(&normalized, content_width);
        for chunk in chunks {
            out.push(Line::from(vec![
                Span::styled(" │ ", marker_style),
                Span::styled(chunk, text_style),
            ]));
        }
    }
}

fn append_markdown_answer(
    out: &mut Vec<Line<'static>>,
    icon: &str,
    md_lines: Vec<Line<'static>>,
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

    let prefix = format!("{icon} ");
    let last_idx = md_lines.len() - 1;

    for (i, mut line) in md_lines.into_iter().enumerate() {
        if i == 0 {
            // First line: prepend emoji icon prefix.
            line.spans.insert(0, Span::raw(prefix.clone()));
        } else {
            // Continuation line: prepend margin marker.
            let marker = if i == last_idx && streaming {
                " ┆ "
            } else if i == last_idx {
                " ╰ "
            } else {
                " │ "
            };
            line.spans.insert(0, Span::raw(marker));
        }

        if streaming && i == last_idx {
            line.spans
                .push(Span::styled("▋", Style::default().fg(Color::Yellow)));
        }

        out.push(line);
    }
}

fn append_message_markdown(
    out: &mut Vec<Line<'static>>,
    content: &str,
    width: usize,
    bg: Color,
    markdown_theme: &crate::theme::MarkdownTheme,
) {
    let md_lines = crate::markdown::render_with_theme(content, width, "", markdown_theme);
    if md_lines.is_empty() {
        return;
    }

    out.push(halfblock_line(width, '▄', bg));

    for line in md_lines {
        let text_width: usize = line.spans.iter().map(|s| s.content.width()).sum();
        let padding = width.saturating_sub(text_width);
        let mut spans: Vec<Span<'static>> = line
            .spans
            .into_iter()
            .map(|s| Span::styled(s.content, s.style.bg(bg)))
            .collect();
        if padding > 0 {
            spans.push(Span::styled(" ".repeat(padding), Style::default().bg(bg)));
        }
        out.push(Line::from(spans));
    }

    out.push(halfblock_line(width, '▀', bg));
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
    use crate::llm::{AssistantPhase, DisplayRange, Message, Role};
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
        // Default user bg = Rgb(50, 50, 64)
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
        // Bar line: fg matches user bg, no bg
        let user_bg = Color::Rgb(50, 50, 64);
        let bar_line = Line::from(vec![Span::styled("▄▄▄", Style::default().fg(user_bg))]);
        // Text line: bg matches user bg, no fg
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
        let (lines, _sources) = build_log_lines(
            &[msg],
            true,
            80,
            &cfg(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        );
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
        let (lines, _sources) = build_log_lines(
            &[call, result],
            false,
            120,
            &cfg(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        );
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
        assert!(text.last().unwrap().contains("20 total lines"));
    }

    #[test]
    fn read_file_result_no_truncation_marker_when_within_limit() {
        let call = Message::tool_call("c1", "read_file", serde_json::json!({"path": "foo.rs"}));
        let content = "line1\nline2\nline3";
        let result = Message::tool_result("c1", content, false);
        let (lines, _sources) = build_log_lines(
            &[call, result],
            false,
            120,
            &cfg(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        );
        let text: Vec<String> = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        assert!(!text.iter().any(|t| t.contains("total lines")));
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
        let (lines, _sources) = build_log_lines(
            &[call, result],
            false,
            120,
            &cfg(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        );
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
        let (lines, _sources) = build_log_lines(
            &[call, result],
            false,
            120,
            &cfg(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        );
        let text: Vec<String> = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        assert!(text.last().unwrap().contains("12 total lines"));
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
        let (lines, _sources) = build_log_lines(
            &[call, result],
            false,
            120,
            &cfg(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        );
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
            text.iter().any(|t| t.contains("old line")),
            "expected old line"
        );
        assert!(
            text.iter().any(|t| t.contains("new line")),
            "expected new line"
        );
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
        let (lines, _sources) = build_log_lines(
            &[call, result],
            false,
            120,
            &cfg(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        );
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
        let marker_count = text.iter().filter(|t| t.contains("total lines")).count();
        assert_eq!(marker_count, 2);
    }

    #[test]
    fn edit_file_pure_addition_no_common_lines_placeholders() {
        // old_text is empty → pure addition; common-line placeholders must not appear.
        let call = Message::tool_call(
            "c1",
            "edit_file",
            serde_json::json!({"path": "foo.rs", "old_text": "prefix\n", "new_text": "prefix\nnew line\n"}),
        );
        let result = Message::tool_result("c1", "ok", false);
        let (lines, _sources) = build_log_lines(
            &[call, result],
            false,
            120,
            &cfg(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        );
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
            !text.iter().any(|t| t.contains("common lines")),
            "pure addition should not show common-lines placeholders; got: {text:?}"
        );
        assert!(
            text.iter().any(|t| t.contains("new line")),
            "should show added line"
        );
    }

    #[test]
    fn edit_file_pure_removal_no_common_lines_placeholders() {
        // new_text is empty → pure removal; common-line placeholders must not appear.
        let call = Message::tool_call(
            "c1",
            "edit_file",
            serde_json::json!({"path": "foo.rs", "old_text": "prefix\nold line\n", "new_text": "prefix\n"}),
        );
        let result = Message::tool_result("c1", "ok", false);
        let (lines, _sources) = build_log_lines(
            &[call, result],
            false,
            120,
            &cfg(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        );
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
            !text.iter().any(|t| t.contains("common lines")),
            "pure removal should not show common-lines placeholders; got: {text:?}"
        );
        assert!(
            text.iter().any(|t| t.contains("old line")),
            "should show removed line"
        );
    }

    #[test]
    fn edit_file_error_shows_plain_content() {
        let call = Message::tool_call(
            "c1",
            "edit_file",
            serde_json::json!({"path": "foo.rs", "old_text": "x", "new_text": "y"}),
        );
        let result = Message::tool_result("c1", "old_text not found", true);
        let (lines, _sources) = build_log_lines(
            &[call, result],
            false,
            120,
            &cfg(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        );
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
        let (lines, _sources) = build_log_lines(
            &[call, result],
            false,
            120,
            &cfg(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        );
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
            body[0].contains("20 total lines"),
            "expected marker first, got: {}",
            body[0]
        );
        assert!(
            body.last().unwrap().contains("20"),
            "expected last line to be 20"
        );
    }

    // ── python result tail truncation ──────────────────────────────────────────

    #[test]
    fn python_result_tail_truncated() {
        let call = Message::tool_call(
            "c1",
            "run_python",
            serde_json::json!({"script": "for i in range(1, 21): print(i)"}),
        );
        let content = (1..=20)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let result = Message::tool_result("c1", &content, false);
        let (lines, _sources) = build_log_lines(
            &[call, result],
            false,
            120,
            &cfg(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        );
        let text: Vec<String> = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        // Body starts after the headline line.
        let body: Vec<&String> = text.iter().skip(1).collect();
        assert!(
            body[0].contains("20 total lines"),
            "expected tail-truncated marker first, got: {}",
            body[0]
        );
        assert!(
            body.last().unwrap().contains("20"),
            "expected last visible line to be 20, got: {}",
            body.last().unwrap()
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
        let (lines, _sources) = build_log_lines(
            &[call, result],
            false,
            120,
            &full_cfg,
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        );
        let text: Vec<String> = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        assert!(!text.iter().any(|t| t.contains("total lines")));
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
        // Question always renders in the log body.
        let (lines, _sources) = build_log_lines(
            &[call],
            false,
            120,
            &cfg(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        );
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
            "question should be visible in the log"
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
        // Committed turn: question should appear in the log.
        let (lines, _sources) = build_log_lines(
            &[call, result],
            false,
            120,
            &cfg(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        );
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
            "question should appear in committed log"
        );
        assert!(
            text.iter().any(|t| t.contains("Option A")),
            "response not rendered"
        );
    }

    #[test]
    fn ask_user_renders_question_from_partial_snapshot_during_streaming() {
        // During streaming, tool_args is empty but tool_partial_snapshot has
        // the question. The question must render in the log.
        let mut call = Message {
            role: Role::ToolCall,
            tool_call_id: Some("c1".to_string()),
            tool_name: Some("ask_user".to_string()),
            tool_args: Some(serde_json::json!({})), // empty — still streaming
            tool_partial_snapshot: Some(serde_json::json!({
                "question": "What do you think?"
            })),
            tool_streaming_field: Some("question".to_string()),
            ..Message::default()
        };
        call.role = Role::ToolCall;
        let (lines, _) = build_log_lines(
            &[call],
            false,
            120,
            &cfg(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        );
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
            text.iter().any(|t| t.contains("What do you think?")),
            "question from partial_snapshot should be visible in the log:\n{}",
            text.join("\n")
        );
    }

    #[test]
    fn ask_user_renders_context_from_partial_snapshot_during_streaming() {
        // During streaming, tool_args is empty but tool_partial_snapshot has
        // the context. The context must render in the log.
        let mut call = Message {
            role: Role::ToolCall,
            tool_call_id: Some("c1".to_string()),
            tool_name: Some("ask_user".to_string()),
            tool_args: Some(serde_json::json!({})), // empty — still streaming
            tool_partial_snapshot: Some(serde_json::json!({
                "question": "Proceed?",
                "context": "Summary: we found the bug."
            })),
            tool_streaming_field: Some("question".to_string()),
            ..Message::default()
        };
        call.role = Role::ToolCall;
        let (lines, _) = build_log_lines(
            &[call],
            false,
            120,
            &cfg(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        );
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
            text.iter()
                .any(|t| t.contains("Summary: we found the bug.")),
            "context from partial_snapshot should be visible in the log:\n{}",
            text.join("\n")
        );
        assert!(
            text.iter().any(|t| t.contains("Proceed?")),
            "question should also be visible"
        );
    }

    // ── Regression: finalized write_file headline shows path, not placeholder ─

    #[test]
    fn write_file_finalized_headline_shows_path() {
        // When a write_file tool call has complete args (ToolCallStart
        // arrived, partial_args cleared), the headline must show the path,
        // not the italic "📄 writing…" placeholder.
        let call = Message::tool_call(
            "c1",
            "write_file",
            serde_json::json!({"path": "/tmp/out.rs", "content": "fn main() {}"}),
        );
        let result = Message::tool_result("c1", "Written 1 lines to /tmp/out.rs", false);
        let (lines, _sources) = build_log_lines(
            &[call, result],
            false,
            120,
            &cfg(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        );
        // The first line is the headline.
        let headline: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            headline.contains("/tmp/out.rs"),
            "finalized write_file headline must show path, got: {headline}"
        );
        assert!(
            !headline.contains("writing…"),
            "finalized headline must not be a placeholder, got: {headline}"
        );
    }

    // ── Wrapped-line truncation (regression: very long logical lines) ────────

    /// Helper: collect all text from rendered lines as a single string for inspection.
    fn lines_text_joined(lines: &[Line]) -> String {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn head_truncation_limits_wrapped_lines_not_logical() {
        // Two logical lines, each wrapping to ~five visual lines at width 20.
        // max_lines = 8 visual lines → only 8 wrapped lines shown, truncation
        // marker present.
        let long_line = "x".repeat(100); // ~5 wrapped lines at width 20
        let content = format!("{}\n{}", long_line, long_line);
        let call = Message::tool_call("c1", "read_file", serde_json::json!({"path": "f"}));
        let result = Message::tool_result("c1", &content, false);
        let (lines, _sources) = build_log_lines(
            &[call, result],
            false,
            20, // narrow terminal → forces wrapping
            &cfg(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        );
        // Lines: 1 headline + up to 8 body lines + 1 truncation marker = max 10
        let body_text = lines_text_joined(&lines);
        assert!(
            body_text.contains("2 total lines"),
            "expected truncation marker, got:\n{body_text}"
        );
        // Count body lines (exclude headline — first line contains the tool icon).
        let body_start = 1; // skip headline
        let body_end = lines.len();
        let body_count = body_end - body_start;
        assert!(
            body_count <= 9, // 8 wrapped lines + 1 marker
            "too many body lines ({body_count}), expected ≤ 9:\n{body_text}"
        );
    }

    #[test]
    fn tail_truncation_limits_wrapped_lines_not_logical() {
        // Two logical lines, each wrapping to ~five visual lines at width 20.
        let long_line = "x".repeat(100);
        let content = format!("{}\n{}", long_line, long_line);
        let call = Message::tool_call("c1", "bash", serde_json::json!({"command": "echo"}));
        let result = Message::tool_result("c1", &content, false);
        let (lines, _sources) = build_log_lines(
            &[call, result],
            false,
            20,
            &cfg(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        );
        let body_text = lines_text_joined(&lines);
        assert!(
            body_text.contains("2 total lines"),
            "expected truncation marker, got:\n{body_text}"
        );
    }

    #[test]
    fn head_truncation_no_marker_when_wrapped_lines_fit() {
        // Short content: all wrapped lines fit within the limit.
        let content = "short line";
        let call = Message::tool_call("c1", "read_file", serde_json::json!({"path": "f"}));
        let result = Message::tool_result("c1", content, false);
        let (lines, _sources) = build_log_lines(
            &[call, result],
            false,
            120,
            &cfg(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        );
        let body_text = lines_text_joined(&lines);
        assert!(
            !body_text.contains("total lines"),
            "unexpected truncation marker for short content:\n{body_text}"
        );
    }

    #[test]
    fn tail_truncation_no_marker_when_wrapped_lines_fit() {
        let content = "short output";
        let call = Message::tool_call("c1", "bash", serde_json::json!({"command": "echo"}));
        let result = Message::tool_result("c1", content, false);
        let (lines, _sources) = build_log_lines(
            &[call, result],
            false,
            120,
            &cfg(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        );
        let body_text = lines_text_joined(&lines);
        assert!(
            !body_text.contains("total lines"),
            "unexpected truncation marker:\n{body_text}"
        );
    }

    #[test]
    fn head_truncation_single_long_logical_line_is_bounded() {
        // One very long logical line → should be capped at max_lines wrapped chunks.
        let long_line = "x".repeat(500); // ~seven wrapped lines at width 80
        let call = Message::tool_call("c1", "read_file", serde_json::json!({"path": "f"}));
        let result = Message::tool_result("c1", long_line, false);
        let (lines, _sources) = build_log_lines(
            &[call, result],
            false,
            80,
            &cfg(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        );
        let body_text = lines_text_joined(&lines);
        // Should have truncation marker since head_lines=8 but the line wraps
        // to ~7 chunks (< 8 at width 80), so actually no truncation for 500
        // chars at width 80.  Let's verify: at width=80, content_width=77,
        // 500/77 ≈ 7 chunks → fits within 8.
        // Use narrower width to force truncation.
        assert!(
            !body_text.contains("total lines"),
            "500 chars at width 80 should fit in 8 wrapped lines:\n{body_text}"
        );
    }

    #[test]
    fn head_truncation_single_very_long_line_is_truncated() {
        // One very long logical line at narrow width → many wrapped chunks → must truncate.
        let long_line = "x".repeat(500);
        let call = Message::tool_call("c1", "read_file", serde_json::json!({"path": "f"}));
        let result = Message::tool_result("c1", long_line, false);
        let (lines, _sources) = build_log_lines(
            &[call, result],
            false,
            20, // narrow → ~26 wrapped chunks (500/17 ≈ 30)
            &cfg(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        );
        let body_text = lines_text_joined(&lines);
        assert!(
            body_text.contains("1 total lines"),
            "expected truncation marker for single long line at narrow width:\n{body_text}"
        );
    }

    #[test]
    fn tail_truncation_single_long_line_is_bounded() {
        let long_line = "x".repeat(500);
        let call = Message::tool_call("c1", "bash", serde_json::json!({"command": "echo"}));
        let result = Message::tool_result("c1", long_line, false);
        let (lines, _sources) = build_log_lines(
            &[call, result],
            false,
            20,
            &cfg(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        );
        let body_text = lines_text_joined(&lines);
        assert!(
            body_text.contains("1 total lines"),
            "expected truncation marker for long line in tail mode:\n{body_text}"
        );
    }

    #[test]
    fn diff_body_removed_block_limited_by_wrapped_lines() {
        // old_text has one very long line that wraps many times at width 20.
        // diff_lines default = 4; should limit to 4 wrapped visual lines.
        // There's only 1 logical line, so no logical truncation marker is
        // expected — but the visual display is still bounded.
        let old_long = "r".repeat(200);
        let new = "a";
        let call = Message::tool_call(
            "c1",
            "edit_file",
            serde_json::json!({"path": "f", "old_text": old_long, "new_text": new}),
        );
        let result = Message::tool_result("c1", "ok", false);
        let (lines, _sources) = build_log_lines(
            &[call, result],
            false,
            20,
            &cfg(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        );
        let body_text = lines_text_joined(&lines);
        // Count how many body lines contain the removed marker pattern " │ r".
        // Should be exactly 4 (diff_lines limit), not ~12 (the full wrapped count).
        let removed_line_count = body_text.lines().filter(|l| l.contains("│ rrr")).count();
        assert_eq!(
            removed_line_count, 4,
            "expected exactly 4 wrapped lines of 'r's (diff_lines limit), got {removed_line_count}:\n{body_text}"
        );
        // The new_text 'a' should also appear.
        assert!(
            body_text.contains("│ a"),
            "expected new_text 'a' in diff output:\n{body_text}"
        );
    }

    #[test]
    fn full_output_disables_wrapped_truncation() {
        let long_line = "x".repeat(500);
        let call = Message::tool_call("c1", "read_file", serde_json::json!({"path": "f"}));
        let result = Message::tool_result("c1", long_line, false);
        let cfg_full = ToolBodyConfig {
            full_output: true,
            ..ToolBodyConfig::default()
        };
        let (lines, _sources) = build_log_lines(
            &[call, result],
            false,
            20,
            &cfg_full,
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        );
        let body_text = lines_text_joined(&lines);
        assert!(
            !body_text.contains("total lines"),
            "full_output must not truncate:\n{body_text}"
        );
    }
}
