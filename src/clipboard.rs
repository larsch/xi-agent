use crossterm::ExecutableCommand;
use std::io::Write;

/// Copy `text` to the terminal host clipboard via OSC 52.
///
/// Requires `set -g set-clipboard on` in `.tmux.conf` when running
/// inside tmux.
pub fn set_clipboard(text: &str) -> Result<(), String> {
    std::io::stdout()
        .execute(crossterm::clipboard::CopyToClipboard::to_clipboard_from(
            text,
        ))
        .map_err(|e| e.to_string())?;
    std::io::stdout().flush().map_err(|e| e.to_string())?;
    Ok(())
}
