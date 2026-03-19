/// Try to open `url` in the user's default browser.
///
/// Uses `open` on macOS, `cmd /C start` on Windows, and `xdg-open` on other
/// platforms.  Returns an error if the helper binary cannot be spawned; the
/// caller decides how to surface that to the user.
pub fn open_url(url: &str) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg(url).spawn()?;
    }
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn()?;
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        std::process::Command::new("xdg-open").arg(url).spawn()?;
    }
    Ok(())
}
