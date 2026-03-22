use std::sync::Arc;

use serde::de::DeserializeOwned;

use crate::agent::types::{ToolRegistry, ToolResult};

/// Deserialize a JSON `args` object into a typed struct, returning a
/// `ToolResult::err` on failure.  Used by every built-in tool to replace
/// the repetitive `args.get("x").and_then(|v| v.as_str())` pattern.
pub(super) fn parse_args<T: DeserializeOwned>(args: serde_json::Value) -> Result<T, ToolResult> {
    serde_json::from_value::<T>(args)
        .map_err(|e| ToolResult::err(format!("Invalid arguments: {e}")))
}

pub mod ask_user;
#[cfg(not(target_os = "windows"))]
pub mod bash;
#[cfg(target_os = "windows")]
pub mod cmd;
pub mod edit;
pub mod find;
#[cfg(target_os = "windows")]
pub mod powershell;
pub mod read;
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
pub fn register_builtin_tools(ask_tx: Option<AskRequestTx>) -> ToolRegistry {
    let mut registry = ToolRegistry::new();

    let mut tools: Vec<Arc<dyn crate::agent::types::Tool>> = vec![
        Arc::new(ReadFileTool),
        Arc::new(WriteTool),
        Arc::new(EditTool),
        Arc::new(FindTool),
        Arc::new(AskUserTool::new(ask_tx)),
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
