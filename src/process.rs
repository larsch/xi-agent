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

/// Windows-specific controls for an already-spawned agent subprocess.
///
/// This module deliberately contains only OS process control. Timeout policy,
/// agent-loop state, and any future supervision decisions belong above it.
#[cfg(windows)]
pub(crate) mod control {
    use std::collections::{HashMap, HashSet};

    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Console::{CTRL_BREAK_EVENT, GenerateConsoleCtrlEvent};
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess};

    /// Ask every console process in the isolated tool group to stop gracefully.
    pub(crate) fn request_interrupt(process_group_id: u32) {
        // SAFETY: the PID comes from the child process spawned by this tool.
        // CREATE_NEW_PROCESS_GROUP makes it a valid, isolated group identifier.
        if let Err(error) = unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, process_group_id) }
        {
            log::debug!(
                "failed to send CTRL_BREAK_EVENT to process group {process_group_id}: {error}"
            );
        }
    }

    /// Terminate a root process and all currently enumerated descendants,
    /// deepest first so children cannot survive through their parent.
    pub(crate) fn force_kill_tree(root_pid: u32) {
        let snapshot = match unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) } {
            Ok(snapshot) => snapshot,
            Err(error) => {
                log::debug!("failed to snapshot processes while terminating {root_pid}: {error}");
                return;
            }
        };

        let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        // SAFETY: `entry` is initialized with the required size and remains
        // valid throughout enumeration; `snapshot` is owned until scope exit.
        let first = unsafe { Process32FirstW(snapshot, &mut entry) };
        if first.is_ok() {
            loop {
                children
                    .entry(entry.th32ParentProcessID)
                    .or_default()
                    .push(entry.th32ProcessID);
                // SAFETY: same initialized entry buffer as Process32FirstW.
                if unsafe { Process32NextW(snapshot, &mut entry) }.is_err() {
                    break;
                }
            }
        }
        // SAFETY: snapshot is the owned handle returned by CreateToolhelp32Snapshot.
        let _ = unsafe { CloseHandle(snapshot) };

        let mut descendants = Vec::new();
        let mut visited = HashSet::new();
        collect_descendants(root_pid, &children, &mut visited, &mut descendants);
        descendants.push(root_pid);
        for pid in descendants {
            // SAFETY: PROCESS_TERMINATE requests only the right needed for a
            // process ID obtained from the operating-system process snapshot.
            match unsafe { OpenProcess(PROCESS_TERMINATE, false, pid) } {
                Ok(process) => {
                    // SAFETY: `process` is a valid owned process handle.
                    if let Err(error) = unsafe { TerminateProcess(process, 1) } {
                        log::debug!("failed to terminate process {pid}: {error}");
                    }
                    // SAFETY: `process` is the owned handle returned by OpenProcess.
                    let _ = unsafe { CloseHandle(process) };
                }
                Err(error) => log::debug!("failed to open process {pid} for termination: {error}"),
            }
        }
    }

    fn collect_descendants(
        pid: u32,
        children: &HashMap<u32, Vec<u32>>,
        visited: &mut HashSet<u32>,
        descendants: &mut Vec<u32>,
    ) {
        if !visited.insert(pid) {
            return;
        }
        if let Some(child_pids) = children.get(&pid) {
            for &child_pid in child_pids {
                collect_descendants(child_pid, children, visited, descendants);
                descendants.push(child_pid);
            }
        }
    }
}

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
