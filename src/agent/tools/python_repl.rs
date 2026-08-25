use std::pin::Pin;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::Child;
use tokio::sync::Mutex;

use super::python::PythonRuntime;
use super::terminal::apply_terminal_render;
use super::truncate::truncate_tail;
use crate::agent::types::{CancelLevel, Tool, ToolCallContext, ToolResult};
use crate::process::DetachFromTty;

const KERNEL_SOURCE: &str = include_str!("python_repl_kernel.py");
const KERNEL_BOOTSTRAP: &str =
    "import sys; exec(compile(sys.argv[1], '<xi-python-repl-kernel>', 'exec'))";
const PROTOCOL_VERSION: u32 = 1;
const DEFAULT_TIMEOUT_MS: u64 = 10_000;
const MAX_FRAME_BYTES: usize = 1024 * 1024;

#[derive(Default)]
struct PythonReplState {
    kernel: Option<PythonKernel>,
    dependencies: Vec<String>,
    dependencies_configured: bool,
}

#[derive(Default)]
pub struct PythonReplSession {
    state: Mutex<PythonReplState>,
}

impl PythonReplSession {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn shutdown(&self) {
        let mut state = self.state.lock().await;
        if let Some(mut kernel) = state.kernel.take() {
            kernel.stop().await;
        }
    }
}

pub struct PythonReplTool {
    runtime: PythonRuntime,
    uv_available: bool,
    description: String,
}

impl PythonReplTool {
    pub fn new(runtime: PythonRuntime, uv_available: bool) -> Self {
        let dependency_description = if uv_available {
            " Optional `with` dependencies are installed by uv when the kernel starts. Adding a dependency to an existing kernel requires reset=true; omitted or already-active dependencies need not be repeated."
        } else {
            ""
        };
        let description = format!(
            "Execute Python {} code in a persistent REPL scoped to the current agent loop. \
             Variables and imports persist between calls. The final expression is returned. \
             Set reset=true to discard existing state before execution. Top-level await is supported.{dependency_description}",
            runtime.version()
        );
        Self {
            runtime,
            uv_available,
            description,
        }
    }
}

#[derive(Deserialize)]
struct PythonReplArgs {
    code: String,
    #[serde(default)]
    reset: bool,
    #[serde(default)]
    timeout_ms: Option<u64>,
    #[serde(default)]
    with: Option<Vec<String>>,
}

impl Tool for PythonReplTool {
    fn name(&self) -> &str {
        "python_repl_execute"
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> Value {
        let mut properties = serde_json::json!({
            "code": { "type": "string", "description": "Python code to execute" },
            "reset": { "type": "boolean", "description": "Discard existing REPL state before execution" },
            "timeout_ms": { "type": "integer", "minimum": 1, "description": "Execution timeout in milliseconds" }
        });
        if self.uv_available {
            properties["with"] = serde_json::json!({
                "type": "array",
                "items": { "type": "string", "minLength": 1 },
                "description": "Dependencies required by this code. Existing dependencies need not be repeated. Adding dependencies to a running kernel requires reset=true."
            });
        }
        serde_json::json!({
            "type": "object",
            "properties": properties,
            "required": ["code"]
        })
    }

    fn streaming_field(&self) -> Option<&'static str> {
        Some("code")
    }

    fn run(
        &self,
        args: Value,
        ctx: ToolCallContext,
    ) -> Pin<Box<dyn std::future::Future<Output = ToolResult> + Send + '_>> {
        Box::pin(async move {
            let PythonReplArgs {
                code,
                reset,
                timeout_ms,
                with,
            } = match super::parse_args(args) {
                Ok(args) => args,
                Err(error) => return *error,
            };
            let requested = match normalize_dependencies(with) {
                Ok(dependencies) => dependencies,
                Err(error) => return ToolResult::err(error),
            };
            if requested.is_some() && !self.uv_available {
                return ToolResult::err(
                    "Python REPL dependencies require uv, but uv is not available on this host.",
                );
            }
            let Some(session) = ctx.python_repl.as_ref() else {
                return ToolResult::err("Python REPL is unavailable outside an agent loop");
            };
            let mut state = session.state.lock().await;
            if !reset
                && let Some(requested) = requested.as_ref()
                && !requested
                    .iter()
                    .all(|item| state.dependencies.contains(item))
                && (state.kernel.is_some() || state.dependencies_configured)
            {
                let added: Vec<_> = requested
                    .iter()
                    .filter(|item| !state.dependencies.contains(item))
                    .cloned()
                    .collect();
                return ToolResult::err(format!(
                    "Adding Python REPL dependencies requires reset=true. New dependencies: {}",
                    added.join(", ")
                ));
            }
            if reset {
                if let Some(mut kernel) = state.kernel.take() {
                    kernel.stop().await;
                }
                if let Some(requested) = requested {
                    state.dependencies = requested;
                    state.dependencies_configured = true;
                }
            } else if !state.dependencies_configured
                && let Some(requested) = requested
            {
                state.dependencies = requested;
                state.dependencies_configured = true;
            }
            if !state.dependencies_configured && state.kernel.is_none() {
                state.dependencies_configured = true;
            }
            if reset && code.is_empty() {
                return ToolResult::ok_str("Python REPL state was reset.");
            }
            if state.kernel.is_none() {
                match PythonKernel::start(&self.runtime, &state.dependencies).await {
                    Ok(kernel) => state.kernel = Some(kernel),
                    Err(error) => return ToolResult::err(error),
                }
            }
            let timeout = Duration::from_millis(timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS).max(1));
            let kernel = state.kernel.as_mut().expect("kernel was initialized");
            match kernel.execute(&code, timeout, &ctx).await {
                KernelOutcome::Completed(response) => {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    format_completed(kernel.take_output().await, response)
                }
                KernelOutcome::TimedOut => {
                    let output = kernel.take_output().await;
                    if let Some(mut dead) = state.kernel.take() {
                        dead.stop().await;
                    }
                    format_status(
                        output,
                        "Python execution timed out. The kernel was terminated and its state was discarded.",
                    )
                }
                KernelOutcome::Cancelled => {
                    let output = kernel.take_output().await;
                    if let Some(mut dead) = state.kernel.take() {
                        dead.stop().await;
                    }
                    format_status(
                        output,
                        "Python execution was cancelled. The kernel was terminated and its state was discarded.",
                    )
                }
                KernelOutcome::Exited(status) => {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    let output = kernel.take_output().await;
                    state.kernel = None;
                    format_status(output, &status)
                }
                KernelOutcome::ProtocolError(error) => {
                    if let Some(mut dead) = state.kernel.take() {
                        dead.stop().await;
                    }
                    ToolResult::err(format!("Python REPL protocol error: {error}"))
                }
            }
        })
    }
}

#[derive(Serialize)]
struct KernelRequest<'a> {
    id: u64,
    method: &'a str,
    code: &'a str,
}

#[derive(Deserialize)]
struct KernelResponse {
    id: u64,
    result: Option<String>,
    exception: Option<String>,
}

#[derive(Deserialize)]
struct KernelHello {
    token: String,
    protocol_version: u32,
    runtime: String,
    runtime_version: String,
}

struct CapturedOutput {
    stdout: String,
    stderr: String,
}

struct PythonKernel {
    child: Child,
    pid: Option<u32>,
    control: TcpStream,
    stdout: Arc<Mutex<Vec<u8>>>,
    stderr: Arc<Mutex<Vec<u8>>>,
    stdout_pos: usize,
    stderr_pos: usize,
    next_id: u64,
}

enum KernelOutcome {
    Completed(KernelResponse),
    TimedOut,
    Cancelled,
    Exited(String),
    ProtocolError(String),
}

impl PythonKernel {
    async fn start(runtime: &PythonRuntime, dependencies: &[String]) -> Result<Self, String> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|error| format!("Failed to bind Python REPL control socket: {error}"))?;
        let port = listener
            .local_addr()
            .map_err(|error| error.to_string())?
            .port();
        let token = random_token()?;
        let mut command = if dependencies.is_empty() {
            match runtime {
                PythonRuntime::Uv { .. } => {
                    let mut command = tokio::process::Command::new("uv");
                    command.args([
                        "run",
                        "--no-project",
                        "python",
                        "-u",
                        "-c",
                        KERNEL_BOOTSTRAP,
                        KERNEL_SOURCE,
                    ]);
                    command
                }
                PythonRuntime::Native { cmd, .. } => {
                    let mut command = tokio::process::Command::new(cmd);
                    command.args(["-u", "-c", KERNEL_BOOTSTRAP, KERNEL_SOURCE]);
                    command
                }
            }
        } else {
            let mut command = tokio::process::Command::new("uv");
            command.args(["run", "--no-project"]);
            for dependency in dependencies {
                command.arg("--with").arg(dependency);
            }
            command.args(["python", "-u", "-c", KERNEL_BOOTSTRAP, KERNEL_SOURCE]);
            command
        };
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .detach_from_tty()
            .env("XI_REPL_PORT", port.to_string())
            .env("XI_REPL_TOKEN", &token)
            .env("PYTHONUTF8", "1")
            .env("PYTHONIOENCODING", "utf-8");
        let mut child = command
            .spawn()
            .map_err(|error| format!("Failed to start Python REPL: {error}"))?;
        let pid = child.id();
        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take().expect("stderr was piped");
        let (mut control, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
            .await
            .map_err(|_| "Python REPL did not connect within 10 seconds".to_string())?
            .map_err(|error| format!("Failed to accept Python REPL connection: {error}"))?;
        let hello: KernelHello = read_frame(&mut control).await?;
        if hello.token != token
            || hello.protocol_version != PROTOCOL_VERSION
            || hello.runtime != "python"
        {
            let _ = child.kill().await;
            return Err("Python REPL handshake validation failed".to_string());
        }
        log::debug!("python repl: connected to Python {}", hello.runtime_version);
        let stdout_buffer = Arc::new(Mutex::new(Vec::new()));
        let stderr_buffer = Arc::new(Mutex::new(Vec::new()));
        spawn_reader(stdout, Arc::clone(&stdout_buffer));
        spawn_reader(stderr, Arc::clone(&stderr_buffer));
        Ok(Self {
            child,
            pid,
            control,
            stdout: stdout_buffer,
            stderr: stderr_buffer,
            stdout_pos: 0,
            stderr_pos: 0,
            next_id: 1,
        })
    }

    async fn execute(
        &mut self,
        code: &str,
        timeout: Duration,
        ctx: &ToolCallContext,
    ) -> KernelOutcome {
        let id = self.next_id;
        self.next_id += 1;
        let request = KernelRequest {
            id,
            method: "execute",
            code,
        };
        if let Err(error) = write_frame(&mut self.control, &request).await {
            return self.exit_or_protocol(error).await;
        }
        enum WaitResult {
            Response(Result<KernelResponse, String>),
            TimedOut,
            Cancelled,
        }
        let outcome = {
            let response = read_frame::<_, KernelResponse>(&mut self.control);
            tokio::pin!(response);
            let mut cancel_rx = ctx.cancel_rx.clone();
            let sleep = tokio::time::sleep(timeout);
            tokio::pin!(sleep);
            loop {
                tokio::select! {
                    result = &mut response => break WaitResult::Response(result),
                    _ = &mut sleep => break WaitResult::TimedOut,
                    changed = async {
                        match cancel_rx.as_mut() {
                            Some(rx) => rx.changed().await.ok(),
                            None => std::future::pending().await,
                        }
                    } => {
                        if changed.is_some()
                            && cancel_rx.as_ref().is_some_and(|rx| *rx.borrow() >= CancelLevel::HardAbort)
                        {
                            break WaitResult::Cancelled;
                        }
                    }
                }
            }
        };
        match outcome {
            WaitResult::Response(Ok(response)) if response.id == id => {
                KernelOutcome::Completed(response)
            }
            WaitResult::Response(Ok(_)) => {
                KernelOutcome::ProtocolError("response ID did not match request".to_string())
            }
            WaitResult::Response(Err(error)) => self.exit_or_protocol(error).await,
            WaitResult::TimedOut => KernelOutcome::TimedOut,
            WaitResult::Cancelled => KernelOutcome::Cancelled,
        }
    }

    async fn exit_or_protocol(&mut self, error: String) -> KernelOutcome {
        match self.child.try_wait() {
            Ok(Some(status)) => KernelOutcome::Exited(format_exit(status)),
            Ok(None) => {
                match tokio::time::timeout(Duration::from_millis(100), self.child.wait()).await {
                    Ok(Ok(status)) => KernelOutcome::Exited(format_exit(status)),
                    Ok(Err(wait_error)) => KernelOutcome::ProtocolError(format!(
                        "{error}; process status failed: {wait_error}"
                    )),
                    Err(_) => KernelOutcome::ProtocolError(error),
                }
            }
            Err(wait_error) => KernelOutcome::ProtocolError(format!(
                "{error}; process status failed: {wait_error}"
            )),
        }
    }

    async fn take_output(&mut self) -> CapturedOutput {
        let stdout = {
            let buffer = self.stdout.lock().await;
            let bytes = buffer[self.stdout_pos.min(buffer.len())..].to_vec();
            self.stdout_pos = buffer.len();
            String::from_utf8_lossy(&bytes).into_owned()
        };
        let stderr = {
            let buffer = self.stderr.lock().await;
            let bytes = buffer[self.stderr_pos.min(buffer.len())..].to_vec();
            self.stderr_pos = buffer.len();
            String::from_utf8_lossy(&bytes).into_owned()
        };
        CapturedOutput { stdout, stderr }
    }

    async fn stop(&mut self) {
        #[cfg(windows)]
        if let Some(pid) = self.pid {
            let _ = tokio::process::Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await;
        }
        #[cfg(unix)]
        if let Some(pid) = self.pid {
            // The child starts a detached session, so its PID is also its
            // process-group ID. A negative target terminates descendants too.
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
    }
}

fn spawn_reader<R>(mut reader: R, buffer: Arc<Mutex<Vec<u8>>>)
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut chunk = [0_u8; 8192];
        loop {
            match reader.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(count) => buffer.lock().await.extend_from_slice(&chunk[..count]),
            }
        }
    });
}

async fn write_frame<W, T>(writer: &mut W, value: &T) -> Result<(), String>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let body = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    if body.len() > MAX_FRAME_BYTES {
        return Err("protocol frame exceeds size limit".to_string());
    }
    writer
        .write_all(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes())
        .await
        .map_err(|error| error.to_string())?;
    writer
        .write_all(&body)
        .await
        .map_err(|error| error.to_string())?;
    writer.flush().await.map_err(|error| error.to_string())
}

async fn read_frame<R, T>(reader: &mut R) -> Result<T, String>
where
    R: AsyncRead + Unpin,
    T: for<'de> Deserialize<'de>,
{
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") {
        if header.len() >= 8192 {
            return Err("protocol header exceeds size limit".to_string());
        }
        let byte = reader.read_u8().await.map_err(|error| error.to_string())?;
        header.push(byte);
    }
    let header = std::str::from_utf8(&header).map_err(|error| error.to_string())?;
    let mut content_length = None;
    for line in header[..header.len() - 4].split("\r\n") {
        if let Some(value) = line.strip_prefix("Content-Length:") {
            content_length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .map_err(|_| "invalid Content-Length".to_string())?,
            );
        }
    }
    let length = content_length.ok_or_else(|| "missing Content-Length".to_string())?;
    if length > MAX_FRAME_BYTES {
        return Err("protocol frame exceeds size limit".to_string());
    }
    let mut body = vec![0; length];
    reader
        .read_exact(&mut body)
        .await
        .map_err(|error| error.to_string())?;
    serde_json::from_slice(&body).map_err(|error| error.to_string())
}

fn normalize_dependencies(with: Option<Vec<String>>) -> Result<Option<Vec<String>>, String> {
    let Some(dependencies) = with else {
        return Ok(None);
    };
    let mut normalized = Vec::new();
    for dependency in dependencies {
        let dependency = dependency.trim();
        if dependency.is_empty() {
            return Err("Python REPL dependencies must not be empty".to_string());
        }
        if !normalized.iter().any(|existing| existing == dependency) {
            normalized.push(dependency.to_string());
        }
    }
    Ok(Some(normalized))
}

fn random_token() -> Result<String, String> {
    let mut bytes = [0_u8; 32];
    getrandom::getrandom(&mut bytes).map_err(|error| error.to_string())?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn format_completed(output: CapturedOutput, response: KernelResponse) -> ToolResult {
    let mut sections = Vec::new();
    let stdout = sanitize(&output.stdout);
    let stderr = sanitize(&output.stderr);
    if !stdout.is_empty() {
        sections.push(stdout);
    }
    if !stderr.is_empty() {
        sections.push(stderr);
    }
    if let Some(exception) = response.exception {
        sections.push(sanitize(&exception));
    } else if let Some(result) = response.result {
        sections.push(sanitize(&result));
    }
    finish_result(sections.join("\n"), false)
}

fn format_status(output: CapturedOutput, status: &str) -> ToolResult {
    let mut sections = Vec::new();
    let stdout = sanitize(&output.stdout);
    let stderr = sanitize(&output.stderr);
    if !stdout.is_empty() {
        sections.push(stdout);
    }
    if !stderr.is_empty() {
        sections.push(stderr);
    }
    sections.push(status.to_string());
    finish_result(sections.join("\n"), false)
}

fn finish_result(content: String, is_error: bool) -> ToolResult {
    let truncation = truncate_tail(&content);
    if truncation.truncated {
        let mut result = ToolResult::ok_truncated(truncation, content, String::new());
        result.is_error = is_error;
        result
    } else if is_error {
        ToolResult::err(content)
    } else {
        ToolResult::ok(truncation)
    }
}

fn sanitize(value: &str) -> String {
    apply_terminal_render(value).trim_end().to_string()
}

fn format_exit(status: std::process::ExitStatus) -> String {
    if let Some(code) = status.code() {
        format!("Python kernel exited with code {code}. Its state was discarded.")
    } else {
        "Python kernel terminated without an exit code. Its state was discarded.".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_tool() -> Option<PythonReplTool> {
        super::super::python::detect_native_python()
            .map(|runtime| PythonReplTool::new(runtime, super::super::python::detect_uv()))
    }

    async fn run(
        tool: &PythonReplTool,
        session: &Arc<PythonReplSession>,
        args: Value,
    ) -> ToolResult {
        let mut ctx = ToolCallContext::noop("python-repl-test");
        ctx.python_repl = Some(Arc::clone(session));
        tool.run(args, ctx).await
    }

    #[tokio::test]
    async fn state_persists_and_reset_clears_it() {
        let Some(tool) = test_tool() else { return };
        let session = Arc::new(PythonReplSession::new());

        let first = run(&tool, &session, serde_json::json!({"code": "x = 40"})).await;
        assert!(!first.is_error, "{}", first.content.as_text());
        let second = run(&tool, &session, serde_json::json!({"code": "x + 2"})).await;
        assert_eq!(second.content.as_text(), "42");

        let reset = run(
            &tool,
            &session,
            serde_json::json!({"code": "", "reset": true}),
        )
        .await;
        assert_eq!(reset.content.as_text(), "Python REPL state was reset.");
        let missing = run(&tool, &session, serde_json::json!({"code": "x"})).await;
        assert!(missing.content.as_text().contains("NameError"));
        session.shutdown().await;
    }

    #[tokio::test]
    async fn captures_output_exception_and_top_level_await() {
        let Some(tool) = test_tool() else { return };
        let session = Arc::new(PythonReplSession::new());

        let output = run(
            &tool,
            &session,
            serde_json::json!({"code": "import sys\nprint('out')\nprint('err', file=sys.stderr)\n6 * 7"}),
        )
        .await;
        let text = output.content.as_text();
        assert!(text.contains("out"), "{text}");
        assert!(text.contains("err"), "{text}");
        assert!(text.ends_with("42"), "{text}");

        let awaited = run(
            &tool,
            &session,
            serde_json::json!({"code": "import asyncio\nawait asyncio.sleep(0)\n'awake'"}),
        )
        .await;
        assert_eq!(awaited.content.as_text(), "'awake'");

        let failed = run(&tool, &session, serde_json::json!({"code": "1 / 0"})).await;
        let traceback = failed.content.as_text();
        assert!(traceback.contains("ZeroDivisionError"));
        assert!(traceback.contains("<python_repl_execute code>"));
        assert!(traceback.contains("<xi-python-repl-kernel>"));
        assert!(!traceback.contains("<string>"));
        session.shutdown().await;
    }

    #[test]
    fn with_schema_depends_on_uv_availability() {
        let Some(runtime) = super::super::python::detect_native_python() else {
            return;
        };
        let without_uv = PythonReplTool::new(runtime.clone(), false).parameters_schema();
        assert!(without_uv["properties"].get("with").is_none());
        let with_uv = PythonReplTool::new(runtime, true).parameters_schema();
        assert!(with_uv["properties"].get("with").is_some());
    }

    #[tokio::test]
    async fn adding_dependencies_to_configured_kernel_requires_reset() {
        let Some(runtime) = super::super::python::detect_native_python() else {
            return;
        };
        let tool = PythonReplTool::new(runtime, true);
        let session = Arc::new(PythonReplSession::new());
        let initial = run(&tool, &session, serde_json::json!({"code": "1"})).await;
        assert_eq!(initial.content.as_text(), "1");

        let added = run(
            &tool,
            &session,
            serde_json::json!({"code": "", "with": ["example-package"]}),
        )
        .await;
        assert!(added.is_error);
        assert!(added.content.as_text().contains("requires reset=true"));

        let reset = run(
            &tool,
            &session,
            serde_json::json!({"code": "", "reset": true, "with": ["example-package"]}),
        )
        .await;
        assert!(!reset.is_error);
        let state = session.state.lock().await;
        assert_eq!(state.dependencies, ["example-package"]);
        assert!(state.kernel.is_none());
    }

    #[tokio::test]
    async fn timeout_discards_kernel_state() {
        let Some(tool) = test_tool() else { return };
        let session = Arc::new(PythonReplSession::new());
        let timed_out = run(
            &tool,
            &session,
            serde_json::json!({"code": "x = 1\nwhile True: pass", "timeout_ms": 50}),
        )
        .await;
        assert!(timed_out.content.as_text().contains("timed out"));
        let next = run(&tool, &session, serde_json::json!({"code": "x"})).await;
        assert!(next.content.as_text().contains("NameError"));
        session.shutdown().await;
    }
}
