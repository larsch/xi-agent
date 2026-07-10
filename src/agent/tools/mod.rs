use std::sync::{Arc, Mutex};

use serde::de::DeserializeOwned;

use crate::agent::file_tracker::FileTracker;
use crate::agent::types::{Tool, ToolRegistry, ToolResult};

/// Translate a serde_json inner error message into a model-friendly description.
///
/// Matches the small set of patterns produced by serde_json and maps Rust type
/// names to JSON concepts.  Returns `None` on non-match so the caller can fall
/// back to the raw message.
fn translate_serde_message(msg: &str) -> Option<String> {
    // "missing field `<name>`"
    if let Some(rest) = msg.strip_prefix("missing field `") {
        let field = rest.trim_end_matches('`');
        return Some(format!("required argument `{field}` is missing"));
    }

    // "invalid type: <got>, expected <want>"
    if let Some(rest) = msg.strip_prefix("invalid type: ")
        && let Some(idx) = rest.find(", expected ")
    {
        let got_raw = &rest[..idx];
        let want_raw = &rest[idx + ", expected ".len()..];
        let got = describe_got(got_raw);
        let want = describe_want(want_raw);
        return Some(format!("expected {want}, got {got}"));
    }

    // "invalid value: integer `<n>`, expected <want>"
    if let Some(rest) = msg.strip_prefix("invalid value: integer `")
        && let Some(idx) = rest.find("`, expected ")
    {
        let n = &rest[..idx];
        let want_raw = &rest[idx + "`, expected ".len()..];
        let want = describe_want(want_raw);
        return Some(format!("expected {want}, got integer `{n}`"));
    }

    None
}

/// Map the "got" fragment from a serde_json "invalid type" message to a
/// JSON-friendly description.
fn describe_got(raw: &str) -> String {
    // serde_json formats the received value as one of:
    //   null | boolean `<v>` | integer `<n>` | floating point `<n>`
    //   string "<s>" | sequence | map
    if raw == "null" {
        "null".to_string()
    } else if raw == "sequence" {
        "an array".to_string()
    } else if raw == "map" {
        "an object".to_string()
    } else if let Some(inner) = raw
        .strip_prefix("integer `")
        .and_then(|s| s.strip_suffix('`'))
    {
        format!("integer `{inner}`")
    } else if let Some(inner) = raw
        .strip_prefix("floating point `")
        .and_then(|s| s.strip_suffix('`'))
    {
        format!("number `{inner}`")
    } else if let Some(inner) = raw
        .strip_prefix("boolean `")
        .and_then(|s| s.strip_suffix('`'))
    {
        format!("boolean `{inner}`")
    } else if let Some(inner) = raw
        .strip_prefix("string \"")
        .and_then(|s| s.strip_suffix('"'))
    {
        format!("string \"{inner}\"")
    } else {
        raw.to_string()
    }
}

/// Map the "expected" fragment from a serde_json message to a JSON-friendly
/// description.  Rust type names (u64, usize, i64, …) are mapped to JSON
/// concepts.
fn describe_want(raw: &str) -> String {
    match raw {
        "u8" | "u16" | "u32" | "u64" | "u128" | "usize" => "a non-negative integer".to_string(),
        "i8" | "i16" | "i32" | "i64" | "i128" | "isize" => "an integer".to_string(),
        "f32" | "f64" => "a number".to_string(),
        "a string" | "str" | "String" => "a string".to_string(),
        "a boolean" | "bool" => "a boolean".to_string(),
        "a sequence" => "an array".to_string(),
        "a map" => "an object".to_string(),
        // Rust compound type names — map to JSON concepts.
        "struct variant" | "newtype variant" | "unit variant" | "tuple variant" => {
            "an object".to_string()
        }
        other => {
            if other.starts_with("struct ")
                || other.starts_with("enum ")
                || other.starts_with("newtype struct ")
            {
                "an object".to_string()
            } else if other.starts_with("tuple struct ") {
                "an array".to_string()
            } else if other.starts_with("unit struct ") {
                "null".to_string()
            } else {
                other.to_string()
            }
        }
    }
}

/// Format a `serde_path_to_error::Error<serde_json::Error>` into a
/// model-friendly message, falling back to the raw serde message when the
/// inner message does not match any known pattern.
fn format_parse_error(err: serde_path_to_error::Error<serde_json::Error>) -> String {
    let path = err.path().to_string();
    let inner = err.inner().to_string();

    // Strip the trailing " at line N column M" noise that serde_json appends.
    let bare = inner
        .find(" at line ")
        .map(|i| &inner[..i])
        .unwrap_or(&inner);

    let translated = translate_serde_message(bare);

    match (path.as_str(), translated) {
        // Top-level path (".") with a successful translation — no field prefix needed.
        (".", Some(msg)) => format!("Invalid arguments: {msg}"),
        // Named path with a successful translation.
        (p, Some(msg)) => format!("Invalid arguments: argument `{p}`: {msg}"),
        // Any path, no translation — fall back to raw (still better than nothing).
        (_, None) => format!("Invalid arguments: {inner}"),
    }
}

/// Deserialize a JSON `args` object into a typed struct, returning a
/// `ToolResult::err` on failure.  Used by every built-in tool to replace
/// the repetitive `args.get("x").and_then(|v| v.as_str())` pattern.
///
/// Errors are translated into model-friendly messages that name the offending
/// field and describe the expected type in JSON terms.
pub(super) fn parse_args<T: DeserializeOwned>(
    args: serde_json::Value,
) -> Result<T, Box<ToolResult>> {
    serde_path_to_error::deserialize::<_, T>(args)
        .map_err(|e| Box::new(ToolResult::err(format_parse_error(e))))
}

pub mod ask_user;
#[cfg(not(target_os = "windows"))]
pub mod bash;
#[cfg(target_os = "windows")]
pub mod cmd;
pub mod custom;
pub mod edit;
#[cfg(not(target_os = "windows"))]
pub mod exec;
pub mod find;
#[cfg(target_os = "windows")]
pub mod powershell;
pub mod python;
pub mod read;
pub mod read_skill;
pub mod subprocess;
pub mod terminal;
pub mod truncate;
pub mod utf8;
pub mod write;

use ask_user::AskUserTool;
#[cfg(not(target_os = "windows"))]
use bash::BashTool;
#[cfg(target_os = "windows")]
use cmd::CmdTool;
use edit::EditTool;
#[cfg(not(target_os = "windows"))]
use exec::ExecTool;
use find::FindTool;
#[cfg(target_os = "windows")]
use powershell::PowerShellTool;
use python::PythonTool;
use read::ReadFileTool;
use read_skill::ReadSkillTool;
use write::WriteTool;

use crate::app_event::AppEventTx;

/// Instantiate the built-in tools and return a populated `ToolRegistry`.
///
/// `custom` tools are appended after built-ins; any custom tool whose name
/// collides with a built-in is silently dropped (logged at debug).
pub async fn register_builtin_tools(
    app_event_tx: Option<AppEventTx>,
    file_tracker: Arc<Mutex<FileTracker>>,
    skills: Arc<Vec<crate::skills::SkillMeta>>,
    custom: Vec<custom::CustomTool>,
) -> ToolRegistry {
    let mut registry = ToolRegistry::new();

    let mut tools: Vec<Arc<dyn crate::agent::types::Tool>> = vec![
        Arc::new(ReadFileTool::new(Arc::clone(&file_tracker))),
        Arc::new(WriteTool::new(Arc::clone(&file_tracker))),
        Arc::new(EditTool::new(Arc::clone(&file_tracker))),
        Arc::new(FindTool),
        Arc::new(ReadSkillTool::new(Arc::clone(&skills))),
        Arc::new(AskUserTool::new(
            app_event_tx,
            Some(Arc::clone(&file_tracker)),
        )),
    ];

    #[cfg(target_os = "windows")]
    {
        tools.push(Arc::new(PowerShellTool));
        tools.push(Arc::new(CmdTool));
    }

    #[cfg(not(target_os = "windows"))]
    {
        tools.push(Arc::new(BashTool));
        tools.push(Arc::new(ExecTool));
    }

    // Detect and register the Python tool if a suitable runtime is available.
    if let Some(runtime) = python::detect_python().await {
        tools.push(Arc::new(PythonTool::new(runtime)));
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
    use super::{parse_args, translate_serde_message};
    use serde::Deserialize;

    // ── Structs used by parse_args tests ────────────────────────────────────

    #[derive(Debug, Deserialize)]
    struct Simple {
        x: String,
    }

    #[derive(Debug, Deserialize)]
    struct Multi {
        name: String,
        count: u64,
        flag: bool,
    }

    // ── parse_args happy-path ────────────────────────────────────────────────

    #[test]
    fn parse_args_ok_for_valid_json() {
        let v = serde_json::json!({"x": "hello"});
        let r: Result<Simple, _> = parse_args(v);
        assert!(r.is_ok());
        assert_eq!(r.unwrap().x, "hello");
    }

    #[test]
    fn parse_args_ignores_extra_fields() {
        let v = serde_json::json!({"x": "hi", "extra": true});
        let r: Result<Simple, _> = parse_args(v);
        assert!(r.is_ok());
        assert_eq!(r.unwrap().x, "hi");
    }

    // ── parse_args error messages ────────────────────────────────────────────

    fn err_content<T: for<'de> Deserialize<'de> + std::fmt::Debug>(v: serde_json::Value) -> String {
        parse_args::<T>(v)
            .unwrap_err()
            .content
            .as_text()
            .to_string()
    }

    #[test]
    fn parse_args_missing_field_names_the_field() {
        let msg = err_content::<Simple>(serde_json::json!({}));
        assert!(msg.contains("Invalid arguments"), "prefix missing: {msg}");
        assert!(msg.contains("`x`"), "field name missing: {msg}");
        assert!(msg.contains("missing"), "word 'missing' absent: {msg}");
    }

    #[test]
    fn parse_args_wrong_type_string_got_integer() {
        // `x` is String, send integer
        let msg = err_content::<Simple>(serde_json::json!({"x": 42}));
        assert!(msg.contains("Invalid arguments"), "{msg}");
        assert!(msg.contains("`x`"), "field name missing: {msg}");
        assert!(msg.contains("string"), "expected type missing: {msg}");
    }

    #[test]
    fn parse_args_wrong_type_integer_got_string() {
        // Verify successful parse reads back correctly
        let ok: Multi = parse_args(serde_json::json!({"name":"a","count":1,"flag":true})).unwrap();
        assert_eq!(ok.name, "a");
        assert_eq!(ok.count, 1);
        assert!(ok.flag);
        // `count` is u64, send string
        let msg = err_content::<Multi>(serde_json::json!({"name":"a","count":"hi","flag":true}));
        assert!(msg.contains("Invalid arguments"), "{msg}");
        assert!(msg.contains("`count`"), "field name missing: {msg}");
        assert!(
            msg.contains("integer") || msg.contains("non-negative"),
            "expected type missing: {msg}"
        );
    }

    #[test]
    fn parse_args_wrong_type_boolean_got_string() {
        // `flag` is bool, send string
        let msg = err_content::<Multi>(serde_json::json!({"name":"a","count":1,"flag":"yes"}));
        assert!(msg.contains("Invalid arguments"), "{msg}");
        assert!(msg.contains("`flag`"), "field name missing: {msg}");
        assert!(msg.contains("boolean"), "expected type missing: {msg}");
    }

    #[test]
    fn parse_args_wrong_type_string_got_null() {
        let msg = err_content::<Simple>(serde_json::json!({"x": null}));
        assert!(msg.contains("Invalid arguments"), "{msg}");
        assert!(msg.contains("`x`"), "field name missing: {msg}");
        assert!(msg.contains("null"), "received value missing: {msg}");
    }

    #[test]
    fn parse_args_negative_integer_for_unsigned() {
        // `count` is u64, send -5
        let msg = err_content::<Multi>(serde_json::json!({"name":"a","count":-5,"flag":true}));
        assert!(msg.contains("Invalid arguments"), "{msg}");
        assert!(msg.contains("`count`"), "field name missing: {msg}");
        assert!(
            msg.contains("non-negative") || msg.contains("-5"),
            "value or constraint missing: {msg}"
        );
    }

    #[test]
    fn parse_args_wrong_type_array_got_string() {
        #[derive(Debug, Deserialize)]
        struct WithVec {
            tags: Vec<String>,
        }
        let ok: WithVec = parse_args(serde_json::json!({"tags": ["a", "b"]})).unwrap();
        assert_eq!(ok.tags, ["a", "b"]);
        let msg = err_content::<WithVec>(serde_json::json!({"tags": "oops"}));
        assert!(msg.contains("Invalid arguments"), "{msg}");
        assert!(msg.contains("`tags`"), "field name missing: {msg}");
        assert!(
            msg.contains("array") || msg.contains("sequence"),
            "expected type missing: {msg}"
        );
    }

    #[test]
    fn parse_args_wrong_type_object_got_array() {
        #[derive(Debug, Deserialize)]
        struct WithMap {
            meta: std::collections::HashMap<String, String>,
        }
        let ok: WithMap = parse_args(serde_json::json!({"meta": {"k": "v"}})).unwrap();
        assert_eq!(ok.meta["k"], "v");
        let msg = err_content::<WithMap>(serde_json::json!({"meta": []}));
        assert!(msg.contains("Invalid arguments"), "{msg}");
        assert!(msg.contains("`meta`"), "field name missing: {msg}");
        assert!(
            msg.contains("object") || msg.contains("map"),
            "expected type missing: {msg}"
        );
    }

    // ── translate_serde_message unit tests (canary for serde_json upgrades) ──

    #[test]
    fn translate_missing_field() {
        let r = translate_serde_message("missing field `path`");
        assert_eq!(r, Some("required argument `path` is missing".to_string()));
    }

    #[test]
    fn translate_invalid_type_string_got_integer() {
        let r = translate_serde_message("invalid type: integer `42`, expected a string");
        assert_eq!(r, Some("expected a string, got integer `42`".to_string()));
    }

    #[test]
    fn translate_invalid_type_int_expected_u64() {
        let r = translate_serde_message("invalid type: string \"hi\", expected u64");
        assert_eq!(
            r,
            Some("expected a non-negative integer, got string \"hi\"".to_string())
        );
    }

    #[test]
    fn translate_invalid_type_bool_expected() {
        let r = translate_serde_message("invalid type: string \"yes\", expected a boolean");
        assert_eq!(
            r,
            Some("expected a boolean, got string \"yes\"".to_string())
        );
    }

    #[test]
    fn translate_invalid_type_null() {
        let r = translate_serde_message("invalid type: null, expected a string");
        assert_eq!(r, Some("expected a string, got null".to_string()));
    }

    #[test]
    fn translate_invalid_type_sequence_got_string() {
        let r = translate_serde_message("invalid type: string \"x\", expected a sequence");
        assert_eq!(r, Some("expected an array, got string \"x\"".to_string()));
    }

    #[test]
    fn translate_invalid_type_map_got_sequence() {
        let r = translate_serde_message("invalid type: sequence, expected a map");
        assert_eq!(r, Some("expected an object, got an array".to_string()));
    }

    #[test]
    fn translate_invalid_value_negative_u64() {
        let r = translate_serde_message("invalid value: integer `-5`, expected u64");
        assert_eq!(
            r,
            Some("expected a non-negative integer, got integer `-5`".to_string())
        );
    }

    #[test]
    fn translate_unknown_message_returns_none() {
        let r = translate_serde_message("some future serde error format we don't know about");
        assert_eq!(r, None);
    }

    #[test]
    fn translate_null_for_struct() {
        let r = translate_serde_message("invalid type: null, expected struct ExecArgs");
        assert_eq!(r, Some("expected an object, got null".to_string()));
    }

    #[test]
    fn translate_string_for_struct() {
        let r = translate_serde_message("invalid type: string \"hi\", expected struct BashArgs");
        assert_eq!(r, Some("expected an object, got string \"hi\"".to_string()));
    }

    #[test]
    fn translate_array_for_struct() {
        let r = translate_serde_message("invalid type: sequence, expected struct Foo");
        assert_eq!(r, Some("expected an object, got an array".to_string()));
    }

    #[test]
    fn translate_enum_variant_patterns() {
        let r = translate_serde_message("invalid type: null, expected struct variant");
        assert_eq!(r, Some("expected an object, got null".to_string()));
        let r = translate_serde_message("invalid type: null, expected newtype variant");
        assert_eq!(r, Some("expected an object, got null".to_string()));
    }

    #[test]
    fn parse_args_null_for_struct_gives_friendly_error() {
        #[derive(Debug, Deserialize)]
        struct Args {
            _field: Option<String>,
        }
        let msg = err_content::<Args>(serde_json::Value::Null);
        assert!(
            msg.contains("expected an object, got null"),
            "unfriendly error: {msg}"
        );
    }
}
