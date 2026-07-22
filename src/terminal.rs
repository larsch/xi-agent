use crossterm::{
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{
        EnterAlternateScreen, LeaveAlternateScreen, SetTitle, disable_raw_mode, enable_raw_mode,
    },
};
use ratatui::{Terminal, backend::CrosstermBackend};
use std::io::{self, ErrorKind};

pub(crate) fn init_terminal(
    window_title: &str,
) -> io::Result<(Terminal<CrosstermBackend<io::Stdout>>, bool)> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        SetTitle(window_title),
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )?;

    let mut keyboard_enhancements_enabled = false;
    match execute!(
        stdout,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES),
    ) {
        Ok(()) => keyboard_enhancements_enabled = true,
        Err(e) if e.kind() == ErrorKind::Unsupported => {
            log::debug!(
                "keyboard progressive enhancement unsupported on this terminal; continuing without it"
            );
        }
        Err(e) => return Err(e),
    }

    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    Ok((terminal, keyboard_enhancements_enabled))
}

pub(crate) fn shutdown_terminal(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    keyboard_enhancements_enabled: bool,
) -> io::Result<()> {
    disable_raw_mode()?;
    if keyboard_enhancements_enabled {
        execute!(terminal.backend_mut(), PopKeyboardEnhancementFlags)?;
    }
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        DisableBracketedPaste
    )?;
    terminal.show_cursor()?;
    Ok(())
}

pub(crate) fn recreate_terminal(
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
    keyboard_enhancements_enabled: &mut bool,
    window_title: &str,
) -> io::Result<Terminal<CrosstermBackend<io::Stdout>>> {
    drop(terminal);
    let (new_terminal, new_kbe) = init_terminal(window_title)?;
    *keyboard_enhancements_enabled = new_kbe;
    Ok(new_terminal)
}

#[cfg(unix)]
pub(crate) fn suspend_interactive_ui(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    keyboard_enhancements_enabled: bool,
) -> io::Result<()> {
    use crossterm::cursor::Show;

    disable_raw_mode()?;
    if keyboard_enhancements_enabled {
        execute!(terminal.backend_mut(), PopKeyboardEnhancementFlags)?;
    }
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        DisableBracketedPaste,
        Show
    )?;

    let pid = std::process::id() as i32;
    // SAFETY: sends SIGTSTP to the current process so the parent shell can resume it with fg.
    let rc = unsafe { libc::kill(pid, libc::SIGTSTP) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }

    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn suspend_interactive_ui(
    _terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    _keyboard_enhancements_enabled: bool,
) -> io::Result<()> {
    Ok(())
}
