use std::pin::Pin;
use std::process::Stdio;

use serde_json::Value;
use tokio::io::AsyncReadExt;

use super::terminal::apply_terminal_render;
use super::truncate::truncate_tail;
use crate::agent::types::{Tool, ToolResult};
use crate::process::DetachFromTty;

pub struct ExecTool;

/// Arguments for the `exec` tool.
///
/// Optional fields (`cwd`, `env`) default to the agent's own working directory
/// and environment if omitted.
#[derive(serde::Deserialize)]
struct ExecArgs {
    /// Path or name of the executable to run.
    program: String,
    /// Argument list passed directly to the process — no shell interpretation.
    #[serde(default)]
    args: Vec<String>,
    /// Optional working directory for the child process.
    cwd: Option<String>,
    /// Optional extra environment variables to set (merged with the current
    /// environment).
    #[serde(default)]
    env: std::collections::HashMap<String, String>,
}

impl Tool for ExecTool {
    fn name(&self) -> &str {
        "exec"
    }

    fn description(&self) -> &str {
        "Execute a program directly with an argv-style argument list, bypassing the shell. \
         Arguments are passed literally — no shell quoting, escaping, or glob expansion is \
         performed. Use this tool instead of bash when arguments contain spaces, backticks, \
         quotes, dollar signs, newlines, or other characters that are fragile under shell \
         parsing. Stdout and stderr are captured separately and merged in the response; \
         a non-zero exit code is appended as `exit N`. \
         Output is truncated to the last 2000 lines or 50 KiB (whichever is hit first); \
         if truncated, full stdout/stderr are saved to temp files and a notice with the \
         paths is appended."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "program": {
                    "type": "string",
                    "description": "Executable path or name (resolved via PATH)"
                },
                "args": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Argument list passed directly to the process without shell interpretation"
                },
                "cwd": {
                    "type": "string",
                    "description": "Optional working directory for the child process"
                },
                "env": {
                    "type": "object",
                    "additionalProperties": { "type": "string" },
                    "description": "Optional extra environment variables (merged with current environment)"
                }
            },
            "required": ["program"]
        })
    }

    fn saves_output(&self) -> bool {
        true
    }

    fn streaming_field(&self) -> Option<String> {
        Some("args".to_string())
    }

    fn execute(
        &self,
        args: Value,
    ) -> Pin<Box<dyn std::future::Future<Output = ToolResult> + Send + '_>> {
        Box::pin(async move {
            let ExecArgs {
                program,
                args: argv,
                cwd,
                env,
            } = match super::parse_args(args) {
                Ok(a) => a,
                Err(e) => return *e,
            };

            let mut cmd = tokio::process::Command::new(&program);
            cmd.args(&argv)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .detach_from_tty();

            if let Some(ref dir) = cwd {
                cmd.current_dir(dir);
            }

            for (key, val) in &env {
                cmd.env(key, val);
            }

            let mut child = match cmd.spawn() {
                Ok(c) => c,
                Err(e) => return ToolResult::err(format!("Failed to spawn `{program}`: {e}")),
            };

            let mut stdout_handle = child.stdout.take().expect("stdout is piped");
            let mut stderr_handle = child.stderr.take().expect("stderr is piped");

            let mut out_buf = Vec::new();
            let mut err_buf = Vec::new();

            let read_stdout = stdout_handle.read_to_end(&mut out_buf);
            let read_stderr = stderr_handle.read_to_end(&mut err_buf);

            tokio::pin!(read_stdout);
            tokio::pin!(read_stderr);

            // Phase 1: wait for process exit while concurrently draining pipes.
            let status = loop {
                tokio::select! {
                    status = child.wait() => {
                        match status {
                            Ok(s) => break s,
                            Err(e) => return ToolResult::err(format!("Failed to wait for `{program}`: {e}")),
                        }
                    }
                    _ = &mut read_stdout => {}
                    _ = &mut read_stderr => {}
                }
            };

            // Phase 2: process has exited; drain remaining pipe data with a
            // short deadline in case a background child holds write-ends open.
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

            let stdout = apply_terminal_render(&stdout);
            let stderr = apply_terminal_render(&stderr);

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
    async fn exec_captures_stdout() {
        let tool = ExecTool;
        let args = serde_json::json!({"program": "echo", "args": ["hello"]});
        let result = tool.execute(args).await;
        assert!(!result.is_error);
        assert!(
            result.content.as_text().contains("hello"),
            "stdout: {}",
            result.content.as_text()
        );
    }

    #[tokio::test]
    async fn exec_captures_stderr() {
        let tool = ExecTool;
        // Use sh just to write to stderr; this tests the capture path.
        let args = serde_json::json!({"program": "sh", "args": ["-c", "echo oops >&2"]});
        let result = tool.execute(args).await;
        assert!(!result.is_error);
        assert!(
            result.content.as_text().contains("oops"),
            "stderr: {}",
            result.content.as_text()
        );
    }

    #[tokio::test]
    async fn exec_nonzero_exit_not_error() {
        let tool = ExecTool;
        let args = serde_json::json!({"program": "sh", "args": ["-c", "exit 42"]});
        let result = tool.execute(args).await;
        assert!(!result.is_error);
        assert!(
            result.content.as_text().contains("exit 42"),
            "output: {}",
            result.content.as_text()
        );
    }

    #[tokio::test]
    async fn exec_zero_exit_omits_exit_line() {
        let tool = ExecTool;
        let args = serde_json::json!({"program": "echo", "args": ["ok"]});
        let result = tool.execute(args).await;
        assert!(!result.is_error);
        assert!(result.content.as_text().contains("ok"));
        assert!(
            !result.content.as_text().contains("exit 0"),
            "should omit zero exit: {}",
            result.content.as_text()
        );
    }

    /// Core regression: arguments containing backticks, spaces, quotes, and
    /// dollar-signs must be passed literally without shell interpretation.
    #[tokio::test]
    async fn exec_passes_special_chars_literally() {
        let tool = ExecTool;
        // printf %s prints each arg without interpretation.
        let special = "hello `world` $PATH \"quoted\" 'single' \nnewline";
        let args = serde_json::json!({
            "program": "printf",
            "args": ["%s", special]
        });
        let result = tool.execute(args).await;
        assert!(!result.is_error);
        // The string should be echoed back verbatim.
        assert!(
            result.content.as_text().contains("hello `world` $PATH"),
            "special chars not preserved: {}",
            result.content.as_text()
        );
        assert!(
            result.content.as_text().contains("\"quoted\""),
            "double quotes not preserved: {}",
            result.content.as_text()
        );
    }

    /// Argument with spaces must arrive as a single argument, not be split.
    #[tokio::test]
    async fn exec_argument_with_spaces_is_single_arg() {
        let tool = ExecTool;
        // sh -c 'printf "%d\n" "$#"' -- arg1 "a b" arg3  =>  reports 3 args
        let args = serde_json::json!({
            "program": "sh",
            "args": ["-c", "printf '%d\\n' \"$#\"", "--", "a", "b c", "d"]
        });
        let result = tool.execute(args).await;
        assert!(!result.is_error);
        // 3 positional arguments: "a", "b c", "d"
        assert!(
            result.content.as_text().trim() == "3",
            "expected 3 args, got: {}",
            result.content.as_text()
        );
    }

    #[tokio::test]
    async fn exec_cwd_is_used() {
        let tool = ExecTool;
        let args = serde_json::json!({"program": "pwd", "cwd": "/tmp"});
        let result = tool.execute(args).await;
        assert!(!result.is_error);
        assert!(
            result.content.as_text().trim() == "/tmp",
            "cwd not applied: {}",
            result.content.as_text()
        );
    }

    #[tokio::test]
    async fn exec_env_is_merged() {
        let tool = ExecTool;
        let args = serde_json::json!({
            "program": "sh",
            "args": ["-c", "echo $MYVAR"],
            "env": {"MYVAR": "tau_test_value"}
        });
        let result = tool.execute(args).await;
        assert!(!result.is_error);
        assert!(
            result.content.as_text().contains("tau_test_value"),
            "env var not set: {}",
            result.content.as_text()
        );
    }

    #[tokio::test]
    async fn exec_missing_program_is_error() {
        let tool = ExecTool;
        let args = serde_json::json!({});
        let result = tool.execute(args).await;
        assert!(result.is_error);
        assert!(
            result.content.as_text().contains("Invalid arguments"),
            "{}",
            result.content.as_text()
        );
    }

    #[tokio::test]
    async fn exec_unknown_program_is_error() {
        let tool = ExecTool;
        let args = serde_json::json!({"program": "__no_such_program_tau__"});
        let result = tool.execute(args).await;
        assert!(result.is_error);
        assert!(
            result.content.as_text().contains("Failed to spawn"),
            "expected spawn error: {}",
            result.content.as_text()
        );
    }

    #[tokio::test]
    async fn exec_truncates_large_output() {
        let tool = ExecTool;
        // base64 encode so output is valid UTF-8
        let args = serde_json::json!({
            "program": "sh",
            "args": ["-c", "head -c 102400 /dev/urandom | base64"]
        });
        let result = tool.execute(args).await;
        assert!(!result.is_error);
        assert!(result.is_truncated, "expected truncation for large output");
        assert!(result.truncation.is_some());
    }

    /// Regression: argument containing a newline must be passed as-is and
    /// survive the round-trip through the exec path.
    #[tokio::test]
    async fn exec_argument_with_newline() {
        let tool = ExecTool;
        // printf %s prints args without a trailing newline; check for the literal \n inside.
        let args = serde_json::json!({
            "program": "sh",
            "args": ["-c", "printf '%d\\n' \"$#\"", "--", "line1\nline2"]
        });
        let result = tool.execute(args).await;
        assert!(!result.is_error);
        // One argument containing a newline — argc should be 1
        assert!(
            result.content.as_text().trim() == "1",
            "expected 1 arg, got: {}",
            result.content.as_text()
        );
    }
}
