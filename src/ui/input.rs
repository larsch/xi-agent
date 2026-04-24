use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph},
};
use ratatui_textarea::TextArea;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{App, InputMode, ProviderSetupStep, SetupInputKind};

/// Background colour of the input panel.
pub(super) const INPUT_BG: Color = Color::Rgb(30, 30, 40);
/// Background colour of shell input panel.
pub(super) const SHELL_INPUT_BG: Color = Color::Rgb(24, 34, 32);
/// Background colour of the input panel when typing a free-form ask_user response.
pub(super) const ASK_USER_INPUT_BG: Color = Color::Rgb(50, 30, 15);
const TAB_WIDTH: usize = 4;

#[derive(Debug, Clone)]
pub(super) struct WrappedInput {
    pub(super) lines: Vec<String>,
    pub(super) cursor: (usize, usize),
}

pub(super) fn style_textarea(app: &mut App) {
    let bg = if app.input_mode == InputMode::Shell {
        SHELL_INPUT_BG
    } else if app.ask_user_freeform_mode() {
        ASK_USER_INPUT_BG
    } else {
        INPUT_BG
    };

    let active: &mut TextArea<'static> = if app.input_mode == InputMode::Shell {
        &mut app.shell_textarea
    } else {
        &mut app.textarea
    };

    active.set_block(
        Block::default()
            .borders(Borders::NONE)
            .style(Style::default().bg(bg)),
    );
    active.set_style(Style::default().fg(Color::White).bg(bg));
    active.set_cursor_line_style(Style::default().bg(bg));
}

pub(super) fn normalize_terminal_segment(text: &str, start_col: usize) -> String {
    let mut normalized = String::with_capacity(text.len());
    let mut col = start_col;

    for ch in text.chars() {
        match ch {
            '\t' => {
                let spaces = TAB_WIDTH - (col % TAB_WIDTH);
                normalized.push_str(&" ".repeat(spaces));
                col += spaces;
            }
            c if c.is_control() => {
                normalized.push(' ');
                col += 1;
            }
            c => {
                normalized.push(c);
                col += c.width().unwrap_or(0);
            }
        }
    }

    normalized
}

pub(super) fn wrap_str(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    if text.is_empty() {
        return vec![String::new()];
    }
    textwrap::wrap(text, width)
        .into_iter()
        .map(|cow| cow.into_owned())
        .collect()
}

pub(super) fn wrap_input_line(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    if text.is_empty() {
        return vec![String::new()];
    }

    let mut out: Vec<String> = Vec::new();
    let mut line = String::new();
    let mut line_w = 0usize;
    let mut run = String::new();
    let mut run_is_ws: Option<bool> = None;

    let flush_line = |out: &mut Vec<String>, line: &mut String, line_w: &mut usize| {
        out.push(std::mem::take(line));
        *line_w = 0;
    };

    let append_piece = |line: &mut String, line_w: &mut usize, piece: &str| {
        line.push_str(piece);
        *line_w += piece.width();
    };

    let handle_run = |run_text: &str,
                      is_ws: bool,
                      out: &mut Vec<String>,
                      line: &mut String,
                      line_w: &mut usize| {
        if run_text.is_empty() {
            return;
        }

        if is_ws {
            for ch in run_text.chars() {
                let ch_w = ch.width().unwrap_or(0);
                if *line_w + ch_w > width && !line.is_empty() {
                    flush_line(out, line, line_w);
                }
                line.push(ch);
                *line_w += ch_w;
                if *line_w >= width {
                    flush_line(out, line, line_w);
                }
            }
            return;
        }

        let token_w = run_text.width();
        let long_token = token_w.saturating_mul(2) > width;

        if long_token {
            for ch in run_text.chars() {
                let ch_w = ch.width().unwrap_or(0);
                if *line_w + ch_w > width && !line.is_empty() {
                    flush_line(out, line, line_w);
                }
                line.push(ch);
                *line_w += ch_w;
                if *line_w >= width {
                    flush_line(out, line, line_w);
                }
            }
        } else if *line_w + token_w > width && !line.is_empty() {
            flush_line(out, line, line_w);
            append_piece(line, line_w, run_text);
        } else {
            append_piece(line, line_w, run_text);
        }
    };

    for ch in text.chars() {
        let is_ws = ch.is_whitespace();
        match run_is_ws {
            None => {
                run_is_ws = Some(is_ws);
                run.push(ch);
            }
            Some(kind) if kind == is_ws => run.push(ch),
            Some(kind) => {
                handle_run(&run, kind, &mut out, &mut line, &mut line_w);
                run.clear();
                run.push(ch);
                run_is_ws = Some(is_ws);
            }
        }
    }

    if let Some(kind) = run_is_ws {
        handle_run(&run, kind, &mut out, &mut line, &mut line_w);
    }

    if out.is_empty() || !line.is_empty() {
        out.push(line);
    }

    out
}

pub(super) fn wrap_input_for_render(
    lines: &[String],
    cursor: (usize, usize),
    width: usize,
) -> WrappedInput {
    if width == 0 {
        return WrappedInput {
            lines: lines.to_vec(),
            cursor,
        };
    }

    let mut wrapped_lines: Vec<String> = Vec::new();
    let mut wrapped_cursor = (0usize, 0usize);

    for (row_idx, line) in lines.iter().enumerate() {
        let normalized = normalize_terminal_segment(line, 0);
        let chunks = wrap_input_line(&normalized, width);

        if row_idx == cursor.0 {
            let mut before = String::new();
            for ch in normalized.chars().take(cursor.1) {
                before.push(ch);
            }
            let before_w = before.width();

            let mut consumed = 0usize;
            let mut row_off = 0usize;
            let mut col_off = 0usize;

            for (idx, chunk) in chunks.iter().enumerate() {
                let chunk_w = chunk.width();
                if before_w <= consumed + chunk_w {
                    row_off = idx;
                    col_off = before_w.saturating_sub(consumed);
                    break;
                }
                consumed += chunk_w;
                if idx == chunks.len() - 1 {
                    row_off = idx;
                    col_off = chunk_w;
                }
            }

            wrapped_cursor = (wrapped_lines.len() + row_off, col_off);
        }

        wrapped_lines.extend(chunks);
    }

    if wrapped_lines.is_empty() {
        wrapped_lines.push(String::new());
    }

    WrappedInput {
        lines: wrapped_lines,
        cursor: wrapped_cursor,
    }
}

pub(super) fn split_scrollbar_column(area: Rect) -> (Rect, Option<Rect>) {
    if area.width > 1 {
        let parts = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(0), Constraint::Length(1)])
            .split(area);
        (parts[0], Some(parts[1]))
    } else {
        (area, None)
    }
}

pub(super) fn render_input_panel(f: &mut ratatui::Frame, area: Rect, app: &App, panel_bg: Color) {
    let is_shell = app.input_mode == InputMode::Shell;
    let input_width = area.width as usize;

    let (input_lines, cursor, prefix, hint) = if is_shell {
        let cwd = if app.current_cwd().is_empty() {
            ".".to_string()
        } else {
            app.current_cwd().to_string()
        };
        let prefix = if app.available_shells.len() > 1 {
            format!(
                "[{}] {}{} ",
                app.selected_shell.label(),
                cwd,
                app.selected_shell.prompt_char()
            )
        } else {
            format!("{}{} ", cwd, app.selected_shell.prompt_char())
        };
        (
            app.shell_textarea.lines().to_vec(),
            app.shell_textarea.cursor(),
            prefix,
            (app.available_shells.len() > 1).then_some("Ctrl+S switch".to_string()),
        )
    } else if app.provider.setup_step != ProviderSetupStep::Idle {
        let instance = app.pending_provider_instance();
        let kind = match &app.provider.setup_step {
            ProviderSetupStep::Endpoint => SetupInputKind::BaseUrl,
            ProviderSetupStep::ApiKey { .. } => SetupInputKind::ApiKey,
            ProviderSetupStep::Name => SetupInputKind::Name,
            ProviderSetupStep::Idle => unreachable!(),
        };
        (
            app.textarea.lines().to_vec(),
            app.textarea.cursor(),
            kind.prompt_label(instance.as_ref()),
            Some(kind.prompt_hint(instance.as_ref())),
        )
    } else {
        (
            app.textarea.lines().to_vec(),
            app.textarea.cursor(),
            String::new(),
            app.ask_user_question().map(str::to_owned),
        )
    };

    let wrap_width = if prefix.is_empty() {
        input_width
    } else {
        input_width.saturating_sub(prefix.width()).max(1)
    };
    let wrapped = wrap_input_for_render(&input_lines, cursor, wrap_width);
    let wrapped_lines = wrapped.lines;
    let wrapped_cursor = wrapped.cursor;

    let mut lines: Vec<Line<'static>> = wrapped_lines
        .into_iter()
        .enumerate()
        .map(|(idx, row)| {
            if idx == 0 && !prefix.is_empty() {
                Line::from(vec![
                    Span::styled(
                        prefix.clone(),
                        Style::default().fg(Color::Cyan).bg(panel_bg),
                    ),
                    Span::styled(row, Style::default().fg(Color::White).bg(panel_bg)),
                ])
            } else {
                Line::from(Span::styled(
                    row,
                    Style::default().fg(Color::White).bg(panel_bg),
                ))
            }
        })
        .collect();

    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            prefix.clone(),
            Style::default().fg(Color::Cyan).bg(panel_bg),
        )));
    }

    if let Some(hint) = hint {
        let hint_style = Style::default()
            .fg(Color::Rgb(120, 140, 140))
            .bg(panel_bg)
            .add_modifier(ratatui::style::Modifier::DIM);
        if let Some(first) = lines.first_mut() {
            first
                .spans
                .push(Span::styled(format!("  {hint}"), hint_style));
        }
    }

    let paragraph = Paragraph::new(Text::from(lines))
        .block(
            Block::default()
                .borders(Borders::NONE)
                .style(Style::default().bg(panel_bg)),
        )
        .style(Style::default().fg(Color::White).bg(panel_bg));

    f.render_widget(paragraph, area);

    let cursor_x = area
        .x
        .saturating_add((wrapped_cursor.1 + prefix.width()) as u16);
    let cursor_y = area.y.saturating_add(wrapped_cursor.0 as u16);
    f.set_cursor_position((cursor_x, cursor_y));
}
