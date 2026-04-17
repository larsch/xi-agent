use tokio::sync::mpsc::UnboundedSender;

use crate::{
    agent::types::{AgentEvent, AskRequest},
    auth::LoginEvent,
    llm::ProviderError,
};

/// Background events delivered to the interactive app loop.
#[derive(Debug)]
pub enum AppEvent {
    Agent(AgentEvent),
    ModelsReady(Result<Vec<String>, ProviderError>),
    Login(LoginEvent),
    AskUser(AskRequest),
}

pub type AppEventTx = UnboundedSender<AppEvent>;
