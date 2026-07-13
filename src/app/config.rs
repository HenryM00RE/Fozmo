use crate::app::identity;
use crate::diagnostics::logging::LogFormat;
use crate::services::discovery;
use crate::zones::DEFAULT_PAIRING_TOKEN_TTL_SECS;
use reqwest::Url;
use std::error::Error;
use std::fmt;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AppMode {
    Core,
    Agent,
}

#[derive(Debug, Eq, PartialEq)]
pub enum ConfigError {
    InvalidEnv {
        key: String,
        value: String,
        expected: &'static str,
    },
    InvalidArg {
        flag: &'static str,
        value: String,
        expected: &'static str,
    },
    MissingArgValue {
        flag: &'static str,
        expected: &'static str,
    },
    IncompatibleArg {
        flag: &'static str,
        requirement: &'static str,
    },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidEnv {
                key,
                value,
                expected,
            } => write!(f, "invalid {key}='{value}'; expected {expected}"),
            Self::InvalidArg {
                flag,
                value,
                expected,
            } => write!(f, "invalid {flag}='{value}'; expected {expected}"),
            Self::MissingArgValue { flag, expected } => {
                write!(f, "missing value for {flag}; expected {expected}")
            }
            Self::IncompatibleArg { flag, requirement } => {
                write!(f, "{flag} requires {requirement}")
            }
        }
    }
}

impl Error for ConfigError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppConfig {
    pub mode: AppMode,
    pub log_format: LogFormat,
    pub lan_enabled: bool,
    pub pairing_required: bool,
    pub pairing_token_ttl_secs: u64,
    pub allow_query_token_auth: bool,
    pub startup_scan_enabled: bool,
    /// Exit cleanly when the supervising launcher's stdin pipe closes.
    pub exit_on_stdin_eof: bool,
    /// Core Bonjour advertising can be delegated to the native launcher.
    pub core_mdns_enabled: bool,
    /// Private, fail-closed mode used only by the packaged release smoke test.
    pub(crate) release_smoke: bool,
    pub port: u16,
    pub public_base_url: String,
}

impl AppConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        let args: Vec<String> = std::env::args().collect();
        Self::from_env_and_args(|key| std::env::var(key).ok(), args)
    }

    fn from_env_and_args(
        env: impl Fn(&str) -> Option<String>,
        args: impl IntoIterator<Item = String>,
    ) -> Result<Self, ConfigError> {
        let args: Vec<String> = args.into_iter().collect();
        // Deliberately CLI-only: an inherited environment variable must never
        // make a normal launch discard persistent Keychain-backed secrets.
        let release_smoke = args.iter().skip(1).any(|arg| arg == "--release-smoke");
        let log_format = log_format_from_env_or_arg(&env, &args)?;
        let mode = mode_from_env_or_args(&env, &args)?;
        let lan_enabled =
            bool_from_env_or_flags(&env, &args, "LAN", "--lan", "--local-only", false)?;
        let pairing_requested = bool_from_env_or_flags(
            &env,
            &args,
            "REQUIRE_PAIRING",
            "--require-pairing",
            "--no-require-pairing",
            false,
        )?;
        // Local and LAN requests share the trusted-network listener and do
        // not require authentication by default. Remote Access is a separate
        // TLS listener with mandatory remote-session authentication.
        let pairing_required = pairing_requested;
        let pairing_token_ttl_secs = pairing_token_ttl_from_env_or_arg(&env, &args)?;
        let allow_query_token_auth = bool_from_env_or_flag(
            &env,
            &args,
            "ALLOW_QUERY_TOKEN_AUTH",
            "--allow-query-token-auth",
        )?;
        let startup_scan_enabled =
            bool_from_env_or_flag(&env, &args, "SCAN_ON_STARTUP", "--scan-on-startup")?;
        let exit_on_stdin_eof =
            bool_from_env_or_flag(&env, &args, "EXIT_ON_STDIN_EOF", "--exit-on-stdin-eof")?;
        let core_mdns_enabled = bool_from_env_or_flags(
            &env,
            &args,
            "CORE_MDNS",
            "--core-mdns",
            "--no-core-mdns",
            true,
        )?;
        let port = port_from_env_or_arg(&env, &args)?;
        let public_base_url = public_base_url_from_env(&env, port, lan_enabled)?;

        let config = Self {
            mode,
            log_format,
            lan_enabled,
            pairing_required,
            pairing_token_ttl_secs,
            allow_query_token_auth,
            startup_scan_enabled,
            exit_on_stdin_eof,
            core_mdns_enabled,
            release_smoke,
            port,
            public_base_url,
        };
        config.validate_release_smoke()?;
        Ok(config)
    }

    fn validate_release_smoke(&self) -> Result<(), ConfigError> {
        if !self.release_smoke {
            return Ok(());
        }

        let requirement = if self.mode != AppMode::Core {
            Some("core mode")
        } else if self.lan_enabled {
            Some("local-only networking")
        } else if self.pairing_required {
            Some("pairing to be disabled")
        } else if self.allow_query_token_auth {
            Some("query-token authentication to be disabled")
        } else if self.startup_scan_enabled {
            Some("startup scanning to be disabled")
        } else if self.core_mdns_enabled {
            Some("core mDNS advertising to be disabled")
        } else if self.exit_on_stdin_eof {
            Some("stdin EOF shutdown to be disabled")
        } else if self.public_base_url != discovery::default_public_base_url(self.port, false) {
            Some("the default loopback public URL")
        } else {
            None
        };

        match requirement {
            Some(requirement) => Err(ConfigError::IncompatibleArg {
                flag: "--release-smoke",
                requirement,
            }),
            None => Ok(()),
        }
    }
}

const EXPECTED_BOOL: &str = "one of 1, true, yes, on, 0, false, no, off";
const EXPECTED_LOG_FORMAT: &str = "compact, human, text, or json";
const EXPECTED_MODE: &str = "'core' or 'agent'";
const EXPECTED_PORT: &str = "1-65535";
const EXPECTED_POSITIVE_INTEGER: &str = "a positive integer";
const EXPECTED_PUBLIC_BASE_URL: &str = "an absolute http(s) URL";

fn log_format_from_env_or_arg(
    env: &impl Fn(&str) -> Option<String>,
    args: &[String],
) -> Result<LogFormat, ConfigError> {
    if let Some((key, value)) = env_value(env, "LOG_FORMAT") {
        return LogFormat::parse(&value).ok_or(ConfigError::InvalidEnv {
            key,
            value,
            expected: EXPECTED_LOG_FORMAT,
        });
    }

    arg_value(args, "--log-format", EXPECTED_LOG_FORMAT)?
        .map(|value| {
            LogFormat::parse(&value).ok_or(ConfigError::InvalidArg {
                flag: "--log-format",
                value,
                expected: EXPECTED_LOG_FORMAT,
            })
        })
        .transpose()
        .map(|value| value.unwrap_or(LogFormat::Compact))
}

fn mode_from_env_or_args(
    env: &impl Fn(&str) -> Option<String>,
    args: &[String],
) -> Result<AppMode, ConfigError> {
    if let Some((key, value)) = env_value(env, "MODE") {
        return match value.trim().to_ascii_lowercase().as_str() {
            "core" => Ok(AppMode::Core),
            "agent" => Ok(AppMode::Agent),
            _ => Err(ConfigError::InvalidEnv {
                key,
                value,
                expected: EXPECTED_MODE,
            }),
        };
    }

    if args.iter().any(|arg| arg == "--agent") {
        Ok(AppMode::Agent)
    } else {
        Ok(AppMode::Core)
    }
}

fn pairing_token_ttl_from_env_or_arg(
    env: &impl Fn(&str) -> Option<String>,
    args: &[String],
) -> Result<u64, ConfigError> {
    if let Some((key, value)) = env_value(env, "PAIRING_TOKEN_TTL_SECS") {
        return parse_positive_u64(&value).ok_or(ConfigError::InvalidEnv {
            key,
            value,
            expected: EXPECTED_POSITIVE_INTEGER,
        });
    }

    arg_value(args, "--pairing-token-ttl-secs", EXPECTED_POSITIVE_INTEGER)?
        .map(|value| {
            parse_positive_u64(&value).ok_or(ConfigError::InvalidArg {
                flag: "--pairing-token-ttl-secs",
                value,
                expected: EXPECTED_POSITIVE_INTEGER,
            })
        })
        .transpose()
        .map(|value| value.unwrap_or(DEFAULT_PAIRING_TOKEN_TTL_SECS))
}

fn port_from_env_or_arg(
    env: &impl Fn(&str) -> Option<String>,
    args: &[String],
) -> Result<u16, ConfigError> {
    if let Some((key, value)) = env_value(env, "PORT") {
        return parse_port(&value).ok_or(ConfigError::InvalidEnv {
            key,
            value,
            expected: EXPECTED_PORT,
        });
    }

    arg_value(args, "--port", EXPECTED_PORT)?
        .map(|value| {
            parse_port(&value).ok_or(ConfigError::InvalidArg {
                flag: "--port",
                value,
                expected: EXPECTED_PORT,
            })
        })
        .transpose()
        .map(|value| value.unwrap_or(3001))
}

fn public_base_url_from_env(
    env: &impl Fn(&str) -> Option<String>,
    port: u16,
    lan_enabled: bool,
) -> Result<String, ConfigError> {
    if let Some((key, value)) = env_value(env, "PUBLIC_BASE_URL") {
        if is_valid_public_base_url(&value) {
            return Ok(value);
        }
        return Err(ConfigError::InvalidEnv {
            key,
            value,
            expected: EXPECTED_PUBLIC_BASE_URL,
        });
    }

    Ok(discovery::default_public_base_url(port, lan_enabled))
}

/// Read the current environment name first, then the pre-rename name. This
/// keeps upgraded deployments secure while making an explicitly configured
/// `FOZMO_*` value authoritative when both are present.
fn env_value(env: &impl Fn(&str) -> Option<String>, suffix: &str) -> Option<(String, String)> {
    let key = identity::env_key(suffix);
    if let Some(value) = env(&key) {
        return Some((key, value));
    }

    let legacy_key = identity::legacy_env_key(suffix);
    env(&legacy_key).map(|value| {
        eprintln!("warning: {legacy_key} is deprecated; use {key}");
        (legacy_key, value)
    })
}

fn bool_from_env_or_flag(
    env: &impl Fn(&str) -> Option<String>,
    args: &[String],
    env_suffix: &str,
    flag: &str,
) -> Result<bool, ConfigError> {
    if let Some((key, value)) = env_value(env, env_suffix) {
        return parse_bool(&value).ok_or(ConfigError::InvalidEnv {
            key,
            value,
            expected: EXPECTED_BOOL,
        });
    }

    Ok(args.iter().any(|arg| arg == flag))
}

fn bool_from_env_or_flags(
    env: &impl Fn(&str) -> Option<String>,
    args: &[String],
    env_suffix: &str,
    enable_flag: &str,
    disable_flag: &str,
    default: bool,
) -> Result<bool, ConfigError> {
    if let Some((key, value)) = env_value(env, env_suffix) {
        return parse_bool(&value).ok_or(ConfigError::InvalidEnv {
            key,
            value,
            expected: EXPECTED_BOOL,
        });
    }
    if args.iter().any(|arg| arg == disable_flag) {
        return Ok(false);
    }
    if args.iter().any(|arg| arg == enable_flag) {
        return Ok(true);
    }
    Ok(default)
}

fn arg_value(
    args: &[String],
    flag: &'static str,
    expected: &'static str,
) -> Result<Option<String>, ConfigError> {
    let prefix = format!("{flag}=");
    for (index, arg) in args.iter().enumerate().skip(1) {
        if arg == flag {
            if let Some(value) = args.get(index + 1).filter(|value| !value.starts_with("--")) {
                return Ok(Some(value.clone()));
            }
            return Err(ConfigError::MissingArgValue { flag, expected });
        }
        if let Some(value) = arg.strip_prefix(&prefix) {
            return Ok(Some(value.to_string()));
        }
    }
    Ok(None)
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn parse_positive_u64(value: &str) -> Option<u64> {
    let value = value.trim().parse::<u64>().ok()?;
    (value > 0).then_some(value)
}

fn parse_port(value: &str) -> Option<u16> {
    let port = value.trim().parse::<u16>().ok()?;
    (port > 0).then_some(port)
}

fn is_valid_public_base_url(value: &str) -> bool {
    let Ok(url) = Url::parse(value) else {
        return false;
    };
    matches!(url.scheme(), "http" | "https") && url.host_str().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn config_result(env: &[(&str, &str)], args: &[&str]) -> Result<AppConfig, ConfigError> {
        let env: HashMap<String, String> = env
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        AppConfig::from_env_and_args(
            |key| env.get(key).cloned(),
            args.iter().map(|arg| (*arg).to_string()),
        )
    }

    fn config_from(env: &[(&str, &str)], args: &[&str]) -> AppConfig {
        config_result(env, args).expect("config should parse")
    }

    #[test]
    fn agent_mode_can_come_from_env_or_flag() {
        assert_eq!(
            config_from(&[("FOZMO_MODE", "agent")], &["fozmo"]).mode,
            AppMode::Agent
        );
        assert_eq!(config_from(&[], &["fozmo", "--agent"]).mode, AppMode::Agent);
    }

    #[test]
    fn core_mode_is_default() {
        let config = config_from(&[], &["fozmo"]);
        assert_eq!(config.mode, AppMode::Core);
        assert_eq!(config.log_format, LogFormat::Compact);
        assert_eq!(config.port, 3001);
        assert!(!config.lan_enabled);
        assert!(!config.pairing_required);
        assert!(config.core_mdns_enabled);
        assert!(!config.exit_on_stdin_eof);
    }

    #[test]
    fn env_port_overrides_arg_port() {
        assert_eq!(
            config_from(&[("FOZMO_PORT", "4000")], &["fozmo", "--port=3001"]).port,
            4000
        );
        assert_eq!(config_from(&[], &["fozmo", "--port", "3002"]).port, 3002);
    }

    #[test]
    fn flags_enable_lan_pairing_and_startup_scan() {
        let config = config_from(
            &[],
            &["fozmo", "--lan", "--require-pairing", "--scan-on-startup"],
        );

        assert!(config.lan_enabled);
        assert!(config.pairing_required);
        assert!(config.startup_scan_enabled);
    }

    #[test]
    fn pairing_token_ttl_and_query_auth_are_explicit() {
        let config = config_from(
            &[("FOZMO_PAIRING_TOKEN_TTL_SECS", "60")],
            &["fozmo", "--allow-query-token-auth"],
        );

        assert_eq!(config.pairing_token_ttl_secs, 60);
        assert!(config.allow_query_token_auth);
    }

    #[test]
    fn log_format_can_come_from_env_or_flag() {
        assert_eq!(
            config_from(&[("FOZMO_LOG_FORMAT", "json")], &["fozmo"]).log_format,
            LogFormat::Json
        );
        assert_eq!(
            config_from(&[], &["fozmo", "--log-format=json"]).log_format,
            LogFormat::Json
        );
    }

    #[test]
    fn invalid_env_values_report_clear_errors() {
        let err = config_result(&[("FOZMO_PORT", "abc")], &["fozmo"]).unwrap_err();
        assert_eq!(
            err.to_string(),
            "invalid FOZMO_PORT='abc'; expected 1-65535"
        );

        let err = config_result(&[("FOZMO_MODE", "server")], &["fozmo"]).unwrap_err();
        assert_eq!(
            err.to_string(),
            "invalid FOZMO_MODE='server'; expected 'core' or 'agent'"
        );

        let err = config_result(&[("FOZMO_LOG_FORMAT", "bogus")], &["fozmo"]).unwrap_err();
        assert_eq!(
            err.to_string(),
            "invalid FOZMO_LOG_FORMAT='bogus'; expected compact, human, text, or json"
        );
    }

    #[test]
    fn invalid_arg_values_report_clear_errors() {
        let err = config_result(&[], &["fozmo", "--port=0"]).unwrap_err();
        assert_eq!(err.to_string(), "invalid --port='0'; expected 1-65535");

        let err = config_result(&[], &["fozmo", "--port", "abc"]).unwrap_err();
        assert_eq!(err.to_string(), "invalid --port='abc'; expected 1-65535");

        let err = config_result(&[], &["fozmo", "--log-format=xml"]).unwrap_err();
        assert_eq!(
            err.to_string(),
            "invalid --log-format='xml'; expected compact, human, text, or json"
        );

        let err = config_result(&[], &["fozmo", "--pairing-token-ttl-secs"]).unwrap_err();
        assert_eq!(
            err.to_string(),
            "missing value for --pairing-token-ttl-secs; expected a positive integer"
        );
    }

    #[test]
    fn boolean_env_values_are_strict() {
        let config = config_from(&[("FOZMO_LAN", "off")], &["fozmo", "--lan"]);
        assert!(!config.lan_enabled);

        let err = config_result(&[("FOZMO_REQUIRE_PAIRING", "maybe")], &["fozmo"]).unwrap_err();
        assert_eq!(
            err.to_string(),
            "invalid FOZMO_REQUIRE_PAIRING='maybe'; expected one of 1, true, yes, on, 0, false, no, off"
        );
    }

    #[test]
    fn legacy_transient_env_vars_are_still_honored() {
        let config = config_from(
            &[("TRANSIENT_LAN", "1"), ("TRANSIENT_REQUIRE_PAIRING", "1")],
            &["fozmo"],
        );

        assert!(config.lan_enabled);
        assert!(config.pairing_required);
    }

    #[test]
    fn fozmo_env_vars_override_legacy_transient_env_vars() {
        let config = config_from(
            &[
                ("FOZMO_LAN", "0"),
                ("TRANSIENT_REQUIRE_PAIRING", "1"),
                ("FOZMO_REQUIRE_PAIRING", "0"),
            ],
            &["fozmo"],
        );

        assert!(!config.pairing_required);
    }

    #[test]
    fn lan_mode_respects_disabled_pairing_configuration() {
        let config = config_from(
            &[("FOZMO_LAN", "1"), ("FOZMO_REQUIRE_PAIRING", "0")],
            &["fozmo"],
        );
        assert!(!config.pairing_required);
    }

    #[test]
    fn explicit_local_only_and_supervisor_flags_are_supported() {
        let config = config_from(
            &[],
            &[
                "fozmo",
                "--local-only",
                "--no-require-pairing",
                "--exit-on-stdin-eof",
                "--no-core-mdns",
            ],
        );
        assert!(!config.lan_enabled);
        assert!(!config.pairing_required);
        assert!(config.exit_on_stdin_eof);
        assert!(!config.core_mdns_enabled);
    }

    #[test]
    fn release_smoke_accepts_only_the_isolated_effective_configuration() {
        let config = config_from(
            &[
                ("FOZMO_MODE", "core"),
                ("FOZMO_LAN", "0"),
                ("FOZMO_REQUIRE_PAIRING", "0"),
                ("FOZMO_ALLOW_QUERY_TOKEN_AUTH", "0"),
                ("FOZMO_SCAN_ON_STARTUP", "0"),
                ("FOZMO_CORE_MDNS", "0"),
                ("FOZMO_PORT", "4100"),
                ("FOZMO_PUBLIC_BASE_URL", "http://127.0.0.1:4100"),
            ],
            &["fozmo", "--release-smoke"],
        );

        assert!(config.release_smoke);
        assert_eq!(config.mode, AppMode::Core);
        assert!(!config.lan_enabled);
        assert!(!config.pairing_required);
        assert!(!config.allow_query_token_auth);
        assert!(!config.startup_scan_enabled);
        assert!(!config.core_mdns_enabled);
    }

    #[test]
    fn release_smoke_cannot_be_activated_from_the_environment() {
        let config = config_from(&[("FOZMO_RELEASE_SMOKE", "1")], &["fozmo"]);
        assert!(!config.release_smoke);
    }

    #[test]
    fn release_smoke_rejects_inherited_unsafe_configuration() {
        let safe_args = ["fozmo", "--release-smoke", "--no-core-mdns"];
        let cases = [
            (("FOZMO_MODE", "agent"), "core mode"),
            (("FOZMO_LAN", "1"), "local-only networking"),
            (("FOZMO_REQUIRE_PAIRING", "1"), "pairing to be disabled"),
            (
                ("FOZMO_ALLOW_QUERY_TOKEN_AUTH", "1"),
                "query-token authentication to be disabled",
            ),
            (
                ("FOZMO_SCAN_ON_STARTUP", "1"),
                "startup scanning to be disabled",
            ),
            (
                ("FOZMO_CORE_MDNS", "1"),
                "core mDNS advertising to be disabled",
            ),
            (
                ("FOZMO_EXIT_ON_STDIN_EOF", "1"),
                "stdin EOF shutdown to be disabled",
            ),
            (
                ("FOZMO_PUBLIC_BASE_URL", "https://example.test"),
                "the default loopback public URL",
            ),
        ];

        for ((key, value), requirement) in cases {
            let error = config_result(&[(key, value)], &safe_args)
                .expect_err("unsafe inherited configuration must fail closed");
            assert_eq!(
                error,
                ConfigError::IncompatibleArg {
                    flag: "--release-smoke",
                    requirement,
                }
            );
        }
    }

    #[test]
    fn invalid_legacy_env_values_fail_with_the_legacy_key() {
        let err = config_result(&[("TRANSIENT_REQUIRE_PAIRING", "maybe")], &["fozmo"])
            .expect_err("invalid legacy security settings must not silently default");

        assert_eq!(
            err.to_string(),
            "invalid TRANSIENT_REQUIRE_PAIRING='maybe'; expected one of 1, true, yes, on, 0, false, no, off"
        );
    }

    #[test]
    fn legacy_fallback_applies_to_non_boolean_boot_config() {
        let config = config_from(
            &[
                ("TRANSIENT_PORT", "4100"),
                ("TRANSIENT_LOG_FORMAT", "json"),
                ("TRANSIENT_PAIRING_TOKEN_TTL_SECS", "90"),
            ],
            &["fozmo"],
        );

        assert_eq!(config.port, 4100);
        assert_eq!(config.log_format, LogFormat::Json);
        assert_eq!(config.pairing_token_ttl_secs, 90);
    }

    #[test]
    fn public_base_url_env_must_be_absolute_http_url() {
        let config = config_from(
            &[("FOZMO_PUBLIC_BASE_URL", "https://core.example.test:3001")],
            &["fozmo"],
        );
        assert_eq!(config.public_base_url, "https://core.example.test:3001");

        let err = config_result(
            &[("FOZMO_PUBLIC_BASE_URL", "core.example.test")],
            &["fozmo"],
        )
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "invalid FOZMO_PUBLIC_BASE_URL='core.example.test'; expected an absolute http(s) URL"
        );
    }
}
