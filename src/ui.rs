mod info;
mod input;
mod layout;
pub(crate) mod log;
mod login;
mod menu;
mod pending;
mod status;

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState},
};
use unicode_width::UnicodeWidthStr;

use crate::{
    agent_turn_state::VisualUpdate,
    app::{App, InputMode},
    config::DisplayConfig,
    context_window::context_window_for_model,
    log_view_state::PaddingState,
    mouse_select::LineSource,
    selection_state::{MAX_SELECTION_VISIBLE, SelectionKind},
};

use self::{
    info::build_info_line,
    input::{render_input_panel, split_scrollbar_column, style_textarea},
    layout::{PanelInputs, compute_panel_heights, input_visual_line_count},
    log::{ToolBodyConfig, build_log_layout_with_expansion},
    login::build_login_content_lines,
    menu::{build_completion_lines, build_selection_lines},
};

fn halfblock_line(width: usize, ch: char, color: Color) -> Line<'static> {
    Line::from(Span::styled(
        ch.to_string().repeat(width),
        Style::default().fg(color),
    ))
}

/// Sentinel `LineSource` for padding rows that carry no block metadata.
fn empty_line_source() -> LineSource {
    LineSource {
        decoration_width: 0,
        streaming: false,
        block_identity: None,
        foldable: false,
    }
}

fn split_panel(area: Rect, heights: layout::PanelHeights) -> Vec<Rect> {
    Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(heights.activity_height),
            Constraint::Length(heights.pending_messages_height),
            Constraint::Length(heights.provider_status_height),
            Constraint::Length(heights.completion_height),
            Constraint::Length(heights.selection_header_height),
            Constraint::Length(heights.selection_items_height),
            Constraint::Length(heights.login_header_height),
            Constraint::Length(heights.login_content_height),
            Constraint::Length(heights.halfblock_height),
            Constraint::Length(heights.input_height),
            Constraint::Length(heights.halfblock_height),
            Constraint::Length(heights.info_height),
        ])
        .split(area)
        .to_vec()
}

fn build_log_layout_cached(
    app: &mut App,
    width: usize,
    inner_height: usize,
    display: &DisplayConfig,
) -> usize {
    // Flush any pending session-mutation dirty flag into the log cache.
    if app.session.take_dirty() {
        app.log_view.invalidate();
    }
    let step_cursor = app.step_back.cursor;
    let streaming = app.streaming();
    if !matches!(&app.log_view.log_cache.cached_layout, Some((rev, w, sc, _))
        if *rev == app.log_view.log_cache.revision && *w == width && *sc == step_cursor)
    {
        let cfg = ToolBodyConfig {
            full_output: app.log_view.full_output,
            ..ToolBodyConfig::default()
        };
        let mut layout = if let Some((kept, discarded)) = app.display_messages_split() {
            let mut layout = build_log_layout_with_expansion(
                &[],
                &kept,
                0,
                false,
                width,
                &cfg,
                &app.theme,
                display,
                &app.log_view.expanded_blocks,
                &mut app.log_view.block_cache,
            );
            let mut discarded_layout = build_log_layout_with_expansion(
                &[],
                &discarded,
                0,
                false,
                width,
                &cfg,
                &app.theme,
                display,
                &app.log_view.expanded_blocks,
                &mut app.log_view.block_cache,
            );
            discarded_layout.dim();
            layout.blocks.extend(discarded_layout.blocks);
            layout
        } else {
            let committed = app
                .session
                .session_state
                .as_ref()
                .map(|s| s.display_messages())
                .unwrap_or(&[]);
            let overlay = app.session.live_turn.render_overlay(streaming);
            let committed_generation = app
                .session
                .session_state
                .as_ref()
                .map_or(0, |s| s.display_generation());
            build_log_layout_with_expansion(
                committed,
                &overlay,
                committed_generation,
                streaming,
                width,
                &cfg,
                &app.theme,
                display,
                &app.log_view.expanded_blocks,
                &mut app.log_view.block_cache,
            )
        };
        let previous = if app.log_view.visual_baseline_width == Some(width) {
            app.log_view.take_visual_baseline()
        } else {
            app.log_view.visual_baseline_width = Some(width);
            None
        };
        if let Some(previous) = previous.as_deref()
            && streaming
        {
            layout.pad_shrink(previous);
        }
        let baseline: Vec<(String, usize)> = layout
            .blocks
            .iter()
            .map(|block| (block.identity.clone(), block.lines.len()))
            .collect();
        let update = if previous.is_none() {
            VisualUpdate::NonContentLayoutChange
        } else {
            layout.visual_update(previous.as_deref())
        };
        let baseline_blocks = baseline.len();
        let previous_blocks = previous.as_ref().map_or(0, Vec::len);
        ::log::debug!(
            target: "throbber.trace",
            "layout update: revision={} width={} step_cursor={step_cursor:?} streaming={streaming} previous_blocks={previous_blocks} new_blocks={baseline_blocks} update={update:?}",
            app.log_view.log_cache.revision,
            width,
        );
        let anchor_padding = app
            .log_view
            .last_block_padding
            .as_ref()
            .map_or(0, |padding| padding.remaining.max(0) as usize);
        app.log_view.set_visual_baseline(baseline);
        app.agent_turn.update_visual_state_with_padding(
            Some(update),
            anchor_padding,
            std::time::Instant::now(),
        );
        if let VisualUpdate::Delta(delta) = update
            && let Some(padding) = &mut app.log_view.last_block_padding
        {
            padding.remaining -= delta;
            if padding.remaining < 0 {
                padding.remaining = 0;
            }
        }
        let content_len = layout.total_lines();

        // The initial total is the anchor. Subsequent positive visual deltas
        // consume this padding; do not raise the anchor after each update.
        if streaming && app.log_view.last_block_padding.is_none() {
            app.log_view.last_block_padding = Some(PaddingState {
                max_total_lines: content_len,
                inner_height_when_set: inner_height,
                remaining: 0,
            });
        }

        let rev = app.log_view.log_cache.revision;
        app.log_view.log_cache.cached_layout = Some((rev, width, step_cursor, layout));
    } else {
        app.agent_turn
            .update_visual_state(None, std::time::Instant::now());
    }
    app.log_view
        .log_cache
        .cached_layout
        .as_ref()
        .unwrap()
        .3
        .total_lines()
}

pub fn draw(f: &mut ratatui::Frame, app: &mut App) {
    style_textarea(app);

    let terminal_height = f.area().height as usize;
    let width = f.area().width as usize;
    let resume_hint_visible = app.should_show_resume_hint();

    let active_lines = if app.input_mode == InputMode::Shell {
        app.shell.textarea.lines()
    } else {
        app.textarea.lines()
    };

    let input_line_count = input_visual_line_count(active_lines, width);
    let display = app.display.clone();

    let ask_user_header_lines = if app.selection.active { 1 } else { 0 };

    ::log::debug!(
        target: "throbber.trace",
        "panel decision: active={} visible={} full_output={} activity_row_before={} width={} terminal_height={terminal_height}",
        app.streaming(),
        app.throbber_visible(),
        app.log_view.full_output,
        app.log_view.full_output,
        width,
    );
    let mut layout = compute_panel_heights(PanelInputs {
        terminal_height,
        width,
        input_line_count,
        show_info: app.show_info,
        login_active: app.login.active,
        selection_mode: app.selection.active,
        selection_items_len: app.selection.items.len(),
        completions_len: app.completion.completions.len(),
        resume_hint_visible,
        ask_user_selection_no_freeform: app.ask_user_selection_no_freeform(),
        ask_user_header_lines,
        login_url: app.login.url.as_deref(),
        has_login_code: app.login.code.is_some(),
        has_activity: app.log_view.full_output,
        has_provider_status: app.provider_status_visible(),
        queued_steering_len: app.queued_steering().len(),
    });

    let mut chunks = split_panel(f.area(), layout);

    let mut log_area = chunks[0];
    let mut activity_area = chunks[1];
    let mut pending_messages_area = chunks[2];
    let mut provider_status_area = chunks[3];
    let mut completion_area = chunks[4];
    let mut sel_header_area = chunks[5];
    let mut sel_items_area = chunks[6];
    let mut login_hdr_area = chunks[7];
    let mut login_body_area = chunks[8];
    let mut top_hb_area = chunks[9];
    let mut input_area = chunks[10];
    let mut bot_hb_area = chunks[11];
    let mut info_area = chunks[12];

    let mut inner_height = log_area.height as usize;
    let (mut log_content_area, mut log_scrollbar_area) = split_scrollbar_column(log_area);
    let mut log_width = log_content_area.width as usize;
    let previous_log_height = app.log_view.last_log_height;
    app.log_view.last_log_height = inner_height;

    // Reset block padding on terminal resize.
    if app.log_view.last_log_width != 0 && app.log_view.last_log_width != log_width {
        app.log_view.clear_padding();
    }
    app.log_view.last_log_width = log_width;

    ::log::debug!(
        target: "throbber.trace",
        "viewport: activity_height={} log_height={} log_width={} previous_log_height={} auto_scroll={} scroll={} padding={:?}",
        layout.activity_height,
        inner_height,
        log_width,
        previous_log_height,
        app.log_view.auto_scroll,
        app.log_view.log_scroll,
        app.log_view.last_block_padding.as_ref().map(|p| (p.max_total_lines, p.inner_height_when_set)),
    );

    let content_len = build_log_layout_cached(app, log_width, inner_height, &display);

    // The logical layout update above may change activity-row visibility. Recompute
    // panel geometry before rendering so the same frame uses the new row height;
    // otherwise the log is drawn once with stale geometry and visibly jumps on
    // the following frame.
    let final_has_activity = app.log_view.full_output;
    let final_layout = compute_panel_heights(PanelInputs {
        terminal_height,
        width,
        input_line_count,
        show_info: app.show_info,
        login_active: app.login.active,
        selection_mode: app.selection.active,
        selection_items_len: app.selection.items.len(),
        completions_len: app.completion.completions.len(),
        resume_hint_visible,
        ask_user_selection_no_freeform: app.ask_user_selection_no_freeform(),
        ask_user_header_lines,
        login_url: app.login.url.as_deref(),
        has_login_code: app.login.code.is_some(),
        has_activity: final_has_activity,
        has_provider_status: app.provider_status_visible(),
        queued_steering_len: app.queued_steering().len(),
    });
    if final_layout != layout {
        layout = final_layout;
        chunks = split_panel(f.area(), layout);
        log_area = chunks[0];
        activity_area = chunks[1];
        pending_messages_area = chunks[2];
        provider_status_area = chunks[3];
        completion_area = chunks[4];
        sel_header_area = chunks[5];
        sel_items_area = chunks[6];
        login_hdr_area = chunks[7];
        login_body_area = chunks[8];
        top_hb_area = chunks[9];
        input_area = chunks[10];
        bot_hb_area = chunks[11];
        info_area = chunks[12];
        inner_height = log_area.height as usize;
        (log_content_area, log_scrollbar_area) = split_scrollbar_column(log_area);
        log_width = log_content_area.width as usize;
        app.log_view.last_log_height = inner_height;
    }

    // The throbber is rendered as a virtual line appended after the last
    // content line, so it scrolls out of view with the rest of the log.
    let throbber_visible = app.throbber_visible();
    let throbber = throbber_visible.then(|| status::throbber_line(app));
    let total_lines = content_len + usize::from(throbber_visible);
    if let Some((identity, block_screen_top)) = app.log_view.pending_anchor.take()
        && !app.log_view.auto_scroll
        && let Some(new_top) = app
            .log_view
            .log_cache
            .cached_layout
            .as_ref()
            .and_then(|(_, _, _, layout)| layout.block_start_line(&identity))
    {
        app.log_view.log_scroll = new_top.saturating_sub(block_screen_top);
    }
    let max_scroll = total_lines.saturating_sub(inner_height);

    if app.log_view.auto_scroll {
        app.log_view.log_scroll = max_scroll;
    } else if app.step_back.cursor.is_some() && app.log_view.log_scroll == usize::MAX {
        // Centre the step-cursor boundary in the viewport.
        let kept_count = app
            .display_messages_split()
            .map(|(kept, _)| {
                let cfg = ToolBodyConfig {
                    full_output: app.log_view.full_output,
                    ..ToolBodyConfig::default()
                };
                build_log_layout_with_expansion(
                    &[],
                    &kept,
                    0,
                    false,
                    log_width,
                    &cfg,
                    &app.theme,
                    &display,
                    &app.log_view.expanded_blocks,
                    &mut app.log_view.block_cache,
                )
                .total_lines()
            })
            .unwrap_or(0);
        let half_height = inner_height / 2;
        app.log_view.log_scroll = kept_count.saturating_sub(half_height).min(max_scroll);
    } else {
        app.log_view.log_scroll = app.log_view.log_scroll.min(max_scroll);
        if app.log_view.log_scroll >= max_scroll {
            app.log_view.auto_scroll = true;
        }
    }

    let has_scrollbar = total_lines > inner_height && !app.log_view.auto_scroll;
    let log_scroll = app.log_view.log_scroll;

    // Clear block padding when streaming is no longer active.
    if !app.streaming() {
        app.log_view.clear_padding();
    }

    let stored_height = app
        .log_view
        .last_block_padding
        .as_ref()
        .map(|ps| ps.inner_height_when_set)
        .unwrap_or(inner_height);

    // When the log area shrinks (e.g. throbber appears), the bottom
    // padding absorbs it so the content position stays stable.
    let height_decrease = stored_height.saturating_sub(inner_height);

    let max_total = app
        .log_view
        .last_block_padding
        .as_ref()
        .map(|ps| ps.max_total_lines)
        .unwrap_or(0);

    let block_padding = if app.log_view.auto_scroll {
        app.log_view
            .last_block_padding
            .as_ref()
            .map_or(0, |padding| padding.remaining.max(0) as usize)
    } else {
        0
    };

    ::log::debug!(
        target: "throbber.trace",
        "render geometry: total_lines={} inner_height={} stored_height={} max_total={} height_decrease={} block_padding={} throbber_visible={}",
        total_lines,
        inner_height,
        stored_height,
        max_total,
        height_decrease,
        block_padding,
        app.throbber_visible(),
    );

    let (visible_lines, visible_sources): (Vec<Line<'static>>, Vec<LineSource>) = {
        let layout = &app.log_view.log_cache.cached_layout.as_ref().unwrap().3;
        if block_padding > 0 {
            // Anchor against the stored height so content stays put when the
            // log area resizes.  Bottom padding absorbs any shrinkage.
            let anchor_top = max_total.saturating_sub(stored_height);
            let raw_start = anchor_top.min(total_lines);
            let raw_end = total_lines;
            let raw_lines = raw_end.saturating_sub(raw_start);
            let bottom_padding = block_padding.saturating_sub(height_decrease);
            let top_padding = inner_height.saturating_sub(raw_lines + bottom_padding);

            let mut lines: Vec<Line<'static>> = vec![Line::default(); top_padding];
            let mut sources: Vec<LineSource> = vec![empty_line_source(); top_padding];
            let (wl, ws) = layout.visible_window(raw_start, raw_end);
            lines.extend(wl);
            sources.extend(ws);
            lines.extend(std::iter::repeat_n(Line::default(), bottom_padding));
            sources.extend(std::iter::repeat_n(empty_line_source(), bottom_padding));
            (lines, sources)
        } else if total_lines <= inner_height {
            let padding = inner_height - total_lines;
            let mut lines: Vec<Line<'static>> = vec![Line::default(); padding];
            let mut sources: Vec<LineSource> = vec![empty_line_source(); padding];
            let (wl, ws) = layout.visible_window(0, content_len);
            lines.extend(wl);
            sources.extend(ws);
            if let Some(throbber) = &throbber {
                lines.push(throbber.clone());
                sources.push(empty_line_source());
            }
            (lines, sources)
        } else {
            let start = log_scroll;
            let end = (start + inner_height).min(total_lines);
            let content_end = end.min(content_len);
            let (mut lines, mut sources) = layout.visible_window(start, content_end);
            // The throbber occupies the virtual line at index `content_len`,
            // only included when the visible window reaches the bottom.
            if let Some(throbber) = &throbber
                && content_len >= start
                && content_len < end
            {
                lines.push(throbber.clone());
                sources.push(empty_line_source());
            }
            (lines, sources)
        }
    };

    // ── Mouse selection highlight pass ────────────────────────────────────────
    // Store visible lines on the mouse state for text extraction.
    app.mouse_select.visible_lines = visible_lines.clone();
    app.mouse_select.hit_map = visible_sources;
    app.mouse_select.log_area_top = 0;
    app.mouse_select.log_area_width = log_width as u16;
    app.mouse_select.log_scroll = log_scroll;
    let visible_lines = apply_mouse_highlight(visible_lines, app);

    let hover_source = app
        .mouse_select
        .hover_row
        .saturating_sub(app.mouse_select.log_area_top) as usize;
    let hovered_identity = app
        .mouse_select
        .hit_map
        .get(hover_source)
        .filter(|source| source.foldable && !source.streaming)
        .and_then(|source| source.block_identity.clone());
    let chevron_row = hovered_identity.as_ref().and_then(|identity| {
        let start = app
            .log_view
            .log_cache
            .cached_layout
            .as_ref()
            .and_then(|(_, _, _, layout)| layout.block_start_line(identity))?;
        (start >= log_scroll && start < log_scroll.saturating_add(inner_height))
            .then_some(start.saturating_sub(log_scroll))
    });

    let log_paragraph =
        Paragraph::new(Text::from(visible_lines)).block(Block::default().borders(Borders::NONE));

    f.render_widget(Clear, log_area);
    f.render_widget(log_paragraph, log_content_area);
    if let (Some(row), Some(identity)) = (chevron_row, hovered_identity) {
        let glyph = if app.log_view.expanded_blocks.contains(&identity) {
            "⌃"
        } else {
            "⌄"
        };
        let chevron_area = ratatui::layout::Rect {
            x: log_content_area
                .x
                .saturating_add(log_content_area.width.saturating_sub(1)),
            y: log_content_area.y.saturating_add(row as u16),
            width: 1,
            height: 1,
        };
        f.render_widget(
            Paragraph::new(glyph).style(Style::default().fg(Color::DarkGray)),
            chevron_area,
        );
    }

    if has_scrollbar && let Some(scrollbar_area) = log_scrollbar_area {
        let mut scrollbar_state =
            ScrollbarState::new(max_scroll + 1).position(app.log_view.log_scroll);
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight),
            scrollbar_area,
            &mut scrollbar_state,
        );
    }

    if layout.completion_height > 0 {
        if !app.completion.completions.is_empty() {
            let popup_lines = build_completion_lines(
                &app.theme.menu,
                &app.completion.completions,
                app.completion.completion_selected,
                width,
            );
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
        let header_bg = app
            .theme
            .menu
            .selection
            .header
            .bg
            .unwrap_or(Color::Rgb(20, 45, 20));

        let header_lines: Vec<Line<'static>> = {
            // ── Selection header ──────────────────────────────────────────────
            let hints = if app.selection.kind == Some(SelectionKind::AskUser) {
                if app.ask_user_selection_no_freeform() {
                    "↑↓ navigate   Enter select  "
                } else {
                    "↑↓ navigate   Enter select   Esc cancel  "
                }
            } else if app.selection.kind == Some(SelectionKind::KeybindingHelp) {
                "↑↓ navigate   PageUp/PageDown scroll   Esc close  "
            } else if app.in_provider_selection_mode() {
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
            let gap = width.saturating_sub(title.width() + hints.width());
            let header_line = Line::from(vec![
                Span::styled(
                    title,
                    Style::default()
                        .fg(Color::White)
                        .bg(header_bg)
                        .add_modifier(ratatui::style::Modifier::BOLD),
                ),
                Span::styled(" ".repeat(gap), Style::default().bg(header_bg)),
                Span::styled(
                    hints.to_string(),
                    Style::default()
                        .bg(header_bg)
                        .add_modifier(ratatui::style::Modifier::DIM),
                ),
            ]);
            vec![header_line]
        };

        f.render_widget(Paragraph::new(header_lines), sel_header_area);

        let selection_total = app.selection.items.len();
        let selection_scrollbar_needed = selection_total > MAX_SELECTION_VISIBLE;
        let (sel_content_area, sel_scrollbar_area) = if selection_scrollbar_needed {
            split_scrollbar_column(sel_items_area)
        } else {
            (sel_items_area, None)
        };

        let selection_lines = build_selection_lines(
            &app.theme.menu.selection,
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
        let header_bg = app
            .theme
            .login
            .header
            .bg
            .unwrap_or(ratatui::style::Color::Rgb(20, 30, 60));
        let header_line = Line::from(vec![
            Span::styled(
                title,
                Style::default()
                    .fg(Color::White)
                    .bg(header_bg)
                    .add_modifier(ratatui::style::Modifier::BOLD),
            ),
            Span::styled(" ".repeat(gap), Style::default().bg(header_bg)),
            Span::styled(
                LOGIN_HINTS,
                Style::default()
                    .bg(header_bg)
                    .add_modifier(ratatui::style::Modifier::DIM),
            ),
        ]);
        f.render_widget(Paragraph::new(vec![header_line]), login_hdr_area);

        let content_lines = build_login_content_lines(app, width);
        f.render_widget(Paragraph::new(content_lines), login_body_area);
    }

    if !app.login.active && !app.ask_user_selection_no_freeform() {
        let panel_bg = if app.input_mode == InputMode::Shell {
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
        render_input_panel(f, input_area, app, panel_bg);
    }

    if app.show_info {
        let context_window = context_window_for_model(&app.provider.current_model);
        let used_tokens = app.latest_usage.and_then(|u| u.used_tokens());
        let cached_tokens = app.latest_usage.and_then(|u| u.cached_tokens);
        let thinking = app
            .provider
            .thinking_supported
            .then_some(app.provider.current_thinking.as_str());
        let info_line = build_info_line(
            &app.theme.info,
            &app.provider.current_instance.id,
            &app.provider.current_model,
            thinking,
            app.active_agent.as_deref(),
            context_window,
            used_tokens,
            cached_tokens,
            app.cache_miss_warning,
            width,
        );
        f.render_widget(Paragraph::new(vec![info_line]), info_area);
    }
}

/// Apply reverse-video highlighting to lines that are within the mouse
/// drag selection range.
fn apply_mouse_highlight(mut lines: Vec<Line<'static>>, app: &App) -> Vec<Line<'static>> {
    let (start_row, end_row, start_col, end_col) = match app.mouse_select.selection_range() {
        Some(r) => r,
        None => return lines,
    };
    let log_top = app.mouse_select.log_area_top;
    let hit_map = &app.mouse_select.hit_map;

    let highlight_style = Style::default().add_modifier(Modifier::REVERSED);

    for (vi, line) in lines.iter_mut().enumerate() {
        let abs_row = log_top + vi as u16;
        if abs_row < start_row || abs_row > end_row {
            continue;
        }

        let deco = hit_map.get(vi).map(|ls| ls.decoration_width).unwrap_or(0);

        // Clamp column bounds per row (matching extract_selected_text).
        let (col_from, col_to) = if abs_row == start_row && abs_row == end_row {
            (start_col.max(deco), end_col + 1)
        } else if abs_row == start_row {
            (start_col.max(deco), u16::MAX)
        } else if abs_row == end_row {
            (deco, end_col + 1)
        } else {
            (deco, u16::MAX)
        };

        // Walk spans, splitting at column boundaries so only the
        // characters inside [col_from, col_to) get the highlight style.
        let mut col: u16 = 0;
        let mut new_spans: Vec<Span<'static>> = Vec::with_capacity(line.spans.len() * 2);

        for s in &line.spans {
            let content: &str = s.content.as_ref();
            let span_width = unicode_width::UnicodeWidthStr::width(content) as u16;
            let span_end = col + span_width;

            // Entirely before selection or entirely after — pass through.
            if span_end <= col_from || col >= col_to {
                new_spans.push(s.clone());
                col = span_end;
                continue;
            }

            // Split into before / inside / after.
            let mut char_col: u16 = col;
            let mut before = String::new();
            let mut inside = String::new();
            let mut after = String::new();

            for ch in content.chars() {
                let chw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0) as u16;
                let ch_end = char_col + chw;
                if ch_end <= col_from {
                    before.push(ch);
                } else if char_col >= col_to {
                    after.push(ch);
                } else {
                    inside.push(ch);
                }
                char_col = ch_end;
            }

            if !before.is_empty() {
                new_spans.push(Span::styled(before, s.style));
            }
            if !inside.is_empty() {
                new_spans.push(Span::styled(inside, s.style.patch(highlight_style)));
            }
            if !after.is_empty() {
                new_spans.push(Span::styled(after, s.style));
            }

            col = span_end;
        }

        line.spans = new_spans;
    }

    lines
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::ui::log::append_tool_result_block;
    use crate::{
        agent::AgentLoopConfig,
        auth::AuthFlow,
        completion::CompletionItem,
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
        use crate::config::DisplayConfig;
        let instance = crate::provider_instance::ProviderInstance::new(
            "copilot",
            crate::provider_instance::BackendPreset::Copilot,
        );
        App::new(
            instance,
            "gpt-4o",
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
                manual_compaction_requested: false,
                manual_compaction_instructions: None,
                executor: std::sync::Arc::new(crate::agent::DefaultToolExecutor::new()),
                system_prompt: None,
                hooks: HashMap::new(),
                hook_ipc: crate::hooks::HookIpcPublisherHandle::disabled(),
                session_id: String::new(),
            },
            DisplayConfig::default(),
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
        let chunks = input::wrap_input_line("hello world from xi", 11);
        assert_eq!(
            chunks,
            vec!["hello world".to_string(), " from xi".to_string()]
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
            ask_user_header_lines: 0,
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
            ask_user_header_lines: 0,
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
            ask_user_header_lines: 0,
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
            ask_user_header_lines: 0,
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
            ask_user_header_lines: 0,
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
            ask_user_header_lines: 0,
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
            ask_user_header_lines: 0,
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
            ask_user_header_lines: 0,
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
            ask_user_header_lines: 0,
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
            ask_user_header_lines: 0,
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
    fn thinking_shrink_keeps_viewport_anchored() {
        let mut app = make_app();
        app.session.ensure_event_log_for_submit();
        for i in 0..20 {
            app.session
                .append_user_message(format!("user message {i:02}"), 0);
        }
        app.begin_agent_turn();

        // First streaming frame: no thinking yet. This freezes the anchor
        // total at the committed-history height.
        app.session.live_turn.assistant_thinking = Some(String::new());
        let _first = render_to_plain_lines(&mut app, 80, 20);

        // Thinking grows to fill the five-line tail window.
        app.session.live_turn.assistant_thinking = Some("c0\nc1\n\nc2\nc3\nc4".to_string());
        let grown = render_to_plain_lines(&mut app, 80, 20);
        let grown_think = grown.iter().position(|l| l.contains("c1")).unwrap();

        // Appending one more line makes the leading empty line enter the tail
        // window and get trimmed, shrinking the thinking block by one row.
        app.session.live_turn.assistant_thinking = Some("c0\nc1\n\nc2\nc3\nc4\nc5".to_string());
        let shrunk = render_to_plain_lines(&mut app, 80, 20);
        let shrunk_think = shrunk.iter().position(|l| l.contains("c2")).unwrap();

        // Everything above the thinking block must stay put.
        let grown_user = grown
            .iter()
            .position(|l| l.contains("user message 19"))
            .unwrap();
        let shrunk_user = shrunk
            .iter()
            .position(|l| l.contains("user message 19"))
            .unwrap();
        assert_eq!(
            grown_user, shrunk_user,
            "content above the block must stay static"
        );

        // The first visible thinking line must stay on the same screen row.
        assert_eq!(
            grown_think, shrunk_think,
            "thinking block top should stay anchored"
        );

        // The shrunk line is absorbed as a blank row at the bottom of the log.
        let shrunk_last = shrunk.iter().position(|l| l.contains("c5")).unwrap();
        assert!(
            shrunk
                .get(shrunk_last + 1)
                .is_some_and(|l| l.trim().is_empty()),
            "bottom padding should fill the shrunk row, got {:?}",
            shrunk.get(shrunk_last + 1)
        );
    }

    #[test]
    fn throbber_scrolls_out_of_view_when_scrolling_up() {
        let mut app = make_app();
        app.session.ensure_event_log_for_submit();
        for i in 0..20 {
            app.session
                .append_user_message(format!("user message {i:02}"), 0);
        }
        app.begin_agent_turn();
        // Force the throbber visible immediately (skip the 240 ms hold-off).
        app.agent_turn.activity_visible = true;

        let is_braille_line = |l: &str| {
            !l.trim().is_empty()
                && l.trim()
                    .chars()
                    .all(|c| ('\u{2800}'..='\u{28FF}').contains(&c))
        };

        // Auto-scrolled to the bottom: the throbber is rendered within the log.
        let bottom = render_to_plain_lines(&mut app, 80, 20);
        assert!(
            bottom.iter().any(|l| is_braille_line(l)),
            "expected the throbber braille line when auto-scrolled, got {bottom:?}"
        );

        // Scrolling up a page moves the throbber out of the viewport like the
        // rest of the content instead of leaving it pinned at the bottom.
        app.log_view.scroll_up();
        let scrolled = render_to_plain_lines(&mut app, 80, 20);
        assert!(
            !scrolled.iter().any(|l| is_braille_line(l)),
            "throbber should scroll out of view, got {scrolled:?}"
        );
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
        let lines = build_completion_lines(&crate::theme::MenuTheme::default(), &items, 0, 80);
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
        let lines = build_completion_lines(&crate::theme::MenuTheme::default(), &items, 0, 80);
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

        let lines = build_completion_lines(&crate::theme::MenuTheme::default(), &items, 0, 120);
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

        let lines =
            build_selection_lines(&crate::theme::SelectionTheme::default(), &items, 0, 5, 80);
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

        let lines =
            build_selection_lines(&crate::theme::SelectionTheme::default(), &items, 6, 5, 80);
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

        let lines =
            build_selection_lines(&crate::theme::SelectionTheme::default(), &items, 0, 0, 80);
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
        let lines = build_selection_lines(
            &crate::theme::SelectionTheme::default(),
            &items,
            usize::MAX,
            0,
            80,
        );
        let first = line_text(&lines[0]);
        let second = line_text(&lines[1]);
        assert_eq!(first.find('—'), second.find('—'));
    }

    #[test]
    fn hidden_user_messages_are_not_rendered() {
        let mut hidden = Message::user("secret");
        hidden.hidden = true;
        let lines = log::build_log_layout(
            &[hidden, Message::assistant("shown")],
            false,
            80,
            &log::ToolBodyConfig::default(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        )
        .flatten()
        .0;
        assert_eq!(lines.len(), 1);
        assert_eq!(line_text(&lines[0]), "💬 shown");
    }

    #[test]
    fn streaming_empty_assistant_message_is_not_rendered() {
        let lines = log::build_log_layout(
            &[Message::assistant("")],
            true,
            80,
            &log::ToolBodyConfig::default(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        )
        .flatten()
        .0;
        assert!(lines.is_empty());
    }

    #[test]
    fn stream_suffix_is_only_on_final_visible_chunk() {
        let lines = log::build_log_layout(
            &[Message::assistant("abcdefghijklmnopqrstuvwxyz")],
            true,
            8,
            &log::ToolBodyConfig::default(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        )
        .flatten()
        .0;
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
        let lines = log::build_log_layout(
            &[Message::user("hi")],
            false,
            10,
            &log::ToolBodyConfig::default(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        )
        .flatten()
        .0;
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

        let lines = log::build_log_layout(
            &messages,
            false,
            120,
            &log::ToolBodyConfig::default(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        )
        .flatten()
        .0;
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

        let lines = log::build_log_layout(
            &messages,
            false,
            120,
            &log::ToolBodyConfig::default(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        )
        .flatten()
        .0;
        let rendered = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
        assert!(!rendered.contains("[lines 10-20 of 300]"));
        assert!(rendered.contains("╭ alpha"));
    }

    #[test]
    fn tool_result_preview_truncates_with_line_count_marker() {
        // A bash result with many lines should be tail-truncated with ... (N lines total)
        let long_output = (1..=20)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let messages = vec![
            Message::tool_call("1", "bash", json!({"command": "echo hi"})),
            Message::tool_result("1", &long_output, false),
        ];

        let lines = log::build_log_layout(
            &messages,
            false,
            300,
            &log::ToolBodyConfig::default(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        )
        .flatten()
        .0;
        let rendered = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
        assert!(rendered.contains("20 total lines"), "{rendered}");
    }

    #[test]
    fn shell_tool_call_preserves_multiline_command_display() {
        let messages = vec![Message::tool_call(
            "1",
            "bash",
            json!({"command": "echo one\necho two\necho three"}),
        )];

        let lines = log::build_log_layout(
            &messages,
            false,
            120,
            &log::ToolBodyConfig::default(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        )
        .flatten()
        .0;
        let rendered = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
        assert!(
            rendered.contains("💻 echo one\n │ echo two\n ╰ echo three"),
            "{rendered}"
        );
    }

    #[test]
    fn shell_tool_call_truncates_above_five_lines_with_line_count_marker() {
        let messages = vec![Message::tool_call(
            "1",
            "bash",
            json!({"command": "l1\nl2\nl3\nl4\nl5\nl6"}),
        )];

        let lines = log::build_log_layout(
            &messages,
            false,
            120,
            &log::ToolBodyConfig::default(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        )
        .flatten()
        .0;
        let rendered = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
        assert!(
            rendered.contains("💻 l1\n │ l2\n │ l3\n │ l4\n │ l5\n ┆ … 6 total lines"),
            "{rendered}"
        );
        assert!(!rendered.contains("l6"), "{rendered}");
    }

    #[test]
    fn assistant_lines_are_prefixed_with_speech_bubble() {
        let messages = vec![Message::assistant("hello")];
        let lines = log::build_log_layout(
            &messages,
            false,
            80,
            &log::ToolBodyConfig::default(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        )
        .flatten()
        .0;
        assert_eq!(line_text(&lines[0]), "💬 hello");
    }

    #[test]
    fn assistant_provisional_phase_uses_thought_bubble() {
        let mut msg = Message::assistant("working");
        msg.assistant_phase = Some(AssistantPhase::Provisional);
        let lines = log::build_log_layout(
            &[msg],
            false,
            80,
            &log::ToolBodyConfig::default(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        )
        .flatten()
        .0;
        assert_eq!(line_text(&lines[0]), "💭 working");
    }

    #[test]
    fn assistant_unknown_phase_streaming_uses_thought_bubble() {
        let mut msg = Message::assistant("streaming");
        msg.assistant_phase = Some(AssistantPhase::Unknown);
        let lines = log::build_log_layout(
            &[msg],
            true,
            80,
            &log::ToolBodyConfig::default(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        )
        .flatten()
        .0;
        assert_eq!(line_text(&lines[0]), "💭 streaming▋");
    }

    #[test]
    fn static_assistant_notice_does_not_get_streaming_cursor() {
        let msg = Message::assistant(
            "[Agent is working. Press Ctrl-D again to quit and abort the agent loop]",
        );
        let lines = log::build_log_layout(
            &[msg],
            true,
            120,
            &log::ToolBodyConfig::default(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        )
        .flatten()
        .0;
        assert_eq!(
            line_text(&lines[0]),
            "💬 [Agent is working. Press Ctrl-D again to quit and abort the agent loop]"
        );
    }

    #[test]
    fn assistant_thinking_is_prefixed_with_brain() {
        let mut msg = Message::assistant("answer");
        msg.thinking = Some("planning".to_string());
        let messages = vec![msg];
        let lines = log::build_log_layout(
            &messages,
            false,
            80,
            &log::ToolBodyConfig::default(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        )
        .flatten()
        .0;
        assert_eq!(line_text(&lines[0]), "🧠 planning");
        assert_eq!(line_text(&lines[1]), "💬 answer");
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
        assert_eq!(line_text(&out[0]), " │ line one");
        assert_eq!(line_text(&out[1]), " │ line two");
    }

    #[test]
    fn tool_result_block_omits_trailing_blank_line() {
        let mut out = Vec::new();
        append_tool_result_block(&mut out, "uptime output\n", 80, Color::Green);
        assert_eq!(out.len(), 1);
        assert_eq!(line_text(&out[0]), " │ uptime output");
    }

    #[test]
    fn tool_result_block_wraps_and_keeps_prefix() {
        let mut out = Vec::new();
        append_tool_result_block(&mut out, "abcdef", 4, Color::Green);
        assert!(out.len() >= 2);
        for line in out {
            let text = line_text(&line);
            assert!(text.starts_with(" │ "));
            assert!(unicode_width::UnicodeWidthStr::width(text.as_str()) <= 4);
        }
    }

    #[test]
    fn tool_result_block_expands_leading_tabs_after_prefix() {
        let mut out = Vec::new();
        append_tool_result_block(&mut out, "\talpha", 20, Color::Green);
        assert_eq!(line_text(&out[0]), " │  alpha");
    }

    #[test]
    fn redraw_clears_stale_tool_output_cells() {
        let backend = ratatui::backend::TestBackend::new(40, 10);
        let mut terminal = ratatui::Terminal::new(backend).expect("test terminal");
        let mut app = make_app();
        app.session.live_turn.notices.clear();
        app.session
            .live_turn
            .notices
            .extend(vec![Message::tool_result(
                "1",
                "tool output that used to be much longer",
                false,
            )]);

        terminal
            .draw(|f| draw(f, &mut app))
            .expect("first draw succeeds");

        app.session.live_turn.notices.clear();
        app.session
            .live_turn
            .notices
            .extend(vec![Message::tool_result("1", "short", false)]);

        terminal
            .draw(|f| draw(f, &mut app))
            .expect("second draw succeeds");

        let joined = buffer_to_plain_lines(terminal.backend().buffer(), 40, 10).join("\n");
        assert!(joined.contains("· short"), "{joined}");
        assert!(!joined.contains("much longer"), "{joined}");
    }

    #[test]
    fn log_user_background_does_not_extend_into_scrollbar_column() {
        let backend = ratatui::backend::TestBackend::new(20, 8);
        let mut terminal = ratatui::Terminal::new(backend).expect("test terminal");
        let mut app = make_app();
        app.session.live_turn.notices.extend(vec![
            Message::user("one"),
            Message::user("two"),
            Message::user("three"),
            Message::user("four"),
        ]);
        // Scrolled up: scrollbar should be visible, reserving the rightmost column.
        app.log_view.auto_scroll = false;
        app.log_view.log_scroll = 0;

        terminal.draw(|f| draw(f, &mut app)).expect("draw succeeds");

        let buf = terminal.backend().buffer();
        let rightmost_x = 19;
        for y in 0..8 {
            assert_ne!(buf[(rightmost_x, y)].bg, Color::Rgb(50, 50, 64));
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
        app.session.live_turn.notices.extend(vec![
            Message::user("one"),
            Message::user("two"),
            Message::user("three"),
            Message::user("four"),
        ]);
        // Default: auto_scroll = true (pinned to bottom).
        assert!(app.log_view.auto_scroll);

        terminal.draw(|f| draw(f, &mut app)).expect("draw succeeds");

        // auto_scroll should still be true — content fits, no scrollbar needed.
        assert!(
            app.log_view.auto_scroll,
            "auto_scroll should remain true when content fits"
        );

        // Verify no scrollbar glyph in the last column of the log rows.
        let buf = terminal.backend().buffer().clone();
        let width = buf.area.width;
        let log_height = app.log_view.last_log_height;
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
        let sel_bg = crate::theme::SelectionTheme::default()
            .bg
            .unwrap_or(ratatui::style::Color::Reset);
        let sel_sel_bg = crate::theme::SelectionTheme::default()
            .selected
            .bg
            .unwrap_or(ratatui::style::Color::Reset);
        for y in 0..20 {
            let bg = buf[(rightmost_x, y)].bg;
            assert_ne!(bg, sel_bg);
            assert_ne!(bg, sel_sel_bg);
        }
    }

    #[test]
    fn info_context_value_without_usage_shows_window_only() {
        assert_eq!(
            info::format_context_value(Some(128_000), None, None, false),
            "128k"
        );
    }

    #[test]
    fn info_context_value_with_usage_shows_ratio_and_percent() {
        assert_eq!(
            info::format_context_value(Some(128_000), Some(32_000), None, false),
            "32k / 128k (25%)"
        );
    }

    #[test]
    fn info_context_value_unknown_window_stays_unknown() {
        assert_eq!(
            info::format_context_value(None, Some(123), None, false),
            "unknown"
        );
    }

    #[test]
    fn info_context_value_shows_cached_suffix() {
        assert_eq!(
            info::format_context_value(Some(128_000), Some(32_000), Some(16_000), false),
            "32k / 128k (25%) [16k⚡]"
        );
    }

    #[test]
    fn info_context_value_cached_zero_omits_suffix() {
        assert_eq!(
            info::format_context_value(Some(128_000), Some(32_000), Some(0), false),
            "32k / 128k (25%)"
        );
    }

    #[test]
    fn info_context_value_unknown_window_with_cached() {
        assert_eq!(
            info::format_context_value(None, None, Some(4_000), false),
            "unknown [4k⚡]"
        );
    }

    #[test]
    fn info_context_value_shows_warning_on_cache_miss() {
        assert_eq!(
            info::format_context_value(Some(128_000), Some(32_000), Some(0), true),
            "32k / 128k (25%) ⚠️"
        );
    }

    #[test]
    fn info_context_value_warning_overrides_cached_suffix() {
        // Warning takes precedence over cached suffix when both are set.
        assert_eq!(
            info::format_context_value(Some(128_000), Some(64_000), Some(16_000), true),
            "64k / 128k (50%) ⚠️"
        );
    }

    #[test]
    fn info_line_renders_context_utilization_when_available() {
        let line = info::build_info_line(
            &crate::theme::InfoTheme::default(),
            "copilot",
            "gpt-4o",
            Some("medium"),
            None,
            Some(128_000),
            Some(64_000),
            None,
            false,
            200,
        );
        let text = line_text(&line);
        assert!(text.contains("context 64k / 128k (50%)"), "{text}");
    }

    #[test]
    fn info_line_omits_thinking_when_unavailable() {
        let line = info::build_info_line(
            &crate::theme::InfoTheme::default(),
            "openai",
            "gpt-4o",
            None,
            None,
            Some(128_000),
            None,
            None,
            false,
            200,
        );
        let text = line_text(&line);
        assert!(!text.contains("thinking"), "{text}");
    }

    #[test]
    fn info_line_shows_agent_when_set() {
        let line = info::build_info_line(
            &crate::theme::InfoTheme::default(),
            "copilot",
            "gpt-4o",
            None,
            Some("explorer"),
            Some(128_000),
            None,
            None,
            false,
            200,
        );
        let text = line_text(&line);
        assert!(text.contains("agent explorer"), "{text}");
    }

    #[test]
    fn info_line_omits_agent_when_none() {
        let line = info::build_info_line(
            &crate::theme::InfoTheme::default(),
            "copilot",
            "gpt-4o",
            None,
            None,
            Some(128_000),
            None,
            None,
            false,
            200,
        );
        let text = line_text(&line);
        assert!(!text.contains("agent"), "{text}");
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

        let lines = log::build_log_layout(
            &messages,
            false,
            80,
            &log::ToolBodyConfig::default(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        )
        .flatten()
        .0;
        // Leading/trailing newlines are stripped; leading spaces (indentation)
        // on the first content line are preserved.
        let result_lines: Vec<_> = lines
            .iter()
            .map(line_text)
            .filter(|l| {
                l.starts_with(" ╭ ")
                    || l.starts_with(" │ ")
                    || l.starts_with(" ╰ ")
                    || l.starts_with(" · ")
                    || l.starts_with(" ┆ ")
            })
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

        let lines = log::build_log_layout(
            &messages,
            false,
            80,
            &log::ToolBodyConfig::default(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        )
        .flatten()
        .0;
        let result_lines: Vec<_> = lines
            .iter()
            .map(line_text)
            .filter(|l| {
                l.starts_with(" ╭ ")
                    || l.starts_with(" │ ")
                    || l.starts_with(" ╰ ")
                    || l.starts_with(" · ")
                    || l.starts_with(" ┆ ")
            })
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

        let lines = log::build_log_layout(
            &messages,
            false,
            80,
            &log::ToolBodyConfig::default(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        )
        .flatten()
        .0;
        let rendered = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
        assert!(rendered.contains("· load: 1.0"), "{rendered}");
        // No extra blank line after the content.
        assert!(!rendered.contains("· load: 1.0\n│"), "{rendered}");
    }

    #[test]
    fn input_panel_scrolls_when_text_exceeds_viewport() {
        let mut app = make_app();
        // Insert 20 short lines — no wrapping, but they exceed the input
        // panel height (capped at 40% of terminal = ~9 rows in 24-row term).
        for i in 1..=20 {
            app.textarea.insert_str(format!("Line {i}\n"));
        }
        // Render into a typical terminal. Input height = min(20, 24*40%) = 9.
        render_to_plain_lines(&mut app, 80, 24);

        assert!(
            app.input_scroll > 0,
            "input_scroll should be > 0 when 20 lines exceed ~9-line viewport, got {}",
            app.input_scroll
        );
    }

    #[test]
    fn input_panel_last_line_shows_cursor_line_when_scrolled() {
        let mut app = make_app();
        for i in 1..=20 {
            app.textarea.insert_str(format!("Line {i}\n"));
        }
        let buf = render_to_buffer(&mut app, 80, 24);

        let input_rows: Vec<(u16, String)> = (0..24u16)
            .filter_map(|y| {
                let row_text: String = (0..80u16)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string();
                if row_text.contains("Line ") && !row_text.contains("💬") {
                    Some((y, row_text))
                } else {
                    None
                }
            })
            .collect();

        assert!(
            !input_rows.is_empty(),
            "expected to find input lines containing 'Line '"
        );

        let last_input_row = input_rows.last().unwrap();
        assert!(
            !last_input_row.1.contains("Line 1"),
            "last visible input row should not be 'Line 1' when scrolled down; got: {}",
            last_input_row.1
        );
        assert!(
            last_input_row.1.contains("Line ") && !last_input_row.1.contains("Line 1"),
            "last visible input row should show a scrolled-to line number (not Line 1); got: {}",
            last_input_row.1
        );
    }

    #[test]
    fn ask_user_freeform_typing_uses_ask_user_input_bg() {
        use crate::agent::types::{AskRequest, AskUserOption, AskUserResponse};
        let mut app = make_app();
        let (reply_tx, _reply_rx) = tokio::sync::oneshot::channel::<AskUserResponse>();
        app.receive_ask_request(AskRequest {
            question: "What next?".to_string(),
            context: None,
            options: vec![AskUserOption {
                title: "Option A".to_string(),
                description: None,
            }],
            allow_multiple: false,
            allow_freeform: true,
            reply: reply_tx,
        });

        // Before typing: input should have ask_user bg (Rgb(50, 30, 15)).
        let buf_before = render_to_buffer(&mut app, 40, 12);
        // After typing: input should have ask_user bg.
        app.begin_ask_freeform_typing();
        app.textarea.insert_char('x');
        let buf_after = render_to_buffer(&mut app, 40, 12);

        // Find the input row — the row where 'x' appears with the ask_user
        // input background (not in the header where "next" contains 'x').
        let ask_user_bg = ratatui::style::Color::Rgb(50, 30, 15);
        let input_row = (0..12u16)
            .find(|&y| {
                (0..40u16).any(|x| {
                    buf_after[(x, y)].symbol() == "x" && buf_after[(x, y)].bg == ask_user_bg
                })
            })
            .expect("should find input row");

        // The cell background on the input row should be the ask_user bg.
        let bg_after = buf_after[(0, input_row)].bg;
        let bg_before = buf_before[(0, input_row)].bg;
        assert_eq!(
            bg_after, ask_user_bg,
            "input bg after typing should be ask_user bg (Rgb(50, 30, 15)), got {bg_after:?}"
        );
        assert_ne!(
            bg_after, bg_before,
            "input bg should change when freeform typing begins (before={bg_before:?}, after={bg_after:?})"
        );
    }

    // ── Rendering benchmark harness ───────────────────────────────────────────
    //
    // These tests are not part of the correctness suite: they measure how long
    // a full frame takes to render with the ratatui `TestBackend`. They are
    // gated behind `#[ignore]` so the normal `cargo test` run stays fast.
    //
    // Run with:
    //   cargo test --release -- --ignored --nocapture bench_render
    //
    // The goal is a cheap, dependency-free way to (a) get a baseline, (b) spot
    // a hot path, and (c) compare two git revisions to locate a regression.
    // For continuous regression tracking, a `criterion` bench that drives the
    // same `draw` entry point is preferable (see notes in the report).

    /// A single markdown-heavy assistant message: prose, a fenced code block,
    /// a table, and a nested list. These are the expensive paths in
    /// `markdown::render_with_theme`.
    fn bench_assistant_body() -> &'static str {
        r#"Here's how to solve that problem.

```rust
fn main() {
    let xs: Vec<u32> = (0..100).collect();
    let total: u32 = xs.iter().sum();
    println!("total = {total}");
}
```

Some prose with **bold**, *italic*, and `inline code`. Here is a table:

| Name | Value | Notes |
|------|-------|-------|
| alpha | 1 | first item in the table |
| beta  | 2 | second item in the table |
| gamma | 3 | third item in the table |

- first bullet point with some text
- second bullet point
  - nested bullet that should indent correctly
  - another nested bullet
- third bullet point wrapping onto multiple lines to exercise the wrapping path
"#
    }

    /// Build an app whose log contains `turns` user/assistant pairs.
    fn bench_app(turns: usize) -> App {
        let mut app = make_app();
        let body = bench_assistant_body();
        for i in 0..turns {
            app.push_notice(Message::user(format!(
                "Question number {i}: please explain the following code and give me a table."
            )));
            app.push_notice(Message::assistant(body));
        }
        app
    }

    /// Build an app whose log contains `turns` user/assistant pairs committed
    /// to session state (the durable committed history), plus nothing in the
    /// live overlay. Exercises the committed-heavy streaming path.
    fn bench_app_committed(turns: usize) -> App {
        use crate::event_log::EventLog;
        use crate::session_event::SessionEvent;
        use crate::session_state::SessionState;

        let mut app = make_app();
        let body = bench_assistant_body();
        let path =
            std::env::temp_dir().join(format!("xi-bench-committed-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut state = SessionState::from_event_log(EventLog::load(&path).expect("event log"));
        for i in 0..turns {
            state
                .append_immediate(SessionEvent::UserMessage {
                    content: format!("Question number {i}: explain this code and give a table."),
                    timestamp: (i * 2) as u64,
                })
                .expect("append user");
            state
                .append_batch(&[SessionEvent::AssistantMessage {
                    content: body.to_string(),
                    thinking: None,
                    phase: AssistantPhase::Final,
                    usage: None,
                    timestamp: (i * 2 + 1) as u64,
                }])
                .expect("append assistant");
        }
        app.session.session_state = Some(state);
        app
    }

    /// Measure three frame-cost scenarios across a sweep of terminal sizes and
    /// message counts:
    ///
    /// - `full`: a true cold rebuild (block cache emptied) — every message's
    ///   markdown + wrapping re-runs.
    /// - `stream`: the streaming path — each frame bumps the revision and
    ///   grows the tail message, so prior messages are cache hits and only the
    ///   tail re-renders.
    /// - `warm`: idle redraw with no change — the whole-log cache hits and the
    ///   markdown path is skipped entirely.
    ///
    /// Prints a table of per-frame timings; asserts nothing.
    #[test]
    #[ignore]
    fn bench_render() {
        use std::time::Instant;

        const ITERS: usize = 20;
        let sizes: [(u16, u16); 3] = [(80, 24), (120, 40), (200, 60)];

        println!("\n=== xi-agent render benchmark (TestBackend) ===");
        println!("turns | size        | full (us) | stream (us) | warm (us)");
        println!("------+-------------+-----------+-------------+----------");

        for &turns in &[20usize, 50, 200] {
            for &(w, h) in &sizes {
                // Full rebuild: clear the block cache so every message re-renders.
                let mut app = bench_app(turns);
                let backend = ratatui::backend::TestBackend::new(w, h);
                let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
                let mut full = std::time::Duration::ZERO;
                for _ in 0..ITERS {
                    app.log_view.invalidate();
                    app.log_view.block_cache.clear();
                    let t = Instant::now();
                    terminal.draw(|f| draw(f, &mut app)).unwrap();
                    full += t.elapsed();
                }

                // Streaming: grow the tail message each frame; prior messages
                // must be cache hits.
                let mut app = bench_app(turns);
                app.push_notice(Message::assistant("initial streaming response"));
                let backend = ratatui::backend::TestBackend::new(w, h);
                let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
                terminal.draw(|f| draw(f, &mut app)).unwrap(); // warm the caches
                let mut stream = std::time::Duration::ZERO;
                for _ in 0..ITERS {
                    app.session
                        .live_turn
                        .notices
                        .last_mut()
                        .unwrap()
                        .content
                        .push_str(" streaming word ");
                    let t = Instant::now();
                    terminal.draw(|f| draw(f, &mut app)).unwrap();
                    stream += t.elapsed();
                }

                // Warm: idle redraw, no change.
                let mut app = bench_app(turns);
                let backend = ratatui::backend::TestBackend::new(w, h);
                let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
                terminal.draw(|f| draw(f, &mut app)).unwrap(); // warm the caches
                let mut warm = std::time::Duration::ZERO;
                for _ in 0..ITERS {
                    let t = Instant::now();
                    terminal.draw(|f| draw(f, &mut app)).unwrap();
                    warm += t.elapsed();
                }

                let full_us = full.as_micros() as f64 / ITERS as f64;
                let stream_us = stream.as_micros() as f64 / ITERS as f64;
                let warm_us = warm.as_micros() as f64 / ITERS as f64;
                println!(
                    "{:>5} | {:>3}x{:<3}     | {:>9.1} | {:>11.1} | {:>8.1}",
                    turns, w, h, full_us, stream_us, warm_us
                );
            }
        }
        println!();
    }

    /// Same three scenarios, but with `turns` messages committed to session
    /// state (the durable history) and the streaming tail in the live overlay.
    /// This is the long-session shape the incremental rendering optimizes for:
    /// committed messages should be cache hits, so `stream`/`warm` should not
    /// scale with `turns`.
    #[test]
    #[ignore]
    fn bench_render_committed() {
        use std::time::Instant;

        const ITERS: usize = 20;
        let sizes: [(u16, u16); 3] = [(80, 24), (120, 40), (200, 60)];

        println!("\n=== xi-agent render benchmark (committed history) ===");
        println!("turns | size        | full (us) | stream (us) | warm (us)");
        println!("------+-------------+-----------+-------------+----------");

        for &turns in &[20usize, 50, 200] {
            for &(w, h) in &sizes {
                // Full rebuild: clear the block cache so every committed message
                // re-renders.
                let mut app = bench_app_committed(turns);
                let backend = ratatui::backend::TestBackend::new(w, h);
                let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
                let mut full = std::time::Duration::ZERO;
                for _ in 0..ITERS {
                    app.log_view.invalidate();
                    app.log_view.block_cache.clear();
                    let t = Instant::now();
                    terminal.draw(|f| draw(f, &mut app)).unwrap();
                    full += t.elapsed();
                }

                // Streaming: grow the overlay assistant each frame; committed
                // messages must be cache hits (generation-keyed, no re-hash).
                let mut app = bench_app_committed(turns);
                app.session.live_turn.assistant_content = "initial".to_string();
                let backend = ratatui::backend::TestBackend::new(w, h);
                let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
                terminal.draw(|f| draw(f, &mut app)).unwrap(); // warm the caches
                let mut stream = std::time::Duration::ZERO;
                for _ in 0..ITERS {
                    app.session
                        .live_turn
                        .assistant_content
                        .push_str(" streaming word ");
                    let t = Instant::now();
                    terminal.draw(|f| draw(f, &mut app)).unwrap();
                    stream += t.elapsed();
                }

                // Warm: idle redraw, no change.
                let mut app = bench_app_committed(turns);
                let backend = ratatui::backend::TestBackend::new(w, h);
                let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
                terminal.draw(|f| draw(f, &mut app)).unwrap(); // warm the caches
                let mut warm = std::time::Duration::ZERO;
                for _ in 0..ITERS {
                    let t = Instant::now();
                    terminal.draw(|f| draw(f, &mut app)).unwrap();
                    warm += t.elapsed();
                }

                let full_us = full.as_micros() as f64 / ITERS as f64;
                let stream_us = stream.as_micros() as f64 / ITERS as f64;
                let warm_us = warm.as_micros() as f64 / ITERS as f64;
                println!(
                    "{:>5} | {:>3}x{:<3}     | {:>9.1} | {:>11.1} | {:>8.1}",
                    turns, w, h, full_us, stream_us, warm_us
                );
            }
        }
        println!();
    }
}
