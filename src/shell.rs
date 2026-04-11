use std::process::Stdio;

use crate::process::DetachFromTty;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellKind {
    Bash,
    #[cfg(windows)]
    Cmd,
    #[cfg(windows)]
    PowerShell,
}

impl ShellKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Bash => "bash",
            #[cfg(windows)]
            Self::Cmd => "cmd",
            #[cfg(windows)]
            Self::PowerShell => "PS",
        }
    }

    pub fn prompt_char(self) -> char {
        match self {
            Self::Bash => '$',
            #[cfg(windows)]
            Self::Cmd | Self::PowerShell => '>',
        }
    }
}

#[derive(Debug, Clone)]
pub struct ShellRunResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

pub fn discover_available_shells() -> Vec<ShellKind> {
    #[cfg(windows)]
    {
        vec![ShellKind::PowerShell, ShellKind::Cmd]
    }

    #[cfg(not(windows))]
    {
        vec![ShellKind::Bash]
    }
}

pub fn run_shell_command_blocking(shell: ShellKind, cwd: &str, command: &str) -> ShellRunResult {
    let mut cmd = match shell {
        ShellKind::Bash => {
            let mut c = std::process::Command::new("sh");
            c.arg("-c").arg(command);
            c
        }
        #[cfg(windows)]
        ShellKind::Cmd => {
            let mut c = std::process::Command::new("cmd.exe");
            c.arg("/D").arg("/S").arg("/C").arg(command);
            c
        }
        #[cfg(windows)]
        ShellKind::PowerShell => {
            let mut c = std::process::Command::new("powershell.exe");
            c.arg("-NoProfile").arg("-Command").arg(command);
            c
        }
    };

    cmd.current_dir(cwd);
    cmd.stdin(Stdio::null());
    cmd.detach_from_tty();

    match cmd.output() {
        Ok(output) => ShellRunResult {
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            exit_code: output.status.code().unwrap_or(-1),
        },
        Err(e) => ShellRunResult {
            stdout: String::new(),
            stderr: format!("Failed to run {}: {}", shell.label(), e),
            exit_code: -1,
        },
    }
}
