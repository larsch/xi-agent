use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
};
use unicode_width::UnicodeWidthStr;

use crate::{app::App, auth::AuthFlow};

use super::input::wrap_str;

/// Indentation applied to each wrapped URL line.
pub(super) const LOGIN_URL_INDENT: &str = "    ";

pub(super) fn build_login_content_lines(app: &mut App, width: usize) -> Vec<Line<'static>> {
    let theme = &app.theme.login;
    let content_bg = theme.content.bg.unwrap_or(Color::Rgb(15, 22, 48));
    let fill_style = Style::default().bg(content_bg);
    let fill = |used: usize| Span::styled(" ".repeat(width.saturating_sub(used)), fill_style);

    let instruction_fg = theme.instruction.fg.unwrap_or(Color::Rgb(180, 180, 200));
    let instruction_style = Style::default().fg(instruction_fg).bg(content_bg);
    let status_fg = theme.status.fg.unwrap_or(Color::White);
    let status_style = Style::default().fg(status_fg).bg(content_bg);
    let url_key_fg = theme.url_key.fg.unwrap_or(Color::Rgb(120, 200, 255));
    let url_key_style = Style::default().fg(url_key_fg).bg(content_bg);
    let url_val_fg = theme.url_val.fg.unwrap_or(Color::Rgb(100, 220, 100));
    let url_val_style = Style::default().fg(url_val_fg).bg(content_bg);
    let code_key_fg = theme.code_key.fg.unwrap_or(Color::Rgb(120, 200, 255));
    let code_key_style = Style::default().fg(code_key_fg).bg(content_bg);
    let code_val_fg = theme.code_val.fg.unwrap_or(Color::Yellow);
    let code_val_style = Style::default().fg(code_val_fg).bg(content_bg);

    let mut lines: Vec<Line<'static>> = Vec::new();

    let instruction = match app.login.auth_flow {
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

    let info = app.login.info.clone();
    let info_len = info.width();
    lines.push(Line::from(vec![
        Span::styled(format!("  {info}"), status_style),
        fill(2 + info_len),
    ]));

    if let Some(url) = &app.login.url.clone() {
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

    if let Some(code) = &app.login.code.clone() {
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
