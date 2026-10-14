#![cfg(unix)]

use std::collections::VecDeque;

use crate::sudo_ipc::AskpassRequest;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalMode {
    Once,
    OnceSavePassword,
    AllSession,
}

pub(crate) struct PendingSudo {
    pub request: AskpassRequest,
    pub command: String,
}

pub(crate) struct SudoState {
    pub queue: VecDeque<PendingSudo>,
    pub active: Option<PendingSudo>,
    pub mode: Option<ApprovalMode>,
    pub password: Option<String>,
    pub password_entry: bool,
    pub prompt: Option<String>,
}

impl SudoState {
    pub fn new() -> Self {
        Self {
            queue: VecDeque::new(),
            active: None,
            mode: None,
            password: None,
            password_entry: false,
            prompt: None,
        }
    }

    pub fn enqueue(&mut self, request: AskpassRequest, command: String) {
        self.queue.push_back(PendingSudo { request, command });
    }

    pub fn activate_next(&mut self) {
        if self.active.is_none() {
            self.active = self.queue.pop_front();
        }
    }

    pub fn clear_password(&mut self) {
        self.password = None;
    }

    pub fn shutdown(&mut self) {
        if let Some(active) = self.active.take() {
            let _ = active
                .request
                .reply
                .send(crate::sudo_ipc::Response::denied(active.request.id));
        }
        for queued in self.queue.drain(..) {
            let _ = queued
                .request
                .reply
                .send(crate::sudo_ipc::Response::denied(queued.request.id));
        }
        self.clear_password();
        self.mode = None;
    }
}

impl Default for SudoState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_state_has_no_credential() {
        let state = SudoState::new();
        assert!(state.password.is_none());
        assert!(state.active.is_none());
        assert!(state.queue.is_empty());
    }

    #[test]
    fn clearing_password_removes_saved_credential() {
        let mut state = SudoState::new();
        state.password = Some("secret".to_string());
        state.clear_password();
        assert!(state.password.is_none());
    }
}
