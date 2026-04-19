use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
};
use unicode_width::UnicodeWidthStr;

use crate::{
    llm::{AssistantPhase, Message, Role},
    tool_presentation,
};

use super::input::{normalize_terminal_segment, wrap_str};

pub(super) const USER_BG: Color = Color::Rgb(50, 50, 60);
const ASK_USER_INPUT_BG: Color = Color::Rgb(50, 30, 15);

pub(super) fn build_log_lines(
    messages: &[Message],
    streaming: bool,
    width: usize,
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
                    append_message_dim(
                        &mut lines,
                        &format!("🧠 {}", sanitize_for_display(thinking)),
                        "",
                        width,
                    );
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
                    let mut md_lines = crate::markdown::render(&content, width, &prefix);
                    if answer_icon == "💭" {
                        dim_lines(&mut md_lines);
                    }
                    append_markdown_answer(&mut lines, md_lines, is_streaming_last);
                }
            }
            Role::ToolCall => {
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

                    if let Some(ctx) = context {
                        let md_lines = crate::markdown::render(ctx, width, "❓ ");
                        append_markdown_answer(&mut lines, md_lines, false);
                        lines.push(Line::default());
                    }

                    if !question.is_empty() {
                        let q_prefix = if context.is_none() { "❓ " } else { "" };
                        let md_lines = crate::markdown::render(question, width, q_prefix);
                        append_markdown_answer(&mut lines, md_lines, false);
                    }
                } else {
                    let mut label = if name == "local_shell" {
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
                    } else {
                        match msg.tool_args.as_ref() {
                            Some(args) => tool_presentation::tool_invocation_label(name, args),
                            None => tool_presentation::tool_invocation_label(
                                name,
                                &serde_json::Value::Null,
                            ),
                        }
                    };

                    if matches!(name, "read" | "read_file")
                        && let Some(next) = messages.get(idx + 1)
                        && next.role == Role::ToolResult
                        && let Some(ref dr) = next.display_range
                    {
                        label.push_str(&format!(
                            " [{}-{}/{}]",
                            dr.first_line, dr.last_line, dr.total_lines
                        ));
                    }

                    let color = if name == "local_shell" {
                        Color::LightBlue
                    } else {
                        Color::Cyan
                    };
                    append_message_colored(&mut lines, &label, width, color);
                }
            }
            Role::ToolResult => {
                let prev_is_local_shell = matches!(
                    messages.get(idx.saturating_sub(1)),
                    Some(prev)
                        if prev.role == Role::ToolCall
                            && matches!(prev.tool_name.as_deref(), Some("local_shell"))
                );
                let prev_is_ask_user = matches!(
                    messages.get(idx.saturating_sub(1)),
                    Some(prev)
                        if prev.role == Role::ToolCall
                            && matches!(prev.tool_name.as_deref(), Some("ask_user"))
                );

                let content_for_display = msg.content.clone();
                const DISPLAY_CHARS: usize = 200;
                const SANITIZE_LIMIT: usize = DISPLAY_CHARS * 5;
                let original_overflows = content_for_display.chars().nth(DISPLAY_CHARS).is_some();
                let sanitize_input = if original_overflows {
                    let byte_end = content_for_display
                        .char_indices()
                        .nth(SANITIZE_LIMIT)
                        .map(|(b, _)| b)
                        .unwrap_or(content_for_display.len());
                    &content_for_display[..byte_end]
                } else {
                    &content_for_display
                };
                let content_for_display = sanitize_for_display(sanitize_input);

                let preview: String = content_for_display.chars().take(DISPLAY_CHARS).collect();
                let truncated = original_overflows || content_for_display.len() > DISPLAY_CHARS;
                let display = if truncated {
                    format!("{preview}…")
                } else {
                    preview
                };
                let color = if prev_is_local_shell {
                    if msg.is_error {
                        Color::LightRed
                    } else {
                        Color::LightBlue
                    }
                } else if msg.is_error {
                    Color::Red
                } else {
                    Color::Green
                };
                if prev_is_ask_user {
                    append_ask_user_response_block(&mut lines, &display, width, ASK_USER_INPUT_BG);
                } else {
                    append_tool_result_block(&mut lines, &display, width, color);
                }
            }
        }
    }

    lines
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
            .add_modifier(ratatui::style::Modifier::ITALIC);
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

fn append_ask_user_response_block(
    out: &mut Vec<Line<'static>>,
    content: &str,
    width: usize,
    bg: Color,
) {
    let bg_style = Style::default().bg(bg);
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

fn append_message_dim(
    out: &mut Vec<Line<'static>>,
    content: &str,
    suffix: &'static str,
    width: usize,
) {
    let dim_style = Style::default()
        .fg(Color::DarkGray)
        .add_modifier(ratatui::style::Modifier::DIM);
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

fn dim_lines(lines: &mut [Line<'static>]) {
    let dim_style = Style::default()
        .fg(Color::DarkGray)
        .add_modifier(ratatui::style::Modifier::DIM);
    for line in lines.iter_mut() {
        for span in &mut line.spans {
            span.style = span.style.patch(dim_style);
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

#[cfg(test)]
mod tests {
    use super::{build_log_lines, trim_assistant_block_edges};
    use crate::llm::{AssistantPhase, Message};

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

        let lines = build_log_lines(&[msg], true, 80);
        assert!(lines.is_empty());
    }
}
