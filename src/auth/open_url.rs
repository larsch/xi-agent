#[cfg(target_os = "windows")]
use std::os::windows::ffi::OsStrExt;

#[cfg(target_os = "windows")]
fn to_wide(s: &str) -> Vec<u16> {
    std::ffi::OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// Try to open `url` in the user's default browser.
///
/// Uses `open` on macOS, `ShellExecuteW` on Windows, and `xdg-open` on other
/// platforms. Returns an error if the helper cannot be launched; the caller
/// decides how to surface that to the user.
pub fn open_url(url: &str) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(url)
            .stdin(std::process::Stdio::null())
            .spawn()?;
    }
    #[cfg(target_os = "windows")]
    {
        let operation = to_wide("open");
        let file = to_wide(url);
        let result = unsafe {
            use windows::core::PCWSTR;

            windows::Win32::UI::Shell::ShellExecuteW(
                None,
                PCWSTR(operation.as_ptr()),
                PCWSTR(file.as_ptr()),
                None,
                None,
                windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL,
            )
        };
        if result.0 as isize <= 32 {
            return Err(std::io::Error::other(format!(
                "ShellExecuteW failed with code {result:?}"
            )));
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        std::process::Command::new("xdg-open")
            .arg(url)
            .stdin(std::process::Stdio::null())
            .spawn()?;
    }
    Ok(())
}
