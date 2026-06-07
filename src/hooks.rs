//! Agent-level hooks — user-defined commands that run at specific points in
//! the agent's main loop.
//!
//! Hooks are configured in `~/.config/xi/config.toml` as arrays of tables
//! under each hook point:
//!
//! ```toml
//! [[hooks.post_turn]]
//! bash = "mpg123 --quiet /home/user/sounds/done.mp3"
//! cwd = "/home/user"
//! timeout = 15
//!
//! [[hooks.post_turn]]
//! bash = "echo 'tool completed' >> /tmp/log"
//! include_tools = ["bash", "exec"]
//!
//! [[hooks.pre_tool]]
//! command = "/home/user/bin/notify-tool"
//! args = ["--verbose"]
//! timeout = 5
//! ```
//!
//! Each hook point supports multiple executions (array of tables). The runtime
//! runs all matching hooks for the point in order.
//!
//! Each hook supports multiple execution methods — the runtime picks the
//! first one that is both configured and available on the current platform:
//!
//! | Key          | How it runs               | Best for           |
//! |--------------|---------------------------|--------------------|
//! | `bash`       | `sh -c <value>`           | Linux / macOS      |
//! | `powershell` | `pwsh -c <value>`         | Windows            |
//! | `cmd`        | `cmd /c <value>`          | Windows (legacy)   |
//! | `command`    | Direct executable + `args`| Cross-platform     |
//!
//! Each hook fires synchronously: the agent loop waits for the hook to
//! complete (or time out) before proceeding.

use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::process::Command;

// ── HookPoint ─────────────────────────────────────────────────────────────────

/// The specific point in the agent loop where a hook fires.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookPoint {
    /// Just before a tool is executed.
    PreTool,
    /// Just after a tool completes.
    PostTool,
    /// At the start of a new user turn, before the LLM is called.
    /// Fires once per LLM cycle (may be multiple per user prompt).
    PreTurn,
    /// After the agent finishes responding to a turn (all tool calls in
    /// one LLM cycle are done).  May fire multiple times per user prompt
    /// if the model makes multiple tool-call cycles.
    PostTurn,
    /// When an unhandled error occurs during the agent loop.
    OnError,
    /// After the agent finishes its full response to a user prompt — fires
    /// once when the final answer has been delivered and the loop exits.
    OnDone,
}

impl std::fmt::Display for HookPoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PreTool => write!(f, "pre_tool"),
            Self::PostTool => write!(f, "post_tool"),
            Self::PreTurn => write!(f, "pre_turn"),
            Self::PostTurn => write!(f, "post_turn"),
            Self::OnError => write!(f, "on_error"),
            Self::OnDone => write!(f, "on_done"),
        }
    }
}

// ── HookConfig ────────────────────────────────────────────────────────────────

/// Configuration for a single hook, as defined in `config.toml`.
///
/// At least one of `bash`, `powershell`, `cmd`, or `command` must be set,
/// otherwise the hook is silently skipped.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookConfig {
    /// Shell command executed via `sh -c` (Linux / macOS).
    pub bash: Option<String>,

    /// Shell command executed via `pwsh -c` (Windows).
    pub powershell: Option<String>,

    /// Shell command executed via `cmd /c` (Windows).
    pub cmd: Option<String>,

    /// Absolute or `$PATH`-resolved executable path. The shell is **not**
    /// invoked; the executable is spawned directly with `args`.
    ///
    /// Used as a cross-platform fallback when no shell-specific key matches
    /// the current platform.
    pub command: Option<String>,

    /// Arguments passed to the `command` executable (ignored when a shell key
    /// like `bash` or `powershell` is used).
    #[serde(default)]
    pub args: Vec<String>,

    /// Working directory for the hook process. When `None`, inherits the
    /// agent process's working directory.
    pub cwd: Option<String>,

    /// When non-empty, the hook only fires when the current tool name is in
    /// this list (case-sensitive). Only meaningful for `pre_tool` and
    /// `post_tool` hooks; ignored for other hook points.
    #[serde(default)]
    pub include_tools: Vec<String>,

    /// When non-empty, the hook is skipped when the current tool name is in
    /// this list (case-sensitive). `include_tools` is checked first, then
    /// `exclude_tools` further narrows.
    #[serde(default)]
    pub exclude_tools: Vec<String>,

    /// Maximum seconds the hook is allowed to run. If exceeded, the hook
    /// process is killed (SIGTERM → SIGKILL after 2 s grace) and a warning is
    /// logged. The agent loop continues regardless.
    #[serde(default = "default_hook_timeout")]
    pub timeout: u64,
}

fn default_hook_timeout() -> u64 {
    30
}

impl Default for HookConfig {
    fn default() -> Self {
        Self {
            bash: None,
            powershell: None,
            cmd: None,
            command: None,
            args: Vec::new(),
            cwd: None,
            include_tools: Vec::new(),
            exclude_tools: Vec::new(),
            timeout: default_hook_timeout(),
        }
    }
}

impl HookConfig {
    /// Returns `true` when no execution method is configured at all.
    pub fn is_empty(&self) -> bool {
        self.bash.is_none()
            && self.powershell.is_none()
            && self.cmd.is_none()
            && self.command.is_none()
    }

    /// Resolve the (program, args) tuple to use for this hook on the current
    /// platform.  Resolution order:
    ///
    /// 1. `powershell` — `pwsh -c <script>` (Windows)
    /// 2. `cmd`        — `cmd /c <script>` (Windows)
    /// 3. `bash`       — `sh -c <script>`  (Unix / Windows Git Bash)
    /// 4. `command`    — direct executable + `args` (cross-platform fallback)
    fn resolved_program(&self) -> Option<(String, Vec<String>)> {
        #[cfg(windows)]
        {
            // Windows: powershell > cmd > bash > command
            if let Some(ref ps) = self.powershell {
                return Some(("pwsh".into(), vec!["-c".into(), ps.clone()]));
            }
            if let Some(ref cmd_script) = self.cmd {
                return Some(("cmd".into(), vec!["/c".into(), cmd_script.clone()]));
            }
            if let Some(ref sh) = self.bash {
                return Some(("sh".into(), vec!["-c".into(), sh.clone()]));
            }
        }

        #[cfg(not(windows))]
        {
            // Unix: bash > powershell > command
            if let Some(ref sh) = self.bash {
                return Some(("sh".into(), vec!["-c".into(), sh.clone()]));
            }
            if let Some(ref ps) = self.powershell {
                return Some(("pwsh".into(), vec!["-c".into(), ps.clone()]));
            }
        }

        // Cross-platform fallback: direct executable + args
        self.command
            .as_ref()
            .map(|cmd| (cmd.clone(), self.args.clone()))
    }
}

// ── HookContext ───────────────────────────────────────────────────────────────

/// Context data passed to a hook execution.
pub struct HookContext<'a> {
    /// Which hook point is being fired.
    pub point: HookPoint,
    /// Persistent session identifier.
    pub session_id: &'a str,
    /// Optional JSON payload to pipe via stdin.
    pub stdin_json: Option<serde_json::Value>,
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Human-readable label for the configured hook (for log messages).
fn hook_label(config: &HookConfig) -> String {
    if let Some(ref sh) = config.bash {
        format!("bash:{sh:.60}")
    } else if let Some(ref ps) = config.powershell {
        format!("powershell:{ps:.60}")
    } else if let Some(ref cmd) = config.cmd {
        format!("cmd:{cmd:.60}")
    } else if let Some(ref exe) = config.command {
        if config.args.is_empty() {
            exe.clone()
        } else {
            format!("{} {:?}", exe, config.args)
        }
    } else {
        "<empty>".into()
    }
}

/// Spawn a process, optionally pipe JSON to stdin, wait with timeout, and
/// handle timeout with SIGTERM → SIGKILL grace period.
async fn spawn_and_wait(
    program: &str,
    args: &[String],
    session_id: &str,
    point: HookPoint,
    stdin_body: Option<&str>,
    timeout_secs: u64,
    cwd: Option<&str>,
) {
    let mut cmd = Command::new(program);
    cmd.args(args);
    cmd.env("XI_HOOK_POINT", point.to_string());
    cmd.env("XI_SESSION_ID", session_id);

    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }

    if stdin_body.is_some() {
        cmd.stdin(std::process::Stdio::piped());
    }
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            log::warn!(
                "hooks: failed to spawn '{program}' ({label}): {e}",
                label = point,
            );
            return;
        }
    };

    // Write stdin if we have data.
    if let Some(body) = stdin_body
        && let Some(mut stdin) = child.stdin.take()
    {
        use tokio::io::AsyncWriteExt;
        if let Err(e) = stdin.write_all(body.as_bytes()).await {
            log::warn!(
                "hooks: failed to write stdin for '{program}' ({label}): {e}",
                label = point,
            );
        }
        // Dropping stdin closes it — child sees EOF.
    }

    // Wait with timeout.
    let timeout_dur = Duration::from_secs(timeout_secs);
    let started = std::time::Instant::now();

    match tokio::time::timeout(timeout_dur, child.wait()).await {
        Ok(Ok(_status)) => {
            // Exit code is intentionally ignored.
        }
        Ok(Err(e)) => {
            log::warn!(
                "hooks: '{program}' ({label}) failed to wait: {e}",
                label = point,
            );
        }
        Err(_elapsed) => {
            log::warn!(
                "hooks: '{program}' ({label}) timed out after {timeout_dur:?}",
                label = point,
            );
            let _ = child.start_kill();
            let grace = Duration::from_secs(2);
            let remaining = timeout_dur.saturating_sub(started.elapsed());
            let wait_grace = if remaining < grace { remaining } else { grace };
            tokio::time::timeout(wait_grace, child.wait()).await.ok();
            let _ = child.kill().await;
        }
    }
}

// ── run_hook ──────────────────────────────────────────────────────────────────

/// Execute a hook: resolve the program/args from the config, pass JSON context
/// via stdin, set environment variables, and enforce the timeout.
///
/// This function never fails the caller — all errors (missing binary, timeout,
/// execution failure) are logged as warnings and the function returns `()`.
pub async fn run_hook(config: &HookConfig, ctx: &HookContext<'_>) {
    let Some((program, args)) = config.resolved_program() else {
        log::warn!(
            "hooks: no command configured for {} (all keys empty)",
            ctx.point
        );
        return;
    };

    let label = hook_label(config);
    let stdin_body = ctx
        .stdin_json
        .as_ref()
        .map(|j| serde_json::to_string(j).unwrap_or_default());

    log::debug!("hooks: running {label}: {program} {args:?}", args = args,);

    spawn_and_wait(
        &program,
        &args,
        ctx.session_id,
        ctx.point,
        stdin_body.as_deref(),
        config.timeout,
        config.cwd.as_deref(),
    )
    .await;
}

// ── JSON payload builders ─────────────────────────────────────────────────────

/// Build the JSON value for a `pre_tool` hook stdin payload.
pub fn tool_json(name: &str, args: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "tool": name,
        "arguments": args,
    })
}

/// Build the JSON value for a `post_tool` hook stdin payload (includes result).
pub fn post_tool_json(
    name: &str,
    args: &serde_json::Value,
    is_error: bool,
    output_truncated: bool,
) -> serde_json::Value {
    serde_json::json!({
        "tool": name,
        "arguments": args,
        "exit_code": if is_error { 1 } else { 0 },
        "output_truncated": output_truncated,
    })
}

/// Build the JSON value for an `on_error` hook stdin payload.
pub fn on_error_json(
    error: &str,
    tool: Option<&str>,
    args: Option<&serde_json::Value>,
) -> serde_json::Value {
    let mut payload = serde_json::json!({
        "error": error,
    });
    if let Some(t) = tool {
        payload["tool"] = serde_json::Value::String(t.to_string());
    }
    if let Some(a) = args {
        payload["arguments"] = a.clone();
    }
    payload
}

// ── Convenience: run tool hook ────────────────────────────────────────────────

/// Check whether a hook should fire for the given tool name based on
/// `include_tools` and `exclude_tools` filters.
///
/// - `include_tools` empty  → no positive filter (include all)
/// - `include_tools` non-empty → only included tools match
/// - `exclude_tools` non-empty → matching tools are removed from the set
pub fn matches_tool(config: &HookConfig, tool_name: &str) -> bool {
    // include filter: if non-empty, tool must be in the list
    if !config.include_tools.is_empty() && !config.include_tools.iter().any(|t| t == tool_name) {
        return false;
    }
    // exclude filter: if non-empty, tool must NOT be in the list
    if !config.exclude_tools.is_empty() && config.exclude_tools.iter().any(|t| t == tool_name) {
        return false;
    }
    true
}

/// Look up the hooks for `point` in `hooks` and run all matching configs.
/// This is a no-op when no hook is configured for that point.
pub async fn maybe_run_hook(
    hooks: &HashMap<HookPoint, Vec<HookConfig>>,
    point: HookPoint,
    session_id: &str,
    stdin_json: Option<serde_json::Value>,
    tool_name: Option<&str>, // None for non-tool hooks (no tool filter applied)
) {
    let Some(configs) = hooks.get(&point) else {
        return;
    };
    for config in configs {
        if config.is_empty() {
            continue;
        }
        // Apply tool-name filter (only for pre_tool / post_tool).
        if let Some(name) = tool_name
            && !matches_tool(config, name)
        {
            continue;
        }
        let ctx = HookContext {
            point,
            session_id,
            stdin_json: stdin_json.clone(),
        };
        run_hook(config, &ctx).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── HookPoint ─────────────────────────────────────────────────────────

    #[test]
    fn hook_point_display_and_deserialize() {
        let cases = [
            (HookPoint::PreTool, "pre_tool"),
            (HookPoint::PostTool, "post_tool"),
            (HookPoint::PreTurn, "pre_turn"),
            (HookPoint::PostTurn, "post_turn"),
            (HookPoint::OnError, "on_error"),
            (HookPoint::OnDone, "on_done"),
        ];
        for (point, expected) in &cases {
            assert_eq!(point.to_string(), *expected);
            let deserialized: HookPoint = serde_json::from_str(&format!("\"{expected}\"")).unwrap();
            assert_eq!(deserialized, *point);
        }
    }

    // ── HookConfig defaults ───────────────────────────────────────────────

    #[test]
    fn hook_config_default_is_empty() {
        let cfg = HookConfig::default();
        assert!(cfg.is_empty());
        assert_eq!(cfg.timeout, 30);
    }

    #[test]
    fn hook_config_bash_only() {
        let toml_str = r#"
bash = "echo hello"
"#;
        let cfg: HookConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.bash.as_deref(), Some("echo hello"));
        assert!(cfg.powershell.is_none());
        assert!(cfg.cmd.is_none());
        assert!(cfg.command.is_none());
        assert!(cfg.args.is_empty());
        assert_eq!(cfg.timeout, 30);
        assert!(!cfg.is_empty());
    }

    #[test]
    fn hook_config_command_with_args() {
        let toml_str = r#"
command = "/bin/true"
args = ["--flag", "value"]
timeout = 5
"#;
        let cfg: HookConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.command.as_deref(), Some("/bin/true"));
        assert_eq!(cfg.args, vec!["--flag", "value"]);
        assert_eq!(cfg.timeout, 5);
    }

    #[test]
    fn hook_config_all_keys_toml() {
        let toml_str = r#"
bash = "echo hi"
powershell = "Write-Host hi"
cmd = "echo hi"
command = "/usr/bin/true"
args = ["-x"]
cwd = "/tmp"
timeout = 10
"#;
        let cfg: HookConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.bash.as_deref(), Some("echo hi"));
        assert_eq!(cfg.powershell.as_deref(), Some("Write-Host hi"));
        assert_eq!(cfg.cmd.as_deref(), Some("echo hi"));
        assert_eq!(cfg.command.as_deref(), Some("/usr/bin/true"));
        assert_eq!(cfg.args, vec!["-x"]);
        assert_eq!(cfg.cwd.as_deref(), Some("/tmp"));
        assert_eq!(cfg.timeout, 10);
    }

    #[test]
    fn hook_config_cwd_parses() {
        let toml_str = r#"
bash = "pwd"
cwd = "/tmp"
"#;
        let cfg: HookConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.bash.as_deref(), Some("pwd"));
        assert_eq!(cfg.cwd.as_deref(), Some("/tmp"));
    }

    // ── resolved_program ─────────────────────────────────────────────────

    #[test]
    fn resolved_program_bash_on_unix() {
        let cfg = HookConfig {
            bash: Some("echo hello".into()),
            ..Default::default()
        };
        let (prog, args) = cfg.resolved_program().expect("resolved");
        // On Unix: sh -c "echo hello"
        assert_eq!(prog, "sh");
        assert_eq!(args, vec!["-c", "echo hello"]);
    }

    #[test]
    fn resolved_program_command_fallback() {
        let cfg = HookConfig {
            command: Some("/bin/true".into()),
            args: vec!["--quiet".into()],
            ..Default::default()
        };
        let (prog, args) = cfg.resolved_program().expect("resolved");
        assert_eq!(prog, "/bin/true");
        assert_eq!(args, vec!["--quiet"]);
    }

    #[test]
    fn resolved_program_empty_returns_none() {
        let cfg = HookConfig::default();
        assert!(cfg.resolved_program().is_none());
    }

    // ── Serialization round-trip ──────────────────────────────────────────

    #[test]
    fn hook_config_round_trip_toml() {
        let cfg = HookConfig {
            bash: Some("echo hi".into()),
            command: Some("/bin/true".into()),
            args: vec!["-x".into()],
            timeout: 10,
            ..Default::default()
        };
        let toml_str = toml::to_string_pretty(&cfg).unwrap();
        let parsed: HookConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.bash, cfg.bash);
        assert_eq!(parsed.command, cfg.command);
        assert_eq!(parsed.args, cfg.args);
        assert_eq!(parsed.timeout, cfg.timeout);
    }

    // ── JSON payload builders ─────────────────────────────────────────────

    #[test]
    fn tool_json_format() {
        let json = tool_json("bash", &serde_json::json!({"command": "ls"}));
        assert_eq!(json["tool"], "bash");
        assert_eq!(json["arguments"]["command"], "ls");
    }

    #[test]
    fn post_tool_json_format() {
        let json = post_tool_json("bash", &serde_json::json!({"command": "ls"}), false, false);
        assert_eq!(json["tool"], "bash");
        assert_eq!(json["exit_code"], 0);
        assert_eq!(json["output_truncated"], false);
    }

    #[test]
    fn post_tool_json_error() {
        let json = post_tool_json(
            "bash",
            &serde_json::json!({"command": "ls /nonexistent"}),
            true,
            false,
        );
        assert_eq!(json["exit_code"], 1);
    }

    #[test]
    fn on_error_json_with_tool() {
        let json = on_error_json(
            "Tool execution failed",
            Some("bash"),
            Some(&serde_json::json!({"command": "ls /nonexistent"})),
        );
        assert_eq!(json["error"], "Tool execution failed");
        assert_eq!(json["tool"], "bash");
        assert_eq!(json["arguments"]["command"], "ls /nonexistent");
    }

    #[test]
    fn on_error_json_without_tool() {
        let json = on_error_json("Something went wrong", None, None);
        assert_eq!(json["error"], "Something went wrong");
        assert!(json.get("tool").is_none());
        assert!(json.get("arguments").is_none());
    }

    // ── hook_label ────────────────────────────────────────────────────────

    #[test]
    fn hook_label_bash() {
        let cfg = HookConfig {
            bash: Some("mpg123 file.mp3".into()),
            ..Default::default()
        };
        let label = hook_label(&cfg);
        assert!(label.starts_with("bash:mpg123"), "label={label}");
    }

    #[test]
    fn hook_label_command_with_args() {
        let cfg = HookConfig {
            command: Some("/usr/bin/mpg123".into()),
            args: vec!["file.mp3".into()],
            ..Default::default()
        };
        let label = hook_label(&cfg);
        assert!(label.starts_with("/usr/bin/mpg123"), "label={label}");
    }
}
