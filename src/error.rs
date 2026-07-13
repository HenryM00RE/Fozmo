use std::fmt;

/// Stable failure categories shared by lower-layer domain errors.
///
/// Messages remain diagnostic and may change. Callers that need control-flow
/// decisions should match on this category instead of parsing error strings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorCategory {
    Validation,
    Unavailable,
    Authentication,
    NotFound,
    Conflict,
    RetryableNetwork,
    Persistence,
    InternalInvariant,
}

impl ErrorCategory {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Validation => "validation",
            Self::Unavailable => "unavailable",
            Self::Authentication => "authentication",
            Self::NotFound => "not_found",
            Self::Conflict => "conflict",
            Self::RetryableNetwork => "retryable_network",
            Self::Persistence => "persistence",
            Self::InternalInvariant => "internal_invariant",
        }
    }
}

impl fmt::Display for ErrorCategory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// General-purpose structured error for boundaries that do not need a more
/// specific domain enum yet.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DomainError {
    category: ErrorCategory,
    message: String,
}

impl DomainError {
    pub fn new(category: ErrorCategory, message: impl Into<String>) -> Self {
        Self {
            category,
            message: message.into(),
        }
    }

    pub const fn category(&self) -> ErrorCategory {
        self.category
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn persistence(message: impl Into<String>) -> Self {
        Self::new(ErrorCategory::Persistence, message)
    }

    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(ErrorCategory::Unavailable, message)
    }

    pub fn retryable_network(message: impl Into<String>) -> Self {
        Self::new(ErrorCategory::RetryableNetwork, message)
    }

    pub fn internal_invariant(message: impl Into<String>) -> Self {
        Self::new(ErrorCategory::InternalInvariant, message)
    }
}

impl fmt::Display for DomainError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for DomainError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn categories_are_stable_and_messages_remain_separate() {
        let error = DomainError::new(ErrorCategory::Persistence, "database is locked");
        assert_eq!(error.category(), ErrorCategory::Persistence);
        assert_eq!(error.category().as_str(), "persistence");
        assert_eq!(error.message(), "database is locked");
    }
}
