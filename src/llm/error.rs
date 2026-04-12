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
    /// Low-level source identity that produced the error (transport/provider module).
    /// This is preserved for debugging and logs, not for final UI wording.
    pub source: String,
    /// Original provider/body message, preserved as-is.
    pub message: String,
}

impl ProviderError {
    fn new(
        kind: ProviderErrorKind,
        status_code: Option<u16>,
        source: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            status_code,
            source: source.into(),
            message: message.into(),
        }
    }

    /// Construct an `Unauthorized` error.
    pub fn unauthorized(source: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(ProviderErrorKind::Unauthorized, Some(401), source, message)
    }

    /// Construct a `Forbidden` error.
    pub fn forbidden(source: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(ProviderErrorKind::Forbidden, Some(403), source, message)
    }

    /// Construct a `RateLimited` error.
    pub fn rate_limited(source: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(ProviderErrorKind::RateLimited, Some(429), source, message)
    }

    /// Construct a `ServerError` error.
    pub fn server_error(
        source: impl Into<String>,
        status_code: u16,
        message: impl Into<String>,
    ) -> Self {
        Self::new(
            ProviderErrorKind::ServerError,
            Some(status_code),
            source,
            message,
        )
    }

    /// Construct a `Network` error.
    pub fn network(source: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(ProviderErrorKind::Network, None, source, message)
    }

    /// Construct an `Other` error.
    pub fn other(source: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(ProviderErrorKind::Other, None, source, message)
    }
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(status) = self.status_code {
            write!(f, "{} ({}): {}", self.source, status, self.message)
        } else {
            write!(f, "{}: {}", self.source, self.message)
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
        assert_eq!(err.source, "copilot");
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
        assert_eq!(format!("{err}"), "copilot (401): invalid token");
    }

    #[test]
    fn display_without_status_code() {
        let err = ProviderError::network("openai", "connection timeout");
        assert_eq!(format!("{err}"), "openai: connection timeout");
    }

    #[test]
    fn source_is_preserved_even_when_ui_uses_other_identity() {
        let err = ProviderError::server_error("OpenAI", 524, "error code: 524");
        assert_eq!(err.source, "OpenAI");
        assert_eq!(err.status_code, Some(524));
        assert_eq!(err.message, "error code: 524");
    }
}
