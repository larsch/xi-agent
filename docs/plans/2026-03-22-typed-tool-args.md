# Typed tool argument deserialization

**Date:** 2026-03-22  
**Status:** Implemented  
**Priority:** High  
**Risk:** Low — isolated to `src/agent/tools/`, no trait or interface changes  
**Source:** TAU-REVIEW.md §5 — Implicit Tool Trait Coupling to serde_json

---

## Problem

Every tool manually extracts its parameters from a raw `serde_json::Value` using
the same repetitive pattern:

```rust
let command = match args.get("command").and_then(|v| v.as_str()) {
    Some(c) => c.to_string(),
    None => return ToolResult::err("Missing required parameter: command"),
};
```

This pattern appears in every tool (bash, read, write, edit, find, powershell, cmd).
Consequences:

1. **Silent schema drift.** The `parameters_schema()` JSON and the manual field
   extraction can diverge without any compile-time warning.
2. **Weak error messages.** Type mismatches produce "Missing required parameter: x"
   even when the field is present but has the wrong type.
3. **Boilerplate.** Each new tool or new parameter requires copy-pasting the same
   `match args.get(...).and_then(...)` chain.
4. **No coverage of wrong-type args.** Current tests only check for absent
   parameters, not for wrong types.

## Goals

1. Replace all manual parameter extraction with typed `#[derive(Deserialize)]`
   argument structs and a single `parse_args` helper.
2. Produce a useful error message when the LLM supplies wrong types.
3. Keep the `Tool` trait signature unchanged — this is a purely internal refactor.
4. Cover missing and wrong-type argument cases with unit tests in each tool module.

## Non-goals

- Changing the `Tool` trait interface (that is a separate, larger plan).
- Auto-generating `parameters_schema()` from the arg struct (useful but deferred).
- Schema validation beyond what `serde_json::from_value` provides.

## Design

### Helper function in `src/agent/tools/mod.rs`

```rust
use serde::de::DeserializeOwned;
use crate::agent::types::ToolResult;

/// Deserialize a JSON `args` object into a typed struct, returning a
/// `ToolResult::err` on failure.  Used by every built-in tool.
pub(crate) fn parse_args<T: DeserializeOwned>(
    args: serde_json::Value,
) -> Result<T, ToolResult> {
    serde_json::from_value::<T>(args)
        .map_err(|e| ToolResult::err(format!("Invalid arguments: {e}")))
}
```

### Per-tool arg struct

Each tool gets a private `Args` struct in its own file:

```rust
// bash.rs
#[derive(serde::Deserialize)]
struct BashArgs {
    command: String,
}

// In Tool::execute():
let BashArgs { command } = match parse_args(args) {
    Ok(a) => a,
    Err(e) => return e,
};
```

Serde's default behaviour already does what we need:
- Required fields (no `Option<>`) → missing key or wrong type → error
- Optional fields → `Option<T>` or `#[serde(default)]`

### Existing `parameters_schema()` stays unchanged

The JSON schema is the contract sent to the LLM. The arg struct acts as the
Rust-side mirror of that schema. They are intentionally kept separate to allow
schema descriptions to be richer than the struct.

## Affected files

| File | Change |
|------|--------|
| `src/agent/tools/mod.rs` | Add `parse_args<T>` helper |
| `src/agent/tools/bash.rs` | Add `BashArgs`, replace manual extraction |
| `src/agent/tools/read.rs` | Add `ReadFileArgs`, replace manual extraction |
| `src/agent/tools/write.rs` | Add `WriteArgs`, replace manual extraction |
| `src/agent/tools/edit.rs` | Add `EditArgs`, replace manual extraction |
| `src/agent/tools/find.rs` | Add `FindArgs`, replace manual extraction |
| `src/agent/tools/powershell.rs` | Add `PowerShellArgs`, replace manual extraction |
| `src/agent/tools/cmd.rs` | Add `CmdArgs`, replace manual extraction |

## Tests

Each tool already has unit tests covering the happy path and the
"missing required field" case. Extend or add the following per-tool:

| Test name | What it checks |
|-----------|---------------|
| `{tool}_missing_required_param_is_error` | Already exists; verify error message still says "Invalid arguments" |
| `{tool}_wrong_type_for_param_is_error` | New — pass `{"command": 42}` instead of string |
| `{tool}_extra_unknown_fields_are_ignored` | New — extra keys should not cause an error (serde default) |

The `parse_args` helper itself should have a small test in `tools/mod.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::parse_args;
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct Simple { x: String }

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
        assert!(r.is_err());
        assert!(r.unwrap_err().is_error);
    }

    #[test]
    fn parse_args_err_for_wrong_type() {
        let v = serde_json::json!({"x": 99});
        let r: Result<Simple, _> = parse_args(v);
        assert!(r.is_err());
    }

    #[test]
    fn parse_args_ignores_extra_fields() {
        let v = serde_json::json!({"x": "hi", "extra": true});
        let r: Result<Simple, _> = parse_args(v);
        assert!(r.is_ok());
    }
}
```

## Verification checklist

Before marking done:

1. `cargo fmt`
2. `cargo clippy --all-targets`
3. `cargo test` — all existing tests pass, new wrong-type tests added for each tool
4. Confirm no `args.get(...)` / `.and_then(...)` extraction chains remain in tool files

## Risks and mitigations

| Risk | Likelihood | Mitigation |
|------|-----------|------------|
| Serde error messages differ from previous ones (LLM feedback) | Low | The agent receives tool errors as `ToolResult::err`; message wording change is acceptable |
| Optional fields with custom defaults change behaviour | Low | Audit each tool's optional params before converting; use `#[serde(default)]` explicitly |
