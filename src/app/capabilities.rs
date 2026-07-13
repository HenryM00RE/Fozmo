use schemars::JsonSchema;
use serde::Serialize;

#[derive(Debug, Clone, Copy, Serialize, JsonSchema)]
pub struct BuildCapabilities {
    pub local_library: bool,
    pub qobuz: bool,
    pub pcm_output: bool,
    pub airplay2: bool,
    pub asio: bool,
    pub apple_music_capture: bool,
    pub sonos: bool,
    pub hegel: bool,
    pub upnp: bool,
    pub experimental_dsd256: bool,
}

impl BuildCapabilities {
    pub fn current() -> Self {
        Self {
            local_library: cfg!(feature = "local_library"),
            qobuz: cfg!(feature = "qobuz"),
            pcm_output: cfg!(feature = "pcm_output"),
            // Direct AirPlay is provided by a separately licensed helper at
            // runtime; this flag means this server build supports its IPC.
            airplay2: cfg!(feature = "airplay_helper"),
            asio: cfg!(feature = "asio"),
            apple_music_capture: cfg!(feature = "apple_music_capture"),
            sonos: cfg!(feature = "sonos"),
            hegel: cfg!(feature = "hegel"),
            upnp: cfg!(feature = "upnp"),
            experimental_dsd256: cfg!(feature = "experimental_dsd256"),
        }
    }
}
