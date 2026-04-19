#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingLevel {
    Off,
    Minimal,
    Low,
    Medium,
    High,
    XHigh,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeminiThinkingLevel {
    Minimal,
    Low,
    Medium,
    High,
}

impl ThinkingLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.to_ascii_lowercase().as_str() {
            "off" => Some(Self::Off),
            "minimal" => Some(Self::Minimal),
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            "xhigh" => Some(Self::XHigh),
            _ => None,
        }
    }

    pub fn all() -> &'static [ThinkingLevel] {
        &[
            Self::Off,
            Self::Minimal,
            Self::Low,
            Self::Medium,
            Self::High,
            Self::XHigh,
        ]
    }

    pub fn to_reasoning_effort(self) -> Option<&'static str> {
        match self {
            Self::Off => None,
            Self::Minimal => Some("minimal"),
            Self::Low => Some("low"),
            Self::Medium => Some("medium"),
            Self::High => Some("high"),
            Self::XHigh => Some("xhigh"),
        }
    }

    pub fn to_reasoning_effort_string(self) -> Option<String> {
        self.to_reasoning_effort().map(str::to_string)
    }

    pub fn to_gemini_thinking_level(self) -> Option<GeminiThinkingLevel> {
        match self {
            Self::Off => None,
            Self::Minimal => Some(GeminiThinkingLevel::Minimal),
            Self::Low => Some(GeminiThinkingLevel::Low),
            Self::Medium => Some(GeminiThinkingLevel::Medium),
            Self::High | Self::XHigh => Some(GeminiThinkingLevel::High),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{GeminiThinkingLevel, ThinkingLevel};

    #[test]
    fn parse_round_trip_known_levels() {
        for level in ThinkingLevel::all() {
            let parsed = ThinkingLevel::parse(level.as_str());
            assert_eq!(parsed, Some(*level));
        }
    }

    #[test]
    fn reasoning_effort_matches_expected_strings() {
        assert_eq!(ThinkingLevel::Off.to_reasoning_effort_string(), None);
        assert_eq!(
            ThinkingLevel::Minimal.to_reasoning_effort_string(),
            Some("minimal".to_string())
        );
        assert_eq!(
            ThinkingLevel::Low.to_reasoning_effort_string(),
            Some("low".to_string())
        );
        assert_eq!(
            ThinkingLevel::Medium.to_reasoning_effort_string(),
            Some("medium".to_string())
        );
        assert_eq!(
            ThinkingLevel::High.to_reasoning_effort_string(),
            Some("high".to_string())
        );
        assert_eq!(
            ThinkingLevel::XHigh.to_reasoning_effort_string(),
            Some("xhigh".to_string())
        );
    }

    #[test]
    fn gemini_mapping_clamps_xhigh_to_high() {
        assert_eq!(ThinkingLevel::Off.to_gemini_thinking_level(), None);
        assert_eq!(
            ThinkingLevel::Minimal.to_gemini_thinking_level(),
            Some(GeminiThinkingLevel::Minimal)
        );
        assert_eq!(
            ThinkingLevel::Low.to_gemini_thinking_level(),
            Some(GeminiThinkingLevel::Low)
        );
        assert_eq!(
            ThinkingLevel::Medium.to_gemini_thinking_level(),
            Some(GeminiThinkingLevel::Medium)
        );
        assert_eq!(
            ThinkingLevel::High.to_gemini_thinking_level(),
            Some(GeminiThinkingLevel::High)
        );
        assert_eq!(
            ThinkingLevel::XHigh.to_gemini_thinking_level(),
            Some(GeminiThinkingLevel::High)
        );
    }
}
