//! Typed provider error representation for LLM operations.

use std::fmt;

/// Category of provider error for programmatic handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderErrorKind {
    /// HTTP 401: authentication required or token expired/invalid.
    Unauthorized,
    /// HTTP 403: authenticated but not permitted to access the resource.
    Forbidden,
    /// HTTP 429: rate limit exceeded.
    RateLimited,
    /// HTTP 5xx: server-side error.
    ServerError,
    /// Network/connection failure before receiving an HTTP response.
    Network,
    /// Other error not covered by the above categories.
    Other,
}

/// A structured error returned by an LLM provider operation.
#[derive(Debug, Clone)]
pub struct ProviderError {
    pub kind: ProviderErrorKind,
    pub status_code: Option<u16>,
    pub provider: String,
    pub message: String,
}

impl ProviderError {
    /// Construct an `Unauthorized` error.
    pub fn unauthorized(provider: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            kind: ProviderErrorKind::Unauthorized,
            status_code: Some(401),
            provider: provider.into(),
            message: message.into(),
        }
    }

    /// Construct a `Forbidden` error.
    pub fn forbidden(provider: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            kind: ProviderErrorKind::Forbidden,
            status_code: Some(403),
            provider: provider.into(),
            message: message.into(),
        }
    }

    /// Construct a `RateLimited` error.
    pub fn rate_limited(provider: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            kind: ProviderErrorKind::RateLimited,
            status_code: Some(429),
            provider: provider.into(),
            message: message.into(),
        }
    }

    /// Construct a `ServerError` error.
    pub fn server_error(
        provider: impl Into<String>,
        status_code: u16,
        message: impl Into<String>,
    ) -> Self {
        Self {
            kind: ProviderErrorKind::ServerError,
            status_code: Some(status_code),
            provider: provider.into(),
            message: message.into(),
        }
    }

    /// Construct a `Network` error.
    pub fn network(provider: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            kind: ProviderErrorKind::Network,
            status_code: None,
            provider: provider.into(),
            message: message.into(),
        }
    }

    /// Construct an `Other` error.
    pub fn other(provider: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            kind: ProviderErrorKind::Other,
            status_code: None,
            provider: provider.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(status) = self.status_code {
            write!(f, "{} returned {}: {}", self.provider, status, self.message)
        } else {
            write!(f, "{}: {}", self.provider, self.message)
        }
    }
}

impl std::error::Error for ProviderError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unauthorized_constructor() {
        let err = ProviderError::unauthorized("copilot", "token expired");
        assert_eq!(err.kind, ProviderErrorKind::Unauthorized);
        assert_eq!(err.status_code, Some(401));
        assert_eq!(err.provider, "copilot");
        assert_eq!(err.message, "token expired");
    }

    #[test]
    fn forbidden_constructor() {
        let err = ProviderError::forbidden("openai", "quota exceeded");
        assert_eq!(err.kind, ProviderErrorKind::Forbidden);
        assert_eq!(err.status_code, Some(403));
    }

    #[test]
    fn rate_limited_constructor() {
        let err = ProviderError::rate_limited("anthropic", "too many requests");
        assert_eq!(err.kind, ProviderErrorKind::RateLimited);
        assert_eq!(err.status_code, Some(429));
    }

    #[test]
    fn server_error_constructor() {
        let err = ProviderError::server_error("gemini", 503, "service unavailable");
        assert_eq!(err.kind, ProviderErrorKind::ServerError);
        assert_eq!(err.status_code, Some(503));
    }

    #[test]
    fn network_constructor() {
        let err = ProviderError::network("ollama", "connection refused");
        assert_eq!(err.kind, ProviderErrorKind::Network);
        assert!(err.status_code.is_none());
    }

    #[test]
    fn other_constructor() {
        let err = ProviderError::other("custom", "parse error");
        assert_eq!(err.kind, ProviderErrorKind::Other);
        assert!(err.status_code.is_none());
    }

    #[test]
    fn display_with_status_code() {
        let err = ProviderError::unauthorized("copilot", "invalid token");
        assert_eq!(format!("{err}"), "copilot returned 401: invalid token");
    }

    #[test]
    fn display_without_status_code() {
        let err = ProviderError::network("openai", "connection timeout");
        assert_eq!(format!("{err}"), "openai: connection timeout");
    }
}
