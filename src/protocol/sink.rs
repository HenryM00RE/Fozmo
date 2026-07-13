use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SinkProtocol {
    LocalCoreAudio,
    RemoteAgent,
    AirPlayCoreAudio,
    AirPlayRaop,
    AirPlay2,
    SonosUpnp,
    UpnpAvRenderer,
    AsioOutput,
}

impl SinkProtocol {
    pub fn supports_dsp(&self) -> bool {
        !matches!(
            self,
            Self::AirPlayCoreAudio | Self::AirPlayRaop | Self::AirPlay2 | Self::SonosUpnp
        )
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum UpnpPcmContainer {
    Flac,
    Wav,
}

impl UpnpPcmContainer {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Flac => "flac",
            Self::Wav => "wav",
        }
    }

    pub fn mime_type(self) -> &'static str {
        match self {
            Self::Flac => "audio/flac",
            Self::Wav => "audio/wav",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct UpnpPcmContainerCapability {
    pub container: UpnpPcmContainer,
    pub max_sample_rate: u32,
    pub max_bit_depth: u8,
}

pub fn system_audio_backend() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "coreaudio"
    }
    #[cfg(target_os = "windows")]
    {
        "wasapi"
    }
    #[cfg(target_os = "linux")]
    {
        "alsa"
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        "system"
    }
}

#[cfg(test)]
mod tests {
    use super::SinkProtocol;

    #[test]
    fn airplay_and_sonos_do_not_support_dsp() {
        assert!(!SinkProtocol::AirPlayCoreAudio.supports_dsp());
        assert!(!SinkProtocol::AirPlayRaop.supports_dsp());
        assert!(!SinkProtocol::AirPlay2.supports_dsp());
        assert!(!SinkProtocol::SonosUpnp.supports_dsp());
        assert!(SinkProtocol::LocalCoreAudio.supports_dsp());
        assert!(SinkProtocol::UpnpAvRenderer.supports_dsp());
    }
}
