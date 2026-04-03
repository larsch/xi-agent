use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::{
    app::{App, InputMode, MAX_SELECTION_VISIBLE},
    auth::AuthFlow,
    commands::CompletionItem,
    llm::{AssistantPhase, Role},
    provider::{ProviderKind, ThinkingSupport, context_window_for_model, thinking_support_for},
    tool_presentation,
};

/// Background colour of the input panel.
const INPUT_BG: Color = Color::Rgb(30, 30, 40);

/// Background colour of shell input panel.
const SHELL_INPUT_BG: Color = Color::Rgb(24, 34, 32);

/// Background colour of user message blocks in the chat log.
const USER_BG: Color = Color::Rgb(50, 50, 60);

/// Background colour of the completion popup (unselected rows).
const COMPLETION_BG: Color = Color::Rgb(22, 22, 38);

/// Background colour of the selected completion row.
const COMPLETION_SEL_BG: Color = Color::Rgb(55, 55, 100);

/// Foreground colour for the command usage column in the popup.
const COMPLETION_CMD_FG: Color = Color::Rgb(120, 200, 255);

/// Foreground colour for the description column in the popup.
const COMPLETION_DESC_FG: Color = Color::Rgb(140, 140, 160);

/// Foreground colour for the highlighted (matched) portion of a completion label.
const COMPLETION_MATCH_FG: Color = Color::Rgb(255, 220, 80);

/// Background colour of the selection menu header.
const SELECTION_HEADER_BG: Color = Color::Rgb(20, 45, 20);

/// Background colour of the selection menu items (unselected).
const SELECTION_BG: Color = Color::Rgb(18, 35, 18);

/// Background colour of the selected item in the selection menu.
const SELECTION_SEL_BG: Color = Color::Rgb(30, 90, 30);

/// Foreground colour for model names in the selection menu.
const SELECTION_ITEM_FG: Color = Color::Rgb(140, 220, 140);

/// Background colour of the login panel header.
const LOGIN_HEADER_BG: Color = Color::Rgb(20, 30, 60);

/// Background colour of the login panel content rows.
const LOGIN_CONTENT_BG: Color = Color::Rgb(15, 22, 48);

/// all rendering concerns live here.
fn style_textarea(app: &mut App) {
    // The Block's style fills every cell the widget owns (including empty
    // lines below the cursor); set_style() only covers the text spans.
    // Both must carry the mode background so the panel is uniform.
    let bg = if app.input_mode == InputMode::Shell {
        SHELL_INPUT_BG
    } else {
        INPUT_BG
    };

    let active = if app.input_mode == InputMode::Shell {
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

const TAB_WIDTH: usize = 4;

/// Throbber animation frames (braille spinner).
const THROBBER_FRAMES: &[char] = &['⣾', '⣽', '⣻', '⢿', '⡿', '⣟', '⣯', '⣷'];

/// Render a full-width row of halfblock characters in `color` so that a
/// coloured panel appears to have a smooth sub-character edge against the
/// default terminal background.
///
/// - Top edge: `▄` (lower-half block) — upper half = bg, lower half = color
/// - Bottom edge: `▀` (upper-half block) — upper half = color, lower half = bg
fn halfblock_line(width: usize, ch: char, color: Color) -> Line<'static> {
    Line::from(Span::styled(
        ch.to_string().repeat(width),
        Style::default().fg(color),
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PanelHeights {
    completion_height: u16,
    selection_header_height: u16,
    selection_items_height: u16,
    login_header_height: u16,
    login_content_height: u16,
    halfblock_height: u16,
    status_height: u16,
    input_height: u16,
    info_height: u16,
}

#[derive(Debug, Clone, Copy)]
struct PanelInputs<'a> {
    terminal_height: usize,
    width: usize,
    input_line_count: usize,
    show_info: bool,
    login_active: bool,
    selection_mode: bool,
    selection_items_len: usize,
    completions_len: usize,
    resume_hint_visible: bool,
    ask_user_freeform_mode: bool,
    ask_user_question: Option<&'a str>,
    login_url: Option<&'a str>,
    has_login_code: bool,
    streaming: bool,
    has_provider_status: bool,
}

fn input_visual_line_count(lines: &[String], width: usize) -> usize {
    if width == 0 {
        return lines.len().max(1);
    }

    let mut count = 0usize;
    for line in lines {
        let normalized = normalize_terminal_segment(line, 0);
        count += wrap_input_line(&normalized, width).len();
    }

    count.max(1)
}

fn compute_panel_heights(input: PanelInputs<'_>) -> PanelHeights {
    let capped_input = input
        .input_line_count
        .max(1)
        .min((input.terminal_height * 40 / 100).max(1)) as u16;

    let info_height: u16 = if input.show_info { 1 } else { 0 };

    let completion_height = if input.login_active || input.selection_mode {
        0
    } else if input.ask_user_freeform_mode {
        let question = input.ask_user_question.unwrap_or("Answer");
        let prompt = format!("{question}   Enter submit   Esc cancel");
        wrap_str(&prompt, input.width.max(1)).len() as u16
    } else if input.completions_len > 0 {
        input.completions_len as u16
    } else if input.resume_hint_visible {
        1
    } else {
        0
    };

    let selection_header_height: u16 = if input.selection_mode { 1 } else { 0 };
    let selection_items_height: u16 = if input.selection_mode {
        input.selection_items_len.clamp(1, MAX_SELECTION_VISIBLE) as u16
    } else {
        0
    };

    let login_header_height: u16 = if input.login_active { 1 } else { 0 };
    let login_content_height: u16 = if input.login_active {
        let mut h = 2usize;
        if let Some(url) = input.login_url {
            let url_indent = LOGIN_URL_INDENT.len();
            let wrap_width = input.width.saturating_sub(url_indent).max(1);
            let url_lines = wrap_str(url, wrap_width).len();
            h += 1 + url_lines;
        }
        if input.has_login_code {
            h += 1;
        }
        h as u16
    } else {
        0
    };

    let input_height = if input.login_active { 0 } else { capped_input };
    let halfblock_height: u16 = if input.login_active { 0 } else { 1 };
    let status_height: u16 =
        if !input.login_active && (input.streaming || input.has_provider_status) {
            1
        } else {
            0
        };

    PanelHeights {
        completion_height,
        selection_header_height,
        selection_items_height,
        login_header_height,
        login_content_height,
        halfblock_height,
        status_height,
        input_height,
        info_height,
    }
}

fn build_log_lines_cached(app: &mut App, width: usize) -> &Vec<Line<'static>> {
    if !matches!(&app.cached_log_lines, Some((rev, w, _)) if *rev == app.log_revision && *w == width)
    {
        let lines = build_log_lines(&app.messages, app.streaming, &app.queued_steering, width);
        app.cached_log_lines = Some((app.log_revision, width, lines));
    }
    &app.cached_log_lines.as_ref().unwrap().2
}

pub fn draw(f: &mut ratatui::Frame, app: &mut App) {
    style_textarea(app);

    let terminal_height = f.area().height as usize;
    let width = f.area().width as usize;
    let resume_hint_visible = app.should_show_resume_hint();

    let active_lines = if app.input_mode == InputMode::Shell {
        app.shell_textarea.lines()
    } else {
        app.textarea.lines()
    };

    let input_line_count = input_visual_line_count(active_lines, width);

    let layout = compute_panel_heights(PanelInputs {
        terminal_height,
        width,
        input_line_count,
        show_info: app.show_info,
        login_active: app.login_active,
        selection_mode: app.selection_mode,
        selection_items_len: app.selection_items.len(),
        completions_len: app.completions.len(),
        resume_hint_visible,
        ask_user_freeform_mode: app.ask_user_freeform_mode,
        ask_user_question: app.ask_user_question.as_deref(),
        login_url: app.login_url.as_deref(),
        has_login_code: app.login_code.is_some(),
        streaming: app.throbber_visible(),
        has_provider_status: app.provider_status.is_some(),
    });

    // Layout: chat log | completions | sel header | sel items
    //       | login header | login content
    //       | status | top halfblock | input | bottom halfblock | info bar
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),                                 // 0: chat log
            Constraint::Length(layout.completion_height),       // 1: completion popup
            Constraint::Length(layout.selection_header_height), // 2: selection header
            Constraint::Length(layout.selection_items_height),  // 3: selection items
            Constraint::Length(layout.login_header_height),     // 4: login header
            Constraint::Length(layout.login_content_height),    // 5: login content
            Constraint::Length(layout.status_height),           // 6: throbber / status bar
            Constraint::Length(layout.halfblock_height),        // 7: ▄ top edge
            Constraint::Length(layout.input_height),            // 8: input textarea
            Constraint::Length(layout.halfblock_height),        // 9: ▀ bottom edge
            Constraint::Length(layout.info_height),             // 10: info bar
        ])
        .split(f.area());

    let log_area = chunks[0];
    let completion_area = chunks[1];
    let sel_header_area = chunks[2];
    let sel_items_area = chunks[3];
    let login_hdr_area = chunks[4];
    let login_body_area = chunks[5];
    let status_area = chunks[6];
    let top_hb_area = chunks[7];
    let input_area = chunks[8];
    let bot_hb_area = chunks[9];
    let info_area = chunks[10];

    // ── Chat log ──────────────────────────────────────────────────────────────
    let inner_height = log_area.height as usize;

    // Pre-wrapped lines: each Line is exactly one visual row.
    // To avoid wrapping the full log twice on every keypress, pick the likely
    // content width first using the previous frame's scrollbar state.
    let mut assumed_log_width = log_area.width as usize;
    if app.log_had_scrollbar && assumed_log_width > 1 {
        assumed_log_width -= 1;
    }

    // Use the cached lines by reference — no clone of the full Vec.
    // We determine the line count and scroll position first, then copy only
    // the visible slice (~terminal height lines) into ratatui.
    let assumed_line_count = build_log_lines_cached(app, assumed_log_width).len();
    let mut has_scrollbar = assumed_line_count > inner_height;

    // If our width assumption was wrong, rebuild once with the correct width.
    let final_log_width = if has_scrollbar != app.log_had_scrollbar {
        let rewrap_width = if has_scrollbar {
            split_scrollbar_column(log_area).0.width as usize
        } else {
            log_area.width as usize
        };
        let rewrap_count = build_log_lines_cached(app, rewrap_width).len();
        has_scrollbar = rewrap_count > inner_height;
        rewrap_width
    } else {
        assumed_log_width
    };

    // Final geometry after potential re-wrap.
    let (log_content_area, log_scrollbar_area) = if has_scrollbar {
        let (content_area, scrollbar_area) = split_scrollbar_column(log_area);
        (content_area, scrollbar_area)
    } else {
        (log_area, None)
    };

    app.log_had_scrollbar = has_scrollbar;

    // Store log height for use as page size in the event loop.
    app.last_log_height = inner_height;

    // Determine scroll position from the full line count, then extract only
    // the visible slice.  This means we clone at most `inner_height` (~40)
    // Lines regardless of how long the session is.
    let total_lines = build_log_lines_cached(app, final_log_width).len();
    let max_scroll = total_lines.saturating_sub(inner_height);

    if app.auto_scroll {
        app.log_scroll = max_scroll;
    } else {
        app.log_scroll = app.log_scroll.min(max_scroll);
        if app.log_scroll >= max_scroll {
            app.auto_scroll = true;
        }
    }

    // Build the visible slice.  Pad with empty lines at the top when the
    // content is shorter than the pane so it is anchored to the bottom.
    // We clone at most `inner_height` lines, never the full session.
    let log_scroll = app.log_scroll;
    let visible_lines: Vec<Line<'static>> = {
        let all = build_log_lines_cached(app, final_log_width);
        if total_lines <= inner_height {
            // Short log: pad top, then clone the whole (small) thing.
            let padding = inner_height - total_lines;
            let mut v: Vec<Line<'static>> = vec![Line::default(); padding];
            v.extend(all.iter().cloned());
            v
        } else {
            // Long log: clone only the visible window.
            let start = log_scroll;
            let end = (start + inner_height).min(total_lines);
            all[start..end].to_vec()
        }
    };

    // No `.scroll()` needed — we already sliced to the visible window.
    let log_paragraph =
        Paragraph::new(Text::from(visible_lines)).block(Block::default().borders(Borders::NONE));

    f.render_widget(Clear, log_area);
    f.render_widget(log_paragraph, log_content_area);

    if total_lines > inner_height {
        let mut scrollbar_state = ScrollbarState::new(max_scroll + 1).position(app.log_scroll);
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight),
            log_scrollbar_area.unwrap_or(log_area),
            &mut scrollbar_state,
        );
    }

    // ── Completion popup / resume hint ───────────────────────────────────────
    if layout.completion_height > 0 {
        if app.ask_user_freeform_mode {
            let question = app.ask_user_question.as_deref().unwrap_or("Answer");
            let prompt = format!("{question}   Enter submit   Esc cancel");
            let hint_style = Style::default()
                .fg(Color::Rgb(120, 140, 140))
                .add_modifier(ratatui::style::Modifier::DIM);
            let lines: Vec<Line<'static>> = wrap_str(&prompt, width.max(1))
                .into_iter()
                .map(|chunk| Line::from(Span::styled(chunk, hint_style)))
                .collect();
            f.render_widget(Paragraph::new(lines), completion_area);
        } else if !app.completions.is_empty() {
            let popup_lines =
                build_completion_lines(&app.completions, app.completion_selected, width);
            f.render_widget(Paragraph::new(popup_lines), completion_area);
        } else if resume_hint_visible {
            let hint = Line::from(vec![
                Span::styled(
                    "  hint: ",
                    Style::default().add_modifier(ratatui::style::Modifier::DIM),
                ),
                Span::styled("Ctrl+R", Style::default().fg(Color::Yellow)),
                Span::styled(
                    " resumes the latest session for this folder • /resume opens session picker",
                    Style::default().add_modifier(ratatui::style::Modifier::DIM),
                ),
            ]);
            f.render_widget(Paragraph::new(vec![hint]), completion_area);
        }
    }

    // ── Selection menu ────────────────────────────────────────────────────────
    if app.selection_mode {
        // Header row: title on the left, key hints on the right.
        let hints = if app.selection_filter_enabled() {
            "↑↓ navigate   type filter   Enter select   Esc cancel  "
        } else {
            "↑↓ navigate   Enter select   Esc cancel  "
        };
        let title = app.selection_title;
        let query = if app.selection_query.is_empty() {
            "".to_string()
        } else {
            format!("filter: {}", app.selection_query)
        };
        let query_width = query.width();
        let gap = width.saturating_sub(title.width() + query_width + hints.width());
        let header_line = Line::from(vec![
            Span::styled(
                title,
                Style::default()
                    .fg(Color::White)
                    .bg(SELECTION_HEADER_BG)
                    .add_modifier(ratatui::style::Modifier::BOLD),
            ),
            Span::styled(" ".repeat(gap), Style::default().bg(SELECTION_HEADER_BG)),
            Span::styled(
                query,
                Style::default().fg(Color::Yellow).bg(SELECTION_HEADER_BG),
            ),
            Span::styled(
                hints.to_string(),
                Style::default()
                    .bg(SELECTION_HEADER_BG)
                    .add_modifier(ratatui::style::Modifier::DIM),
            ),
        ]);
        f.render_widget(Paragraph::new(vec![header_line]), sel_header_area);

        // Item rows.
        if layout.selection_items_height > 0 {
            let selection_total = app.selection_items.len();
            let selection_scrollbar_needed = selection_total > MAX_SELECTION_VISIBLE;
            let (selection_content_area, selection_scrollbar_area) = if selection_scrollbar_needed {
                split_scrollbar_column(sel_items_area)
            } else {
                (sel_items_area, None)
            };
            let item_lines = if app.selection_items.is_empty() {
                vec![Line::from(vec![
                    Span::styled("  ", Style::default().bg(SELECTION_BG)),
                    Span::styled(
                        "no matches",
                        Style::default().bg(SELECTION_BG).add_modifier(
                            ratatui::style::Modifier::ITALIC | ratatui::style::Modifier::DIM,
                        ),
                    ),
                ])]
            } else {
                build_selection_lines(
                    &app.selection_items,
                    app.selection_selected,
                    app.selection_scroll,
                    selection_content_area.width as usize,
                )
            };
            f.render_widget(Paragraph::new(item_lines), selection_content_area);

            // Scrollbar when the list is longer than the visible window.
            if selection_scrollbar_needed {
                let max_scroll = selection_total - MAX_SELECTION_VISIBLE;
                let mut sb_state =
                    ScrollbarState::new(max_scroll + 1).position(app.selection_scroll);
                f.render_stateful_widget(
                    Scrollbar::new(ScrollbarOrientation::VerticalRight),
                    selection_scrollbar_area.unwrap_or(sel_items_area),
                    &mut sb_state,
                );
            }
        }
    }

    // ── Login panel ───────────────────────────────────────────────────────────
    if app.login_active {
        let provider = app
            .login_provider
            .clone()
            .unwrap_or_else(|| "provider".to_string());

        // Header: static hints — actions are accessed via Enter (action menu).
        const LOGIN_HINTS: &str = "Enter actions  Esc cancel  ";
        let title = format!("  Authenticating: {provider}");
        let gap = width.saturating_sub(title.width() + LOGIN_HINTS.width());
        let header_line = Line::from(vec![
            Span::styled(
                title,
                Style::default()
                    .fg(Color::White)
                    .bg(LOGIN_HEADER_BG)
                    .add_modifier(ratatui::style::Modifier::BOLD),
            ),
            Span::styled(" ".repeat(gap), Style::default().bg(LOGIN_HEADER_BG)),
            Span::styled(
                LOGIN_HINTS,
                Style::default()
                    .bg(LOGIN_HEADER_BG)
                    .add_modifier(ratatui::style::Modifier::DIM),
            ),
        ]);
        f.render_widget(Paragraph::new(vec![header_line]), login_hdr_area);

        // Content rows.
        let content_lines = build_login_content_lines(app, width);
        f.render_widget(Paragraph::new(content_lines), login_body_area);
    }

    // ── Halfblock edges ───────────────────────────────────────────────────────
    if !app.login_active {
        let panel_bg = if app.input_mode == InputMode::Shell {
            SHELL_INPUT_BG
        } else {
            INPUT_BG
        };
        f.render_widget(
            Paragraph::new(halfblock_line(width, '▄', panel_bg)),
            top_hb_area,
        );
        f.render_widget(
            Paragraph::new(halfblock_line(width, '▀', panel_bg)),
            bot_hb_area,
        );
    }

    // ── Status bar (throbber + provider status) ───────────────────────────────
    if layout.status_height > 0 {
        let throbber_style = Style::default()
            .fg(Color::Rgb(160, 200, 255))
            .add_modifier(ratatui::style::Modifier::BOLD);
        let status_text_style = Style::default()
            .fg(Color::Rgb(160, 160, 180))
            .add_modifier(ratatui::style::Modifier::ITALIC);

        let show_throbber = app.throbber_visible();
        let frame = THROBBER_FRAMES[(app.throbber_tick as usize) % THROBBER_FRAMES.len()];

        let status_line = match (show_throbber, &app.provider_status) {
            (true, Some(status)) => Line::from(vec![
                Span::styled(format!("{frame}"), throbber_style),
                Span::styled(format!(" {status}"), status_text_style),
            ]),
            (true, None) => Line::from(Span::styled(format!("{frame}"), throbber_style)),
            (false, Some(status)) => Line::from(Span::styled(status.clone(), status_text_style)),
            (false, None) => Line::default(),
        };
        f.render_widget(Paragraph::new(status_line), status_area);
    }

    // ── Input box ─────────────────────────────────────────────────────────────
    if !app.login_active {
        let is_shell = app.input_mode == InputMode::Shell;
        let panel_bg = if is_shell { SHELL_INPUT_BG } else { INPUT_BG };

        // ratatui-textarea scrolls horizontally for long single lines.
        // For chat-style input we want visual hard-wrapping instead.
        let input_width = input_area.width as usize;

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
        } else if app.ollama_endpoint_input_mode {
            (
                app.textarea.lines().to_vec(),
                app.textarea.cursor(),
                "ollama endpoint: ".to_string(),
                Some("http://host:11434   Enter confirm   Esc cancel".to_string()),
            )
        } else if app.open_webui_url_input_mode {
            (
                app.textarea.lines().to_vec(),
                app.textarea.cursor(),
                "open-webui URL: ".to_string(),
                Some("https://my-webui.example.com   Enter confirm   Esc cancel".to_string()),
            )
        } else if app.open_webui_token_input_mode {
            (
                app.textarea.lines().to_vec(),
                app.textarea.cursor(),
                "open-webui token: ".to_string(),
                Some("sk-…   Enter confirm   Esc cancel".to_string()),
            )
        } else if app.ask_user_freeform_mode {
            (
                app.textarea.lines().to_vec(),
                app.textarea.cursor(),
                "❓ ".to_string(),
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

        f.render_widget(paragraph, input_area);

        let cursor_x = input_area
            .x
            .saturating_add((wrapped_cursor.1 + prefix.width()) as u16);
        let cursor_y = input_area.y.saturating_add(wrapped_cursor.0 as u16);
        f.set_cursor_position((cursor_x, cursor_y));
    }

    // ── Info bar ──────────────────────────────────────────────────────────────
    if app.show_info {
        let context_window = context_window_for_model(&app.current_model);
        let used_tokens = app.latest_usage.and_then(|u| u.used_tokens());
        let thinking = ProviderKind::from_name(&app.current_provider).and_then(|kind| {
            (thinking_support_for(&kind, &app.current_model) == ThinkingSupport::Applied)
                .then_some(app.current_thinking.as_str())
        });
        let info_line = build_info_line(
            &app.current_provider,
            &app.current_model,
            thinking,
            context_window,
            used_tokens,
            width,
        );
        f.render_widget(Paragraph::new(vec![info_line]), info_area);
    }
}

// ── Login panel content rendering ─────────────────────────────────────────────

/// Indentation applied to each wrapped URL line.
const LOGIN_URL_INDENT: &str = "    ";

/// Build the content rows for the login panel.
///
/// Layout (when a URL is present):
/// ```
///   <instruction line>
///   <status / progress line>
///   URL:
///     <url line 1>
///     <url line 2>   ← only when URL wraps
///   Code: ABCD-1234  ← Copilot device flow only
/// ```
///
/// The URL is displayed as plain, selectable text wrapped across as many lines
/// as needed — no OSC 8 hyperlink tricks that would break in most terminals.
fn build_login_content_lines(app: &mut App, width: usize) -> Vec<Line<'static>> {
    let instruction_style = Style::default()
        .fg(Color::Rgb(180, 180, 200))
        .bg(LOGIN_CONTENT_BG);
    let status_style = Style::default().fg(Color::White).bg(LOGIN_CONTENT_BG);
    let url_key_style = Style::default()
        .fg(Color::Rgb(120, 200, 255))
        .bg(LOGIN_CONTENT_BG);
    let url_val_style = Style::default()
        .fg(Color::Rgb(100, 220, 100))
        .bg(LOGIN_CONTENT_BG);
    let code_key_style = Style::default()
        .fg(Color::Rgb(120, 200, 255))
        .bg(LOGIN_CONTENT_BG);
    let code_val_style = Style::default().fg(Color::Yellow).bg(LOGIN_CONTENT_BG);
    let fill_style = Style::default().bg(LOGIN_CONTENT_BG);
    let fill = |used: usize| Span::styled(" ".repeat(width.saturating_sub(used)), fill_style);

    let mut lines: Vec<Line<'static>> = Vec::new();

    // Row 0 — instruction text (flow-dependent).
    let instruction = match app.login_auth_flow {
        Some(AuthFlow::DeviceCode) => {
            "  Open the URL below, then enter the code shown into the browser."
        }
        Some(AuthFlow::RedirectCallback) => {
            "  Open the URL below; the browser will redirect back automatically."
        }
        None => "  Follow the browser prompt to authenticate.",
    };
    let instr_len = instruction.width();
    lines.push(Line::from(vec![
        Span::styled(instruction.to_string(), instruction_style),
        fill(instr_len),
    ]));

    // Row 1 — status / progress line.
    let info = app.login_info.clone();
    let info_len = info.width();
    lines.push(Line::from(vec![
        Span::styled(format!("  {info}"), status_style),
        fill(2 + info_len),
    ]));

    // Rows 2+ — URL label + wrapped URL lines.
    if let Some(url) = &app.login_url {
        let url_label = "  URL:";
        let url_label_len = url_label.width();
        lines.push(Line::from(vec![
            Span::styled(url_label.to_string(), url_key_style),
            fill(url_label_len),
        ]));

        let indent_len = LOGIN_URL_INDENT.len();
        let wrap_width = width.saturating_sub(indent_len).max(1);
        for chunk in wrap_str(url, wrap_width) {
            let used = indent_len + chunk.width();
            lines.push(Line::from(vec![
                Span::styled(LOGIN_URL_INDENT.to_string(), fill_style),
                Span::styled(chunk, url_val_style),
                fill(used),
            ]));
        }
    }

    // Final row — device code (Copilot only).
    if let Some(code) = &app.login_code {
        const CODE_PREFIX: &str = "  Code: ";
        let used = CODE_PREFIX.len() + code.len();
        lines.push(Line::from(vec![
            Span::styled(CODE_PREFIX, code_key_style),
            Span::styled(code.clone(), code_val_style),
            fill(used),
        ]));
    }

    lines
}

// ── Completion popup rendering ────────────────────────────────────────────────

/// Build one `Line` per completion item for the popup.
///
/// Layout per row:  `  <label padded>  —  <detail> <fill>`
/// Loading rows are rendered in a dim italic style with no separator.
fn build_completion_lines(
    completions: &[CompletionItem],
    selected: usize,
    terminal_width: usize,
) -> Vec<Line<'static>> {
    // Align the detail column by padding labels to the longest label string.
    let label_col = completions
        .iter()
        .filter(|c| !c.loading)
        .map(|c| c.label.len())
        .max()
        .unwrap_or(0)
        .max(8);

    const SEP: &str = "  —  ";
    const INDENT: &str = "  ";

    completions
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let bg = if i == selected {
                COMPLETION_SEL_BG
            } else {
                COMPLETION_BG
            };

            if item.loading {
                // Non-interactive status row — dim italic for loading, red for errors.
                let fill =
                    " ".repeat(terminal_width.saturating_sub(INDENT.len() + item.label.len()));
                let fg = if item.error { Color::Red } else { Color::Reset };
                return Line::from(vec![
                    Span::styled(INDENT, Style::default().bg(bg)),
                    Span::styled(
                        item.label.clone(),
                        Style::default().fg(fg).bg(bg).add_modifier(
                            ratatui::style::Modifier::ITALIC | ratatui::style::Modifier::DIM,
                        ),
                    ),
                    Span::styled(fill, Style::default().bg(bg)),
                ]);
            }

            let label_padded = format!("{:<width$}", item.label, width = label_col);
            let used = INDENT.len()
                + label_col
                + if item.detail.is_empty() {
                    0
                } else {
                    SEP.len() + item.detail.len()
                };
            let fill = " ".repeat(terminal_width.saturating_sub(used));

            // Build label spans, splitting at the match range if present.
            let label_spans: Vec<Span<'static>> = if let Some((mstart, mend)) = item.match_range {
                // Clamp to valid byte boundaries.
                let mstart = mstart.min(item.label.len());
                let mend = mend.min(item.label.len());
                let before = item.label[..mstart].to_string();
                let matched = item.label[mstart..mend].to_string();
                let after_raw = &item.label[mend..];
                // Pad the trailing portion to fill label_col.
                let after = format!(
                    "{after_raw:<pad$}",
                    pad = label_col.saturating_sub(mstart + matched.len())
                );
                vec![
                    Span::styled(before, Style::default().fg(COMPLETION_CMD_FG).bg(bg)),
                    Span::styled(
                        matched,
                        Style::default()
                            .fg(COMPLETION_MATCH_FG)
                            .bg(bg)
                            .add_modifier(ratatui::style::Modifier::BOLD),
                    ),
                    Span::styled(after, Style::default().fg(COMPLETION_CMD_FG).bg(bg)),
                ]
            } else {
                vec![Span::styled(
                    label_padded,
                    Style::default().fg(COMPLETION_CMD_FG).bg(bg),
                )]
            };

            if item.detail.is_empty() {
                let mut spans = vec![Span::styled(INDENT, Style::default().bg(bg))];
                spans.extend(label_spans);
                spans.push(Span::styled(fill, Style::default().bg(bg)));
                Line::from(spans)
            } else {
                let mut spans = vec![Span::styled(INDENT, Style::default().bg(bg))];
                spans.extend(label_spans);
                spans.push(Span::styled(
                    SEP,
                    Style::default()
                        .bg(bg)
                        .add_modifier(ratatui::style::Modifier::DIM),
                ));
                spans.push(Span::styled(
                    item.detail.clone(),
                    Style::default().fg(COMPLETION_DESC_FG).bg(bg),
                ));
                spans.push(Span::styled(fill, Style::default().bg(bg)));
                Line::from(spans)
            }
        })
        .collect()
}

// ── Selection menu rendering ──────────────────────────────────────────────────

/// Build one `Line` per item for the model selection menu.
fn build_selection_lines(
    items: &[CompletionItem],
    selected: usize,
    scroll: usize,
    terminal_width: usize,
) -> Vec<Line<'static>> {
    const INDENT: &str = "  ";

    items
        .iter()
        .enumerate()
        .skip(scroll)
        .take(MAX_SELECTION_VISIBLE)
        .map(|(i, item)| {
            let is_sel = i == selected;
            let bg = if is_sel {
                SELECTION_SEL_BG
            } else {
                SELECTION_BG
            };

            if item.loading {
                let fill =
                    " ".repeat(terminal_width.saturating_sub(INDENT.len() + item.label.width()));
                let fg = if item.error { Color::Red } else { Color::Reset };
                return Line::from(vec![
                    Span::styled(INDENT, Style::default().bg(bg)),
                    Span::styled(
                        item.label.clone(),
                        Style::default().fg(fg).bg(bg).add_modifier(
                            ratatui::style::Modifier::ITALIC | ratatui::style::Modifier::DIM,
                        ),
                    ),
                    Span::styled(fill, Style::default().bg(bg)),
                ]);
            }

            // Cursor indicator for selected row.
            let prefix = if is_sel { "▶ " } else { "  " };
            let used = INDENT.len() + prefix.width() + item.label.width();
            let fill = " ".repeat(terminal_width.saturating_sub(used));
            Line::from(vec![
                Span::styled(INDENT, Style::default().bg(bg)),
                Span::styled(prefix, Style::default().fg(Color::White).bg(bg)),
                Span::styled(
                    item.label.clone(),
                    Style::default().fg(SELECTION_ITEM_FG).bg(bg),
                ),
                Span::styled(fill, Style::default().bg(bg)),
            ])
        })
        .collect()
}

/// Build all visual lines for the chat log, pre-wrapped to `width` columns.
/// Each returned `Line` occupies exactly one terminal row.
fn build_log_lines(
    messages: &[crate::llm::Message],
    streaming: bool,
    queued_steering: &[String],
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
            Role::System => {
                // System messages are not displayed in the chat log.
            }
            Role::Assistant => {
                let thinking = msg.thinking.as_deref().unwrap_or("");
                let is_streaming_last = streaming && is_last;
                let has_answer = is_streaming_last || !msg.content.is_empty();

                // Render thinking block (if any thinking content has arrived).
                if !thinking.is_empty() {
                    append_message_dim(
                        &mut lines,
                        &format!("🧠 {}", sanitize_for_display(thinking)),
                        "",
                        width,
                    );
                    // Separator between thinking and answer is lazy: only render
                    // when an answer line will actually be shown.
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

                // Render the answer only when there is visible answer content.
                // Show the streaming cursor (▋) at the end of the answer area
                // whenever this is the active streaming message.
                if has_answer {
                    let content = if is_streaming_last && msg.content.is_empty() {
                        format!("{answer_icon} ▋")
                    } else {
                        format!("{answer_icon} {}", sanitize_for_display(&msg.content))
                    };
                    let suffix = if is_streaming_last && !msg.content.is_empty() {
                        "▋"
                    } else {
                        ""
                    };
                    append_message(&mut lines, &content, suffix, width, false);
                }
            }
            Role::ToolCall => {
                let name = msg.tool_name.as_deref().unwrap_or("unknown");
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
                        None => {
                            tool_presentation::tool_invocation_label(name, &serde_json::Value::Null)
                        }
                    }
                };

                if matches!(name, "read" | "read_file")
                    && let Some(next) = messages.get(idx + 1)
                    && next.role == Role::ToolResult
                    && let Some((start, end, total, _)) = split_read_file_header(&next.content)
                {
                    label.push_str(&format!(" [{start}-{end}/{total}]"));
                }

                let color = if name == "local_shell" {
                    Color::LightBlue
                } else {
                    Color::Cyan
                };
                append_message_colored(&mut lines, &label, width, color);
            }
            Role::ToolResult => {
                let prev_is_read = matches!(
                    messages.get(idx.saturating_sub(1)),
                    Some(prev)
                        if prev.role == Role::ToolCall
                            && matches!(prev.tool_name.as_deref(), Some("read" | "read_file"))
                );
                let prev_is_local_shell = matches!(
                    messages.get(idx.saturating_sub(1)),
                    Some(prev)
                        if prev.role == Role::ToolCall
                            && matches!(prev.tool_name.as_deref(), Some("local_shell"))
                );

                let content_for_display = if prev_is_read {
                    split_read_file_header(&msg.content)
                        .map(|(_, _, _, body)| body.to_string())
                        .unwrap_or_else(|| msg.content.clone())
                } else {
                    msg.content.clone()
                };
                // Sanitize for display: strip trailing whitespace per line,
                // leading/trailing newlines, and collapse excess blank lines.
                // Pre-truncate to a generous limit before sanitizing so we
                // don't pay O(n) cost on 91 KB tool results when only 200
                // display chars will ever be shown.
                const DISPLAY_CHARS: usize = 200;
                const SANITIZE_LIMIT: usize = DISPLAY_CHARS * 5;
                // Record whether the original content exceeds the display cap
                // *before* we slice it, so the ellipsis is shown correctly.
                let original_overflows = content_for_display.chars().nth(DISPLAY_CHARS).is_some();
                let sanitize_input = if original_overflows {
                    // Find the byte boundary for SANITIZE_LIMIT chars.
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
                append_tool_result_block(&mut lines, &display, width, color);
            }
        }
    }

    for queued in queued_steering {
        append_message(&mut lines, &format!("🕹️ {queued}"), "", width, false);
    }

    lines
}

/// Append pre-wrapped colored lines for a tool label.
/// Preserves explicit newlines in `content`, wrapping each segment as needed.
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

/// Append tool output as a colored block with a left marker on every visual line.
fn append_tool_result_block(
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

/// Append pre-wrapped dim (thinking) lines for one block.
/// Same wrapping logic as `append_message` but renders using `DIM` on default fg.
fn append_message_dim(
    out: &mut Vec<Line<'static>>,
    content: &str,
    suffix: &'static str,
    width: usize,
) {
    let dim_style = Style::default().add_modifier(ratatui::style::Modifier::DIM);

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
                spans.push(Span::styled(
                    suffix,
                    Style::default().add_modifier(ratatui::style::Modifier::DIM),
                ));
            }
            out.push(Line::from(spans));
        }
    }
}

/// Append pre-wrapped visual lines for one message to `out`.
///
/// `user` — when true each line is padded to `width` and given a grey
/// background so it stands out from assistant replies.
fn append_message(
    out: &mut Vec<Line<'static>>,
    content: &str,
    suffix: &'static str,
    width: usize,
    user: bool,
) {
    let user_bg_style = Style::default().bg(USER_BG);

    // Split on explicit newlines first, then wrap each segment to width.
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
                // Pad to `width` so the background spans the full line.
                // Use unicode-aware column width, not scalar char count.
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

/// Sanitize text for display in the chat log.
///
/// Rules applied in order:
/// 1. Strip trailing whitespace (spaces/tabs) from every line.
/// 2. Strip leading newlines from the start of the string.
/// 3. Strip trailing newlines from the end of the string.
/// 4. Collapse runs of 3+ consecutive newlines to exactly 2.
///
/// Leading spaces/tabs on content lines (indentation) are never removed.
/// The original message content stored in memory is not affected.
fn sanitize_for_display(text: &str) -> String {
    // Pass 1: strip trailing whitespace from each line.
    let mut s = String::with_capacity(text.len());
    for line in text.split('\n') {
        s.push_str(line.trim_end());
        s.push('\n');
    }
    // Remove the extra trailing '\n' added after the last split segment.
    if s.ends_with('\n') {
        s.pop();
    }

    // Pass 2+3: strip leading and trailing newlines.
    // Use trim_matches which is char-aware and safe for multi-byte characters.
    let s = s.trim_matches('\n');

    // Pass 4: collapse 3+ consecutive newlines to 2.
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

fn split_read_file_header(content: &str) -> Option<(usize, usize, usize, &str)> {
    let mut lines = content.lines();
    let first = lines.next()?;

    let rest = first.strip_prefix("[lines ")?;
    let (range, total_with_bracket) = rest.split_once(" of ")?;
    let total = total_with_bracket
        .strip_suffix(']')?
        .parse::<usize>()
        .ok()?;
    let (start, end) = range.split_once('-')?;
    let start = start.parse::<usize>().ok()?;
    let end = end.parse::<usize>().ok()?;

    let header_len = first.len();
    let body = if content.len() > header_len {
        // Skip the trailing '\n' after the header when present.
        content.get((header_len + 1)..).unwrap_or("")
    } else {
        ""
    };

    Some((start, end, total, body))
}

/// Split `text` into lines of at most `width` display columns, preserving
/// internal whitespace. Uses `textwrap` for word-boundary splitting and
/// `unicode-width` for correct column measurement of CJK / emoji characters.
/// Always returns at least one element (empty string when `text` is empty).
fn wrap_str(text: &str, width: usize) -> Vec<String> {
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

/// Wrap a single input line.
///
/// Rule set:
/// - Regular tokens (<= 50% of the viewport width) use normal word-wrap.
/// - Long tokens (> 50% of width) are split at the viewport boundary.
fn wrap_input_line(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    if text.is_empty() {
        return vec![String::new()];
    }

    let mut out: Vec<String> = Vec::new();
    let mut line = String::new();
    let mut line_w = 0usize;

    // Iterate over alternating runs of whitespace and non-whitespace so we can
    // preserve typed spaces while applying token-aware wrapping.
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
            // Preserve whitespace exactly; wrap at viewport boundary if needed.
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
            // Requested behaviour: split long tokens at window boundary.
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
        } else if *line_w + token_w <= width {
            append_piece(line, line_w, run_text);
        } else if line.is_empty() {
            // Defensive: should rarely happen for "small" tokens, but keep safe.
            append_piece(line, line_w, run_text);
        } else {
            flush_line(out, line, line_w);
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

#[derive(Debug, Clone)]
struct WrappedInput {
    lines: Vec<String>,
    cursor: (usize, usize),
}

fn wrap_input_for_render(lines: &[String], cursor: (usize, usize), width: usize) -> WrappedInput {
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

fn split_scrollbar_column(area: Rect) -> (Rect, Option<Rect>) {
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

fn normalize_terminal_segment(text: &str, start_col: usize) -> String {
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

// ── Info bar ──────────────────────────────────────────────────────────────────

/// Background colour for the info bar.
const INFO_BG: Color = Color::Rgb(20, 20, 30);

/// Build the single info-bar `Line` showing provider / model / context window.
fn build_info_line<'a>(
    provider: &str,
    model: &str,
    thinking: Option<&str>,
    context_window: Option<usize>,
    used_tokens: Option<usize>,
    width: usize,
) -> Line<'a> {
    let sep_style = Style::default().fg(Color::Rgb(60, 60, 80)).bg(INFO_BG);
    let key_style = Style::default().fg(Color::Rgb(100, 100, 130)).bg(INFO_BG);
    let val_style = Style::default().fg(Color::Rgb(180, 200, 255)).bg(INFO_BG);
    let fill_style = Style::default().bg(INFO_BG);
    let hint_style = Style::default().fg(Color::Rgb(60, 60, 80)).bg(INFO_BG);

    let hint = "Ctrl+I";
    let context_value = format_context_value(context_window, used_tokens);
    // Build all the content spans.
    let mut content_spans: Vec<Span<'a>> = vec![
        Span::styled(" ", fill_style),
        Span::styled("provider", key_style),
        Span::styled(" ", fill_style),
        Span::styled(provider.to_string(), val_style),
        Span::styled("  │  ", sep_style),
        Span::styled("model", key_style),
        Span::styled(" ", fill_style),
        Span::styled(model.to_string(), val_style),
    ];

    if let Some(thinking) = thinking {
        content_spans.push(Span::styled("  │  ", sep_style));
        content_spans.push(Span::styled("thinking", key_style));
        content_spans.push(Span::styled(" ", fill_style));
        content_spans.push(Span::styled(thinking.to_string(), val_style));
    }

    content_spans.push(Span::styled("  │  ", sep_style));
    content_spans.push(Span::styled("context", key_style));
    content_spans.push(Span::styled(" ", fill_style));
    content_spans.push(Span::styled(context_value.clone(), val_style));

    // Calculate used columns (approximate; ASCII only for labels).
    let mut used: usize = 1 // leading space
        + "provider".len() + 1 + provider.len()
        + 5 // sep
        + "model".len() + 1 + model.len();

    if let Some(thinking) = thinking {
        used += 5 + "thinking".len() + 1 + thinking.len();
    }

    used += 5 + "context".len() + 1 + context_value.len();

    let hint_len = hint.len() + 1; // hint + trailing space
    let fill_len = width.saturating_sub(used + hint_len);

    let mut spans = content_spans;
    spans.push(Span::styled(" ".repeat(fill_len), fill_style));
    spans.push(Span::styled(hint.to_string(), hint_style));
    spans.push(Span::styled(" ", fill_style));

    Line::from(spans)
}

/// Format context display value for the info line.
fn format_context_value(context_window: Option<usize>, used_tokens: Option<usize>) -> String {
    match context_window {
        Some(max) => {
            let max_fmt = format_context_size(max);
            if let Some(used) = used_tokens {
                let pct = ((used.saturating_mul(100)) / max.max(1)).min(999);
                format!("{} / {} ({}%)", format_context_size(used), max_fmt, pct)
            } else {
                max_fmt
            }
        }
        None => "unknown".to_string(),
    }
}

/// Format a context window token count as a human-readable string,
/// e.g. 128000 → "128k", 200000 → "200k", 8192 → "8k".
fn format_context_size(n: usize) -> String {
    if n >= 1_000 {
        format!("{}k", n / 1_000)
    } else {
        n.to_string()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::{
        agent::AgentLoopConfig,
        llm::{AssistantPhase, Message},
        provider::ProviderKind,
        thinking::ThinkingLevel,
    };
    use serde_json::json;

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
    }

    fn make_app() -> App {
        App::new(
            "gpt-4o",
            &ProviderKind::Copilot,
            ThinkingLevel::Medium,
            AgentLoopConfig {
                tools: HashMap::new(),
                file_tracker: std::sync::Arc::new(std::sync::Mutex::new(
                    crate::agent::FileTracker::new(),
                )),
                tool_output_log: std::sync::Arc::new(std::sync::Mutex::new(
                    crate::agent::ToolOutputLog::new("test"),
                )),
                before_tool_call: None,
                after_tool_call: None,
            },
        )
    }

    fn render_to_plain_lines(app: &mut App, width: u16, height: u16) -> Vec<String> {
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).expect("test terminal");
        terminal.draw(|f| draw(f, app)).expect("draw succeeds");

        buffer_to_plain_lines(terminal.backend().buffer(), width, height)
    }

    fn buffer_to_plain_lines(
        buf: &ratatui::buffer::Buffer,
        width: u16,
        height: u16,
    ) -> Vec<String> {
        (0..height)
            .map(|y| {
                let mut row = String::new();
                for x in 0..width {
                    row.push_str(buf[(x, y)].symbol());
                }
                row.trim_end().to_string()
            })
            .collect()
    }

    #[test]
    fn input_wrap_prefers_word_boundaries() {
        let chunks = wrap_input_line("hello world from tau", 11);
        assert_eq!(
            chunks,
            vec!["hello world".to_string(), " from tau".to_string()]
        );
    }

    #[test]
    fn input_wrap_splits_long_tokens_at_viewport_boundary() {
        let chunks = wrap_input_line("small superlongtokenhere", 10);
        assert_eq!(
            chunks,
            vec![
                "small supe".to_string(),
                "rlongtoken".to_string(),
                "here".to_string()
            ]
        );
    }

    #[test]
    fn input_visual_line_count_wraps_long_lines() {
        let lines = vec!["short".to_string(), "12345 67890".to_string()];
        let count = input_visual_line_count(&lines, 6);
        assert_eq!(count, 3);
    }

    #[test]
    fn layout_uses_visual_input_line_count_for_wrapped_input() {
        let wrapped_lines = input_visual_line_count(&["a very long single line".to_string()], 8);
        assert!(wrapped_lines > 1);

        let heights = compute_panel_heights(PanelInputs {
            terminal_height: 20,
            width: 8,
            input_line_count: wrapped_lines,
            show_info: false,
            login_active: false,
            selection_mode: false,
            selection_items_len: 0,
            completions_len: 0,
            resume_hint_visible: false,
            ask_user_freeform_mode: false,
            ask_user_question: None,
            login_url: None,
            has_login_code: false,
            streaming: false,
            has_provider_status: false,
        });

        assert_eq!(heights.input_height as usize, wrapped_lines);
    }

    #[test]
    fn layout_hides_input_and_halfblocks_when_login_active() {
        let heights = compute_panel_heights(PanelInputs {
            terminal_height: 40,
            width: 100,
            input_line_count: 8,
            show_info: false,
            login_active: true,
            selection_mode: false,
            selection_items_len: 0,
            completions_len: 3,
            resume_hint_visible: false,
            ask_user_freeform_mode: false,
            ask_user_question: None,
            login_url: None,
            has_login_code: false,
            streaming: false,
            has_provider_status: false,
        });

        assert_eq!(heights.input_height, 0);
        assert_eq!(heights.halfblock_height, 0);
        assert_eq!(heights.login_header_height, 1);
        assert!(heights.login_content_height >= 2);
    }

    #[test]
    fn layout_hides_completion_when_login_or_selection_active() {
        let login = compute_panel_heights(PanelInputs {
            terminal_height: 40,
            width: 100,
            input_line_count: 2,
            show_info: false,
            login_active: true,
            selection_mode: false,
            selection_items_len: 0,
            completions_len: 5,
            resume_hint_visible: false,
            ask_user_freeform_mode: false,
            ask_user_question: None,
            login_url: None,
            has_login_code: false,
            streaming: false,
            has_provider_status: false,
        });
        let selection = compute_panel_heights(PanelInputs {
            terminal_height: 40,
            width: 100,
            input_line_count: 2,
            show_info: false,
            login_active: false,
            selection_mode: true,
            selection_items_len: 4,
            completions_len: 5,
            resume_hint_visible: false,
            ask_user_freeform_mode: false,
            ask_user_question: None,
            login_url: None,
            has_login_code: false,
            streaming: false,
            has_provider_status: false,
        });

        assert_eq!(login.completion_height, 0);
        assert_eq!(selection.completion_height, 0);
    }

    #[test]
    fn layout_shows_resume_hint_row_when_applicable() {
        let heights = compute_panel_heights(PanelInputs {
            terminal_height: 30,
            width: 100,
            input_line_count: 1,
            show_info: false,
            login_active: false,
            selection_mode: false,
            selection_items_len: 0,
            completions_len: 0,
            resume_hint_visible: true,
            ask_user_freeform_mode: false,
            ask_user_question: None,
            login_url: None,
            has_login_code: false,
            streaming: false,
            has_provider_status: false,
        });
        assert_eq!(heights.completion_height, 1);
    }

    #[test]
    fn layout_ask_user_freeform_wraps_question_across_rows() {
        let heights = compute_panel_heights(PanelInputs {
            terminal_height: 30,
            width: 24,
            input_line_count: 1,
            show_info: false,
            login_active: false,
            selection_mode: false,
            selection_items_len: 0,
            completions_len: 0,
            resume_hint_visible: false,
            ask_user_freeform_mode: true,
            ask_user_question: Some(
                "Please provide a detailed response with enough words to force wrapping",
            ),
            login_url: None,
            has_login_code: false,
            streaming: false,
            has_provider_status: false,
        });

        assert!(heights.completion_height > 1, "{heights:?}");
    }

    #[test]
    fn draw_ask_user_freeform_renders_full_question() {
        let mut app = make_app();
        app.ask_user_freeform_mode = true;
        app.ask_user_question = Some(
            "Please provide a detailed response with enough words to force wrapping".to_string(),
        );

        let lines = render_to_plain_lines(&mut app, 24, 12);
        let joined = lines.join("\n");
        assert!(joined.contains("Please provide"), "{joined}");
        assert!(joined.contains("wrapping"), "{joined}");
        assert!(joined.contains("Esc cancel"), "{joined}");
    }

    #[test]
    fn layout_selection_item_rows_are_clamped_to_max_visible() {
        let heights = compute_panel_heights(PanelInputs {
            terminal_height: 40,
            width: 100,
            input_line_count: 1,
            show_info: false,
            login_active: false,
            selection_mode: true,
            selection_items_len: MAX_SELECTION_VISIBLE + 10,
            completions_len: 0,
            resume_hint_visible: false,
            ask_user_freeform_mode: false,
            ask_user_question: None,
            login_url: None,
            has_login_code: false,
            streaming: false,
            has_provider_status: false,
        });

        assert_eq!(heights.selection_header_height, 1);
        assert_eq!(
            heights.selection_items_height as usize,
            MAX_SELECTION_VISIBLE
        );
    }

    #[test]
    fn layout_input_height_is_capped_at_40_percent_of_terminal() {
        let heights = compute_panel_heights(PanelInputs {
            terminal_height: 20,
            width: 80,
            input_line_count: 99,
            show_info: false,
            login_active: false,
            selection_mode: false,
            selection_items_len: 0,
            completions_len: 0,
            resume_hint_visible: false,
            ask_user_freeform_mode: false,
            ask_user_question: None,
            login_url: None,
            has_login_code: false,
            streaming: false,
            has_provider_status: false,
        });
        assert_eq!(heights.input_height, 8);
        assert_eq!(heights.halfblock_height, 1);
    }

    #[test]
    fn layout_info_bar_height_follows_toggle() {
        let hidden = compute_panel_heights(PanelInputs {
            terminal_height: 20,
            width: 80,
            input_line_count: 1,
            show_info: false,
            login_active: false,
            selection_mode: false,
            selection_items_len: 0,
            completions_len: 0,
            resume_hint_visible: false,
            ask_user_freeform_mode: false,
            ask_user_question: None,
            login_url: None,
            has_login_code: false,
            streaming: false,
            has_provider_status: false,
        });
        let shown = compute_panel_heights(PanelInputs {
            terminal_height: 20,
            width: 80,
            input_line_count: 1,
            show_info: true,
            login_active: false,
            selection_mode: false,
            selection_items_len: 0,
            completions_len: 0,
            resume_hint_visible: false,
            ask_user_freeform_mode: false,
            ask_user_question: None,
            login_url: None,
            has_login_code: false,
            streaming: false,
            has_provider_status: false,
        });

        assert_eq!(hidden.info_height, 0);
        assert_eq!(shown.info_height, 1);
    }

    #[test]
    fn layout_handles_small_terminals_without_underflow() {
        let heights = compute_panel_heights(PanelInputs {
            terminal_height: 1,
            width: 2,
            input_line_count: 0,
            show_info: true,
            login_active: true,
            selection_mode: true,
            selection_items_len: 0,
            completions_len: 0,
            resume_hint_visible: true,
            ask_user_freeform_mode: false,
            ask_user_question: None,
            login_url: Some("https://example.com/very/long/url"),
            has_login_code: true,
            streaming: false,
            has_provider_status: false,
        });

        assert!(heights.input_height <= 1);
        assert_eq!(heights.selection_header_height, 1);
        assert_eq!(heights.selection_items_height, 1);
        assert!(heights.login_content_height >= 2);
    }

    #[test]
    fn draw_login_mode_renders_auth_header_and_hides_input_textarea() {
        let mut app = make_app();
        app.login_active = true;
        app.login_provider = Some("copilot".to_string());
        app.login_info = "Waiting for browser".to_string();

        app.textarea.insert_char('x');

        let lines = render_to_plain_lines(&mut app, 80, 20);
        let joined = lines.join("\n");
        assert!(joined.contains("Authenticating: copilot"), "{joined}");
        assert!(!joined.contains('x'), "{joined}");
    }

    #[test]
    fn draw_selection_mode_renders_title_and_visible_items() {
        let mut app = make_app();
        app.selection_mode = true;
        app.selection_title = "  Pick item  ";
        app.selection_items = vec![
            CompletionItem {
                label: "alpha".to_string(),
                detail: String::new(),
                complete_to: String::new(),
                loading: false,
                error: false,
                match_range: None,
            },
            CompletionItem {
                label: "beta".to_string(),
                detail: String::new(),
                complete_to: String::new(),
                loading: false,
                error: false,
                match_range: None,
            },
        ];

        let lines = render_to_plain_lines(&mut app, 80, 20);
        let joined = lines.join("\n");
        assert!(joined.contains("Pick item"), "{joined}");
        assert!(joined.contains("alpha"), "{joined}");
        assert!(joined.contains("beta"), "{joined}");
    }

    #[test]
    fn draw_info_bar_renders_provider_model_context_sections() {
        let mut app = make_app();
        app.show_info = true;

        let lines = render_to_plain_lines(&mut app, 120, 20);
        let joined = lines.join("\n");
        assert!(joined.contains("provider copilot"), "{joined}");
        assert!(joined.contains("model gpt-4o"), "{joined}");
        assert!(joined.contains("context"), "{joined}");
    }

    #[test]
    fn login_content_uses_device_flow_instruction() {
        let mut app = make_app();
        app.login_auth_flow = Some(AuthFlow::DeviceCode);
        app.login_info = "Waiting".to_string();

        let lines = build_login_content_lines(&mut app, 80);
        let row0 = line_text(&lines[0]);
        assert!(row0.contains("enter the code shown"), "{row0}");
    }

    #[test]
    fn login_content_uses_redirect_flow_instruction() {
        let mut app = make_app();
        app.login_auth_flow = Some(AuthFlow::RedirectCallback);
        app.login_info = "Waiting".to_string();

        let lines = build_login_content_lines(&mut app, 80);
        let row0 = line_text(&lines[0]);
        assert!(row0.contains("redirect back automatically"), "{row0}");
    }

    #[test]
    fn login_content_wraps_url_for_narrow_width() {
        let mut app = make_app();
        app.login_info = "Waiting".to_string();
        app.login_url = Some("https://example.com/very/long/path/that/should/wrap".to_string());

        let lines = build_login_content_lines(&mut app, 20);
        assert!(
            lines.len() >= 5,
            "expected wrapped URL rows, got {}",
            lines.len()
        );
        assert!(lines.iter().any(|l| line_text(l).contains("URL:")));
    }

    #[test]
    fn login_content_shows_code_row_only_when_present() {
        let mut without_code = make_app();
        without_code.login_info = "Waiting".to_string();
        let lines_without = build_login_content_lines(&mut without_code, 80);
        assert!(!lines_without.iter().any(|l| line_text(l).contains("Code:")));

        let mut with_code = make_app();
        with_code.login_info = "Waiting".to_string();
        with_code.login_code = Some("ABCD-1234".to_string());
        let lines_with = build_login_content_lines(&mut with_code, 80);
        assert!(lines_with.iter().any(|l| line_text(l).contains("Code:")));
    }

    #[test]
    fn completion_rows_omit_separator_when_detail_empty() {
        let items = vec![CompletionItem {
            label: "/model gpt-4o".to_string(),
            detail: String::new(),
            complete_to: "/model gpt-4o".to_string(),
            loading: false,
            error: false,
            match_range: None,
        }];
        let lines = build_completion_lines(&items, 0, 80);
        assert!(!line_text(&lines[0]).contains('—'));
    }

    #[test]
    fn completion_loading_rows_render_without_detail_column() {
        let items = vec![CompletionItem {
            label: "loading models…".to_string(),
            detail: "ignored".to_string(),
            complete_to: String::new(),
            loading: true,
            error: false,
            match_range: None,
        }];
        let lines = build_completion_lines(&items, 0, 80);
        let row = line_text(&lines[0]);
        assert!(row.contains("loading models…"));
        assert!(!row.contains('—'));
    }

    #[test]
    fn completion_label_column_alignment_is_structurally_consistent() {
        let items = vec![
            CompletionItem {
                label: "/m".to_string(),
                detail: "first".to_string(),
                complete_to: "/m".to_string(),
                loading: false,
                error: false,
                match_range: None,
            },
            CompletionItem {
                label: "/very-long-command".to_string(),
                detail: "second".to_string(),
                complete_to: "/very-long-command".to_string(),
                loading: false,
                error: false,
                match_range: None,
            },
        ];

        let lines = build_completion_lines(&items, 0, 120);
        let first = line_text(&lines[0]);
        let second = line_text(&lines[1]);
        assert_eq!(first.find('—'), second.find('—'));
    }

    #[test]
    fn selection_window_respects_scroll_and_max_visible() {
        let items: Vec<CompletionItem> = (0..30)
            .map(|i| CompletionItem {
                label: format!("item-{i}"),
                detail: String::new(),
                complete_to: String::new(),
                loading: false,
                error: false,
                match_range: None,
            })
            .collect();

        let lines = build_selection_lines(&items, 0, 5, 80);
        assert_eq!(lines.len(), MAX_SELECTION_VISIBLE);
        assert!(line_text(&lines[0]).contains("item-5"));
        assert!(line_text(lines.last().expect("expected last line")).contains("item-16"));
    }

    #[test]
    fn selection_selected_row_contains_cursor_prefix() {
        let items: Vec<CompletionItem> = (0..8)
            .map(|i| CompletionItem {
                label: format!("item-{i}"),
                detail: String::new(),
                complete_to: String::new(),
                loading: false,
                error: false,
                match_range: None,
            })
            .collect();

        let lines = build_selection_lines(&items, 6, 5, 80);
        assert!(line_text(&lines[1]).contains("▶ item-6"));
        assert!(!line_text(&lines[0]).contains('▶'));
    }

    #[test]
    fn selection_loading_row_renders_label_only() {
        let items = vec![CompletionItem {
            label: "fetching…".to_string(),
            detail: "unused".to_string(),
            complete_to: String::new(),
            loading: true,
            error: false,
            match_range: None,
        }];

        let lines = build_selection_lines(&items, 0, 0, 80);
        let row = line_text(&lines[0]);
        assert!(row.contains("fetching…"));
        assert!(!row.contains("▶ "));
    }

    #[test]
    fn hidden_user_messages_are_not_rendered() {
        let mut hidden = Message::user("secret");
        hidden.hidden = true;
        let lines = build_log_lines(&[hidden, Message::assistant("shown")], false, &[], 80);
        assert_eq!(lines.len(), 1);
        assert_eq!(line_text(&lines[0]), "💬 shown");
    }

    #[test]
    fn streaming_empty_assistant_message_shows_cursor() {
        let lines = build_log_lines(&[Message::assistant("")], true, &[], 80);
        assert_eq!(line_text(&lines[0]), "💭 ▋");
    }

    #[test]
    fn stream_suffix_is_only_on_final_visible_chunk() {
        let lines = build_log_lines(
            &[Message::assistant("abcdefghijklmnopqrstuvwxyz")],
            true,
            &[],
            8,
        );
        let rows_with_cursor: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter_map(|(idx, l)| line_text(l).contains('▋').then_some(idx))
            .collect();

        assert_eq!(rows_with_cursor.len(), 1);
        assert_eq!(rows_with_cursor[0], lines.len() - 1);
    }

    #[test]
    fn user_message_renders_block_edges() {
        let lines = build_log_lines(&[Message::user("hi")], false, &[], 10);
        assert_eq!(line_text(&lines[0]), "▄▄▄▄▄▄▄▄▄▄");
        assert_eq!(line_text(&lines[1]), "hi        ");
        assert_eq!(line_text(&lines[2]), "▀▀▀▀▀▀▀▀▀▀");
    }

    #[test]
    fn read_file_tool_call_annotates_range_from_next_result_header() {
        let messages = vec![
            Message::tool_call("1", "read_file", json!({"path": "src/main.rs"})),
            Message::tool_result("1", "[lines 10-20 of 300]\nalpha\nbeta", false),
        ];

        let lines = build_log_lines(&messages, false, &[], 120);
        assert!(line_text(&lines[0]).contains("[10-20/300]"));
    }

    #[test]
    fn read_file_result_display_omits_header_line() {
        let messages = vec![
            Message::tool_call("1", "read_file", json!({"path": "src/main.rs"})),
            Message::tool_result("1", "[lines 10-20 of 300]\nalpha\nbeta", false),
        ];

        let lines = build_log_lines(&messages, false, &[], 120);
        let rendered = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
        assert!(!rendered.contains("[lines 10-20 of 300]"));
        assert!(rendered.contains("│alpha"));
    }

    #[test]
    fn tool_result_preview_truncates_with_ellipsis_after_limit() {
        let messages = vec![
            Message::tool_call("1", "bash", json!({"command": "echo hi"})),
            Message::tool_result("1", "a".repeat(250), false),
        ];

        let lines = build_log_lines(&messages, false, &[], 300);
        let rendered = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
        assert!(rendered.contains('…'));
    }

    #[test]
    fn shell_tool_call_preserves_multiline_command_display() {
        let messages = vec![Message::tool_call(
            "1",
            "bash",
            json!({"command": "echo one\necho two\necho three"}),
        )];

        let lines = build_log_lines(&messages, false, &[], 120);
        let rendered = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
        assert!(
            rendered.contains("💻 echo one\necho two\necho three"),
            "{rendered}"
        );
    }

    #[test]
    fn shell_tool_call_truncates_above_five_lines_with_ellipsis_on_its_own_line() {
        let messages = vec![Message::tool_call(
            "1",
            "bash",
            json!({"command": "l1\nl2\nl3\nl4\nl5\nl6"}),
        )];

        let lines = build_log_lines(&messages, false, &[], 120);
        let rendered = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
        assert!(rendered.contains("💻 l1\nl2\nl3\nl4\nl5\n…"), "{rendered}");
        assert!(!rendered.contains("\n\n…"), "{rendered}");
        assert!(!rendered.contains("l6"), "{rendered}");
    }

    #[test]
    fn assistant_lines_are_prefixed_with_speech_bubble() {
        let messages = vec![Message::assistant("hello")];
        let lines = build_log_lines(&messages, false, &[], 80);
        assert_eq!(line_text(&lines[0]), "💬 hello");
    }

    #[test]
    fn assistant_provisional_phase_uses_thought_bubble() {
        let mut msg = Message::assistant("working");
        msg.assistant_phase = Some(AssistantPhase::Provisional);
        let lines = build_log_lines(&[msg], false, &[], 80);
        assert_eq!(line_text(&lines[0]), "💭 working");
    }

    #[test]
    fn assistant_unknown_phase_streaming_uses_thought_bubble() {
        let mut msg = Message::assistant("streaming");
        msg.assistant_phase = Some(AssistantPhase::Unknown);
        let lines = build_log_lines(&[msg], true, &[], 80);
        assert_eq!(line_text(&lines[0]), "💭 streaming▋");
    }

    #[test]
    fn assistant_thinking_is_prefixed_with_brain() {
        let mut msg = Message::assistant("answer");
        msg.thinking = Some("planning".to_string());
        let messages = vec![msg];
        let lines = build_log_lines(&messages, false, &[], 80);
        assert_eq!(line_text(&lines[0]), "🧠 planning");
        assert_eq!(line_text(&lines[2]), "💬 answer");
    }

    #[test]
    fn queued_steering_renders_with_joystick_at_bottom() {
        let messages = vec![Message::assistant("done")];
        let queued = vec!["wait, do this first".to_string()];
        let lines = build_log_lines(&messages, false, &queued, 80);
        assert_eq!(
            line_text(lines.last().expect("expected line")),
            "🕹️ wait, do this first"
        );
    }

    #[test]
    fn wrap_str_splits_at_width() {
        // "hello world" at width 5 should produce at least two chunks.
        let chunks = wrap_str("hello world", 5);
        assert!(
            chunks.len() >= 2,
            "expected at least 2 chunks, got: {:?}",
            chunks
        );
    }

    #[test]
    fn wrap_str_handles_empty_input() {
        let chunks = wrap_str("", 80);
        assert_eq!(chunks, vec![String::new()]);
    }

    #[test]
    fn wrap_str_handles_width_zero() {
        // width=0 is the degenerate case; the whole string is returned as-is.
        let chunks = wrap_str("some text", 0);
        assert_eq!(chunks, vec!["some text".to_string()]);
    }

    #[test]
    fn wrap_str_short_text_fits_in_one_chunk() {
        let chunks = wrap_str("hi", 80);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], "hi");
    }

    #[test]
    fn normalize_terminal_segment_expands_tabs_from_current_column() {
        assert_eq!(normalize_terminal_segment("\talpha", 0), "    alpha");
        assert_eq!(normalize_terminal_segment("\talpha", 1), "   alpha");
    }

    #[test]
    fn normalize_terminal_segment_replaces_control_chars_with_spaces() {
        assert_eq!(normalize_terminal_segment("a\rb\u{1b}[31m", 0), "a b [31m");
    }

    #[test]
    fn tool_result_block_prefixes_each_line() {
        let mut out = Vec::new();
        append_tool_result_block(&mut out, "line one\nline two", 80, Color::Green);
        assert_eq!(out.len(), 2);
        assert_eq!(line_text(&out[0]), "│line one");
        assert_eq!(line_text(&out[1]), "│line two");
    }

    #[test]
    fn tool_result_block_omits_trailing_blank_line() {
        let mut out = Vec::new();
        append_tool_result_block(&mut out, "uptime output\n", 80, Color::Green);
        assert_eq!(out.len(), 1);
        assert_eq!(line_text(&out[0]), "│uptime output");
    }

    #[test]
    fn tool_result_block_wraps_and_keeps_prefix() {
        let mut out = Vec::new();
        append_tool_result_block(&mut out, "abcdef", 4, Color::Green);
        assert!(out.len() >= 2);
        for line in out {
            let text = line_text(&line);
            assert!(text.starts_with('│'));
            assert!(unicode_width::UnicodeWidthStr::width(text.as_str()) <= 4);
        }
    }

    #[test]
    fn tool_result_block_expands_leading_tabs_after_prefix() {
        let mut out = Vec::new();
        append_tool_result_block(&mut out, "\talpha", 20, Color::Green);
        assert_eq!(line_text(&out[0]), "│   alpha");
    }

    #[test]
    fn redraw_clears_stale_tool_output_cells() {
        let backend = ratatui::backend::TestBackend::new(40, 10);
        let mut terminal = ratatui::Terminal::new(backend).expect("test terminal");
        let mut app = make_app();
        app.messages = vec![Message::tool_result(
            "1",
            "tool output that used to be much longer",
            false,
        )];
        app.mark_log_dirty();

        terminal
            .draw(|f| draw(f, &mut app))
            .expect("first draw succeeds");

        app.messages = vec![Message::tool_result("1", "short", false)];
        app.mark_log_dirty();

        terminal
            .draw(|f| draw(f, &mut app))
            .expect("second draw succeeds");

        let joined = buffer_to_plain_lines(terminal.backend().buffer(), 40, 10).join("\n");
        assert!(joined.contains("│short"), "{joined}");
        assert!(!joined.contains("much longer"), "{joined}");
    }

    #[test]
    fn log_user_background_does_not_extend_into_scrollbar_column() {
        let backend = ratatui::backend::TestBackend::new(20, 8);
        let mut terminal = ratatui::Terminal::new(backend).expect("test terminal");
        let mut app = make_app();
        app.messages = vec![
            Message::user("one"),
            Message::user("two"),
            Message::user("three"),
            Message::user("four"),
        ];

        terminal.draw(|f| draw(f, &mut app)).expect("draw succeeds");

        let buf = terminal.backend().buffer();
        let rightmost_x = 19;
        for y in 0..8 {
            assert_ne!(buf[(rightmost_x, y)].bg, USER_BG);
        }
    }

    #[test]
    fn selection_background_does_not_extend_into_scrollbar_column() {
        let backend = ratatui::backend::TestBackend::new(30, 20);
        let mut terminal = ratatui::Terminal::new(backend).expect("test terminal");
        let mut app = make_app();
        app.selection_mode = true;
        app.selection_items = (0..30)
            .map(|i| CompletionItem {
                label: format!("item-{i}"),
                detail: String::new(),
                complete_to: String::new(),
                loading: false,
                error: false,
                match_range: None,
            })
            .collect();

        terminal.draw(|f| draw(f, &mut app)).expect("draw succeeds");

        let buf = terminal.backend().buffer();
        let rightmost_x = 29;
        for y in 0..20 {
            let bg = buf[(rightmost_x, y)].bg;
            assert_ne!(bg, SELECTION_BG);
            assert_ne!(bg, SELECTION_SEL_BG);
        }
    }

    #[test]
    fn info_context_value_without_usage_shows_window_only() {
        assert_eq!(format_context_value(Some(128_000), None), "128k");
    }

    #[test]
    fn info_context_value_with_usage_shows_ratio_and_percent() {
        assert_eq!(
            format_context_value(Some(128_000), Some(32_000)),
            "32k / 128k (25%)"
        );
    }

    #[test]
    fn info_context_value_unknown_window_stays_unknown() {
        assert_eq!(format_context_value(None, Some(123)), "unknown");
    }

    #[test]
    fn info_line_renders_context_utilization_when_available() {
        let line = build_info_line(
            "copilot",
            "gpt-4o",
            Some("medium"),
            Some(128_000),
            Some(64_000),
            200,
        );
        let text = line_text(&line);
        assert!(text.contains("context 64k / 128k (50%)"), "{text}");
    }

    #[test]
    fn info_line_omits_thinking_when_unavailable() {
        let line = build_info_line("openai", "gpt-4o", None, Some(128_000), None, 200);
        let text = line_text(&line);
        assert!(!text.contains("thinking"), "{text}");
    }
    #[test]
    fn split_read_file_header_parses_and_returns_body() {
        let input = "[lines 10-20 of 300]\nalpha\nbeta";
        let parsed = split_read_file_header(input).expect("expected header parse");
        assert_eq!(parsed.0, 10);
        assert_eq!(parsed.1, 20);
        assert_eq!(parsed.2, 300);
        assert_eq!(parsed.3, "alpha\nbeta");
    }

    #[test]
    fn split_read_file_header_rejects_non_header() {
        assert!(split_read_file_header("alpha\nbeta").is_none());
    }

    #[test]
    fn sanitize_for_display_strips_trailing_whitespace_per_line() {
        assert_eq!(sanitize_for_display("hello   \nworld  "), "hello\nworld");
        assert_eq!(sanitize_for_display("  indented   "), "  indented");
    }

    #[test]
    fn sanitize_for_display_strips_leading_and_trailing_newlines() {
        assert_eq!(sanitize_for_display("\n\nhello\n\n"), "hello");
        // Leading spaces on the first line are preserved.
        assert_eq!(sanitize_for_display("\n\n  hello\n\n"), "  hello");
    }

    #[test]
    fn sanitize_for_display_preserves_up_to_two_consecutive_newlines() {
        assert_eq!(sanitize_for_display("a\nb"), "a\nb");
        assert_eq!(sanitize_for_display("a\n\nb"), "a\n\nb");
    }

    #[test]
    fn sanitize_for_display_collapses_three_or_more_newlines_to_two() {
        assert_eq!(sanitize_for_display("a\n\n\nb"), "a\n\nb");
        assert_eq!(sanitize_for_display("a\n\n\n\n\nb"), "a\n\nb");
        assert_eq!(sanitize_for_display("a\n\n\nb\n\n\n\nc"), "a\n\nb\n\nc");
    }

    #[test]
    fn sanitize_for_display_handles_multibyte_chars_without_panic() {
        // ─ is a 3-byte UTF-8 character; trailing-newline stripping must not
        // slice into the middle of it.
        assert_eq!(sanitize_for_display("─\n"), "─");
        assert_eq!(sanitize_for_display("\n─"), "─");
        assert_eq!(sanitize_for_display("hello ─\n"), "hello ─");
        assert_eq!(sanitize_for_display("a\n\n\n─ b"), "a\n\n─ b");
    }

    #[test]
    fn sanitize_for_display_trailing_whitespace_counts_as_blank_line() {
        // A line with only spaces between two newlines becomes an empty line;
        // three or more such separators still collapse to two newlines.
        assert_eq!(sanitize_for_display("a\n   \n\nb"), "a\n\nb");
        assert_eq!(sanitize_for_display("a\n \n \n \nb"), "a\n\nb");
    }

    #[test]
    fn tool_result_display_strips_leading_and_trailing_newlines_only() {
        let messages = vec![
            Message::tool_call("1", "bash", json!({"command": "echo hi"})),
            Message::tool_result("1", "\n\n  output line  \n\n", false),
        ];

        let lines = build_log_lines(&messages, false, &[], 80);
        // Leading/trailing newlines are stripped; leading spaces (indentation)
        // on the first content line are preserved.
        let result_lines: Vec<_> = lines
            .iter()
            .map(line_text)
            .filter(|l| l.starts_with('│'))
            .collect();
        assert_eq!(result_lines.len(), 1, "should be exactly one result line");
        assert!(
            result_lines[0].contains("  output line"),
            "indent should be preserved: {:?}",
            result_lines[0]
        );
    }

    #[test]
    fn tool_result_display_preserves_indentation_on_first_line() {
        let messages = vec![
            Message::tool_call("1", "bash", json!({"command": "cat f"})),
            Message::tool_result("1", "    indented output", false),
        ];

        let lines = build_log_lines(&messages, false, &[], 80);
        let result_lines: Vec<_> = lines
            .iter()
            .map(line_text)
            .filter(|l| l.starts_with('│'))
            .collect();
        assert!(!result_lines.is_empty());
        assert!(
            result_lines[0].contains("    indented output"),
            "indent stripped: {:?}",
            result_lines[0]
        );
    }

    #[test]
    fn tool_result_display_trims_trailing_newline() {
        let messages = vec![
            Message::tool_call("1", "bash", json!({"command": "uptime"})),
            Message::tool_result("1", "load: 1.0\n", false),
        ];

        let lines = build_log_lines(&messages, false, &[], 80);
        let rendered = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
        assert!(rendered.contains("│load: 1.0"), "{rendered}");
        // No extra blank line after the content.
        assert!(!rendered.contains("│load: 1.0\n│"), "{rendered}");
    }
}
