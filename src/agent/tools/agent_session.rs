use crate::agent::types::{Tool, ToolCallContext, ToolResult};
use serde_json::Value;
use std::pin::Pin;

#[derive(serde::Deserialize)]
struct AgentSessionArgs {
    action: String,
    cwd: String,
    prompt: Option<String>,
}

pub struct AgentSessionTool;

impl Tool for AgentSessionTool {
    fn name(&self) -> &str {
        "agent_session"
    }

    fn description(&self) -> &str {
        "Interact with a live external xi session associated with another worktree. Use inspect to identify the session, state to check its runtime status, or post_prompt to ask it to act as a worker. This contacts a running session; it does not inspect source files. Returns unavailable when no live session owns the target directory."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["inspect", "state", "post_prompt"] },
                "cwd": { "type": "string", "description": "Absolute path to the worktree of the live external session to contact" },
                "prompt": { "type": "string", "description": "Instruction for the external worker session; required for post_prompt" }
            },
            "required": ["action", "cwd"]
        })
    }

    fn run(
        &self,
        args: Value,
        ctx: ToolCallContext,
    ) -> Pin<Box<dyn std::future::Future<Output = ToolResult> + Send + '_>> {
        Box::pin(async move {
            let args: AgentSessionArgs = match super::parse_args(args) {
                Ok(v) => v,
                Err(e) => return *e,
            };
            let result = match args.action.as_str() {
                "inspect" => {
                    crate::session_ipc::client_call(
                        &args.cwd,
                        "inspect_session",
                        serde_json::json!({}),
                    )
                    .await
                }
                "state" => {
                    crate::session_ipc::client_call(&args.cwd, "get_state", serde_json::json!({}))
                        .await
                }
                "post_prompt" => {
                    let Some(prompt) = args.prompt.filter(|p| !p.trim().is_empty()) else {
                        return ToolResult::err("prompt is required for post_prompt");
                    };
                    let Some(app_event_tx) = ctx.tx.clone() else {
                        return ToolResult::err(
                            "agent session control is unavailable in this context",
                        );
                    };
                    crate::session_ipc::client_post_prompt(&args.cwd, &prompt, app_event_tx).await
                }
                other => return ToolResult::err(format!("unknown action '{other}'")),
            };
            match result {
                Ok(value) => ToolResult::ok_str(value.to_string()),
                Err(error) => ToolResult::err(error),
            }
        })
    }
}
