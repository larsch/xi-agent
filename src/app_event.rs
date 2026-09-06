use tokio::sync::mpsc::UnboundedSender;

use crate::{
    LoadedContext,
    agent::types::{AgentEvent, AskRequest, ToolResult},
    auth::LoginEvent,
    llm::ProviderError,
    session_ipc::IpcCommand,
};

/// Background events delivered to the interactive app loop.
#[derive(Debug)]
pub enum AppEvent {
    Agent(AgentEvent),
    ModelsReady(Result<Vec<String>, ProviderError>),
    Login(LoginEvent),
    AskUser(AskRequest),
    /// A local shell command subprocess has finished.
    ShellComplete {
        call_id: String,
        result: ToolResult,
    },
    /// The deferred context load (tools, skills, agents) has completed.
    ContextLoaded(LoadedContext),
    /// The `restart_host` tool requested a process restart.  Handled by the app
    /// loop by flushing the pending turn and re-exec'ing the binary.
    #[cfg(all(feature = "restart", unix))]
    Restart,
    // Constructed by the Unix IPC transport; retained on other platforms so
    // application event handling remains platform-independent.
    #[cfg_attr(not(unix), allow(dead_code))]
    Ipc(IpcCommand),
    #[cfg_attr(not(unix), allow(dead_code))]
    IpcNotification {
        cwd: String,
        event: serde_json::Value,
    },
}

pub type AppEventTx = UnboundedSender<AppEvent>;

/// Extension trait for fire-and-forget channel sends.
///
/// `send_ignore` is equivalent to `let _ = self.send(val)` — it discards
/// the error that occurs when all receivers have been dropped.  Use this
/// instead of the noisy `let _ =` pattern at every call site.
pub trait SendIgnore<T> {
    fn send_ignore(&self, val: T);
}

impl<T> SendIgnore<T> for UnboundedSender<T> {
    fn send_ignore(&self, val: T) {
        let _ = self.send(val);
    }
}
