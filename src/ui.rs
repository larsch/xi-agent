use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState},
};
use unicode_width::UnicodeWidthStr;

use crate::{
    app::{App, MAX_SELECTION_VISIBLE},
    auth::AuthFlow,
    commands::CompletionItem,
    llm::{AssistantPhase, Role},
    provider::context_window_for_model,
    tool_presentation,
};

/// Background colour of the input panel.
const INPUT_BG: Color = Color::Rgb(30, 30, 40);

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
    // Both must carry INPUT_BG so the background is uniform.
    app.textarea.set_block(
        Block::default()
            .borders(Borders::NONE)
            .style(Style::default().bg(INPUT_BG)),
    );
    app.textarea
        .set_style(Style::default().fg(Color::White).bg(INPUT_BG));
    app.textarea
        .set_cursor_line_style(Style::default().bg(INPUT_BG));
}

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

pub fn draw(f: &mut ratatui::Frame, app: &mut App) {
    style_textarea(app);

    let terminal_height = f.area().height as usize;
    let width = f.area().width as usize;

    let input_line_count = app.textarea.lines().len().max(1);
    let max_input_height = (terminal_height * 40 / 100).max(1);
    let input_height = input_line_count.min(max_input_height) as u16;

    // Info bar: 1 row when show_info is active, 0 otherwise.
    let info_height: u16 = if app.show_info { 1 } else { 0 };

    // Completion popup: suppressed while login is active.
    let resume_hint_visible = app.should_show_resume_hint();
    let completion_height = if app.login_active || app.selection_mode {
        0
    } else if !app.completions.is_empty() {
        app.completions.len() as u16
    } else if resume_hint_visible {
        1
    } else {
        0
    };

    // Selection menu: shown whenever selection_mode is active, including on
    // top of the login panel (for the LoginAction menu).
    let selection_header_height: u16 = if app.selection_mode { 1 } else { 0 };
    let selection_items_height: u16 = if app.selection_mode {
        app.selection_items.len().clamp(1, MAX_SELECTION_VISIBLE) as u16
    } else {
        0
    };

    // ── Login panel dimensions ─────────────────────────────────────────────
    // When login is active the input box and its halfblock borders are hidden;
    // the login header + content rows take their place.
    let login_header_height: u16 = if app.login_active { 1 } else { 0 };
    let login_content_height: u16 = if app.login_active {
        // 1 instruction line + 1 status/info line
        let mut h = 2usize;
        if let Some(url) = &app.login_url {
            // URL label row + however many wrapped lines the URL needs.
            let url_indent = LOGIN_URL_INDENT.len();
            let wrap_width = width.saturating_sub(url_indent).max(1);
            let url_lines = wrap_str(url, wrap_width).len();
            h += 1 + url_lines; // "  URL:" header + wrapped content
        }
        if app.login_code.is_some() {
            h += 1;
        }
        h as u16
    } else {
        0
    };

    // Input area and its halfblock borders are suppressed during login.
    let effective_input_height = if app.login_active { 0 } else { input_height };
    let hb_height: u16 = if app.login_active { 0 } else { 1 };

    // Layout: chat log | completions | sel header | sel items
    //       | login header | login content
    //       | top halfblock | input | bottom halfblock | info bar
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),                          // 0: chat log
            Constraint::Length(completion_height),       // 1: completion popup
            Constraint::Length(selection_header_height), // 2: selection header
            Constraint::Length(selection_items_height),  // 3: selection items
            Constraint::Length(login_header_height),     // 4: login header
            Constraint::Length(login_content_height),    // 5: login content
            Constraint::Length(hb_height),               // 6: ▄ top edge
            Constraint::Length(effective_input_height),  // 7: input textarea
            Constraint::Length(hb_height),               // 8: ▀ bottom edge
            Constraint::Length(info_height),             // 9: info bar
        ])
        .split(f.area());

    let log_area = chunks[0];
    let completion_area = chunks[1];
    let sel_header_area = chunks[2];
    let sel_items_area = chunks[3];
    let login_hdr_area = chunks[4];
    let login_body_area = chunks[5];
    let top_hb_area = chunks[6];
    let input_area = chunks[7];
    let bot_hb_area = chunks[8];
    let info_area = chunks[9];

    // ── Chat log ──────────────────────────────────────────────────────────────
    let inner_height = log_area.height as usize;
    let pane_width = log_area.width as usize;

    // Pre-wrapped lines: each Line is exactly one visual row.
    let mut lines = build_log_lines(
        &app.messages,
        app.streaming,
        &app.queued_steering,
        pane_width,
    );

    // Store log height for use as page size in the event loop.
    app.last_log_height = inner_height;

    // Pad the top with empty lines so content is anchored to the bottom.
    if lines.len() < inner_height {
        let padding = inner_height - lines.len();
        let mut padded = vec![Line::default(); padding];
        padded.append(&mut lines);
        lines = padded;
    }

    let total_lines = lines.len();
    let max_scroll = total_lines.saturating_sub(inner_height);

    if app.auto_scroll {
        app.log_scroll = max_scroll;
    } else {
        app.log_scroll = app.log_scroll.min(max_scroll);
        if app.log_scroll >= max_scroll {
            app.auto_scroll = true;
        }
    }

    let log_paragraph = Paragraph::new(Text::from(lines))
        .block(Block::default().borders(Borders::NONE))
        .scroll((app.log_scroll as u16, 0));

    f.render_widget(log_paragraph, log_area);

    if total_lines > inner_height {
        let mut scrollbar_state = ScrollbarState::new(max_scroll + 1).position(app.log_scroll);
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight),
            log_area,
            &mut scrollbar_state,
        );
    }

    // ── Completion popup / resume hint ───────────────────────────────────────
    if completion_height > 0 {
        if !app.completions.is_empty() {
            let popup_lines =
                build_completion_lines(&app.completions, app.completion_selected, width);
            f.render_widget(Paragraph::new(popup_lines), completion_area);
        } else if resume_hint_visible {
            let hint = Line::from(vec![
                Span::styled("  hint: ", Style::default().fg(Color::DarkGray)),
                Span::styled("Ctrl+R", Style::default().fg(Color::Yellow)),
                Span::styled(
                    " resumes the latest session for this folder • /resume opens session picker",
                    Style::default().fg(Color::DarkGray),
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
                Style::default().fg(Color::DarkGray).bg(SELECTION_HEADER_BG),
            ),
        ]);
        f.render_widget(Paragraph::new(vec![header_line]), sel_header_area);

        // Item rows.
        if selection_items_height > 0 {
            let item_lines = if app.selection_items.is_empty() {
                vec![Line::from(vec![
                    Span::styled("  ", Style::default().bg(SELECTION_BG)),
                    Span::styled(
                        "no matches",
                        Style::default()
                            .fg(Color::DarkGray)
                            .bg(SELECTION_BG)
                            .add_modifier(ratatui::style::Modifier::ITALIC),
                    ),
                ])]
            } else {
                build_selection_lines(
                    &app.selection_items,
                    app.selection_selected,
                    app.selection_scroll,
                    width,
                )
            };
            f.render_widget(Paragraph::new(item_lines), sel_items_area);

            // Scrollbar when the list is longer than the visible window.
            let total = app.selection_items.len();
            if total > MAX_SELECTION_VISIBLE {
                let max_scroll = total - MAX_SELECTION_VISIBLE;
                let mut sb_state =
                    ScrollbarState::new(max_scroll + 1).position(app.selection_scroll);
                f.render_stateful_widget(
                    Scrollbar::new(ScrollbarOrientation::VerticalRight),
                    sel_items_area,
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
                Style::default().fg(Color::DarkGray).bg(LOGIN_HEADER_BG),
            ),
        ]);
        f.render_widget(Paragraph::new(vec![header_line]), login_hdr_area);

        // Content rows.
        let content_lines = build_login_content_lines(app, width);
        f.render_widget(Paragraph::new(content_lines), login_body_area);
    }

    // ── Halfblock edges ───────────────────────────────────────────────────────
    if !app.login_active {
        f.render_widget(
            Paragraph::new(halfblock_line(width, '▄', INPUT_BG)),
            top_hb_area,
        );
        f.render_widget(
            Paragraph::new(halfblock_line(width, '▀', INPUT_BG)),
            bot_hb_area,
        );
    }

    // ── Input box ─────────────────────────────────────────────────────────────
    if !app.login_active {
        f.render_widget(&app.textarea, input_area);
    }

    // ── Info bar ──────────────────────────────────────────────────────────────
    if app.show_info {
        let context_window = context_window_for_model(&app.current_model);
        let used_tokens = app.latest_usage.and_then(|u| u.used_tokens());
        let info_line = build_info_line(
            &app.current_provider,
            &app.current_model,
            app.current_thinking.as_str(),
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
                let fg = if item.error {
                    Color::Red
                } else {
                    Color::DarkGray
                };
                return Line::from(vec![
                    Span::styled(INDENT, Style::default().bg(bg)),
                    Span::styled(
                        item.label.clone(),
                        Style::default()
                            .fg(fg)
                            .bg(bg)
                            .add_modifier(ratatui::style::Modifier::ITALIC),
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

            if item.detail.is_empty() {
                Line::from(vec![
                    Span::styled(INDENT, Style::default().bg(bg)),
                    Span::styled(label_padded, Style::default().fg(COMPLETION_CMD_FG).bg(bg)),
                    Span::styled(fill, Style::default().bg(bg)),
                ])
            } else {
                Line::from(vec![
                    Span::styled(INDENT, Style::default().bg(bg)),
                    Span::styled(label_padded, Style::default().fg(COMPLETION_CMD_FG).bg(bg)),
                    Span::styled(SEP, Style::default().fg(Color::DarkGray).bg(bg)),
                    Span::styled(
                        item.detail.clone(),
                        Style::default().fg(COMPLETION_DESC_FG).bg(bg),
                    ),
                    Span::styled(fill, Style::default().bg(bg)),
                ])
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
                let fg = if item.error {
                    Color::Red
                } else {
                    Color::DarkGray
                };
                return Line::from(vec![
                    Span::styled(INDENT, Style::default().bg(bg)),
                    Span::styled(
                        item.label.clone(),
                        Style::default()
                            .fg(fg)
                            .bg(bg)
                            .add_modifier(ratatui::style::Modifier::ITALIC),
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
                append_message(&mut lines, &msg.content, "", width, true);
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
                    append_message_dim(&mut lines, &format!("🧠 {thinking}"), "", width);
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
                        format!("{answer_icon} {}", msg.content)
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
                let mut label = match msg.tool_args.as_ref() {
                    Some(args) => tool_presentation::tool_invocation_label(name, args),
                    None => {
                        tool_presentation::tool_invocation_label(name, &serde_json::Value::Null)
                    }
                };

                if matches!(name, "read" | "read_file")
                    && let Some(next) = messages.get(idx + 1)
                    && next.role == Role::ToolResult
                    && let Some((start, end, total, _)) = split_read_file_header(&next.content)
                {
                    label.push_str(&format!(" [{start}-{end}/{total}]"));
                }

                append_message_colored(&mut lines, &label, width, Color::Cyan);
            }
            Role::ToolResult => {
                let prev_is_read = matches!(
                    messages.get(idx.saturating_sub(1)),
                    Some(prev)
                        if prev.role == Role::ToolCall
                            && matches!(prev.tool_name.as_deref(), Some("read" | "read_file"))
                );

                let content_for_display = if prev_is_read {
                    split_read_file_header(&msg.content)
                        .map(|(_, _, _, body)| body.to_string())
                        .unwrap_or_else(|| msg.content.clone())
                } else {
                    msg.content.clone()
                };

                let preview: String = content_for_display.chars().take(200).collect();
                let truncated = content_for_display.len() > 200;
                let display = if truncated {
                    format!("{preview}…")
                } else {
                    preview
                };
                let color = if msg.is_error {
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

/// Append pre-wrapped colored lines for a single-line tool label.
/// Wraps if the label is wider than `width`, renders in the given `color`.
fn append_message_colored(out: &mut Vec<Line<'static>>, content: &str, width: usize, color: Color) {
    let style = Style::default().fg(color);
    let chunks = wrap_str(content, width);
    for chunk in chunks {
        out.push(Line::from(vec![Span::styled(chunk, style)]));
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

    if width == 0 {
        out.push(Line::from(vec![Span::styled(
            "│".to_string(),
            marker_style,
        )]));
        return;
    }

    let content_width = width.saturating_sub(1).max(1);
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
        let segment = segments[seg_idx];
        let chunks = wrap_str(segment, content_width);
        for chunk in chunks {
            out.push(Line::from(vec![
                Span::styled("│", marker_style),
                Span::styled(chunk, text_style),
            ]));
        }
    }
}

/// Append pre-wrapped dim (thinking) lines for one block.
/// Same wrapping logic as `append_message` but renders in `DarkGray`.
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
        let chunks = wrap_str(segment, width);
        let last_chunk = chunks.len() - 1;

        for (chunk_idx, chunk) in chunks.iter().enumerate() {
            let is_last_chunk = chunk_idx == last_chunk;
            let show_suffix = !suffix.is_empty() && is_last_visible_seg && is_last_chunk;

            let mut spans: Vec<Span<'static>> = vec![Span::styled(chunk.clone(), dim_style)];
            if show_suffix {
                spans.push(Span::styled(suffix, Style::default().fg(Color::DarkGray)));
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
        let chunks = wrap_str(segment, width);
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

// ── Info bar ──────────────────────────────────────────────────────────────────

/// Background colour for the info bar.
const INFO_BG: Color = Color::Rgb(20, 20, 30);

/// Build the single info-bar `Line` showing provider / model / context window.
fn build_info_line<'a>(
    provider: &str,
    model: &str,
    thinking: &str,
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
    let content_spans: Vec<Span<'a>> = vec![
        Span::styled(" ", fill_style),
        Span::styled("provider", key_style),
        Span::styled(" ", fill_style),
        Span::styled(provider.to_string(), val_style),
        Span::styled("  │  ", sep_style),
        Span::styled("model", key_style),
        Span::styled(" ", fill_style),
        Span::styled(model.to_string(), val_style),
        Span::styled("  │  ", sep_style),
        Span::styled("thinking", key_style),
        Span::styled(" ", fill_style),
        Span::styled(thinking.to_string(), val_style),
        Span::styled("  │  ", sep_style),
        Span::styled("context", key_style),
        Span::styled(" ", fill_style),
        Span::styled(context_value.clone(), val_style),
    ];

    // Calculate used columns (approximate; ASCII only for labels).
    let used: usize = 1 // leading space
        + "provider".len() + 1 + provider.len()
        + 5 // sep
        + "model".len() + 1 + model.len()
        + 5 // sep
        + "thinking".len() + 1 + thinking.len()
        + 5 // sep
        + "context".len() + 1 + context_value.len();

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
    use super::*;
    use crate::llm::{AssistantPhase, Message};

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
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
            "medium",
            Some(128_000),
            Some(64_000),
            200,
        );
        let text = line_text(&line);
        assert!(text.contains("context 64k / 128k (50%)"), "{text}");
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
}
