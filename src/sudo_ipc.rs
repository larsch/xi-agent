//! Private IPC used by sudo's askpass helper.
//!
//! The transport is deliberately separate from session IPC for now, while the
//! envelope follows the same versioned request/response shape for future
//! consolidation.

#[cfg(unix)]
use crate::app_event::AppEvent;
#[cfg(unix)]
use crate::app_event::AppEventTx;
#[cfg(unix)]
use std::collections::HashMap;
#[cfg(unix)]
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::sync::{Arc, Mutex};
#[cfg(unix)]
use tokio::sync::oneshot;

pub const PROTOCOL_VERSION: u32 = 1;
pub const OP_ASKPASS: &str = "sudo_askpass";

#[cfg(unix)]
#[derive(Debug)]
pub struct Authorization {
    pub command: String,
    pub askpass_count: usize,
}

#[cfg(unix)]
static AUTHORIZATIONS: std::sync::OnceLock<Arc<Mutex<HashMap<String, Authorization>>>> =
    std::sync::OnceLock::new();
#[cfg(unix)]
static SOCKET_PATH: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

#[cfg(unix)]
pub fn register(token: String, command: String) -> bool {
    AUTHORIZATIONS
        .get_or_init(|| Arc::new(Mutex::new(HashMap::new())))
        .lock()
        .expect("sudo authorization registry poisoned")
        .insert(
            token,
            Authorization {
                command,
                askpass_count: 0,
            },
        )
        .is_none()
}

#[cfg(unix)]
pub fn unregister(token: &str) {
    if let Some(registry) = AUTHORIZATIONS.get() {
        registry
            .lock()
            .expect("sudo authorization registry poisoned")
            .remove(token);
    }
}

#[cfg(unix)]
pub fn note_askpass(token: &str) -> Option<Authorization> {
    let registry = AUTHORIZATIONS.get()?;
    let mut registry = registry.lock().ok()?;
    let state = registry.get_mut(token)?;
    state.askpass_count += 1;
    Some(Authorization {
        command: state.command.clone(),
        askpass_count: state.askpass_count,
    })
}

#[cfg(unix)]
pub fn socket_path() -> Option<PathBuf> {
    SOCKET_PATH.get().cloned()
}

#[cfg(unix)]
pub type AskpassReply = oneshot::Sender<Response>;

#[cfg(unix)]
#[derive(Debug)]
pub struct AskpassRequest {
    pub id: String,
    pub token: String,
    pub prompt: Option<String>,
    pub reply: AskpassReply,
}

#[cfg(unix)]
pub struct Server {
    socket_path: PathBuf,
    socket_identity: (u64, u64),
}

#[cfg(unix)]
impl Server {
    pub fn bind(event_tx: AppEventTx) -> std::io::Result<Self> {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
        let uid = unsafe { libc::getuid() };
        let directory = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|path| {
                std::fs::metadata(path)
                    .map(|m| m.is_dir() && m.uid() == uid)
                    .unwrap_or(false)
            })
            .map(|path| path.join("xi"))
            .unwrap_or_else(|| PathBuf::from(format!("/tmp/xi-{uid}")));
        let mut builder = std::fs::DirBuilder::new();
        builder.mode(0o700);
        match builder.create(&directory) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        let metadata = std::fs::metadata(&directory)?;
        if !metadata.is_dir() || metadata.uid() != uid {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "sudo IPC directory is not user-owned",
            ));
        }
        if metadata.mode() & 0o077 != 0 {
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
        }
        let socket_path =
            directory.join(format!("sudo-{}-{}", std::process::id(), random_hex(16)?));
        let listener = std::os::unix::net::UnixListener::bind(&socket_path)?;
        std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        let listener = tokio::net::UnixListener::from_std(listener)?;
        let socket_identity = socket_identity(&socket_path)?;
        let _ = SOCKET_PATH.set(socket_path.clone());
        tokio::spawn(accept_loop(listener, event_tx));
        Ok(Self {
            socket_path,
            socket_identity,
        })
    }
}

#[cfg(unix)]
impl Drop for Server {
    fn drop(&mut self) {
        if socket_identity(&self.socket_path).ok() == Some(self.socket_identity) {
            let _ = std::fs::remove_file(&self.socket_path);
        }
    }
}

#[cfg(unix)]
fn socket_identity(path: &Path) -> std::io::Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::symlink_metadata(path)?;
    Ok((metadata.dev(), metadata.ino()))
}

#[cfg(unix)]
async fn accept_loop(listener: tokio::net::UnixListener, event_tx: AppEventTx) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            break;
        };
        let tx = event_tx.clone();
        tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
            let (read, mut write) = stream.into_split();
            let mut line = String::new();
            let result = BufReader::new(read).read_line(&mut line).await;
            let request = result
                .ok()
                .and_then(|n| (n > 0).then(|| serde_json::from_str::<Request>(line.trim()).ok()))
                .flatten();
            let Some(request) = request else { return };
            if request.version != PROTOCOL_VERSION || request.op != OP_ASKPASS {
                return;
            }
            let (reply, response) = oneshot::channel();
            if tx
                .send(AppEvent::SudoAskpass(AskpassRequest {
                    id: request.id,
                    token: request.params.token,
                    prompt: request.params.prompt,
                    reply,
                }))
                .is_err()
            {
                return;
            }
            let Ok(response) = response.await else { return };
            let Ok(mut encoded) = serde_json::to_vec(&response) else {
                return;
            };
            encoded.push(b'\n');
            let _ = write.write_all(&encoded).await;
        });
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct Request {
    pub version: u32,
    pub id: String,
    pub op: String,
    pub params: RequestParams,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct RequestParams {
    pub token: String,
    #[serde(default)]
    pub prompt: Option<String>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct Response {
    pub version: u32,
    pub id: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<ResponseResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorBody>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct ResponseResult {
    pub password: String,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
}

impl Response {
    pub fn denied(id: String) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            id,
            ok: false,
            result: None,
            error: Some(ErrorBody {
                code: "denied".to_string(),
                message: "sudo authorization denied".to_string(),
            }),
        }
    }

    pub fn password(id: String, password: String) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            id,
            ok: true,
            result: Some(ResponseResult { password }),
            error: None,
        }
    }
}

/// Return whether the process was invoked as the sudo askpass helper.
///
/// A partial context fails closed. The caller must not start normal xi when
/// this returns `Some(Err(_))`.
#[cfg(unix)]
pub fn helper_context() -> Option<Result<HelperContext, String>> {
    let token = std::env::var("XI_SUDO_TOKEN").ok();
    let socket = std::env::var_os("XI_SOCKET").map(PathBuf::from);
    match (token, socket) {
        (None, None) => None,
        (Some(token), Some(socket)) if !token.is_empty() && !socket.as_os_str().is_empty() => {
            Some(Ok(HelperContext { token, socket }))
        }
        _ => Some(Err("incomplete sudo helper context".to_string())),
    }
}

#[cfg(not(unix))]
pub fn helper_context() -> Option<Result<(), String>> {
    None
}

#[cfg(unix)]
pub struct HelperContext {
    token: String,
    socket: PathBuf,
}

#[cfg(unix)]
impl HelperContext {
    pub async fn run(self, prompt: Option<String>) -> std::io::Result<()> {
        use std::io::ErrorKind;
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        use tokio::net::UnixStream;

        let request = Request {
            version: PROTOCOL_VERSION,
            id: random_hex(16)?,
            op: OP_ASKPASS.to_string(),
            params: RequestParams {
                token: self.token,
                prompt,
            },
        };
        let mut stream = UnixStream::connect(&self.socket).await?;
        let mut encoded = serde_json::to_vec(&request)
            .map_err(|error| std::io::Error::new(ErrorKind::InvalidData, error))?;
        encoded.push(b'\n');
        stream.write_all(&encoded).await?;
        stream.shutdown().await?;

        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).await?;
        let response: Response = serde_json::from_str(line.trim())
            .map_err(|error| std::io::Error::new(ErrorKind::InvalidData, error))?;
        if response.version != PROTOCOL_VERSION || response.id != request.id || !response.ok {
            return Err(std::io::Error::new(
                ErrorKind::PermissionDenied,
                "sudo authorization denied",
            ));
        }
        let password = response
            .result
            .ok_or_else(|| std::io::Error::new(ErrorKind::InvalidData, "missing password"))?
            .password;
        let mut stdout = std::io::stdout().lock();
        use std::io::Write;
        stdout.write_all(password.as_bytes())?;
        stdout.write_all(b"\n")?;
        stdout.flush()
    }
}

#[cfg(unix)]
pub fn random_hex(bytes: usize) -> std::io::Result<String> {
    let mut data = vec![0_u8; bytes];
    getrandom::fill(&mut data)
        .map_err(|error| std::io::Error::other(format!("random generation failed: {error}")))?;
    let mut result = String::with_capacity(bytes * 2);
    for byte in data {
        use std::fmt::Write;
        write!(result, "{byte:02x}").expect("String formatting cannot fail");
    }
    Ok(result)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn response_round_trip_preserves_password() {
        let response = Response::password("id".to_string(), "secret".to_string());
        let encoded = serde_json::to_string(&response).unwrap();
        let decoded: Response = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded.result.unwrap().password, "secret");
    }

    #[test]
    fn denial_does_not_contain_a_result() {
        let response = Response::denied("id".to_string());
        assert!(!response.ok);
        assert!(response.result.is_none());
    }

    #[test]
    fn token_registry_counts_askpass_requests_and_expires() {
        let token = random_hex(32).unwrap();
        assert!(register(token.clone(), "id".to_string()));
        assert_eq!(note_askpass(&token).unwrap().askpass_count, 1);
        assert_eq!(note_askpass(&token).unwrap().askpass_count, 2);
        unregister(&token);
        assert!(note_askpass(&token).is_none());
    }
}
