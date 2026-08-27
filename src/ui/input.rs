use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph},
};
use ratatui_textarea::TextArea;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{App, InputMode};
use crate::provider_manager::{ProviderSetupStep, SetupInputKind};

const TAB_WIDTH: usize = 4;

#[derive(Debug, Clone)]
pub(super) struct WrappedInput {
    pub(super) lines: Vec<String>,
    pub(super) cursor: (usize, usize),
    pub(super) selection: Option<((usize, usize), (usize, usize))>,
}

pub(super) fn style_textarea(app: &mut App) {
    let bg = if app.input_mode == InputMode::Shell {
        app.theme.input.shell.bg.unwrap_or(Color::Rgb(24, 34, 32))
    } else if app.ask_user_freeform_mode() {
        app.theme
            .input
            .ask_user
            .bg
            .unwrap_or(Color::Rgb(50, 30, 15))
    } else {
        app.theme.input.normal.bg.unwrap_or(Color::Rgb(30, 30, 40))
    };

    let active: &mut TextArea<'static> = if app.input_mode == InputMode::Shell {
        &mut app.shell.textarea
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
    selection: Option<((usize, usize), (usize, usize))>,
    width: usize,
) -> WrappedInput {
    if width == 0 {
        return WrappedInput {
            lines: lines.to_vec(),
            cursor,
            selection,
        };
    }

    let mut wrapped_lines: Vec<String> = Vec::new();
    let mut wrapped_cursor = (0usize, 0usize);
    let mut wrapped_selection = selection.map(|_| ((0usize, 0usize), (0usize, 0usize)));

    for (row_idx, line) in lines.iter().enumerate() {
        let normalized = normalize_terminal_segment(line, 0);
        let chunks = wrap_input_line(&normalized, width);
        let wrapped_row_start = wrapped_lines.len();

        let wrap_position = |column: usize| {
            let source_prefix: String = line.chars().take(column).collect();
            let visual_column = normalize_terminal_segment(&source_prefix, 0).width();
            let mut consumed = 0usize;

            for (idx, chunk) in chunks.iter().enumerate() {
                let chunk_width = chunk.width();
                if visual_column <= consumed + chunk_width || idx == chunks.len() - 1 {
                    return (
                        wrapped_row_start + idx,
                        visual_column.saturating_sub(consumed).min(chunk_width),
                    );
                }
                consumed += chunk_width;
            }

            (wrapped_row_start, 0)
        };

        if row_idx == cursor.0 {
            wrapped_cursor = wrap_position(cursor.1);
        }
        if let (Some((start, end)), Some((wrapped_start, wrapped_end))) =
            (selection, wrapped_selection.as_mut())
        {
            if row_idx == start.0 {
                *wrapped_start = wrap_position(start.1);
            }
            if row_idx == end.0 {
                *wrapped_end = wrap_position(end.1);
            }
        }

        wrapped_lines.extend(chunks);
    }

    if wrapped_lines.is_empty() {
        wrapped_lines.push(String::new());
    }

    WrappedInput {
        lines: wrapped_lines,
        cursor: wrapped_cursor,
        selection: wrapped_selection,
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

pub(super) fn render_input_panel(
    f: &mut ratatui::Frame,
    area: Rect,
    app: &mut App,
    panel_bg: Color,
) {
    let is_shell = app.input_mode == InputMode::Shell;
    let input_width = area.width as usize;
    let (mut selection, selection_style) = if is_shell {
        (
            app.shell.textarea.selection_range(),
            app.shell.textarea.selection_style(),
        )
    } else {
        (
            app.textarea.selection_range(),
            app.textarea.selection_style(),
        )
    };

    let (mut input_lines, mut cursor, mut prefix, mut hint) = if is_shell {
        let cwd = if app.session.current_cwd.is_empty() {
            ".".to_string()
        } else {
            app.session.current_cwd.clone()
        };
        let prefix = if app.shell.available.len() > 1 {
            format!(
                "[{}] {}{} ",
                app.shell.selected.label(),
                cwd,
                app.shell.selected.prompt_char()
            )
        } else {
            format!("{}{} ", cwd, app.shell.selected.prompt_char())
        };
        (
            app.shell.textarea.lines().to_vec(),
            app.shell.textarea.cursor(),
            prefix,
            (app.shell.available.len() > 1).then_some("Ctrl+S switch".to_string()),
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
            None,
        )
    } else {
        (
            app.textarea.lines().to_vec(),
            app.textarea.cursor(),
            String::new(),
            None,
        )
    };

    // ── Selection filter mirror ───────────────────────────────────────────
    // When the selection menu is active and supports filtering, show the
    // filter query in the input field instead of the (typically empty) textarea.
    if app.selection.active && app.selection_filter_enabled() && !app.ask_user_freeform_mode() {
        let filter = app.selection.query.clone();
        cursor = (0, filter.len()).into();
        input_lines = if filter.is_empty() {
            vec![String::new()]
        } else {
            vec![filter]
        };
        prefix = String::new();
        hint = None;
        selection = None;
    }

    let wrap_width = if prefix.is_empty() {
        input_width
    } else {
        input_width.saturating_sub(prefix.width()).max(1)
    };
    let cursor = (cursor.0, cursor.1);
    let wrapped = wrap_input_for_render(&input_lines, cursor, selection, wrap_width);
    let wrapped_lines = wrapped.lines;
    let wrapped_cursor = wrapped.cursor;
    let wrapped_selection = wrapped.selection;

    // ── Viewport scrolling ────────────────────────────────────────────────
    let viewport_h = area.height as usize;
    let total = wrapped_lines.len().max(1);

    // Keep scroll in bounds and follow the cursor.
    app.input_scroll = app.input_scroll.min(total.saturating_sub(viewport_h));
    if wrapped_cursor.0 < app.input_scroll {
        app.input_scroll = wrapped_cursor.0;
    } else if wrapped_cursor.0 >= app.input_scroll + viewport_h {
        app.input_scroll = wrapped_cursor
            .0
            .saturating_sub(viewport_h.saturating_sub(1));
    }

    let scroll = app.input_scroll;
    let visible_end = (scroll + viewport_h).min(total);
    let visible_lines: Vec<String> = wrapped_lines[scroll..visible_end].to_vec();

    let input_style = Style::default().fg(Color::White).bg(panel_bg);
    let selected_style = input_style.patch(selection_style);
    let mut lines: Vec<Line<'static>> = visible_lines
        .into_iter()
        .enumerate()
        .map(|(i, row)| {
            let abs_idx = scroll + i;
            let mut spans = Vec::new();
            if abs_idx == 0 && !prefix.is_empty() {
                spans.push(Span::styled(
                    prefix.clone(),
                    Style::default()
                        .fg(app
                            .theme
                            .input
                            .normal
                            .field
                            .prefix
                            .fg
                            .unwrap_or(ratatui::style::Color::Cyan))
                        .bg(panel_bg),
                ));
            }

            let selected_columns = wrapped_selection.and_then(|(start, end)| {
                if abs_idx < start.0 || abs_idx > end.0 || start == end {
                    return None;
                }
                let row_width = row.width();
                let start_col = if abs_idx == start.0 { start.1 } else { 0 };
                let end_col = if abs_idx == end.0 { end.1 } else { row_width };
                (start_col < end_col).then_some((start_col, end_col))
            });

            if let Some((start_col, end_col)) = selected_columns {
                let mut column = 0usize;
                let mut before = String::new();
                let mut selected = String::new();
                let mut after = String::new();
                for ch in row.chars() {
                    let next_column = column + ch.width().unwrap_or(0);
                    if column < start_col {
                        before.push(ch);
                    } else if column < end_col {
                        selected.push(ch);
                    } else {
                        after.push(ch);
                    }
                    column = next_column;
                }
                if !before.is_empty() {
                    spans.push(Span::styled(before, input_style));
                }
                if !selected.is_empty() {
                    spans.push(Span::styled(selected, selected_style));
                }
                if !after.is_empty() {
                    spans.push(Span::styled(after, input_style));
                }
            } else {
                spans.push(Span::styled(row, input_style));
            }

            Line::from(spans)
        })
        .collect();

    // When the visible slice is empty or the first visible line is scrolled
    // past the prefix line, still show the prefix on a lone empty line.
    if lines.is_empty() && !prefix.is_empty() {
        lines.push(Line::from(Span::styled(
            prefix.clone(),
            Style::default()
                .fg(app
                    .theme
                    .input
                    .normal
                    .field
                    .prefix
                    .fg
                    .unwrap_or(Color::Cyan))
                .bg(panel_bg),
        )));
    } else if lines.is_empty() {
        lines.push(Line::default());
    }

    if let Some(hint) = hint {
        let hint_style = Style::default()
            .fg(app
                .theme
                .input
                .normal
                .placeholder
                .fg
                .unwrap_or(Color::Rgb(120, 140, 140)))
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
    let cursor_y = area
        .y
        .saturating_add((wrapped_cursor.0.saturating_sub(scroll)) as u16);
    f.set_cursor_position((cursor_x, cursor_y));
}
