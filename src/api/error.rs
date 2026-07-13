use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};

use crate::diagnostics::logging::sanitize_error;
use crate::error::{DomainError, ErrorCategory};
use crate::playback::error::{PlaybackError, ResolveError};

pub type ApiResult<T> = Result<T, ApiError>;

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    #[track_caller]
    pub fn internal(message: impl Into<String>) -> Self {
        let detail = message.into();
        let caller = std::panic::Location::caller();
        tracing::error!(
            event = "api_internal_error",
            error_kind = "internal",
            source_file = caller.file(),
            source_line = caller.line(),
            error = %sanitize_error(&detail),
            "API request failed"
        );
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error")
    }

    #[track_caller]
    pub fn upstream(message: impl Into<String>) -> Self {
        let detail = message.into();
        let caller = std::panic::Location::caller();
        tracing::error!(
            event = "api_upstream_error",
            error_kind = "upstream",
            source_file = caller.file(),
            source_line = caller.line(),
            error = %sanitize_error(&detail),
            "Upstream API request failed"
        );
        Self::new(StatusCode::BAD_GATEWAY, "Upstream service error")
    }

    pub fn status(&self) -> StatusCode {
        self.status
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, self.message).into_response()
    }
}

impl From<String> for ApiError {
    fn from(message: String) -> Self {
        Self::internal(message)
    }
}

impl From<DomainError> for ApiError {
    fn from(error: DomainError) -> Self {
        match error.category() {
            ErrorCategory::Validation => Self::new(StatusCode::BAD_REQUEST, "Invalid request"),
            ErrorCategory::Authentication => {
                Self::new(StatusCode::UNAUTHORIZED, "Authentication required")
            }
            ErrorCategory::NotFound => Self::new(StatusCode::NOT_FOUND, "Not found"),
            ErrorCategory::Conflict => Self::new(StatusCode::CONFLICT, "Conflict"),
            ErrorCategory::Unavailable | ErrorCategory::RetryableNetwork => {
                Self::upstream(error.to_string())
            }
            ErrorCategory::Persistence | ErrorCategory::InternalInvariant => {
                Self::internal(error.to_string())
            }
        }
    }
}

impl From<ResolveError> for ApiError {
    fn from(error: ResolveError) -> Self {
        match error {
            ResolveError::TrackNotFound => Self::new(StatusCode::NOT_FOUND, "Track not found"),
            ResolveError::FileNotFound(message) => Self::new(StatusCode::NOT_FOUND, message),
            ResolveError::InvalidFileName => {
                Self::new(StatusCode::BAD_REQUEST, "Invalid file name")
            }
            ResolveError::Library(error) => Self::internal(error.to_string()),
        }
    }
}

impl From<PlaybackError> for ApiError {
    fn from(error: PlaybackError) -> Self {
        match error {
            PlaybackError::Conflict(message) => Self::new(StatusCode::CONFLICT, message),
            PlaybackError::ZoneNotAvailable => Self::new(StatusCode::NOT_FOUND, error.message()),
            PlaybackError::BadRequest(message) => Self::new(StatusCode::BAD_REQUEST, message),
            PlaybackError::Forbidden(message) => Self::new(StatusCode::FORBIDDEN, message),
            PlaybackError::NotFound(message) => Self::new(StatusCode::NOT_FOUND, message),
            PlaybackError::Library(error) => Self::internal(error.to_string()),
            PlaybackError::Integration(error) => Self::upstream(error.to_string()),
            PlaybackError::RetryableNetwork(error) => Self::upstream(error.to_string()),
            PlaybackError::Persistence(error) | PlaybackError::InternalInvariant(error) => {
                Self::internal(error.to_string())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use std::io::{self, Write};
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

    struct CapturedWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for CapturedWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturedLogs {
        type Writer = CapturedWriter;

        fn make_writer(&'a self) -> Self::Writer {
            CapturedWriter(Arc::clone(&self.0))
        }
    }

    #[tokio::test]
    async fn internal_errors_return_a_generic_body_and_log_sanitized_detail() {
        let logs = CapturedLogs::default();
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(logs.clone())
            .finish();
        let error = tracing::subscriber::with_default(subscriber, || {
            ApiError::internal(
                "database open failed at /Users/alice/private.db via https://cdn.example.test/file?token=secret user@example.test token=abcdefghijklmnopqrstuvwxyz012345",
            )
        });

        let response = error.into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&body[..], b"Internal server error");

        let logs = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
        assert!(logs.contains("database open failed"));
        assert!(logs.contains("api_internal_error"));
        assert!(!logs.contains("/Users/alice"));
        assert!(!logs.contains("https://cdn.example.test"));
        assert!(!logs.contains("user@example.test"));
        assert!(!logs.contains("abcdefghijklmnopqrstuvwxyz012345"));
    }

    #[test]
    fn expected_public_errors_keep_their_status_and_message() {
        let error = ApiError::new(StatusCode::NOT_FOUND, "Track not found");
        assert_eq!(error.status(), StatusCode::NOT_FOUND);
        assert_eq!(error.message(), "Track not found");
    }

    #[test]
    fn integration_errors_keep_bad_gateway_status_but_hide_upstream_detail() {
        let error = ApiError::from(PlaybackError::integration(
            "request https://upstream.example.test/?token=private failed",
        ));

        assert_eq!(error.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(error.message(), "Upstream service error");
    }

    #[test]
    fn domain_errors_map_by_category_without_exposing_internal_detail() {
        let unavailable = ApiError::from(DomainError::unavailable(
            "request https://upstream.example.test/?token=private failed",
        ));
        assert_eq!(unavailable.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(unavailable.message(), "Upstream service error");

        let persistence = ApiError::from(DomainError::persistence(
            "database /Users/alice/private.db is locked",
        ));
        assert_eq!(persistence.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(persistence.message(), "Internal server error");
    }
}
