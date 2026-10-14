//! Windows client transport to a WSL Unix socket via wsl.exe and socat.

use std::process::Stdio;
use tokio::io::DuplexStream;

pub(super) struct Target {
    pub distro: String,
    pub workdir: String,
}

pub(super) fn parse_target(cwd: &str) -> Result<Option<Target>, String> {
    let path = if let Some(rest) = cwd.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else {
        cwd.to_owned()
    };
    let path = path.replace('/', "\\");
    let Some(rest) = path
        .get(..7)
        .filter(|prefix| prefix.eq_ignore_ascii_case(r"\\wsl$\"))
        .map(|_| &path[7..])
        .or_else(|| {
            path.get(..16)
                .filter(|prefix| prefix.eq_ignore_ascii_case(r"\\wsl.localhost\"))
                .map(|_| &path[16..])
        })
    else {
        return Ok(None);
    };
    let (distro, tail) = rest
        .split_once('\\')
        .ok_or("invalid_path: WSL UNC path needs a distro and an absolute worktree path")?;
    if distro.is_empty()
        || tail.is_empty()
        || tail
            .split('\\')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err("invalid_path: WSL UNC path must name a distro and a worktree without . or .. components".into());
    }
    Ok(Some(Target {
        distro: distro.to_owned(),
        workdir: format!("/{}", tail.replace('\\', "/")),
    }))
}

// Discover the socket *inside* the selected distro: canonicalization and
// XDG_RUNTIME_DIR are Linux-side facts, not properties of the Windows UNC path.
const RELAY: &str = r#"
set -eu
workdir=$(readlink -f -- "$1")
[ -d "$workdir" ] || { echo 'WSL worktree does not exist' >&2; exit 1; }
uid=$(id -u)
hash=$(printf 'xi-session-ipc-v1\0%s' "$workdir" | sha256sum)
hash=${hash%% *}
name=s-$hash
fallback=/tmp/xi-$uid
runtime=$fallback
if [ -n "${XDG_RUNTIME_DIR:-}" ] && [ -d "$XDG_RUNTIME_DIR/xi" ]; then
    runtime=$XDG_RUNTIME_DIR/xi
fi
socket=$runtime/$name
if [ "${#socket}" -gt 100 ] || [ ! -S "$socket" ]; then
    socket=$fallback/$name
fi
if [ ! -S "$socket" ]; then socket=$workdir/.xi/xi.sock; fi
[ -S "$socket" ] || { echo 'No xi session socket found for worktree' >&2; exit 1; }
exec socat - "UNIX-CONNECT:$socket"
"#;

pub(super) async fn connect(target: Target) -> Result<DuplexStream, String> {
    let mut child = tokio::process::Command::new("wsl.exe")
        .args(["--distribution", &target.distro, "--exec", "sh", "-c", RELAY, "sh", &target.workdir])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("connection_error: cannot start WSL relay (requires wsl.exe and socat in {}): {error}", target.distro))?;
    let mut stdin = child.stdin.take().expect("piped stdin");
    let mut stdout = child.stdout.take().expect("piped stdout");
    let (client, relay) = tokio::io::duplex(64 * 1024);
    let (mut read, mut write) = tokio::io::split(relay);
    tokio::spawn(async move {
        // If either side closes, tear down wsl.exe as well. An agent may
        // finish using a connection without ever closing its other half.
        tokio::select! {
            _ = tokio::io::copy(&mut read, &mut stdin) => {},
            _ = tokio::io::copy(&mut stdout, &mut write) => {},
            _ = child.wait() => {},
        }
        // Drop the child to kill it when the controller drops the stream.
    });
    Ok(client)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_wsl_unc_without_treating_normal_windows_paths_as_wsl() {
        let target = parse_target(r"\\wsl$\Ubuntu\home\user\repo")
            .unwrap()
            .unwrap();
        assert_eq!(target.distro, "Ubuntu");
        assert_eq!(target.workdir, "/home/user/repo");
        assert!(parse_target(r"\\wsl$\Ubuntu\home\..\repo").is_err());
        assert!(parse_target(r"C:\repo").unwrap().is_none());
        assert_eq!(
            parse_target(r"\\?\UNC\wsl$\Ubuntu\home\user\repo")
                .unwrap()
                .unwrap()
                .workdir,
            "/home/user/repo"
        );
        assert_eq!(
            parse_target(r"\\WSL$\Ubuntu\home\user\repo")
                .unwrap()
                .unwrap()
                .workdir,
            "/home/user/repo"
        );
        assert_eq!(
            parse_target(r"\\wsl.localhost\Debian\repo")
                .unwrap()
                .unwrap()
                .workdir,
            "/repo"
        );
    }
}
