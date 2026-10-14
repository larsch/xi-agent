use std::pin::Pin;

use serde_json::Value;

use super::subprocess::SubprocessCommand;
use crate::agent::types::{Tool, ToolCallContext, ToolResult};

/// Run a shell command through sudo's askpass interface.
pub struct SudoTool;

#[derive(serde::Deserialize)]
struct SudoArgs {
    command: String,
}

impl Tool for SudoTool {
    fn name(&self) -> &str {
        "sudo"
    }

    fn description(&self) -> &str {
        "Run a shell command with sudo. The parent xi session asks for approval and, when needed, a password; approval and password handling stay in memory and are never included in the command result."
    }

    fn prompt_snippet(&self) -> Option<&str> {
        Some("Run a shell command with sudo")
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "Shell command to execute as root" }
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
            let SudoArgs { command } = match super::parse_args(args) {
                Ok(args) => args,
                Err(error) => return *error,
            };
            let Some(socket) = crate::sudo_ipc::socket_path() else {
                return ToolResult::err(
                    "sudo is unavailable: no interactive parent authorization socket",
                );
            };
            let Ok(token) = crate::sudo_ipc::random_hex(32) else {
                return ToolResult::err(
                    "sudo is unavailable: could not create authorization token",
                );
            };
            if !crate::sudo_ipc::register(token.clone(), command.clone()) {
                return ToolResult::err("sudo is unavailable: authorization token collision");
            }
            let _cleanup = AuthorizationCleanup(token.clone());
            let result = SubprocessCommand::new("sudo")
                .args(["-A", "/bin/sh", "-c"])
                .arg(command)
                .env("SUDO_ASKPASS", current_executable())
                .env("XI_SUDO_TOKEN", token.clone())
                .env("XI_SOCKET", socket.to_string_lossy().into_owned())
                .run(ctx.clone())
                .await;
            crate::sudo_ipc::unregister(&token);
            if let Some(tx) = ctx.tx {
                let _ = tx.send(crate::app_event::AppEvent::SudoComplete(token));
            }
            result
        })
    }
}

struct AuthorizationCleanup(String);

impl Drop for AuthorizationCleanup {
    fn drop(&mut self) {
        crate::sudo_ipc::unregister(&self.0);
    }
}

fn current_executable() -> String {
    std::env::current_exe()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "xi".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::types::Tool;

    #[tokio::test]
    async fn missing_command_is_error() {
        let result = SudoTool.execute(serde_json::json!({})).await;
        assert!(result.is_error);
    }
}
