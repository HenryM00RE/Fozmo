use crate::protocol::PlaylistContext;
use bytes::Bytes;
use futures_util::stream::BoxStream;
use reqwest::StatusCode;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct QobuzUser {
    pub email: String,
    pub display_name: String,
    pub subscription_label: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct QobuzStatus {
    pub initialized: bool,
    pub logged_in: bool,
    pub user: Option<QobuzUser>,
}

#[derive(Debug, Serialize)]
pub struct QobuzCacheInfo {
    pub bytes: u64,
    pub files: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct QobuzContributorCredit {
    pub name: String,
    pub roles: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct QobuzTrack {
    pub id: u64,
    pub title: String,
    pub artist: String,
    #[serde(default)]
    pub artist_id: Option<u64>,
    pub album: String,
    pub album_id: Option<String>,
    pub track_number: Option<u32>,
    pub disc_number: Option<u32>,
    pub duration: u32,
    pub image_url: Option<String>,
    pub maximum_sampling_rate: Option<f64>,
    pub maximum_bit_depth: Option<u32>,
    pub hires: bool,
    pub streamable: bool,
    #[serde(default)]
    pub composer: Option<String>,
    #[serde(default)]
    pub work: Option<String>,
    #[serde(default)]
    pub isrc: Option<String>,
    #[serde(default)]
    pub copyright: Option<String>,
    #[serde(default)]
    pub performers_raw: Option<String>,
    #[serde(default)]
    pub credits: Vec<QobuzContributorCredit>,
    #[serde(default)]
    pub play_count: i64,
    #[serde(default)]
    pub last_played_at: Option<i64>,
    #[serde(default)]
    pub listened_secs: f64,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct QobuzSearchResponse {
    pub tracks: Vec<QobuzTrack>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct QobuzAlbum {
    pub id: String,
    pub title: String,
    pub artist: String,
    #[serde(default)]
    pub artist_id: Option<u64>,
    pub image_url: Option<String>,
    /// Exact release date from `release_date_original` ("YYYY-MM-DD"), when Qobuz provides it.
    pub release_date: Option<String>,
    /// Release year parsed from `release_date_original`.
    pub year: Option<i32>,
    pub tracks_count: Option<u32>,
    pub duration: Option<u32>,
    pub maximum_sampling_rate: Option<f64>,
    pub maximum_bit_depth: Option<u32>,
    pub hires: bool,
    pub genre: Option<String>,
    #[serde(default)]
    pub genre_id: Option<u64>,
    pub label: Option<String>,
    /// `album`, `ep`, `single`, `epMini`, `live`, `compilation`, etc. Used by the
    /// frontend to group releases in the artist Discography view.
    pub release_type: Option<String>,
    /// Qobuz edition/version label such as `Expanded Edition`, `Live`, or `Remixes`.
    /// Artist list payloads often omit `release_type`, so the frontend also uses
    /// this value for discography grouping.
    pub version: Option<String>,
    /// Editorial review/description (Qobuz `description` field). Present on
    /// `/album/get` responses, absent on search-result list items.
    pub description: Option<String>,
    /// Barcode (UPC/EAN) of the release. Cross-checked against the
    /// MusicBrainz barcode when deciding whether to auto-link an album.
    #[serde(default)]
    pub upc: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct QobuzPlaylist {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub image_url: Option<String>,
    #[serde(default)]
    pub tracks_count: Option<u32>,
    #[serde(default)]
    pub duration: Option<u32>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct QobuzFeaturedPlaylistsResponse {
    pub playlists: Vec<QobuzPlaylist>,
    pub limit: u32,
    pub offset: u32,
    pub count: u32,
    pub total: Option<u32>,
    pub has_more: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct QobuzAlbumPageResponse {
    pub albums: Vec<QobuzAlbum>,
    pub limit: u32,
    pub offset: u32,
    pub count: u32,
    pub total: Option<u32>,
    pub has_more: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct QobuzPlaylistTag {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct QobuzGenre {
    pub id: u64,
    pub label: String,
    #[serde(default)]
    pub parent_id: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct QobuzHomeArtist {
    pub id: Option<u64>,
    pub name: String,
    pub image_url: Option<String>,
    pub subtitle: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct QobuzHomeSection {
    pub id: String,
    pub title: String,
    pub subtitle: Option<String>,
    pub item_type: String,
    #[serde(default)]
    pub albums: Vec<QobuzAlbum>,
    #[serde(default)]
    pub artists: Vec<QobuzHomeArtist>,
    #[serde(default)]
    pub playlists: Vec<QobuzPlaylist>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct QobuzHomeResponse {
    pub logged_in: bool,
    pub sections: Vec<QobuzHomeSection>,
    pub partial_errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct QobuzArtist {
    pub id: u64,
    pub name: String,
    pub image_url: Option<String>,
    pub genre: Option<String>,
    pub albums_count: Option<u32>,
    pub biography: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct QobuzArtistImageResponse {
    pub name: String,
    pub artist_id: Option<u64>,
    pub image_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_image_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct QobuzArtistDetail {
    pub artist: QobuzArtist,
    pub top_tracks: Vec<QobuzTrack>,
    pub albums: Vec<QobuzAlbum>,
    pub similar: Vec<QobuzArtist>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct QobuzArtistCore {
    pub artist: QobuzArtist,
    pub albums: Vec<QobuzAlbum>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct QobuzArtistTopTracks {
    pub top_tracks: Vec<QobuzTrack>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct QobuzArtistSimilar {
    pub similar: Vec<QobuzArtist>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct QobuzArtistSearchResponse {
    pub artists: Vec<QobuzArtist>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct QobuzAlbumSearchResponse {
    pub albums: Vec<QobuzAlbum>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct QobuzAlbumDetail {
    pub album: QobuzAlbum,
    pub tracks: Vec<QobuzTrack>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct QobuzPlaylistDetail {
    pub playlist: QobuzPlaylist,
    pub tracks: Vec<QobuzTrack>,
}

#[derive(Debug, Serialize)]
pub struct QobuzRadioRecommendation {
    pub track: QobuzTrack,
    pub algorithm: Option<String>,
}

#[derive(Deserialize)]
pub struct QobuzLoginRequest {
    pub email: String,
    pub password: String,
}

impl std::fmt::Debug for QobuzLoginRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QobuzLoginRequest")
            .field("email", &"[redacted]")
            .field("password", &"[redacted]")
            .finish()
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct QobuzPlayRequest {
    pub track_id: u64,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    #[serde(default)]
    pub album_id: Option<String>,
    pub image_url: Option<String>,
    #[serde(default)]
    pub duration_secs: Option<f64>,
    pub format_id: Option<u32>,
    #[serde(default)]
    pub expected_current: Option<String>,
    #[serde(default)]
    pub radio_auto: bool,
    #[serde(default)]
    pub replace_current: bool,
    #[serde(default)]
    pub playlist_context: Option<PlaylistContext>,
    #[serde(default)]
    pub queue: Vec<QobuzQueueTrack>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct QobuzQueueTrack {
    pub track_id: u64,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    #[serde(default)]
    pub album_id: Option<String>,
    pub image_url: Option<String>,
    #[serde(default)]
    pub duration_secs: Option<f64>,
    pub format_id: Option<u32>,
    #[serde(default)]
    pub radio: bool,
    #[serde(default)]
    pub playlist_context: Option<PlaylistContext>,
}

pub struct QobuzResolvedStream {
    pub mime_type: String,
    pub sample_rate_hz: u32,
    pub bit_depth: u32,
    pub format_id: u32,
    pub byte_len: Option<u64>,
}

pub struct QobuzProxyResponse {
    pub status: StatusCode,
    pub content_type: Option<String>,
    pub content_length: Option<String>,
    pub content_range: Option<String>,
    /// Sample rate of the delivered file as reported by `getFileUrl` (Hz).
    pub sampling_rate_hz: Option<u32>,
    pub bit_depth: Option<u32>,
    pub body: BoxStream<'static, Result<Bytes, reqwest::Error>>,
}
