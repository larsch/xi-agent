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
    let frame = THROBBER_FRAMES[(app.throbber_tick as usize) % THROBBER_FRAMES.len()];
    let line = if app.throbber_visible() {
        Line::from(Span::styled(format!("{frame}"), throbber_style))
    } else {
        Line::default()
    };
    f.render_widget(Paragraph::new(line), area);
}

pub(super) fn render_provider_status(f: &mut ratatui::Frame, area: Rect, app: &App) {
    let status_text_style = Style::default()
        .fg(Color::Rgb(160, 160, 180))
        .add_modifier(ratatui::style::Modifier::ITALIC);

    let provider_message = match &app.streaming_status {
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
