//! Shared-runtime IPC for interacting with a running xi session.
//!
//! Unix sockets are stored in a private per-user runtime directory. Their
//! fixed-length names are SHA-256 digests of canonical worktree paths. Clients
//! also try the legacy `.xi/xi.sock` endpoint for upgrade compatibility.

#[cfg(unix)]
use crate::app_event::AppEvent;
use crate::app_event::AppEventTx;
#[cfg(unix)]
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::collections::HashMap;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(unix)]
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::{mpsc, oneshot};

pub const PROTOCOL_VERSION: u32 = 1;

pub type IpcReply = oneshot::Sender<Result<serde_json::Value, ErrorBody>>;
pub type PendingPrompt = (u64, String, IpcReply);

#[cfg(unix)]
static NEXT_CONNECTION_ID: AtomicU64 = AtomicU64::new(1);

#[cfg(unix)]
#[derive(Debug, serde::Deserialize)]
struct Request {
    id: serde_json::Value,
    op: String,
    #[serde(default)]
    params: serde_json::Value,
}

#[cfg(unix)]
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

// Constructed by the Unix IPC transport; retained on other platforms so App
// can use one platform-independent event handler.
#[cfg_attr(not(unix), allow(dead_code))]
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

#[cfg(unix)]
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

pub struct IpcServer {
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

    #[cfg(not(unix))]
    pub fn bind(_cwd: &Path, _command_tx: AppEventTx) -> std::io::Result<Option<Self>> {
        Ok(None)
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
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

#[cfg(unix)]
async fn connection(stream: tokio::net::UnixStream, command_tx: AppEventTx, connection_id: u64) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    let mut line = String::new();
    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<String>();
    let _ = command_tx.send(AppEvent::Ipc(IpcCommand::Subscribe {
        connection_id,
        events: event_tx.clone(),
    }));
    loop {
        tokio::select! {
            result = reader.read_line(&mut line) => {
                let Ok(size) = result else { break };
                if size == 0 { break; }
                let response = match serde_json::from_str::<Request>(line.trim()) {
                    Ok(request) => dispatch(request, &command_tx, connection_id).await,
                    Err(error) => Response { version: PROTOCOL_VERSION, id: serde_json::Value::Null, ok: false, result: None, error: Some(ErrorBody { code: "invalid_request".into(), message: error.to_string() }) },
                };
                line.clear();
                let Ok(mut data) = serde_json::to_vec(&response) else { break };
                data.push(b'\n');
                if write.write_all(&data).await.is_err() { break; }
            }
            Some(event) = event_rx.recv() => {
                if write.write_all(event.as_bytes()).await.is_err() { break; }
                if write.write_all(b"\n").await.is_err() { break; }
            }
        }
    }
    let _ = command_tx.send(AppEvent::Ipc(IpcCommand::Disconnect { connection_id }));
}

#[cfg(unix)]
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

#[cfg(unix)]
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

pub fn control_revoked_event(session_id: &str) -> String {
    serde_json::json!({
        "version": PROTOCOL_VERSION,
        "seq": 0,
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "session_id": session_id,
        "event": "control.revoked",
        "payload": { "reason": "user_took_control", "pending_input_dropped": true }
    })
    .to_string()
}

pub fn completion_event(
    session_id: &str,
    turn_id: &str,
    status: &str,
    log_path: &Path,
    response: Option<&crate::session_event::SessionEvent>,
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
        "seq": 0,
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "session_id": session_id,
        "event": "agent.completed",
        "payload": {
            "turn_id": turn_id,
            "status": status,
            "log_path": log_path,
            "response": response,
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

/// Send one request to a running worktree session.
pub async fn client_call(
    cwd: &str,
    op: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    #[cfg(unix)]
    {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let stream = connect_session(cwd).await?;
        let (read, mut write) = stream.into_split();
        let request = serde_json::json!({"id": 1, "op": op, "params": params});
        write
            .write_all(format!("{}\n", request).as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        let mut line = String::new();
        BufReader::new(read)
            .read_line(&mut line)
            .await
            .map_err(|e| e.to_string())?;
        let response: serde_json::Value =
            serde_json::from_str(line.trim()).map_err(|e| e.to_string())?;
        if response.get("ok") == Some(&serde_json::Value::Bool(true)) {
            Ok(response
                .get("result")
                .cloned()
                .unwrap_or(serde_json::Value::Null))
        } else {
            Err(response
                .pointer("/error/message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("IPC request failed")
                .to_string())
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (cwd, op, params);
        Err("unavailable: session IPC is not supported on this platform".into())
    }
}

#[cfg(unix)]
struct PersistentClient {
    writer: tokio::sync::Mutex<tokio::net::unix::OwnedWriteHalf>,
    incoming: tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<serde_json::Value>>,
}

#[cfg(unix)]
static CLIENTS: OnceLock<Mutex<HashMap<PathBuf, Arc<PersistentClient>>>> = OnceLock::new();

#[cfg(unix)]
async fn get_persistent_client(
    cwd: &str,
    app_event_tx: &AppEventTx,
) -> Result<Arc<PersistentClient>, String> {
    let clients = CLIENTS.get_or_init(|| Mutex::new(HashMap::new()));
    let canonical_cwd = canonical_worktree(Path::new(cwd)).map_err(|error| error.to_string())?;
    if let Some(client) = clients
        .lock()
        .expect("IPC client map poisoned")
        .get(&canonical_cwd)
        .cloned()
    {
        return Ok(client);
    }
    let stream = connect_session(cwd).await?;
    let (read, writer) = stream.into_split();
    let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
    let event_tx = app_event_tx.clone();
    let event_cwd = canonical_cwd.clone();
    tokio::spawn(async move {
        use tokio::io::{AsyncBufReadExt, BufReader};
        let mut reader = BufReader::new(read);
        let mut line = String::new();
        while reader
            .read_line(&mut line)
            .await
            .ok()
            .filter(|n| *n > 0)
            .is_some()
        {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) {
                if value.get("event").is_some() {
                    let terminal = matches!(
                        value.get("event").and_then(serde_json::Value::as_str),
                        Some("agent.completed" | "control.revoked")
                    );
                    let _ = event_tx.send(AppEvent::IpcNotification {
                        cwd: event_cwd.to_string_lossy().into_owned(),
                        event: value,
                    });
                    if terminal {
                        if let Some(clients) = CLIENTS.get() {
                            clients
                                .lock()
                                .expect("IPC client map poisoned")
                                .remove(Path::new(&event_cwd));
                        }
                        break;
                    }
                } else {
                    let _ = incoming_tx.send(value);
                }
            }
            line.clear();
        }
    });
    let client = Arc::new(PersistentClient {
        writer: tokio::sync::Mutex::new(writer),
        incoming: tokio::sync::Mutex::new(incoming_rx),
    });
    clients
        .lock()
        .expect("IPC client map poisoned")
        .insert(canonical_cwd, Arc::clone(&client));
    Ok(client)
}

#[cfg(unix)]
async fn persistent_request(
    client: &Arc<PersistentClient>,
    op: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    use tokio::io::AsyncWriteExt;
    let request = serde_json::json!({"id": 1, "op": op, "params": params});
    client
        .writer
        .lock()
        .await
        .write_all(format!("{}\n", request).as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    loop {
        let mut incoming = client.incoming.lock().await;
        let Some(value) = incoming.recv().await else {
            return Err("IPC controller connection closed".into());
        };
        drop(incoming);
        if value.get("id") == Some(&serde_json::json!(1)) {
            if value.get("ok") == Some(&serde_json::Value::Bool(true)) {
                return Ok(value
                    .get("result")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null));
            }
            return Err(value
                .pointer("/error/message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("IPC request failed")
                .to_string());
        }
    }
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
    #[cfg(unix)]
    {
        persistent_request(
            &get_persistent_client(cwd, &app_event_tx).await?,
            "post_prompt",
            serde_json::json!({"text": text}),
        )
        .await
    }
    #[cfg(not(unix))]
    {
        let _ = (cwd, text, app_event_tx);
        Err("unavailable: session IPC is not supported on this platform".into())
    }
}
