use std::pin::Pin;

use serde_json::Value;

use crate::agent::types::{Tool, ToolResult};

/// Maximum bytes captured from stdout or stderr before truncation.
const MAX_OUTPUT_BYTES: usize = 8 * 1024; // 8 KiB

pub struct PowerShellTool;

impl Tool for PowerShellTool {
    fn name(&self) -> &str {
        "powershell"
    }

    fn description(&self) -> &str {
        "Run a command via `powershell.exe -NoProfile -Command` and return compact output. \
         Stdout/stderr are emitted directly without section headings, and a \
         non-zero exit code is appended as `exit N`. \
         Both stdout and stderr are truncated to 8 KiB each. \
         Pass a raw PowerShell command string; do not wrap the whole command in extra quotes. \
         For arguments with spaces, use normal PowerShell quoting like \"C:\\Program Files\" or 'C:\\Program Files'. \
         Avoid literal \\\" sequences in the final command string; PowerShell treats them as backslash+quote characters."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "Raw PowerShell command to execute (no outer wrapping quotes). Use normal PowerShell quotes for spaced args, e.g. \"C:\\Program Files\" or 'C:\\Program Files'; avoid literal \\\" in the final command string."
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

            let output = match tokio::process::Command::new("powershell.exe")
                .arg("-NoProfile")
                .arg("-Command")
                .arg(&command)
                .output()
                .await
            {
                Ok(o) => o,
                Err(e) => return ToolResult::err(format!("Failed to spawn powershell.exe: {e}")),
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
    async fn powershell_captures_stdout() {
        let tool = PowerShellTool;
        let args = serde_json::json!({"command": "Write-Output hello"});
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
    async fn powershell_nonzero_exit_not_error() {
        let tool = PowerShellTool;
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
    async fn powershell_zero_exit_omits_exit_line() {
        let tool = PowerShellTool;
        let args = serde_json::json!({"command": "Write-Output ok"});
        let result = tool.execute(args).await;
        assert!(!result.is_error);
        assert!(result.content.to_lowercase().contains("ok"));
        assert!(!result.content.contains("exit 0"));
    }

    #[tokio::test]
    async fn powershell_allows_double_quoted_arguments() {
        let tool = PowerShellTool;
        let args = serde_json::json!({"command": "Write-Output \"a b\""});
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
    async fn powershell_handles_spaces_in_double_quoted_paths() {
        let tool = PowerShellTool;
        let args = serde_json::json!({"command": "Test-Path \"C:\\Program Files\""});
        let result = tool.execute(args).await;

        assert!(!result.is_error);
        assert!(
            result.content.to_lowercase().contains("true"),
            "double-quoted path with spaces failed: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn powershell_backslash_escaped_quotes_are_literal() {
        let tool = PowerShellTool;
        let args = serde_json::json!({"command": "Write-Output \\\"a b\\\""});
        let result = tool.execute(args).await;

        assert!(!result.is_error);
        assert!(
            result.content.contains("\\a b\\"),
            "expected literal backslashes from \\\" escaping: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn powershell_wrapped_command_string_causes_parse_error() {
        let tool = PowerShellTool;
        let args = serde_json::json!({"command": "\"Write-Output \\\"a b\\\"\""});
        let result = tool.execute(args).await;

        assert!(!result.is_error);
        assert!(
            result.content.contains("ParserError") || result.content.contains("Unexpected token"),
            "expected parser error when command is wrapped in quotes: {}",
            result.content
        );
        assert!(result.content.contains("exit 1"));
    }

    #[tokio::test]
    async fn powershell_invokes_external_program_with_spaced_argument() {
        let tool = PowerShellTool;
        let args = serde_json::json!({"command": "& cmd.exe '/d' '/c' 'echo' 'a b'"});
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
    async fn powershell_invokes_external_program_with_double_quoted_argument() {
        let tool = PowerShellTool;
        let args = serde_json::json!({"command": "'foo bar' | & findstr.exe \"foo bar\""});
        let result = tool.execute(args).await;

        assert!(!result.is_error);
        assert!(
            result.content.contains("foo bar"),
            "double-quoted argument to external program failed: {}",
            result.content
        );
        assert!(!result.content.contains("exit 1"));
    }
}
