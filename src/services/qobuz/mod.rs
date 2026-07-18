//! Minimal Qobuz client for the MVP streaming page.
//!
//! The web-player token extraction and request signatures are adapted from the
//! MIT-licensed QBZ project: https://github.com/vicrodh/qbz

use crate::secrets::SecretsStore;
use reqwest::header::CONTENT_TYPE;
use reqwest::redirect::Policy;
use reqwest::{Client, Url};

mod album;
mod artist;
mod auth;
mod cache;
mod client;
mod model;
mod parser;
mod playlist;
mod radio;
mod search;
mod stream;
use self::stream::StreamUrl;
// Compatibility facade for callers that still construct player media sources directly.
#[allow(unused_imports)]
pub use self::stream::{QobuzStreamHandle, QobuzStreamSource};
#[cfg(test)]
use self::stream::{
    qobuz_stream_display_name, should_retry_cd_after_download_error, validate_qobuz_stream_url,
};
use auth::{BundleTokens, PendingOAuthState, UserSession, load_session};

/// Keychain account string naming this cache dir's stored session; exposed so
/// bootstrap can list the legacy per-secret keychain item during secrets
/// migration.
pub(crate) fn session_account(cache_dir: &std::path::Path) -> String {
    auth::session_account(cache_dir)
}

#[cfg(test)]
use cache::{
    ALBUM_DETAIL_CACHE_MAX_ENTRIES, ALBUM_DETAIL_CACHE_TTL, normalize_qobuz_album_cache_key,
    now_epoch_secs, prune_album_detail_cache,
};
use cache::{
    CachedArtistImage, CachedArtistTopTracks, CachedQobuzAlbumDetail, CachedQobuzHome,
    load_album_detail_cache_from_disk, load_artist_image_cache_from_disk,
    load_artist_top_tracks_cache_from_disk, load_home_cache_from_disk,
};
pub use model::*;
pub(crate) use parser::qobuz_sized_cover_url as sized_cover_url;
use parser::{
    album_page_response_from_home_response, albums_from_home_response, artists_from_home_albums,
    parse_track, push_album_home_section, push_playlist_home_section,
};
#[cfg(test)]
use parser::{
    featured_playlists_response_from_featured_response, genres_from_response, parse_album,
    parse_playlist, parse_qobuz_performers, parse_radio_recommendation, parse_track_in_album,
    playlist_tags_from_response, playlists_from_featured_response,
    radio_artist_candidates_from_search, radio_suggest_body, standardize_qobuz_album_detail_covers,
    tracks_from_playlist_response,
};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex as TokioMutex, RwLock};

const USER_AGENT: &str = crate::app::identity::USER_AGENT;
const QOBUZ_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const QOBUZ_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const QOBUZ_COVER_MAX_BYTES: u64 = 5 * 1024 * 1024;
const QOBUZ_COVER_TIMEOUT: Duration = Duration::from_secs(10);
const QOBUZ_HOME_SHELF_LIMIT: u32 = 12;
const QOBUZ_HOME_SECTION_MAX_LIMIT: u32 = 50;
const QOBUZ_HOME_SECTION_FILTER_SCAN_BATCH: u32 = 50;
const QOBUZ_HOME_SECTION_FILTER_SCAN_MAX: u32 = 500;

type ArtistDetailCache = Arc<RwLock<HashMap<u64, (Instant, Arc<QobuzArtistDetail>)>>>;
type ProxyStreamUrlCache = Arc<RwLock<HashMap<(u64, Option<u32>), (Instant, StreamUrl)>>>;
type ProgressiveStreamHttpClient = stream_download::http::reqwest::Client;

pub struct QobuzService {
    http: Client,
    /// CDN audio bodies can legitimately remain open for an entire track, so
    /// they use a connect-bounded client without the API client's total
    /// request deadline.
    stream_http: Client,
    /// `stream-download` uses a different reqwest major version. Keep its
    /// client explicit so progressive playback cannot follow an unvalidated
    /// redirect destination.
    progressive_stream_http: ProgressiveStreamHttpClient,
    cover_http: Client,
    cache_dir: PathBuf,
    session_account: String,
    secrets: Arc<dyn SecretsStore>,
    tokens: Arc<RwLock<Option<BundleTokens>>>,
    session: Arc<RwLock<Option<UserSession>>>,
    pending_oauth_states: Arc<TokioMutex<Vec<PendingOAuthState>>>,
    validated_secret: Arc<RwLock<Option<String>>>,
    /// First quality the account is allowed to stream. We discover this on the
    /// first successful `/track/getFileUrl` call and try it first thereafter,
    /// so subsequent tracks skip the 1–3 wasted round trips through the
    /// quality fallback list. Reset on logout.
    preferred_format_id: Arc<RwLock<Option<u32>>>,
    /// In-memory cache of full artist-detail payloads keyed by Qobuz artist id.
    /// The artist page makes 4–5 outbound HTTP calls (Qobuz + MusicBrainz +
    /// ListenBrainz) — caching the assembled response makes repeat visits
    /// instant. Entries are short-lived so similar-artist/popularity drift is
    /// picked up within an hour.
    artist_detail_cache: ArtistDetailCache,
    /// Cached Qobuz home payload. The home screen fans out across several
    /// Qobuz endpoints, so we serve the assembled response from cache and let a
    /// background warmer refresh it hourly.
    home_cache: Arc<RwLock<Option<CachedQobuzHome>>>,
    home_cache_refresh: Arc<TokioMutex<()>>,
    /// Full album details keyed by Qobuz album id. Album pages need the parent
    /// album plus enriched track credits, which is expensive enough to keep
    /// around for recently played/reopened albums.
    album_detail_cache: Arc<RwLock<HashMap<String, CachedQobuzAlbumDetail>>>,
    /// Resolved artist top tracks are expensive because they combine
    /// MusicBrainz, ListenBrainz, and per-title Qobuz search calls.
    artist_top_tracks_cache: Arc<RwLock<HashMap<u64, CachedArtistTopTracks>>>,
    /// Artist portrait lookups keyed by normalized artist name. Stored on disk
    /// because the artists grid can revisit the same names on every app load.
    artist_image_cache: Arc<RwLock<HashMap<String, CachedArtistImage>>>,
    /// Resolved stream URLs for the byte-range proxy, keyed by
    /// (track_id, requested_format_id). Remote agents fetch a track as many
    /// sequential range requests; without this cache every block costs an
    /// extra signed `/track/getFileUrl` round trip. Entries are short-lived
    /// and dropped when Qobuz rejects the signed URL.
    proxy_stream_url_cache: ProxyStreamUrlCache,
}

fn build_qobuz_stream_http_client() -> Result<Client, String> {
    Client::builder()
        .user_agent(USER_AGENT)
        .redirect(Policy::none())
        .connect_timeout(QOBUZ_CONNECT_TIMEOUT)
        .build()
        .map_err(|e| format!("create Qobuz stream HTTP client: {e}"))
}

fn build_qobuz_progressive_stream_http_client() -> Result<ProgressiveStreamHttpClient, String> {
    ProgressiveStreamHttpClient::builder()
        .user_agent(USER_AGENT)
        .redirect(stream_download::http::reqwest::redirect::Policy::none())
        .connect_timeout(QOBUZ_CONNECT_TIMEOUT)
        .build()
        .map_err(|e| format!("create Qobuz progressive stream HTTP client: {e}"))
}

impl QobuzService {
    #[allow(dead_code)]
    pub fn new(cache_dir: PathBuf, secrets: Arc<dyn SecretsStore>) -> Result<Self, String> {
        let account = session_account(&cache_dir);
        Self::new_with_session_account(cache_dir, secrets, account)
    }

    pub(crate) fn new_with_session_account(
        cache_dir: PathBuf,
        secrets: Arc<dyn SecretsStore>,
        session_account: String,
    ) -> Result<Self, String> {
        let http = Client::builder()
            .user_agent(USER_AGENT)
            .cookie_store(true)
            .connect_timeout(QOBUZ_CONNECT_TIMEOUT)
            .timeout(QOBUZ_REQUEST_TIMEOUT)
            .build()
            .map_err(|e| format!("create Qobuz HTTP client: {e}"))?;
        let cover_http = Client::builder()
            .user_agent(USER_AGENT)
            .redirect(Policy::none())
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| format!("create Qobuz cover HTTP client: {e}"))?;
        let stream_http = build_qobuz_stream_http_client()?;
        let progressive_stream_http = build_qobuz_progressive_stream_http_client()?;

        let session = load_session(&cache_dir, secrets.as_ref(), &session_account);

        let home_cache = load_home_cache_from_disk(&cache_dir);
        let album_detail_cache = load_album_detail_cache_from_disk(&cache_dir);
        let artist_top_tracks_cache = load_artist_top_tracks_cache_from_disk(&cache_dir);
        let artist_image_cache = load_artist_image_cache_from_disk(&cache_dir);

        Ok(Self {
            http,
            stream_http,
            progressive_stream_http,
            cover_http,
            cache_dir,
            session_account,
            secrets,
            tokens: Arc::new(RwLock::new(None)),
            session: Arc::new(RwLock::new(session)),
            pending_oauth_states: Arc::new(TokioMutex::new(Vec::new())),
            validated_secret: Arc::new(RwLock::new(None)),
            preferred_format_id: Arc::new(RwLock::new(None)),
            artist_detail_cache: Arc::new(RwLock::new(HashMap::new())),
            home_cache: Arc::new(RwLock::new(home_cache)),
            home_cache_refresh: Arc::new(TokioMutex::new(())),
            album_detail_cache: Arc::new(RwLock::new(album_detail_cache)),
            artist_top_tracks_cache: Arc::new(RwLock::new(artist_top_tracks_cache)),
            artist_image_cache: Arc::new(RwLock::new(artist_image_cache)),
            proxy_stream_url_cache: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    pub async fn status(&self) -> QobuzStatus {
        let initialized = self.tokens.read().await.is_some();
        let session = self.session.read().await.clone();
        QobuzStatus {
            initialized,
            logged_in: session.is_some(),
            user: session.map(|s| s.user),
        }
    }

    pub async fn track_detail(&self, track_id: u64) -> Result<QobuzTrack, String> {
        let json = self
            .optional_get_value(
                "/track/get",
                vec![("track_id", track_id.to_string())],
                "Qobuz track get failed",
                "Qobuz track get response was not JSON",
                "Qobuz track get failed",
            )
            .await?;

        parse_track(&json).ok_or_else(|| "Qobuz track response missing required fields".to_string())
    }

    async fn fetch_home(&self) -> Result<QobuzHomeResponse, String> {
        let logged_in = self.session.read().await.is_some();
        let mut sections = Vec::new();
        let mut partial_errors = Vec::new();
        let mut discovery_albums = Vec::new();

        let _ = self.ensure_secret().await;

        let release_watch = async {
            if logged_in {
                Some(
                    self.release_watch_albums("artists", QOBUZ_HOME_SHELF_LIMIT, 0)
                        .await,
                )
            } else {
                None
            }
        };

        let album_of_the_week = async {
            if logged_in {
                Some(
                    self.discover_albums("/discover/albumOfTheWeek", 6, 0, None)
                        .await,
                )
            } else {
                None
            }
        };

        let new_releases = self.featured_albums("new-releases", QOBUZ_HOME_SHELF_LIMIT, 0, None);

        let standouts = async {
            if logged_in {
                Some(
                    self.discover_albums("/discover/qobuzissims", QOBUZ_HOME_SHELF_LIMIT, 0, None)
                        .await,
                )
            } else {
                None
            }
        };

        let press_awards = self.featured_albums("press-awards", QOBUZ_HOME_SHELF_LIMIT, 0, None);
        let most_streamed = self.featured_albums("most-streamed", QOBUZ_HOME_SHELF_LIMIT, 0, None);
        let playlists = self.featured_playlists(QOBUZ_HOME_SHELF_LIMIT, 0, None, None);

        let (
            release_watch,
            album_of_the_week,
            new_releases,
            standouts,
            press_awards,
            most_streamed,
            playlists,
        ) = tokio::join!(
            release_watch,
            album_of_the_week,
            new_releases,
            standouts,
            press_awards,
            most_streamed,
            playlists,
        );

        let mut push_result =
            |error_label: &str,
             id: &str,
             title: &str,
             subtitle: Option<&str>,
             result: Result<Vec<QobuzAlbum>, String>| {
                match result {
                    Ok(albums) => push_album_home_section(
                        &mut sections,
                        &mut discovery_albums,
                        id,
                        title,
                        subtitle,
                        albums,
                    ),
                    Err(err) => partial_errors.push(format!("{error_label}: {err}")),
                }
            };

        if let Some(result) = release_watch {
            push_result(
                "release-watch",
                "release-watch",
                "New from your artists",
                Some("Followed artists and labels"),
                result,
            );
        }
        if let Some(result) = album_of_the_week {
            push_result(
                "album-of-the-week",
                "album-of-the-week",
                "Albums of the week",
                Some("Qobuz editorial selection"),
                result,
            );
        }
        push_result(
            "new-releases",
            "new-releases",
            "New on Qobuz",
            Some("Recent catalog arrivals"),
            new_releases,
        );
        if let Some(result) = standouts {
            push_result(
                "qobuzissims",
                "qobuzissims",
                "Standouts",
                Some("Curated standouts"),
                result,
            );
        }
        push_result(
            "press-awards",
            "press-awards",
            "Press awards",
            Some("Critic-endorsed albums"),
            press_awards,
        );
        push_result(
            "most-streamed",
            "most-streamed",
            "Popular",
            None,
            most_streamed,
        );
        match playlists {
            Ok(playlists) => push_playlist_home_section(
                &mut sections,
                "editorial-playlists",
                "Qobuz playlists",
                Some("Curated listening from Qobuz editors"),
                playlists.playlists,
            ),
            Err(err) => partial_errors.push(format!("playlists: {err}")),
        }

        let artists = artists_from_home_albums(&discovery_albums, 12);
        if !artists.is_empty() {
            sections.push(QobuzHomeSection {
                id: "new-release-artists".to_string(),
                title: "Artists to explore".to_string(),
                subtitle: Some("Drawn from the new-release shelf".to_string()),
                item_type: "artist".to_string(),
                albums: Vec::new(),
                artists,
                playlists: Vec::new(),
            });
        }

        Ok(QobuzHomeResponse {
            logged_in,
            sections,
            partial_errors,
        })
    }

    pub async fn home_section(
        &self,
        category: &str,
        genre_id: Option<u64>,
        limit: u32,
        offset: u32,
    ) -> Result<QobuzAlbumPageResponse, String> {
        let category = normalize_home_category(category);
        let limit = limit.clamp(1, QOBUZ_HOME_SECTION_MAX_LIMIT);
        match category.as_str() {
            "new" => {
                self.featured_album_page("new-releases", limit, offset, genre_id)
                    .await
            }
            "popular" => {
                self.featured_album_page("most-streamed", limit, offset, genre_id)
                    .await
            }
            "acclaimed" => {
                self.featured_album_page("press-awards", limit, offset, genre_id)
                    .await
            }
            "standouts" => {
                self.discover_standout_album_page(limit, offset, genre_id)
                    .await
            }
            _ => Err(format!("Unknown Qobuz category: {category}")),
        }
    }

    async fn featured_albums(
        &self,
        featured_type: &str,
        limit: u32,
        offset: u32,
        genre_id: Option<u64>,
    ) -> Result<Vec<QobuzAlbum>, String> {
        self.featured_album_page(featured_type, limit, offset, genre_id)
            .await
            .map(|page| page.albums)
    }

    async fn featured_album_page(
        &self,
        featured_type: &str,
        limit: u32,
        offset: u32,
        genre_id: Option<u64>,
    ) -> Result<QobuzAlbumPageResponse, String> {
        let limit = limit.clamp(1, 50);
        let mut params = vec![
            ("limit", limit.to_string()),
            ("offset", offset.to_string()),
            ("type", featured_type.to_string()),
        ];
        if let Some(genre_id) = genre_id {
            params.push(("genre_id", genre_id.to_string()));
        }

        let json = self
            .signed_get_value("/album/getFeatured", "albumgetFeatured", params, false)
            .await?;
        Ok(album_page_response_from_home_response(&json, limit, offset))
    }

    async fn discover_albums(
        &self,
        path: &str,
        limit: u32,
        offset: u32,
        genre_id: Option<u64>,
    ) -> Result<Vec<QobuzAlbum>, String> {
        self.discover_album_page(path, limit, offset, genre_id)
            .await
            .map(|page| page.albums)
    }

    async fn discover_album_page(
        &self,
        path: &str,
        limit: u32,
        offset: u32,
        genre_id: Option<u64>,
    ) -> Result<QobuzAlbumPageResponse, String> {
        let method = path
            .chars()
            .filter(|c| *c != '/' && *c != '.')
            .collect::<String>();
        let limit = limit.clamp(1, 50);
        let mut params = vec![("limit", limit.to_string()), ("offset", offset.to_string())];
        if let Some(genre_id) = genre_id {
            params.push(("genre_id", genre_id.to_string()));
        }
        let json = self.signed_get_value(path, &method, params, true).await?;
        Ok(album_page_response_from_home_response(&json, limit, offset))
    }

    async fn discover_standout_album_page(
        &self,
        limit: u32,
        offset: u32,
        genre_id: Option<u64>,
    ) -> Result<QobuzAlbumPageResponse, String> {
        let limit = limit.clamp(1, 50);
        let needed = offset.saturating_add(limit).saturating_add(1) as usize;
        let mut matched = Vec::new();
        let mut seen = HashSet::new();
        let mut scan_offset = 0_u32;

        while matched.len() < needed && scan_offset < QOBUZ_HOME_SECTION_FILTER_SCAN_MAX {
            let albums = self
                .discover_albums(
                    "/discover/qobuzissims",
                    QOBUZ_HOME_SECTION_FILTER_SCAN_BATCH,
                    scan_offset,
                    genre_id,
                )
                .await?;
            if albums.is_empty() {
                break;
            }

            let source_count = albums.len() as u32;
            let mut new_album_count = 0_u32;
            for album in albums {
                if !seen.insert(album.id.clone()) {
                    continue;
                }
                new_album_count += 1;
                if genre_id
                    .map(|genre_id| qobuz_album_matches_genre(&album, genre_id))
                    .unwrap_or(true)
                {
                    matched.push(album);
                }
            }

            if !qobuz_standout_scan_should_continue(source_count, new_album_count) {
                break;
            }
            scan_offset = scan_offset.saturating_add(source_count);
        }

        Ok(qobuz_standout_album_page_response(matched, offset, limit))
    }

    async fn release_watch_albums(
        &self,
        release_type: &str,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<QobuzAlbum>, String> {
        let json = self
            .authenticated_get_value(
                "/favorite/getNewReleases",
                vec![
                    ("limit", limit.clamp(1, 50).to_string()),
                    ("offset", offset.to_string()),
                    ("type", release_type.to_string()),
                ],
            )
            .await?;
        Ok(albums_from_home_response(&json))
    }

    /// Public cover fetch wrapper used by the deferred-cover background task
    /// in the router. Returns the same shape as the old inline fetch.
    pub async fn fetch_cover_public(
        &self,
        url: &str,
    ) -> Result<(Option<String>, Option<Vec<u8>>), String> {
        self.cached_cover_public(url).await
    }

    async fn fetch_cover(&self, url: &str) -> Result<(Option<String>, Option<Vec<u8>>), String> {
        let url = validate_qobuz_cover_url(url)?;
        let mut response = self
            .cover_http
            .get(url)
            .timeout(QOBUZ_COVER_TIMEOUT)
            .send()
            .await
            .map_err(|e| qobuz_reqwest_error("download Qobuz cover", e))?;
        if !response.status().is_success() {
            return Err(format!("download Qobuz cover: HTTP {}", response.status()));
        }
        if let Some(content_length) = response.content_length()
            && content_length > QOBUZ_COVER_MAX_BYTES
        {
            return Err(format!(
                "Qobuz cover exceeds {} byte limit",
                QOBUZ_COVER_MAX_BYTES
            ));
        }
        let mime = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| "Qobuz cover response is missing a content type".to_string())?
            .to_string();
        if !is_image_content_type(&mime) {
            return Err(format!("Qobuz cover has unsupported content type: {mime}"));
        }

        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| qobuz_reqwest_error("read Qobuz cover bytes", e))?
        {
            let next_len = bytes.len().saturating_add(chunk.len());
            if next_len as u64 > QOBUZ_COVER_MAX_BYTES {
                return Err(format!(
                    "Qobuz cover exceeds {} byte limit",
                    QOBUZ_COVER_MAX_BYTES
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok((Some(mime), Some(bytes)))
    }
}

pub(super) fn qobuz_reqwest_error(context: &str, error: reqwest::Error) -> String {
    format!("{context}: {}", error.without_url())
}

fn validate_qobuz_cover_url(url: &str) -> Result<Url, String> {
    let parsed = Url::parse(url).map_err(|e| format!("invalid Qobuz cover URL: {e}"))?;
    if parsed.scheme() != "https" {
        return Err("Qobuz cover URL must use https".to_string());
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("Qobuz cover URL must not include credentials".to_string());
    }
    if parsed.port().is_some_and(|port| port != 443) {
        return Err("Qobuz cover URL must use the default https port".to_string());
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| "Qobuz cover URL is missing a host".to_string())?
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if host == "qobuz.com" || host.ends_with(".qobuz.com") {
        Ok(parsed)
    } else {
        Err("Qobuz cover URL host is not trusted".to_string())
    }
}

fn is_image_content_type(mime: &str) -> bool {
    mime.split(';')
        .next()
        .map(str::trim)
        .map(|mime| {
            mime.eq_ignore_ascii_case("image/jpeg")
                || mime.eq_ignore_ascii_case("image/png")
                || mime.eq_ignore_ascii_case("image/webp")
        })
        .unwrap_or(false)
}

fn normalize_home_category(category: &str) -> String {
    match category.trim().to_lowercase().as_str() {
        "new-releases" => "new".to_string(),
        "most-streamed" => "popular".to_string(),
        "press-awards" => "acclaimed".to_string(),
        "qobuzissims" => "standouts".to_string(),
        value => value.to_string(),
    }
}

fn qobuz_album_matches_genre(album: &QobuzAlbum, genre_id: u64) -> bool {
    album.genre_id == Some(genre_id)
}

fn qobuz_album_page(albums: Vec<QobuzAlbum>, offset: u32, limit: u32) -> Vec<QobuzAlbum> {
    albums
        .into_iter()
        .skip(offset as usize)
        .take(limit as usize)
        .collect()
}

fn qobuz_standout_album_page_response(
    matched: Vec<QobuzAlbum>,
    offset: u32,
    limit: u32,
) -> QobuzAlbumPageResponse {
    let requested_end = offset.saturating_add(limit) as usize;
    let has_more = matched.len() > requested_end;
    let albums = qobuz_album_page(matched, offset, limit);
    let count = albums.len() as u32;
    QobuzAlbumPageResponse {
        albums,
        limit,
        offset,
        count,
        total: None,
        has_more,
    }
}

fn qobuz_standout_scan_should_continue(source_count: u32, new_album_count: u32) -> bool {
    source_count > 0 && new_album_count > 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn credential_request_and_signed_stream_debug_output_is_redacted() {
        let login = QobuzLoginRequest {
            email: "private@example.test".to_string(),
            password: "password-value".to_string(),
        };
        let stream = StreamUrl {
            url: "https://cdn.example.test/file?token=signed-value".to_string(),
            requested_format_id: 27,
            format_id: 27,
            mime_type: "audio/flac".to_string(),
            sampling_rate: Some(192.0),
            bit_depth: Some(24),
        };

        let login_debug = format!("{login:?}");
        let stream_debug = format!("{stream:?}");
        assert!(!login_debug.contains("private@example.test"));
        assert!(!login_debug.contains("password-value"));
        assert!(!stream_debug.contains("cdn.example.test"));
        assert!(!stream_debug.contains("signed-value"));
        assert!(login_debug.contains("[redacted]"));
        assert!(stream_debug.contains("[redacted]"));
    }

    #[tokio::test]
    async fn reqwest_errors_drop_complete_credential_bearing_urls() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};

            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 2048];
            let _ = socket.read(&mut request).await;
            socket
                .write_all(
                    b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await
                .unwrap();
        });
        let error = reqwest::Client::new()
            .get(format!(
                "http://{address}/login?email=private%40example.test&password=hunter2&token=secret"
            ))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap_err();

        let sanitized = qobuz_reqwest_error("Qobuz login request failed", error);

        assert!(sanitized.contains("Qobuz login request failed"));
        assert!(!sanitized.contains(&address.to_string()));
        assert!(!sanitized.contains("private"));
        assert!(!sanitized.contains("hunter2"));
        assert!(!sanitized.contains("secret"));
    }

    fn play_req(format_id: Option<u32>) -> QobuzPlayRequest {
        QobuzPlayRequest {
            track_id: 42,
            title: Some("Traffic".to_string()),
            artist: Some("Thom Yorke".to_string()),
            album: None,
            album_id: None,
            image_url: None,
            duration_secs: None,
            format_id,
            expected_current: None,
            radio_auto: false,
            replace_current: false,
            playlist_context: None,
            queue: Vec::new(),
        }
    }

    fn album_detail_for_cache(id: &str) -> QobuzAlbumDetail {
        QobuzAlbumDetail {
            album: QobuzAlbum {
                id: id.to_string(),
                title: "Kid A".to_string(),
                artist: "Radiohead".to_string(),
                artist_id: None,
                image_url: None,
                release_date: None,
                year: Some(2000),
                tracks_count: Some(0),
                duration: None,
                maximum_sampling_rate: None,
                maximum_bit_depth: None,
                hires: false,
                genre: None,
                genre_id: None,
                label: None,
                release_type: None,
                version: None,
                description: None,
                upc: None,
            },
            tracks: Vec::new(),
        }
    }

    fn home_album(id: &str, genre_id: Option<u64>) -> QobuzAlbum {
        QobuzAlbum {
            id: id.to_string(),
            title: format!("Album {id}"),
            artist: "Artist".to_string(),
            artist_id: None,
            image_url: None,
            release_date: None,
            year: None,
            tracks_count: None,
            duration: None,
            maximum_sampling_rate: None,
            maximum_bit_depth: None,
            hires: false,
            genre: None,
            genre_id,
            label: None,
            release_type: None,
            version: None,
            description: None,
            upc: None,
        }
    }

    fn temp_qobuz_cache_dir(name: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "fozmo-qobuz-cache-{name}-{}-{unique}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn qobuz_album_cache_key_accepts_route_and_provider_ids() {
        assert_eq!(
            normalize_qobuz_album_cache_key(" qobuz:album:abc123 "),
            "abc123"
        );
        assert_eq!(normalize_qobuz_album_cache_key("abc123"), "abc123");
    }

    #[test]
    fn qobuz_album_cache_prunes_stale_and_oldest_entries() {
        let now = now_epoch_secs();
        let mut cache = HashMap::new();
        cache.insert(
            "stale".to_string(),
            CachedQobuzAlbumDetail {
                stored_at_epoch_secs: now.saturating_sub(ALBUM_DETAIL_CACHE_TTL.as_secs() + 1),
                user_email: None,
                detail: album_detail_for_cache("stale"),
            },
        );
        for index in 0..=ALBUM_DETAIL_CACHE_MAX_ENTRIES {
            let id = format!("album-{index}");
            cache.insert(
                id.clone(),
                CachedQobuzAlbumDetail {
                    stored_at_epoch_secs: now.saturating_sub(index as u64),
                    user_email: None,
                    detail: album_detail_for_cache(&id),
                },
            );
        }

        prune_album_detail_cache(&mut cache);

        assert_eq!(cache.len(), ALBUM_DETAIL_CACHE_MAX_ENTRIES);
        assert!(!cache.contains_key("stale"));
        assert!(!cache.contains_key(&format!("album-{ALBUM_DETAIL_CACHE_MAX_ENTRIES}")));
        assert!(cache.contains_key("album-0"));
    }

    #[tokio::test]
    async fn qobuz_clear_cache_removes_legacy_session_json() {
        let secrets: Arc<dyn SecretsStore> = Arc::new(crate::secrets::MemorySecretsStore::new());
        let cache_dir = temp_qobuz_cache_dir("clear-removes-session");
        let service = QobuzService::new(cache_dir.clone(), secrets).unwrap();
        std::fs::write(cache_dir.join("session.json"), r#"{"auth":"keep"}"#).unwrap();
        std::fs::write(cache_dir.join("home.json"), r#"{"cached":true}"#).unwrap();
        std::fs::create_dir_all(cache_dir.join("covers")).unwrap();
        std::fs::write(cache_dir.join("covers/cover.bin"), b"cached").unwrap();

        service.clear_cache().await.unwrap();

        assert!(!cache_dir.join("session.json").exists());
        assert!(!cache_dir.join("home.json").exists());
        assert!(!cache_dir.join("covers").exists());
    }

    #[test]
    fn qobuz_album_page_uses_filtered_offsets() {
        let albums = vec![
            home_album("1", Some(64)),
            home_album("2", Some(80)),
            home_album("3", Some(80)),
            home_album("4", Some(64)),
            home_album("5", Some(80)),
        ];
        let electronic = albums
            .into_iter()
            .filter(|album| qobuz_album_matches_genre(album, 80))
            .collect::<Vec<_>>();

        assert_eq!(
            qobuz_album_page(electronic, 1, 2)
                .into_iter()
                .map(|album| album.id)
                .collect::<Vec<_>>(),
            vec!["3", "5"]
        );
    }

    #[test]
    fn qobuz_standout_album_page_response_does_not_infer_total() {
        let albums = (1..=24)
            .map(|index| home_album(&index.to_string(), None))
            .collect::<Vec<_>>();

        let page = qobuz_standout_album_page_response(albums, 0, 28);

        assert_eq!(page.count, 24);
        assert_eq!(page.total, None);
        assert!(!page.has_more);
    }

    #[test]
    fn qobuz_standout_album_page_response_reports_more_when_extra_item_was_scanned() {
        let albums = (1..=29)
            .map(|index| home_album(&index.to_string(), None))
            .collect::<Vec<_>>();

        let page = qobuz_standout_album_page_response(albums, 0, 28);

        assert_eq!(page.count, 28);
        assert_eq!(page.total, None);
        assert!(page.has_more);
    }

    #[test]
    fn qobuz_standout_scan_continues_after_short_unique_pages() {
        assert!(qobuz_standout_scan_should_continue(12, 12));
        assert!(qobuz_standout_scan_should_continue(
            QOBUZ_HOME_SECTION_FILTER_SCAN_BATCH - 1,
            QOBUZ_HOME_SECTION_FILTER_SCAN_BATCH - 1
        ));
        assert!(!qobuz_standout_scan_should_continue(12, 0));
        assert!(!qobuz_standout_scan_should_continue(0, 0));
    }

    #[test]
    fn qobuz_cover_url_validation_allows_qobuz_https_hosts() {
        let url = validate_qobuz_cover_url(
            "https://static.qobuz.com/images/covers/ab/cd/example_600.jpg",
        )
        .unwrap();

        assert_eq!(url.host_str(), Some("static.qobuz.com"));
    }

    #[test]
    fn qobuz_cover_url_validation_rejects_ssrf_targets() {
        for url in [
            "http://static.qobuz.com/images/covers/a.jpg",
            "https://127.0.0.1/private",
            "https://[::1]/private",
            "https://qobuz.com.evil.test/cover.jpg",
            "https://static.qobuz.com:444/cover.jpg",
            "https://user:pass@static.qobuz.com/cover.jpg",
        ] {
            assert!(
                validate_qobuz_cover_url(url).is_err(),
                "expected {url} to be rejected"
            );
        }
    }

    #[test]
    fn qobuz_stream_url_validation_allows_verified_playback_hosts() {
        for url in [
            "https://streaming.qobuz.com/file?token=signed",
            "https://streaming2.qobuz.com/file?token=signed",
            "https://streaming-qobuz-sec.akamaized.net/file?token=signed",
        ] {
            assert!(
                validate_qobuz_stream_url(url).is_ok(),
                "expected {url} to be accepted"
            );
        }
    }

    #[test]
    fn qobuz_stream_url_validation_rejects_ssrf_and_lookalike_targets() {
        for url in [
            "http://streaming.qobuz.com/file",
            "https://user:pass@streaming.qobuz.com/file",
            "https://streaming.qobuz.com:444/file",
            "https://127.0.0.1/private",
            "https://10.0.0.1/private",
            "https://169.254.169.254/latest/meta-data",
            "https://[::1]/private",
            "https://[fe80::1]/private",
            "https://localhost/private",
            "https://qobuz.com.evil.test/file",
            "https://streaming.qobuz.com.evil.test/file",
            "https://streamingfoo.qobuz.com/file",
            "https://streaming42.qobuz.com/file",
            "https://unrelated.akamaized.net/file",
            "https://streaming-qobuz-sec.akamaized.net.evil.test/file",
        ] {
            assert!(
                validate_qobuz_stream_url(url).is_err(),
                "expected {url} to be rejected"
            );
        }
    }

    #[tokio::test]
    async fn cached_loopback_stream_url_is_evicted_without_fetching_or_returning_a_body() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let secrets: Arc<dyn SecretsStore> = Arc::new(crate::secrets::MemorySecretsStore::new());
        let service =
            QobuzService::new(temp_qobuz_cache_dir("stream-ssrf-cache"), secrets).unwrap();
        let cache_key = (42, None);
        service.proxy_stream_url_cache.write().await.insert(
            cache_key,
            (
                Instant::now(),
                StreamUrl {
                    url: format!("https://{address}/internal"),
                    requested_format_id: 6,
                    format_id: 6,
                    mime_type: "audio/flac".to_string(),
                    sampling_rate: Some(44.1),
                    bit_depth: Some(16),
                },
            ),
        );

        let result = service.proxy_bytes_with_format(42, None, None).await;

        assert!(result.is_err());
        assert!(
            !service
                .proxy_stream_url_cache
                .read()
                .await
                .contains_key(&cache_key)
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_err(),
            "the rejected loopback URL must never be requested"
        );
    }

    #[tokio::test]
    async fn qobuz_stream_clients_do_not_follow_redirects() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let internal = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let internal_address = internal.local_addr().unwrap();
        let redirects = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let redirect_address = redirects.local_addr().unwrap();
        let redirect_task = tokio::spawn(async move {
            for _ in 0..2 {
                let (mut socket, _) = redirects.accept().await.unwrap();
                let mut request = [0_u8; 1024];
                let _ = socket.read(&mut request).await;
                let response = format!(
                    "HTTP/1.1 302 Found\r\nLocation: http://{internal_address}/secret\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });
        let redirect_url = format!("http://{redirect_address}/redirect");

        let stream_response = build_qobuz_stream_http_client()
            .unwrap()
            .get(&redirect_url)
            .send()
            .await
            .unwrap();
        let progressive_response = build_qobuz_progressive_stream_http_client()
            .unwrap()
            .get(&redirect_url)
            .send()
            .await
            .unwrap();

        assert_eq!(stream_response.status().as_u16(), 302);
        assert_eq!(progressive_response.status().as_u16(), 302);
        redirect_task.await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), internal.accept())
                .await
                .is_err(),
            "neither Qobuz stream client may fetch the redirect destination"
        );
    }

    #[test]
    fn qobuz_stream_display_name_prefers_artist_title() {
        assert_eq!(
            qobuz_stream_display_name(&play_req(None)),
            "Thom Yorke - Traffic"
        );
    }

    #[test]
    fn cd_download_fallback_is_only_for_hires_streams() {
        let req = play_req(None);
        let hires = StreamUrl {
            url: "https://example.test/hires.flac".to_string(),
            requested_format_id: 7,
            format_id: 7,
            mime_type: "audio/flac".to_string(),
            sampling_rate: Some(44.1),
            bit_depth: Some(24),
        };
        let cd = StreamUrl {
            format_id: 6,
            requested_format_id: 6,
            ..hires.clone()
        };

        assert!(should_retry_cd_after_download_error(&req, &hires));
        assert!(!should_retry_cd_after_download_error(&req, &cd));
        assert!(!should_retry_cd_after_download_error(
            &play_req(Some(6)),
            &hires
        ));
    }

    #[test]
    fn qobuz_cover_url_validation_requires_https_qobuz_hosts() {
        assert!(
            validate_qobuz_cover_url("https://static.qobuz.com/images/covers/ab/cd/cover.jpg")
                .is_ok()
        );
        assert!(validate_qobuz_cover_url("https://play.qobuz.com/cover.jpg").is_ok());

        assert!(validate_qobuz_cover_url("http://static.qobuz.com/cover.jpg").is_err());
        assert!(validate_qobuz_cover_url("https://127.0.0.1/cover.jpg").is_err());
        assert!(validate_qobuz_cover_url("https://qobuz.com.evil.test/cover.jpg").is_err());
    }

    #[test]
    fn qobuz_cover_content_type_validation_only_allows_images() {
        assert!(is_image_content_type("image/jpeg"));
        assert!(is_image_content_type("IMAGE/PNG; charset=binary"));
        assert!(is_image_content_type("image/webp"));

        assert!(!is_image_content_type("text/plain"));
        assert!(!is_image_content_type("application/octet-stream"));
    }

    #[test]
    fn parse_track_in_album_keeps_disc_position_and_version_title() {
        let album = QobuzAlbum {
            id: "abc123".to_string(),
            title: "Kid A Mnesia".to_string(),
            artist: "Radiohead".to_string(),
            artist_id: None,
            image_url: None,
            release_date: None,
            year: None,
            tracks_count: None,
            duration: None,
            maximum_sampling_rate: Some(44.1),
            maximum_bit_depth: Some(16),
            hires: false,
            genre: None,
            genre_id: None,
            label: None,
            release_type: None,
            version: None,
            description: None,
            upc: None,
        };
        let item = json!({
            "id": 42,
            "title": "Like Spinning Plates",
            "version": "'Why Us?' Version",
            "track_number": 3,
            "media_number": 3,
            "duration": 304
        });

        let track = parse_track_in_album(&item, &album).unwrap();

        assert_eq!(track.title, "Like Spinning Plates ('Why Us?' Version)");
        assert_eq!(track.track_number, Some(3));
        assert_eq!(track.disc_number, Some(3));
        assert_eq!(track.id, 42);
    }

    #[test]
    fn parse_album_keeps_qobuz_version_label() {
        let item = json!({
            "id": "yntirca1wv5oc",
            "title": "Dots And Loops",
            "artist": { "id": 322106, "name": "Stereolab" },
            "version": "Expanded Edition",
            "tracks_count": 24
        });

        let album = parse_album(&item).unwrap();

        assert_eq!(album.version.as_deref(), Some("Expanded Edition"));
    }

    #[test]
    fn qobuz_album_detail_cover_standardization_updates_album_and_tracks() {
        let mut detail = album_detail_for_cache("kid-a");
        detail.album.image_url =
            Some("https://static.qobuz.com/images/covers/ab/cd/kid-a_org.jpg".to_string());
        detail.tracks.push(QobuzTrack {
            id: 7,
            title: "Everything In Its Right Place".to_string(),
            artist: "Radiohead".to_string(),
            artist_id: None,
            album: "Kid A".to_string(),
            album_id: Some("kid-a".to_string()),
            track_number: Some(1),
            disc_number: Some(1),
            duration: 251,
            image_url: Some(
                "https://static.qobuz.com/images/covers/ab/cd/kid-a_max.jpg".to_string(),
            ),
            maximum_sampling_rate: None,
            maximum_bit_depth: None,
            hires: false,
            streamable: true,
            composer: None,
            work: None,
            isrc: None,
            copyright: None,
            performers_raw: None,
            credits: Vec::new(),
            play_count: 0,
            last_played_at: None,
            listened_secs: 0.0,
        });

        standardize_qobuz_album_detail_covers(&mut detail);

        assert_eq!(
            detail.album.image_url.as_deref(),
            Some("https://static.qobuz.com/images/covers/ab/cd/kid-a_600.jpg")
        );
        assert_eq!(
            detail.tracks[0].image_url.as_deref(),
            Some("https://static.qobuz.com/images/covers/ab/cd/kid-a_600.jpg")
        );
    }

    #[test]
    fn parse_album_keeps_qobuz_api_cover_resolution() {
        let item = json!({
            "id": "yntirca1wv5oc",
            "title": "Dots And Loops",
            "artist": { "id": 322106, "name": "Stereolab" },
            "image": {
                "large": "https://static.qobuz.com/images/covers/ab/cd/example_600.jpg"
            }
        });

        let album = parse_album(&item).unwrap();

        assert_eq!(
            album.image_url.as_deref(),
            Some("https://static.qobuz.com/images/covers/ab/cd/example_600.jpg")
        );
    }

    #[test]
    fn parse_track_keeps_qobuz_api_cover_resolution() {
        let item = json!({
            "id": 7,
            "title": "Everything In Its Right Place",
            "album": {
                "title": "Kid A",
                "id": "kid-a",
                "image": {
                    "large": "https://static.qobuz.com/images/covers/ef/gh/kid-a_600.jpg"
                }
            },
            "performer": { "name": "Radiohead" }
        });

        let track = parse_track(&item).unwrap();

        assert_eq!(
            track.image_url.as_deref(),
            Some("https://static.qobuz.com/images/covers/ef/gh/kid-a_600.jpg")
        );
    }

    #[test]
    fn parse_track_accepts_string_position_fields() {
        let item = json!({
            "id": 7,
            "title": "Everything In Its Right Place",
            "track_number": "1",
            "media_number": "2",
            "album": { "title": "Kid A", "id": "kid-a" },
            "performer": { "name": "Radiohead" }
        });

        let track = parse_track(&item).unwrap();

        assert_eq!(track.track_number, Some(1));
        assert_eq!(track.disc_number, Some(2));
    }

    #[test]
    fn parse_track_reads_qobuz_credit_fields() {
        let item = json!({
            "id": 7,
            "title": "Everything In Its Right Place",
            "album": { "title": "Kid A", "id": "kid-a" },
            "performer": { "name": "Radiohead" },
            "composer": { "name": "Colin Greenwood" },
            "work": "Kid A Suite",
            "isrc": "GBAYE0000815",
            "copyright": "2000 XL Recordings",
            "performers": "Radiohead, MainArtist, AssociatedPerformer - Nigel Godrich, Producer, Mixer - Colin Greenwood, Composer, Writer"
        });

        let track = parse_track(&item).unwrap();

        assert_eq!(track.composer.as_deref(), Some("Colin Greenwood"));
        assert_eq!(track.work.as_deref(), Some("Kid A Suite"));
        assert_eq!(track.isrc.as_deref(), Some("GBAYE0000815"));
        assert_eq!(track.credits.len(), 3);
        assert_eq!(track.credits[1].name, "Nigel Godrich");
        assert_eq!(
            track.credits[1].roles,
            vec!["Producer".to_string(), "Mixer".to_string()]
        );
    }

    #[test]
    fn parse_performers_merges_duplicate_names_and_ignores_malformed_parts() {
        let credits = parse_qobuz_performers(
            "Q, MainArtist - Q, Producer, Mixing Engineer - Broken Part - Steven Marsden, ComposerLyricist, Writer",
        );

        let q = credits.iter().find(|c| c.name == "Q").unwrap();
        assert_eq!(
            q.roles,
            vec![
                "MainArtist".to_string(),
                "Producer".to_string(),
                "Mixing Engineer".to_string()
            ]
        );
        let writer = credits.iter().find(|c| c.name == "Steven Marsden").unwrap();
        assert_eq!(
            writer.roles,
            vec!["ComposerLyricist".to_string(), "Writer".to_string()]
        );
        assert_eq!(credits.len(), 2);
    }

    #[test]
    fn old_qobuz_track_json_deserializes_without_credit_fields() {
        let track: QobuzTrack = serde_json::from_value(json!({
            "id": 7,
            "title": "Everything In Its Right Place",
            "artist": "Radiohead",
            "album": "Kid A",
            "album_id": "kid-a",
            "duration": 251,
            "hires": false,
            "streamable": true
        }))
        .unwrap();

        assert!(track.artist_id.is_none());
        assert!(track.composer.is_none());
        assert!(track.credits.is_empty());
    }

    #[test]
    fn radio_suggest_body_uses_seed_exclusions_and_limit() {
        let body = radio_suggest_body(23929516, &[23929516, 256316240], 600);

        assert_eq!(
            body["track_to_analysed"],
            json!([{
                "track_id": 23929516,
                "artist_id": 0,
                "genre_id": 0,
                "label_id": 0
            }])
        );
        assert_eq!(body["listened_tracks_ids"], json!([23929516, 256316240]));
        assert_eq!(body["limit"], 500);
    }

    #[test]
    fn radio_artist_candidates_prefer_exact_match() {
        let artists = vec![
            QobuzArtist {
                id: 1,
                name: "The Smile".to_string(),
                image_url: None,
                genre: None,
                albums_count: None,
                biography: None,
            },
            QobuzArtist {
                id: 2,
                name: "Radiohead".to_string(),
                image_url: None,
                genre: None,
                albums_count: None,
                biography: None,
            },
        ];

        assert_eq!(
            radio_artist_candidates_from_search("radiohead", &artists),
            vec![(2, "Radiohead".to_string())]
        );
    }

    #[test]
    fn radio_artist_candidates_fallback_to_first_result_and_handle_empty() {
        let artists = vec![QobuzArtist {
            id: 7,
            name: "Radiohead Tribute Band".to_string(),
            image_url: None,
            genre: None,
            albums_count: None,
            biography: None,
        }];

        assert_eq!(
            radio_artist_candidates_from_search("Radiohead", &artists),
            vec![(7, "Radiohead Tribute Band".to_string())]
        );
        assert!(radio_artist_candidates_from_search("Radiohead", &[]).is_empty());
    }

    #[test]
    fn radio_recommendation_skips_excluded_and_unstreamable_tracks() {
        let response = json!({
            "algorithm": "dynamic-suggest",
            "tracks": {
                "items": [
                    {
                        "id": 1,
                        "title": "Excluded",
                        "streamable": true,
                        "album": { "title": "Album", "id": "a", "image": { "large": "https://example.test/a.jpg" } },
                        "performer": { "name": "Artist" }
                    },
                    {
                        "id": 2,
                        "title": "Unavailable",
                        "streamable": false,
                        "album": { "title": "Album", "id": "a" },
                        "performer": { "name": "Artist" }
                    },
                    {
                        "id": 3,
                        "title": "Winner",
                        "duration": 245,
                        "streamable": true,
                        "album": { "title": "Album", "id": "a" },
                        "performer": { "name": "Artist" }
                    }
                ]
            }
        });

        let recommendation = parse_radio_recommendation(&response, &[1]).unwrap();

        assert_eq!(recommendation.track.id, 3);
        assert_eq!(recommendation.track.title, "Winner");
        assert_eq!(recommendation.algorithm.as_deref(), Some("dynamic-suggest"));
    }

    #[test]
    fn radio_recommendation_accepts_radio_track_array_shape() {
        let response = json!({
            "algorithm": "track-radio",
            "tracks": [
                {
                    "id": "42",
                    "title": "Radio Winner",
                    "duration": 190,
                    "streamable": true,
                    "album": { "title": "Album", "id": "a" },
                    "performer": { "id": "99", "name": "Artist" }
                }
            ]
        });

        let recommendation = parse_radio_recommendation(&response, &[]).unwrap();

        assert_eq!(recommendation.track.id, 42);
        assert_eq!(recommendation.track.artist_id, Some(99));
        assert_eq!(recommendation.track.title, "Radio Winner");
        assert_eq!(recommendation.algorithm.as_deref(), Some("track-radio"));
    }

    #[test]
    fn radio_recommendation_returns_none_for_empty_or_fully_filtered_response() {
        let response = json!({
            "tracks": {
                "items": [
                    {
                        "id": 1,
                        "title": "Only Track",
                        "streamable": true,
                        "album": { "title": "Album", "id": "a" },
                        "performer": { "name": "Artist" }
                    }
                ]
            }
        });

        assert!(parse_radio_recommendation(&response, &[1]).is_none());
        assert!(parse_radio_recommendation(&json!({ "tracks": { "items": [] } }), &[]).is_none());
    }

    #[test]
    fn qobuz_playlist_parses_featured_summary_payloads() {
        let response = json!({
            "playlists": {
                "items": [
                    {
                        "id": "pl-1",
                        "name": "Qobuzissime Jazz",
                        "description": "Editor picks",
                        "owner": { "name": "Qobuz" },
                        "image_rectangle": ["https://static.qobuz.com/images/playlists/pl-1-rectangle.jpg"],
                        "image": {
                            "large": "https://static.qobuz.com/images/playlists/pl-1-large.jpg",
                            "extralarge": "https://static.qobuz.com/images/playlists/pl-1-xl.jpg"
                        },
                        "tracks_count": "24",
                        "duration": 7200
                    },
                    {
                        "id": "pl-1",
                        "name": "Duplicate"
                    }
                ]
            }
        });

        let playlists = playlists_from_featured_response(&response);

        assert_eq!(playlists.len(), 1);
        assert_eq!(playlists[0].id, "pl-1");
        assert_eq!(playlists[0].title, "Qobuzissime Jazz");
        assert_eq!(playlists[0].owner.as_deref(), Some("Qobuz"));
        assert_eq!(playlists[0].tracks_count, Some(24));
        assert_eq!(
            playlists[0].image_url.as_deref(),
            Some("https://static.qobuz.com/images/playlists/pl-1-rectangle.jpg")
        );
    }

    #[test]
    fn qobuz_playlist_featured_response_reads_container_total() {
        let response = json!({
            "playlists": {
                "total": 42,
                "items": [
                    { "id": "pl-1", "name": "One" },
                    { "id": "pl-2", "name": "Two" }
                ]
            }
        });

        let page = featured_playlists_response_from_featured_response(&response, 12, 24);

        assert_eq!(page.count, 2);
        assert_eq!(page.total, Some(42));
        assert!(page.has_more);
    }

    #[test]
    fn qobuz_playlist_featured_response_reads_root_string_total() {
        let response = json!({
            "total": "3",
            "items": [
                { "id": "pl-1", "name": "One" },
                { "id": "pl-2", "name": "Two" }
            ]
        });

        let page = featured_playlists_response_from_featured_response(&response, 2, 2);

        assert_eq!(page.total, Some(3));
        assert!(!page.has_more);
    }

    #[test]
    fn qobuz_playlist_featured_response_falls_back_without_total() {
        let full_response = json!({
            "playlists": {
                "items": [
                    { "id": "pl-1", "name": "One" },
                    { "id": "pl-2", "name": "Two" }
                ]
            }
        });
        let short_response = json!({
            "playlists": {
                "items": [
                    { "id": "pl-1", "name": "One" }
                ]
            }
        });

        let full_page = featured_playlists_response_from_featured_response(&full_response, 2, 0);
        let short_page = featured_playlists_response_from_featured_response(&short_response, 2, 2);

        assert_eq!(full_page.total, None);
        assert!(full_page.has_more);
        assert_eq!(short_page.total, None);
        assert!(!short_page.has_more);
    }

    #[test]
    fn qobuz_album_page_response_reads_true_album_totals() {
        let response = json!({
            "albums": {
                "maximum_items": 42,
                "items": [
                    { "id": "album-1", "title": "One", "artist": { "name": "Artist" } },
                    { "id": "album-2", "title": "Two", "artist": { "name": "Artist" } }
                ]
            }
        });

        let page = album_page_response_from_home_response(&response, 28, 28);

        assert_eq!(page.limit, 28);
        assert_eq!(page.count, 2);
        assert_eq!(page.total, Some(42));
        assert!(page.has_more);
    }

    #[test]
    fn qobuz_album_page_response_does_not_treat_count_as_total() {
        let response = json!({
            "albums": {
                "count": 2,
                "items": [
                    { "id": "album-1", "title": "One", "artist": { "name": "Artist" } },
                    { "id": "album-2", "title": "Two", "artist": { "name": "Artist" } }
                ]
            }
        });

        let page = album_page_response_from_home_response(&response, 2, 2);

        assert_eq!(page.total, None);
        assert!(page.has_more);
    }

    #[test]
    fn qobuz_album_page_response_uses_raw_page_count_for_has_more_fallback() {
        let response = json!({
            "albums": {
                "items": [
                    { "id": "album-1", "title": "One", "artist": { "name": "Artist" } },
                    { "id": "album-1", "title": "One again", "artist": { "name": "Artist" } }
                ]
            }
        });

        let page = album_page_response_from_home_response(&response, 2, 0);

        assert_eq!(page.count, 1);
        assert_eq!(page.total, None);
        assert!(page.has_more);
    }

    #[test]
    fn qobuz_home_category_aliases_match_album_section_contract() {
        assert_eq!(normalize_home_category("new-releases"), "new");
        assert_eq!(normalize_home_category("most-streamed"), "popular");
        assert_eq!(normalize_home_category("press-awards"), "acclaimed");
        assert_eq!(normalize_home_category("qobuzissims"), "standouts");
    }

    #[test]
    fn qobuz_home_section_limit_allows_expanded_pages() {
        assert_eq!(28_u32.clamp(1, QOBUZ_HOME_SECTION_MAX_LIMIT), 28);
        assert_eq!(QOBUZ_HOME_SHELF_LIMIT, 12);
    }

    #[test]
    fn qobuz_playlist_detail_accepts_wrapped_and_direct_tracks() {
        let response = json!({
            "playlist": {
                "id": "pl-1",
                "name": "Qobuz Playlist"
            },
            "tracks": {
                "items": [
                    {
                        "track": {
                            "id": 11,
                            "title": "Wrapped",
                            "duration": 180,
                            "streamable": true,
                            "album": { "title": "Album A", "id": "a" },
                            "performer": { "name": "Artist A" }
                        }
                    },
                    {
                        "id": "12",
                        "title": "Direct",
                        "duration": 200,
                        "streamable": true,
                        "album": { "title": "Album B", "id": "b" },
                        "performer": { "name": "Artist B" }
                    }
                ]
            }
        });

        let playlist = parse_playlist(&response["playlist"]).unwrap();
        let tracks = tracks_from_playlist_response(&response);

        assert_eq!(playlist.title, "Qobuz Playlist");
        assert_eq!(
            tracks.iter().map(|track| track.id).collect::<Vec<_>>(),
            vec![11, 12]
        );
        assert_eq!(tracks[0].album_id.as_deref(), Some("a"));
        assert_eq!(tracks[1].artist, "Artist B");
    }

    #[test]
    fn qobuz_playlist_tags_parse_localized_names() {
        let response = json!({
            "tags": [
                {
                    "slug": "hi-res",
                    "name_json": "{\"fr\":\"Hi-Res\",\"en\":\"Hi-Res\",\"de\":\"Hi-Res\"}"
                },
                {
                    "slug": "qobuz-digs",
                    "name": { "en": "Qobuz Digs", "fr": "Qobuz Digs" }
                }
            ]
        });

        let tags = playlist_tags_from_response(&response);

        assert_eq!(tags.len(), 2);
        assert_eq!(tags[0].id, "hi-res");
        assert_eq!(tags[0].label, "Hi-Res");
        assert_eq!(tags[1].id, "qobuz-digs");
        assert_eq!(tags[1].label, "Qobuz Digs");
    }

    #[test]
    fn qobuz_genres_parse_nested_genre_payloads() {
        let response = json!({
            "genres": {
                "items": [
                    {
                        "id": "64",
                        "name": "Jazz",
                        "children": [
                            { "id": 641, "name": "Vocal Jazz" }
                        ]
                    },
                    { "genre_id": 44, "label": "Electronic" }
                ]
            }
        });

        let genres = genres_from_response(&response);

        assert_eq!(
            genres
                .iter()
                .map(|genre| (genre.id, genre.label.as_str(), genre.parent_id))
                .collect::<Vec<_>>(),
            vec![
                (64, "Jazz", None),
                (641, "Vocal Jazz", Some(64)),
                (44, "Electronic", None)
            ]
        );
    }

    #[test]
    fn qobuz_home_response_serializes_playlist_sections() {
        let response = QobuzHomeResponse {
            logged_in: false,
            sections: vec![QobuzHomeSection {
                id: "editorial-playlists".to_string(),
                title: "Qobuz playlists".to_string(),
                subtitle: Some("Curated listening from Qobuz editors".to_string()),
                item_type: "playlist".to_string(),
                albums: Vec::new(),
                artists: Vec::new(),
                playlists: vec![QobuzPlaylist {
                    id: "pl-1".to_string(),
                    title: "Qobuzissime".to_string(),
                    description: None,
                    owner: Some("Qobuz".to_string()),
                    image_url: None,
                    tracks_count: Some(18),
                    duration: None,
                    updated_at: None,
                }],
            }],
            partial_errors: Vec::new(),
        };

        let serialized = serde_json::to_value(response).unwrap();

        assert_eq!(serialized["sections"][0]["item_type"], "playlist");
        assert_eq!(serialized["sections"][0]["playlists"][0]["id"], "pl-1");
        assert_eq!(
            serialized["sections"][0]["playlists"][0]["tracks_count"],
            18
        );
    }
}
