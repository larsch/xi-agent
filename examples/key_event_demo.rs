//! Print crossterm key events as they are received.
//!
//! Run with:
//!     cargo run --example key_event_demo
//!
//! Add `-- --bracketed-paste` to enable xi's Windows VT-input path.
//!
//! Press Esc or Ctrl+C to exit. This is useful for inspecting how a terminal
//! and tmux encode control-key combinations.

use std::io;

use crossterm::{
    event::{self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode},
};

fn main() -> io::Result<()> {
    let bracketed_paste = std::env::args().any(|arg| arg == "--bracketed-paste");
    enable_raw_mode()?;
    let result = (|| {
        if bracketed_paste {
            execute!(io::stdout(), EnableBracketedPaste)?;
        }
        run()
    })();
    let paste_cleanup = if bracketed_paste {
        execute!(io::stdout(), DisableBracketedPaste)
    } else {
        Ok(())
    };
    let raw_cleanup = disable_raw_mode();
    result.and(paste_cleanup).and(raw_cleanup)
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
