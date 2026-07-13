use std::sync::atomic::{AtomicU64, Ordering};
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

static NEXT_OPERATION_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LogFormat {
    Compact,
    Json,
}

impl LogFormat {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "compact" | "human" | "text" => Some(Self::Compact),
            "json" => Some(Self::Json),
            _ => None,
        }
    }
}

pub(crate) fn init_logging(format: LogFormat) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let registry = tracing_subscriber::registry().with(filter);
    let result = match format {
        LogFormat::Compact => registry.with(fmt::layer().compact()).try_init(),
        LogFormat::Json => registry
            .with(fmt::layer().json().flatten_event(true))
            .try_init(),
    };
    let _ = result;
}

pub(crate) fn next_operation_id() -> u64 {
    NEXT_OPERATION_ID.fetch_add(1, Ordering::Relaxed)
}

pub(crate) fn error_kind(error: &str) -> &'static str {
    let lower = error.to_ascii_lowercase();
    if lower.contains("token")
        || lower.contains("auth")
        || lower.contains("login")
        || lower.contains("log in")
        || lower.contains("unauthorized")
    {
        "auth"
    } else if lower.contains("timeout") || lower.contains("timed out") {
        "timeout"
    } else if lower.contains("http 401") || lower.contains("http 403") {
        "forbidden"
    } else if lower.contains("http ") {
        "http"
    } else if lower.contains("network")
        || lower.contains("dns")
        || lower.contains("connect")
        || lower.contains("connection")
    {
        "network"
    } else if lower.contains("not found") || lower.contains("missing") {
        "not_found"
    } else {
        "error"
    }
}

pub(crate) fn sanitize_error(error: &str) -> String {
    let mut out = Vec::new();
    for word in error.split_whitespace() {
        let trimmed = word.trim_matches(|c: char| {
            matches!(
                c,
                '"' | '\'' | ',' | ';' | '(' | ')' | '[' | ']' | '{' | '}'
            )
        });
        let redacted = if let Some(redacted) = redact_sensitive_pair(trimmed) {
            word.replace(trimmed, &redacted)
        } else if looks_sensitive(trimmed) {
            word.replace(trimmed, "[redacted]")
        } else {
            word.to_string()
        };
        out.push(redacted);
    }
    out.join(" ")
}

fn redact_sensitive_pair(value: &str) -> Option<String> {
    let (separator_index, separator) = value
        .find('=')
        .map(|index| (index, '='))
        .or_else(|| value.find(':').map(|index| (index, ':')))?;
    let key = value[..separator_index]
        .trim_start_matches(['?', '&'])
        .trim_matches(|ch| matches!(ch, '"' | '\'' | '{' | '}' | '[' | ']'))
        .to_ascii_lowercase();
    let sensitive = matches!(
        key.as_str(),
        "email"
            | "password"
            | "pass"
            | "token"
            | "access_token"
            | "refresh_token"
            | "user_auth_token"
            | "private_key"
            | "secret"
            | "app_secret"
            | "code"
            | "code_autorisation"
            | "request_sig"
            | "signature"
    );
    sensitive.then(|| format!("{}{}[redacted]", &value[..separator_index], separator))
}

fn looks_sensitive(value: &str) -> bool {
    if value.starts_with("http://") || value.starts_with("https://") {
        return true;
    }
    if value.contains('/') || value.contains('\\') {
        return true;
    }
    if let Some((local, domain)) = value.split_once('@')
        && !local.is_empty()
        && domain.contains('.')
    {
        return true;
    }
    if value.len() >= 32
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '=' | ':'))
    {
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_format_parses_human_and_json_values() {
        assert_eq!(LogFormat::parse("json"), Some(LogFormat::Json));
        assert_eq!(LogFormat::parse("human"), Some(LogFormat::Compact));
        assert_eq!(LogFormat::parse("text"), Some(LogFormat::Compact));
        assert_eq!(LogFormat::parse("unknown"), None);
    }

    #[test]
    fn sanitizer_redacts_urls_tokens_and_paths() {
        let sanitized = sanitize_error(
            "failed https://stream.example.test/file?token=abc /Users/fixture/music.flac abcdefghijklmnopqrstuvwxyzABCDEF user@example.test password=hunter2 code:oauth-code",
        );
        assert!(!sanitized.contains("https://stream.example.test"));
        assert!(!sanitized.contains("/Users/fixture"));
        assert!(!sanitized.contains("abcdefghijklmnopqrstuvwxyzABCDEF"));
        assert!(!sanitized.contains("user@example.test"));
        assert!(!sanitized.contains("hunter2"));
        assert!(!sanitized.contains("oauth-code"));
        assert!(sanitized.contains("password=[redacted]"));
        assert!(sanitized.contains("[redacted]"));
    }

    #[test]
    fn sanitizer_redacts_json_shaped_credential_fields() {
        let sanitized = sanitize_error(
            r#"upstream returned {"email":"private@example.test"} {"token":"secret-value"} {"refresh_token":"refresh-value"}"#,
        );

        assert!(!sanitized.contains("private@example.test"));
        assert!(!sanitized.contains("secret-value"));
        assert!(!sanitized.contains("refresh-value"));
        assert!(sanitized.contains("[redacted]"));
    }

    #[test]
    fn error_kind_classifies_common_failures() {
        assert_eq!(error_kind("Qobuz proxy returned HTTP 500"), "http");
        assert_eq!(error_kind("Log in to Qobuz before playback"), "auth");
        assert_eq!(error_kind("request timed out"), "timeout");
        assert_eq!(error_kind("Track not found"), "not_found");
    }
}
