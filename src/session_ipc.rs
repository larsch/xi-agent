//! Worktree-local IPC for interacting with a running xi session.
//!
//! The first xi instance in a worktree owns `.xi/xi.sock`.  This module keeps
//! transport and framing separate from App state; commands are routed through
//! the normal application event channel.

#[cfg(unix)]
use crate::app_event::AppEvent;
use crate::app_event::AppEventTx;
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

pub struct IpcServer {
    pub socket_path: PathBuf,
}

impl IpcServer {
    /// Claim the endpoint. Returns `None` when another instance owns it.
    #[cfg(unix)]
    pub fn bind(cwd: &Path, command_tx: AppEventTx) -> std::io::Result<Option<Self>> {
        let dir = cwd.join(".xi");
        std::fs::create_dir_all(&dir)?;
        let socket_path = dir.join("xi.sock");
        match std::os::unix::net::UnixListener::bind(&socket_path) {
            Ok(listener) => {
                listener.set_nonblocking(true)?;
                let listener = tokio::net::UnixListener::from_std(listener)?;
                tokio::spawn(accept_loop(listener, command_tx.clone()));
                Ok(Some(Self { socket_path }))
            }
            Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
                // A live owner keeps the endpoint. An unreachable filesystem
                // socket is recoverable after the failed probe.
                let live = std::os::unix::net::UnixStream::connect(&socket_path).is_ok();
                if live {
                    Ok(None)
                } else {
                    let _ = std::fs::remove_file(&socket_path);
                    match std::os::unix::net::UnixListener::bind(&socket_path) {
                        Ok(listener) => {
                            listener.set_nonblocking(true)?;
                            let listener = tokio::net::UnixListener::from_std(listener)?;
                            tokio::spawn(accept_loop(listener, command_tx.clone()));
                            Ok(Some(Self { socket_path }))
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
        let _ = std::fs::remove_file(&self.socket_path);
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

/// Send one request to a running worktree session.
pub async fn client_call(
    cwd: &str,
    op: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    #[cfg(unix)]
    {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let path = Path::new(cwd).join(".xi/xi.sock");
        let stream = tokio::net::UnixStream::connect(&path)
            .await
            .map_err(|_| format!("unavailable: no running xi session at {}", path.display()))?;
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
static CLIENTS: OnceLock<Mutex<HashMap<String, Arc<PersistentClient>>>> = OnceLock::new();

#[cfg(unix)]
async fn get_persistent_client(
    cwd: &str,
    app_event_tx: &AppEventTx,
) -> Result<Arc<PersistentClient>, String> {
    let clients = CLIENTS.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(client) = clients
        .lock()
        .expect("IPC client map poisoned")
        .get(cwd)
        .cloned()
    {
        return Ok(client);
    }
    let path = Path::new(cwd).join(".xi/xi.sock");
    let stream = tokio::net::UnixStream::connect(&path)
        .await
        .map_err(|_| format!("unavailable: no running xi session at {}", path.display()))?;
    let (read, writer) = stream.into_split();
    let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
    let event_tx = app_event_tx.clone();
    let event_cwd = cwd.to_string();
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
                        cwd: event_cwd.clone(),
                        event: value,
                    });
                    if terminal {
                        if let Some(clients) = CLIENTS.get() {
                            clients
                                .lock()
                                .expect("IPC client map poisoned")
                                .remove(&event_cwd);
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
        .insert(cwd.to_string(), Arc::clone(&client));
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
