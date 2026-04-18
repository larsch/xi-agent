use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::app::App;

pub(super) fn render(f: &mut ratatui::Frame, area: Rect, app: &App) {
    let steering_style = Style::default()
        .fg(Color::Rgb(200, 200, 120))
        .add_modifier(ratatui::style::Modifier::ITALIC);

    let steering_lines: Vec<Line<'static>> = app
        .queued_steering()
        .iter()
        .take(3)
        .map(|msg| Line::from(Span::styled(format!("🕹️ {msg}"), steering_style)))
        .collect();

    if !steering_lines.is_empty() {
        f.render_widget(Paragraph::new(steering_lines), area);
    }
}
