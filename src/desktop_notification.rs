//! Best-effort desktop notifications for agent-loop halt points.

use std::io::Write;

const MAX_CONTENT_CHARS: usize = 200;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum NotificationBackend {
    #[default]
    Native,
    Terminal,
}

/// Keep COM alive for the lifetime of the cached WinRT notification factories.
#[cfg(windows)]
pub(crate) fn pin_com_mta() {
    use windows::Win32::System::Com::CoIncrementMTAUsage;

    // An activation can borrow a transient MTA and bypass windows-core's own
    // initialization fallback. When that MTA ends, COM can unload msxml6.dll
    // while windows-core still holds its cached XML activation factory pointer.
    // SAFETY: this acquires a process-wide MTA reference without changing the
    // calling thread's apartment. The cookie has no automatic cleanup.
    // ponytail: deliberately retain the reference until process exit, matching
    // the static factory cache; revisit if windows-core owns that lifetime.
    match unsafe { CoIncrementMTAUsage() } {
        Ok(_cookie) => log::debug!(target: "desktop_notification", "COM MTA pinned"),
        Err(error) => {
            log::debug!(target: "desktop_notification", "COM MTA pin failed: {error:?}");
        }
    }
}

fn notification_title(cwd: &std::path::Path) -> String {
    let project = cwd
        .file_name()
        .filter(|name| !name.is_empty())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| cwd.to_string_lossy().into_owned());
    format!("xi in {project}")
}

fn truncate_content(content: &str) -> String {
    if content.chars().count() <= MAX_CONTENT_CHARS {
        return content.to_string();
    }

    let mut truncated: String = content.chars().take(MAX_CONTENT_CHARS - 1).collect();
    truncated.push('…');
    truncated
}

fn should_notify(terminal_focused: bool) -> bool {
    !terminal_focused
}

fn sanitize_terminal_text(text: &str, title: bool) -> String {
    text.chars()
        .map(|ch| {
            // ESC, BEL and C1 controls can terminate or inject terminal sequences.
            // The title also cannot contain the OSC 777 field separator.
            if ch.is_control() || (title && ch == ';') {
                ' '
            } else {
                ch
            }
        })
        .collect()
}

fn write_terminal_notification(
    writer: &mut impl Write,
    title: &str,
    body: &str,
) -> std::io::Result<()> {
    let title = sanitize_terminal_text(title, true);
    let body = sanitize_terminal_text(body, false);
    write!(writer, "\x1b]777;notify;{title};{body}\x1b\\")?;
    writer.flush()
}

/// Show a best-effort desktop notification without failing the agent run.
///
/// Notifications are suppressed while the terminal has focus. Native delivery
/// runs off-thread; terminal delivery writes a short OSC 777 sequence directly.
pub(crate) fn notify_agent_loop_halt(
    content: &str,
    terminal_focused: bool,
    backend: NotificationBackend,
) {
    if !should_notify(terminal_focused) {
        return;
    }

    #[cfg(not(test))]
    {
        let cwd = std::env::current_dir().unwrap_or_default();
        let title = notification_title(&cwd);
        let body = truncate_content(content);
        match backend {
            NotificationBackend::Native => {
                std::thread::spawn(move || {
                    if let Err(error) = notify_rust::Notification::new()
                        .summary(&title)
                        .body(&body)
                        .show()
                    {
                        log::debug!("failed to show desktop notification: {error}");
                    }
                });
            }
            NotificationBackend::Terminal => {
                use std::io::IsTerminal;

                let stdout = std::io::stdout();
                if !stdout.is_terminal() {
                    log::debug!("skipped terminal notification: stdout is not a terminal");
                    return;
                }
                // Keep the entire sequence and flush under one lock so UI output
                // cannot interleave with it. OSC 777 provides no delivery receipt.
                // ponytail: backend selection is explicit; add automatic fallback
                // only when terminal support AND permission can be established.
                if let Err(error) = write_terminal_notification(&mut stdout.lock(), &title, &body) {
                    log::debug!("failed to write terminal notification: {error}");
                }
            }
        }
    }

    #[cfg(test)]
    let _ = (content, backend);
}

fn write_terminal_bell(writer: &mut impl Write) -> std::io::Result<()> {
    writer.write_all(b"\x07")?;
    writer.flush()
}

/// Request terminal attention independently of desktop notification delivery.
/// The terminal's bell policy decides whether this marks a tab, flashes or sounds.
pub(crate) fn ring_terminal_bell(terminal_focused: bool) {
    if !should_notify(terminal_focused) {
        return;
    }

    #[cfg(not(test))]
    {
        use std::io::IsTerminal;

        let stdout = std::io::stdout();
        if !stdout.is_terminal() {
            log::debug!("skipped terminal bell: stdout is not a terminal");
            return;
        }
        if let Err(error) = write_terminal_bell(&mut stdout.lock()) {
            log::debug!("failed to write terminal bell: {error}");
        }
    }

    #[cfg(test)]
    let _ = terminal_focused;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_bell_writes_one_bel_and_flushes() {
        #[derive(Default)]
        struct RecordingWriter {
            bytes: Vec<u8>,
            flushed: bool,
        }
        impl Write for RecordingWriter {
            fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
                self.bytes.extend_from_slice(data);
                Ok(data.len())
            }

            fn flush(&mut self) -> std::io::Result<()> {
                self.flushed = true;
                Ok(())
            }
        }

        let mut writer = RecordingWriter::default();
        write_terminal_bell(&mut writer).unwrap();
        assert_eq!(writer.bytes, b"\x07");
        assert!(writer.flushed);
    }

    #[test]
    fn terminal_notification_uses_osc777_and_preserves_unicode() {
        let mut output = Vec::new();
        write_terminal_notification(&mut output, "xi in café", "Done 🎉; next step?").unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "\x1b]777;notify;xi in café;Done 🎉; next step?\x1b\\"
        );
    }

    #[test]
    fn terminal_notification_sanitizes_controls_and_title_separator() {
        let mut output = Vec::new();
        write_terminal_notification(
            &mut output,
            "repo;\x1b\x07\u{009c}",
            "line\r\n\t\0\x1b]52;clipboard\x07\u{009b}\u{009c}",
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "\x1b]777;notify;repo    ;line     ]52;clipboard   \x1b\\"
        );
    }

    #[test]
    fn terminal_notification_reports_write_and_flush_errors() {
        struct FailingWriter {
            fail_write: bool,
        }
        impl Write for FailingWriter {
            fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
                if self.fail_write {
                    Err(std::io::Error::other("write failed"))
                } else {
                    Ok(data.len())
                }
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Err(std::io::Error::other("flush failed"))
            }
        }

        for (fail_write, expected) in [(true, "write failed"), (false, "flush failed")] {
            let error =
                write_terminal_notification(&mut FailingWriter { fail_write }, "title", "body")
                    .unwrap_err();
            assert_eq!(error.to_string(), expected);
        }
    }

    #[test]
    fn title_uses_last_directory_name() {
        assert_eq!(
            notification_title(std::path::Path::new("/work/projects/xi-agent")),
            "xi in xi-agent"
        );
    }

    #[test]
    fn notifications_are_only_shown_while_unfocused() {
        assert!(!should_notify(true));
        assert!(should_notify(false));
    }

    #[test]
    fn short_content_is_unchanged() {
        assert_eq!(truncate_content("What should I do?"), "What should I do?");
    }

    #[test]
    fn long_content_is_limited_to_200_characters() {
        let content = "é".repeat(201);
        let truncated = truncate_content(&content);
        assert_eq!(truncated.chars().count(), MAX_CONTENT_CHARS);
        assert!(truncated.ends_with('…'));
    }
}
