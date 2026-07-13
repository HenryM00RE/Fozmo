mod active_zone_policy;
mod agent_bridge;
mod capabilities;
#[cfg_attr(not(any(feature = "hegel", feature = "sonos")), allow(dead_code))]
mod manager;
mod model;
mod pairing;
mod persistence;
mod registry;
mod snapshot;

pub use capabilities::current_playback_config;
pub use manager::ZoneManager;
pub(crate) use pairing::constant_time_token_matches;
pub use pairing::{
    CONTROL_SESSION_COOKIE, DEFAULT_PAIRING_TOKEN_TTL_SECS, PairingManager, REMOTE_SESSION_COOKIE,
    RemoteSessionMetadata, SCOPE_AGENT_CONNECT, SCOPE_CONTROL, SCOPE_REMOTE, SCOPE_SESSION_CREATE,
    SCOPE_STREAM_READ,
};

pub const LOCAL_ZONE_ID: &str = "local-core";

pub fn local_device_zone_id(name: &str) -> String {
    model::local_device_zone_id(name)
}
