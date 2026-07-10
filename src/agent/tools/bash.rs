use std::pin::Pin;

use serde_json::Value;

use super::subprocess::SubprocessCommand;
use crate::agent::types::{Tool, ToolCallContext, ToolResult};

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
         All output (stdout and stderr) is captured and returned; \
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

    fn streaming_field(&self) -> Option<&'static str> {
        Some("command")
    }

    fn run(
        &self,
        args: Value,
        ctx: ToolCallContext,
    ) -> Pin<Box<dyn std::future::Future<Output = ToolResult> + Send + '_>> {
        Box::pin(async move {
            let BashArgs { command } = match super::parse_args(args) {
                Ok(a) => a,
                Err(e) => return *e,
            };

            SubprocessCommand::new("sh")
                .arg("-c")
                .arg(command)
                .run(ctx)
                .await
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
            result.content.as_text().contains("hello"),
            "stdout not captured: {}",
            result.content.as_text()
        );
        assert!(
            !result.content.as_text().contains("stdout:"),
            "stdout heading should be omitted: {}",
            result.content.as_text()
        );
    }

    #[tokio::test]
    async fn bash_subprocess_preserves_utf8_output_and_python_environment() {
        let tool = BashTool;
        let result = tool
            .execute(serde_json::json!({
                "command": "printf 'München – ≤\\n'; printf '%s|%s\\n' \"$PYTHONUTF8\" \"$PYTHONIOENCODING\""
            }))
            .await;
        assert!(!result.is_error, "{}", result.content.as_text());
        assert_eq!(result.content.as_text(), "München – ≤\n1|utf-8");
    }

    #[tokio::test]
    async fn bash_captures_stderr() {
        let tool = BashTool;
        let args = serde_json::json!({"command": "echo oops >&2"});
        let result = tool.execute(args).await;
        assert!(!result.is_error);
        assert!(
            result.content.as_text().contains("oops"),
            "stderr not captured: {}",
            result.content.as_text()
        );
        assert!(
            !result.content.as_text().contains("stderr:"),
            "stderr heading should be omitted: {}",
            result.content.as_text()
        );
    }

    #[tokio::test]
    async fn bash_nonzero_exit_not_error() {
        let tool = BashTool;
        let args = serde_json::json!({"command": "exit 42"});
        let result = tool.execute(args).await;
        assert!(!result.is_error);
        assert!(
            result.content.as_text().contains("exit 42"),
            "exit code not in output: {}",
            result.content.as_text()
        );
    }

    #[tokio::test]
    async fn bash_zero_exit_omits_exit_line() {
        let tool = BashTool;
        let args = serde_json::json!({"command": "echo ok"});
        let result = tool.execute(args).await;
        assert!(!result.is_error);
        assert!(result.content.as_text().contains("ok"));
        assert!(
            !result.content.as_text().contains("exit 0"),
            "should omit zero exit code: {}",
            result.content.as_text()
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
            result.content.as_text().contains("3000"),
            "tail should include last line"
        );
        assert!(
            !result.content.as_text().contains("\n1\n"),
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
            result.content.as_text().contains("Invalid arguments"),
            "expected 'Invalid arguments' in error: {}",
            result.content.as_text()
        );
    }

    #[tokio::test]
    async fn bash_extra_fields_are_ignored() {
        let tool = BashTool;
        let args = serde_json::json!({"command": "echo hi", "timeout": 30});
        let result = tool.execute(args).await;
        assert!(!result.is_error);
        assert!(result.content.as_text().contains("hi"));
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
            result.content.as_text().contains("done"),
            "expected 'done' in output: {}",
            result.content.as_text()
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
            result.content.as_text().contains("[30%]"),
            "expected final progress state in output: {}",
            result.content.as_text()
        );
        // The output should be cleaned, so intermediate states are removed
        // (they would appear as separate lines before the fix)
        assert!(
            !result.content.as_text().contains("[10%]"),
            "should not contain intermediate progress [10%]: {}",
            result.content.as_text()
        );
    }

    #[test]
    fn default_limits_match_pi_mono() {
        use super::super::truncate::{DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES};
        assert_eq!(DEFAULT_MAX_LINES, 2000);
        assert_eq!(DEFAULT_MAX_BYTES, 50 * 1024);
    }

    /// Verify that commands run by BashTool have no controlling terminal:
    /// stdout (fd 1) must not be a TTY, and /dev/tty must not be accessible.
    #[cfg(unix)]
    #[tokio::test]
    async fn bash_no_controlling_terminal() {
        let tool = BashTool;

        // isatty check on fd 1
        let result = tool
            .execute(serde_json::json!({"command": "[ -t 1 ] && echo tty || echo notty"}))
            .await;
        assert!(!result.is_error);
        assert!(
            result.content.as_text().contains("notty"),
            "expected stdout to not be a tty: {}",
            result.content.as_text()
        );

        // /dev/tty should not be openable without a controlling terminal
        let result = tool
            .execute(serde_json::json!({"command": "if (exec 3</dev/tty) 2>/dev/null; then echo has_ctty; else echo no_ctty; fi"}))
            .await;
        assert!(!result.is_error);
        assert!(
            result.content.as_text().contains("no_ctty"),
            "expected /dev/tty to be inaccessible: {}",
            result.content.as_text()
        );
    }
}
