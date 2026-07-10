use std::pin::Pin;

use serde_json::Value;

use super::subprocess::SubprocessCommand;
use crate::agent::types::{Tool, ToolCallContext, ToolResult};

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
         parsing. All output (stdout and stderr) is captured and returned; \
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

    fn streaming_field(&self) -> Option<&'static str> {
        Some("args")
    }

    fn run(
        &self,
        args: Value,
        ctx: ToolCallContext,
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

            let mut cmd = SubprocessCommand::new(program).args(argv);
            for (k, v) in env {
                cmd = cmd.env(k, v);
            }
            if let Some(dir) = cwd {
                cmd = cmd.current_dir(dir);
            }
            cmd.run(ctx).await
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
    async fn exec_subprocess_preserves_utf8_output_and_python_environment() {
        let tool = ExecTool;
        // Construct Unicode from UTF-8 byte escapes so argv remains ASCII.
        let result = tool
            .execute(serde_json::json!({
                "program": "sh",
                "args": ["-c", "printf '\\303\\274\\342\\200\\223\\342\\211\\244\\n'; printf '%s|%s\\n' \"$PYTHONUTF8\" \"$PYTHONIOENCODING\""]
            }))
            .await;
        assert!(!result.is_error, "{}", result.content.as_text());
        assert_eq!(result.content.as_text(), "ü–≤\n1|utf-8");
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
            "env": {"MYVAR": "xi_test_value"}
        });
        let result = tool.execute(args).await;
        assert!(!result.is_error);
        assert!(
            result.content.as_text().contains("xi_test_value"),
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
        let args = serde_json::json!({"program": "__no_such_program_xi__"});
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
