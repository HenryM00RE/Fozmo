pub mod airplay_volume;
pub mod apply_settings;
pub mod artist_radio;
pub mod auto_advance;
pub mod commands;
pub mod config;
pub mod config_applicator;
pub mod control;
pub mod error;
#[cfg_attr(not(feature = "hegel"), allow(dead_code))]
pub mod hegel_control;
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
