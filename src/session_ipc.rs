//! Shared-runtime IPC for interacting with a running xi session.
//!
//! Unix sockets are stored in a private per-user runtime directory. Their
//! fixed-length names are SHA-256 digests of canonical worktree paths. Clients
//! also try the legacy `.xi/xi.sock` endpoint for upgrade compatibility.
//! Windows uses local-only named pipes protected by a current-user DACL, with
//! names derived from the user SID and volume/directory file identity.

use crate::app_event::{AppEvent, AppEventTx};
#[cfg(unix)]
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::{mpsc, oneshot};

#[cfg(unix)]
type IpcStream = tokio::net::UnixStream;
#[cfg(unix)]
type IpcWriteHalf = tokio::net::unix::OwnedWriteHalf;
#[cfg(windows)]
use interprocess::os::windows::named_pipe::{
    PipeListenerOptions,
    pipe_mode::Bytes,
    tokio::{DuplexPipeStream, PipeListener, SendPipeStream},
};
#[cfg(windows)]
type IpcStream = DuplexPipeStream<Bytes>;
#[cfg(windows)]
type IpcWriteHalf = SendPipeStream<Bytes>;

fn split_stream(stream: IpcStream) -> (impl tokio::io::AsyncRead + Unpin + Send, IpcWriteHalf) {
    #[cfg(unix)]
    {
        stream.into_split()
    }
    #[cfg(windows)]
    {
        stream.split()
    }
}

pub const PROTOCOL_VERSION: u32 = 1;

pub type IpcReply = oneshot::Sender<Result<serde_json::Value, ErrorBody>>;
pub type PendingPrompt = (u64, String, IpcReply);

static NEXT_CONNECTION_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_EVENT_SEQ: AtomicU64 = AtomicU64::new(1);
const RESPONSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

pub fn new_turn_id() -> String {
    format!(
        "{}-{}",
        std::process::id(),
        NEXT_EVENT_SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

#[derive(Debug, serde::Deserialize)]
struct Request {
    id: serde_json::Value,
    op: String,
    #[serde(default)]
    params: serde_json::Value,
}

#[derive(Debug, serde::Serialize)]
struct Response {
    version: u32,
    id: serde_json::Value,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<ErrorBody>,
}

#[derive(Debug, serde::Serialize)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
}

#[derive(Debug)]
pub enum IpcCommand {
    Request {
        connection_id: u64,
        op: String,
        params: serde_json::Value,
        reply: oneshot::Sender<Result<serde_json::Value, ErrorBody>>,
    },
    Subscribe {
        connection_id: u64,
        events: mpsc::UnboundedSender<String>,
    },
    Disconnect {
        connection_id: u64,
    },
}

#[derive(Clone)]
pub struct CompletionPublisher {
    tx: mpsc::UnboundedSender<String>,
}

impl CompletionPublisher {
    pub(crate) fn from_sender(tx: mpsc::UnboundedSender<String>) -> Self {
        Self { tx }
    }

    pub fn publish(&self, event: String) {
        let _ = self.tx.send(event);
    }
}

#[cfg(unix)]
const SOCKET_NAME_PREFIX: &str = "s-";
#[cfg(unix)]
const MAX_SOCKET_PATH_BYTES: usize = 100;

#[cfg(unix)]
fn socket_identity(path: &Path) -> std::io::Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::symlink_metadata(path)?;
    Ok((metadata.dev(), metadata.ino()))
}

#[cfg(unix)]
fn recover_stale_socket(path: &Path) -> std::io::Result<bool> {
    if std::os::unix::net::UnixStream::connect(path).is_ok() {
        return Ok(false);
    }
    remove_stale_socket(path)
}

#[cfg(unix)]
fn remove_stale_socket(path: &Path) -> std::io::Result<bool> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(error) => return Err(error),
    };
    if !metadata.file_type().is_socket() || metadata.uid() != unsafe { libc::getuid() } {
        return Ok(false);
    }
    std::fs::remove_file(path)?;
    Ok(true)
}

#[cfg(unix)]
fn ensure_private_dir(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};

    let mut builder = std::fs::DirBuilder::new();
    builder.mode(0o700);
    match builder.create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.file_type().is_dir()
        || metadata.uid() != unsafe { libc::getuid() }
        || metadata.mode() & 0o077 != 0
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "runtime directory {} is not private and user-owned",
                path.display()
            ),
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn runtime_dir() -> std::io::Result<PathBuf> {
    let uid = unsafe { libc::getuid() };
    let xdg = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
    runtime_dir_from(
        xdg.as_deref(),
        &PathBuf::from(format!("/tmp/xi-{uid}")),
        uid,
    )
}

#[cfg(unix)]
fn runtime_dir_from(
    xdg_runtime: Option<&Path>,
    fallback: &Path,
    uid: u32,
) -> std::io::Result<PathBuf> {
    use std::os::unix::fs::MetadataExt;

    if let Some(base) = xdg_runtime
        && let Ok(metadata) = std::fs::metadata(base)
        && metadata.is_dir()
        && metadata.uid() == uid
    {
        let dir = base.join("xi");
        if ensure_private_dir(&dir).is_ok() {
            return Ok(dir);
        }
    }
    ensure_private_dir(fallback)?;
    Ok(fallback.to_path_buf())
}

#[cfg(unix)]
fn canonical_worktree(cwd: &Path) -> std::io::Result<PathBuf> {
    std::fs::canonicalize(cwd)
}

#[cfg(unix)]
fn shared_socket_path(cwd: &Path) -> std::io::Result<PathBuf> {
    shared_socket_path_with_runtime(cwd, &runtime_dir()?)
}

#[cfg(unix)]
fn shared_socket_path_with_runtime(cwd: &Path, runtime: &Path) -> std::io::Result<PathBuf> {
    let uid = unsafe { libc::getuid() };
    let fallback = PathBuf::from(format!("/tmp/xi-{uid}"));
    let path = socket_path_for(cwd, runtime, &fallback)?;
    if path.parent() == Some(fallback.as_path()) {
        ensure_private_dir(&fallback)?;
    }
    Ok(path)
}

#[cfg(unix)]
fn socket_path_for(cwd: &Path, runtime: &Path, fallback: &Path) -> std::io::Result<PathBuf> {
    let canonical = canonical_worktree(cwd)?;
    let mut hasher = Sha256::new();
    hasher.update(b"xi-session-ipc-v1\0");
    hasher.update(canonical.as_os_str().as_encoded_bytes());
    let name = format!("{SOCKET_NAME_PREFIX}{}", hex_digest(&hasher.finalize()));

    let preferred = runtime.join(&name);
    if preferred.as_os_str().as_encoded_bytes().len() <= MAX_SOCKET_PATH_BYTES {
        return Ok(preferred);
    }
    let fallback_path = fallback.join(name);
    if fallback_path.as_os_str().as_encoded_bytes().len() > MAX_SOCKET_PATH_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "runtime directory is too long for a Unix socket path",
        ));
    }
    Ok(fallback_path)
}

fn hex_digest(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut value = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(value, "{byte:02x}").expect("writing to String cannot fail");
    }
    value
}

#[cfg(unix)]
fn legacy_socket_path(cwd: &Path) -> PathBuf {
    cwd.join(".xi/xi.sock")
}

#[cfg(test)]
mod protocol_tests;
#[cfg(windows)]
mod windows;
#[cfg(all(test, windows))]
mod windows_tests;

pub struct IpcServer {
    #[cfg(windows)]
    accept_task: tokio::task::JoinHandle<()>,
    #[cfg(unix)]
    pub socket_path: PathBuf,
    #[cfg(unix)]
    socket_identity: (u64, u64),
}

impl IpcServer {
    /// Claim the endpoint. Returns `None` when another instance owns it.
    #[cfg(unix)]
    pub fn bind(cwd: &Path, command_tx: AppEventTx) -> std::io::Result<Option<Self>> {
        let socket_path = shared_socket_path(cwd)?;
        match std::os::unix::net::UnixListener::bind(&socket_path) {
            Ok(listener) => {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600))?;
                listener.set_nonblocking(true)?;
                let listener = tokio::net::UnixListener::from_std(listener)?;
                tokio::spawn(accept_loop(listener, command_tx.clone()));
                let socket_identity = socket_identity(&socket_path)?;
                Ok(Some(Self {
                    socket_path,
                    socket_identity,
                }))
            }
            Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
                // A live owner keeps the endpoint. An unreachable filesystem
                // socket is recoverable after the failed probe.
                let live = std::os::unix::net::UnixStream::connect(&socket_path).is_ok();
                if live {
                    Ok(None)
                } else {
                    if !recover_stale_socket(&socket_path)? {
                        return Ok(None);
                    }
                    match std::os::unix::net::UnixListener::bind(&socket_path) {
                        Ok(listener) => {
                            use std::os::unix::fs::PermissionsExt;
                            std::fs::set_permissions(
                                &socket_path,
                                std::fs::Permissions::from_mode(0o600),
                            )?;
                            listener.set_nonblocking(true)?;
                            let listener = tokio::net::UnixListener::from_std(listener)?;
                            tokio::spawn(accept_loop(listener, command_tx.clone()));
                            let socket_identity = socket_identity(&socket_path)?;
                            Ok(Some(Self {
                                socket_path,
                                socket_identity,
                            }))
                        }
                        Err(_) => Ok(None),
                    }
                }
            }
            Err(error) => Err(error),
        }
    }

    #[cfg(windows)]
    pub fn bind(cwd: &Path, command_tx: AppEventTx) -> std::io::Result<Option<Self>> {
        let endpoint = windows::endpoint(cwd)?;
        let mut options = PipeListenerOptions::new().path(endpoint.as_str());
        options.security_descriptor = Some(windows::security_descriptor()?);
        options.accept_remote = false;
        let listener = match options.create_tokio_duplex::<Bytes>() {
            Ok(listener) => listener,
            Err(error) if error.raw_os_error() == Some(5) => {
                // First-instance collisions also report access denied. A
                // nonwaiting probe distinguishes an accessible/busy owner from
                // an actual permission failure; it never waits inside bind().
                use interprocess::os::windows::named_pipe::DuplexPipeStream;
                match DuplexPipeStream::<Bytes>::connect_by_path_with_wait_mode(
                    endpoint.as_str(),
                    interprocess::ConnectWaitMode::Timeout(std::time::Duration::ZERO),
                ) {
                    Ok(_) => return Ok(None),
                    Err(probe) if probe.kind() == std::io::ErrorKind::TimedOut => return Ok(None),
                    Err(_) => return Err(error),
                }
            }
            Err(error) => return Err(error),
        };
        Ok(Some(Self {
            accept_task: tokio::spawn(accept_pipe_loop(listener, command_tx)),
        }))
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        #[cfg(windows)]
        self.accept_task.abort();
        #[cfg(unix)]
        if socket_identity(&self.socket_path).ok() == Some(self.socket_identity) {
            let _ = std::fs::remove_file(&self.socket_path);
        }
    }
}

#[cfg(unix)]
async fn accept_loop(listener: tokio::net::UnixListener, command_tx: AppEventTx) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            break;
        };
        let connection_id = NEXT_CONNECTION_ID.fetch_add(1, Ordering::Relaxed);
        tokio::spawn(connection(stream, command_tx.clone(), connection_id));
    }
}

#[cfg(windows)]
async fn accept_pipe_loop(listener: PipeListener<Bytes, Bytes>, command_tx: AppEventTx) {
    let mut connections = tokio::task::JoinSet::new();
    loop {
        let stream = match listener.accept().await {
            Ok(stream) => stream,
            Err(error) => {
                log::debug!("session IPC accept failed: {error}");
                break;
            }
        };
        let connection_id = NEXT_CONNECTION_ID.fetch_add(1, Ordering::Relaxed);
        connections.spawn(connection(stream, command_tx.clone(), connection_id));
        while connections.try_join_next().is_some() {}
    }
}

struct AbortOnDrop(tokio::task::AbortHandle);
impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

struct DisconnectGuard {
    command_tx: AppEventTx,
    connection_id: u64,
}

impl Drop for DisconnectGuard {
    fn drop(&mut self) {
        let _ = self.command_tx.send(AppEvent::Ipc(IpcCommand::Disconnect {
            connection_id: self.connection_id,
        }));
    }
}

async fn connection(stream: IpcStream, command_tx: AppEventTx, connection_id: u64) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let (read, mut write) = split_stream(stream);
    let _disconnect = DisconnectGuard {
        command_tx: command_tx.clone(),
        connection_id,
    };
    let (frames_tx, mut frames_rx) = mpsc::unbounded_channel();
    let read_task = tokio::spawn(async move {
        let mut reader = BufReader::new(read);
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line).await {
                Ok(0) => {
                    let _ = frames_tx.send(Err("peer disconnected".to_string()));
                    break;
                }
                Ok(_) => {
                    if frames_tx.send(Ok(line)).is_err() {
                        break;
                    }
                }
                Err(error) => {
                    let _ = frames_tx.send(Err(format!("read failed: {error}")));
                    break;
                }
            }
        }
    });
    let _read_task = AbortOnDrop(read_task.abort_handle());
    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<String>();
    let _ = command_tx.send(AppEvent::Ipc(IpcCommand::Subscribe {
        connection_id,
        events: event_tx.clone(),
    }));
    let mut pending = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            frame = frames_rx.recv() => {
                let line = match frame {
                    Some(Ok(line)) => line,
                    Some(Err(reason)) => { log::debug!("IPC connection {connection_id}: {reason}"); break; }
                    None => break,
                };
                let tx = command_tx.clone();
                pending.spawn(async move {
                    match serde_json::from_str::<Request>(line.trim()) {
                        Ok(request) => dispatch(request, &tx, connection_id).await,
                        Err(error) => error_response(serde_json::Value::Null, "invalid_request", &error.to_string()),
                    }
                });
            }
            Some(result) = pending.join_next(), if !pending.is_empty() => {
                let response = match result {
                    Ok(response) => response,
                    Err(error) => { log::error!("IPC dispatch failed: {error}"); break; }
                };
                let mut data = serde_json::to_vec(&response).expect("IPC response is serializable");
                data.push(b'\n');
                if let Err(error) = write.write_all(&data).await { log::debug!("IPC response write failed: {error}"); break; }
            }
            Some(event) = event_rx.recv() => {
                if let Err(error) = write.write_all(format!("{event}\n").as_bytes()).await { log::debug!("IPC event write failed: {error}"); break; }
            }
        }
    }
    pending.abort_all();
    while pending.join_next().await.is_some() {}
    // Dropping pending dispatches closes their reply receivers. DisconnectGuard
    // releases ownership and discards any prompt not yet submitted by the UI.
}

async fn dispatch(request: Request, tx: &AppEventTx, connection_id: u64) -> Response {
    let id = request.id.clone();
    let (reply_tx, reply_rx) = oneshot::channel();
    let command = IpcCommand::Request {
        connection_id,
        op: request.op,
        params: request.params,
        reply: reply_tx,
    };
    if tx.send(AppEvent::Ipc(command)).is_err() {
        return error_response(id, "unavailable", "session is no longer running");
    }
    match reply_rx.await {
        Ok(Ok(result)) => Response {
            version: PROTOCOL_VERSION,
            id,
            ok: true,
            result: Some(result),
            error: None,
        },
        Ok(Err(error)) => Response {
            version: PROTOCOL_VERSION,
            id,
            ok: false,
            result: None,
            error: Some(error),
        },
        Err(_) => error_response(id, "unavailable", "session stopped responding"),
    }
}

fn error_response(id: serde_json::Value, code: &str, message: &str) -> Response {
    Response {
        version: PROTOCOL_VERSION,
        id,
        ok: false,
        result: None,
        error: Some(ErrorBody {
            code: code.into(),
            message: message.into(),
        }),
    }
}

pub fn control_revoked_event(session_id: &str, turn_id: Option<&str>) -> String {
    serde_json::json!({
        "version": PROTOCOL_VERSION,
        "seq": NEXT_EVENT_SEQ.fetch_add(1, Ordering::Relaxed),
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "session_id": session_id,
        "event": "control.revoked",
        "payload": { "reason": "user_took_control", "pending_input_dropped": true, "turn_id": turn_id }
    })
    .to_string()
}

pub fn completion_event(
    session_id: &str,
    turn_id: &str,
    status: &str,
    log_path: &Path,
    response: Option<&crate::session_event::SessionEvent>,
    error: Option<&str>,
) -> String {
    let response = response.and_then(|event| match event {
        crate::session_event::SessionEvent::AssistantMessage {
            content,
            thinking,
            phase,
            usage,
            ..
        } => Some(serde_json::json!({
            "content": content,
            "thinking": thinking,
            "phase": phase,
            "usage": usage,
        })),
        _ => None,
    });
    serde_json::json!({
        "version": PROTOCOL_VERSION,
        "seq": NEXT_EVENT_SEQ.fetch_add(1, Ordering::Relaxed),
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "session_id": session_id,
        "event": "agent.completed",
        "payload": {
            "turn_id": turn_id,
            "status": status,
            "log_path": log_path,
            "response": response,
            "error": error,
        }
    })
    .to_string()
}

#[cfg(unix)]
async fn connect_session(cwd: &str) -> Result<tokio::net::UnixStream, String> {
    let cwd = Path::new(cwd);
    let shared = shared_socket_path(cwd).map_err(|error| error.to_string())?;
    match tokio::net::UnixStream::connect(&shared).await {
        Ok(stream) => Ok(stream),
        Err(shared_error) => {
            let legacy = legacy_socket_path(cwd);
            tokio::net::UnixStream::connect(&legacy).await.map_err(|_| {
                format!(
                    "unavailable: no running xi session at {} ({shared_error})",
                    shared.display()
                )
            })
        }
    }
}

#[cfg(windows)]
async fn connect_session(cwd: &str) -> Result<IpcStream, String> {
    let endpoint = windows::endpoint(Path::new(cwd)).map_err(|e| e.to_string())?;
    IpcStream::connect_by_path_with_wait_mode(
        endpoint.as_str(),
        interprocess::ConnectWaitMode::Timeout(std::time::Duration::from_secs(2)),
    )
    .await
    .map_err(|e| {
        let code = match e.kind() {
            std::io::ErrorKind::PermissionDenied => "permission_denied",
            std::io::ErrorKind::TimedOut => "connection_timeout",
            std::io::ErrorKind::NotFound => "unavailable",
            _ => "connection_error",
        };
        format!("{code}: cannot connect to xi session at {endpoint}: {e}")
    })
}

#[cfg(unix)]
fn client_key(cwd: &Path) -> std::io::Result<PathBuf> {
    canonical_worktree(cwd)
}

#[cfg(windows)]
fn client_key(cwd: &Path) -> std::io::Result<PathBuf> {
    windows::endpoint(cwd).map(PathBuf::from)
}

/// Send one request to a running worktree session.
pub async fn client_call(
    cwd: &str,
    op: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let stream = connect_session(cwd).await?;
        let (read, mut write) = split_stream(stream);
        let request = serde_json::json!({"id": 1, "op": op, "params": params});
        write
            .write_all(format!("{}\n", request).as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        let mut line = String::new();
        tokio::time::timeout(RESPONSE_TIMEOUT, BufReader::new(read).read_line(&mut line))
            .await
            .map_err(|_| {
                "timeout: IPC response timed out; request may have been accepted".to_string()
            })?
            .map_err(|e| format!("connection_error: {e}"))?;
        let response: serde_json::Value =
            serde_json::from_str(line.trim()).map_err(|e| e.to_string())?;
        response_result(&response)
    }
}

pub fn display_path(path: &str) -> String {
    if let Some(unc) = path.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{unc}");
    }
    if let Some(drive) = path.strip_prefix(r"\\?\")
        && drive.as_bytes().get(1) == Some(&b':')
    {
        return drive.to_owned();
    }
    path.to_owned()
}

fn response_result(response: &serde_json::Value) -> Result<serde_json::Value, String> {
    if response.get("version").and_then(serde_json::Value::as_u64) != Some(PROTOCOL_VERSION.into())
    {
        return Err("protocol_error: unsupported or missing IPC protocol version".into());
    }
    if response.get("ok") == Some(&serde_json::Value::Bool(true)) {
        Ok(response
            .get("result")
            .cloned()
            .unwrap_or(serde_json::Value::Null))
    } else {
        let code = response
            .pointer("/error/code")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("ipc_error");
        let message = response
            .pointer("/error/message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("IPC request failed");
        Err(format!("{code}: {message}"))
    }
}

struct PersistentClient {
    accepted_turn_id: Mutex<Option<String>>,
    closed: std::sync::atomic::AtomicBool,
    reader_task: Mutex<Option<tokio::task::AbortHandle>>,
    exchange: tokio::sync::Mutex<()>,
    writer: tokio::sync::Mutex<Option<IpcWriteHalf>>,
    incoming:
        tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<Result<serde_json::Value, String>>>,
    notification_tx: AppEventTx,
    cwd: String,
}

static CLIENTS: OnceLock<Mutex<HashMap<PathBuf, Arc<PersistentClient>>>> = OnceLock::new();

async fn get_persistent_client(
    cwd: &str,
    app_event_tx: &AppEventTx,
) -> Result<Arc<PersistentClient>, String> {
    let clients = CLIENTS.get_or_init(|| Mutex::new(HashMap::new()));
    let canonical_cwd = client_key(Path::new(cwd)).map_err(|error| error.to_string())?;
    let notification_cwd = std::fs::canonicalize(cwd).map_err(|error| error.to_string())?;
    if let Some(client) = clients
        .lock()
        .expect("IPC client map poisoned")
        .get(&canonical_cwd)
        .cloned()
    {
        return Ok(client);
    }
    let stream = connect_session(cwd).await?;
    let (read, writer) = split_stream(stream);
    let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
    let event_tx = app_event_tx.clone();
    let event_cwd = canonical_cwd.clone();
    let client = Arc::new(PersistentClient {
        accepted_turn_id: Mutex::new(None),
        closed: std::sync::atomic::AtomicBool::new(false),
        reader_task: Mutex::new(None),
        exchange: tokio::sync::Mutex::new(()),
        writer: tokio::sync::Mutex::new(Some(writer)),
        incoming: tokio::sync::Mutex::new(incoming_rx),
        notification_tx: app_event_tx.clone(),
        cwd: notification_cwd.to_string_lossy().into_owned(),
    });
    clients
        .lock()
        .expect("IPC client map poisoned")
        .insert(canonical_cwd, Arc::clone(&client));
    let reader_client = Arc::downgrade(&client);
    let reader_task = tokio::spawn(async move {
        use tokio::io::{AsyncBufReadExt, BufReader};
        let mut reader = BufReader::new(read);
        let mut line = String::new();
        let failure = loop {
            match reader.read_line(&mut line).await {
                Ok(0) => break Some("connection_error: session disconnected before completion; accepted work may still be running".to_string()),
                Ok(_) => {},
                Err(error) => break Some(format!("connection_error: IPC read failed: {error}")),
            }
            let value = match serde_json::from_str::<serde_json::Value>(line.trim()) {
                Ok(value) => value,
                Err(error) => break Some(format!("protocol_error: invalid IPC response: {error}")),
            };
            {
                if value.get("event").is_some() {
                    let terminal = matches!(
                        value.get("event").and_then(serde_json::Value::as_str),
                        Some("agent.completed" | "control.revoked")
                    );
                    let _ = event_tx.send(AppEvent::IpcNotification {
                        cwd: notification_cwd.to_string_lossy().into_owned(),
                        event: value,
                    });
                    if terminal {
                        break None;
                    }
                } else {
                    // Record correlation before forwarding the reply: EOF may
                    // follow immediately, before the requesting task resumes.
                    if value.get("ok") == Some(&serde_json::Value::Bool(true))
                        && value.pointer("/result/accepted") == Some(&serde_json::Value::Bool(true))
                        && let Some(turn_id) = value
                            .pointer("/result/turn_id")
                            .and_then(serde_json::Value::as_str)
                        && let Some(client) = reader_client.upgrade()
                    {
                        *client
                            .accepted_turn_id
                            .lock()
                            .expect("IPC turn ID poisoned") = Some(turn_id.to_owned());
                    }
                    let _ = incoming_tx.send(Ok(value));
                }
            }
            line.clear();
        };
        if let Some(reason) = failure {
            let _ = incoming_tx.send(Err(reason.clone()));
            if let Some(client) = reader_client.upgrade() {
                report_connection_failure(&client, &reason);
            }
        }
        if let Some(client) = reader_client.upgrade() {
            client.closed.store(true, Ordering::Relaxed);
            client.writer.lock().await.take();
        }
        // EOF, read errors and terminal events all invalidate the cached client.
        // An old reader must not evict a newer connection for the same endpoint.
        if let Some(clients) = CLIENTS.get() {
            let mut clients = clients.lock().expect("IPC client map poisoned");
            if clients
                .get(&event_cwd)
                .is_some_and(|current| std::ptr::eq(Arc::as_ptr(current), reader_client.as_ptr()))
            {
                clients.remove(&event_cwd);
            }
        }
    });
    *client.reader_task.lock().expect("IPC reader task poisoned") =
        Some(reader_task.abort_handle());
    Ok(client)
}

fn report_connection_failure(client: &PersistentClient, reason: &str) {
    log::warn!("IPC {}: {reason}", display_path(&client.cwd));
    let _ = client.notification_tx.send(AppEvent::IpcNotification {
        cwd: client.cwd.clone(),
        event: serde_json::json!({"event":"connection.failed", "payload": {"turn_id":client.accepted_turn_id.lock().expect("IPC turn ID poisoned").clone(), "error":reason, "accepted_work_may_continue":true}}),
    });
}

struct ExchangeGuard<'a> {
    client: &'a Arc<PersistentClient>,
    armed: bool,
}
impl Drop for ExchangeGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            report_connection_failure(
                self.client,
                "cancelled: IPC exchange cancelled; acceptance is unknown; not retried",
            );
            invalidate_client(self.client);
        }
    }
}

fn invalidate_client(client: &Arc<PersistentClient>) {
    client.closed.store(true, Ordering::Relaxed);
    // Cancellation drops the exchange future's writer lock before this guard.
    // If another owner briefly holds it, arrange closure rather than retaining
    // a live controller connection in a closed client object.
    if let Ok(mut writer) = client.writer.try_lock() {
        writer.take();
    } else {
        let client = Arc::clone(client);
        tokio::spawn(async move {
            client.writer.lock().await.take();
        });
    }
    if let Some(task) = client
        .reader_task
        .lock()
        .expect("IPC reader task poisoned")
        .take()
    {
        task.abort();
    }
    if let Some(clients) = CLIENTS.get() {
        clients
            .lock()
            .expect("IPC client map poisoned")
            .retain(|_, cached| !Arc::ptr_eq(cached, client));
    }
}

async fn persistent_request(
    client: &Arc<PersistentClient>,
    op: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    persistent_request_with_timeout(client, op, params, RESPONSE_TIMEOUT).await
}

async fn persistent_request_with_timeout(
    client: &Arc<PersistentClient>,
    op: &str,
    params: serde_json::Value,
    timeout: std::time::Duration,
) -> Result<serde_json::Value, String> {
    let _exchange = client.exchange.lock().await;
    if client.closed.load(Ordering::Relaxed) {
        return Err("connection_error: IPC connection is closed".into());
    }
    let mut cancellation = ExchangeGuard {
        client,
        armed: true,
    };
    let result = tokio::time::timeout(timeout, persistent_exchange(client, op, params)).await;
    cancellation.armed = false;
    match result {
        Ok(result) => result,
        Err(_) => {
            report_connection_failure(
                client,
                "timeout: IPC response timed out; acceptance is unknown; not retried",
            );
            invalidate_client(client);
            Err(
                "timeout: IPC response timed out; request may have been accepted; not retried"
                    .into(),
            )
        }
    }
}

async fn persistent_exchange(
    client: &Arc<PersistentClient>,
    op: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    use tokio::io::AsyncWriteExt;
    let id = NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed);
    let request = serde_json::json!({"id": id, "op": op, "params": params});
    let write_result = {
        let mut writer = client.writer.lock().await;
        let writer = writer
            .as_mut()
            .ok_or("connection_error: IPC writer is closed")?;
        writer.write_all(format!("{}\n", request).as_bytes()).await
    };
    if let Err(error) = write_result {
        let reason = format!("connection_error: IPC write failed: {error}; acceptance is unknown");
        report_connection_failure(client, &reason);
        invalidate_client(client);
        return Err(reason);
    }
    let mut incoming = client.incoming.lock().await;
    let Some(value) = incoming.recv().await else {
        return Err("connection_error: IPC controller connection closed before reply; acceptance is unknown".into());
    };
    drop(incoming);
    let value = value?;
    if value.get("id") != Some(&serde_json::json!(id)) {
        let reason = "protocol_error: unexpected IPC reply ID; connection invalidated";
        report_connection_failure(client, reason);
        invalidate_client(client);
        return Err(reason.into());
    }
    response_result(&value)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn shared_socket_client_call_discovers_server_and_returns_reply() {
        let temp = tempfile::tempdir().unwrap();
        let worktree = temp.path().join("worktree");
        std::fs::create_dir(&worktree).unwrap();
        let (command_tx, mut command_rx) = mpsc::unbounded_channel();
        let server = IpcServer::bind(&worktree, command_tx).unwrap().unwrap();

        assert_eq!(server.socket_path, shared_socket_path(&worktree).unwrap());

        let cwd = worktree.to_string_lossy().into_owned();
        let client_task = tokio::spawn(async move {
            client_call(&cwd, "test_operation", serde_json::json!({"input": 42})).await
        });
        let expected = serde_json::json!({"response": "success"});
        while let Some(event) = command_rx.recv().await {
            if let AppEvent::Ipc(IpcCommand::Request {
                op, params, reply, ..
            }) = event
            {
                assert_eq!(op, "test_operation");
                assert_eq!(params, serde_json::json!({"input": 42}));
                reply.send(Ok(expected.clone())).unwrap();
                break;
            }
        }

        assert_eq!(client_task.await.unwrap().unwrap(), expected);
        drop(server);
    }

    #[test]
    fn socket_names_are_stable_fixed_length_and_distinct() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("worktree [one] Ω");
        let second = temp.path().join("worktree (one) Ω");
        std::fs::create_dir(&first).unwrap();
        std::fs::create_dir(&second).unwrap();
        let runtime = temp.path().join("runtime");
        let fallback = temp.path().join("fallback");
        let a = socket_path_for(&first, &runtime, &fallback).unwrap();
        let b = socket_path_for(&second, &runtime, &fallback).unwrap();
        assert_eq!(a, socket_path_for(&first, &runtime, &fallback).unwrap());
        assert_ne!(a.file_name(), b.file_name());
        assert_eq!(a.file_name().unwrap().as_encoded_bytes().len(), 66);
        assert!(a.file_name().unwrap().to_string_lossy().starts_with("s-"));
    }

    #[test]
    fn long_runtime_prefix_uses_short_fallback_with_same_name() {
        let temp = tempfile::tempdir().unwrap();
        let worktree = temp.path().join("repo");
        std::fs::create_dir(&worktree).unwrap();
        let runtime = temp.path().join("r".repeat(100));
        let fallback = temp.path().join("fallback");
        let path = socket_path_for(&worktree, &runtime, &fallback).unwrap();
        let expected_name = socket_path_for(&worktree, &fallback, &fallback)
            .unwrap()
            .file_name()
            .unwrap()
            .to_owned();
        assert_eq!(path.parent(), Some(fallback.as_path()));
        assert_eq!(path.file_name(), Some(expected_name.as_os_str()));
        assert!(path.as_os_str().as_encoded_bytes().len() <= MAX_SOCKET_PATH_BYTES);
    }

    #[test]
    fn runtime_dir_uses_xdg_when_owned_and_private_fallback_otherwise() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let xdg = temp.path().join("xdg");
        let fallback = temp.path().join("fallback");
        std::fs::create_dir(&xdg).unwrap();
        let resolved = runtime_dir_from(Some(&xdg), &fallback, unsafe { libc::getuid() }).unwrap();
        assert_eq!(resolved, xdg.join("xi"));
        assert_eq!(
            std::fs::metadata(&resolved).unwrap().permissions().mode() & 0o777,
            0o700
        );

        let missing = temp.path().join("missing");
        let resolved =
            runtime_dir_from(Some(&missing), &fallback, unsafe { libc::getuid() }).unwrap();
        assert_eq!(resolved, fallback);
        assert_eq!(
            std::fs::metadata(&fallback).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[test]
    fn legacy_path_remains_explicitly_client_only() {
        assert_eq!(
            legacy_socket_path(Path::new("/repo")),
            Path::new("/repo/.xi/xi.sock")
        );
    }

    #[test]
    fn stale_socket_recovery_preserves_live_owner() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("session.sock");
        let stale = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        drop(stale);
        assert!(recover_stale_socket(&socket).unwrap());

        let live = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        assert!(!recover_stale_socket(&socket).unwrap());
        drop(live);
        assert!(socket.exists());

        let regular = temp.path().join("not-a-socket");
        std::fs::write(&regular, b"preserve").unwrap();
        assert!(!remove_stale_socket(&regular).unwrap());
        assert_eq!(std::fs::read(&regular).unwrap(), b"preserve");
    }
}

pub async fn client_post_prompt(
    cwd: &str,
    text: &str,
    app_event_tx: AppEventTx,
) -> Result<serde_json::Value, String> {
    persistent_request(
        &get_persistent_client(cwd, &app_event_tx).await?,
        "post_prompt",
        serde_json::json!({"text": text}),
    )
    .await
}
