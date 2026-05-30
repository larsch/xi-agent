use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::app::{App, StreamingStatus};

const THROBBER_FRAMES: &[char] = &['⣾', '⣽', '⣻', '⢿', '⡿', '⣟', '⣯', '⣷'];

pub(super) fn render_activity(f: &mut ratatui::Frame, area: Rect, app: &App) {
    let throbber_style = Style::default()
        .fg(Color::Rgb(160, 200, 255))
        .add_modifier(ratatui::style::Modifier::BOLD);
    let hint_style = Style::default()
        .fg(Color::Rgb(100, 140, 100))
        .add_modifier(ratatui::style::Modifier::ITALIC);
    let frame = THROBBER_FRAMES[(app.agent_turn.tick as usize) % THROBBER_FRAMES.len()];

    let mut spans: Vec<Span<'static>> = Vec::new();
    if app.throbber_visible() {
        spans.push(Span::styled(format!("{frame}"), throbber_style));
    }
    if let Some(cursor_idx) = app.step_back.cursor {
        if !spans.is_empty() {
            spans.push(Span::raw("  "));
        }
        let boundaries = app.user_message_boundaries();
        let total = boundaries.len();
        // How many boundaries are at or after the cursor (i.e. will be discarded)?
        let steps_back = boundaries.iter().filter(|&&i| i >= cursor_idx).count();
        let step_style = Style::default()
            .fg(Color::Rgb(220, 180, 80))
            .add_modifier(ratatui::style::Modifier::BOLD);
        spans.push(Span::styled(
            format!("[step back: {steps_back} of {total} — Enter to branch, Esc to cancel]"),
            step_style,
        ));
    }
    if app.log_view.full_output {
        if !spans.is_empty() {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled("[full output — Ctrl+F to toggle]", hint_style));
    }

    let line = if spans.is_empty() {
        Line::default()
    } else {
        Line::from(spans)
    };
    f.render_widget(Paragraph::new(line), area);
}

pub(super) fn render_provider_status(f: &mut ratatui::Frame, area: Rect, app: &App) {
    let status_text_style = Style::default()
        .fg(Color::Rgb(160, 160, 180))
        .add_modifier(ratatui::style::Modifier::ITALIC);

    let provider_message = match &app.agent_turn.status {
        Some(StreamingStatus::Message(s) | StreamingStatus::CompletedMessage(s)) => {
            Some(s.as_str())
        }
        _ => None,
    };

    let line = match provider_message {
        Some(status) => Line::from(Span::styled(status.to_owned(), status_text_style)),
        None => Line::default(),
    };

    f.render_widget(Paragraph::new(line), area);
}
