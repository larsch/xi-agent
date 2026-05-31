//! [`SubprocessCommand`] — a `tokio::process::Command`-shaped builder that
//! owns the full subprocess lifecycle for agent tool calls.
//!
//! Usage:
//! ```ignore
//! SubprocessCommand::new("sh")
//!     .arg("-c")
//!     .arg(&command)
//!     .run(ctx)
//!     .await
//! ```
//!
//! The builder configures subprocess hygiene (`TERM=dumb`, `NO_COLOR=1`,
//! `kill_on_drop`, `detach_from_tty`, piped stdio) and then:
//!
//! 1. Spawns the process.
//! 2. Optionally writes `stdin_data` and closes stdin.
//! 3. Drains stdout/stderr concurrently (platform-specific strategy).
//! 4. Forwards raw chunks as [`AgentEvent::ToolOutputChunk`] when a live
//!    sender is available via [`ToolCallContext`].
//! 5. Applies [`apply_terminal_render`] to the final accumulated output.
//! 6. Strips trailing whitespace.
//! 7. Merges stdout+stderr, appends `exit N` for non-zero exits, truncates,
//!    and returns a [`ToolResult`].

use std::collections::HashMap;
use std::process::Stdio;

use super::terminal::apply_terminal_render;
use super::truncate::truncate_tail;
use crate::agent::types::{AgentEvent, ToolCallContext, ToolResult};
use crate::app_event::AppEvent;
use crate::process::DetachFromTty;

// ── SubprocessCommand ─────────────────────────────────────────────────────────

/// Builder for running a subprocess as an agent tool call.
///
/// Mirrors the `tokio::process::Command` API for the fields tools actually
/// need, while handling all subprocess hygiene and output collection
/// internally.
pub struct SubprocessCommand {
    program: String,
    args: Vec<String>,
    #[cfg(target_os = "windows")]
    raw_args: Vec<String>,
    envs: HashMap<String, String>,
    current_dir: Option<String>,
    stdin_data: Option<Vec<u8>>,
    /// When `true`, a non-zero exit code promotes the result to `is_error`.
    error_on_nonzero: bool,
}

impl SubprocessCommand {
    /// Create a new builder for `program`.
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            #[cfg(target_os = "windows")]
            raw_args: Vec::new(),
            envs: HashMap::new(),
            current_dir: None,
            stdin_data: None,
            error_on_nonzero: false,
        }
    }

    /// Append a single argument.
    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Append multiple arguments.
    #[cfg(not(target_os = "windows"))]
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    /// Append a raw argument using platform-specific native command-line
    /// handling.
    ///
    /// Maps to `tokio::process::Command::raw_arg`, which is needed for
    /// commands like `cmd.exe /S /C` where quoting semantics must be
    /// preserved exactly.
    #[cfg(target_os = "windows")]
    pub fn raw_arg(mut self, arg: impl Into<String>) -> Self {
        self.raw_args.push(arg.into());
        self
    }

    /// Set an environment variable (merged with the inherited environment).
    #[cfg(not(target_os = "windows"))]
    pub fn env(mut self, key: impl Into<String>, val: impl Into<String>) -> Self {
        self.envs.insert(key.into(), val.into());
        self
    }

    /// Set the working directory for the child process.
    #[cfg(not(target_os = "windows"))]
    pub fn current_dir(mut self, dir: impl Into<String>) -> Self {
        self.current_dir = Some(dir.into());
        self
    }

    /// Provide data to write to the child's stdin before closing it.
    pub fn stdin_data(mut self, data: impl Into<Vec<u8>>) -> Self {
        self.stdin_data = Some(data.into());
        self
    }

    /// Promote a non-zero exit code to `is_error = true` in the result.
    ///
    /// Use this for tools whose protocol treats non-zero exit as a tool
    /// failure (e.g. custom tools), rather than a command that happened to
    /// exit with an error code.
    pub fn error_on_nonzero_exit(mut self) -> Self {
        self.error_on_nonzero = true;
        self
    }

    /// Spawn the process, collect output, and return a [`ToolResult`].
    ///
    /// Live output chunks are forwarded via `ctx.tx` as
    /// [`AgentEvent::ToolOutputChunk`] if a sender is present.
    pub async fn run(self, ctx: ToolCallContext) -> ToolResult {
        let mut cmd = tokio::process::Command::new(&self.program);
        cmd.args(&self.args)
            .stdin(if self.stdin_data.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .detach_from_tty()
            .env("TERM", "dumb")
            .env("NO_COLOR", "1");

        for (k, v) in &self.envs {
            cmd.env(k, v);
        }

        #[cfg(target_os = "windows")]
        for raw_arg in &self.raw_args {
            cmd.raw_arg(raw_arg);
        }

        if let Some(ref dir) = self.current_dir {
            cmd.current_dir(dir);
        }

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                return ToolResult::err(format!("Failed to spawn `{}`: {e}", self.program));
            }
        };

        // Write stdin data if provided, then close the pipe.
        if let Some(data) = self.stdin_data {
            use tokio::io::AsyncWriteExt;
            let mut stdin_handle = child.stdin.take().expect("stdin is piped");
            if let Err(e) = stdin_handle.write_all(&data).await {
                return ToolResult::err(format!(
                    "Failed to write to `{}` stdin: {e}",
                    self.program
                ));
            }
            // Drop closes the pipe, signalling EOF to the child.
            drop(stdin_handle);
        }

        collect_output(child, ctx, self.error_on_nonzero).await
    }
}

// ── Output collection ─────────────────────────────────────────────────────────

/// Drain `child`'s stdout and stderr, forward live chunks if `ctx` has a
/// sender, then build the final [`ToolResult`].
///
/// On Unix we use a concurrent Phase1+Phase2 drain so that background
/// processes that hold the pipe write-ends open do not cause a hang.
/// On other platforms we fall back to `wait_with_output()`.
async fn collect_output(
    child: tokio::process::Child,
    ctx: ToolCallContext,
    error_on_nonzero: bool,
) -> ToolResult {
    #[cfg(unix)]
    let (out_bytes, err_bytes, exit_code) = collect_unix(child, &ctx).await;

    #[cfg(not(unix))]
    let (out_bytes, err_bytes, exit_code) = collect_other(child, &ctx).await;

    let stdout = String::from_utf8_lossy(&out_bytes).into_owned();
    let stderr = String::from_utf8_lossy(&err_bytes).into_owned();

    let stdout = apply_terminal_render(&stdout);
    let stderr = apply_terminal_render(&stderr);

    let stdout = stdout.trim_end().to_string();
    let stderr = stderr.trim_end().to_string();

    let mut merged = String::new();
    if !stdout.is_empty() {
        merged.push_str(&stdout);
    }
    if !stderr.is_empty() {
        if !merged.is_empty() {
            merged.push('\n');
        }
        merged.push_str(&stderr);
    }
    if exit_code != 0 {
        if !merged.is_empty() && !merged.ends_with('\n') {
            merged.push('\n');
        }
        merged.push_str(&format!("exit {exit_code}\n"));
    }

    let tr = truncate_tail(&merged);
    let result = if tr.truncated {
        ToolResult::ok_truncated(tr, stdout, stderr)
    } else {
        ToolResult::ok(tr)
    };

    if error_on_nonzero && exit_code != 0 {
        ToolResult::err(result.content.as_text().to_string())
    } else {
        result
    }
}

/// Send a chunk via `ctx.tx` if a sender is wired up.
fn send_chunk(ctx: &ToolCallContext, chunk: &[u8]) {
    if let Some(tx) = &ctx.tx
        && !chunk.is_empty()
    {
        let text = String::from_utf8_lossy(chunk).into_owned();
        let _ = tx.send(AppEvent::Agent(AgentEvent::ToolOutputChunk {
            id: ctx.id.clone(),
            chunk: text,
        }));
    }
}

// ── Unix: concurrent Phase1+Phase2 drain ─────────────────────────────────────

#[cfg(unix)]
async fn collect_unix(
    mut child: tokio::process::Child,
    ctx: &ToolCallContext,
) -> (Vec<u8>, Vec<u8>, i32) {
    use tokio::io::AsyncReadExt;

    let mut stdout_handle = child.stdout.take().expect("stdout is piped");
    let mut stderr_handle = child.stderr.take().expect("stderr is piped");

    let mut out_buf = Vec::new();
    let mut err_buf = Vec::new();

    // Read in fixed-size chunks so we can forward live output as it arrives.
    const CHUNK: usize = 4096;
    let mut out_chunk = vec![0u8; CHUNK];
    let mut err_chunk = vec![0u8; CHUNK];
    let mut out_done = false;
    let mut err_done = false;

    // Phase 1: wait for process exit while draining pipes concurrently.
    let status = loop {
        tokio::select! {
            status = child.wait() => {
                match status {
                    Ok(s) => break s,
                    Err(e) => {
                        let exit_code = -1_i32;
                        log::debug!("collect_unix: wait failed: {e}");
                        return (out_buf, err_buf, exit_code);
                    }
                }
            }
            n = stdout_handle.read(&mut out_chunk), if !out_done => {
                match n {
                    Ok(0) => { out_done = true; }
                    Ok(n) => {
                        send_chunk(ctx, &out_chunk[..n]);
                        out_buf.extend_from_slice(&out_chunk[..n]);
                    }
                    Err(_) => { out_done = true; }
                }
            }
            n = stderr_handle.read(&mut err_chunk), if !err_done => {
                match n {
                    Ok(0) => { err_done = true; }
                    Ok(n) => {
                        send_chunk(ctx, &err_chunk[..n]);
                        err_buf.extend_from_slice(&err_chunk[..n]);
                    }
                    Err(_) => { err_done = true; }
                }
            }
        }
    };

    // Phase 2: process has exited; drain remaining buffered pipe data with a
    // short deadline in case a background child is still holding write-ends.
    let deadline = tokio::time::sleep(std::time::Duration::from_millis(200));
    tokio::pin!(deadline);

    loop {
        if out_done && err_done {
            break;
        }
        tokio::select! {
            _ = &mut deadline => { break; }
            n = stdout_handle.read(&mut out_chunk), if !out_done => {
                match n {
                    Ok(0) | Err(_) => { out_done = true; }
                    Ok(n) => {
                        send_chunk(ctx, &out_chunk[..n]);
                        out_buf.extend_from_slice(&out_chunk[..n]);
                    }
                }
            }
            n = stderr_handle.read(&mut err_chunk), if !err_done => {
                match n {
                    Ok(0) | Err(_) => { err_done = true; }
                    Ok(n) => {
                        send_chunk(ctx, &err_chunk[..n]);
                        err_buf.extend_from_slice(&err_chunk[..n]);
                    }
                }
            }
        }
    }

    let exit_code = status.code().unwrap_or(-1);
    (out_buf, err_buf, exit_code)
}

// ── Non-Unix: simple wait_with_output ────────────────────────────────────────

#[cfg(not(unix))]
async fn collect_other(
    child: tokio::process::Child,
    ctx: &ToolCallContext,
) -> (Vec<u8>, Vec<u8>, i32) {
    match child.wait_with_output().await {
        Ok(output) => {
            send_chunk(ctx, &output.stdout);
            send_chunk(ctx, &output.stderr);
            let exit_code = output.status.code().unwrap_or(-1);
            (output.stdout, output.stderr, exit_code)
        }
        Err(e) => {
            log::debug!("collect_other: wait_with_output failed: {e}");
            (Vec::new(), Vec::new(), -1)
        }
    }
}
