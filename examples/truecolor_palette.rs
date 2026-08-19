//! Displays a 24-bit RGB palette using Ratatui's Crossterm backend.
//!
//! Run with:
//!     cargo run --example truecolor_palette
//!
//! Every colored cell is styled with `Color::Rgb(r, g, b)`. Crossterm therefore
//! writes SGR `48;2;r;g;b` truecolor escape sequences on ANSI-capable terminals.

use std::io::{self, stdout};

use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    text::Line,
    widgets::{Block, Borders, Paragraph},
};

fn main() -> io::Result<()> {
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen)?;

    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;
    let result = run(&mut terminal);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

fn run(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> io::Result<()> {
    loop {
        terminal.draw(draw)?;

        if let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
            && matches!(key.code, KeyCode::Esc | KeyCode::Char('q'))
        {
            return Ok(());
        }
    }
}

fn draw(frame: &mut Frame) {
    let [header, palette, footer] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .areas(frame.area());

    frame.render_widget(
        Paragraph::new(Line::from(
            "24-bit RGB — red increases →, green increases ↓, blue varies within each cell",
        ))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Truecolor palette "),
        ),
        header,
    );

    draw_palette(frame, palette);
    frame.render_widget(Paragraph::new("Press q or Esc to exit"), footer);
}

fn draw_palette(frame: &mut Frame, area: Rect) {
    let buffer = frame.buffer_mut();
    if area.width == 0 || area.height == 0 {
        return;
    }

    for y in 0..area.height {
        for x in 0..area.width {
            let r = scale(x, area.width);
            let g = scale(y, area.height);
            // The checker/ramp component makes adjacent cells differ in all
            // channels and produces far more than a 256-color palette.
            let b = scale(x.wrapping_add(y * 3) % area.width, area.width);
            let style = Style::default().bg(Color::Rgb(r, g, b));
            buffer[(area.x + x, area.y + y)]
                .set_char(' ')
                .set_style(style);
        }
    }
}

fn scale(position: u16, length: u16) -> u8 {
    if length <= 1 {
        return 0;
    }
    ((u32::from(position) * 255) / u32::from(length - 1)) as u8
}
