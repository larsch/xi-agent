# Plan: Custom User Tools

**Date:** 2026-03-27  
**Status:** ✅ Complete — implemented, verified, accepted

---

## Goal

Allow users to extend tau with their own tools (shell scripts, compiled
binaries, or any executable) without modifying or recompiling tau.

---

## Scope

**In:**
- `CustomTool` struct and `load_custom_tools` discovery function
- `--describe` protocol: `executable --describe` → JSON descriptor
- Invocation protocol: JSON args on stdin, stdout = result string, non-zero exit = error
- Integration into the tool registry (built-ins take name-collision precedence)
- `/reload` reloads custom tools alongside skills
- `--print-dirs` updated to list tool directories
- Debug logging for skipped/invalid tools
- Unit tests

**Out:**
- Caching of `--describe` output across restarts
- Sandboxing or permission checks
- Windows-specific script extension resolution beyond `std::process::Command` defaults

---

## Describe Protocol

```
$ <executable> --describe
```

Stdout must be a JSON object:
```json
{
  "name": "my_tool",
  "description": "Does something useful.",
  "parameters_schema": {
    "type": "object",
    "properties": {
      "input": { "type": "string", "description": "The input string" }
    },
    "required": ["input"]
  }
}
```

Executables that fail, time out, or return invalid JSON are silently skipped
(logged at `debug` level).

---

## Invocation Protocol

```
echo '<json-args-object>' | <executable>
```

- Arguments are written as a JSON object to the executable's stdin.
- Stdout is the tool result string returned to the model.
- Exit code 0 → `ToolResult::ok(stdout)`.
- Non-zero exit code → `ToolResult::err(stdout or stderr)`.

---

## Discovery Roots

Searched in order; duplicates resolved by canonical path:

1. `~/.tau/tools/`
2. `./.tau/tools/` (project-local, current working directory)
3. `ProjectDirs::config_dir()/tools/` (e.g. `~/.config/tau/tools/` on Linux)

---

## Implementation Steps

### 1. `src/agent/tools/custom.rs` (new)

- `struct CustomTool { path: PathBuf, name: String, description: String, schema: Value }`
- `impl Tool for CustomTool` — `execute` writes args JSON to child stdin, reads stdout, maps exit code
- `fn custom_tool_dirs() -> Vec<PathBuf>` — returns the three roots
- `fn load_custom_tools(roots: &[PathBuf]) -> Vec<CustomTool>` — iterates roots, deduped by canonical path, runs `--describe`, parses JSON, skips failures

### 2. `src/agent/tools/mod.rs`

- Add `pub mod custom`
- Export `custom::load_custom_tools` and `custom::custom_tool_dirs`
- In `register_builtin_tools` (or a new wrapper), accept a `Vec<CustomTool>` and insert them after built-ins; skip any whose name collides with an already-registered built-in

### 3. `src/main.rs`

- At startup: call `load_custom_tools(&custom_tool_dirs())`, register results
- On `RunResult::ReloadContext`: reload custom tools (same call), rebuild registry, update system prompt

### 4. `src/dirs.rs`

- Add tool directories to `print_dirs()` output

### 5. Tests (`src/agent/tools/custom.rs`)

- Valid fake executable in a temp dir → tool loaded with correct name/description/schema
- `--describe` returns invalid JSON → skipped, no panic
- Executable not found / not executable → skipped gracefully
- Invocation: args on stdin, stdout returned as result
- Non-zero exit → `ToolResult::err`

---

## Affected Files

| File | Change |
|------|--------|
| `src/agent/tools/custom.rs` | **new** |
| `src/agent/tools/mod.rs` | add module, integrate custom tools |
| `src/main.rs` | load + reload custom tools |
| `src/dirs.rs` | add tool dirs to `--print-dirs` |

---

## Risks & Assumptions

- Executables run synchronously at load time; acceptable for small tool sets.
- Execute bit required on Unix; missing bit is caught and skipped.
- `canonicalize()` used for dedup; falls back to raw path if dir doesn't exist.
- Name collision with built-in: custom tool silently dropped (logged at debug).

---

## Verification

- `cargo test` passes (including new unit tests using `tempfile` + inline scripts)
- `cargo clippy` clean, no compiler warnings
- Manual smoke test: shell script with `--describe` in `~/.tau/tools/` → appears in tool list, callable by model
