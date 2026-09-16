//! Platform-specific process spawning helpers.

/// Extension trait for preventing a child from using the agent's terminal.
///
/// Call [`detach_from_tty`](DetachFromTty::detach_from_tty) on a
/// `std::process::Command` or `tokio::process::Command` before spawning.
/// On Unix, this creates a new session with no controlling terminal. On
/// Windows, this creates an isolated process group so cancellation can target
/// the tool with CTRL_BREAK_EVENT without affecting the agent's console.
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

#[cfg(windows)]
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

/// Windows process-creation flags used for agent subprocesses. A separate
/// process group lets HardAbort target only the tool with CTRL_BREAK_EVENT;
/// ForceKill can then terminate its full descendant tree without affecting xi.
///
/// Do not use CREATE_NO_WINDOW here: console control events require the child
/// to remain attached to a console.
#[cfg(windows)]
pub const AGENT_SUBPROCESS_CREATION_FLAGS: u32 = CREATE_NEW_PROCESS_GROUP;

#[cfg(windows)]
impl DetachFromTty for std::process::Command {
    fn detach_from_tty(&mut self) -> &mut Self {
        use std::os::windows::process::CommandExt;
        self.creation_flags(AGENT_SUBPROCESS_CREATION_FLAGS)
    }
}

#[cfg(windows)]
impl DetachFromTty for tokio::process::Command {
    fn detach_from_tty(&mut self) -> &mut Self {
        use std::os::windows::process::CommandExt;
        self.as_std_mut()
            .creation_flags(AGENT_SUBPROCESS_CREATION_FLAGS);
        self
    }
}

#[cfg(not(any(unix, windows)))]
impl DetachFromTty for std::process::Command {
    fn detach_from_tty(&mut self) -> &mut Self {
        self
    }
}

#[cfg(not(any(unix, windows)))]
impl DetachFromTty for tokio::process::Command {
    fn detach_from_tty(&mut self) -> &mut Self {
        self
    }
}
