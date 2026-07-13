#[cfg(feature = "apple_music_capture")]
pub(crate) mod apple_music;
pub(crate) mod discovery;
#[cfg_attr(not(feature = "hegel"), allow(dead_code))]
pub(crate) mod hegel;
pub(crate) mod lastfm;
#[cfg_attr(not(feature = "qobuz"), allow(dead_code))]
pub(crate) mod qobuz;
