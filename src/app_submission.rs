//! Agent task submission, abort, steering, and scroll methods split out of `app.rs`.

use std::path::Path;
use std::sync::Arc;

use crate::agent::types::CancelLevel;
use crate::agent::{AgentLoopConfig, ToolOutputLog, run_agent_loop};
use crate::app::{App, DynProvider, RetryTarget, StreamingStatus};
use crate::at_file::{AtFileResult, parse_at_tokens, resolve_at_tokens};
use crate::live_turn::LiveToolResult;
use crate::llm::{Message, Role};
use crate::session_event::SessionEvent;
impl App {
    // ── LLM submission ────────────────────────────────────────────────────────

    fn start_agent_task(&mut self, provider: &DynProvider) {
        // Ensure the session ID is assigned before creating the log so the
        // output directory uses the real session key, not the "init" placeholder.
        let session_id = self.ensure_session_id();
        // Replace the agent_config log with one keyed to the real session ID.
        // Keeping it in agent_config ensures it outlives the task and the files
        // remain accessible after the agent loop completes.
        self.agent_config.tool_output_log =
            Arc::new(std::sync::Mutex::new(ToolOutputLog::new(&session_id)));
        let session_events = self
            .session
            .session_state
            .as_ref()
            .expect("start_agent_task called before session_state was initialised")
            .events()
            .to_vec();

        // Create cancel channel before the config so the executor can receive
        // a clone for passing into ToolCallContext.
        let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(CancelLevel::None);
        self.runtime.cancel_tx = Some(cancel_tx);

        // Apply agent tool filtering when an agent is active.
        let (tools, system_prompt) = if let Some(agent) = self.resolve_current_agent() {
            let filtered = crate::agents::filter_tools(
                &self.agent_config.tools,
                &agent.include_tools,
                &agent.exclude_tools,
            );
            (filtered, self.agent_config.system_prompt.clone())
        } else {
            (
                self.agent_config.tools.clone(),
                self.agent_config.system_prompt.clone(),
            )
        };

        let config = AgentLoopConfig {
            tools,
            file_tracker: Arc::clone(&self.agent_config.file_tracker),
            tool_output_log: Arc::clone(&self.agent_config.tool_output_log),
            session_events,
            current_model: self.provider.current_model.clone(),
            auto_compaction_enabled: true,
            manual_compaction_instructions: self
                .session
                .pending_manual_compaction_instructions
                .take(),
            executor: std::sync::Arc::new({
                let mut ex = crate::agent::DefaultToolExecutor::new();
                ex.hooks = self.agent_config.hooks.clone();
                ex.hook_ipc = self.agent_config.hook_ipc.clone();
                ex.session_id = session_id.clone();
                ex.cancel_rx = Some(cancel_rx.clone());
                ex
            }),
            system_prompt,
            hooks: self.agent_config.hooks.clone(),
            hook_ipc: self.agent_config.hook_ipc.clone(),
            session_id: session_id.clone(),
        };
        let (steering_tx, steering_rx) = tokio::sync::mpsc::unbounded_channel();
        self.runtime.steering_tx = Some(steering_tx);
        self.runtime.queued_steering.clear();

        let provider = Arc::clone(provider);
        let tx = self.app_event_tx();
        self.runtime.agent_task = Some(tokio::spawn(async move {
            run_agent_loop(config, provider, tx, steering_rx, cancel_rx).await;
        }));
    }

    /// Set streaming flags and spawn the agent task using the current history.
    ///
    /// Call after pushing any new user message(s) and persisting state.
    /// Does **not** perform the pre-flight token check — callers are
    /// responsible for calling `check_token_preflight` before this.
    pub(crate) fn launch_turn(&mut self, provider: &DynProvider) {
        self.clear_abort_status_notice();
        self.session.live_turn.notices.clear();
        self.ensure_event_log_for_submit();
        assert!(
            self.session.session_state.is_some(),
            "launch_turn called before session_state was initialised"
        );
        self.agent_turn.start();
        self.login.auth_retry_budget = 1;
        self.log_view.auto_scroll = true;
        self.runtime.reset_abort_stages();
        self.start_agent_task(provider);
    }

    /// Queue a user steering message while the agent loop is running.
    pub fn enqueue_steering_from_input(&mut self) {
        let lines: Vec<String> = self.textarea.lines().to_vec();
        let text = lines.join("\n");
        let trimmed = text.trim().to_string();
        if trimmed.is_empty() || !self.streaming() || self.login.active {
            return;
        }

        let Some(tx) = self.runtime.steering_tx.as_ref() else {
            return;
        };

        if tx.send(trimmed.clone()).is_ok() {
            self.runtime.queued_steering.push(trimmed);
            self.reset_textarea();
            self.log_view.auto_scroll = true;
        }
    }

    /// Trigger a compaction-only task against the current session state.
    pub fn trigger_manual_compaction(
        &mut self,
        instructions: Option<String>,
        provider: &DynProvider,
    ) {
        if self.streaming() || self.login.active {
            return;
        }

        self.ensure_event_log_for_submit();
        self.session.pending_manual_compaction_instructions = instructions;

        if self.check_token_preflight(RetryTarget::AgentTurn) {
            return;
        }

        self.agent_turn.start();
        self.login.auth_retry_budget = 1;
        self.log_view.auto_scroll = true;
        self.start_agent_task(provider);
    }

    /// Parse `@<path>` tokens from `text`, read each file, and inject a
    /// synthetic `read_file` `ToolCall` + `ToolResult` event pair after the
    /// user message, as if the agent had already performed those reads.
    /// Image files embed the image as a data URI directly in the text
    /// content; text files are inlined verbatim.
    ///
    /// Missing or unreadable files are silently skipped — no error is
    /// displayed or reported to the model.
    fn inject_at_file_attachments(&mut self, results: &[AtFileResult]) {
        if results.is_empty() {
            return;
        }

        // Ensure the event log exists before appending events.
        self.session.ensure_event_log_for_submit();

        let ts = Self::now_ts();

        for (idx, result) in results.iter().enumerate() {
            match result {
                AtFileResult::Text { content, path } => {
                    let call_id = format!("attach_{idx}");
                    let args = serde_json::json!({ "path": path });

                    self.session.append_event_immediate(SessionEvent::ToolCall {
                        id: call_id.clone(),
                        name: "read_file".to_string(),
                        args,
                        include_in_llm: true,
                        timestamp: ts,
                    });
                    self.session
                        .append_event_immediate(SessionEvent::ToolResult {
                            id: call_id,
                            name: "read_file".to_string(),
                            content: content.clone(),
                            is_error: false,
                            display_range: None,
                            include_in_llm: true,
                            timestamp: ts,
                        });
                }
                AtFileResult::Image {
                    base64,
                    mime_type,
                    path,
                } => {
                    let call_id = format!("attach_{idx}");
                    let args = serde_json::json!({ "path": path });

                    self.session.append_event_immediate(SessionEvent::ToolCall {
                        id: call_id.clone(),
                        name: "read_file".to_string(),
                        args,
                        include_in_llm: true,
                        timestamp: ts,
                    });
                    // Inline the image as a data URI directly in the content
                    // text so it works with any provider (some OpenAI-compatible
                    // backends don't accept structured content arrays in tool
                    // results).
                    let data_uri = format!("data:{mime_type};base64,{base64}");
                    let content = format!("![{path}]({data_uri})\n\n[Image: {path}]");
                    self.session
                        .append_event_immediate(SessionEvent::ToolResult {
                            id: call_id,
                            name: "read_file".to_string(),
                            content,
                            is_error: false,
                            display_range: None,
                            include_in_llm: true,
                            timestamp: ts,
                        });
                    // No need for pending_attachment_images — image is inline.
                }
                AtFileResult::Error { .. } => {
                    // Silently skip — no error displayed or reported to model.
                }
            }
        }
    }

    /// Take the textarea content and start the agent loop.
    ///
    /// After committing the user message to in-memory display state, this
    /// method sets a flag so the caller can trigger an immediate screen
    /// redraw before the I/O-heavy finalisation step.  Call
    /// [`finalize_submission`] after the redraw to persist and launch.
    ///
    /// [`finalize_submission`]: App::finalize_submission
    pub fn submit(&mut self, _provider: &DynProvider) {
        let lines: Vec<String> = self.textarea.lines().to_vec();
        let text = lines.join("\n");
        let trimmed = text.trim().to_string();
        if trimmed.is_empty() || self.streaming() || self.login.active {
            return;
        }

        let cwd = Path::new(&self.session.current_cwd).to_path_buf();
        let tokens = parse_at_tokens(&trimmed);
        let results = resolve_at_tokens(&tokens, &cwd);
        let rewritten = crate::at_file::rewrite_user_text(&trimmed, &results);

        self.append_user_message(rewritten);
        self.inject_at_file_attachments(&results);
        self.finish_submit_commit();
    }

    /// Submit a chat message from the textarea, guarding against submission
    /// when no provider has been explicitly selected.
    pub fn submit_chat_message(&mut self, provider: &DynProvider) {
        if !self.provider.provider_selected {
            self.push_notice(Message::assistant(
                "[no provider selected — type /provider to pick one]",
            ));
            return;
        }
        self.submit(provider);
    }

    /// Submit a pre-built text string directly to the agent loop, bypassing the
    /// textarea.  Used by `/skill:<name>` command expansion.
    pub fn submit_with_text(&mut self, text: String, _provider: &DynProvider) {
        let trimmed = text.trim().to_string();
        if trimmed.is_empty() || self.streaming() || self.login.active {
            return;
        }
        self.append_user_message(trimmed);
        self.finish_submit_commit();
    }

    /// Shared tail for [`submit`] and [`submit_with_text`]: clear the
    /// textarea and flag for deferred finalisation so the caller can redraw
    /// before the I/O phase.
    fn finish_submit_commit(&mut self) {
        self.reset_textarea();
        self.runtime.pending_finalize = true;
    }

    /// Complete a submission that was prepared by [`submit`] or
    /// [`submit_with_text`].  Call this after the caller has had a chance to
    /// redraw the screen so the user message is already visible.
    ///
    /// [`submit`]: App::submit
    /// [`submit_with_text`]: App::submit_with_text
    pub fn finalize_submission(&mut self, provider: &DynProvider) {
        if !self.runtime.pending_finalize {
            return;
        }
        self.runtime.pending_finalize = false;

        self.persist_messages();

        // Proactive token refresh check before starting the request.
        if self.check_token_preflight(RetryTarget::AgentTurn) {
            // Refresh triggered; request will be retried after refresh completes.
            return;
        }

        self.launch_turn(provider);
    }

    pub fn retry_last_request(&mut self, provider: &DynProvider) {
        if self.streaming() || self.login.active {
            return;
        }

        // Pop trailing TurnError from committed session events if present.
        if let Some(ss) = self.session.session_state.as_mut() {
            ss.pop_trailing_turn_error();
        }

        // Pop the trailing error notice if present.
        if let Some(last) = self.session.live_turn.notices.last()
            && last.role == Role::Assistant
            && (last.content.starts_with("[Error:") || last.content.starts_with("[token refresh"))
        {
            self.session.live_turn.notices.pop();
        }

        self.persist_messages();
        self.launch_turn(provider);
    }

    fn append_abort_results_for_pending_tool_calls(&mut self) {
        // Find tool call IDs in the pending turn buffer that haven't been
        // completed with a ToolResult yet.
        let mut pending_ids: Vec<String> = Vec::new();
        for ev in &self.session.pending_turn_events {
            match ev {
                SessionEvent::ToolCall { id, .. } if !pending_ids.iter().any(|p| p == id) => {
                    pending_ids.push(id.clone());
                }
                SessionEvent::ToolResult { id, .. } => {
                    pending_ids.retain(|p| p != id);
                }
                _ => {}
            }
        }

        for id in pending_ids {
            if let Some(entry) = self.session.live_turn.find_tool_entry_mut(&id)
                && entry.result.is_none()
            {
                entry.result = Some(LiveToolResult {
                    content: "Interrupted by user".to_string(),
                    is_error: true,
                    display_range: None,
                    image_data: None,
                });
            }

            let name = self
                .session
                .pending_turn_events
                .iter()
                .rev()
                .find_map(|e| {
                    if let SessionEvent::ToolCall { id: cid, name, .. } = e {
                        if cid == &id { Some(name.clone()) } else { None }
                    } else {
                        None
                    }
                })
                .unwrap_or_default();

            self.session
                .pending_turn_events
                .push(SessionEvent::ToolResult {
                    id,
                    name,
                    content: "Interrupted by user".to_string(),
                    is_error: true,
                    display_range: None,
                    include_in_llm: true,
                    timestamp: Self::now_ts(),
                });
        }
    }

    fn clear_abort_status_notice(&mut self) {
        if matches!(
            self.agent_turn.status,
            Some(StreamingStatus::CompletedMessage(ref s)) if s == "[agent loop aborted]"
        ) {
            self.agent_turn.set_status(None);
        }
    }

    pub fn abort_agent_loop(&mut self) {
        if let Some(handle) = self.runtime.agent_task.take() {
            // Signal cooperative cancellation; hard-abort as fallback.
            if let Some(tx) = self.runtime.cancel_tx.take() {
                let _ = tx.send(CancelLevel::HardAbort);
            }
            handle.abort();
            self.agent_turn
                .set_status(Some(StreamingStatus::CompletedMessage(
                    "[agent loop aborted]".to_string(),
                )));
            self.agent_turn.last_output_at = None;
            self.runtime.steering_tx = None;
            self.runtime.queued_steering.clear();
            self.append_abort_results_for_pending_tool_calls();
            self.finalise_assistant_turn_event();
            self.flush_turn_events();
            self.persist_messages();
        }
    }

    /// Stage 1: request the agent loop to stop after the current turn completes.
    /// The turn finishes normally — model response + tool batch — but no new
    /// turn is started.
    pub fn request_soft_stop(&mut self) {
        if !self.runtime.is_running() {
            return;
        }
        self.runtime.abort_stage = CancelLevel::SoftStop;
        if let Some(tx) = self.runtime.cancel_tx.as_ref() {
            let _ = tx.send(CancelLevel::SoftStop);
        }
        self.agent_turn.set_status(Some(StreamingStatus::Message(
            "[Stopping after current turn… Press Ctrl-C again to abort now]".to_string(),
        )));
    }

    /// Stage 2: abort the current model request and send SIGTERM to the
    /// current subprocess tool. When a tool is currently executing, keep the
    /// agent task alive so the tool can observe the cancel level and exit
    /// cooperatively. When no tool is running, abort immediately.
    pub fn request_hard_abort(&mut self) {
        if !self.runtime.is_running() {
            return;
        }
        self.runtime.abort_stage = CancelLevel::HardAbort;
        if let Some(tx) = self.runtime.cancel_tx.as_ref() {
            let _ = tx.send(CancelLevel::HardAbort);
        }

        let tool_running = self
            .session
            .live_turn
            .tool_entries
            .iter()
            .any(|entry| entry.result.is_none());

        if tool_running {
            self.agent_turn.set_status(Some(StreamingStatus::Message(
                "[Aborting… Press Ctrl-C again to force kill]".to_string(),
            )));
            return;
        }

        // No tool is currently running, so abort the task immediately to stop
        // model streaming and commit the partial turn.
        self.abort_agent_loop();
        self.runtime.abort_stage = CancelLevel::HardAbort;
        self.agent_turn
            .set_status(Some(StreamingStatus::CompletedMessage(
                "[Aborting… Press Ctrl-C again to force kill]".to_string(),
            )));
    }

    /// Stage 3: send SIGKILL to the current subprocess tool immediately.
    pub fn request_force_kill(&mut self) {
        if !self.runtime.is_running() {
            self.agent_turn
                .set_status(Some(StreamingStatus::CompletedMessage(
                    "[Agent loop already stopped]".to_string(),
                )));
            return;
        }
        self.runtime.abort_stage = CancelLevel::ForceKill;
        if let Some(tx) = self.runtime.cancel_tx.as_ref() {
            let _ = tx.send(CancelLevel::ForceKill);
        }
        self.agent_turn.set_status(Some(StreamingStatus::Message(
            "[Force killing…]".to_string(),
        )));
    }

    // ── Scrolling ─────────────────────────────────────────────────────────────

    pub fn scroll_up(&mut self) {
        self.log_view.scroll_up();
    }

    pub fn scroll_up_lines(&mut self, n: usize) {
        self.log_view.scroll_up_lines(n);
    }

    pub fn scroll_down_lines(&mut self, n: usize) {
        self.log_view.scroll_down_lines(n);
    }

    pub fn scroll_down(&mut self) {
        self.log_view.scroll_down();
    }
}
