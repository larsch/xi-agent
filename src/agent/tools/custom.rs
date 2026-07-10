use std::{collections::HashSet, env, path::PathBuf, pin::Pin, process::Stdio};

use serde_json::Value;

use super::subprocess::SubprocessCommand;
use crate::agent::types::{Tool, ToolCallContext, ToolResult};

// ── CustomTool ────────────────────────────────────────────────────────────────

/// A user-defined tool loaded from an executable on disk.
///
/// The executable must implement the describe/invoke protocol:
/// - `executable --describe` → JSON descriptor on stdout
/// - UTF-8 JSON on stdin → UTF-8 result on stdout; non-zero exit = error
///
/// Rich or structured write parameters must document and prefer a UTF-8
/// `--patch-file`, `--fields-file`, or stdin interface.
pub struct CustomTool {
    /// Absolute path to the executable.
    path: PathBuf,
    name: String,
    description: String,
    schema: Value,
}

impl Tool for CustomTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> Value {
        self.schema.clone()
    }

    fn run(
        &self,
        args: Value,
        ctx: ToolCallContext,
    ) -> Pin<Box<dyn std::future::Future<Output = ToolResult> + Send + '_>> {
        Box::pin(async move {
            let args_json = args.to_string();

            SubprocessCommand::new(self.path.to_string_lossy())
                .stdin_data(args_json.into_bytes())
                .error_on_nonzero_exit()
                .run(ctx)
                .await
        })
    }
}

// ── Discovery ─────────────────────────────────────────────────────────────────

/// Returns the ordered list of directories to search for custom tools:
/// 1. `~/.xi/tools/`
/// 2. `./.xi/tools/` (project-local)
/// 3. `ProjectDirs::config_dir()/tools/`
pub fn custom_tool_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    if let Some(home) = env::var_os("HOME").filter(|s| !s.is_empty()) {
        dirs.push(PathBuf::from(home).join(".xi").join("tools"));
    }

    if let Ok(cwd) = env::current_dir() {
        dirs.push(cwd.join(".xi").join("tools"));
    }

    if let Ok(proj) = crate::dirs::project_dirs() {
        dirs.push(proj.config_dir().join("tools"));
    }

    dirs
}

/// Scan `roots` for executable files, run `executable --describe` on each,
/// parse the JSON descriptor, and return the resulting [`CustomTool`] list.
///
/// Roots are deduplicated by canonical path. Files that are not executable,
/// fail to run, or return invalid JSON are silently skipped (logged at debug).
///
/// The returned tools are in directory-traversal order (sorted by name within
/// each directory).
pub fn load_custom_tools(roots: &[PathBuf]) -> Vec<CustomTool> {
    let mut seen_dirs: HashSet<PathBuf> = HashSet::new();
    let mut tools = Vec::new();

    for root in roots {
        let canonical = root.canonicalize().unwrap_or_else(|_| root.clone());
        if !seen_dirs.insert(canonical) {
            continue;
        }
        if !root.is_dir() {
            continue;
        }
        tools.extend(load_tools_from_dir(root));
    }

    tools
}

fn load_tools_from_dir(dir: &std::path::Path) -> Vec<CustomTool> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return vec![];
    };

    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && is_executable(p))
        .collect();

    paths.sort();

    paths
        .into_iter()
        .filter_map(|path| load_tool_from_executable(&path))
        .collect()
}

/// Run `executable --describe` synchronously and parse the JSON descriptor.
/// Returns `None` (and logs at debug) if anything goes wrong.
fn load_tool_from_executable(path: &std::path::Path) -> Option<CustomTool> {
    // Retry once on ETXTBSY: another thread may have a write fd open on the
    // same inode (e.g. a NamedTempFile in a concurrent test) for a very brief
    // window.  A short sleep is always sufficient to outlast it.
    let output = {
        let attempt = std::process::Command::new(path)
            .arg("--describe")
            .stdin(Stdio::null())
            .output();
        match attempt {
            Err(ref e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                std::thread::sleep(std::time::Duration::from_millis(10));
                std::process::Command::new(path)
                    .arg("--describe")
                    .stdin(Stdio::null())
                    .output()
            }
            other => other,
        }
    };

    let output = match output {
        Ok(o) => o,
        Err(e) => {
            log::debug!(
                "custom tool: failed to run --describe on {}: {e}",
                path.display()
            );
            return None;
        }
    };

    if !output.status.success() {
        log::debug!(
            "custom tool: --describe exited with {} for {}",
            output.status,
            path.display()
        );
        return None;
    }

    let json: Value = match serde_json::from_slice(&output.stdout) {
        Ok(v) => v,
        Err(e) => {
            log::debug!(
                "custom tool: invalid JSON from --describe on {}: {e}",
                path.display()
            );
            return None;
        }
    };

    let name = json.get("name").and_then(Value::as_str)?.trim().to_string();
    let description = json
        .get("description")
        .and_then(Value::as_str)?
        .trim()
        .to_string();
    let schema = json
        .get("parameters_schema")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({"type": "object", "properties": {}}));

    if name.is_empty() || description.is_empty() {
        log::debug!(
            "custom tool: missing name or description from --describe on {}",
            path.display()
        );
        return None;
    }

    Some(CustomTool {
        path: path.to_path_buf(),
        name,
        description,
        schema,
    })
}

// ── Platform helpers ──────────────────────────────────────────────────────────

#[cfg(unix)]
fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .map(|m| m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &std::path::Path) -> bool {
    // On Windows, rely on the OS to decide via file extension (.exe, .cmd, etc.)
    // We include all regular files and let Command::spawn fail gracefully.
    path.exists()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// Write a shell script to `path` with the execute bit set.
    fn write_script(path: &std::path::Path, body: &str) {
        std::fs::write(path, body).unwrap();
        let mut perms = std::fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).unwrap();
    }

    fn describe_script(name: &str, description: &str) -> String {
        format!(
            r#"#!/bin/sh
if [ "$1" = "--describe" ]; then
  printf '{{"name":"{name}","description":"{description}","parameters_schema":{{"type":"object","properties":{{"input":{{"type":"string"}}}}}}}}'
  exit 0
fi
input=$(cat)
printf "got: $input"
"#
        )
    }

    #[test]
    fn loads_valid_tool_from_directory() {
        let dir = tempfile::tempdir().unwrap();
        let script_path = dir.path().join("my_tool");
        write_script(&script_path, &describe_script("my_tool", "Does something."));

        let tools = load_custom_tools(&[dir.path().to_path_buf()]);
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name(), "my_tool");
        assert_eq!(tools[0].description(), "Does something.");
    }

    #[test]
    fn skips_invalid_json_from_describe() {
        let dir = tempfile::tempdir().unwrap();
        let script_path = dir.path().join("bad_tool");
        write_script(
            &script_path,
            "#!/bin/sh\nif [ \"$1\" = \"--describe\" ]; then echo 'not json'; fi\n",
        );

        let tools = load_custom_tools(&[dir.path().to_path_buf()]);
        assert!(tools.is_empty());
    }

    #[test]
    fn skips_nonzero_describe_exit() {
        let dir = tempfile::tempdir().unwrap();
        let script_path = dir.path().join("fail_tool");
        write_script(
            &script_path,
            "#!/bin/sh\nif [ \"$1\" = \"--describe\" ]; then exit 1; fi\n",
        );

        let tools = load_custom_tools(&[dir.path().to_path_buf()]);
        assert!(tools.is_empty());
    }

    #[test]
    fn skips_non_executable_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("not_executable");
        // Write file without execute bit.
        std::fs::write(&path, "#!/bin/sh\necho hello\n").unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o644);
        std::fs::set_permissions(&path, perms).unwrap();

        let tools = load_custom_tools(&[dir.path().to_path_buf()]);
        assert!(tools.is_empty());
    }

    #[test]
    fn deduplicates_same_directory_via_canonical_path() {
        let dir = tempfile::tempdir().unwrap();
        let script_path = dir.path().join("my_tool");
        write_script(&script_path, &describe_script("my_tool", "Desc."));

        // Pass the same directory twice (once as canonical, once as raw).
        let roots = vec![dir.path().to_path_buf(), dir.path().to_path_buf()];
        let tools = load_custom_tools(&roots);
        assert_eq!(tools.len(), 1);
    }

    #[test]
    fn empty_directory_returns_no_tools() {
        let dir = tempfile::tempdir().unwrap();
        let tools = load_custom_tools(&[dir.path().to_path_buf()]);
        assert!(tools.is_empty());
    }

    #[test]
    fn nonexistent_directory_returns_no_tools() {
        let tools = load_custom_tools(&[PathBuf::from("/nonexistent/xi/tools")]);
        assert!(tools.is_empty());
    }

    #[tokio::test]
    async fn execute_passes_args_on_stdin_and_returns_stdout() {
        let dir = tempfile::tempdir().unwrap();
        let script_path = dir.path().join("echo_tool");
        write_script(&script_path, &describe_script("echo_tool", "Echoes input."));

        let tools = load_custom_tools(&[dir.path().to_path_buf()]);
        assert_eq!(tools.len(), 1);

        let result = tools[0]
            .execute(serde_json::json!({"input": "hello"}))
            .await;
        assert!(!result.is_error);
        assert!(
            result.content.as_text().contains("got:"),
            "got: {}",
            result.content.as_text()
        );
    }

    #[tokio::test]
    async fn execute_nonzero_exit_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let script_path = dir.path().join("fail_on_run");
        write_script(
            &script_path,
            r#"#!/bin/sh
if [ "$1" = "--describe" ]; then
  printf '{"name":"fail_on_run","description":"Always fails.","parameters_schema":{"type":"object","properties":{}}}'
  exit 0
fi
cat > /dev/null
echo "something went wrong"
exit 1
"#,
        );

        let tools = load_custom_tools(&[dir.path().to_path_buf()]);
        assert_eq!(tools.len(), 1);

        let result = tools[0].execute(serde_json::json!({})).await;
        assert!(
            result.is_error,
            "expected is_error, got: {:?}",
            result.content.as_text()
        );
        assert!(
            result.content.as_text().contains("exit 1"),
            "expected 'exit 1' in content, got: {:?}",
            result.content.as_text()
        );
    }

    #[test]
    fn describe_missing_name_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let script_path = dir.path().join("no_name");
        write_script(
            &script_path,
            r#"#!/bin/sh
if [ "$1" = "--describe" ]; then
  printf '{"description":"No name here.","parameters_schema":{"type":"object","properties":{}}}'
  exit 0
fi
"#,
        );

        let tools = load_custom_tools(&[dir.path().to_path_buf()]);
        assert!(tools.is_empty());
    }
}
