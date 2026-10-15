//! Agent task submission, abort, steering, and scroll methods split out of `app.rs`.

use std::path::Path;
use std::sync::Arc;

use crate::agent::runner::SteeringCommand;
use crate::agent::types::CancelLevel;
use crate::agent::{AgentLoopConfig, ToolOutputLog};
use crate::app::{App, DynProvider, RetryTarget, StreamingStatus};
use crate::at_file::{AtFileResult, parse_at_tokens, resolve_at_tokens};
use crate::live_turn::LiveToolResult;
use crate::llm::{Message, Role};
use crate::session_event::SessionEvent;
impl App {
    /// Transfer an IPC-controlled session to the user at submission time.
    /// Unapplied IPC input is discarded; already-applied loop work continues.
    pub(crate) fn take_ipc_control_for_user(&mut self) {
        let text = self.textarea.lines().join("\n");
        if text.trim().is_empty() {
            return;
        }
        self.user_owned = true;
        // A control transfer during an active IPC-owned task must preserve the
        // active state. The submitting Enter then follows the steering path;
        // clearing it would make that same input launch a second runner.
        if !self.streaming() {
            self.agent_turn.set_status(None);
        }
        let Some(owner) = self.ipc_owner.take() else {
            return;
        };
        self.runtime.queued_steering.clear();
        if let Some((_, _, reply)) = self.ipc_prompt.take() {
            let _ = reply.send(Err(crate::session_ipc::ErrorBody {
                code: "control_revoked".into(),
                message: "user took control of the session".into(),
            }));
        }
        let session_id = self.session.current_session_id.clone().unwrap_or_default();
        let turn = self.ipc_turn.take();
        let event = crate::session_ipc::control_revoked_event(
            &session_id,
            turn.as_ref().map(|(id, _)| id.as_str()),
        );
        for (id, publisher) in &self.ipc_subscribers {
            if *id == owner {
                publisher.publish(event.clone());
            }
        }
    }

    // ── LLM submission ────────────────────────────────────────────────────────

    fn start_agent_task(&mut self, provider: &DynProvider) {
        let existing_runner = self.runtime.is_running();
        let session_id = self.session.current_session_id.as_deref().unwrap_or("init");
        log::debug!(
            "starting agent task: pid={} session_id={session_id} existing_runner={existing_runner} streaming={}",
            std::process::id(),
            self.streaming(),
        );
        if existing_runner {
            log::error!(
                "refusing to start an agent task while another runner is active: pid={} session_id={session_id}",
                std::process::id(),
            );
            return;
        }
        // Make sure the tool registry and system prompt are populated before
        // starting a task. They are normally loaded in the background at
        // startup; this covers `--prompt` and any submit that races ahead of
        // that background load.
        self.ensure_context_loaded();

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

        let python_repl = Arc::new(crate::agent::tools::python_repl::PythonReplSession::new());
        let config = AgentLoopConfig {
            tools,
            file_tracker: Arc::clone(&self.agent_config.file_tracker),
            tool_output_log: Arc::clone(&self.agent_config.tool_output_log),
            session_events,
            current_model: self.provider.current_model.clone(),
            auto_compaction_enabled: true,
            manual_compaction_requested: std::mem::take(
                &mut self.session.pending_manual_compaction_requested,
            ),
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
                ex.python_repl = Some(Arc::clone(&python_repl));
                ex
            }),
            system_prompt,
            hooks: self.agent_config.hooks.clone(),
            hook_ipc: self.agent_config.hook_ipc.clone(),
            session_id: session_id.clone(),
        };
        self.runtime.queued_steering.clear();
        self.runtime.queue_selection = None;
        self.runtime.queue_saved_input = None;

        let provider = Arc::clone(provider);
        let tx = self.app_event_tx();
        let handle = crate::agent::runner::AgentHandle::spawn_with_cancel(
            config, provider, tx, cancel_tx, cancel_rx,
        );
        self.runtime.set_agent_handle(handle);
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
        self.begin_agent_turn();
        self.login.auth_retry_budget = 1;
        self.log_view.auto_scroll = true;
        self.runtime.reset_abort_stages();
        self.start_agent_task(provider);
    }

    fn persist_drafts(&mut self, drafts: Vec<String>) -> bool {
        if self.session.stash_load_failed {
            self.push_notice(Message::assistant(
                "[draft stash was unreadable; refusing to overwrite saved data]",
            ));
            return false;
        }
        let session_id = match self.session.ensure_persistent_session_id() {
            Ok(id) => id,
            Err(error) => {
                log::warn!("could not create session for draft stash: {error}");
                self.push_notice(Message::assistant(format!("[draft not saved: {error}]")));
                return false;
            }
        };
        let Some(store) = self.session.session_store.as_ref() else {
            return false;
        };
        if let Err(error) = store.save_stash_for_session(&session_id, &drafts) {
            log::warn!("could not persist draft stash: {error}");
            self.push_notice(Message::assistant(format!("[draft not saved: {error}]")));
            return false;
        }
        self.session.drafts = drafts;
        self.session.refresh_resume_availability();
        true
    }

    /// Stash ordinary composer content at the front without adding it to history.
    pub(crate) fn stash_input(&mut self) -> bool {
        let text = self.textarea.lines().join("\n");
        if text.trim().is_empty() {
            return false;
        }
        let mut drafts = self.session.drafts.clone();
        drafts.insert(0, text);
        if !self.persist_drafts(drafts) {
            return false;
        }
        self.reset_textarea();
        self.runtime.queue_selection = None;
        self.runtime.queue_saved_input = None;
        true
    }

    fn save_selected_stash_edit(&mut self) -> Option<usize> {
        let Some(crate::agent_runtime::QueueSelection::StashedDraft(index)) =
            self.runtime.queue_selection.clone()
        else {
            return None;
        };
        let mut drafts = self.session.drafts.clone();
        if index >= drafts.len() {
            return Some(index);
        }
        let edited = self.textarea.lines().join("\n");
        if edited.trim().is_empty() {
            drafts.remove(index);
            if !self.persist_drafts(drafts) {
                return None;
            }
            return Some(index);
        }
        drafts[index] = edited;
        self.persist_drafts(drafts).then_some(index)
    }

    fn recover_orphaned_steering(&mut self) -> bool {
        if self.streaming() {
            return true;
        }
        let mut recovered = self.runtime.queued_steering.clone();
        let mut withdrawn = self.runtime.queued_steering.clone();
        if let Some(crate::agent_runtime::QueueSelection::PendingSteering {
            text,
            consumed: false,
        }) = self.runtime.queue_selection.as_ref()
        {
            withdrawn.push(text.clone());
            let edited = self.textarea.lines().join("\n");
            if !edited.trim().is_empty() {
                recovered.push(edited);
            }
        }
        if recovered.is_empty() {
            return true;
        }
        let mut drafts = recovered;
        drafts.extend(self.session.drafts.iter().cloned());
        if !self.persist_drafts(drafts) {
            return false;
        }
        self.runtime.withdrawn_steering.extend(withdrawn);
        self.runtime.queued_steering.clear();
        self.runtime.queue_selection = None;
        self.runtime.queue_saved_input = None;
        true
    }

    fn pop_latest_steering(&mut self) -> bool {
        let Some(text) = self.runtime.queued_steering.pop() else {
            return false;
        };
        let remaining = self.runtime.queued_steering.clone();
        if let Some(tx) = self.runtime.steering_tx()
            && tx.send(SteeringCommand::ReplacePending(remaining)).is_err()
        {
            self.runtime.queued_steering.push(text);
            return false;
        }
        if self.runtime.queue_saved_input.is_none() {
            self.runtime.queue_saved_input = Some(self.textarea.lines().join("\n"));
        }
        self.runtime.queue_selection =
            Some(crate::agent_runtime::QueueSelection::PendingSteering {
                text: text.clone(),
                consumed: false,
            });
        self.textarea = crate::app::App::make_textarea();
        self.textarea.insert_str(&text);
        self.completion.clear();
        true
    }

    fn select_stashed_draft(&mut self, index: usize) -> bool {
        let Some(text) = self.session.drafts.get(index) else {
            return false;
        };
        if self.runtime.queue_saved_input.is_none() {
            self.runtime.queue_saved_input = Some(self.textarea.lines().join("\n"));
        }
        self.runtime.queue_selection =
            Some(crate::agent_runtime::QueueSelection::StashedDraft(index));
        self.textarea = crate::app::App::make_textarea();
        self.textarea.insert_str(text);
        self.completion.clear();
        true
    }

    fn restore_queue_composer(&mut self) {
        let saved = self.runtime.queue_saved_input.take().unwrap_or_default();
        self.runtime.queue_selection = None;
        self.textarea = crate::app::App::make_textarea();
        self.textarea.insert_str(&saved);
        self.completion.clear();
    }

    fn save_pending_selection_to_stash(&mut self) -> Option<bool> {
        let Some(crate::agent_runtime::QueueSelection::PendingSteering { text, consumed }) =
            self.runtime.queue_selection.clone()
        else {
            return None;
        };
        let edited = self.textarea.lines().join("\n");
        if edited.trim().is_empty() || (consumed && edited == text) {
            self.runtime.queue_selection = None;
            return Some(false);
        }
        let mut drafts = self.session.drafts.clone();
        drafts.insert(0, edited);
        if !self.persist_drafts(drafts) {
            return None;
        }
        if !consumed {
            self.runtime.withdrawn_steering.push(text);
        }
        self.runtime.queue_selection = None;
        Some(true)
    }

    /// Move one item toward the oldest pending steering / composer boundary.
    pub(crate) fn navigate_queue_up(&mut self) {
        if !self.streaming() && self.is_stepping() {
            self.step_back();
            return;
        }
        if !self.recover_orphaned_steering() {
            return;
        }
        match self.runtime.queue_selection.clone() {
            Some(crate::agent_runtime::QueueSelection::PendingSteering { .. }) => {
                if self.save_pending_selection_to_stash().is_none() {
                    return;
                }
                if !self.pop_latest_steering() {
                    self.restore_queue_composer();
                }
            }
            Some(crate::agent_runtime::QueueSelection::StashedDraft(index)) => {
                let saved_index = match self.save_selected_stash_edit() {
                    Some(index) => index,
                    None => return,
                };
                if saved_index > 0 {
                    self.select_stashed_draft(saved_index - 1);
                } else {
                    self.restore_queue_composer();
                }
                let _ = index;
            }
            None => {
                let text = self.textarea.lines().join("\n");
                if !text.trim().is_empty() {
                    let _ = self.stash_input();
                } else if self.streaming() {
                    let _ = self.pop_latest_steering();
                } else {
                    self.step_back();
                }
            }
        }
    }

    /// Move one item toward the stash end; pending steering is never re-enqueued.
    pub(crate) fn navigate_queue_down(&mut self) {
        if !self.streaming() && self.is_stepping() {
            self.step_forward();
            return;
        }
        if !self.recover_orphaned_steering() {
            return;
        }
        match self.runtime.queue_selection.clone() {
            Some(crate::agent_runtime::QueueSelection::PendingSteering { .. }) => {
                let old_stash_len = self.session.drafts.len();
                if self.save_pending_selection_to_stash().is_none() {
                    return;
                }
                let next_index = if self.session.drafts.len() > old_stash_len {
                    1
                } else {
                    0
                };
                if !self.select_stashed_draft(next_index) {
                    self.restore_queue_composer();
                }
            }
            Some(crate::agent_runtime::QueueSelection::StashedDraft(_)) => {
                let deleted = self.textarea.lines().join("\n").trim().is_empty();
                let index = match self.save_selected_stash_edit() {
                    Some(index) => index,
                    None => return,
                };
                let next_index = if deleted { index } else { index + 1 };
                if !self.select_stashed_draft(next_index) && deleted {
                    self.restore_queue_composer();
                }
            }
            None => {
                if self.textarea.lines().join("\n").trim().is_empty()
                    && !self.select_stashed_draft(0)
                    && !self.streaming()
                {
                    self.step_forward();
                }
            }
        }
    }

    /// Submit the current unified-queue selection.
    pub(crate) fn submit_selected_queue_item(&mut self, provider: &DynProvider) -> bool {
        let text = self.textarea.lines().join("\n");
        let Some(selection) = self.runtime.queue_selection.clone() else {
            return false;
        };
        if text.trim().is_empty() {
            return false;
        }
        if !self.provider.provider_selected || self.login.active {
            return false;
        }
        match selection {
            crate::agent_runtime::QueueSelection::PendingSteering { .. } => {
                if self.streaming() {
                    self.enqueue_steering_from_input()
                } else {
                    self.runtime.queue_selection = None;
                    self.runtime.queue_saved_input = None;
                    self.submit_chat_message(provider);
                    true
                }
            }
            crate::agent_runtime::QueueSelection::StashedDraft(index) => {
                if index >= self.session.drafts.len() {
                    return false;
                }
                let original_drafts = self.session.drafts.clone();
                let mut remaining = original_drafts.clone();
                remaining.remove(index);
                let mut reordered = remaining.split_off(index.min(remaining.len()));
                reordered.extend(remaining);
                if !self.persist_drafts(reordered) {
                    return false;
                }
                if self.streaming() {
                    if self.enqueue_steering_from_input() {
                        true
                    } else {
                        let _ = self.persist_drafts(original_drafts);
                        false
                    }
                } else {
                    self.runtime.queue_selection = None;
                    self.runtime.queue_saved_input = None;
                    self.submit_chat_message(provider);
                    true
                }
            }
        }
    }

    /// Queue a user steering message while the agent loop is running.
    pub fn enqueue_steering_from_input(&mut self) -> bool {
        if self.streaming() && self.runtime.abort_stage >= CancelLevel::HardAbort {
            self.show_abort_pending_notice();
            return false;
        }
        let text = self.textarea.lines().join("\n");
        let trimmed = text.trim().to_string();
        if trimmed.is_empty() || !self.streaming() || self.login.active {
            return false;
        }

        let command = SteeringCommand::Enqueue(trimmed.clone());
        let Some(tx) = self.runtime.steering_tx() else {
            return false;
        };

        if tx.send(command).is_ok() {
            self.runtime.queued_steering.push(trimmed);
            self.runtime.queue_selection = None;
            self.runtime.queue_saved_input = None;
            self.reset_textarea();
            self.log_view.auto_scroll = true;
            true
        } else {
            false
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
        self.session.pending_manual_compaction_requested = true;
        self.session.pending_manual_compaction_instructions = instructions;

        if self.check_token_preflight(RetryTarget::AgentTurn) {
            return;
        }

        self.begin_agent_turn();
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
                            image_data: None,
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
                    // Keep the transcript readable; the actual image travels
                    // as structured payload alongside the text placeholder.
                    let content = format!("[Image: {path}]");
                    self.session
                        .append_event_immediate(SessionEvent::ToolResult {
                            id: call_id,
                            name: "read_file".to_string(),
                            content,
                            image_data: Some(crate::llm::ImageData {
                                base64: base64.clone(),
                                mime_type: mime_type.clone(),
                            }),
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

            // Insert the abort result immediately after its own intent so the
            // ToolCall/ToolResult pairing stays intact (parallel batches emit
            // all intents before any result).
            let call_pos =
                self.session.pending_turn_events.iter().rposition(
                    |e| matches!(e, SessionEvent::ToolCall { id: cid, .. } if cid == &id),
                );
            let name = call_pos
                .and_then(|pos| match &self.session.pending_turn_events[pos] {
                    SessionEvent::ToolCall { name, .. } => Some(name.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            let insert_at = call_pos
                .map(|pos| pos + 1)
                .unwrap_or(self.session.pending_turn_events.len());

            self.session.pending_turn_events.insert(
                insert_at,
                SessionEvent::ToolResult {
                    id,
                    name,
                    content: "Interrupted by user".to_string(),
                    image_data: None,
                    is_error: true,
                    display_range: None,
                    include_in_llm: true,
                    timestamp: Self::now_ts(),
                },
            );
        }
    }

    pub(crate) fn show_abort_pending_notice(&mut self) {
        self.agent_turn.set_status(Some(StreamingStatus::Message(
            "[Aborting current tool; wait for it to finish before submitting]".to_string(),
        )));
    }

    fn clear_abort_status_notice(&mut self) {
        if matches!(
            self.agent_turn.status,
            Some(StreamingStatus::CompletedMessage(ref s)) if s == "[agent loop aborted]"
        ) {
            self.agent_turn.set_status(None);
        }
    }

    pub(crate) fn preserve_queued_steering(&mut self) -> bool {
        let restore_input = self
            .runtime
            .queue_selection
            .as_ref()
            .map(|_| self.runtime.queue_saved_input.clone().unwrap_or_default());
        if matches!(
            self.runtime.queue_selection,
            Some(crate::agent_runtime::QueueSelection::StashedDraft(_))
        ) {
            if self.save_selected_stash_edit().is_none() {
                return false;
            }
            self.runtime.queue_selection = None;
        }
        let pending_selection = matches!(
            self.runtime.queue_selection,
            Some(crate::agent_runtime::QueueSelection::PendingSteering {
                consumed: false,
                ..
            })
        );
        if self.runtime.queued_steering.is_empty() && !pending_selection {
            self.runtime.queue_selection = None;
            self.runtime.queue_saved_input = None;
            if let Some(saved) = restore_input {
                self.textarea = crate::app::App::make_textarea();
                self.textarea.insert_str(&saved);
            }
            return true;
        }
        let mut recovered = self.runtime.queued_steering.clone();
        let mut withdrawn = self.runtime.queued_steering.clone();
        if let Some(crate::agent_runtime::QueueSelection::PendingSteering {
            text,
            consumed: false,
        }) = self.runtime.queue_selection.as_ref()
        {
            withdrawn.push(text.clone());
            let edited = self.textarea.lines().join("\n");
            if !edited.trim().is_empty() {
                recovered.push(edited);
            }
        }
        if !recovered.is_empty() {
            let mut drafts = recovered.clone();
            drafts.extend(self.session.drafts.iter().cloned());
            if !self.persist_drafts(drafts) {
                self.push_notice(Message::assistant(
                    "[pending steering remains available; draft recovery could not be saved]",
                ));
                return false;
            }
        }
        self.runtime.withdrawn_steering.extend(withdrawn);
        self.runtime.queued_steering.clear();
        self.runtime.queue_selection = None;
        self.runtime.queue_saved_input = None;
        if let Some(saved) = restore_input {
            self.textarea = crate::app::App::make_textarea();
            self.textarea.insert_str(&saved);
        }
        true
    }

    pub fn abort_agent_loop(&mut self) {
        if let Some(handle) = self.runtime.take_agent_handle() {
            // Signal cooperative cancellation; hard-abort as fallback.
            let _ = handle.cancel_sender().send(CancelLevel::HardAbort);
            handle.task.abort();
            self.agent_turn
                .set_status(Some(StreamingStatus::CompletedMessage(
                    "[agent loop aborted]".to_string(),
                )));
            self.agent_turn.holdoff_started_at = None;
            self.preserve_queued_steering();
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
        if let Some(tx) = self.runtime.cancel_tx() {
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
        if let Some(tx) = self.runtime.cancel_tx() {
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
                "[agent loop aborted]".to_string(),
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
        if let Some(tx) = self.runtime.cancel_tx() {
            let _ = tx.send(CancelLevel::ForceKill);
        }
        self.agent_turn.set_status(Some(StreamingStatus::Message(
            "[Force abort requested; waiting for current tool to stop]".to_string(),
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
