use crate::error::{DomainError, ErrorCategory};

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) enum ResolveError {
    TrackNotFound,
    FileNotFound(String),
    InvalidFileName,
    Library(DomainError),
}

impl ResolveError {
    pub(crate) fn library(message: impl Into<String>) -> Self {
        Self::Library(DomainError::persistence(message))
    }

    pub(crate) fn message(&self) -> &str {
        match self {
            Self::TrackNotFound => "Track not found",
            Self::FileNotFound(message) => message,
            Self::InvalidFileName => "Invalid file name",
            Self::Library(error) => error.message(),
        }
    }
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.message())
    }
}

impl std::error::Error for ResolveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Library(error) => Some(error),
            Self::TrackNotFound | Self::FileNotFound(_) | Self::InvalidFileName => None,
        }
    }
}

impl From<DomainError> for ResolveError {
    fn from(error: DomainError) -> Self {
        Self::Library(error)
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) enum PlaybackError {
    Conflict(String),
    ZoneNotAvailable,
    BadRequest(String),
    Forbidden(String),
    NotFound(String),
    Library(DomainError),
    Integration(DomainError),
    RetryableNetwork(DomainError),
    Persistence(DomainError),
    InternalInvariant(DomainError),
}

impl PlaybackError {
    pub(crate) fn conflict(message: impl Into<String>) -> Self {
        Self::Conflict(message.into())
    }

    pub(crate) fn bad_request(message: impl Into<String>) -> Self {
        Self::BadRequest(message.into())
    }

    pub(crate) fn forbidden(message: impl Into<String>) -> Self {
        Self::Forbidden(message.into())
    }

    pub(crate) fn not_found(message: impl Into<String>) -> Self {
        Self::NotFound(message.into())
    }

    pub(crate) fn library(message: impl Into<String>) -> Self {
        Self::Library(DomainError::persistence(message))
    }

    pub(crate) fn integration(message: impl Into<String>) -> Self {
        Self::Integration(DomainError::unavailable(message))
    }

    pub(crate) fn retryable_network(message: impl Into<String>) -> Self {
        Self::RetryableNetwork(DomainError::retryable_network(message))
    }

    pub(crate) fn persistence(message: impl Into<String>) -> Self {
        Self::Persistence(DomainError::persistence(message))
    }

    pub(crate) fn internal_invariant(message: impl Into<String>) -> Self {
        Self::InternalInvariant(DomainError::internal_invariant(message))
    }

    pub(crate) fn message(&self) -> &str {
        match self {
            Self::Conflict(message)
            | Self::BadRequest(message)
            | Self::Forbidden(message)
            | Self::NotFound(message) => message,
            Self::Library(error)
            | Self::Integration(error)
            | Self::RetryableNetwork(error)
            | Self::Persistence(error)
            | Self::InternalInvariant(error) => error.message(),
            Self::ZoneNotAvailable => "Zone not available",
        }
    }

    pub(crate) fn kind(&self) -> &'static str {
        self.category().as_str()
    }

    pub(crate) const fn category(&self) -> ErrorCategory {
        match self {
            Self::BadRequest(_) => ErrorCategory::Validation,
            Self::ZoneNotAvailable | Self::Integration(_) => ErrorCategory::Unavailable,
            Self::Forbidden(_) => ErrorCategory::Authentication,
            Self::NotFound(_) => ErrorCategory::NotFound,
            Self::Conflict(_) => ErrorCategory::Conflict,
            Self::RetryableNetwork(_) => ErrorCategory::RetryableNetwork,
            Self::Library(_) | Self::Persistence(_) => ErrorCategory::Persistence,
            Self::InternalInvariant(_) => ErrorCategory::InternalInvariant,
        }
    }
}

impl std::fmt::Display for PlaybackError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.message())
    }
}

impl std::error::Error for PlaybackError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Library(error)
            | Self::Integration(error)
            | Self::RetryableNetwork(error)
            | Self::Persistence(error)
            | Self::InternalInvariant(error) => Some(error),
            Self::Conflict(_)
            | Self::ZoneNotAvailable
            | Self::BadRequest(_)
            | Self::Forbidden(_)
            | Self::NotFound(_) => None,
        }
    }
}

impl From<DomainError> for PlaybackError {
    fn from(error: DomainError) -> Self {
        match error.category() {
            ErrorCategory::Validation => Self::bad_request(error.to_string()),
            ErrorCategory::Unavailable => Self::Integration(error),
            ErrorCategory::Authentication => Self::forbidden(error.to_string()),
            ErrorCategory::NotFound => Self::not_found(error.to_string()),
            ErrorCategory::Conflict => Self::conflict(error.to_string()),
            ErrorCategory::RetryableNetwork => Self::RetryableNetwork(error),
            ErrorCategory::Persistence => Self::Persistence(error),
            ErrorCategory::InternalInvariant => Self::InternalInvariant(error),
        }
    }
}

impl From<ResolveError> for PlaybackError {
    fn from(error: ResolveError) -> Self {
        match error {
            ResolveError::TrackNotFound => PlaybackError::not_found("Track not found"),
            ResolveError::FileNotFound(message) => PlaybackError::not_found(message),
            ResolveError::InvalidFileName => PlaybackError::bad_request("Invalid file name"),
            ResolveError::Library(error) => PlaybackError::from(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_stable_categories_without_parsing_messages() {
        assert_eq!(
            PlaybackError::bad_request("bad rate").category(),
            ErrorCategory::Validation
        );
        assert_eq!(
            PlaybackError::retryable_network("timeout").category(),
            ErrorCategory::RetryableNetwork
        );
        assert_eq!(
            PlaybackError::persistence("database locked").category(),
            ErrorCategory::Persistence
        );
        assert_eq!(
            PlaybackError::internal_invariant("worker stopped").category(),
            ErrorCategory::InternalInvariant
        );
        assert!(std::error::Error::source(&PlaybackError::library("db failed")).is_some());
        assert!(std::error::Error::source(&ResolveError::library("db failed")).is_some());
    }
}
