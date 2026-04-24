//! Provider state management.
//!
//! `ProviderManager` groups the fields that track which provider/model/thinking
//! configuration is currently active, the snapshot of configured instances used
//! for completions and selection menus, and the transient setup-flow state.
//!
//! The setup-flow *methods* remain on `App` because they need access to the
//! textarea and selection widgets.  `ProviderManager` is a pure data holder.

use crate::app::{PendingProviderRemoval, PendingProviderSetup, ProviderSetupStep};
use crate::provider_instance::ProviderInstance;
use crate::thinking::ThinkingLevel;

/// All provider-related state owned by the application.
pub(crate) struct ProviderManager {
    /// Snapshot of configured provider instances (for completions / selection).
    /// Updated whenever the provider list changes.
    pub instances: Vec<ProviderInstance>,

    /// The currently active provider instance.
    pub current_instance: ProviderInstance,

    /// Currently active model name.
    pub current_model: String,

    /// Currently active thinking / reasoning level.
    pub current_thinking: ThinkingLevel,

    /// Whether the current provider+model combination supports thinking.
    pub thinking_supported: bool,

    /// Which step of the provider setup input flow is currently active.
    pub setup_step: ProviderSetupStep,

    /// Pending provider instance being configured through the add-provider flow.
    pub pending_setup: Option<PendingProviderSetup>,

    /// Pending custom provider instance being confirmed for removal.
    pub pending_removal: Option<PendingProviderRemoval>,
}

impl ProviderManager {
    pub(crate) fn new(
        initial_instance: ProviderInstance,
        initial_model: String,
        initial_thinking: ThinkingLevel,
    ) -> Self {
        Self {
            instances: Vec::new(),
            current_instance: initial_instance,
            current_model: initial_model,
            current_thinking: initial_thinking,
            thinking_supported: false,
            setup_step: ProviderSetupStep::Idle,
            pending_setup: None,
            pending_removal: None,
        }
    }
}
