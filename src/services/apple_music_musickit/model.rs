use crate::protocol::SourceRef;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub(super) const PROTOCOL_VERSION: u32 = 2;
pub(super) const EXPECTED_HELPER_BUNDLE_ID: &str = "com.fozmo.apple-music-helper";

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
    /// Filled in by Fozmo rather than by Apple. Absent until a track has played.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_format: Option<AppleVerifiedFormat>,
}

/// The decoder format Fozmo verified while this catalog item actually played.
/// Apple publishes only a coarse quality tier, so an exact rate and depth can
/// come from nowhere else.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq)]
pub(crate) struct AppleVerifiedFormat {
    pub codec: String,
    pub sample_rate: i64,
    #[serde(default)]
    pub bit_depth: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub(crate) struct MusicAppTrack {
    pub track_key: Option<String>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration_secs: Option<f64>,
    pub position_secs: Option<f64>,
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
    /// Best format Fozmo has verified across this album's played tracks.
    /// Filled in by Fozmo rather than by Apple, and absent until one has played.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_format: Option<AppleVerifiedFormat>,
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
    pub helper_capabilities: Vec<String>,
    pub last_error: Option<AppleMusicMvpError>,
    pub integration_stage: String,
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
            helper_capabilities: Vec::new(),
            last_error: None,
            integration_stage: "musickit_catalog_music_app_capture".to_string(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct AppleMusicAuthorizeRequest {
    #[serde(default)]
    pub present_ui: bool,
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
            song_id: None,
            album_id: None,
            term: None,
            limit: None,
            storefront: None,
            catalog_song: None,
            catalog_album: None,
            catalog_search: None,
            code: None,
            message: None,
            retryable: None,
        }
    }
}
