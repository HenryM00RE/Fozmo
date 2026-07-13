use crate::app::config::ConfigError;
use crate::error::{DomainError, ErrorCategory};
use std::error::Error;
use std::fmt;
use std::io;
use std::net::SocketAddr;

#[derive(Debug)]
pub enum AppError {
    Config(ConfigError),
    Io {
        context: &'static str,
        source: io::Error,
    },
    Library(DomainError),
    Persistence(DomainError),
    Qobuz(DomainError),
    LastFm(DomainError),
    Sonos(io::Error),
    ServerBind {
        addr: SocketAddr,
        source: io::Error,
    },
    Server(io::Error),
    Agent(DomainError),
}

impl AppError {
    pub fn io(context: &'static str, source: io::Error) -> Self {
        Self::Io { context, source }
    }

    pub(crate) fn persistence(message: impl Into<String>) -> Self {
        Self::Persistence(DomainError::persistence(message))
    }

    pub(crate) fn library(message: impl Into<String>) -> Self {
        Self::Library(DomainError::persistence(message))
    }

    pub(crate) fn qobuz(message: impl Into<String>) -> Self {
        Self::Qobuz(DomainError::unavailable(message))
    }

    pub(crate) fn lastfm(message: impl Into<String>) -> Self {
        Self::LastFm(DomainError::unavailable(message))
    }

    pub(crate) fn agent(message: impl Into<String>) -> Self {
        Self::Agent(DomainError::unavailable(message))
    }

    pub fn category(&self) -> ErrorCategory {
        match self {
            Self::Config(_) => ErrorCategory::Validation,
            Self::Io { .. } => ErrorCategory::Persistence,
            Self::Library(error)
            | Self::Persistence(error)
            | Self::Qobuz(error)
            | Self::LastFm(error)
            | Self::Agent(error) => error.category(),
            Self::Sonos(_) => ErrorCategory::Unavailable,
            Self::ServerBind { .. } | Self::Server(_) => ErrorCategory::Unavailable,
        }
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(source) => write!(f, "{source}"),
            Self::Io { context, source } => write!(f, "{context}: {source}"),
            Self::Library(message) => write!(f, "library startup failed: {message}"),
            Self::Persistence(message) => write!(f, "persistence operation failed: {message}"),
            Self::Qobuz(message) => write!(f, "Qobuz startup failed: {message}"),
            Self::LastFm(message) => write!(f, "Last.fm startup failed: {message}"),
            Self::Sonos(source) => write!(f, "Sonos startup failed: {source}"),
            Self::ServerBind { addr, source } => {
                write!(f, "failed to bind web server at {addr}: {source}")
            }
            Self::Server(source) => write!(f, "web server failed: {source}"),
            Self::Agent(message) => write!(f, "agent runtime failed: {message}"),
        }
    }
}

impl Error for AppError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Config(source) => Some(source),
            Self::Io { source, .. }
            | Self::Sonos(source)
            | Self::ServerBind { source, .. }
            | Self::Server(source) => Some(source),
            Self::Library(source)
            | Self::Persistence(source)
            | Self::Qobuz(source)
            | Self::LastFm(source)
            | Self::Agent(source) => Some(source),
        }
    }
}

impl From<ConfigError> for AppError {
    fn from(source: ConfigError) -> Self {
        Self::Config(source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_bind_error_includes_address() {
        let error = AppError::ServerBind {
            addr: SocketAddr::from(([127, 0, 0, 1], 3000)),
            source: io::Error::new(io::ErrorKind::AddrInUse, "busy"),
        };

        assert_eq!(
            error.to_string(),
            "failed to bind web server at 127.0.0.1:3000: busy"
        );
    }

    #[test]
    fn subsystem_failures_keep_categories_and_sources() {
        let library = AppError::library("database unavailable");
        assert_eq!(library.category(), ErrorCategory::Persistence);
        assert!(library.source().is_some());

        let qobuz = AppError::qobuz("upstream unavailable");
        assert_eq!(qobuz.category(), ErrorCategory::Unavailable);
        assert!(qobuz.source().is_some());
    }
}
