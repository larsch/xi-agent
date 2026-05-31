use std::pin::Pin;

use serde_json::Value;

use super::subprocess::SubprocessCommand;
use crate::agent::types::{Tool, ToolCallContext, ToolResult};

pub struct PowerShellTool;

#[derive(serde::Deserialize)]
struct PowerShellArgs {
    command: String,
}

impl Tool for PowerShellTool {
    fn name(&self) -> &str {
        "powershell"
    }

    fn description(&self) -> &str {
        "Run a command via `powershell.exe -NoProfile -Command` and return compact output. \
         Stdout and stderr are captured separately and merged in the response; \
         a non-zero exit code is appended as `exit N`. \
         Output is truncated to the last 2000 lines or 50 KiB (whichever is hit first); \
         if truncated, full stdout/stderr are saved to temp files and a notice with the \
         paths is appended. \
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

    fn streaming_field(&self) -> Option<String> {
        Some("command".to_string())
    }

    fn run(
        &self,
        args: Value,
        ctx: ToolCallContext,
    ) -> Pin<Box<dyn std::future::Future<Output = ToolResult> + Send + '_>> {
        Box::pin(async move {
            let PowerShellArgs { command } = match super::parse_args(args) {
                Ok(a) => a,
                Err(e) => return *e,
            };

            SubprocessCommand::new("powershell.exe")
                .arg("-NoProfile")
                .arg("-Command")
                .arg(command)
                .run(ctx)
                .await
        })
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
            result.content.as_text().to_lowercase().contains("hello"),
            "stdout not captured: {}",
            result.content.as_text()
        );
        assert!(!result.content.as_text().contains("stdout:"));
    }

    #[tokio::test]
    async fn powershell_nonzero_exit_not_error() {
        let tool = PowerShellTool;
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
    async fn powershell_zero_exit_omits_exit_line() {
        let tool = PowerShellTool;
        let args = serde_json::json!({"command": "Write-Output ok"});
        let result = tool.execute(args).await;
        assert!(!result.is_error);
        assert!(result.content.as_text().to_lowercase().contains("ok"));
        assert!(!result.content.as_text().contains("exit 0"));
    }

    #[tokio::test]
    async fn powershell_allows_double_quoted_arguments() {
        let tool = PowerShellTool;
        let args = serde_json::json!({"command": "Write-Output \"a b\""});
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
    async fn powershell_handles_spaces_in_double_quoted_paths() {
        let tool = PowerShellTool;
        let args = serde_json::json!({"command": "Test-Path \"C:\\Program Files\""});
        let result = tool.execute(args).await;

        assert!(!result.is_error);
        assert!(
            result.content.as_text().to_lowercase().contains("true"),
            "double-quoted path with spaces failed: {}",
            result.content.as_text()
        );
    }

    #[tokio::test]
    async fn powershell_backslash_escaped_quotes_are_literal() {
        let tool = PowerShellTool;
        let args = serde_json::json!({"command": "Write-Output \\\"a b\\\""});
        let result = tool.execute(args).await;

        assert!(!result.is_error);
        assert!(
            result.content.as_text().contains("\\a b\\"),
            "expected literal backslashes from \\\" escaping: {}",
            result.content.as_text()
        );
    }

    #[tokio::test]
    async fn powershell_wrapped_command_string_causes_parse_error() {
        let tool = PowerShellTool;
        let args = serde_json::json!({"command": "\"Write-Output \\\"a b\\\"\""});
        let result = tool.execute(args).await;

        assert!(!result.is_error);
        assert!(
            result.content.as_text().contains("ParserError")
                || result.content.as_text().contains("Unexpected token"),
            "expected parser error when command is wrapped in quotes: {}",
            result.content.as_text()
        );
        assert!(result.content.as_text().contains("exit 1"));
    }

    #[tokio::test]
    async fn powershell_invokes_external_program_with_spaced_argument() {
        let tool = PowerShellTool;
        let args = serde_json::json!({"command": "& cmd.exe '/d' '/c' 'echo' 'a b'"});
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
    async fn powershell_invokes_external_program_with_double_quoted_argument() {
        let tool = PowerShellTool;
        let args = serde_json::json!({"command": "'foo bar' | & findstr.exe \"foo bar\""});
        let result = tool.execute(args).await;

        assert!(!result.is_error);
        assert!(
            result.content.as_text().contains("foo bar"),
            "double-quoted argument to external program failed: {}",
            result.content.as_text()
        );
        assert!(!result.content.as_text().contains("exit 1"));
    }
}
