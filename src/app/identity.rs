pub const APP_SLUG: &str = "fozmo";
pub const APP_DISPLAY_NAME: &str = "Fozmo";
pub const ENV_PREFIX: &str = "FOZMO";
pub const LEGACY_ENV_PREFIX: &str = "TRANSIENT";
pub const AUTH_HEADER: &str = "x-fozmo-token";
pub const PLAYBACK_CLIENT_HEADER: &str = "x-fozmo-playback-client";
pub const PLAYBACK_SEQUENCE_HEADER: &str = "x-fozmo-playback-seq";
/// Request-scoped listening profile. The server validates this value and
/// carries it through profile-sensitive application operations.
pub const PROFILE_HEADER: &str = "x-fozmo-profile-id";
/// Carries the browser-zone agent id so zone routes can match a request to
/// the browser-private zone it owns.
pub const BROWSER_ZONE_HEADER: &str = "x-fozmo-browser-zone";
/// Per-launch secret accepted only by loopback maintenance endpoints owned by
/// the native supervisor. It is never a browser/session credential.
pub const LAUNCHER_CONTROL_HEADER: &str = "x-fozmo-launcher-token";
pub const LOCAL_STORAGE_PREFIX: &str = "fozmo";
pub const DATA_DIR_NAME: &str = "Fozmo";
pub const SCHEMA_BASE_URL: &str = "https://fozmo.local/schemas";
pub const SCHEMA_ENDPOINTS_EXTENSION: &str = "x-fozmo-endpoints";
pub const USER_AGENT: &str = concat!("Fozmo/", env!("CARGO_PKG_VERSION"));
pub const MDNS_CORE_SERVICE_TYPE: &str = "_fozmo._tcp.local.";
pub const MDNS_HTTP_SERVICE_TYPE: &str = "_http._tcp.local.";

pub fn env_key(suffix: &str) -> String {
    format!("{ENV_PREFIX}_{suffix}")
}

pub fn legacy_env_key(suffix: &str) -> String {
    format!("{LEGACY_ENV_PREFIX}_{suffix}")
}
