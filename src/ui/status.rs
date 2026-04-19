use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::app::{App, StreamingStatus};

const THROBBER_FRAMES: &[char] = &['⣾', '⣽', '⣻', '⢿', '⡿', '⣟', '⣯', '⣷'];

pub(super) fn render(f: &mut ratatui::Frame, area: Rect, app: &App) {
    let throbber_style = Style::default()
        .fg(Color::Rgb(160, 200, 255))
        .add_modifier(ratatui::style::Modifier::BOLD);
    let status_text_style = Style::default()
        .fg(Color::Rgb(160, 160, 180))
        .add_modifier(ratatui::style::Modifier::ITALIC);

    let show_throbber = app.throbber_visible();
    let frame = THROBBER_FRAMES[(app.throbber_tick as usize) % THROBBER_FRAMES.len()];

    let provider_message = match &app.streaming_status {
        Some(StreamingStatus::Message(s) | StreamingStatus::CompletedMessage(s)) => {
            Some(s.as_str())
        }
        _ => None,
    };
    let status_line = match (show_throbber, provider_message) {
        (true, Some(status)) => Line::from(vec![
            Span::styled(format!("{frame}"), throbber_style),
            Span::styled(format!(" {status}"), status_text_style),
        ]),
        (true, None) => Line::from(Span::styled(format!("{frame}"), throbber_style)),
        (false, Some(status)) => Line::from(Span::styled(status.to_owned(), status_text_style)),
        (false, None) => Line::default(),
    };
    f.render_widget(Paragraph::new(status_line), area);
}
