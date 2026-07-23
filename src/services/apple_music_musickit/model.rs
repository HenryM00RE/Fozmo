use serde::{Deserialize, Serialize};

pub(super) const PROTOCOL_VERSION: u32 = 1;
pub(super) const EXPECTED_HELPER_BUNDLE_ID: &str = "com.fozmo.apple-music-helper";

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub(crate) struct AppleMusicNowPlaying {
    pub song_id: String,
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub duration_secs: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AppleMusicMvpState {
    HelperMissing,
    Stopped,
    LaunchingHelper,
    CheckingAuthorization,
    AwaitingAuthorization,
    Ready,
    PreparingQueue,
    Playing,
    Paused,
    Stopping,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct AppleMusicMvpError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
    pub stage: String,
    pub cleanup_complete: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct AppleMusicProcessTapMetrics {
    pub callbacks_received: u64,
    pub frames_received: u64,
    pub ring_overruns: u64,
    pub invalid_callbacks: u64,
    pub rms_l: f32,
    pub rms_r: f32,
    pub last_callback_age_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AppleMusicProcessTapStatus {
    pub supported: bool,
    pub minimum_macos_version: String,
    pub state: String,
    pub music_app_running: bool,
    pub music_app_pid: Option<u32>,
    pub audio_process_object_id: Option<u32>,
    pub tap_object_id: Option<u32>,
    pub aggregate_device_id: Option<u32>,
    pub sample_rate_hz: Option<u32>,
    pub channels: Option<u32>,
    pub interleaved: Option<bool>,
    /// Native PCM representation delivered by the Core Audio tap.
    pub sample_format: Option<String>,
    /// Storage width of each tap sample, not the catalog asset's bit depth.
    pub sample_container_bits: Option<u32>,
    /// Numerical precision of the tap representation (24 bits for IEEE F32).
    pub sample_precision_bits: Option<u32>,
    /// Original decoded asset depth, when a provider can authoritatively report it.
    pub source_bit_depth_bits: Option<u32>,
    /// Whether Core Audio reports the tap format property as writable.
    pub format_settable: Option<bool>,
    /// True when Fozmo copies tap sample values without quantizing or scaling them.
    pub sample_values_preserved: bool,
    pub original_audio_muted_while_tapped: bool,
    pub dsp_handoff_active: bool,
    pub output_device: Option<String>,
    pub metrics: AppleMusicProcessTapMetrics,
    pub last_error: Option<AppleMusicMvpError>,
}

impl Default for AppleMusicProcessTapStatus {
    fn default() -> Self {
        Self {
            supported: true,
            minimum_macos_version: "14.2".to_string(),
            state: "stopped".to_string(),
            music_app_running: false,
            music_app_pid: None,
            audio_process_object_id: None,
            tap_object_id: None,
            aggregate_device_id: None,
            sample_rate_hz: None,
            channels: None,
            interleaved: None,
            sample_format: None,
            sample_container_bits: None,
            sample_precision_bits: None,
            source_bit_depth_bits: None,
            format_settable: None,
            sample_values_preserved: false,
            original_audio_muted_while_tapped: false,
            dsp_handoff_active: false,
            output_device: None,
            metrics: AppleMusicProcessTapMetrics::default(),
            last_error: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AppleMusicMvpStatus {
    pub feature_enabled: bool,
    pub supported: bool,
    pub minimum_macos_version: String,
    pub helper_present: bool,
    pub helper_bundle_id: String,
    pub helper_version: Option<String>,
    pub helper_musickit_entitled: bool,
    pub helper_pid: Option<u32>,
    pub session_id: Option<String>,
    pub state: AppleMusicMvpState,
    pub authorization: String,
    pub can_play_catalog_content: Option<bool>,
    pub playback_state: String,
    pub playback_time_secs: Option<f64>,
    pub queue_revision: u64,
    pub now_playing: Option<AppleMusicNowPlaying>,
    pub helper_capabilities: Vec<String>,
    pub last_error: Option<AppleMusicMvpError>,
    pub integration_stage: String,
    pub process_tap: AppleMusicProcessTapStatus,
}

impl AppleMusicMvpStatus {
    pub(super) fn new(helper_present: bool) -> Self {
        Self {
            feature_enabled: true,
            supported: true,
            minimum_macos_version: "14.2".to_string(),
            helper_present,
            helper_bundle_id: EXPECTED_HELPER_BUNDLE_ID.to_string(),
            helper_version: None,
            helper_musickit_entitled: false,
            helper_pid: None,
            session_id: None,
            state: if helper_present {
                AppleMusicMvpState::Stopped
            } else {
                AppleMusicMvpState::HelperMissing
            },
            authorization: "not_determined".to_string(),
            can_play_catalog_content: None,
            playback_state: "stopped".to_string(),
            playback_time_secs: None,
            queue_revision: 0,
            now_playing: None,
            helper_capabilities: Vec::new(),
            last_error: None,
            integration_stage: "music_app_process_tap".to_string(),
            process_tap: AppleMusicProcessTapStatus::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct AppleMusicAuthorizeRequest {
    #[serde(default)]
    pub present_ui: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct AppleMusicDevPlaySongRequest {
    pub song_id: String,
    pub storefront: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct AppleMusicTransportRequest {
    pub command: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct AppleMusicProcessTapStartRequest {
    #[serde(default)]
    pub confirm_system_audio_capture: bool,
    #[serde(default = "default_true")]
    pub mute_original_audio: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct HelperQueueItem {
    pub song_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storefront: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct HelperMessage {
    pub v: u32,
    #[serde(rename = "type")]
    pub message_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bundle_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub helper_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub musickit_entitled: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol_version: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub present_ui: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authorization: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub can_play_catalog_content: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub playback_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub playback_time_secs: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queue_revision: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub now_playing: Option<AppleMusicNowPlaying>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retryable: Option<bool>,
}

impl HelperMessage {
    pub(super) fn command(id: String, message_type: &str, session_id: String) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            message_type: message_type.to_string(),
            id: Some(id),
            command_id: None,
            session_id: Some(session_id),
            token: None,
            pid: None,
            bundle_id: None,
            helper_version: None,
            musickit_entitled: None,
            capabilities: Vec::new(),
            protocol_version: None,
            present_ui: None,
            authorization: None,
            can_play_catalog_content: None,
            playback_state: None,
            playback_time_secs: None,
            queue_revision: None,
            now_playing: None,
            code: None,
            message: None,
            retryable: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct SetQueueCommand {
    pub v: u32,
    pub id: String,
    #[serde(rename = "type")]
    pub message_type: &'static str,
    pub session_id: String,
    pub queue_revision: u64,
    pub items: Vec<HelperQueueItem>,
    pub start_index: usize,
}
