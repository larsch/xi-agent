use ratatui::{
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::app::App;

pub(super) fn render(f: &mut ratatui::Frame, area: Rect, app: &App) {
    let steering = &app.theme.log.steering;
    let mut style = Style::default();
    if let Some(fg) = steering.fg {
        style = style.fg(fg);
    }
    if steering.italic == Some(true) {
        style = style.add_modifier(ratatui::style::Modifier::ITALIC);
    }

    let prefix = steering.prefix.text.as_deref().unwrap_or("🕹️ ");

    let steering_lines: Vec<Line<'static>> = app
        .queued_steering()
        .iter()
        .take(3)
        .map(|msg| Line::from(Span::styled(format!("{prefix}{msg}"), style)))
        .collect();

    if !steering_lines.is_empty() {
        f.render_widget(Paragraph::new(steering_lines), area);
    }
}
