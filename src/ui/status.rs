use ratatui::{
    layout::Rect,
    style::Modifier,
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::agent::AgentActivity;
use crate::app::{App, StreamingStatus};

const BRAILLE_BITS: [[u8; 2]; 4] = [[0x01, 0x08], [0x02, 0x10], [0x04, 0x20], [0x40, 0x80]];
const TRIANGLE_PERIOD: usize = 12;
const TRIANGLE_PERIMETER: [(usize, usize); TRIANGLE_PERIOD] = [
    (0, 0),
    (1, 0),
    (2, 0),
    (3, 0),
    (3, 1),
    (3, 2),
    (3, 3),
    (2, 3),
    (1, 3),
    (0, 3),
    (0, 2),
    (0, 1),
];
const TRIANGLE_OFFSETS: [usize; 3] = [0, 4, 8];

type Pixel = (usize, usize);

fn braille(pixels: &[Pixel]) -> String {
    let mut cells = [0_u8; 2];
    for &(x, y) in pixels {
        cells[x / 2] |= BRAILLE_BITS[y][x % 2];
    }
    cells
        .into_iter()
        .map(|cell| char::from_u32(0x2800 + u32::from(cell)).unwrap())
        .collect()
}

fn triangle_frame(tick: usize) -> String {
    let pixels: [Pixel; 3] =
        TRIANGLE_OFFSETS.map(|offset| TRIANGLE_PERIMETER[(tick + offset) % TRIANGLE_PERIOD]);
    braille(&pixels)
}

fn line_length(line_number: usize) -> usize {
    let mut value = (line_number as u32).wrapping_add(0x9E37_79B9);
    value ^= value >> 16;
    value = value.wrapping_mul(0x85EB_CA6B);
    value ^= value >> 13;
    [1, 2, 3, 4, 4][value as usize % 5]
}

fn writer_position(mut pose: usize) -> (usize, usize) {
    let mut line_number = 0;
    while pose > line_length(line_number) {
        pose -= line_length(line_number) + 1;
        line_number += 1;
    }
    (line_number, pose)
}

fn line_writer_frame(pose: usize) -> String {
    let (current_line, progress) = writer_position(pose);
    let feeding = progress == line_length(current_line);
    let history_end = if feeding {
        current_line + 1
    } else {
        current_line
    };
    let mut pixels = Vec::new();

    for row in 0..3 {
        let line_number = history_end as isize - 3 + row as isize;
        if line_number >= 0 {
            for x in 0..line_length(line_number as usize) {
                pixels.push((x, row));
            }
        }
    }

    if !feeding {
        for x in 0..=progress {
            pixels.push((x, 3));
        }
    }
    braille(&pixels)
}

fn throbber_frame(activity: AgentActivity, tick: usize) -> String {
    match activity {
        AgentActivity::ModelRequest => triangle_frame(tick),
        AgentActivity::LocalWork => line_writer_frame(tick),
    }
}

/// Build the throbber frame as a standalone styled line, for embedding in the
/// scrollable log content (so it scrolls out of view like any other line).
pub(super) fn throbber_line(app: &App) -> Line<'static> {
    let theme = &app.theme.status;
    let throbber_style = match app.agent_turn.activity {
        AgentActivity::ModelRequest => theme.idle.to_ratatui_style(),
        AgentActivity::LocalWork => theme
            .provider
            .to_ratatui_style()
            .remove_modifier(Modifier::ITALIC)
            .add_modifier(Modifier::DIM),
    };
    let frame = throbber_frame(app.agent_turn.activity, app.agent_turn.tick as usize);
    Line::from(Span::styled(frame, throbber_style))
}

pub(super) fn render_activity(f: &mut ratatui::Frame, area: Rect, app: &App) {
    let theme = &app.theme.status;
    let hint_style = theme.idle.to_ratatui_style();

    let mut spans: Vec<Span<'static>> = Vec::new();
    if let Some(cursor_idx) = app.step_back.cursor {
        if !spans.is_empty() {
            spans.push(Span::raw("  "));
        }
        let boundaries = app.step_boundaries();
        let total = boundaries.len();
        // How many boundaries are at or after the cursor (i.e. will be discarded)?
        let steps_back = boundaries.iter().filter(|&&i| i >= cursor_idx).count();
        let step_style = theme.cost.to_ratatui_style();
        spans.push(Span::styled(
            format!("[step back: {steps_back} of {total} — Enter to branch, Esc to cancel]"),
            step_style,
        ));
    }
    if app.log_view.full_output {
        if !spans.is_empty() {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled("[full output — Alt+O to toggle]", hint_style));
    }

    let line = if spans.is_empty() {
        Line::default()
    } else {
        Line::from(spans)
    };
    f.render_widget(Paragraph::new(line), area);
}

pub(super) fn render_provider_status(f: &mut ratatui::Frame, area: Rect, app: &App) {
    let status_text_style = app.theme.status.idle.to_ratatui_style();

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn triangle_is_two_cells_and_repeats_after_perimeter_period() {
        let first = triangle_frame(0);
        assert_eq!(first.chars().count(), 2);
        assert_eq!(first, triangle_frame(TRIANGLE_PERIOD));
        assert_ne!(first, triangle_frame(1));
    }

    #[test]
    fn line_writer_is_two_cells_and_advances_through_output_poses() {
        let frames: Vec<_> = (0..12).map(line_writer_frame).collect();
        assert!(frames.iter().all(|frame| frame.chars().count() == 2));
        assert!(frames.windows(2).any(|window| window[0] != window[1]));
    }

    #[test]
    fn activity_selects_the_expected_animation() {
        assert_eq!(
            throbber_frame(AgentActivity::ModelRequest, 0),
            triangle_frame(0)
        );
        assert_eq!(
            throbber_frame(AgentActivity::LocalWork, 0),
            line_writer_frame(0)
        );
    }
}
