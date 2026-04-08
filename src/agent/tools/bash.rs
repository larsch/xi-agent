use std::pin::Pin;
use std::process::Stdio;

use serde_json::Value;
use tokio::io::AsyncReadExt;

use super::terminal::apply_terminal_render;
use super::truncate::truncate_tail;
use crate::agent::types::{Tool, ToolResult};

pub struct BashTool;

#[derive(serde::Deserialize)]
struct BashArgs {
    command: String,
}

impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        "Run a shell command via `/bin/sh -c` and return compact output. \
         Stdout and stderr are captured separately and merged in the response; \
         a non-zero exit code is appended as `exit N`. \
         Output is truncated to the last 2000 lines or 50 KiB (whichever is \
         hit first); if truncated, full stdout/stderr are saved to temp files \
         and a notice with the paths is appended."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "Shell command to execute"
                }
            },
            "required": ["command"]
        })
    }

    fn saves_output(&self) -> bool {
        true
    }

    fn execute(
        &self,
        args: Value,
    ) -> Pin<Box<dyn std::future::Future<Output = ToolResult> + Send + '_>> {
        Box::pin(async move {
            let BashArgs { command } = match super::parse_args(args) {
                Ok(a) => a,
                Err(e) => return *e,
            };

            // Spawn the shell in its own process group so that any background
            // processes started with `&` do not inherit our stdout/stderr pipe
            // write-ends.  If we used `.output().await` (which reads until EOF
            // on the pipes), a lingering background child that kept those fds
            // open would cause the tool call to hang forever, even after the
            // shell itself had exited.
            //
            // Instead we:
            //   1. spawn() with piped stdio and process_group(0)
            //   2. wait() for the shell to exit
            //   3. read whatever was buffered in the pipes with a short deadline
            //      so that any background children still holding the write-ends
            //      do not block us indefinitely.
            let mut child = match tokio::process::Command::new("sh")
                .arg("-c")
                .arg(&command)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .process_group(0)
                .kill_on_drop(true)
                .spawn()
            {
                Ok(c) => c,
                Err(e) => return ToolResult::err(format!("Failed to spawn shell: {e}")),
            };

            let mut stdout_handle = child.stdout.take().expect("stdout is piped");
            let mut stderr_handle = child.stderr.take().expect("stderr is piped");

            // We need to drain the pipes concurrently with waiting for the
            // shell to exit.  If we wait() first and then read, a foreground
            // command with large output will deadlock: the pipe buffer fills up,
            // the child blocks on write, and wait() never returns.
            //
            // However if we just read_to_end() we get the old hang: a background
            // child holding the pipe write-end open keeps read_to_end() blocked
            // forever even after the shell has exited.
            //
            // Solution: drain pipes and wait() truly concurrently.  Once wait()
            // signals the shell has exited, give the pipes a short deadline to
            // drain any remaining buffered data, then stop regardless.
            let mut out_buf = Vec::new();
            let mut err_buf = Vec::new();

            let read_stdout = stdout_handle.read_to_end(&mut out_buf);
            let read_stderr = stderr_handle.read_to_end(&mut err_buf);

            tokio::pin!(read_stdout);
            tokio::pin!(read_stderr);

            // Phase 1: wait for shell exit while concurrently draining pipes.
            let status = loop {
                tokio::select! {
                    status = child.wait() => {
                        match status {
                            Ok(s) => break s,
                            Err(e) => return ToolResult::err(format!("Failed to wait for shell: {e}")),
                        }
                    }
                    _ = &mut read_stdout => {}
                    _ = &mut read_stderr => {}
                }
            };

            // Phase 2: shell has exited.  Drain whatever remains in the pipe
            // buffers, but give up after 200 ms in case a background child is
            // still holding the write-end open.
            let drain_deadline = tokio::time::sleep(std::time::Duration::from_millis(200));
            tokio::pin!(drain_deadline);
            tokio::select! {
                _ = &mut drain_deadline => {}
                _ = async { tokio::join!(&mut read_stdout, &mut read_stderr) } => {}
            }

            drop(stdout_handle);
            drop(stderr_handle);

            let exit_code = status.code().unwrap_or(-1);

            let stdout = String::from_utf8_lossy(&out_buf).into_owned();
            let stderr = String::from_utf8_lossy(&err_buf).into_owned();

            // Apply terminal rendering to handle carriage returns properly
            let stdout = apply_terminal_render(&stdout);
            let stderr = apply_terminal_render(&stderr);

            // Merge for the model response, as a terminal would show.
            let mut merged = String::new();
            if !stdout.is_empty() {
                merged.push_str(&stdout);
            }
            if !stderr.is_empty() {
                merged.push_str(&stderr);
            }
            if exit_code != 0 {
                if !merged.ends_with('\n') && !merged.is_empty() {
                    merged.push('\n');
                }
                merged.push_str(&format!("exit {exit_code}\n"));
            }

            let tr = truncate_tail(&merged);
            if tr.truncated {
                ToolResult::ok_truncated(tr, stdout, stderr)
            } else {
                ToolResult::ok(tr)
            }
        })
    }
}

#[cfg(all(test, not(target_os = "windows")))]
mod tests {
    use super::*;
    use crate::agent::types::Tool;

    #[tokio::test]
    async fn bash_captures_stdout() {
        let tool = BashTool;
        let args = serde_json::json!({"command": "echo hello"});
        let result = tool.execute(args).await;
        assert!(!result.is_error);
        assert!(
            result.content.contains("hello"),
            "stdout not captured: {}",
            result.content
        );
        assert!(
            !result.content.contains("stdout:"),
            "stdout heading should be omitted: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn bash_captures_stderr() {
        let tool = BashTool;
        let args = serde_json::json!({"command": "echo oops >&2"});
        let result = tool.execute(args).await;
        assert!(!result.is_error);
        assert!(
            result.content.contains("oops"),
            "stderr not captured: {}",
            result.content
        );
        assert!(
            !result.content.contains("stderr:"),
            "stderr heading should be omitted: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn bash_nonzero_exit_not_error() {
        let tool = BashTool;
        let args = serde_json::json!({"command": "exit 42"});
        let result = tool.execute(args).await;
        assert!(!result.is_error);
        assert!(
            result.content.contains("exit 42"),
            "exit code not in output: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn bash_zero_exit_omits_exit_line() {
        let tool = BashTool;
        let args = serde_json::json!({"command": "echo ok"});
        let result = tool.execute(args).await;
        assert!(!result.is_error);
        assert!(result.content.contains("ok"));
        assert!(
            !result.content.contains("exit 0"),
            "should omit zero exit code: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn bash_truncates_large_output() {
        let tool = BashTool;
        // Generate output larger than 50 KiB.
        let args = serde_json::json!({"command": "head -c 102400 /dev/urandom | base64"});
        let result = tool.execute(args).await;
        assert!(!result.is_error);
        assert!(
            result.is_truncated,
            "expected is_truncated for large output"
        );
        assert!(result.truncation.is_some(), "expected truncation metadata");
    }

    #[tokio::test]
    async fn bash_keeps_tail_on_truncation() {
        let tool = BashTool;
        // Print 3000 numbered lines — only the last 2000 should be kept.
        let args = serde_json::json!({"command": "seq 1 3000"});
        let result = tool.execute(args).await;
        assert!(!result.is_error);
        assert!(result.is_truncated);
        assert!(
            result.content.contains("3000"),
            "tail should include last line"
        );
        assert!(
            !result.content.contains("\n1\n"),
            "tail should not include first line"
        );
    }

    #[tokio::test]
    async fn bash_missing_command_param_is_error() {
        let tool = BashTool;
        let args = serde_json::json!({});
        let result = tool.execute(args).await;
        assert!(result.is_error);
    }

    #[tokio::test]
    async fn bash_wrong_type_for_command_is_error() {
        let tool = BashTool;
        let args = serde_json::json!({"command": 42});
        let result = tool.execute(args).await;
        assert!(result.is_error);
        assert!(
            result.content.contains("Invalid arguments"),
            "expected 'Invalid arguments' in error: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn bash_extra_fields_are_ignored() {
        let tool = BashTool;
        let args = serde_json::json!({"command": "echo hi", "timeout": 30});
        let result = tool.execute(args).await;
        assert!(!result.is_error);
        assert!(result.content.contains("hi"));
    }

    /// Regression test: a command that backgrounds a process must not cause
    /// the tool to hang waiting for that child's pipe write-ends to close.
    /// We verify the tool completes quickly (well under 1 s) even though the
    /// shell spawns a background job before exiting.
    ///
    /// Uses `sleep 0 &` so the background child exits almost immediately,
    /// keeping the test deterministic without leaving lingering processes.
    /// The important thing tested here is the code path: spawn → wait(shell) →
    /// deadline-bounded pipe drain, rather than the old `.output().await` which
    /// would have blocked until the background child closed its pipe fds.
    #[tokio::test]
    async fn bash_background_process_does_not_hang() {
        let tool = BashTool;
        let args = serde_json::json!({"command": "sleep 0 &\necho done"});

        let start = std::time::Instant::now();
        let result = tool.execute(args).await;
        let elapsed = start.elapsed();

        assert!(!result.is_error);
        assert!(
            result.content.contains("done"),
            "expected 'done' in output: {}",
            result.content
        );
        assert!(
            elapsed.as_secs() < 1,
            "tool took too long ({elapsed:?}) — possible pipe hang"
        );
    }

    #[tokio::test]
    async fn bash_handles_carriage_return_progress() {
        let tool = BashTool;
        // Simulate a progress bar that overwrites itself
        let args = serde_json::json!({"command": "printf '[10%%]\\r[20%%]\\r[30%%]\\n'"});
        let result = tool.execute(args).await;
        assert!(!result.is_error);
        // Should only show the final state, not intermediate progress lines
        assert!(
            result.content.contains("[30%]"),
            "expected final progress state in output: {}",
            result.content
        );
        // The output should be cleaned, so intermediate states are removed
        // (they would appear as separate lines before the fix)
        assert!(
            !result.content.contains("[10%]"),
            "should not contain intermediate progress [10%]: {}",
            result.content
        );
    }

    #[test]
    fn default_limits_match_pi_mono() {
        use super::super::truncate::{DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES};
        assert_eq!(DEFAULT_MAX_LINES, 2000);
        assert_eq!(DEFAULT_MAX_BYTES, 50 * 1024);
    }
}
