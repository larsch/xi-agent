//! Runtime feature switches. Configuration and CLI store overrides, not defaults.

use std::collections::BTreeMap;

use clap::{Args, ValueEnum};

use crate::session_event::SessionEvent;

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    serde::Deserialize,
    serde::Serialize,
    ValueEnum,
)]
#[serde(rename_all = "kebab-case")]
pub enum Feature {
    /// Worktree session control endpoint and agent_session tool.
    SessionIpc,
    /// Best-effort publisher of hook events.
    HookIpc,
    /// Desktop notifications while the terminal is unfocused.
    DesktopNotifications,
    /// Automatic context compaction (manual /compact remains available).
    AutoCompaction,
}

impl Feature {
    pub fn default_enabled(self) -> bool {
        matches!(self, Self::DesktopNotifications | Self::AutoCompaction)
    }

    pub fn session_scoped(self) -> bool {
        matches!(self, Self::AutoCompaction)
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::SessionIpc => "session-ipc",
            Self::HookIpc => "hook-ipc",
            Self::DesktopNotifications => "desktop-notifications",
            Self::AutoCompaction => "auto-compaction",
        }
    }
}

pub type FeatureOverrides = BTreeMap<Feature, bool>;

#[derive(Debug, Default, Args)]
pub struct FeatureArgs {
    /// Enable a feature for this invocation (repeatable; last override wins).
    #[arg(long, value_enum, value_name = "FEATURE")]
    enable: Vec<Feature>,
    /// Disable a feature for this invocation (repeatable; last override wins).
    #[arg(long, value_enum, value_name = "FEATURE")]
    disable: Vec<Feature>,
    /// Compatibility spelling for --enable=session-ipc.
    #[arg(long, hide = true, action = clap::ArgAction::Count)]
    enable_session_ipc: u8,
}

impl FeatureArgs {
    /// Merge both switches by their original argv positions, not by flag name.
    pub fn overrides(&self, matches: &clap::ArgMatches) -> FeatureOverrides {
        let mut ordered = Vec::new();
        for (id, values, enabled) in [
            ("enable", &self.enable, true),
            ("disable", &self.disable, false),
        ] {
            if let Some(indices) = matches.indices_of(id) {
                ordered.extend(
                    indices
                        .zip(values)
                        .map(|(index, feature)| (index, *feature, enabled)),
                );
            }
        }
        if self.enable_session_ipc > 0 {
            ordered.push((
                matches
                    .index_of("enable_session_ipc")
                    .expect("present flag has an index"),
                Feature::SessionIpc,
                true,
            ));
        }
        ordered.sort_by_key(|(index, _, _)| *index);
        ordered
            .into_iter()
            .map(|(_, feature, enabled)| (feature, enabled))
            .collect()
    }
}

/// Invocation baseline plus the active conversation's restorable settings.
#[derive(Debug, Default, Clone)]
pub struct FeatureState {
    config: FeatureOverrides,
    cli: FeatureOverrides,
    session: BTreeMap<String, bool>,
}

impl FeatureState {
    pub fn new(config: FeatureOverrides, cli: FeatureOverrides) -> Self {
        Self {
            config,
            cli,
            session: BTreeMap::new(),
        }
    }

    pub fn enabled(&self, feature: Feature) -> bool {
        self.cli
            .get(&feature)
            .copied()
            .or_else(|| {
                feature
                    .session_scoped()
                    .then(|| self.session.get(feature.name()).copied())
                    .flatten()
            })
            .or_else(|| self.config.get(&feature).copied())
            .unwrap_or_else(|| feature.default_enabled())
    }

    /// Headless mode deliberately has no IPC services or desktop notifications.
    pub fn validate_print_mode(&self) -> std::io::Result<()> {
        for feature in [
            Feature::SessionIpc,
            Feature::HookIpc,
            Feature::DesktopNotifications,
        ] {
            if self.cli.get(&feature) == Some(&true) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!(
                        "feature '{}' is unavailable in --print mode",
                        feature.name()
                    ),
                ));
            }
        }
        Ok(())
    }

    pub fn restore(&mut self, events: &[SessionEvent]) {
        self.session = session_values(events);
    }

    pub fn clear_session(&mut self) {
        self.session.clear();
    }

    /// Only conversation behavior is persisted. Never restore process authority.
    pub fn session_snapshot(&self) -> BTreeMap<String, bool> {
        Feature::value_variants()
            .iter()
            .copied()
            .filter(|feature| feature.session_scoped())
            .map(|feature| (feature.name().to_string(), self.enabled(feature)))
            .collect()
    }

    #[cfg(all(feature = "restart", unix))]
    pub fn restart_args(&self) -> Vec<String> {
        self.cli
            .iter()
            .map(|(feature, enabled)| {
                format!(
                    "--{}={}",
                    if *enabled { "enable" } else { "disable" },
                    feature.name()
                )
            })
            .collect()
    }
}

pub fn session_values(events: &[SessionEvent]) -> BTreeMap<String, bool> {
    let mut values = BTreeMap::new();
    for event in events {
        if let SessionEvent::FeaturesChanged {
            values: changes, ..
        } = event
        {
            values.extend(
                changes
                    .iter()
                    .map(|(name, enabled)| (name.clone(), *enabled)),
            );
        }
    }
    values
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn print_mode_rejects_explicit_unavailable_enablement_only() {
        let config = FeatureOverrides::from([(Feature::SessionIpc, true)]);
        assert!(
            FeatureState::new(config.clone(), FeatureOverrides::new())
                .validate_print_mode()
                .is_ok()
        );
        for feature in [
            Feature::SessionIpc,
            Feature::HookIpc,
            Feature::DesktopNotifications,
        ] {
            let features =
                FeatureState::new(config.clone(), FeatureOverrides::from([(feature, true)]));
            assert!(
                features
                    .validate_print_mode()
                    .unwrap_err()
                    .to_string()
                    .contains(feature.name())
            );
            assert!(
                FeatureState::new(config.clone(), FeatureOverrides::from([(feature, false)]))
                    .validate_print_mode()
                    .is_ok()
            );
        }
        assert!(
            FeatureState::new(
                config,
                FeatureOverrides::from([(Feature::AutoCompaction, true)])
            )
            .validate_print_mode()
            .is_ok()
        );
    }

    #[cfg(all(feature = "restart", unix))]
    #[test]
    fn restart_forwards_cli_overrides_not_config_or_session_authority() {
        let features = FeatureState::new(
            FeatureOverrides::from([(Feature::HookIpc, true)]),
            FeatureOverrides::from([
                (Feature::SessionIpc, true),
                (Feature::AutoCompaction, false),
            ]),
        );
        assert_eq!(
            features.restart_args(),
            ["--enable=session-ipc", "--disable=auto-compaction"]
        );
    }

    #[test]
    fn defaults_and_explicit_false() {
        let mut features = FeatureState::default();
        assert!(!features.enabled(Feature::SessionIpc));
        assert!(!features.enabled(Feature::HookIpc));
        assert!(features.enabled(Feature::DesktopNotifications));
        assert!(features.enabled(Feature::AutoCompaction));
        features.config.insert(Feature::SessionIpc, true);
        features.cli.insert(Feature::SessionIpc, false);
        assert!(!features.enabled(Feature::SessionIpc));
    }

    #[test]
    fn resume_only_restores_session_features_and_cli_wins() {
        let events = vec![SessionEvent::FeaturesChanged {
            values: BTreeMap::from([
                ("auto-compaction".into(), false),
                ("session-ipc".into(), true),
                ("future-feature".into(), true),
            ]),
            timestamp: 1,
        }];
        let mut features = FeatureState::default();
        features.restore(&events);
        assert!(!features.enabled(Feature::AutoCompaction));
        assert!(!features.enabled(Feature::SessionIpc));
        assert_eq!(
            features.session_snapshot(),
            BTreeMap::from([("auto-compaction".into(), false)])
        );
        features.cli.insert(Feature::AutoCompaction, true);
        assert!(features.enabled(Feature::AutoCompaction));
        features.cli.clear();
        features.restore(&[]);
        assert!(features.enabled(Feature::AutoCompaction));
        features.restore(&events);
        features.clear_session();
        assert!(features.enabled(Feature::AutoCompaction));
    }

    #[test]
    fn session_changes_round_trip_and_last_value_wins() {
        let events = [false, true].map(|enabled| SessionEvent::FeaturesChanged {
            values: BTreeMap::from([("auto-compaction".into(), enabled)]),
            timestamp: 1,
        });
        let raw = serde_json::to_string(&events).unwrap();
        let decoded: Vec<SessionEvent> = serde_json::from_str(&raw).unwrap();
        assert!(session_values(&decoded)["auto-compaction"]);
        assert!(crate::projection::project_llm_messages(&decoded).is_empty());
        assert!(crate::projection::project_display_messages(&decoded).is_empty());
    }
}
