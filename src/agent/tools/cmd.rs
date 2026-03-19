use std::pin::Pin;

use serde_json::Value;

use crate::agent::types::{Tool, ToolResult};

/// Maximum bytes captured from stdout or stderr before truncation.
const MAX_OUTPUT_BYTES: usize = 8 * 1024; // 8 KiB

pub struct CmdTool;

impl Tool for CmdTool {
    fn name(&self) -> &str {
        "cmd"
    }

    fn description(&self) -> &str {
        "Run a command via `cmd.exe /C` and return compact output. \
         Stdout/stderr are emitted directly without section headings, and a \
         non-zero exit code is appended as `exit N`. \
         Both stdout and stderr are truncated to 8 KiB each."
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

    fn execute(
        &self,
        args: Value,
    ) -> Pin<Box<dyn std::future::Future<Output = ToolResult> + Send + '_>> {
        Box::pin(async move {
            let command = match args.get("command").and_then(|v| v.as_str()) {
                Some(c) => c.to_string(),
                None => return ToolResult::err("Missing required parameter: command"),
            };

            let wrapped_command = format!("\"{command}\"");
            let output = match tokio::process::Command::new("cmd.exe")
                .arg("/D") // Disable AutoRun commands from registry.
                .arg("/S") // Preserve predictable quote handling with /C.
                .arg("/C")
                .raw_arg(&wrapped_command)
                .output()
                .await
            {
                Ok(o) => o,
                Err(e) => return ToolResult::err(format!("Failed to run cmd.exe: {e}")),
            };

            let exit_code = output.status.code().unwrap_or(-1);
            let stdout = truncate_bytes(&output.stdout, MAX_OUTPUT_BYTES);
            let stderr = truncate_bytes(&output.stderr, MAX_OUTPUT_BYTES);

            let mut result = String::new();

            if !stdout.is_empty() {
                result.push_str(&stdout);
                if !stdout.ends_with('\n') {
                    result.push('\n');
                }
            }

            if !stderr.is_empty() {
                result.push_str(&stderr);
                if !stderr.ends_with('\n') {
                    result.push('\n');
                }
            }

            if exit_code != 0 {
                result.push_str(&format!("exit {exit_code}\n"));
            }

            ToolResult::ok(result)
        })
    }
}

fn truncate_bytes(bytes: &[u8], max_bytes: usize) -> String {
    if bytes.is_empty() {
        return String::new();
    }

    if bytes.len() <= max_bytes {
        String::from_utf8_lossy(bytes).into_owned()
    } else {
        let mut s = String::from_utf8_lossy(&bytes[..max_bytes]).into_owned();
        s.push_str("\n[truncated]");
        s
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
            result.content.to_lowercase().contains("hello"),
            "stdout not captured: {}",
            result.content
        );
        assert!(!result.content.contains("stdout:"));
    }

    #[tokio::test]
    async fn cmd_nonzero_exit_not_error() {
        let tool = CmdTool;
        let args = serde_json::json!({"command": "exit /b 42"});
        let result = tool.execute(args).await;
        assert!(!result.is_error);
        assert!(
            result.content.contains("exit 42"),
            "exit code not in output: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn cmd_zero_exit_omits_exit_line() {
        let tool = CmdTool;
        let args = serde_json::json!({"command": "echo ok"});
        let result = tool.execute(args).await;
        assert!(!result.is_error);
        assert!(result.content.to_lowercase().contains("ok"));
        assert!(!result.content.contains("exit 0"));
    }

    #[tokio::test]
    async fn cmd_allows_double_quoted_arguments() {
        let tool = CmdTool;
        let args = serde_json::json!({"command": "echo \"a b\""});
        let result = tool.execute(args).await;

        assert!(!result.is_error);
        assert!(
            result.content.contains("a b"),
            "double-quoted argument did not round-trip: {}",
            result.content
        );
        assert!(!result.content.contains("exit 1"));
    }

    #[tokio::test]
    async fn cmd_backslash_escaped_quotes_are_literal() {
        let tool = CmdTool;
        let args = serde_json::json!({"command": "echo \\\"a b\\\""});
        let result = tool.execute(args).await;

        assert!(!result.is_error);
        assert!(
            result.content.contains("a b"),
            "expected echoed payload to include a b: {}",
            result.content
        );
        assert!(
            result.content.contains("\\\""),
            "expected literal backslash+quote sequence from \\\" escaping: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn cmd_wrapped_command_string_fails() {
        let tool = CmdTool;
        let args = serde_json::json!({"command": "\"echo \\\"a b\\\"\""});
        let result = tool.execute(args).await;

        assert!(!result.is_error);
        assert!(
            result.content.to_lowercase().contains("cannot find")
                || result.content.to_lowercase().contains("not recognized"),
            "expected wrapped command string to fail: {}",
            result.content
        );
        assert!(result.content.contains("exit 1"));
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
            result.content.contains("a b"),
            "external command did not receive spaced argument: {}",
            result.content
        );
        assert!(!result.content.contains("exit 1"));
    }

    #[tokio::test]
    async fn cmd_handles_double_quoted_windows_path_argument() {
        let tool = CmdTool;
        let args = serde_json::json!({
            "command": "dir \"C:\\Program Files\""
        });
        let result = tool.execute(args).await;

        assert!(
            result.content.to_lowercase().contains("program files"),
            "double-quoted Windows path argument did not execute properly: {}",
            result.content
        );
    }
}
