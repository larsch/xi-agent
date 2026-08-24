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
#[cfg(windows)]
use std::io::Write;
use std::io::{self, ErrorKind};

// Crossterm PR #1030 enables VT input on Windows, which makes Windows
// Terminal/ConPTY deliver mouse input as VT sequences instead of Win32
// MOUSE_EVENT records (https://github.com/microsoft/terminal/issues/15296).
// Crossterm's Windows EnableMouseCapture still enables
// only the Win32 path, so request VT mouse tracking as well. Keep the Win32
// capture command for terminals such as conhost that continue to use it.
#[cfg(windows)]
const ENABLE_VT_MOUSE_CAPTURE: &[u8] = b"\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1015h\x1b[?1006h";
#[cfg(windows)]
const DISABLE_VT_MOUSE_CAPTURE: &[u8] = b"\x1b[?1006l\x1b[?1015l\x1b[?1003l\x1b[?1002l\x1b[?1000l";

#[cfg(windows)]
fn set_vt_mouse_capture(writer: &mut impl Write, enabled: bool) -> io::Result<()> {
    let sequence = if enabled {
        ENABLE_VT_MOUSE_CAPTURE
    } else {
        DISABLE_VT_MOUSE_CAPTURE
    };
    writer.write_all(sequence)?;
    writer.flush()
}

/// Install a "last resort" signal guard that forces immediate exit if a
/// second termination signal arrives while the primary cleanup path is
/// already running. Prevents a hung process when terminal restoration stalls.
///
/// # Safety
///
/// Installs bare `libc::signal` handlers. Call only after the tokio signal
/// streams have been dropped (post-event-loop).
#[cfg(unix)]
pub(crate) fn install_termination_guard() {
    unsafe {
        extern "C" fn force_exit(_sig: i32) {
            unsafe {
                libc::_exit(1);
            }
        }
        libc::signal(libc::SIGTERM, force_exit as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, force_exit as *const () as libc::sighandler_t);
        libc::signal(libc::SIGHUP, force_exit as *const () as libc::sighandler_t);
        libc::signal(libc::SIGQUIT, force_exit as *const () as libc::sighandler_t);
    }
}

pub(crate) type Backend = CrosstermBackend<io::Stdout>;

pub(crate) fn init_terminal(window_title: &str) -> io::Result<(Terminal<Backend>, bool)> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        SetTitle(window_title),
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )?;
    #[cfg(windows)]
    set_vt_mouse_capture(&mut stdout, true)?;

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
    terminal: &mut Terminal<Backend>,
    keyboard_enhancements_enabled: bool,
) -> io::Result<()> {
    disable_raw_mode()?;
    if keyboard_enhancements_enabled {
        execute!(terminal.backend_mut(), PopKeyboardEnhancementFlags)?;
    }
    #[cfg(windows)]
    set_vt_mouse_capture(terminal.backend_mut(), false)?;
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
    terminal: Terminal<Backend>,
    keyboard_enhancements_enabled: &mut bool,
    window_title: &str,
) -> io::Result<Terminal<Backend>> {
    drop(terminal);
    let (new_terminal, new_kbe) = init_terminal(window_title)?;
    *keyboard_enhancements_enabled = new_kbe;
    Ok(new_terminal)
}

#[cfg(unix)]
pub(crate) fn suspend_interactive_ui(
    terminal: &mut Terminal<Backend>,
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

    // Tokio installs a SIGTSTP handler while the event loop is running. Restore
    // the native default action before re-raising the signal so the kernel stops
    // us as an interactive job, allowing the shell to resume us with `fg`.
    // SAFETY: `action` is initialized with a valid empty signal mask and the
    // default SIGTSTP disposition before it is passed to libc.
    let rc = unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = libc::SIG_DFL;
        libc::sigemptyset(&mut action.sa_mask);
        libc::sigaction(libc::SIGTSTP, &action, std::ptr::null_mut())
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }

    let pid = std::process::id() as libc::pid_t;
    // SAFETY: sends SIGTSTP to the current process so the parent shell can resume it with fg.
    let rc = unsafe { libc::kill(pid, libc::SIGTSTP) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }

    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn suspend_interactive_ui(
    _terminal: &mut Terminal<Backend>,
    _keyboard_enhancements_enabled: bool,
) -> io::Result<()> {
    Ok(())
}
