use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
};
use unicode_width::UnicodeWidthStr;

use crate::{
    completion::CompletionItem,
    selection_state::MAX_SELECTION_VISIBLE,
    theme::{MenuTheme, SelectionTheme},
};

pub(super) fn build_completion_lines(
    theme: &MenuTheme,
    completions: &[CompletionItem],
    selected: usize,
    terminal_width: usize,
) -> Vec<Line<'static>> {
    let ct = &theme.completion;
    let completion_bg = ct.bg.unwrap_or(Color::Rgb(22, 22, 38));
    let sel_bg = ct.selected.bg.unwrap_or(Color::Rgb(55, 55, 100));
    let cmd_fg = ct.cmd.fg.unwrap_or(Color::Rgb(120, 200, 255));
    let desc_fg = ct.desc.fg.unwrap_or(Color::Rgb(140, 140, 160));
    let match_fg = ct.r#match.fg.unwrap_or(Color::Rgb(255, 220, 80));

    let label_col = completions
        .iter()
        .filter(|c| !c.loading)
        .map(|c| c.label.len())
        .max()
        .unwrap_or(0)
        .max(8);

    const SEP: &str = "  —  ";
    const INDENT: &str = "  ";

    completions
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let bg = if i == selected { sel_bg } else { completion_bg };

            if item.loading {
                let fill =
                    " ".repeat(terminal_width.saturating_sub(INDENT.len() + item.label.len()));
                let fg = if item.error { Color::Red } else { Color::Reset };
                return Line::from(vec![
                    Span::styled(INDENT, Style::default().bg(bg)),
                    Span::styled(
                        item.label.clone(),
                        Style::default().fg(fg).bg(bg).add_modifier(
                            ratatui::style::Modifier::ITALIC | ratatui::style::Modifier::DIM,
                        ),
                    ),
                    Span::styled(fill, Style::default().bg(bg)),
                ]);
            }

            let label_padded = format!("{:<width$}", item.label, width = label_col);
            let used = INDENT.len()
                + label_col
                + if item.detail.is_empty() {
                    0
                } else {
                    SEP.len() + item.detail.len()
                };
            let fill = " ".repeat(terminal_width.saturating_sub(used));

            let label_spans: Vec<Span<'static>> = if let Some((mstart, mend)) = item.match_range {
                let mstart = mstart.min(item.label.len());
                let mend = mend.min(item.label.len());
                let before = item.label[..mstart].to_string();
                let matched = item.label[mstart..mend].to_string();
                let after_raw = &item.label[mend..];
                let after = format!(
                    "{after_raw:<pad$}",
                    pad = label_col.saturating_sub(mstart + matched.len())
                );
                vec![
                    Span::styled(before, Style::default().fg(cmd_fg).bg(bg)),
                    Span::styled(
                        matched,
                        Style::default()
                            .fg(match_fg)
                            .bg(bg)
                            .add_modifier(ratatui::style::Modifier::BOLD),
                    ),
                    Span::styled(after, Style::default().fg(cmd_fg).bg(bg)),
                ]
            } else {
                vec![Span::styled(
                    label_padded,
                    Style::default().fg(cmd_fg).bg(bg),
                )]
            };

            if item.detail.is_empty() {
                let mut spans = vec![Span::styled(INDENT, Style::default().bg(bg))];
                spans.extend(label_spans);
                spans.push(Span::styled(fill, Style::default().bg(bg)));
                Line::from(spans)
            } else {
                let mut spans = vec![Span::styled(INDENT, Style::default().bg(bg))];
                spans.extend(label_spans);
                spans.push(Span::styled(
                    SEP,
                    Style::default()
                        .bg(bg)
                        .add_modifier(ratatui::style::Modifier::DIM),
                ));
                spans.push(Span::styled(
                    item.detail.clone(),
                    Style::default().fg(desc_fg).bg(bg),
                ));
                spans.push(Span::styled(fill, Style::default().bg(bg)));
                Line::from(spans)
            }
        })
        .collect()
}

pub(super) fn build_selection_lines(
    theme: &SelectionTheme,
    items: &[CompletionItem],
    selected: usize,
    scroll: usize,
    terminal_width: usize,
) -> Vec<Line<'static>> {
    const INDENT: &str = "  ";
    const PREFIX_WIDTH: usize = 2;
    const SEP: &str = "  —  ";

    let sel_bg = theme.selected.bg.unwrap_or(Color::Rgb(30, 90, 30));
    let item_bg = theme.bg.unwrap_or(Color::Rgb(18, 35, 18));
    let item_fg = theme.item.fg.unwrap_or(Color::Rgb(140, 220, 140));

    let visible: Vec<_> = items
        .iter()
        .skip(scroll)
        .take(MAX_SELECTION_VISIBLE)
        .collect();
    let label_col = visible
        .iter()
        .filter(|it| !it.loading && !it.detail.is_empty())
        .map(|it| it.label.width())
        .max()
        .unwrap_or(0);

    items
        .iter()
        .enumerate()
        .skip(scroll)
        .take(MAX_SELECTION_VISIBLE)
        .map(|(i, item)| {
            let is_sel = i == selected;
            let bg = if is_sel { sel_bg } else { item_bg };

            if item.loading {
                let fill =
                    " ".repeat(terminal_width.saturating_sub(INDENT.len() + item.label.width()));
                let fg = if item.error {
                    Color::Red
                } else if item.complete_to.is_empty() && item.detail.is_empty() {
                    Color::Yellow
                } else {
                    Color::Reset
                };
                let modifier = if item.complete_to.is_empty() && item.detail.is_empty() {
                    ratatui::style::Modifier::BOLD
                } else {
                    ratatui::style::Modifier::ITALIC | ratatui::style::Modifier::DIM
                };
                return Line::from(vec![
                    Span::styled(INDENT, Style::default().bg(bg)),
                    Span::styled(
                        item.label.clone(),
                        Style::default().fg(fg).bg(bg).add_modifier(modifier),
                    ),
                    Span::styled(fill, Style::default().bg(bg)),
                ]);
            }

            let prefix = if is_sel { "▶ " } else { "  " };

            if !item.detail.is_empty() {
                let pad = " ".repeat(label_col.saturating_sub(item.label.width()));
                let used =
                    INDENT.len() + PREFIX_WIDTH + label_col + SEP.len() + item.detail.width();
                let fill = " ".repeat(terminal_width.saturating_sub(used));
                Line::from(vec![
                    Span::styled(INDENT, Style::default().bg(bg)),
                    Span::styled(prefix, Style::default().fg(Color::White).bg(bg)),
                    Span::styled(item.label.clone(), Style::default().fg(item_fg).bg(bg)),
                    Span::styled(pad, Style::default().bg(bg)),
                    Span::styled(SEP, Style::default().fg(Color::DarkGray).bg(bg)),
                    Span::styled(item.detail.clone(), Style::default().fg(Color::Gray).bg(bg)),
                    Span::styled(fill, Style::default().bg(bg)),
                ])
            } else {
                let used = INDENT.len() + PREFIX_WIDTH + item.label.width();
                let fill = " ".repeat(terminal_width.saturating_sub(used));
                Line::from(vec![
                    Span::styled(INDENT, Style::default().bg(bg)),
                    Span::styled(prefix, Style::default().fg(Color::White).bg(bg)),
                    Span::styled(item.label.clone(), Style::default().fg(item_fg).bg(bg)),
                    Span::styled(fill, Style::default().bg(bg)),
                ])
            }
        })
        .collect()
}
