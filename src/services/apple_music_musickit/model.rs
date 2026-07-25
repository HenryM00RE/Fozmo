use crate::protocol::SourceRef;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub(super) const PROTOCOL_VERSION: u32 = 2;
pub(super) const EXPECTED_HELPER_BUNDLE_ID: &str = "com.fozmo.apple-music-helper";

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq)]
pub(crate) struct AppleMusicNowPlaying {
    pub song_id: String,
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub duration_secs: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq)]
pub(crate) struct AppleCatalogSong {
    pub song_id: String,
    pub storefront: String,
    #[serde(default)]
    pub album_id: Option<String>,
    pub title: String,
    pub artist: String,
    #[serde(default)]
    pub album_title: Option<String>,
    #[serde(default)]
    pub album_artist: Option<String>,
    #[serde(default)]
    pub duration_secs: Option<f64>,
    #[serde(default)]
    pub track_number: Option<u32>,
    #[serde(default)]
    pub disc_number: Option<u32>,
    #[serde(default)]
    pub isrc: Option<String>,
    #[serde(default)]
    pub artwork_url: Option<String>,
    /// Variants Apple advertises for this catalog item. Playback still gates
    /// on MusicKit's active variant because availability is not selection.
    #[serde(default)]
    pub audio_variants: Vec<String>,
}

impl AppleCatalogSong {
    pub(crate) fn source_ref(&self) -> SourceRef {
        SourceRef::AppleMusicTrack {
            song_id: self.song_id.clone(),
            storefront: (!self.storefront.trim().is_empty()).then(|| self.storefront.clone()),
            title: Some(self.title.clone()),
            artist: Some(self.artist.clone()),
            album: self.album_title.clone(),
            album_artist: self.album_artist.clone(),
            album_id: self.album_id.clone(),
            artwork_url: self.artwork_url.clone(),
            duration_secs: self.duration_secs,
            track_number: self.track_number,
            disc_number: self.disc_number,
            isrc: self.isrc.clone(),
            radio: false,
            radio_context: None,
            playlist_context: None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq)]
pub(crate) struct AppleCatalogAlbum {
    pub album_id: String,
    pub storefront: String,
    pub title: String,
    pub artist: String,
    #[serde(default)]
    pub upc: Option<String>,
    #[serde(default)]
    pub release_date: Option<String>,
    #[serde(default)]
    pub artwork_url: Option<String>,
    #[serde(default)]
    pub editorial_notes_standard: Option<String>,
    #[serde(default)]
    pub audio_variants: Vec<String>,
    #[serde(default)]
    pub tracks: Vec<AppleCatalogSong>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq)]
pub(crate) struct AppleCatalogSearchResult {
    pub term: String,
    pub storefront: String,
    #[serde(default)]
    pub songs: Vec<AppleCatalogSong>,
    #[serde(default)]
    pub albums: Vec<AppleCatalogAlbum>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub(crate) struct AppleMusicAlbumVersionRequest {
    pub album_id: String,
    #[serde(default)]
    pub storefront: Option<String>,
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

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub(crate) struct AppleMusicMvpError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
    pub stage: String,
    pub cleanup_complete: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub(crate) struct ApplePlaybackSnapshot {
    pub zone_id: String,
    pub player_epoch: u64,
    pub helper_session_id: String,
    pub queue_revision: u64,
    pub segment: Vec<SourceRef>,
    pub current_segment_index: usize,
    pub playback_state: String,
    pub position_secs: f64,
    #[serde(default)]
    pub last_error: Option<AppleMusicMvpError>,
}

impl ApplePlaybackSnapshot {
    pub(crate) fn current_source(&self) -> Option<&SourceRef> {
        self.segment.get(self.current_segment_index)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub(crate) struct AppleMusicHelperEventSummary {
    pub event_type: String,
    #[serde(default)]
    pub queue_revision: Option<u64>,
    #[serde(default)]
    pub segment_index: Option<usize>,
    #[serde(default)]
    pub song_id: Option<String>,
    #[serde(default)]
    pub playback_position: Option<f64>,
    #[serde(default)]
    pub finish_reason: Option<String>,
    #[serde(default)]
    pub code: Option<String>,
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
    pub target_pid: Option<u32>,
    pub target_process_kind: Option<String>,
    pub target_display_name: Option<String>,
    pub audio_process_object_id: Option<u32>,
    pub tap_object_id: Option<u32>,
    pub aggregate_device_id: Option<u32>,
    /// PCM mix rate currently delivered by the Core Audio process tap.
    pub sample_rate_hz: Option<u32>,
    pub channels: Option<u32>,
    pub interleaved: Option<bool>,
    /// Native PCM representation delivered by the Core Audio tap.
    pub sample_format: Option<String>,
    /// Storage width of each tap sample, not the catalog asset's bit depth.
    pub sample_container_bits: Option<u32>,
    /// Numerical precision of the tap representation (24 bits for IEEE F32).
    pub sample_precision_bits: Option<u32>,
    /// Decoded asset rate from a fresh, PID-scoped Apple lossless-decoder event.
    /// This remains unset when no authoritative source-rate signal is available.
    pub source_sample_rate_hz: Option<u32>,
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
            target_pid: None,
            target_process_kind: None,
            target_display_name: None,
            audio_process_object_id: None,
            tap_object_id: None,
            aggregate_device_id: None,
            sample_rate_hz: None,
            channels: None,
            interleaved: None,
            sample_format: None,
            sample_container_bits: None,
            sample_precision_bits: None,
            source_sample_rate_hz: None,
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
    /// The quality variant MusicKit actually selected for the active entry.
    /// Fozmo accepts only `lossless` and `highResolutionLossless`.
    pub active_audio_variant: Option<String>,
    pub playback_time_secs: Option<f64>,
    pub queue_revision: u64,
    pub now_playing: Option<AppleMusicNowPlaying>,
    pub helper_capabilities: Vec<String>,
    pub last_error: Option<AppleMusicMvpError>,
    pub integration_stage: String,
    pub process_tap: AppleMusicProcessTapStatus,
    pub playback_session: Option<ApplePlaybackSnapshot>,
    pub recent_events: Vec<AppleMusicHelperEventSummary>,
    pub comparison: AppleMusicComparisonStatus,
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
            active_audio_variant: None,
            playback_time_secs: None,
            queue_revision: 0,
            now_playing: None,
            helper_capabilities: Vec::new(),
            last_error: None,
            integration_stage: "musickit_renderer_process_tap".to_string(),
            process_tap: AppleMusicProcessTapStatus::default(),
            playback_session: None,
            recent_events: Vec::new(),
            comparison: AppleMusicComparisonStatus::default(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub(crate) struct AppleMusicComparisonTrack {
    pub track_key: Option<String>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration_secs: Option<f64>,
    pub position_secs: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct AppleMusicComparisonReference {
    pub zone_id: String,
    pub zone_name: String,
    pub provider: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub position_secs: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct AppleMusicComparisonStatus {
    pub active_side: String,
    pub can_switch_to_fozmo: bool,
    pub match_position: bool,
    pub reference: Option<AppleMusicComparisonReference>,
    pub apple_music_track: Option<AppleMusicComparisonTrack>,
    pub last_switch_message: Option<String>,
}

impl Default for AppleMusicComparisonStatus {
    fn default() -> Self {
        Self {
            active_side: "fozmo".to_string(),
            can_switch_to_fozmo: false,
            match_position: true,
            reference: None,
            apple_music_track: None,
            last_switch_message: None,
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

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct AppleMusicPlayRequest {
    #[serde(default)]
    pub zone_id: Option<String>,
    #[serde(default)]
    pub song_id: Option<String>,
    #[serde(default)]
    pub source: Option<SourceRef>,
    #[serde(default)]
    pub storefront: Option<String>,
    #[serde(default)]
    pub queue: Vec<SourceRef>,
    #[serde(default)]
    pub confirm_system_audio_capture: bool,
}

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub(crate) struct AppleMusicCatalogQuery {
    #[serde(default)]
    pub storefront: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct AppleMusicCatalogSearchQuery {
    #[serde(default)]
    pub term: String,
    #[serde(default)]
    pub storefront: Option<String>,
    #[serde(default = "default_catalog_search_limit")]
    pub limit: u32,
}

fn default_catalog_search_limit() -> u32 {
    10
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

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct AppleMusicCaptureConfirmationRequest {
    #[serde(default)]
    pub confirm_system_audio_capture: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct AppleMusicComparisonSwitchRequest {
    pub target: String,
    #[serde(default)]
    pub confirm_system_audio_capture: bool,
    #[serde(default = "default_true")]
    pub match_position: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct HelperQueueItem {
    pub song_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storefront: Option<String>,
    pub segment_index: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct HelperMessage {
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
    pub audio_variant: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub playback_time_secs: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queue_revision: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub segment_index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub song_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub album_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub term: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storefront: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub playback_position: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position_secs: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub now_playing: Option<AppleMusicNowPlaying>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub catalog_song: Option<AppleCatalogSong>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub catalog_album: Option<AppleCatalogAlbum>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub catalog_search: Option<AppleCatalogSearchResult>,
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
            audio_variant: None,
            playback_time_secs: None,
            queue_revision: None,
            segment_index: None,
            song_id: None,
            album_id: None,
            term: None,
            limit: None,
            storefront: None,
            playback_position: None,
            position_secs: None,
            finish_reason: None,
            now_playing: None,
            catalog_song: None,
            catalog_album: None,
            catalog_search: None,
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
