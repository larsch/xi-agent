//! Selection menu, provider wizard, ask-user, and conversation management
//! methods split out of `app.rs`.

use crate::agent::types::{AskRequest, AskUserResponse};
use crate::app::{App, DEFAULT_OLLAMA_ENDPOINT, SelectionResult};
use crate::ask_user_state::PendingAsk;
use crate::auth::LoginEvent;
use crate::completion::CompletionItem;
use crate::export;
use crate::llm::Message;
use crate::login_state::LoginActionKind;
use crate::provider_instance::{ApiType, BackendPreset, ProviderInstance};
use crate::provider_manager::{PendingProviderRemoval, PendingProviderSetup, ProviderSetupStep};
use crate::selection_state::{MAX_SELECTION_VISIBLE, SelectionKind};
use crate::thinking::ThinkingLevel;
use ratatui_textarea::{CursorMove, TextArea};

impl App {
    // ── Selection menu ────────────────────────────────────────────────────────

    pub(crate) fn set_selection_items(&mut self, items: Vec<CompletionItem>) {
        self.selection.set_items(items);
    }

    pub(crate) fn select_current_default(&mut self) {
        let target = match self.selection.kind {
            Some(SelectionKind::Model) => Some(format!("/model {}", self.provider.current_model)),
            Some(SelectionKind::Thinking) => Some(format!(
                "/thinking {}",
                self.provider.current_thinking.as_str()
            )),
            Some(SelectionKind::Provider) => {
                Some(format!("/provider {}", self.provider.current_instance.id))
            }
            Some(SelectionKind::LoginProvider)
            | Some(SelectionKind::ResumeSession)
            | Some(SelectionKind::AskUser)
            | Some(SelectionKind::LoginAction)
            | Some(SelectionKind::ConfirmProviderRemoval)
            | Some(SelectionKind::ProviderApiType)
            | Some(SelectionKind::KeybindingHelp)
            | None => None,
        };

        if let Some(target) = target
            && let Some(idx) = self
                .selection
                .items
                .iter()
                .position(|item| item.complete_to == target)
        {
            self.selection.selected = idx;
            self.selection.ensure_visible();
        }
    }

    pub(crate) fn apply_selection_filter(&mut self) {
        self.selection.apply_filter();
    }

    pub(crate) fn ensure_selection_visible(&mut self) {
        self.selection.ensure_visible();
    }

    /// Open the model selection menu, pre-populating from cache or showing a
    /// loading indicator when the list hasn't been fetched yet.
    pub fn enter_model_selection_mode(&mut self) {
        self.reset_textarea();
        self.session.live_turn.notices.clear();
        let items = if let Some(err) = &self.completion.model_fetch_error {
            vec![CompletionItem::error_indicator(err)]
        } else if let Some(models) = &self.completion.available_models {
            models
                .iter()
                .map(|m| CompletionItem::from_model(m))
                .collect()
        } else {
            vec![CompletionItem::loading_indicator()]
        };
        self.selection
            .activate(SelectionKind::Model, "  Select model  ", items);
        self.select_current_default();
    }

    /// Returns true when the active selection is the provider picker.
    pub fn in_provider_selection_mode(&self) -> bool {
        self.selection.kind == Some(SelectionKind::Provider)
    }

    /// Returns true when the active selection is the provider-removal confirmation.
    pub fn in_provider_removal_confirmation_mode(&self) -> bool {
        self.selection.kind == Some(SelectionKind::ConfirmProviderRemoval)
    }

    /// Returns the currently highlighted provider id in the provider picker.
    pub fn selected_provider_id(&self) -> Option<&str> {
        if self.selection.kind != Some(SelectionKind::Provider) {
            return None;
        }
        self.selection
            .items
            .get(self.selection.selected)?
            .complete_to
            .strip_prefix("/provider ")
    }

    /// Open the thinking-level selection menu.
    pub fn enter_thinking_selection_mode(&mut self) {
        self.reset_textarea();
        self.session.live_turn.notices.clear();
        let items = ThinkingLevel::all()
            .iter()
            .map(|lvl| CompletionItem {
                label: lvl.as_str().to_string(),
                detail: String::new(),
                complete_to: format!("/thinking {}", lvl.as_str()),
                loading: false,
                error: false,
                match_range: None,
            })
            .collect();
        self.selection
            .activate(SelectionKind::Thinking, "  Select thinking  ", items);
        self.select_current_default();
    }

    /// Open the provider selection menu showing configured instances.
    ///
    /// When no providers are configured, shows a placeholder with a hint to
    /// use `/login`.  A "Login to a service…" entry is always present at the
    /// bottom to jump to the login menu.
    pub fn enter_provider_selection_mode(&mut self, instances: &[ProviderInstance]) {
        self.reset_textarea();
        self.session.live_turn.notices.clear();
        let mut items: Vec<CompletionItem> = if instances.is_empty() {
            vec![CompletionItem {
                label: "No providers configured".to_string(),
                detail: "Use /login to connect to a service".to_string(),
                complete_to: String::new(),
                loading: false,
                error: false,
                match_range: None,
            }]
        } else {
            instances
                .iter()
                .map(|p| CompletionItem::from_provider(&p.id, &p.label()))
                .collect()
        };
        items.push(CompletionItem {
            label: "Login to a service…".to_string(),
            detail: "Connect to a new provider".to_string(),
            complete_to: "/login".to_string(),
            loading: false,
            error: false,
            match_range: None,
        });
        self.selection
            .activate(SelectionKind::Provider, "  Select provider  ", items);
        self.select_current_default();
    }

    /// Start editing an existing custom provider instance.
    pub fn enter_provider_edit_mode(&mut self, instance: &ProviderInstance) {
        self.exit_selection_mode();
        self.provider.pending_removal = None;
        self.provider.pending_setup = Some(PendingProviderSetup::from_instance(instance));
        self.enter_provider_endpoint_input_mode();
        self.textarea = Self::make_textarea();
        if let Some(base_url) = instance.base_url.as_deref() {
            self.textarea.insert_str(base_url);
        }
    }

    pub fn pending_provider_setup_is_edit(&self) -> bool {
        self.provider.pending_setup_is_edit()
    }

    pub fn pending_provider_original_id(&self) -> Option<&str> {
        self.provider.pending_original_id()
    }

    /// Begin setup for a new custom provider instance.
    /// Enter freeform input mode for the new provider instance name.
    pub fn enter_provider_name_input_mode(&mut self) {
        self.exit_selection_mode();
        self.reset_textarea();
        self.provider.setup_step = ProviderSetupStep::Name;
        self.provider.pending_removal = None;
        if let Some(existing_id) = self
            .provider
            .pending_setup
            .as_ref()
            .filter(|setup| setup.editing_existing)
            .map(|setup| setup.id.clone())
        {
            self.textarea.insert_str(&existing_id);
        } else if let Some(suggested) = self.suggested_pending_provider_id() {
            self.textarea.insert_str(&suggested);
        }
    }

    /// Enter freeform input mode for a provider endpoint / base URL.
    /// This is the unified entry point that replaces `enter_ollama_endpoint_freeform_mode`,
    /// `enter_open_webui_url_input_mode`, and `enter_provider_base_url_input_mode`.
    pub fn enter_provider_endpoint_input_mode(&mut self) {
        self.exit_selection_mode();
        self.reset_textarea();
        self.provider.setup_step = ProviderSetupStep::Endpoint;
        // Pre-fill with the default endpoint hint for Ollama when adding a new instance.
        if !self.pending_provider_setup_is_edit()
            && self
                .pending_provider_instance()
                .is_some_and(|i| i.backend_preset == BackendPreset::Ollama)
        {
            self.textarea.insert_str(DEFAULT_OLLAMA_ENDPOINT);
        }
    }

    /// Enter freeform input mode for a provider API key / token.
    pub fn enter_provider_api_key_input_mode(&mut self) {
        self.exit_selection_mode();
        self.reset_textarea();
        let pending_url =
            if let ProviderSetupStep::ApiKey { pending_url } = &self.provider.setup_step {
                pending_url.clone()
            } else {
                None
            };
        self.provider.setup_step = ProviderSetupStep::ApiKey { pending_url };
    }

    /// Cancel the active provider setup input and clear pending state.
    pub fn cancel_setup_input(&mut self) {
        self.provider.setup_step = ProviderSetupStep::Idle;
        self.provider.pending_setup = None;
        self.provider.pending_removal = None;
        self.reset_textarea();
    }

    fn suggested_pending_provider_id(&self) -> Option<String> {
        self.provider.suggested_id()
    }

    /// Read the typed provider name, normalize it into a stable id, and store
    /// it as the pending add-provider setup target.
    pub fn submit_provider_name_input(
        &mut self,
        existing_instances: &[ProviderInstance],
    ) -> Option<String> {
        let raw = self.textarea.lines().join(" ");
        let id = self.provider.submit_name_input(&raw, existing_instances)?;
        self.reset_textarea();
        Some(id)
    }

    pub fn submit_pending_provider_base_url(&mut self) -> Option<String> {
        let raw = self.textarea.lines().join("");
        let url = self.provider.submit_base_url(&raw)?;
        self.reset_textarea();
        Some(url)
    }

    pub fn submit_pending_provider_api_key(&mut self) -> Option<String> {
        let raw = self.textarea.lines().join("");
        let token = self.provider.submit_api_key(&raw)?;
        self.reset_textarea();
        Some(token)
    }
    /// Show the API-type menu for the pending provider instance.
    pub fn enter_provider_api_type_selection_mode(&mut self, backend_preset: &BackendPreset) {
        self.reset_textarea();
        self.session.live_turn.notices.clear();
        let items = backend_preset
            .def()
            .allowed_apis
            .iter()
            .map(|api| CompletionItem {
                label: api.label().to_string(),
                detail: String::new(),
                complete_to: format!("/provider_api {}", api.label()),
                loading: false,
                error: false,
                match_range: None,
            })
            .collect();
        self.selection
            .activate(SelectionKind::ProviderApiType, "  Select API type  ", items);
    }

    // Used by tests in app.rs.
    #[allow(dead_code)]
    pub fn set_pending_provider_backend_preset(&mut self, backend_preset: BackendPreset) {
        self.provider.set_pending_backend_preset(backend_preset);
    }

    pub fn set_pending_provider_api_type(&mut self, api_type: ApiType) {
        self.provider.set_pending_api_type(api_type);
    }

    pub fn pending_provider_instance(&self) -> Option<ProviderInstance> {
        self.provider.pending_instance()
    }

    pub fn finish_pending_provider_setup(&mut self) -> Option<ProviderInstance> {
        self.provider.finish_setup()
    }

    pub fn clear_pending_provider_setup(&mut self) {
        self.provider.clear_setup();
    }

    pub fn enter_provider_removal_confirmation_mode(&mut self, instance: &ProviderInstance) {
        self.reset_textarea();
        self.session.live_turn.notices.clear();
        self.provider.pending_setup = None;
        self.provider.pending_removal = Some(PendingProviderRemoval {
            id: instance.id.clone(),
        });
        let items = vec![
            CompletionItem {
                label: format!("Remove {}", instance.id),
                detail: format!(
                    "Delete custom provider ({})",
                    instance.backend_preset.label()
                ),
                complete_to: "/provider_remove_confirm".to_string(),
                loading: false,
                error: false,
                match_range: None,
            },
            CompletionItem {
                label: "Cancel".to_string(),
                detail: "Keep provider".to_string(),
                complete_to: "/provider_remove_cancel".to_string(),
                loading: false,
                error: false,
                match_range: None,
            },
        ];
        self.selection.activate(
            SelectionKind::ConfirmProviderRemoval,
            "  Remove provider?  ",
            items,
        );
    }

    pub fn clear_pending_provider_removal(&mut self) {
        self.provider.clear_removal();
    }

    /// Read the textarea as an Ollama endpoint URL, normalize shorthand
    /// forms, and return `Some(url)` if it looks valid, `None` otherwise.
    pub fn take_ollama_endpoint_input(&mut self) -> Option<String> {
        let raw = self.textarea.lines().join("");
        let norm = BackendPreset::Ollama.def().url_normalization.as_ref()?;
        let url = norm.normalize(&raw)?;
        self.provider.setup_step = ProviderSetupStep::Idle;
        self.reset_textarea();
        Some(url)
    }

    // ── Open WebUI interactive setup ──────────────────────────────────────────

    /// Submit the URL typed in Open WebUI URL input mode.
    /// Returns the normalised URL if valid, and transitions to token input mode.
    pub fn submit_open_webui_url_input(&mut self) -> Option<String> {
        let instance = self.pending_provider_instance()?;
        let raw = self.textarea.lines().join("");
        let norm = instance.backend_preset.def().url_normalization.as_ref()?;
        let url = norm.normalize(&raw)?;
        if let Some(setup) = self.provider.pending_setup.as_mut() {
            setup.base_url = Some(url.clone());
        }
        // Transition to ApiKey step, carrying the URL forward.
        self.provider.setup_step = ProviderSetupStep::ApiKey {
            pending_url: Some(url.clone()),
        };
        self.exit_selection_mode();
        self.reset_textarea();
        if let Some(existing_token) = self
            .provider
            .pending_setup
            .as_ref()
            .and_then(|setup| setup.api_key.as_deref())
        {
            self.textarea.insert_str(existing_token);
        }
        Some(url)
    }

    /// Submit the token typed in Open WebUI token input mode.
    /// Returns `Some((url, token))` if a pending URL exists and the token is non-empty.
    pub fn take_open_webui_token_input(&mut self) -> Option<(String, String)> {
        let token = self.submit_pending_provider_api_key()?;
        let url = if let ProviderSetupStep::ApiKey { pending_url } = &self.provider.setup_step {
            pending_url.clone()
        } else {
            None
        };
        // pending_url may already have been cleared by submit_pending_provider_api_key
        // so fall back to setup.base_url if needed
        let url = url.or_else(|| {
            self.provider
                .pending_setup
                .as_ref()
                .and_then(|s| s.base_url.clone())
        })?;
        self.provider.setup_step = ProviderSetupStep::Idle;
        Some((url, token))
    }

    /// Open provider picker for `/login` command.
    pub fn enter_login_selection_mode(&mut self) {
        self.reset_textarea();
        self.session.live_turn.notices.clear();
        self.login.enter_login_selection_mode(&mut self.selection);
    }

    /// Dismiss the selection menu without applying a choice.
    pub fn exit_selection_mode(&mut self) {
        self.selection.reset();
    }

    /// Returns true when a model fetch should be triggered for the model
    /// selection menu (models not yet loaded, no fetch in flight).
    pub fn should_fetch_models_for_selection(&self) -> bool {
        // Only fetch if the menu shows the loading indicator (model menu).
        self.selection.active
            && self.selection.kind == Some(SelectionKind::Model)
            && self.selection.items.iter().any(|i| i.loading)
            && !self.completion.models_loading
    }

    /// Returns true if the active selection menu supports free-text filtering.
    /// The login-action menu is a fixed short list and disables filtering.
    pub fn selection_filter_enabled(&self) -> bool {
        !matches!(
            self.selection.kind,
            Some(SelectionKind::LoginAction) | Some(SelectionKind::ConfirmProviderRemoval)
        )
    }

    pub fn selection_add_char(&mut self, c: char) {
        // The login action menu is a small fixed list; filtering adds no value.
        if self.selection.kind == Some(SelectionKind::LoginAction) {
            return;
        }
        self.selection.query.push(c);
        self.apply_selection_filter();
    }

    pub fn selection_backspace(&mut self) {
        if self.selection.kind == Some(SelectionKind::LoginAction) {
            return;
        }
        self.selection.query.pop();
        self.apply_selection_filter();
    }

    /// Navigate the selection menu down (wraps around).
    pub fn selection_select_next(&mut self) {
        if self.selection.items.is_empty() {
            return;
        }
        self.selection.selected =
            advance_selection(&self.selection.items, self.selection.selected, true);
        if self.selection.selected == 0 {
            self.selection.scroll = 0;
        } else {
            self.ensure_selection_visible();
        }
    }

    /// Navigate the selection menu up (wraps around).
    pub fn selection_select_prev(&mut self) {
        if self.selection.items.is_empty() {
            return;
        }
        self.selection.selected =
            advance_selection(&self.selection.items, self.selection.selected, false);
        if self.selection.selected == self.selection.items.len() - 1 {
            self.selection.scroll = self
                .selection
                .items
                .len()
                .saturating_sub(MAX_SELECTION_VISIBLE);
        } else {
            self.ensure_selection_visible();
        }
    }

    /// Jump forward one page (MAX_SELECTION_VISIBLE items) in the selection menu.
    pub fn selection_page_down(&mut self) {
        if self.selection.items.is_empty() {
            return;
        }
        let len = self.selection.items.len();
        let start = self.selection.selected;
        let target = (self.selection.selected + MAX_SELECTION_VISIBLE).min(len - 1);
        self.selection.selected = nearest_non_loading(&self.selection.items, target, true);
        if self.selection.selected != start {
            self.ensure_selection_visible();
        }
    }

    /// Jump backward one page (MAX_SELECTION_VISIBLE items) in the selection menu.
    pub fn selection_page_up(&mut self) {
        if self.selection.items.is_empty() {
            return;
        }
        let start = self.selection.selected;
        let target = self
            .selection
            .selected
            .saturating_sub(MAX_SELECTION_VISIBLE);
        self.selection.selected = nearest_non_loading(&self.selection.items, target, false);
        if self.selection.selected != start {
            self.ensure_selection_visible();
        }
    }

    /// Confirm the currently highlighted selection.
    pub fn apply_selection(&mut self) -> Option<SelectionResult> {
        let item = self.selection.items.get(self.selection.selected)?;
        if item.loading || item.complete_to.is_empty() {
            return None;
        }

        let result = match self.selection.kind {
            Some(SelectionKind::Model) => item
                .complete_to
                .strip_prefix("/model ")
                .map(|name| SelectionResult::Model(name.to_string())),
            Some(SelectionKind::Thinking) => item
                .complete_to
                .strip_prefix("/thinking ")
                .and_then(ThinkingLevel::parse)
                .map(SelectionResult::Thinking),
            Some(SelectionKind::Provider) => {
                if let Some(name) = item.complete_to.strip_prefix("/provider ") {
                    Some(SelectionResult::Provider(name.to_string()))
                } else if item.complete_to == "/login" {
                    self.enter_login_selection_mode();
                    Some(SelectionResult::LoginProvider(String::new()))
                } else {
                    None
                }
            }
            Some(SelectionKind::ConfirmProviderRemoval) => match item.complete_to.as_str() {
                "/provider_remove_confirm" => self
                    .provider
                    .pending_removal
                    .as_ref()
                    .map(|pending| SelectionResult::RemoveProvider(pending.id.clone())),
                "/provider_remove_cancel" => Some(SelectionResult::CancelProviderRemoval),
                _ => None,
            },
            Some(SelectionKind::ProviderApiType) => item
                .complete_to
                .strip_prefix("/provider_api ")
                .and_then(|label| {
                    self.provider
                        .pending_setup
                        .as_ref()?
                        .backend_preset
                        .as_ref()?
                        .def()
                        .allowed_apis
                        .iter()
                        .find(|api| api.label() == label)
                        .cloned()
                })
                .map(SelectionResult::ProviderApiType),
            Some(SelectionKind::LoginProvider) => item
                .complete_to
                .strip_prefix("/login ")
                .map(|name| SelectionResult::LoginProvider(name.to_string())),
            Some(SelectionKind::ResumeSession) => item
                .complete_to
                .strip_prefix("/resume_session ")
                .map(|id| SelectionResult::ResumeSession(id.to_string())),
            Some(SelectionKind::AskUser) => item
                .complete_to
                .strip_prefix("/ask_user_option ")
                .map(|name| SelectionResult::AskOption(name.to_string()))
                .or_else(|| {
                    (item.complete_to == "/ask_user_freeform")
                        .then_some(SelectionResult::AskFreeform)
                }),
            Some(SelectionKind::LoginAction) => match item.complete_to.as_str() {
                crate::login_state::LOGIN_ACTION_OPEN_BROWSER => {
                    Some(SelectionResult::LoginAction(LoginActionKind::OpenBrowser))
                }
                crate::login_state::LOGIN_ACTION_COPY_URL => {
                    Some(SelectionResult::LoginAction(LoginActionKind::CopyUrl))
                }
                crate::login_state::LOGIN_ACTION_COPY_CODE => {
                    Some(SelectionResult::LoginAction(LoginActionKind::CopyCode))
                }
                crate::login_state::LOGIN_ACTION_CANCEL => {
                    Some(SelectionResult::LoginAction(LoginActionKind::Cancel))
                }
                _ => None,
            },
            Some(SelectionKind::KeybindingHelp) => None,
            None => None,
        }?;

        self.exit_selection_mode();
        Some(result)
    }

    pub fn has_pending_ask(&self) -> bool {
        self.ask_user.has_pending()
    }

    /// Restore the ask_user prompt UI from a ToolCall's arguments during
    /// step-back navigation.  No reply channel is set because the agent is
    /// not running; the answer will be handled by `finish_pending_ask` in
    /// step mode.
    pub(crate) fn restore_ask_user_from_step(&mut self, args: &serde_json::Value) {
        use crate::agent::types::AskUserOption;

        let question = args
            .get("question")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let allow_freeform = args
            .get("allowFreeform")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        let options: Vec<AskUserOption> = match args.get("options") {
            Some(serde_json::Value::Array(items)) => items
                .iter()
                .filter_map(|item| match item {
                    serde_json::Value::String(s) => {
                        let title = s.trim();
                        if title.is_empty() {
                            None
                        } else {
                            Some(AskUserOption {
                                title: title.to_string(),
                                description: None,
                            })
                        }
                    }
                    serde_json::Value::Object(obj) => {
                        let title = obj.get("title").and_then(serde_json::Value::as_str)?.trim();
                        if title.is_empty() {
                            return None;
                        }
                        let description = obj
                            .get("description")
                            .and_then(serde_json::Value::as_str)
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(ToOwned::to_owned);
                        Some(AskUserOption {
                            title: title.to_string(),
                            description,
                        })
                    }
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        };

        self.ask_user.pending = Some(PendingAsk {
            question,
            options: options.clone(),
            allow_freeform,
        });
        // No reply channel in step mode — finish_pending_ask will detect
        // step mode and commit the step branch instead.
        self.ask_user.reply = None;

        if options.is_empty() {
            self.ask_user.freeform_mode = true;
            self.exit_selection_mode();
            self.reset_textarea();
            return;
        }

        self.reset_textarea();
        let mut items: Vec<CompletionItem> = options
            .iter()
            .map(|opt| CompletionItem {
                label: opt.title.clone(),
                detail: opt.description.clone().unwrap_or_default(),
                complete_to: format!("/ask_user_option {}", opt.title),
                loading: false,
                error: false,
                match_range: None,
            })
            .collect();

        if allow_freeform {
            items.push(CompletionItem {
                label: "Type your response…".to_string(),
                detail: String::new(),
                complete_to: "/ask_user_freeform".to_string(),
                loading: false,
                error: false,
                match_range: None,
            });
        }

        self.selection
            .activate(SelectionKind::AskUser, "Options:", items);
    }

    /// Returns true when the current pending ask allows a free-form typed answer.
    pub fn pending_ask_allows_freeform(&self) -> bool {
        self.ask_user.allows_freeform()
    }

    /// Returns true when a pending ask is showing its selection menu and does
    /// NOT allow free-form input.
    pub fn ask_user_selection_no_freeform(&self) -> bool {
        self.selection.active
            && self.selection.kind == Some(SelectionKind::AskUser)
            && !self.pending_ask_allows_freeform()
    }

    pub fn receive_ask_request(&mut self, req: AskRequest) {
        let AskRequest {
            question,
            context: _context,
            options,
            allow_multiple: _allow_multiple,
            allow_freeform,
            reply,
        } = req;

        self.ask_user.pending = Some(PendingAsk {
            question,
            options: options.clone(),
            allow_freeform,
        });
        self.ask_user.reply = Some(reply);

        // Don't push an [ask_user] assistant message into the projected
        // display log — the agent's ToolCall message already represents this
        // in the conversation history and UI. Adding an extra assistant
        // message here would corrupt the tool_use / tool_result pairing
        // expected by the Anthropic API.

        if options.is_empty() {
            // No options: go straight to freeform input so the user can type
            // their answer.  The question is visible in the log tool call.
            self.ask_user.freeform_mode = true;
            self.exit_selection_mode();
            self.reset_textarea();
            return;
        }

        self.reset_textarea();
        let mut items: Vec<CompletionItem> = options
            .iter()
            .map(|opt| CompletionItem {
                label: opt.title.clone(),
                detail: opt.description.clone().unwrap_or_default(),
                complete_to: format!("/ask_user_option {}", opt.title),
                loading: false,
                error: false,
                match_range: None,
            })
            .collect();

        if allow_freeform {
            items.push(CompletionItem {
                label: "Type your response…".to_string(),
                detail: String::new(),
                complete_to: "/ask_user_freeform".to_string(),
                loading: false,
                error: false,
                match_range: None,
            });
        }

        self.selection
            .activate(SelectionKind::AskUser, "Options:", items);
    }

    pub fn enter_ask_freeform_mode(&mut self) {
        self.exit_selection_mode();
        self.reset_textarea();
        if self.pending_ask_allows_freeform() && !self.ask_user.freeform_mode {
            self.ask_user.freeform_mode = true;
        }
    }

    /// While the ask_user selection menu is still visible, mark the freeform
    /// sentinel as selected and activate the brown input field so the user can
    /// see what they are typing.  The selection menu stays open so they can
    /// still navigate back to a predefined option.
    pub fn begin_ask_freeform_typing(&mut self) {
        if !self.pending_ask_allows_freeform() {
            return;
        }
        // Select the freeform sentinel item in the list.
        if let Some(idx) = self
            .selection
            .items
            .iter()
            .position(|item| item.complete_to == "/ask_user_freeform")
        {
            self.selection.selected = idx;
            self.ensure_selection_visible();
        }
        // Activate the brown input field.
        if !self.ask_user.freeform_mode {
            self.ask_user.freeform_mode = true;
        }
    }

    /// Clear freeform typing state without dismissing the selection menu.
    /// Called when the user navigates away from the freeform sentinel.
    pub fn cancel_ask_freeform_typing(&mut self) {
        self.ask_user.freeform_mode = false;
        self.reset_textarea();
    }

    pub fn submit_pending_ask_answer(&mut self) {
        let Some(pending) = self.ask_user.pending.as_ref() else {
            return;
        };

        if !pending.allow_freeform && !pending.options.is_empty() {
            return;
        }

        let text = self.textarea.lines().join("\n").trim().to_string();
        if text.is_empty() {
            return;
        }

        // Don't push the answer as a plain user message — the agent's
        // ToolResult message will represent it in history and UI.
        self.finish_pending_ask(AskUserResponse::Answer(text));
    }

    pub fn select_pending_ask_option(&mut self, answer: String) {
        if self.ask_user.pending.is_none() {
            return;
        }
        // Don't push the answer as a plain user message — the agent's
        // ToolResult message will represent it in history and UI.
        self.finish_pending_ask(AskUserResponse::Answer(answer));
    }

    pub fn cancel_pending_ask(&mut self) {
        if self.ask_user.pending.is_none() {
            return;
        }
        self.finish_pending_ask(AskUserResponse::Cancelled);
        self.abort_agent_loop();
    }

    fn finish_pending_ask(&mut self, answer: AskUserResponse) {
        // Step mode with ask_user: commit the step branch with the answer
        // instead of sending it back to the agent (which isn't running).
        if self.ask_user.reply.is_none() && self.is_stepping() {
            match answer {
                AskUserResponse::Answer(text) => {
                    // Populate textarea with the answer so commit_step_branch
                    // can read it.
                    self.textarea = TextArea::new(vec![text.clone()]);
                    self.textarea.move_cursor(CursorMove::End);
                    // Commit the step branch (creates new session from events
                    // up to the ask_user ToolResult, excluding it).
                    self.commit_step_branch();
                    // Append the answer as a UserMessage to the branch session.
                    self.append_user_message(text);
                    self.reset_textarea();
                    // Set pending_finalize so the main loop launches the turn
                    // on the next iteration.
                    self.runtime.pending_finalize = true;
                }
                AskUserResponse::Cancelled => {
                    self.cancel_stepping();
                }
            }
            self.ask_user.pending = None;
            self.ask_user.freeform_mode = false;
            self.exit_selection_mode();
            return;
        }

        if let Some(reply) = self.ask_user.reply.take() {
            let _ = reply.send(answer);
        }
        self.ask_user.pending = None;
        self.ask_user.freeform_mode = false;
        self.exit_selection_mode();
        self.reset_textarea();
    }

    // ── Login panel actions ───────────────────────────────────────────────────

    // ── Login panel actions ───────────────────────────────────────────────────

    /// Open the action selection menu for the active login panel.
    pub fn enter_login_action_menu(&mut self) {
        self.session.live_turn.notices.clear();
        self.login.enter_login_action_menu(&mut self.selection);
    }

    /// Execute a login action chosen from the action menu.
    pub fn apply_login_action(&mut self, action: LoginActionKind) {
        self.login.apply_login_action(action, &mut self.selection);
    }

    pub fn start_login(&mut self, provider: &str) {
        let tx = self.app_event_tx();
        self.login.start_login(provider, tx);
    }

    pub fn cancel_login(&mut self) {
        self.login.cancel_login();
    }

    pub fn apply_login_event(&mut self, ev: LoginEvent) {
        let App {
            login,
            session,
            selection,
            log_view,
            ..
        } = self;
        login.apply_login_event(ev, session, selection, &mut log_view.log_cache);
    }

    // ── Conversation management ───────────────────────────────────────────────

    pub(crate) fn refresh_resume_availability(&mut self) {
        self.session.refresh_resume_availability();
    }

    /// Return the current session ID, creating a new session if one does not
    /// yet exist.  Falls back to a random ephemeral ID if persistence is
    /// unavailable.
    pub(crate) fn ensure_session_id(&mut self) -> String {
        self.session.ensure_session_id()
    }

    /// Ensure a [`SessionState`] exists for the current session before submitting
    /// a user message. Creates the session and loads (or initialises) the state
    /// if needed. No-op when session state is already populated.
    ///
    /// When persistent session storage is unavailable, falls back to an
    /// ephemeral event log in the system temp directory so the refactored
    /// ownership model still holds: committed conversation state always enters
    /// through `SessionEvent` ingestion and `SessionState` is always present
    /// before a turn launches.
    pub(crate) fn ensure_event_log_for_submit(&mut self) {
        self.session.ensure_event_log_for_submit();
    }

    pub(crate) fn persist_messages(&mut self) {
        // Persistence is now driven incrementally by `flush_turn_events` and
        // `append_event_immediate` via the event log.  This method is kept as
        // a call-site placeholder so that callers do not need to be updated
        // individually; its only remaining job is to refresh the resume hint.
        self.session.refresh_persistence();
    }

    /// Append a user-visible user message to the active session.
    ///
    /// When event-log persistence is available, writes the durable event and
    /// lets the display projection update from that source of truth. When
    /// persistence is unavailable, falls back to a transient visible message.
    pub(crate) fn append_user_message(&mut self, content: String) {
        // A new user message begins a fresh turn; clear any stale cache-miss
        // warning from the previous turn so the next response is evaluated
        // on its own merit.
        self.cache_miss_warning = false;
        self.session.append_user_message(content, Self::now_ts());
    }

    /// Export the current visible session to a standalone HTML file.
    pub fn export_session_html(&mut self, requested_path: Option<&str>) {
        let path = export::resolve_export_path(&self.session.current_cwd, requested_path);
        // Use the committed session state projection when available.
        let display_messages;
        let messages_ref: &[Message] = if let Some(ss) = &self.session.session_state {
            display_messages = ss.projected_display_messages();
            &display_messages
        } else {
            &[]
        };
        let html = export::build_session_export_html(
            messages_ref,
            &self.session.current_cwd,
            &self.provider.current_instance.id,
            &self.provider.current_model,
            self.session.current_session_id.as_deref(),
        );

        match export::write_export_file(&path, &html) {
            Ok(()) => {
                self.session
                    .live_turn
                    .notices
                    .push(Message::assistant(format!(
                        "[session exported to {}]",
                        path.display()
                    )));
            }
            Err(e) => {
                self.session
                    .live_turn
                    .notices
                    .push(Message::assistant(format!("[export failed: {e}]")));
            }
        }
        self.persist_messages();
    }

    /// Clear the conversation history and reset the input area.
    pub fn clear_session_state(&mut self) {
        self.session.current_session_id = None;
        self.session.session_state = None;
        self.session.live_turn.clear_all();
        self.session.pending_turn_events.clear();
        self.runtime.queued_steering.clear();
        self.runtime.steering_tx = None;
        self.agent_turn.set_status(None);
        self.latest_usage = None;
        self.reset_textarea();
        self.log_view.auto_scroll = true;
        self.refresh_resume_availability();
    }
}

// ── Selection navigation helpers ──────────────────────────────────────────────

/// Advance `selected` by one index (wrapping), skipping loading items.
/// Returns the new index, or the original if all items are loading.
fn advance_selection(items: &[CompletionItem], current: usize, forward: bool) -> usize {
    let len = items.len();
    if len <= 1 {
        return current;
    }
    let start = current;
    let mut idx = current;
    loop {
        idx = if forward {
            (idx + 1) % len
        } else {
            (idx + len - 1) % len
        };
        if !items[idx].loading || idx == start {
            return idx;
        }
    }
}

/// Find the nearest non-loading item starting from `idx`.
///
/// When `prefer_forward` is true, scans forward from `idx` to the end, then
/// backward from `idx - 1` to the start. When false, scans backward first,
/// then forward.
fn nearest_non_loading(items: &[CompletionItem], idx: usize, prefer_forward: bool) -> usize {
    if items.is_empty() {
        return 0;
    }
    if prefer_forward {
        for (i, item) in items.iter().enumerate().skip(idx) {
            if !item.loading {
                return i;
            }
        }
        for (i, item) in items.iter().enumerate().take(idx).rev() {
            if !item.loading {
                return i;
            }
        }
    } else {
        for (i, item) in items.iter().enumerate().take(idx + 1).rev() {
            if !item.loading {
                return i;
            }
        }
        for (i, item) in items.iter().enumerate().skip(idx + 1) {
            if !item.loading {
                return i;
            }
        }
    }
    idx
}

#[cfg(test)]
mod selection_nav_tests {
    use super::*;
    use crate::completion::CompletionItem;

    fn item(label: &str, loading: bool) -> CompletionItem {
        CompletionItem {
            label: label.to_string(),
            detail: String::new(),
            complete_to: if loading {
                String::new()
            } else {
                label.to_string()
            },
            loading,
            error: false,
            match_range: None,
        }
    }

    #[test]
    fn advance_forward_skips_loading() {
        let items = vec![
            item("a", false),
            item("header", true),
            item("b", false),
            item("header2", true),
            item("c", false),
        ];
        // from 0 (a) -> skip header -> 2 (b)
        assert_eq!(advance_selection(&items, 0, true), 2);
        // from 2 (b) -> skip header2 -> 4 (c)
        assert_eq!(advance_selection(&items, 2, true), 4);
    }

    #[test]
    fn advance_backward_skips_loading() {
        let items = vec![item("a", false), item("header", true), item("b", false)];
        // from 2 (b) -> skip header -> 0 (a)
        assert_eq!(advance_selection(&items, 2, false), 0);
    }

    #[test]
    fn advance_wraps_around() {
        let items = vec![item("a", false), item("header", true), item("b", false)];
        // from 2 (b) forward -> wraps to 0 (a)
        assert_eq!(advance_selection(&items, 2, true), 0);
        // from 0 (a) backward -> wraps to 2 (b), skipping header
        assert_eq!(advance_selection(&items, 0, false), 2);
    }

    #[test]
    fn advance_all_loading_returns_original() {
        let items = vec![item("x", true), item("y", true)];
        assert_eq!(advance_selection(&items, 0, true), 0);
        assert_eq!(advance_selection(&items, 1, false), 1);
    }

    #[test]
    fn advance_empty_returns_zero() {
        let items: Vec<CompletionItem> = vec![];
        assert_eq!(advance_selection(&items, 0, true), 0);
    }

    #[test]
    fn advance_single_item_returns_same() {
        let items = vec![item("only", false)];
        assert_eq!(advance_selection(&items, 0, true), 0);
    }

    #[test]
    fn nearest_non_loading_prefers_forward() {
        let items = vec![
            item("a", false),
            item("h1", true),
            item("h2", true),
            item("b", false),
            item("c", false),
        ];
        // from 1 (h1) -> forward to 3 (b)
        assert_eq!(nearest_non_loading(&items, 1, true), 3);
    }

    #[test]
    fn nearest_non_loading_falls_back_forward() {
        let items = vec![item("a", false), item("b", false), item("h1", true)];
        // from 2 (h1) -> forward finds nothing -> backward to 1 (b)
        assert_eq!(nearest_non_loading(&items, 2, true), 1);
    }

    #[test]
    fn nearest_non_loading_prefers_backward() {
        let items = vec![
            item("a", false),
            item("b", false),
            item("h1", true),
            item("h2", true),
            item("c", false),
        ];
        // from 3 (h2) -> backward to 1 (b)
        assert_eq!(nearest_non_loading(&items, 3, false), 1);
    }

    #[test]
    fn nearest_non_loading_falls_forward_backward() {
        let items = vec![item("h1", true), item("a", false), item("b", false)];
        // from 0 (h1) -> backward finds nothing -> forward to 1 (a)
        assert_eq!(nearest_non_loading(&items, 0, false), 1);
    }

    #[test]
    fn nearest_non_loading_all_loading_returns_idx() {
        let items = vec![item("x", true), item("y", true)];
        assert_eq!(nearest_non_loading(&items, 0, true), 0);
        assert_eq!(nearest_non_loading(&items, 1, false), 1);
    }

    #[test]
    fn nearest_non_loading_empty_returns_zero() {
        let items: Vec<CompletionItem> = vec![];
        assert_eq!(nearest_non_loading(&items, 0, true), 0);
    }
}
