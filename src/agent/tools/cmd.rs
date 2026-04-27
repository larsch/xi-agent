use std::pin::Pin;
use std::process::Stdio;

use serde_json::Value;

use super::truncate::truncate_tail;
use crate::agent::types::{Tool, ToolResult};

pub struct CmdTool;

#[derive(serde::Deserialize)]
struct CmdArgs {
    command: String,
}

impl Tool for CmdTool {
    fn name(&self) -> &str {
        "cmd"
    }

    fn description(&self) -> &str {
        "Run a command via `cmd.exe /C` and return compact output. \
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
                    "description": "Command to execute with cmd.exe /C"
                }
            },
            "required": ["command"]
        })
    }

    fn saves_output(&self) -> bool {
        true
    }

    fn streaming_field(&self) -> Option<String> {
        Some("command".to_string())
    }

    fn execute(
        &self,
        args: Value,
    ) -> Pin<Box<dyn std::future::Future<Output = ToolResult> + Send + '_>> {
        Box::pin(async move {
            let CmdArgs { command } = match super::parse_args(args) {
                Ok(a) => a,
                Err(e) => return *e,
            };

            let wrapped_command = format!("\"{command}\"");
            let output = match tokio::process::Command::new("cmd.exe")
                .arg("/D") // Disable AutoRun commands from registry.
                .arg("/S") // Preserve predictable quote handling with /C.
                .arg("/C")
                .raw_arg(&wrapped_command)
                .stdin(Stdio::null())
                .kill_on_drop(true)
                .output()
                .await
            {
                Ok(o) => o,
                Err(e) => return ToolResult::err(format!("Failed to run cmd.exe: {e}")),
            };

            let exit_code = output.status.code().unwrap_or(-1);

            let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::types::Tool;

    #[tokio::test]
    async fn cmd_captures_stdout() {
        let tool = CmdTool;
        let args = serde_json::json!({"command": "echo hello"});
        let result = tool.execute(args).await;
        assert!(!result.is_error);
        assert!(
            result.content.as_text().to_lowercase().contains("hello"),
            "stdout not captured: {}",
            result.content.as_text()
        );
        assert!(!result.content.as_text().contains("stdout:"));
    }

    #[tokio::test]
    async fn cmd_nonzero_exit_not_error() {
        let tool = CmdTool;
        let args = serde_json::json!({"command": "exit /b 42"});
        let result = tool.execute(args).await;
        assert!(!result.is_error);
        assert!(
            result.content.as_text().contains("exit 42"),
            "exit code not in output: {}",
            result.content.as_text()
        );
    }

    #[tokio::test]
    async fn cmd_zero_exit_omits_exit_line() {
        let tool = CmdTool;
        let args = serde_json::json!({"command": "echo ok"});
        let result = tool.execute(args).await;
        assert!(!result.is_error);
        assert!(result.content.as_text().to_lowercase().contains("ok"));
        assert!(!result.content.as_text().contains("exit 0"));
    }

    #[tokio::test]
    async fn cmd_allows_double_quoted_arguments() {
        let tool = CmdTool;
        let args = serde_json::json!({"command": "echo \"a b\""});
        let result = tool.execute(args).await;

        assert!(!result.is_error);
        assert!(
            result.content.as_text().contains("a b"),
            "double-quoted argument did not round-trip: {}",
            result.content.as_text()
        );
        assert!(!result.content.as_text().contains("exit 1"));
    }

    #[tokio::test]
    async fn cmd_backslash_escaped_quotes_are_literal() {
        let tool = CmdTool;
        let args = serde_json::json!({"command": "echo \\\"a b\\\""});
        let result = tool.execute(args).await;

        assert!(!result.is_error);
        assert!(
            result.content.as_text().contains("a b"),
            "expected echoed payload to include a b: {}",
            result.content.as_text()
        );
        assert!(
            result.content.as_text().contains("\\\""),
            "expected literal backslash+quote sequence from \\\" escaping: {}",
            result.content.as_text()
        );
    }

    #[tokio::test]
    async fn cmd_wrapped_command_string_fails() {
        let tool = CmdTool;
        let args = serde_json::json!({"command": "\"echo \\\"a b\\\"\""});
        let result = tool.execute(args).await;

        assert!(!result.is_error);
        assert!(
            result
                .content
                .as_text()
                .to_lowercase()
                .contains("cannot find")
                || result
                    .content
                    .as_text()
                    .to_lowercase()
                    .contains("not recognized"),
            "expected wrapped command string to fail: {}",
            result.content.as_text()
        );
        assert!(result.content.as_text().contains("exit 1"));
    }

    #[tokio::test]
    async fn cmd_invokes_external_program_with_spaced_argument() {
        let tool = CmdTool;
        let args = serde_json::json!({
            "command": "powershell -NoProfile -Command \"Write-Output 'a b'\""
        });
        let result = tool.execute(args).await;

        assert!(!result.is_error);
        assert!(
            result.content.as_text().contains("a b"),
            "external command did not receive spaced argument: {}",
            result.content.as_text()
        );
        assert!(!result.content.as_text().contains("exit 1"));
    }

    #[tokio::test]
    async fn cmd_handles_double_quoted_windows_path_argument() {
        let tool = CmdTool;
        let args = serde_json::json!({
            "command": "dir \"C:\\Program Files\""
        });
        let result = tool.execute(args).await;

        assert!(
            result
                .content
                .as_text()
                .to_lowercase()
                .contains("program files"),
            "double-quoted Windows path argument did not execute properly: {}",
            result.content.as_text()
        );
    }
}
