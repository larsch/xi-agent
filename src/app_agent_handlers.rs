//! AgentEvent handler methods split out of `app.rs`.

use tokio::sync::mpsc::error::TryRecvError;

use crate::agent::types::AgentEvent;
use crate::app::{App, RetryTarget, StreamingStatus};
use crate::app_event::AppEvent;
use crate::live_turn::{LiveToolEntry, LiveToolResult};
use crate::llm::{AssistantPhase, DisplayRange, UsageStats};
use crate::provider_manager::{active_provider_display_name, format_provider_error_for_display};
use crate::session_event::SessionEvent;

/// Current wall-clock time as seconds since UNIX epoch.
pub(crate) fn now_ts() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

impl App {
    // ── Agent event handling ──────────────────────────────────────────────────

    /// Current wall-clock time as seconds since UNIX epoch.
    pub(crate) fn now_ts() -> u64 {
        now_ts()
    }

    /// Flush `pending_turn_events` to the event log and clear the buffer.
    ///
    /// Called at every turn-completion boundary (`TurnEnd`, `Done`, `Error`).
    pub(crate) fn flush_turn_events(&mut self) {
        self.session.flush_turn_events();
    }

    /// Append a single event to the event log immediately (for events that are
    /// complete units on their own: `UserMessage`, `ModelChanged`,
    /// `ThinkingLevelChanged`).
    pub(crate) fn append_event_immediate(&mut self, ev: SessionEvent) {
        self.session.append_event_immediate(ev);
    }

    pub fn apply_app_event(&mut self, ev: AppEvent) {
        match ev {
            AppEvent::Agent(ev) => {
                self.apply_agent_event(ev);
                self.drain_app_events();
            }
            AppEvent::ModelsReady(result) => self.apply_model_list(result),
            AppEvent::Login(ev) => self.apply_login_event(ev),
            AppEvent::AskUser(req) => self.receive_ask_request(req),
        }
    }

    // ── AgentEvent handlers ───────────────────────────────────────────────────

    /// Dispatch an `AgentEvent` to the appropriate named handler.
    pub fn apply_agent_event(&mut self, ev: AgentEvent) {
        match ev {
            AgentEvent::ThinkingToken(token) => self.on_thinking_token(token),
            AgentEvent::Usage(usage) => self.on_usage(usage),
            AgentEvent::TextToken { text, phase } => self.on_text_token(text, phase),
            AgentEvent::ToolCallIntent {
                id,
                name,
                streaming_field,
            } => self.on_tool_call_intent(id, name, streaming_field),
            AgentEvent::ToolCallArgsDelta { id, partial_json } => {
                self.on_tool_call_args_delta(id, partial_json)
            }
            AgentEvent::SteeringConsumed { text } => self.on_steering_consumed(text),
            AgentEvent::StatusUpdate(msg) => self.on_status_update(msg),
            AgentEvent::Compacting => self.on_compacting(),
            AgentEvent::CompactionDone {
                summary,
                trigger_reason,
                context_window,
                reserve_tokens,
                keep_recent_tokens,
                tokens_before,
                tokens_after,
                retained_event_count,
                read_files,
                modified_files,
            } => self.on_compaction_done(
                summary,
                trigger_reason,
                context_window,
                reserve_tokens,
                keep_recent_tokens,
                tokens_before,
                tokens_after,
                retained_event_count,
                read_files,
                modified_files,
            ),
            AgentEvent::ToolCallStart { id, name, args } => self.on_tool_call_start(id, name, args),
            AgentEvent::ToolOutputChunk { id, chunk } => self.on_tool_output_chunk(id, chunk),
            AgentEvent::ToolCallEnd { id, result } => self.on_tool_call_end(id, result),
            AgentEvent::ExternalFileChange {
                paths: _,
                notification,
            } => self.on_external_file_change(notification),
            AgentEvent::TurnEnd => self.on_turn_end(),
            AgentEvent::Done => self.on_agent_done(),
            AgentEvent::Error(e) => self.on_agent_error(e),
        }
    }

    fn on_thinking_token(&mut self, token: String) {
        if !token.trim().is_empty() {
            self.last_output_at = Some(std::time::Instant::now());
        }
        self.session
            .live_turn
            .assistant_thinking
            .get_or_insert_with(String::new)
            .push_str(&token);
    }

    fn on_usage(&mut self, usage: UsageStats) {
        self.latest_usage = Some(usage);
    }

    fn on_text_token(&mut self, text: String, phase: AssistantPhase) {
        if !text.trim().is_empty() {
            self.last_output_at = Some(std::time::Instant::now());
        }
        self.session.live_turn.assistant_content.push_str(&text);
        if phase != AssistantPhase::Unknown {
            self.session.live_turn.assistant_phase = phase;
        }
    }

    fn on_tool_call_intent(&mut self, id: String, name: String, streaming_field: Option<String>) {
        self.session.live_turn.assistant_phase = AssistantPhase::Provisional;
        // Create a live entry with no args yet — partial args will stream in.
        self.session.live_turn.tool_entries.push(LiveToolEntry {
            id,
            name,
            args: serde_json::Value::Object(Default::default()),
            partial_args: String::new(),
            partial_snapshot: None,
            streaming_field,
            running_output: String::new(),
            result: None,
        });
    }

    fn on_tool_call_args_delta(&mut self, id: String, partial_json: String) {
        if let Some(entry) = self.session.live_turn.find_tool_entry_mut(&id) {
            entry.partial_args.push_str(&partial_json);
            if let Ok(completed) = jawohl::complete_json(&entry.partial_args)
                && let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&completed)
            {
                entry.partial_snapshot = Some(parsed);
            }
        }
    }

    fn on_steering_consumed(&mut self, text: String) {
        self.last_output_at = Some(std::time::Instant::now());
        if let Some(pos) = self.runtime.queued_steering.iter().position(|m| m == &text) {
            self.runtime.queued_steering.remove(pos);
        }
        // Flush any buffered assistant turn events first so the assistant
        // response appears before the steering message in the conversation log.
        // (The agent sends SteeringConsumed before TurnEnd, so without this the
        // UserMessage would be committed before the AssistantMessage.)
        if !self.session.pending_turn_events.is_empty() {
            self.finalise_assistant_turn_event();
            self.flush_turn_events();
        }
        // Steering messages are user messages — append immediately.
        self.append_user_message(text);
    }

    fn on_status_update(&mut self, msg: String) {
        if !msg.is_empty() {
            self.last_output_at = Some(std::time::Instant::now());
        }
        self.streaming_status = if msg.is_empty() {
            Some(StreamingStatus::Waiting)
        } else {
            Some(StreamingStatus::Message(msg))
        };
    }

    fn on_compacting(&mut self) {
        self.last_output_at = Some(std::time::Instant::now());
        self.streaming_status = Some(StreamingStatus::Message("compacting…".to_string()));
    }

    #[allow(clippy::too_many_arguments)]
    fn on_compaction_done(
        &mut self,
        summary: String,
        trigger_reason: crate::session_event::CompactionTrigger,
        context_window: usize,
        reserve_tokens: usize,
        keep_recent_tokens: usize,
        tokens_before: usize,
        tokens_after: usize,
        retained_event_count: usize,
        read_files: Vec<String>,
        modified_files: Vec<String>,
    ) {
        let ev = SessionEvent::CompactionSummary {
            summary,
            trigger_reason,
            context_window,
            reserve_tokens,
            keep_recent_tokens,
            tokens_before,
            tokens_after,
            retained_event_count: Some(retained_event_count),
            read_files,
            modified_files,
            timestamp: Self::now_ts(),
        };
        self.append_event_immediate(ev);
        // append_immediate already updates display incrementally via SessionState.
        self.latest_usage = Some(UsageStats {
            input_tokens: Some(tokens_after),
            output_tokens: None,
            total_tokens: Some(tokens_after),
        });
        self.log_view.auto_scroll = true;
        self.persist_messages();
    }

    fn on_tool_call_start(&mut self, id: String, name: String, args: serde_json::Value) {
        self.last_output_at = Some(std::time::Instant::now());
        // The live entry was already created by on_tool_call_intent when the
        // LLM started the tool block. Update it with the complete args.
        // If for some reason no intent entry exists (e.g. provider that skips
        // ToolCallIntent), push a new one.
        if let Some(entry) = self.session.live_turn.find_tool_entry_mut(&id) {
            entry.args = args.clone();
            entry.partial_snapshot = Some(args.clone());
        } else {
            self.session.live_turn.tool_entries.push(LiveToolEntry {
                id: id.clone(),
                name: name.clone(),
                args: args.clone(),
                partial_args: String::new(),
                partial_snapshot: Some(args.clone()),
                streaming_field: None,
                running_output: String::new(),
                result: None,
            });
        }
        // ToolCall is buffered; only flushed together with its result.
        self.session
            .pending_turn_events
            .push(SessionEvent::ToolCall {
                id,
                name,
                args,
                timestamp: Self::now_ts(),
            });
    }

    fn on_tool_output_chunk(&mut self, id: String, chunk: String) {
        if let Some(entry) = self.session.live_turn.find_tool_entry_mut(&id) {
            entry.running_output.push_str(&chunk);
        }
    }

    fn on_tool_call_end(&mut self, id: String, result: crate::agent::types::ToolResult) {
        self.last_output_at = Some(std::time::Instant::now());
        let display_range = result.truncation.as_ref().map(|tr| DisplayRange {
            first_line: tr.first_kept_line,
            last_line: tr.first_kept_line + tr.output_lines - 1,
            total_lines: tr.total_lines,
        });
        // Update the matching live tool entry with its result.
        if let Some(entry) = self.session.live_turn.find_tool_entry_mut(&id) {
            entry.running_output.clear();
            entry.result = Some(LiveToolResult {
                content: result.content.as_text().to_string(),
                is_error: result.is_error,
                display_range: display_range.clone(),
                image_data: result.content.image_base64().map(|(mime, b64)| {
                    crate::llm::ImageData {
                        base64: b64,
                        mime_type: mime.to_string(),
                    }
                }),
            });
        }
        // Resolve tool name from the matching pending ToolCall.
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
                content: result.content.as_text().to_string(),
                is_error: result.is_error,
                display_range,
                timestamp: Self::now_ts(),
            });
    }

    fn on_external_file_change(&mut self, notification: String) {
        self.last_output_at = Some(std::time::Instant::now());
        // External file change notifications are user-visible context
        // injected into the conversation — treat as UserMessage.
        self.append_user_message(notification);
    }

    fn on_turn_end(&mut self) {
        self.streaming_status = Some(StreamingStatus::Waiting);
        // Finalise the assistant message in the pending buffer before
        // flushing, using the current in-memory messages state.
        self.finalise_assistant_turn_event();
        self.flush_turn_events();
        self.persist_messages();
    }

    fn on_agent_done(&mut self) {
        self.streaming_status = None;
        self.last_output_at = None;
        self.runtime.agent_task = None;
        self.runtime.cancel_tx = None;
        self.runtime.steering_tx = None;
        self.runtime.queued_steering.clear();
        // The final TurnEnd already flushed the turn buffer.
        // Done only cleans up live streaming state.
        self.persist_messages();
    }

    fn on_agent_error(&mut self, e: crate::llm::ProviderError) {
        self.streaming_status = None;
        self.last_output_at = None;
        self.runtime.agent_task = None;
        self.runtime.cancel_tx = None;
        self.runtime.steering_tx = None;
        self.runtime.queued_steering.clear();

        let is_unauthorized = e.kind == crate::llm::ProviderErrorKind::Unauthorized;

        if is_unauthorized
            && self.login.auth_retry_budget > 0
            && self.trigger_auth_refresh(RetryTarget::AgentTurn)
        {
            log::debug!(
                "received 401, refresh triggered: provider={} remaining_budget= {}",
                self.provider.current_instance.id,
                self.login.auth_retry_budget
            );
            self.login.auth_retry_budget -= 1;
            self.streaming_status = None;
            // Refresh triggered; retry will happen automatically after refresh completes.
            // Discard pending events and in-flight turn state — the turn will be retried.
            self.session.pending_turn_events.clear();
            self.session.live_turn.clear_turn();
        } else {
            let provider_label = active_provider_display_name(
                &self.provider.current_instance.id,
                &self.provider.instances,
            );
            let rendered = format_provider_error_for_display(&provider_label, &e);
            self.streaming_status = None;
            // Discard any partially accumulated assistant/tool events
            // and append a TurnError instead. Provider errors are already
            // shown in the output area via the committed TurnError, so do not
            // also keep them as persistent status/notices.
            self.session.pending_turn_events.clear();
            self.session.live_turn.clear_turn();
            self.append_event_immediate(SessionEvent::TurnError {
                message: format!("[Error: {rendered}]"),
                timestamp: Self::now_ts(),
            });
            self.persist_messages();
        }
    }

    /// Assemble the `AssistantMessage` session event from `LiveTurnState` fields
    /// and insert it into `pending_turn_events`.
    ///
    /// Called just before flushing the turn buffer so that the final content,
    /// thinking, phase, and usage are captured directly from `live_turn` —
    /// not read back from committed display state.
    pub(crate) fn finalise_assistant_turn_event(&mut self) {
        let content = self.session.live_turn.assistant_content.clone();
        let thinking = self.session.live_turn.assistant_thinking.clone();
        let phase = self.session.live_turn.assistant_phase;

        // Don't record a completely empty assistant turn with no tools either.
        let has_tools = self
            .session
            .pending_turn_events
            .iter()
            .any(|e| matches!(e, SessionEvent::ToolCall { .. }));
        if content.is_empty() && thinking.is_none() && !has_tools {
            return;
        }

        let ev = SessionEvent::AssistantMessage {
            content,
            thinking,
            phase: if phase == AssistantPhase::Unknown {
                AssistantPhase::Final
            } else {
                phase
            },
            usage: self.latest_usage,
            timestamp: Self::now_ts(),
        };

        // Replace existing AssistantMessage in the buffer or prepend.
        if let Some(pos) = self
            .session
            .pending_turn_events
            .iter()
            .position(|e| matches!(e, SessionEvent::AssistantMessage { .. }))
        {
            self.session.pending_turn_events[pos] = ev;
        } else {
            self.session.pending_turn_events.insert(0, ev);
        }
    }

    pub fn drain_app_events(&mut self) {
        loop {
            match self.runtime.try_recv_app_event() {
                Ok(AppEvent::Agent(ev)) => self.apply_agent_event(ev),
                Ok(other) => self.apply_app_event(other),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
    }
}
