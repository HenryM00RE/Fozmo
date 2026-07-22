use super::SinkProtocol;
use crate::audio::dsp::resampler::DEFAULT_FILTER_NAME;
use crate::library::{BrowserStreamSettings, ZoneHegelSettings, ZoneUpnpCapabilities};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityDetectionSource {
    Advertised,
    Probed,
    Probing,
    #[default]
    Fallback,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityDetectionStatus {
    Complete,
    Probing,
    Deferred,
    Failed,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ZoneCapabilities {
    pub max_sample_rate: u32,
    pub max_bit_depth: u8,
    #[serde(default)]
    pub max_dsd_rate: Option<u16>,
    pub exclusive_supported: bool,
    pub gapless_supported: bool,
    #[serde(default)]
    pub supports_dsd128: bool,
    #[serde(default)]
    pub supports_dsd256: bool,
    #[serde(default)]
    pub capability_detection_source: CapabilityDetectionSource,
    #[serde(default)]
    pub capability_detection_status: CapabilityDetectionStatus,
    #[serde(default)]
    pub capability_detection_message: Option<String>,
}

impl Default for ZoneCapabilities {
    fn default() -> Self {
        Self {
            max_sample_rate: 384_000,
            max_bit_depth: 32,
            max_dsd_rate: None,
            exclusive_supported: true,
            gapless_supported: true,
            supports_dsd128: false,
            supports_dsd256: false,
            capability_detection_source: CapabilityDetectionSource::Fallback,
            capability_detection_status: CapabilityDetectionStatus::Unknown,
            capability_detection_message: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DspProfile {
    pub upsampling_enabled: bool,
    pub filter_type: String,
    pub target_rate: u32,
    pub dither_mode: String,
}

impl Default for DspProfile {
    fn default() -> Self {
        Self {
            upsampling_enabled: false,
            filter_type: DEFAULT_FILTER_NAME.to_string(),
            target_rate: 0,
            dither_mode: "Auto".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ZoneProfile {
    pub id: String,
    pub name: String,
    pub protocol: SinkProtocol,
    #[serde(default)]
    pub agent_name: Option<String>,
    #[serde(default)]
    pub backend: Option<String>,
    pub capabilities: ZoneCapabilities,
    pub dsp_profile: DspProfile,
    pub status: ZoneStatus,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub device_name: Option<String>,
    #[serde(default)]
    pub network_address: Option<String>,
    #[serde(default)]
    pub status_message: Option<String>,
    #[serde(default)]
    pub playing_state: Option<String>,
    #[serde(default)]
    pub track_title: Option<String>,
    #[serde(default)]
    pub airplay_default_volume: Option<f32>,
    #[serde(default)]
    pub airplay_max_volume: Option<f32>,
    #[serde(default)]
    pub airplay_last_volume: Option<f32>,
    #[serde(default)]
    pub qobuz_hires_enabled: bool,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub device_type: Option<String>,
    #[serde(default)]
    pub hegel: Option<ZoneHegelSettings>,
    #[serde(default)]
    pub upnp_calibrated_capabilities: Option<ZoneUpnpCapabilities>,
    /// True for a browser-private zone: playable only from the browser
    /// session that registered it.
    #[serde(default)]
    pub browser: bool,
    /// Stream delivery choice for browser zones (FLAC vs Opus + bitrate).
    #[serde(default)]
    pub browser_stream: Option<BrowserStreamSettings>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ZoneStatus {
    Available,
    Active,
    Offline,
}
