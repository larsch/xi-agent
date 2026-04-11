//! Platform-specific process spawning helpers.

/// Extension trait for detaching a command from the controlling terminal.
///
/// Call [`detach_from_tty`](DetachFromTty::detach_from_tty) on a
/// `std::process::Command` or `tokio::process::Command` before spawning to
/// ensure the child runs in its own session with no controlling terminal.
/// This prevents it from reading `/dev/tty`, seeing `isatty() == true`,
/// stealing the foreground process group, or receiving terminal signals.
///
/// On non-Unix platforms this is a no-op.
pub trait DetachFromTty {
    fn detach_from_tty(&mut self) -> &mut Self;
}

#[cfg(unix)]
impl DetachFromTty for std::process::Command {
    fn detach_from_tty(&mut self) -> &mut Self {
        use std::os::unix::process::CommandExt;
        // SAFETY: setsid() is async-signal-safe and has no Rust invariants to
        // uphold. EPERM (already a session leader) is silently ignored — it
        // cannot happen in normal operation.
        unsafe {
            self.pre_exec(|| {
                libc::setsid();
                Ok(())
            })
        }
    }
}

#[cfg(unix)]
impl DetachFromTty for tokio::process::Command {
    fn detach_from_tty(&mut self) -> &mut Self {
        // tokio::process::Command::pre_exec is an inherent method on Unix;
        // no trait import needed.
        // SAFETY: same as above.
        unsafe {
            self.pre_exec(|| {
                libc::setsid();
                Ok(())
            })
        }
    }
}

#[cfg(not(unix))]
impl DetachFromTty for std::process::Command {
    fn detach_from_tty(&mut self) -> &mut Self {
        self
    }
}

#[cfg(not(unix))]
impl DetachFromTty for tokio::process::Command {
    fn detach_from_tty(&mut self) -> &mut Self {
        self
    }
}
