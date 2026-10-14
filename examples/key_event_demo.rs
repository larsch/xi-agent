//! Print crossterm key events as they are received.
//!
//! Run with:
//!     cargo run --example key_event_demo
//!
//! Press Esc or Ctrl+C to exit. This is useful for inspecting how a terminal
//! and tmux encode control-key combinations.

use std::io;

use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    terminal::{disable_raw_mode, enable_raw_mode},
};

fn main() -> io::Result<()> {
    enable_raw_mode()?;
    let result = run();
    disable_raw_mode()?;
    result
}

fn run() -> io::Result<()> {
    println!("Press keys; Esc or Ctrl+C exits.");

    loop {
        let event = event::read()?;
        println!("{event:?}");

        if let Event::Key(key) = event
            && key.kind == KeyEventKind::Press
            && (key.code == KeyCode::Esc
                || (key.code == KeyCode::Char('c')
                    && key
                        .modifiers
                        .contains(crossterm::event::KeyModifiers::CONTROL)))
        {
            return Ok(());
        }
    }
}
