use std::sync::{Arc, Mutex};

use serde::de::DeserializeOwned;

use crate::agent::file_tracker::FileTracker;
use crate::agent::types::{Tool, ToolRegistry, ToolResult};

/// Deserialize a JSON `args` object into a typed struct, returning a
/// `ToolResult::err` on failure.  Used by every built-in tool to replace
/// the repetitive `args.get("x").and_then(|v| v.as_str())` pattern.
pub(super) fn parse_args<T: DeserializeOwned>(
    args: serde_json::Value,
) -> Result<T, Box<ToolResult>> {
    serde_json::from_value::<T>(args)
        .map_err(|e| Box::new(ToolResult::err(format!("Invalid arguments: {e}"))))
}

pub mod ask_user;
#[cfg(not(target_os = "windows"))]
pub mod bash;
#[cfg(target_os = "windows")]
pub mod cmd;
pub mod custom;
pub mod edit;
pub mod find;
#[cfg(target_os = "windows")]
pub mod powershell;
pub mod read;
pub mod truncate;
pub mod write;

use ask_user::AskUserTool;
#[cfg(not(target_os = "windows"))]
use bash::BashTool;
#[cfg(target_os = "windows")]
use cmd::CmdTool;
use edit::EditTool;
use find::FindTool;
#[cfg(target_os = "windows")]
use powershell::PowerShellTool;
use read::ReadFileTool;
use write::WriteTool;

use crate::agent::types::AskRequestTx;

/// Instantiate the built-in tools and return a populated `ToolRegistry`.
///
/// `custom` tools are appended after built-ins; any custom tool whose name
/// collides with a built-in is silently dropped (logged at debug).
pub fn register_builtin_tools(
    ask_tx: Option<AskRequestTx>,
    file_tracker: Arc<Mutex<FileTracker>>,
    custom: Vec<custom::CustomTool>,
) -> ToolRegistry {
    let mut registry = ToolRegistry::new();

    let mut tools: Vec<Arc<dyn crate::agent::types::Tool>> = vec![
        Arc::new(ReadFileTool::new(Arc::clone(&file_tracker))),
        Arc::new(WriteTool::new(Arc::clone(&file_tracker))),
        Arc::new(EditTool::new(Arc::clone(&file_tracker))),
        Arc::new(FindTool),
        Arc::new(AskUserTool::new(ask_tx, Some(Arc::clone(&file_tracker)))),
    ];

    #[cfg(target_os = "windows")]
    {
        tools.push(Arc::new(PowerShellTool));
        tools.push(Arc::new(CmdTool));
    }

    #[cfg(not(target_os = "windows"))]
    {
        tools.push(Arc::new(BashTool));
    }

    for tool in tools {
        registry.insert(tool.name().to_string(), tool);
    }

    // Register custom tools; skip any whose name is already taken by a built-in.
    for tool in custom {
        if registry.contains_key(tool.name()) {
            log::debug!(
                "custom tool '{}' skipped: name conflicts with a built-in tool",
                tool.name()
            );
        } else {
            registry.insert(tool.name().to_string(), Arc::new(tool));
        }
    }

    registry
}

#[cfg(test)]
mod tests {
    use super::parse_args;
    use serde::Deserialize;

    #[derive(Debug, Deserialize)]
    struct Simple {
        x: String,
    }

    #[test]
    fn parse_args_ok_for_valid_json() {
        let v = serde_json::json!({"x": "hello"});
        let r: Result<Simple, _> = parse_args(v);
        assert!(r.is_ok());
        assert_eq!(r.unwrap().x, "hello");
    }

    #[test]
    fn parse_args_err_for_missing_field() {
        let v = serde_json::json!({});
        let r: Result<Simple, _> = parse_args(v);
        let err = r.unwrap_err();
        assert!(err.is_error);
        assert!(err.content.contains("Invalid arguments"));
    }

    #[test]
    fn parse_args_err_for_wrong_type() {
        let v = serde_json::json!({"x": 99});
        let r: Result<Simple, _> = parse_args(v);
        assert!(r.unwrap_err().is_error);
    }

    #[test]
    fn parse_args_ignores_extra_fields() {
        let v = serde_json::json!({"x": "hi", "extra": true});
        let r: Result<Simple, _> = parse_args(v);
        assert!(r.is_ok());
        assert_eq!(r.unwrap().x, "hi");
    }
}
