mod info;
mod input;
mod layout;
mod log;
mod login;
mod menu;
mod pending;
mod status;

use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState},
};
use unicode_width::UnicodeWidthStr;

use crate::{
    app::{App, InputMode, MAX_SELECTION_VISIBLE},
    provider::context_window_for_model,
};

use self::{
    info::build_info_line,
    input::{
        ASK_USER_INPUT_BG, INPUT_BG, SHELL_INPUT_BG, render_input_panel, split_scrollbar_column,
        style_textarea,
    },
    layout::{PanelInputs, compute_panel_heights, input_visual_line_count},
    log::build_log_lines,
    login::{LOGIN_HEADER_BG, build_login_content_lines},
    menu::{build_completion_lines, build_selection_lines},
};

/// Background colour of the selection menu header.
const SELECTION_HEADER_BG: Color = Color::Rgb(20, 45, 20);

fn halfblock_line(width: usize, ch: char, color: Color) -> Line<'static> {
    Line::from(Span::styled(
        ch.to_string().repeat(width),
        Style::default().fg(color),
    ))
}

fn build_log_lines_cached(app: &mut App, width: usize) -> &Vec<Line<'static>> {
    if !matches!(&app.cached_log_lines, Some((rev, w, _)) if *rev == app.log_revision && *w == width)
    {
        let combined = app.display_messages_combined();
        let lines = build_log_lines(&combined, app.streaming(), width);
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
        login_active: app.login.active,
        selection_mode: app.selection.active,
        selection_items_len: app.selection.items.len(),
        completions_len: app.completions.len(),
        resume_hint_visible,
        ask_user_selection_no_freeform: app.ask_user_selection_no_freeform(),
        login_url: app.login.url.as_deref(),
        has_login_code: app.login.code.is_some(),
        has_activity: app.throbber_visible(),
        has_provider_status: app.provider_status_visible(),
        queued_steering_len: app.queued_steering().len(),
    });

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(layout.activity_height),
            Constraint::Length(layout.pending_messages_height),
            Constraint::Length(layout.provider_status_height),
            Constraint::Length(layout.completion_height),
            Constraint::Length(layout.selection_header_height),
            Constraint::Length(layout.selection_items_height),
            Constraint::Length(layout.login_header_height),
            Constraint::Length(layout.login_content_height),
            Constraint::Length(layout.halfblock_height),
            Constraint::Length(layout.input_height),
            Constraint::Length(layout.halfblock_height),
            Constraint::Length(layout.info_height),
        ])
        .split(f.area());

    let log_area = chunks[0];
    let activity_area = chunks[1];
    let pending_messages_area = chunks[2];
    let provider_status_area = chunks[3];
    let completion_area = chunks[4];
    let sel_header_area = chunks[5];
    let sel_items_area = chunks[6];
    let login_hdr_area = chunks[7];
    let login_body_area = chunks[8];
    let top_hb_area = chunks[9];
    let input_area = chunks[10];
    let bot_hb_area = chunks[11];
    let info_area = chunks[12];

    let inner_height = log_area.height as usize;
    let (log_content_area, log_scrollbar_area) = split_scrollbar_column(log_area);
    let log_width = log_content_area.width as usize;
    app.last_log_height = inner_height;

    let total_lines = build_log_lines_cached(app, log_width).len();
    let max_scroll = total_lines.saturating_sub(inner_height);

    if app.auto_scroll {
        app.log_scroll = max_scroll;
    } else {
        app.log_scroll = app.log_scroll.min(max_scroll);
        if app.log_scroll >= max_scroll {
            app.auto_scroll = true;
        }
    }

    let has_scrollbar = total_lines > inner_height && !app.auto_scroll;
    let log_scroll = app.log_scroll;
    let visible_lines: Vec<Line<'static>> = {
        let all = build_log_lines_cached(app, log_width);
        if total_lines <= inner_height {
            let padding = inner_height - total_lines;
            let mut v: Vec<Line<'static>> = vec![Line::default(); padding];
            v.extend(all.iter().cloned());
            v
        } else {
            let start = log_scroll;
            let end = (start + inner_height).min(total_lines);
            all[start..end].to_vec()
        }
    };

    let log_paragraph =
        Paragraph::new(Text::from(visible_lines)).block(Block::default().borders(Borders::NONE));

    f.render_widget(Clear, log_area);
    f.render_widget(log_paragraph, log_content_area);

    if has_scrollbar && let Some(scrollbar_area) = log_scrollbar_area {
        let mut scrollbar_state = ScrollbarState::new(max_scroll + 1).position(app.log_scroll);
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight),
            scrollbar_area,
            &mut scrollbar_state,
        );
    }

    if layout.completion_height > 0 {
        if !app.completions.is_empty() {
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

    if app.selection.active {
        let hints = if app.in_provider_selection_mode() {
            if app.selection_filter_enabled() {
                "↑↓ navigate   Enter select   Ctrl+E edit provider   Ctrl+R remove provider   type filter   Esc cancel  "
            } else {
                "↑↓ navigate   Enter select   Ctrl+E edit provider   Ctrl+R remove provider   Esc cancel  "
            }
        } else if app.in_provider_removal_confirmation_mode() {
            "↑↓ navigate   Enter select   Esc cancel  "
        } else if app.selection_filter_enabled() {
            "↑↓ navigate   type filter   Enter select   Esc cancel  "
        } else {
            "↑↓ navigate   Enter select   Esc cancel  "
        };
        let title = app.selection.title;
        let query = if app.selection.query.is_empty() {
            "".to_string()
        } else {
            format!("filter: {}", app.selection.query)
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

        let selection_total = app.selection.items.len();
        let selection_scrollbar_needed = selection_total > MAX_SELECTION_VISIBLE;
        let (sel_content_area, sel_scrollbar_area) = if selection_scrollbar_needed {
            split_scrollbar_column(sel_items_area)
        } else {
            (sel_items_area, None)
        };

        let selection_lines = build_selection_lines(
            &app.selection.items,
            app.selection.selected,
            app.selection.scroll,
            sel_content_area.width as usize,
        );
        f.render_widget(Paragraph::new(selection_lines), sel_content_area);

        if selection_scrollbar_needed && let Some(scrollbar_area) = sel_scrollbar_area {
            let max_scroll = selection_total - MAX_SELECTION_VISIBLE;
            let mut scrollbar_state =
                ScrollbarState::new(max_scroll + 1).position(app.selection.scroll);
            f.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight),
                scrollbar_area,
                &mut scrollbar_state,
            );
        }
    }

    if app.login.active {
        const LOGIN_HINTS: &str = "Enter actions   Esc cancel  ";
        let provider = app.login.provider.as_deref().unwrap_or("provider");
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

        let content_lines = build_login_content_lines(app, width);
        f.render_widget(Paragraph::new(content_lines), login_body_area);
    }

    if !app.login.active && !app.ask_user_selection_no_freeform() {
        let panel_bg = if app.input_mode == InputMode::Shell {
            SHELL_INPUT_BG
        } else if app.ask_user_freeform_mode() {
            ASK_USER_INPUT_BG
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

    if layout.activity_height > 0 {
        status::render_activity(f, activity_area, app);
    }

    if layout.pending_messages_height > 0 {
        pending::render(f, pending_messages_area, app);
    }

    if layout.provider_status_height > 0 {
        status::render_provider_status(f, provider_status_area, app);
    }

    if !app.login.active && !app.ask_user_selection_no_freeform() {
        let is_shell = app.input_mode == InputMode::Shell;
        let panel_bg = if is_shell {
            SHELL_INPUT_BG
        } else if app.ask_user_freeform_mode() {
            ASK_USER_INPUT_BG
        } else {
            INPUT_BG
        };
        render_input_panel(f, input_area, app, panel_bg);
    }

    if app.show_info {
        let context_window = context_window_for_model(&app.current_model);
        let used_tokens = app.latest_usage.and_then(|u| u.used_tokens());
        let thinking = app
            .thinking_supported
            .then_some(app.current_thinking.as_str());
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::ui::{
        log::{USER_BG, append_tool_result_block},
        menu::{SELECTION_BG, SELECTION_SEL_BG},
    };
    use crate::{
        agent::AgentLoopConfig,
        auth::AuthFlow,
        commands::CompletionItem,
        llm::{AssistantPhase, Message},
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
            "copilot",
            ThinkingLevel::Medium,
            AgentLoopConfig {
                tools: HashMap::new(),
                file_tracker: std::sync::Arc::new(std::sync::Mutex::new(
                    crate::agent::FileTracker::new(),
                )),
                tool_output_log: std::sync::Arc::new(std::sync::Mutex::new(
                    crate::agent::ToolOutputLog::new("test"),
                )),
                session_events: vec![],
                current_model: "gpt-4o".to_string(),
                auto_compaction_enabled: true,
                manual_compaction_instructions: None,
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

    fn render_to_buffer(app: &mut App, width: u16, height: u16) -> ratatui::buffer::Buffer {
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).expect("test terminal");
        terminal.draw(|f| draw(f, app)).expect("draw succeeds");
        terminal.backend().buffer().clone()
    }

    fn buffer_to_plain_lines(
        buf: &ratatui::buffer::Buffer,
        width: u16,
        height: u16,
    ) -> Vec<String> {
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }
    #[test]
    fn input_wrap_prefers_word_boundaries() {
        let chunks = input::wrap_input_line("hello world from tau", 11);
        assert_eq!(
            chunks,
            vec!["hello world".to_string(), " from tau".to_string()]
        );
    }

    #[test]
    fn input_wrap_splits_long_tokens_at_viewport_boundary() {
        let chunks = input::wrap_input_line("small superlongtokenhere", 10);
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
            ask_user_selection_no_freeform: false,
            login_url: None,
            has_login_code: false,
            has_activity: false,
            has_provider_status: false,
            queued_steering_len: 0,
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
            ask_user_selection_no_freeform: false,
            login_url: None,
            has_login_code: false,
            has_activity: false,
            has_provider_status: false,
            queued_steering_len: 0,
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
            ask_user_selection_no_freeform: false,
            login_url: None,
            has_login_code: false,
            has_activity: false,
            has_provider_status: false,
            queued_steering_len: 0,
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
            ask_user_selection_no_freeform: false,
            login_url: None,
            has_login_code: false,
            has_activity: false,
            has_provider_status: false,
            queued_steering_len: 0,
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
            ask_user_selection_no_freeform: false,
            login_url: None,
            has_login_code: false,
            has_activity: false,
            has_provider_status: false,
            queued_steering_len: 0,
        });
        assert_eq!(heights.completion_height, 1);
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
            ask_user_selection_no_freeform: false,
            login_url: None,
            has_login_code: false,
            has_activity: false,
            has_provider_status: false,
            queued_steering_len: 0,
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
            ask_user_selection_no_freeform: false,
            login_url: None,
            has_login_code: false,
            has_activity: false,
            has_provider_status: false,
            queued_steering_len: 0,
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
            ask_user_selection_no_freeform: false,
            login_url: None,
            has_login_code: false,
            has_activity: false,
            has_provider_status: false,
            queued_steering_len: 0,
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
            ask_user_selection_no_freeform: false,
            login_url: None,
            has_login_code: false,
            has_activity: false,
            has_provider_status: false,
            queued_steering_len: 0,
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
            ask_user_selection_no_freeform: false,
            login_url: Some("https://example.com/very/long/url"),
            has_login_code: true,
            has_activity: false,
            has_provider_status: false,
            queued_steering_len: 0,
        });

        assert!(heights.input_height <= 1);
        assert_eq!(heights.selection_header_height, 1);
        assert_eq!(heights.selection_items_height, 1);
        assert!(heights.login_content_height >= 2);
    }

    #[test]
    fn draw_login_mode_renders_auth_header_and_hides_input_textarea() {
        let mut app = make_app();
        app.login.active = true;
        app.login.provider = Some("copilot".to_string());
        app.login.info = "Waiting for browser".to_string();

        app.textarea.insert_char('x');

        let lines = render_to_plain_lines(&mut app, 80, 20);
        let joined = lines.join("\n");
        assert!(joined.contains("Authenticating: copilot"), "{joined}");
        assert!(!joined.contains('x'), "{joined}");
    }

    #[test]
    fn draw_selection_mode_renders_title_and_visible_items() {
        let mut app = make_app();
        app.selection.active = true;
        app.selection.title = "  Pick item  ";
        app.selection.items = vec![
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
        app.login.auth_flow = Some(AuthFlow::DeviceCode);
        app.login.info = "Waiting".to_string();

        let lines = build_login_content_lines(&mut app, 80);
        let row0 = line_text(&lines[0]);
        assert!(row0.contains("enter the code shown"), "{row0}");
    }

    #[test]
    fn login_content_uses_redirect_flow_instruction() {
        let mut app = make_app();
        app.login.auth_flow = Some(AuthFlow::RedirectCallback);
        app.login.info = "Waiting".to_string();

        let lines = build_login_content_lines(&mut app, 80);
        let row0 = line_text(&lines[0]);
        assert!(row0.contains("redirect back automatically"), "{row0}");
    }

    #[test]
    fn login_content_wraps_url_for_narrow_width() {
        let mut app = make_app();
        app.login.info = "Waiting".to_string();
        app.login.url = Some("https://example.com/very/long/path/that/should/wrap".to_string());

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
        without_code.login.info = "Waiting".to_string();
        let lines_without = build_login_content_lines(&mut without_code, 80);
        assert!(!lines_without.iter().any(|l| line_text(l).contains("Code:")));

        let mut with_code = make_app();
        with_code.login.info = "Waiting".to_string();
        with_code.login.code = Some("ABCD-1234".to_string());
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
    fn selection_detail_column_is_vertically_aligned() {
        let items = vec![
            CompletionItem {
                label: "short".to_string(),
                detail: "Alpha".to_string(),
                complete_to: String::new(),
                loading: false,
                error: false,
                match_range: None,
            },
            CompletionItem {
                label: "a-much-longer-label".to_string(),
                detail: "Beta".to_string(),
                complete_to: String::new(),
                loading: false,
                error: false,
                match_range: None,
            },
        ];

        // Use selected = usize::MAX so neither row gets the ▶ cursor prefix,
        // avoiding multi-byte offset skew in the byte-position comparison.
        let lines = build_selection_lines(&items, usize::MAX, 0, 80);
        let first = line_text(&lines[0]);
        let second = line_text(&lines[1]);
        assert_eq!(first.find('—'), second.find('—'));
    }

    #[test]
    fn hidden_user_messages_are_not_rendered() {
        let mut hidden = Message::user("secret");
        hidden.hidden = true;
        let lines = log::build_log_lines(&[hidden, Message::assistant("shown")], false, 80);
        assert_eq!(lines.len(), 1);
        assert_eq!(line_text(&lines[0]), "💬 shown");
    }

    #[test]
    fn streaming_empty_assistant_message_shows_cursor() {
        let lines = log::build_log_lines(&[Message::assistant("")], true, 80);
        assert_eq!(line_text(&lines[0]), "💭 ▋");
    }

    #[test]
    fn stream_suffix_is_only_on_final_visible_chunk() {
        let lines =
            log::build_log_lines(&[Message::assistant("abcdefghijklmnopqrstuvwxyz")], true, 8);
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
        let lines = log::build_log_lines(&[Message::user("hi")], false, 10);
        assert_eq!(line_text(&lines[0]), "▄▄▄▄▄▄▄▄▄▄");
        assert_eq!(line_text(&lines[1]), "hi        ");
        assert_eq!(line_text(&lines[2]), "▀▀▀▀▀▀▀▀▀▀");
    }

    #[test]
    fn read_file_tool_call_annotates_range_from_next_result_display_range() {
        let messages = vec![
            Message::tool_call("1", "read_file", json!({"path": "src/main.rs"})),
            Message::tool_result("1", "alpha\nbeta", false).with_display_range(
                crate::llm::DisplayRange {
                    first_line: 10,
                    last_line: 20,
                    total_lines: 300,
                },
            ),
        ];

        let lines = log::build_log_lines(&messages, false, 120);
        assert!(line_text(&lines[0]).contains("[10-20/300]"));
    }

    #[test]
    fn read_file_result_display_shows_content_without_header() {
        let messages = vec![
            Message::tool_call("1", "read_file", json!({"path": "src/main.rs"})),
            Message::tool_result("1", "alpha\nbeta", false).with_display_range(
                crate::llm::DisplayRange {
                    first_line: 10,
                    last_line: 20,
                    total_lines: 300,
                },
            ),
        ];

        let lines = log::build_log_lines(&messages, false, 120);
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

        let lines = log::build_log_lines(&messages, false, 300);
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

        let lines = log::build_log_lines(&messages, false, 120);
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

        let lines = log::build_log_lines(&messages, false, 120);
        let rendered = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
        assert!(rendered.contains("💻 l1\nl2\nl3\nl4\nl5\n…"), "{rendered}");
        assert!(!rendered.contains("\n\n…"), "{rendered}");
        assert!(!rendered.contains("l6"), "{rendered}");
    }

    #[test]
    fn assistant_lines_are_prefixed_with_speech_bubble() {
        let messages = vec![Message::assistant("hello")];
        let lines = log::build_log_lines(&messages, false, 80);
        assert_eq!(line_text(&lines[0]), "💬 hello");
    }

    #[test]
    fn assistant_provisional_phase_uses_thought_bubble() {
        let mut msg = Message::assistant("working");
        msg.assistant_phase = Some(AssistantPhase::Provisional);
        let lines = log::build_log_lines(&[msg], false, 80);
        assert_eq!(line_text(&lines[0]), "💭 working");
    }

    #[test]
    fn assistant_unknown_phase_streaming_uses_thought_bubble() {
        let mut msg = Message::assistant("streaming");
        msg.assistant_phase = Some(AssistantPhase::Unknown);
        let lines = log::build_log_lines(&[msg], true, 80);
        assert_eq!(line_text(&lines[0]), "💭 streaming▋");
    }

    #[test]
    fn assistant_thinking_is_prefixed_with_brain() {
        let mut msg = Message::assistant("answer");
        msg.thinking = Some("planning".to_string());
        let messages = vec![msg];
        let lines = log::build_log_lines(&messages, false, 80);
        assert_eq!(line_text(&lines[0]), "🧠 planning");
        assert_eq!(line_text(&lines[2]), "💬 answer");
    }

    #[test]
    fn wrap_str_splits_at_width() {
        // "hello world" at width 5 should produce at least two chunks.
        let chunks = input::wrap_str("hello world", 5);
        assert!(
            chunks.len() >= 2,
            "expected at least 2 chunks, got: {:?}",
            chunks
        );
    }

    #[test]
    fn wrap_str_handles_empty_input() {
        let chunks = input::wrap_str("", 80);
        assert_eq!(chunks, vec![String::new()]);
    }

    #[test]
    fn wrap_str_handles_width_zero() {
        // width=0 is the degenerate case; the whole string is returned as-is.
        let chunks = input::wrap_str("some text", 0);
        assert_eq!(chunks, vec!["some text".to_string()]);
    }

    #[test]
    fn wrap_str_short_text_fits_in_one_chunk() {
        let chunks = input::wrap_str("hi", 80);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], "hi");
    }

    #[test]
    fn normalize_terminal_segment_expands_tabs_from_current_column() {
        assert_eq!(input::normalize_terminal_segment("\talpha", 0), "    alpha");
        assert_eq!(input::normalize_terminal_segment("\talpha", 1), "   alpha");
    }

    #[test]
    fn normalize_terminal_segment_replaces_control_chars_with_spaces() {
        assert_eq!(
            input::normalize_terminal_segment("a\rb\u{1b}[31m", 0),
            "a b [31m"
        );
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
        app.live_turn.notices.clear();
        app.live_turn.notices.extend(vec![Message::tool_result(
            "1",
            "tool output that used to be much longer",
            false,
        )]);
        app.mark_log_dirty();

        terminal
            .draw(|f| draw(f, &mut app))
            .expect("first draw succeeds");

        app.live_turn.notices.clear();
        app.live_turn
            .notices
            .extend(vec![Message::tool_result("1", "short", false)]);
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
        app.live_turn.notices.extend(vec![
            Message::user("one"),
            Message::user("two"),
            Message::user("three"),
            Message::user("four"),
        ]);
        // Scrolled up: scrollbar should be visible, reserving the rightmost column.
        app.auto_scroll = false;
        app.log_scroll = 0;

        terminal.draw(|f| draw(f, &mut app)).expect("draw succeeds");

        let buf = terminal.backend().buffer();
        let rightmost_x = 19;
        for y in 0..8 {
            assert_ne!(buf[(rightmost_x, y)].bg, USER_BG);
        }
    }

    #[test]
    fn scrollbar_hidden_when_at_bottom() {
        // When auto_scroll is true (pinned to bottom) the scrollbar must not
        // be rendered.  Verify by checking that no scrollbar glyph appears in
        // the rightmost column of the log rows (rows 0..last_log_height).
        let backend = ratatui::backend::TestBackend::new(20, 8);
        let mut terminal = ratatui::Terminal::new(backend).expect("test terminal");
        let mut app = make_app();
        app.live_turn.notices.extend(vec![
            Message::user("one"),
            Message::user("two"),
            Message::user("three"),
            Message::user("four"),
        ]);
        // Default: auto_scroll = true (pinned to bottom).
        assert!(app.auto_scroll);

        terminal.draw(|f| draw(f, &mut app)).expect("draw succeeds");

        // auto_scroll should still be true — content fits, no scrollbar needed.
        assert!(
            app.auto_scroll,
            "auto_scroll should remain true when content fits"
        );

        // Verify no scrollbar glyph in the last column of the log rows.
        let buf = terminal.backend().buffer().clone();
        let width = buf.area.width;
        let log_height = app.last_log_height;
        let scrollbar_col_has_glyph = (0..log_height as u16).any(|row| {
            let cell = buf.cell((width - 1, row)).unwrap();
            !cell.symbol().trim().is_empty()
        });
        assert!(
            !scrollbar_col_has_glyph,
            "scrollbar should be hidden at bottom — no glyph expected in log scrollbar column"
        );
    }

    #[test]
    fn selection_background_does_not_extend_into_scrollbar_column() {
        let backend = ratatui::backend::TestBackend::new(30, 20);
        let mut terminal = ratatui::Terminal::new(backend).expect("test terminal");
        let mut app = make_app();
        app.selection.active = true;
        app.selection.items = (0..30)
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
        assert_eq!(info::format_context_value(Some(128_000), None), "128k");
    }

    #[test]
    fn info_context_value_with_usage_shows_ratio_and_percent() {
        assert_eq!(
            info::format_context_value(Some(128_000), Some(32_000)),
            "32k / 128k (25%)"
        );
    }

    #[test]
    fn info_context_value_unknown_window_stays_unknown() {
        assert_eq!(info::format_context_value(None, Some(123)), "unknown");
    }

    #[test]
    fn info_line_renders_context_utilization_when_available() {
        let line = info::build_info_line(
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
        let line = info::build_info_line("openai", "gpt-4o", None, Some(128_000), None, 200);
        let text = line_text(&line);
        assert!(!text.contains("thinking"), "{text}");
    }
    #[test]
    fn sanitize_for_display_strips_trailing_whitespace_per_line() {
        assert_eq!(
            log::sanitize_for_display("hello   \nworld  "),
            "hello\nworld"
        );
        assert_eq!(log::sanitize_for_display("  indented   "), "  indented");
    }

    #[test]
    fn sanitize_for_display_strips_leading_and_trailing_newlines() {
        assert_eq!(log::sanitize_for_display("\n\nhello\n\n"), "hello");
        // Leading spaces on the first line are preserved.
        assert_eq!(log::sanitize_for_display("\n\n  hello\n\n"), "  hello");
    }

    #[test]
    fn sanitize_for_display_preserves_up_to_two_consecutive_newlines() {
        assert_eq!(log::sanitize_for_display("a\nb"), "a\nb");
        assert_eq!(log::sanitize_for_display("a\n\nb"), "a\n\nb");
    }

    #[test]
    fn sanitize_for_display_collapses_three_or_more_newlines_to_two() {
        assert_eq!(log::sanitize_for_display("a\n\n\nb"), "a\n\nb");
        assert_eq!(log::sanitize_for_display("a\n\n\n\n\nb"), "a\n\nb");
        assert_eq!(
            log::sanitize_for_display("a\n\n\nb\n\n\n\nc"),
            "a\n\nb\n\nc"
        );
    }

    #[test]
    fn sanitize_for_display_handles_multibyte_chars_without_panic() {
        // ─ is a 3-byte UTF-8 character; trailing-newline stripping must not
        // slice into the middle of it.
        assert_eq!(log::sanitize_for_display("─\n"), "─");
        assert_eq!(log::sanitize_for_display("\n─"), "─");
        assert_eq!(log::sanitize_for_display("hello ─\n"), "hello ─");
        assert_eq!(log::sanitize_for_display("a\n\n\n─ b"), "a\n\n─ b");
    }

    #[test]
    fn sanitize_for_display_trailing_whitespace_counts_as_blank_line() {
        // A line with only spaces between two newlines becomes an empty line;
        // three or more such separators still collapse to two newlines.
        assert_eq!(log::sanitize_for_display("a\n   \n\nb"), "a\n\nb");
        assert_eq!(log::sanitize_for_display("a\n \n \n \nb"), "a\n\nb");
    }

    #[test]
    fn tool_result_display_strips_leading_and_trailing_newlines_only() {
        let messages = vec![
            Message::tool_call("1", "bash", json!({"command": "echo hi"})),
            Message::tool_result("1", "\n\n  output line  \n\n", false),
        ];

        let lines = log::build_log_lines(&messages, false, 80);
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

        let lines = log::build_log_lines(&messages, false, 80);
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

        let lines = log::build_log_lines(&messages, false, 80);
        let rendered = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
        assert!(rendered.contains("│load: 1.0"), "{rendered}");
        // No extra blank line after the content.
        assert!(!rendered.contains("│load: 1.0\n│"), "{rendered}");
    }

    #[test]
    fn ask_user_freeform_typing_uses_ask_user_input_bg() {
        use crate::agent::types::{AskRequest, AskUserOption, AskUserResponse};
        let mut app = make_app();
        let (reply_tx, _reply_rx) = tokio::sync::oneshot::channel::<AskUserResponse>();
        app.receive_ask_request(AskRequest {
            question: "Choose?".to_string(),
            context: None,
            options: vec![AskUserOption {
                title: "Option A".to_string(),
                description: None,
            }],
            allow_multiple: false,
            allow_freeform: true,
            reply: reply_tx,
        });

        // Before typing: input should have INPUT_BG.
        let buf_before = render_to_buffer(&mut app, 40, 8);
        // After typing: input should have ASK_USER_INPUT_BG.
        app.begin_ask_freeform_typing();
        app.textarea.insert_char('x');
        let buf_after = render_to_buffer(&mut app, 40, 8);

        // Find the input row (row with 'x' in it).
        let input_row = (0..8u16)
            .find(|&y| {
                buf_after[(0, y)].symbol() == "x"
                    || (1..40u16).any(|x| buf_after[(x, y)].symbol() == "x")
            })
            .expect("should find input row");

        // The cell background on the input row should be ASK_USER_INPUT_BG.
        let bg_after = buf_after[(0, input_row)].bg;
        let bg_before = buf_before[(0, input_row)].bg;
        assert_eq!(
            bg_after,
            ratatui::style::Color::Rgb(50, 30, 15),
            "input bg after typing should be ASK_USER_INPUT_BG, got {bg_after:?}"
        );
        assert_ne!(
            bg_after, bg_before,
            "input bg should change when freeform typing begins (before={bg_before:?}, after={bg_after:?})"
        );
    }
}
