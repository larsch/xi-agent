use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::App;

pub(super) fn render_before(f: &mut ratatui::Frame, area: Rect, app: &App) {
    let (before, _) = app.queue_preview();
    render_side(f, area, app, before);
}

pub(super) fn render_after(f: &mut ratatui::Frame, area: Rect, app: &App) {
    let (_, after) = app.queue_preview();
    render_side(f, area, app, after);
}

fn render_side(f: &mut ratatui::Frame, area: Rect, app: &App, entries: Vec<(String, bool)>) {
    let steering = &app.theme.log.steering;
    let prefix = steering.prefix.text.as_deref().unwrap_or("📣 ");
    let mut msg_style = Style::default().fg(Color::DarkGray);
    if steering.italic == Some(true) {
        msg_style = msg_style.add_modifier(Modifier::ITALIC);
    }
    let lines = entries
        .into_iter()
        .map(|(text, stashed)| {
            let icon = if stashed { "📌 " } else { prefix };
            let text = text.lines().next().unwrap_or_default().replace('\t', " ");
            let available = (area.width as usize).saturating_sub(icon.width());
            Line::from(vec![
                Span::raw(icon.to_string()),
                Span::styled(truncate_width(&text, available), msg_style),
            ])
        })
        .collect::<Vec<_>>();
    if !lines.is_empty() {
        f.render_widget(Paragraph::new(lines), area);
    }
}

fn truncate_width(text: &str, max_width: usize) -> String {
    if text.width() <= max_width {
        return text.to_string();
    }
    if max_width == 0 {
        return String::new();
    }
    let content_width = max_width.saturating_sub(1);
    let mut output = String::new();
    let mut width = 0;
    for ch in text.chars() {
        let char_width = ch.width().unwrap_or(0);
        if width + char_width > content_width {
            break;
        }
        output.push(ch);
        width += char_width;
    }
    output.push('…');
    output
}

#[cfg(test)]
mod tests {
    use super::truncate_width;

    #[test]
    fn preview_truncation_respects_display_width() {
        assert_eq!(truncate_width("long draft", 6), "long …");
        assert_eq!(truncate_width("界界界", 5), "界界…");
    }
}
