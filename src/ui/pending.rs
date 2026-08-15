use ratatui::{
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::app::App;

pub(super) fn render(f: &mut ratatui::Frame, area: Rect, app: &App) {
    let steering = &app.theme.log.steering;
    let mut icon_style = Style::default();
    let mut msg_style = Style::default();
    if let Some(fg) = steering.fg {
        icon_style = icon_style.fg(fg);
        msg_style = msg_style.fg(fg);
    }
    if steering.italic == Some(true) {
        msg_style = msg_style.add_modifier(ratatui::style::Modifier::ITALIC);
    }

    let prefix = steering.prefix.text.as_deref().unwrap_or("📣 ");

    let steering_lines: Vec<Line<'static>> = app
        .queued_steering()
        .iter()
        .take(3)
        .map(|msg| {
            Line::from(vec![
                Span::styled(prefix.to_string(), icon_style),
                Span::styled(msg.to_string(), msg_style),
            ])
        })
        .collect();

    if !steering_lines.is_empty() {
        f.render_widget(Paragraph::new(steering_lines), area);
    }
}
