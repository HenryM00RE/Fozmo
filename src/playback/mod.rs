pub mod airplay_volume;
#[cfg(all(target_os = "macos", feature = "apple_music_musickit"))]
#[allow(dead_code)]
pub mod apple_island;
#[cfg(all(target_os = "macos", feature = "apple_music_musickit"))]
pub(crate) mod apple_music_native;
#[cfg(all(target_os = "macos", feature = "apple_music_musickit"))]
pub(crate) mod apple_music_relay;
pub mod apply_settings;
pub mod artist_radio;
pub mod auto_advance;
// The identity, boundary, and coordinator modules are the substrate the mixed
// Apple/local/Qobuz transition work is built on. They ship before the engine
// hands them authority, so parts of each are unreferenced until that switch.
#[allow(dead_code)]
pub mod boundary;
pub mod commands;
pub mod config;
pub mod config_applicator;
pub mod control;
#[allow(dead_code)]
pub mod coordinator;
pub mod error;
#[cfg_attr(not(feature = "hegel"), allow(dead_code))]
pub mod hegel_control;
#[allow(dead_code)]
pub mod identity;
pub mod intent;
pub mod lastfm;
pub mod local;
pub mod monitor;
pub mod now_playing;
pub mod output_devices;
#[cfg_attr(not(feature = "qobuz"), allow(dead_code))]
pub mod qobuz;
pub mod queue;
pub mod resolver;
pub mod router;
pub mod sequencer;
pub mod service;
#[cfg_attr(not(feature = "sonos"), allow(dead_code))]
pub mod sonos;
pub mod source;
#[cfg_attr(
    not(any(feature = "hegel", feature = "sonos", feature = "upnp")),
    allow(dead_code)
)]
pub mod status;
#[cfg(test)]
pub(crate) mod test_support;
pub mod transfer;
#[cfg_attr(not(feature = "upnp"), allow(dead_code))]
pub mod upnp;
#[cfg_attr(not(feature = "upnp"), allow(dead_code))]
pub(crate) mod upnp_dsp;
#[cfg_attr(not(feature = "hegel"), allow(dead_code))]
pub mod zone_service;
