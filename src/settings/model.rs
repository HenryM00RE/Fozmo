use super::dsd::DsdSourceRule;
use super::playback::ZonePlaybackSettings;
use crate::audio::eq::EqConfig;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub const DEFAULT_PROFILE_ID: &str = "default";

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ListeningProfile {
    pub id: String,
    pub name: String,
    pub color: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent_searches: Vec<String>,
}

impl Default for ListeningProfile {
    fn default() -> Self {
        Self {
            id: DEFAULT_PROFILE_ID.to_string(),
            name: "Default".to_string(),
            color: "#7c8f6a".to_string(),
            image: None,
            recent_searches: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HegelSettings {
    #[serde(default)]
    pub enabled: bool,
    pub zone_id: Option<String>,
    #[serde(default)]
    pub linked_airplay_zone_id: Option<String>,
    pub host: Option<String>,
    #[serde(default = "default_hegel_port")]
    pub port: u16,
    #[serde(default = "default_hegel_usb_input")]
    pub input: u8,
    #[serde(default = "default_hegel_default_volume")]
    pub default_volume: u8,
    #[serde(default = "default_hegel_max_volume")]
    pub max_volume: u8,
    #[serde(default)]
    pub standby_usb_visible: bool,
}

impl Default for HegelSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            zone_id: None,
            linked_airplay_zone_id: None,
            host: None,
            port: default_hegel_port(),
            input: default_hegel_usb_input(),
            default_volume: default_hegel_default_volume(),
            max_volume: default_hegel_max_volume(),
            standby_usb_visible: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppleMusicCaptureSettings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub capture_device_name: Option<String>,
    #[serde(default)]
    pub output_device_name: Option<String>,
    #[serde(default = "default_apple_music_buffer_ms")]
    pub buffer_ms: u32,
    /// When true, starting capture switches the macOS default output to
    /// Fozmo Capture and restores the previous default on stop.
    #[serde(default = "default_apple_music_auto_route")]
    pub auto_route_system_output: bool,
}

impl Default for AppleMusicCaptureSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            capture_device_name: None,
            output_device_name: None,
            buffer_ms: default_apple_music_buffer_ms(),
            auto_route_system_output: default_apple_music_auto_route(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppearanceSettings {
    #[serde(default)]
    pub custom_display_font_enabled: bool,
    #[serde(default)]
    pub custom_display_font_user_configured: bool,
    #[serde(default = "default_custom_display_font_scale_percent")]
    pub custom_display_font_scale_percent: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_display_font_filename: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_display_font_original_filename: Option<String>,
    #[serde(default)]
    pub custom_display_font_version: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub custom_display_font_supported_ranges: Vec<[u32; 2]>,
}

impl Default for AppearanceSettings {
    fn default() -> Self {
        Self {
            custom_display_font_enabled: false,
            custom_display_font_user_configured: false,
            custom_display_font_scale_percent: default_custom_display_font_scale_percent(),
            custom_display_font_filename: None,
            custom_display_font_original_filename: None,
            custom_display_font_version: 0,
            custom_display_font_supported_ranges: Vec::new(),
        }
    }
}

fn default_custom_display_font_scale_percent() -> u16 {
    100
}

/// Roon-ARC-style remote access over a manually forwarded router port.
/// Disabled by default; the remote listener only ever starts from these
/// persisted settings (or the LAN-only settings API), never from a remote
/// session.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RemoteAccessSettings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_remote_access_port")]
    pub port: u16,
    /// Display/URL-hint only. Never an input to auth decisions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_cert_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_key_path: Option<String>,
}

impl Default for RemoteAccessSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            port: default_remote_access_port(),
            external_host: None,
            custom_cert_path: None,
            custom_key_path: None,
        }
    }
}

fn default_remote_access_port() -> u16 {
    8443
}

fn default_apple_music_buffer_ms() -> u32 {
    250
}

fn default_apple_music_auto_route() -> bool {
    false
}

fn default_hegel_port() -> u16 {
    50001
}

fn default_hegel_usb_input() -> u8 {
    9
}

fn default_hegel_default_volume() -> u8 {
    20
}

fn default_hegel_max_volume() -> u8 {
    50
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum AuthTokenKind {
    PairingToken,
    ControlSession,
    AgentToken,
    StreamToken,
    RemoteLinkCode,
    RemoteSession,
    #[default]
    LegacyToken,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuthTokenBinding {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct RemoteSessionClientMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_family: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_hint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ip_family: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PairingTokenRecord {
    pub id: String,
    #[serde(default)]
    pub kind: AuthTokenKind,
    pub token_hash: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub issued_at_unix_secs: u64,
    pub expires_at_unix_secs: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used_at_unix_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotated_at_unix_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at_unix_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding: Option<AuthTokenBinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_session_metadata: Option<RemoteSessionClientMetadata>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PersistedSettings {
    // Legacy/global playback fields are kept for compatibility with older settings files.
    // New writes also mirror the active zone here, while zone-specific copies live below.
    pub filter_type: Option<String>,
    /// `None` or `Some(0)` both mean "Auto Best".
    pub target_rate: Option<u32>,
    /// Target PCM bit depth for rendered/transported DSP output.
    pub target_bit_depth: Option<u32>,
    pub upsampling_enabled: Option<bool>,
    pub exclusive: Option<bool>,
    pub dither_mode: Option<String>,
    /// "Pcm" (default), "Dsd64", "Dsd128", or "Dsd256".
    pub output_mode: Option<String>,
    /// Selectable values are "Standard" and "EcBeam2". Retired EC-depth and
    /// EcBeam values are accepted as legacy input and normalize to "Standard".
    pub dsd_modulator: Option<String>,
    /// DSD EC transition-loss compensation. 0.0 means no compensation.
    pub dsd_isi_penalty: Option<f32>,
    #[serde(default)]
    pub dsd_rules_enabled: bool,
    #[serde(default)]
    pub dsd_rules: Vec<DsdSourceRule>,
    /// Output attenuation in dB, clamped to -24.0..=0.0.
    pub headroom_db: Option<f32>,
    /// DSP output preroll buffer in milliseconds. 0 means automatic/default.
    pub dsp_buffer_ms: Option<u32>,
    pub device_name: Option<String>,
    pub active_zone_id: Option<String>,
    pub volume: Option<f32>,
    pub eq: Option<EqConfig>,
    #[serde(default, skip_serializing)]
    pub pairing_tokens: Option<Vec<String>>,
    #[serde(default, skip_serializing)]
    pub pairing_token_records: Vec<PairingTokenRecord>,
    pub music_dirs: Option<Vec<String>>,
    pub profiles: Option<Vec<ListeningProfile>>,
    pub active_profile_id: Option<String>,
    #[serde(default)]
    pub qobuz_radio_enabled: Option<bool>,
    #[serde(default)]
    pub lastfm_radio_enabled: Option<bool>,
    #[serde(default, skip_serializing)]
    pub lastfm_api_key: Option<String>,
    #[serde(default)]
    pub hegel: HegelSettings,
    #[serde(default)]
    pub apple_music_capture: AppleMusicCaptureSettings,
    #[serde(default)]
    pub appearance: AppearanceSettings,
    #[serde(default)]
    pub remote_access: RemoteAccessSettings,
    #[serde(default)]
    pub zone_settings: HashMap<String, ZonePlaybackSettings>,
}
