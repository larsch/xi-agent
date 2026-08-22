use std::pin::Pin;

use serde_json::Value;

use crate::agent::types::{Tool, ToolCallContext, ToolResult};
use crate::app_event::{AppEvent, SendIgnore};

/// A built-in tool that restarts the `xi` host process.
///
/// Calling `restart_host` with no arguments requests that the process re-`exec`
/// itself from disk (so a freshly rebuilt binary takes effect) and resume the
/// active session.  Because `exec` replaces the process image, this tool never
/// returns a result: it signals the app to perform the restart and then parks
/// its future forever.  The resumed process detects the unanswered call and
/// synthesizes the result (`Restarted from binary … last modified at …`).
pub struct RestartHostTool {
    description: String,
}

impl RestartHostTool {
    pub fn new() -> Self {
        let exe = crate::restart::current_exe_display();
        let description = format!(
            "Restart the `xi` host process that is running this conversation — re-execute its binary ({exe}) from disk to load a freshly rebuilt build, then resume the current session, preserving all context so the conversation continues uninterrupted. Takes no arguments."
        );
        Self { description }
    }
}

impl Tool for RestartHostTool {
    fn name(&self) -> &str {
        crate::restart::RESTART_TOOL_NAME
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {}
        })
    }

    fn run(
        &self,
        _args: Value,
        ctx: ToolCallContext,
    ) -> Pin<Box<dyn std::future::Future<Output = ToolResult> + Send + '_>> {
        Box::pin(async move {
            if let Some(tx) = ctx.tx {
                tx.send_ignore(AppEvent::Restart);
            }
            // The process is replaced via exec() before a result is ever
            // requested; never produce one.  Parking the future forever keeps
            // the agent loop from recording a spurious ToolResult.
            std::future::pending::<ToolResult>().await
        })
    }
}
