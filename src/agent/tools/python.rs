use std::pin::Pin;
use std::process::Stdio;

use serde_json::Value;

use super::subprocess::SubprocessCommand;
use crate::agent::types::{Tool, ToolCallContext, ToolResult};

/// Which Python runtime was detected at registration time.
#[derive(Debug, Clone)]
pub(crate) enum PythonRuntime {
    /// `uv` is available; run scripts as `uv run python -`.
    Uv { version: String },
    /// A bare `python` / `python3` binary is available.
    Native { cmd: String, version: String },
}

impl PythonRuntime {
    fn version(&self) -> &str {
        match self {
            PythonRuntime::Uv { version } => version,
            PythonRuntime::Native { version, .. } => version,
        }
    }

    fn is_uv(&self) -> bool {
        matches!(self, PythonRuntime::Uv { .. })
    }
}

/// Run `<program> [args...]` and return trimmed stdout, or `None` on failure.
async fn probe_version(program: &str, args: &[&str]) -> Option<String> {
    let output = tokio::process::Command::new(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
    // Some versions print to stderr (Python 2 did), try that as fallback.
    if raw.is_empty() {
        let err = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if err.is_empty() { None } else { Some(err) }
    } else {
        Some(raw)
    }
}

/// Parse `"Python X.Y.Z"` → `"X.Y.Z"`, returning the input unchanged on mismatch.
fn strip_python_prefix(raw: &str) -> String {
    raw.strip_prefix("Python ").unwrap_or(raw).to_string()
}

/// Return the major version number from a version string like `"3.12.1"` or
/// `"Python 3.12.1"`, or `None` if it cannot be parsed.
fn major_version(raw: &str) -> Option<u32> {
    let s = raw.strip_prefix("Python ").unwrap_or(raw);
    s.split('.').next()?.parse().ok()
}

/// Detect the best available Python runtime.  Returns `None` if nothing usable
/// is found.
pub async fn detect_python() -> Option<PythonRuntime> {
    // 1. Prefer uv.
    if let Some(raw) = probe_version("uv", &["run", "python", "--version"]).await {
        let version = strip_python_prefix(&raw);
        log::debug!("python tool: detected uv with Python {version}");
        return Some(PythonRuntime::Uv { version });
    }

    // 2. Prefer `python` if it is Python 3+.
    if let Some(raw) = probe_version("python", &["--version"]).await {
        if major_version(&raw).unwrap_or(0) >= 3 {
            let version = strip_python_prefix(&raw);
            log::debug!("python tool: detected python ({version})");
            return Some(PythonRuntime::Native {
                cmd: "python".to_string(),
                version,
            });
        }
        log::debug!("python tool: `python` is Python 2, skipping");
    }

    // 3. Fall back to `python3`.
    if let Some(raw) = probe_version("python3", &["--version"]).await {
        let version = strip_python_prefix(&raw);
        log::debug!("python tool: detected python3 ({version})");
        return Some(PythonRuntime::Native {
            cmd: "python3".to_string(),
            version,
        });
    }

    log::debug!("python tool: no Python runtime found, tool not registered");
    None
}

pub struct PythonTool {
    runtime: PythonRuntime,
    description: String,
    schema: Value,
}

impl PythonTool {
    pub fn new(runtime: PythonRuntime) -> Self {
        let version = runtime.version().to_string();
        let uv = runtime.is_uv();

        let with_desc = if uv {
            " Supports running with specified dependencies."
        } else {
            ""
        };

        let description = format!(
            "Run an ad-hoc Python {version} script. \
             Provide the script as a string; it is piped to the interpreter via stdin. \
             Stdout, stderr, and exit code are returned.{with_desc}"
        );

        let mut properties = serde_json::json!({
            "script": {
                "type": "string",
                "description": "Python script source code to execute"
            }
        });

        if uv {
            properties["with"] = serde_json::json!({
                "type": "array",
                "items": { "type": "string" },
                "description": "Packages to install for this run"
            });
        }

        let schema = serde_json::json!({
            "type": "object",
            "properties": properties,
            "required": ["script"]
        });

        Self {
            runtime,
            description,
            schema,
        }
    }
}

#[derive(serde::Deserialize)]
struct PythonArgs {
    script: String,
    #[serde(default)]
    with: Vec<String>,
}

impl Tool for PythonTool {
    fn name(&self) -> &str {
        "python"
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> Value {
        self.schema.clone()
    }

    fn streaming_field(&self) -> Option<&'static str> {
        Some("script")
    }

    fn run(
        &self,
        args: Value,
        ctx: ToolCallContext,
    ) -> Pin<Box<dyn std::future::Future<Output = ToolResult> + Send + '_>> {
        Box::pin(async move {
            let PythonArgs { script, with } = match super::parse_args(args) {
                Ok(a) => a,
                Err(e) => return *e,
            };

            let cmd = match &self.runtime {
                PythonRuntime::Uv { .. } => {
                    let mut c = SubprocessCommand::new("uv").arg("run");
                    for pkg in &with {
                        c = c.arg("--with").arg(pkg);
                    }
                    c.arg("python").arg("-u").arg("-")
                }
                PythonRuntime::Native {
                    cmd: python_cmd, ..
                } => SubprocessCommand::new(python_cmd).arg("-u").arg("-"),
            };

            cmd.stdin_data(script.into_bytes()).run(ctx).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::types::Tool;

    fn make_tool() -> Option<PythonTool> {
        // Use `python` or `python3` directly — skip uv to keep tests hermetic.
        for cmd in &["python", "python3"] {
            if let Ok(out) = std::process::Command::new(cmd).arg("--version").output()
                && out.status.success()
            {
                let raw = String::from_utf8_lossy(&out.stdout).trim().to_string();
                let raw = if raw.is_empty() {
                    String::from_utf8_lossy(&out.stderr).trim().to_string()
                } else {
                    raw
                };
                if major_version(&raw).unwrap_or(0) >= 3 {
                    let version = strip_python_prefix(&raw);
                    return Some(PythonTool::new(PythonRuntime::Native {
                        cmd: cmd.to_string(),
                        version,
                    }));
                }
            }
        }
        None
    }

    #[tokio::test]
    async fn python_captures_stdout() {
        let Some(tool) = make_tool() else { return };
        let result = tool
            .execute(serde_json::json!({"script": "print('hello')"}))
            .await;
        assert!(!result.is_error, "{}", result.content.as_text());
        assert!(result.content.as_text().contains("hello"));
    }

    #[tokio::test]
    async fn python_captures_stderr() {
        let Some(tool) = make_tool() else { return };
        let result = tool
            .execute(serde_json::json!({"script": "import sys; sys.stderr.write('oops\\n')"}))
            .await;
        assert!(!result.is_error, "{}", result.content.as_text());
        assert!(result.content.as_text().contains("oops"));
    }

    #[tokio::test]
    async fn python_nonzero_exit() {
        let Some(tool) = make_tool() else { return };
        let result = tool
            .execute(serde_json::json!({"script": "import sys; sys.exit(42)"}))
            .await;
        assert!(!result.is_error);
        assert!(result.content.as_text().contains("exit 42"));
    }

    #[tokio::test]
    async fn python_zero_exit_omits_exit_line() {
        let Some(tool) = make_tool() else { return };
        let result = tool
            .execute(serde_json::json!({"script": "print('ok')"}))
            .await;
        assert!(!result.is_error);
        assert!(result.content.as_text().contains("ok"));
        assert!(!result.content.as_text().contains("exit 0"));
    }

    #[tokio::test]
    async fn python_missing_script_is_error() {
        let Some(tool) = make_tool() else { return };
        let result = tool.execute(serde_json::json!({})).await;
        assert!(result.is_error);
        assert!(result.content.as_text().contains("Invalid arguments"));
    }

    #[test]
    fn with_absent_from_schema_without_uv() {
        let Some(tool) = make_tool() else { return };
        let schema = tool.parameters_schema();
        let props = &schema["properties"];
        assert!(
            props.get("with").is_none(),
            "`with` should be absent without uv: {props}"
        );
    }

    #[test]
    fn with_present_in_schema_with_uv() {
        let tool = PythonTool::new(PythonRuntime::Uv {
            version: "3.13.0".to_string(),
        });
        let schema = tool.parameters_schema();
        let props = &schema["properties"];
        assert!(
            props.get("with").is_some(),
            "`with` should be present with uv: {props}"
        );
    }

    #[test]
    fn description_contains_version() {
        let Some(tool) = make_tool() else { return };
        assert!(
            tool.description().contains("Python 3."),
            "description: {}",
            tool.description()
        );
    }
}

#[cfg(test)]
mod version_tests {
    use super::*;

    #[test]
    fn strip_python_prefix_works() {
        assert_eq!(strip_python_prefix("Python 3.12.1"), "3.12.1");
        assert_eq!(strip_python_prefix("3.12.1"), "3.12.1");
    }

    #[test]
    fn major_version_parses_correctly() {
        assert_eq!(major_version("Python 3.12.1"), Some(3));
        assert_eq!(major_version("Python 2.7.18"), Some(2));
        assert_eq!(major_version("3.11.0"), Some(3));
        assert_eq!(major_version("garbage"), None);
    }
}
