use crate::protocol::{SourceRef, UpnpPcmContainerCapability};
use crate::services::qobuz::{QobuzAlbum, QobuzContributorCredit};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Serialize)]
pub struct ArtRefreshResult {
    pub processed: usize,
    pub improved: usize,
}

#[derive(Debug, Serialize, Clone, JsonSchema)]
pub struct LibraryScanResult {
    pub scanned: usize,
    pub updated: usize,
    pub removed: usize,
}

#[derive(Debug, Serialize, Clone, JsonSchema)]
pub struct LibraryScanProgress {
    pub running: bool,
    pub phase: String,
    pub scanned: usize,
    pub total: usize,
    pub updated: usize,
    pub removed: usize,
    pub current_path: Option<String>,
    pub message: String,
    pub last_result: Option<LibraryScanResult>,
    pub error: Option<String>,
}

impl Default for LibraryScanProgress {
    fn default() -> Self {
        Self {
            running: false,
            phase: "idle".to_string(),
            scanned: 0,
            total: 0,
            updated: 0,
            removed: 0,
            current_path: None,
            message: "Ready".to_string(),
            last_result: None,
            error: None,
        }
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct LibrarySummary {
    pub albums: i64,
    pub artists: i64,
    pub tracks: i64,
    pub unmatched_albums: i64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ZoneQueueEntry {
    pub source: SourceRef,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct NowPlayingQueueSnapshot {
    pub state: Value,
    pub updated_at: i64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PlaybackHistoryInput {
    #[serde(default)]
    pub profile_id: Option<String>,
    pub source: SourceRef,
    pub zone_id: String,
    pub zone_name: String,
    #[serde(default)]
    pub played_secs: Option<f64>,
    #[serde(default)]
    pub duration_secs: Option<f64>,
    #[serde(default)]
    pub completed: bool,
    #[serde(default)]
    pub counted: bool,
    #[serde(default)]
    pub radio: bool,
}

#[derive(Debug, Serialize, Clone, JsonSchema)]
pub struct PlaybackHistoryEntry {
    pub id: i64,
    pub profile_id: String,
    pub source: SourceRef,
    pub zone_id: String,
    pub zone_name: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_id: Option<i64>,
    pub art_id: Option<i64>,
    pub image_url: Option<String>,
    pub played_secs: Option<f64>,
    pub duration_secs: Option<f64>,
    pub completed: bool,
    pub counted: bool,
    pub radio: bool,
    pub played_at: i64,
}

#[derive(Debug, Serialize)]
pub struct PlaybackHistoryDataExport {
    pub schema_version: u32,
    pub exported_at: i64,
    pub entries: Vec<PlaybackHistoryDataEntry>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PlaybackHistoryDataEntry {
    #[serde(default)]
    pub profile_id: Option<String>,
    pub source: SourceRef,
    pub zone_id: String,
    pub zone_name: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub artist: Option<String>,
    #[serde(default)]
    pub album: Option<String>,
    #[serde(default)]
    pub image_url: Option<String>,
    #[serde(default)]
    pub played_secs: Option<f64>,
    #[serde(default)]
    pub duration_secs: Option<f64>,
    #[serde(default)]
    pub completed: bool,
    #[serde(default)]
    pub counted: bool,
    #[serde(default)]
    pub radio: bool,
    pub played_at: i64,
}

#[derive(Debug, Serialize)]
pub struct PlaybackHistoryImportResult {
    pub imported: usize,
    pub skipped: usize,
    pub replaced: bool,
}

#[derive(Debug, Serialize, Clone, Default, JsonSchema)]
pub struct PlaybackSummary {
    pub play_count: i64,
    pub last_played_at: Option<i64>,
    pub listened_secs: f64,
}

#[derive(Debug, Clone)]
pub struct ZoneDefinition {
    pub id: String,
    pub name: String,
    pub kind: Option<String>,
    pub device_name: Option<String>,
    pub enabled: bool,
}

fn default_hegel_port() -> u16 {
    50001
}

fn default_hegel_input() -> u8 {
    9
}

fn default_hegel_default_volume() -> u8 {
    20
}

fn default_hegel_max_volume() -> u8 {
    50
}

#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct ZoneHegelSettings {
    #[serde(default)]
    pub linked_airplay_zone_id: Option<String>,
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default = "default_hegel_port")]
    pub port: u16,
    #[serde(default = "default_hegel_input")]
    pub input: u8,
    #[serde(default = "default_hegel_default_volume")]
    pub default_volume: u8,
    #[serde(default = "default_hegel_max_volume")]
    pub max_volume: u8,
    #[serde(default)]
    pub standby_usb_visible: bool,
    #[serde(default)]
    pub model: Option<String>,
}

impl Default for ZoneHegelSettings {
    fn default() -> Self {
        Self {
            linked_airplay_zone_id: None,
            host: None,
            port: default_hegel_port(),
            input: default_hegel_input(),
            default_volume: default_hegel_default_volume(),
            max_volume: default_hegel_max_volume(),
            standby_usb_visible: false,
            model: None,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Default, JsonSchema)]
pub struct ZoneSettings {
    #[serde(default)]
    pub airplay_default_volume: Option<f32>,
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
    pub upnp_capabilities: Option<ZoneUpnpCapabilities>,
    #[serde(default)]
    pub upnp_calibrated_capabilities: Option<ZoneUpnpCapabilities>,
    #[serde(default)]
    pub browser_stream: Option<BrowserStreamSettings>,
}

/// Per-browser-zone stream delivery choice. Absent means the legacy
/// automatic behavior (lossless on LAN, Opus for remote lossless sources).
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema, PartialEq, Eq)]
pub struct BrowserStreamSettings {
    /// "flac" (lossless, EQ baked in server-side) or "opus".
    pub format: String,
    /// Opus bitrate in kbit/s; one of 128, 256, 320.
    #[serde(default = "default_browser_opus_kbps")]
    pub opus_kbps: u32,
}

fn default_browser_opus_kbps() -> u32 {
    crate::audio::transcode::opus::DEFAULT_BITRATE_KBPS
}

#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema, PartialEq, Eq)]
pub struct ZoneUpnpCapabilities {
    pub max_sample_rate: u32,
    #[serde(default = "default_upnp_bit_depth")]
    pub max_bit_depth: u8,
    #[serde(default)]
    pub max_dsd_rate: Option<u16>,
    #[serde(default)]
    pub pcm_containers: Vec<UpnpPcmContainerCapability>,
}

fn default_upnp_bit_depth() -> u8 {
    24
}

#[derive(Debug, Serialize, Clone, JsonSchema)]
pub struct AlbumSummary {
    pub id: i64,
    pub title: String,
    pub album_artist: Option<String>,
    pub year: Option<i32>,
    /// First release date of the MusicBrainz release group — the album's
    /// original year, as opposed to `year` which may be a reissue/remaster date.
    pub original_year: Option<i32>,
    pub track_count: i64,
    pub art_id: Option<i64>,
    pub confidence: i64,
    pub match_status: String,
    pub primary_version_id: Option<i64>,
    pub qobuz_album_id: Option<String>,
    pub qobuz_match_status: Option<String>,
    pub qobuz_match_confidence: Option<i64>,
    pub canonical_art_id: Option<i64>,
    /// Qobuz cover fallback for matched albums that have no stored local or
    /// canonical artwork.
    pub image_url: Option<String>,
    /// Barcode (UPC/EAN) of the matched MusicBrainz release, used to
    /// cross-check Qobuz auto-linking.
    pub mb_barcode: Option<String>,
}

#[derive(Debug, Serialize, Clone, JsonSchema)]
pub struct TrackSummary {
    pub id: i64,
    pub file_name: String,
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub track_number: Option<i64>,
    pub disc_number: Option<i64>,
    pub year: Option<i32>,
    pub genre: Option<String>,
    pub composer: Option<String>,
    pub duration_secs: Option<f64>,
    pub sample_rate: Option<i64>,
    pub bit_depth: Option<i64>,
    pub channels: Option<i64>,
    pub format: Option<String>,
    pub album_id: Option<i64>,
    pub art_id: Option<i64>,
    pub play_count: i64,
    pub last_played_at: Option<i64>,
    pub listened_secs: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preferred_play_source: Option<ResolvedPlaySource>,
}

#[derive(Debug, Serialize, Clone, JsonSchema)]
pub struct ArtistSummary {
    pub name: String,
    pub album_count: i64,
    pub track_count: i64,
    pub play_count: i64,
    pub listened_secs: f64,
}

#[derive(Debug, Serialize)]
pub struct AlbumDetail {
    pub album: AlbumSummary,
    pub tracks: Vec<TrackSummary>,
    pub candidates: Vec<MatchCandidate>,
    pub versions: Vec<AlbumVersionSummary>,
    pub canonical_album: Option<CanonicalAlbum>,
    pub canonical_tracks: Vec<CanonicalTrack>,
    pub qobuz_track_links: Vec<QobuzTrackLinkSummary>,
}

#[derive(Debug, Serialize, Clone, JsonSchema)]
pub struct CanonicalAlbum {
    pub title: String,
    pub album_artist: Option<String>,
    pub release_date: Option<String>,
    pub year: Option<i32>,
    pub track_count: i64,
    pub art_id: Option<i64>,
    pub image_url: Option<String>,
    pub qobuz_album_id: String,
    pub maximum_sampling_rate: Option<i64>,
    pub maximum_bit_depth: Option<i64>,
    pub hires: bool,
    pub description: Option<String>,
    pub genre: Option<String>,
    pub label: Option<String>,
    pub duration_secs: Option<f64>,
}

#[derive(Debug, Serialize, Clone, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResolvedPlaySource {
    Local {
        track_id: i64,
        title: String,
        artist: Option<String>,
        album: Option<String>,
        art_id: Option<i64>,
        duration_secs: Option<f64>,
        file_name: String,
    },
    Qobuz {
        track_id: u64,
        title: String,
        artist: Option<String>,
        album: Option<String>,
        album_id: Option<String>,
        image_url: Option<String>,
        duration_secs: Option<f64>,
        format_id: Option<u32>,
    },
}

#[derive(Debug, Serialize, Clone, JsonSchema)]
pub struct CanonicalTrack {
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub track_number: Option<i64>,
    pub disc_number: Option<i64>,
    pub duration_secs: Option<f64>,
    pub sample_rate: Option<i64>,
    pub format: Option<String>,
    pub bit_depth: Option<i64>,
    pub image_url: Option<String>,
    pub qobuz_track_id: Option<String>,
    pub play_source: Option<ResolvedPlaySource>,
    pub qobuz_source: Option<ResolvedPlaySource>,
    pub composer: Option<String>,
    pub work: Option<String>,
    pub isrc: Option<String>,
    pub copyright: Option<String>,
    pub performers_raw: Option<String>,
    pub credits: Vec<QobuzContributorCredit>,
    pub play_count: i64,
    pub last_played_at: Option<i64>,
    pub listened_secs: f64,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ListeningHistoryStats {
    pub range: String,
    pub total_listened_secs: f64,
    pub weekly_buckets: Vec<ListeningTimeBucket>,
    pub weekday_buckets: Vec<ListeningTimeBucket>,
    pub top_artists: Vec<ListeningRankItem>,
    pub top_albums: Vec<ListeningRankItem>,
    pub top_songs: Vec<ListeningRankItem>,
    pub top_genres: Vec<ListeningRankItem>,
    pub recent_tracks: Vec<PlaybackHistoryEntry>,
}

#[derive(Debug, Serialize, Clone, JsonSchema)]
pub struct ListeningTimeBucket {
    pub key: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_at: Option<i64>,
    pub listened_secs: f64,
}

#[derive(Debug, Serialize, Clone, JsonSchema)]
pub struct ListeningRankItem {
    pub name: String,
    pub subtitle: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub album: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub album_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub qobuz_album_id: Option<String>,
    pub listened_secs: f64,
    pub play_count: i64,
    pub art_id: Option<i64>,
    pub image_url: Option<String>,
}

#[derive(Debug, Serialize, Clone, JsonSchema)]
pub struct ListeningTopSongs {
    pub range: String,
    pub items: Vec<ListeningTopSongItem>,
}

#[derive(Debug, Serialize, Clone, JsonSchema)]
pub struct ListeningTopSongItem {
    pub rank: usize,
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub source_key: String,
    pub play_count: i64,
    pub listened_secs: f64,
    pub last_played_at: i64,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct QobuzTrackLinkSummary {
    pub local_track_id: i64,
    pub qobuz_track_id: String,
    pub confidence: i64,
    pub match_kind: String,
    pub status: String,
}

#[derive(Debug, Serialize)]
pub struct AlbumPlaybackPlan {
    pub album_id: i64,
    pub sources: Vec<ResolvedPlaySource>,
}

#[derive(Debug, Serialize, Clone, JsonSchema)]
pub struct AlbumVersionSummary {
    pub id: i64,
    pub album_id: i64,
    pub provider: String,
    pub provider_id: String,
    pub title: String,
    pub artist: Option<String>,
    pub year: Option<i32>,
    pub track_count: i64,
    pub art_id: Option<i64>,
    pub format: Option<String>,
    pub sample_rate: Option<i64>,
    pub bit_depth: Option<i64>,
    pub source_label: Option<String>,
    pub status: String,
    pub is_primary: bool,
    pub musicbrainz_match_status: Option<String>,
    pub musicbrainz_release_id: Option<String>,
    pub musicbrainz_tagged_at: Option<i64>,
    pub qobuz_match_status: Option<String>,
    pub qobuz_tagged_at: Option<i64>,
    pub autometa_message: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct LibrarySearchResponse {
    pub query: String,
    pub albums: Vec<AlbumSummary>,
    pub artists: Vec<ArtistSummary>,
    pub tracks: Vec<TrackSummary>,
}

#[derive(Debug, Clone, Default)]
pub struct LibraryBrowseQuery {
    pub q: Option<String>,
    pub limit: i64,
    pub offset: i64,
    pub sort: Option<String>,
    pub direction: Option<String>,
    pub genre: Option<String>,
    pub decade: Option<i32>,
    pub quality: Option<String>,
    pub source: Option<String>,
    pub include_facets: bool,
}

#[derive(Debug, Serialize, Clone, JsonSchema)]
pub struct LibraryFacetOption {
    pub value: String,
    pub label: String,
    pub count: i64,
}

#[derive(Debug, Serialize, Clone, Default, JsonSchema)]
pub struct LibraryBrowseFacets {
    pub genres: Vec<LibraryFacetOption>,
    pub decades: Vec<LibraryFacetOption>,
    pub qualities: Vec<LibraryFacetOption>,
    pub sources: Vec<LibraryFacetOption>,
}

#[derive(Debug, Serialize, Clone, JsonSchema)]
pub struct LibraryBrowsePage<T> {
    pub items: Vec<T>,
    pub total: i64,
    pub limit: i64,
    pub offset: i64,
    pub has_more: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub facets: Option<LibraryBrowseFacets>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct FavoriteAlbumSummary {
    pub id: String,
    pub provider: String,
    pub title: String,
    pub album_artist: Option<String>,
    pub art_id: Option<i64>,
    pub image_url: Option<String>,
    pub year: Option<i32>,
    pub is_qobuz: bool,
    pub qobuz_id: Option<String>,
    pub qobuz_album_id: Option<String>,
    pub hires: bool,
    pub favorited_at: i64,
}

#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistSummary {
    pub id: String,
    pub name: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub items: Vec<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistSaveRequest {
    pub name: Option<String>,
    pub created_at: Option<i64>,
    pub updated_at: Option<i64>,
    #[serde(default)]
    pub items: Vec<Value>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RecentPlaylistSummary {
    pub recent_type: String,
    pub id: String,
    pub playlist_id: String,
    pub title: String,
    pub album_artist: String,
    pub played_at: i64,
    pub is_playlist: bool,
    pub items: Vec<Value>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RecentAlbumSummary {
    pub recent_type: String,
    pub id: String,
    pub title: String,
    pub album_artist: String,
    pub art_id: Option<i64>,
    pub image_url: Option<String>,
    pub year: Option<i32>,
    pub is_qobuz: bool,
    pub qobuz_album_id: Option<String>,
    pub source_track_id: Option<String>,
    pub album_id: Option<String>,
    pub hires: bool,
    pub match_status: Option<String>,
    pub played_at: i64,
}

#[derive(Debug, Deserialize)]
pub struct FavoriteAlbumRequest {
    pub id: String,
    pub provider: Option<String>,
    pub title: Option<String>,
    pub album_artist: Option<String>,
    pub artist: Option<String>,
    pub art_id: Option<i64>,
    pub image_url: Option<String>,
    pub year: Option<i32>,
    #[serde(default)]
    pub is_qobuz: bool,
    pub qobuz_id: Option<String>,
    pub qobuz_album_id: Option<String>,
    #[serde(default)]
    pub hires: bool,
}

#[derive(Debug, Deserialize)]
pub struct FavoriteAlbumRemoveRequest {
    pub id: String,
    pub provider: Option<String>,
    pub qobuz_id: Option<String>,
    pub qobuz_album_id: Option<String>,
    #[serde(default)]
    pub is_qobuz: bool,
}

#[derive(Debug, Serialize, Clone)]
pub struct MatchCandidate {
    pub id: i64,
    pub provider: String,
    pub provider_id: String,
    pub title: String,
    pub artist: Option<String>,
    pub year: Option<i32>,
    pub score: i64,
    pub status: String,
}

#[derive(Debug, Serialize)]
pub struct MatchResponse {
    pub album: AlbumSummary,
    pub candidates: Vec<MatchCandidate>,
    pub applied: bool,
}

/// Diff between the current field value in the local library and the value
/// MusicBrainz proposes for the same field. Used by `CandidatePreview` so the
/// UI can render side-by-side comparisons before applying a match.
#[derive(Debug, Serialize)]
pub struct FieldDiff<T: Serialize + PartialEq> {
    pub from: T,
    pub to: T,
    pub changed: bool,
}

impl<T: Serialize + PartialEq> FieldDiff<T> {
    pub(super) fn new(from: T, to: T) -> Self {
        let changed = from != to;
        Self { from, to, changed }
    }
}

#[derive(Debug, Serialize)]
pub struct AlbumPreview {
    pub title: FieldDiff<String>,
    pub album_artist: FieldDiff<Option<String>>,
    pub year: FieldDiff<Option<i32>>,
    pub mb_release_id: String,
    pub mb_release_group_id: Option<String>,
    pub mb_barcode: Option<String>,
    pub country: Option<String>,
    pub date: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct TrackPreview {
    pub file_track_id: i64,
    pub mb_disc: i64,
    pub mb_position: i64,
    pub mb_recording_id: Option<String>,
    pub title: FieldDiff<String>,
    pub artist: FieldDiff<Option<String>>,
    pub track_number: FieldDiff<Option<i64>>,
    pub disc_number: FieldDiff<Option<i64>>,
    /// "exact" when matched by disc+position, "fuzzy" when matched by title only.
    pub match_kind: String,
}

#[derive(Debug, Serialize)]
pub struct MbTrack {
    pub recording_id: Option<String>,
    pub disc: i64,
    pub position: i64,
    pub title: String,
    pub artist: Option<String>,
    pub length_secs: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct CandidatePreview {
    pub candidate: MatchCandidate,
    pub album: AlbumPreview,
    pub tracks: Vec<TrackPreview>,
    pub unmatched_file_tracks: Vec<TrackSummary>,
    pub unmatched_mb_tracks: Vec<MbTrack>,
}

#[derive(Debug, Serialize)]
pub struct MetaBrainzInference {
    pub artist: Option<String>,
    pub album: String,
    pub raw_folder_title: Option<String>,
    pub search_queries: Vec<String>,
}

#[derive(Debug, Serialize, Clone)]
pub struct MetaBrainzEvidence {
    pub auto_apply_eligible: bool,
    pub release_status: Option<String>,
    pub track_count_match: bool,
    pub disc_count_match: Option<bool>,
    pub paired_tracks: usize,
    pub local_track_count: usize,
    pub duration_checked: usize,
    pub duration_within: usize,
    pub warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct MetaBrainzTestResponse {
    pub album: AlbumSummary,
    pub version: Option<AlbumVersionSummary>,
    pub tracks: Vec<TrackSummary>,
    pub inference: MetaBrainzInference,
    pub best_candidate: Option<MatchCandidate>,
    pub preview: Option<CandidatePreview>,
    pub evidence: MetaBrainzEvidence,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub qobuz_match: Option<QobuzMatchTestResponse>,
}

#[derive(Debug, Deserialize, Default)]
pub struct MetaBrainzTestRequest {
    #[serde(default)]
    pub refresh: Option<bool>,
}

#[derive(Debug, Deserialize, Default)]
pub struct AlbumEdit {
    pub title: Option<String>,
    pub album_artist: Option<String>,
    #[serde(default)]
    pub year: Option<Option<i32>>,
    #[serde(default)]
    pub tracks: Vec<TrackEdit>,
}

#[derive(Debug, Deserialize)]
pub struct TrackEdit {
    pub id: i64,
    pub title: String,
    pub artist: Option<String>,
    pub track_number: Option<i64>,
    pub disc_number: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct MatchRequest {
    pub candidate_id: Option<i64>,
    pub refresh: Option<bool>,
    /// When applying a candidate, pull cover art from Cover Art Archive and
    /// overwrite the album's current art. Defaults to true for backwards
    /// compatibility; the UI sets it to false when the album already has a
    /// user-supplied or folder cover the user wants to keep.
    pub replace_cover: Option<bool>,
    /// User-specified file→MB track pairings, taking precedence over the
    /// auto-pairing in `pair_tracks`. Used when the user manually matches
    /// a song in the MusicBrainz modal that the auto-matcher missed or
    /// got wrong.
    #[serde(default)]
    pub manual_pairings: Vec<ManualPairing>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ManualPairing {
    pub file_track_id: i64,
    pub mb_disc: i64,
    pub mb_position: i64,
}

#[derive(Debug, Deserialize)]
pub struct ManualSearchRequest {
    /// Free-form query, used as-is. Either this or the structured fields below
    /// must be set.
    pub query: Option<String>,
    pub album: Option<String>,
    pub artist: Option<String>,
    pub year: Option<i32>,
}

#[derive(Debug, Deserialize)]
pub struct MbidLookupRequest {
    pub mbid: String,
}

#[derive(Debug, Deserialize)]
pub struct ManualQobuzVersionRequest {
    pub provider_id: Option<String>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub year: Option<i32>,
    pub track_count: Option<i64>,
    pub format: Option<String>,
    pub sample_rate: Option<i64>,
    pub bit_depth: Option<i64>,
    pub source_label: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct QobuzLinkRequest {
    pub qobuz_album_id: String,
}

#[derive(Debug, Deserialize)]
pub struct AlbumPlayResolveRequest {
    pub start_index: Option<usize>,
    #[serde(default)]
    pub shuffle: bool,
    pub version_id: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct QobuzMatchResponse {
    pub album: AlbumSummary,
    pub matched: bool,
    pub qobuz_album_id: Option<String>,
    pub score: i64,
    pub status: String,
}

/// Verdict on whether a Qobuz album may be auto-linked as the canonical
/// version of a local album. `score` is the fuzzy 0–100 match score (kept for
/// display/review ranking); `auto_link` is the strict evidence gate.
#[derive(Debug, Serialize, Clone)]
pub struct QobuzMatchAssessment {
    pub score: i64,
    pub auto_link: bool,
    /// Some(true)/Some(false) when both the MB barcode and the Qobuz UPC are
    /// known; None when either side is missing.
    pub barcode_match: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct QobuzMatchTestCandidate {
    pub album: QobuzAlbum,
    pub assessment: QobuzMatchAssessment,
    pub track_count: usize,
}

#[derive(Debug, Serialize)]
pub struct QobuzMatchTestResponse {
    pub matched: bool,
    pub qobuz_album_id: Option<String>,
    pub score: i64,
    pub status: String,
    pub query: String,
    pub album: Option<QobuzAlbum>,
    pub assessment: Option<QobuzMatchAssessment>,
    pub candidates: Vec<QobuzMatchTestCandidate>,
}

#[derive(Debug, Serialize, Clone)]
pub struct AutoMetaProgress {
    pub job_id: Option<i64>,
    pub status: String,
    pub running: bool,
    pub processed: usize,
    pub total: usize,
    pub exact_matched: usize,
    pub musicbrainz_matched: usize,
    pub qobuz_matched: usize,
    pub no_proper_match: usize,
    pub skipped: usize,
    pub errors: usize,
    pub current_album: Option<String>,
    pub current_version: Option<String>,
    pub phase: Option<String>,
    pub mode: Option<String>,
    pub link_qobuz: bool,
    pub last_result: Option<String>,
    pub error: Option<String>,
    pub started_at: Option<i64>,
    pub updated_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub elapsed_secs: Option<i64>,
    pub eta_secs: Option<i64>,
    pub rate_per_min: Option<f64>,
    pub remaining: usize,
    pub pause_requested: bool,
    pub stop_requested: bool,
    pub recent_results: Vec<AutoMetaJobItem>,
}

impl Default for AutoMetaProgress {
    fn default() -> Self {
        Self {
            job_id: None,
            status: "idle".to_string(),
            running: false,
            processed: 0,
            total: 0,
            exact_matched: 0,
            musicbrainz_matched: 0,
            qobuz_matched: 0,
            no_proper_match: 0,
            skipped: 0,
            errors: 0,
            current_album: None,
            current_version: None,
            phase: None,
            mode: None,
            link_qobuz: false,
            last_result: None,
            error: None,
            started_at: None,
            updated_at: None,
            finished_at: None,
            elapsed_secs: None,
            eta_secs: None,
            rate_per_min: None,
            remaining: 0,
            pause_requested: false,
            stop_requested: false,
            recent_results: Vec::new(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct AutoMetaRunRequest {
    #[serde(default)]
    pub link_qobuz: bool,
    #[serde(default = "default_autometa_mode")]
    pub mode: String,
}

fn default_autometa_mode() -> String {
    "remaining".to_string()
}

#[derive(Debug, Serialize, Clone)]
pub struct AutoMetaJobItem {
    pub id: i64,
    pub job_id: i64,
    pub album_id: i64,
    pub version_id: i64,
    pub album_title: String,
    pub version_label: String,
    pub phase: String,
    pub status: String,
    pub attempts: i64,
    pub musicbrainz_release_id: Option<String>,
    pub qobuz_album_id: Option<String>,
    pub message: Option<String>,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub updated_at: i64,
}

#[derive(Debug, Deserialize)]
pub struct AutoMetaItemsQuery {
    pub status: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
pub struct AutoMetaAuditIssue {
    pub album_id: i64,
    pub version_id: Option<i64>,
    pub album_title: String,
    pub version_label: Option<String>,
    pub kind: String,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct AutoMetaLocalVersion {
    pub album_id: i64,
    pub version_id: i64,
    pub album_title: String,
    pub version_label: String,
    pub is_primary_version: bool,
    pub musicbrainz_match_status: Option<String>,
    pub musicbrainz_release_id: Option<String>,
    pub qobuz_match_status: Option<String>,
    pub album_qobuz_match_status: Option<String>,
    pub album_qobuz_album_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AutoMetaMusicBrainzResult {
    pub release_id: String,
}
