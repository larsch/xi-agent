use std::pin::Pin;

use serde_json::Value;

use super::subprocess::SubprocessCommand;
use crate::agent::types::{Tool, ToolCallContext, ToolResult};

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
         and a notice with the paths is appended. For rich or structured writes, create a UTF-8 no-BOM payload file and invoke the target CLI with its --patch-file, --fields-file, or stdin option rather than passing the payload inline."
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

    fn streaming_field(&self) -> Option<&'static str> {
        Some("command")
    }

    fn run(
        &self,
        args: Value,
        ctx: ToolCallContext,
    ) -> Pin<Box<dyn std::future::Future<Output = ToolResult> + Send + '_>> {
        Box::pin(async move {
            let CmdArgs { command } = match super::parse_args(args) {
                Ok(a) => a,
                Err(e) => return *e,
            };

            // Keep the code-page change and user command in one cmd.exe
            // process. This improves UTF-8 output from cmd built-ins; native
            // programs may still use their own encoding and are protected by
            // the strict UTF-8 output decoder.
            let wrapped_command = format!("\"chcp 65001 >nul & {command}\"");
            SubprocessCommand::new("cmd.exe")
                .arg("/D")
                .arg("/S")
                .arg("/C")
                .raw_arg(wrapped_command)
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
    async fn cmd_utf8_code_page_preserves_direct_unicode_and_emoji() {
        let tool = CmdTool;
        let expected = "München, naïve, 日本語, emoji: 😀 👩🏽‍💻 🧪 é";
        let result = tool
            .execute(serde_json::json!({"command": format!("echo {expected}")}))
            .await;
        assert!(!result.is_error, "{}", result.content.as_text());
        assert_eq!(result.content.as_text(), expected);
    }

    #[tokio::test]
    async fn cmd_direct_unicode_command_argument_round_trips() {
        let tool = CmdTool;
        let expected = "München, naïve, 日本語, emoji: 😀";
        let result = tool
            .execute(serde_json::json!({"command": format!("echo {expected}")}))
            .await;
        assert!(!result.is_error, "{}", result.content.as_text());
        assert_eq!(result.content.as_text(), expected);
    }

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
    async fn cmd_code_page_prefix_preserves_command_chaining_and_exit_code() {
        let tool = CmdTool;
        let result = tool
            .execute(serde_json::json!({"command": "echo first && echo second && exit /b 42"}))
            .await;
        assert!(!result.is_error);
        assert!(result.content.as_text().contains("first"));
        assert!(result.content.as_text().contains("second"));
        assert!(result.content.as_text().contains("exit 42"));
    }

    #[tokio::test]
    async fn cmd_code_page_prefix_preserves_pipelines_and_conditional_fallback() {
        let tool = CmdTool;
        let pipeline = tool
            .execute(serde_json::json!({"command": "echo pipeline | findstr pipeline"}))
            .await;
        assert!(!pipeline.is_error, "{}", pipeline.content.as_text());
        assert_eq!(pipeline.content.as_text(), "pipeline");

        let fallback = tool
            .execute(serde_json::json!({"command": "cmd /c exit /b 1 || echo fallback"}))
            .await;
        assert!(!fallback.is_error, "{}", fallback.content.as_text());
        assert_eq!(fallback.content.as_text(), "fallback");
    }

    #[tokio::test]
    async fn cmd_code_page_prefix_preserves_redirection_and_grouping() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("redirected.txt");
        let path = path.to_string_lossy();
        let tool = CmdTool;
        let result = tool
            .execute(serde_json::json!({
                "command": format!("(echo grouped & echo redirected > \"{path}\") & type \"{path}\"")
            }))
            .await;
        assert!(!result.is_error, "{}", result.content.as_text());
        let lines: Vec<_> = result.content.as_text().lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].trim_end(), "grouped");
        assert_eq!(lines[1], "redirected");
    }

    #[tokio::test]
    async fn cmd_code_page_prefix_preserves_escaped_metacharacters_and_percent_expansion() {
        let tool = CmdTool;
        let metacharacters = tool
            .execute(
                serde_json::json!({"command": "echo literal ^& literal ^| literal ^> literal ^<"}),
            )
            .await;
        assert!(
            !metacharacters.is_error,
            "{}",
            metacharacters.content.as_text()
        );
        assert_eq!(
            metacharacters.content.as_text(),
            "literal & literal | literal > literal <"
        );

        // Delayed expansion happens as each command executes, after `set`.
        let expansion = tool
            .execute(serde_json::json!({"command": "cmd /v:on /c \"set XI_CMD_UTF8_TEST=expanded & echo !XI_CMD_UTF8_TEST!\""}))
            .await;
        assert!(!expansion.is_error, "{}", expansion.content.as_text());
        assert_eq!(expansion.content.as_text(), "expanded");
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
