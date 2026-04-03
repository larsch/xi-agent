use std::pin::Pin;

use serde_json::Value;

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

            let output = match tokio::process::Command::new("sh")
                .arg("-c")
                .arg(&command)
                .output()
                .await
            {
                Ok(o) => o,
                Err(e) => return ToolResult::err(format!("Failed to spawn shell: {e}")),
            };

            let exit_code = output.status.code().unwrap_or(-1);

            let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

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

    #[test]
    fn default_limits_match_pi_mono() {
        use super::super::truncate::{DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES};
        assert_eq!(DEFAULT_MAX_LINES, 2000);
        assert_eq!(DEFAULT_MAX_BYTES, 50 * 1024);
    }
}
