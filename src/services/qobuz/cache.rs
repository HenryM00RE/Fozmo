use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::parser::standardize_qobuz_album_detail_covers;
use super::{
    QobuzAlbumDetail, QobuzArtistImageResponse, QobuzCacheInfo, QobuzHomeResponse, QobuzService,
    QobuzTrack, push_album_home_section,
};

pub(super) const ARTIST_DETAIL_TTL: Duration = Duration::from_secs(60 * 60);
const ARTIST_TOP_TRACKS_TTL: Duration = Duration::from_secs(60 * 60 * 24);
const ARTIST_TOP_TRACKS_SCHEMA_VERSION: u32 = 2;
const ARTIST_IMAGE_TTL: Duration = Duration::from_secs(60 * 60 * 24 * 30);
pub(super) const ALBUM_DETAIL_CACHE_TTL: Duration = Duration::from_secs(60 * 60 * 24 * 7);
pub(super) const ALBUM_DETAIL_CACHE_MAX_ENTRIES: usize = 200;
const HOME_CACHE_TTL: Duration = Duration::from_secs(60 * 60);
const HOME_CACHE_SCHEMA_VERSION: u32 = 5;
const COVER_CACHE_TTL: Duration = Duration::from_secs(60 * 60 * 24 * 7);
const HOME_CACHE_FILE: &str = "home.json";
const ALBUM_DETAIL_CACHE_FILE: &str = "album-details.json";
const ARTIST_TOP_TRACKS_CACHE_FILE: &str = "artist-top-tracks.json";
const ARTIST_IMAGES_CACHE_FILE: &str = "artist-images.json";
const COVER_CACHE_DIR: &str = "covers";
const ARTIST_PORTRAIT_CACHE_DIR: &str = "artist-portraits";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct CachedQobuzHome {
    #[serde(default)]
    schema_version: u32,
    stored_at_epoch_secs: u64,
    user_email: Option<String>,
    response: QobuzHomeResponse,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct CachedArtistTopTracks {
    #[serde(default)]
    schema_version: u32,
    stored_at_epoch_secs: u64,
    top_tracks: Vec<QobuzTrack>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct CachedArtistImage {
    stored_at_epoch_secs: u64,
    response: QobuzArtistImageResponse,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct CachedQobuzAlbumDetail {
    pub(super) stored_at_epoch_secs: u64,
    pub(super) user_email: Option<String>,
    pub(super) detail: QobuzAlbumDetail,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedQobuzCoverMeta {
    stored_at_epoch_secs: u64,
    mime: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedQobuzArtistPortraitMeta {
    stored_at_epoch_secs: u64,
    mime: String,
    source_url: String,
}

impl CachedArtistTopTracks {
    fn is_fresh(&self) -> bool {
        self.schema_version == ARTIST_TOP_TRACKS_SCHEMA_VERSION
            && now_epoch_secs().saturating_sub(self.stored_at_epoch_secs)
                < ARTIST_TOP_TRACKS_TTL.as_secs()
    }
}

impl CachedArtistImage {
    fn is_fresh(&self) -> bool {
        now_epoch_secs().saturating_sub(self.stored_at_epoch_secs) < ARTIST_IMAGE_TTL.as_secs()
    }
}

impl CachedQobuzAlbumDetail {
    fn is_fresh(&self) -> bool {
        now_epoch_secs().saturating_sub(self.stored_at_epoch_secs)
            < ALBUM_DETAIL_CACHE_TTL.as_secs()
    }
}

impl CachedQobuzHome {
    fn is_current_schema(&self) -> bool {
        self.schema_version == HOME_CACHE_SCHEMA_VERSION
    }

    fn is_fresh(&self) -> bool {
        now_epoch_secs().saturating_sub(self.stored_at_epoch_secs) < HOME_CACHE_TTL.as_secs()
    }
}

pub(super) fn load_artist_top_tracks_cache_from_disk(
    cache_dir: &Path,
) -> HashMap<u64, CachedArtistTopTracks> {
    let path = cache_dir.join(ARTIST_TOP_TRACKS_CACHE_FILE);
    let Some(content) = std::fs::read_to_string(path).ok() else {
        return HashMap::new();
    };
    serde_json::from_str::<HashMap<u64, CachedArtistTopTracks>>(&content).unwrap_or_default()
}

pub(super) fn load_artist_image_cache_from_disk(
    cache_dir: &Path,
) -> HashMap<String, CachedArtistImage> {
    let path = cache_dir.join(ARTIST_IMAGES_CACHE_FILE);
    let Some(content) = std::fs::read_to_string(path).ok() else {
        return HashMap::new();
    };
    let mut cache =
        serde_json::from_str::<HashMap<String, CachedArtistImage>>(&content).unwrap_or_default();
    cache.retain(|_, cached| cached.is_fresh());
    cache
}

pub(super) fn load_home_cache_from_disk(cache_dir: &Path) -> Option<CachedQobuzHome> {
    let path = cache_dir.join(HOME_CACHE_FILE);
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str::<CachedQobuzHome>(&content).ok()
}

pub(super) fn load_album_detail_cache_from_disk(
    cache_dir: &Path,
) -> HashMap<String, CachedQobuzAlbumDetail> {
    let path = cache_dir.join(ALBUM_DETAIL_CACHE_FILE);
    let Some(content) = std::fs::read_to_string(path).ok() else {
        return HashMap::new();
    };
    let mut cache = serde_json::from_str::<HashMap<String, CachedQobuzAlbumDetail>>(&content)
        .unwrap_or_default();
    prune_album_detail_cache(&mut cache);
    cache
}

pub(super) fn now_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

pub(super) fn normalize_qobuz_album_cache_key(album_id: &str) -> String {
    album_id
        .trim()
        .strip_prefix("qobuz:album:")
        .unwrap_or(album_id.trim())
        .trim()
        .to_string()
}

pub(super) fn prune_album_detail_cache(cache: &mut HashMap<String, CachedQobuzAlbumDetail>) {
    cache.retain(|_, cached| cached.is_fresh());
    if cache.len() <= ALBUM_DETAIL_CACHE_MAX_ENTRIES {
        return;
    }
    let mut entries = cache
        .iter()
        .map(|(key, cached)| (key.clone(), cached.stored_at_epoch_secs))
        .collect::<Vec<_>>();
    entries.sort_by_key(|(_, stored_at)| *stored_at);
    for (key, _) in entries
        .into_iter()
        .take(cache.len().saturating_sub(ALBUM_DETAIL_CACHE_MAX_ENTRIES))
    {
        cache.remove(&key);
    }
}

impl QobuzService {
    pub fn start_home_cache_warmer(self: Arc<Self>) {
        tokio::spawn(async move {
            if let Err(err) = self.refresh_home_cache().await {
                eprintln!("qobuz: initial home cache refresh failed: {err}");
            }
            loop {
                tokio::time::sleep(HOME_CACHE_TTL).await;
                if let Err(err) = self.refresh_home_cache_force().await {
                    eprintln!("qobuz: home cache refresh failed: {err}");
                }
            }
        });
    }

    pub async fn album_detail(&self, album_id: &str) -> Result<QobuzAlbumDetail, String> {
        let album_id = normalize_qobuz_album_cache_key(album_id);
        if let Some(detail) = self.cached_album_detail(&album_id, true).await {
            return Ok(detail);
        }

        match self.fetch_album_detail(&album_id).await {
            Ok(detail) => {
                self.store_album_detail_cache(&album_id, detail.clone())
                    .await;
                Ok(detail)
            }
            Err(err) => {
                if let Some(detail) = self.cached_album_detail(&album_id, false).await {
                    eprintln!("qobuz: serving stale album cache for {album_id}: {err}");
                    return Ok(detail);
                }
                Err(err)
            }
        }
    }

    pub async fn warm_album_detail_cache(&self, album_id: &str) -> Result<(), String> {
        let album_id = normalize_qobuz_album_cache_key(album_id);
        if album_id.is_empty() || self.cached_album_detail(&album_id, true).await.is_some() {
            return Ok(());
        }
        self.album_detail(&album_id).await.map(|_| ())
    }

    pub async fn cached_cover_public(
        &self,
        url: &str,
    ) -> Result<(Option<String>, Option<Vec<u8>>), String> {
        let key = qobuz_cover_cache_key(url);
        if let Some((mime, data)) = self.cached_cover_from_disk(&key) {
            return Ok((Some(mime), Some(data)));
        }

        let (mime, data) = self.fetch_cover(url).await?;
        if let (Some(mime), Some(data)) = (mime.as_ref(), data.as_ref()) {
            self.store_cover_cache(&key, mime, data);
        }
        Ok((mime, data))
    }

    fn cached_cover_from_disk(&self, key: &str) -> Option<(String, Vec<u8>)> {
        let (meta_path, data_path) = cover_cache_paths(&self.cache_dir, key);
        let meta = std::fs::read_to_string(meta_path).ok()?;
        let meta = serde_json::from_str::<CachedQobuzCoverMeta>(&meta).ok()?;
        if now_epoch_secs().saturating_sub(meta.stored_at_epoch_secs) >= COVER_CACHE_TTL.as_secs() {
            return None;
        }
        let data = std::fs::read(data_path).ok()?;
        Some((meta.mime, data))
    }

    pub fn cached_artist_portrait_from_disk(&self, key: &str) -> Option<(String, Vec<u8>)> {
        let (meta_path, data_path) = artist_portrait_cache_paths(&self.cache_dir, key);
        let meta = std::fs::read_to_string(meta_path).ok()?;
        let meta = serde_json::from_str::<CachedQobuzArtistPortraitMeta>(&meta).ok()?;
        if now_epoch_secs().saturating_sub(meta.stored_at_epoch_secs) >= ARTIST_IMAGE_TTL.as_secs()
        {
            return None;
        }
        let data = std::fs::read(data_path).ok()?;
        Some((meta.mime, data))
    }

    pub async fn cached_artist_portrait_public(
        &self,
        key: &str,
        source_url: &str,
    ) -> Result<(String, Vec<u8>), String> {
        if let Some((mime, data)) =
            self.cached_artist_portrait_from_disk_for_source(key, source_url)
        {
            return Ok((mime, data));
        }

        let (mime, data) = self.fetch_cover(source_url).await?;
        let (Some(mime), Some(data)) = (mime, data) else {
            return Err("Qobuz artist portrait response was empty".to_string());
        };
        self.store_artist_portrait_cache(key, source_url, &mime, &data);
        Ok((mime, data))
    }

    fn cached_artist_portrait_from_disk_for_source(
        &self,
        key: &str,
        source_url: &str,
    ) -> Option<(String, Vec<u8>)> {
        let (meta_path, data_path) = artist_portrait_cache_paths(&self.cache_dir, key);
        let meta = std::fs::read_to_string(meta_path).ok()?;
        let meta = serde_json::from_str::<CachedQobuzArtistPortraitMeta>(&meta).ok()?;
        if meta.source_url != source_url {
            return None;
        }
        if now_epoch_secs().saturating_sub(meta.stored_at_epoch_secs) >= ARTIST_IMAGE_TTL.as_secs()
        {
            return None;
        }
        let data = std::fs::read(data_path).ok()?;
        Some((meta.mime, data))
    }

    fn store_artist_portrait_cache(&self, key: &str, source_url: &str, mime: &str, data: &[u8]) {
        let (meta_path, data_path) = artist_portrait_cache_paths(&self.cache_dir, key);
        let Some(dir) = data_path.parent() else {
            return;
        };
        if let Err(e) = std::fs::create_dir_all(dir) {
            eprintln!("qobuz: failed to create artist portrait cache directory: {e}");
            return;
        }
        if let Err(e) = std::fs::write(&data_path, data) {
            eprintln!("qobuz: failed to write artist portrait cache: {e}");
            return;
        }
        let meta = CachedQobuzArtistPortraitMeta {
            stored_at_epoch_secs: now_epoch_secs(),
            mime: mime.to_string(),
            source_url: source_url.to_string(),
        };
        match serde_json::to_string_pretty(&meta) {
            Ok(json) => {
                if let Err(e) = std::fs::write(meta_path, json) {
                    eprintln!("qobuz: failed to write artist portrait cache metadata: {e}");
                }
            }
            Err(e) => eprintln!("qobuz: failed to serialize artist portrait cache metadata: {e}"),
        }
    }

    fn store_cover_cache(&self, key: &str, mime: &str, data: &[u8]) {
        let (meta_path, data_path) = cover_cache_paths(&self.cache_dir, key);
        let Some(dir) = data_path.parent() else {
            return;
        };
        if let Err(e) = std::fs::create_dir_all(dir) {
            eprintln!("qobuz: failed to create cover cache directory: {e}");
            return;
        }
        if let Err(e) = std::fs::write(&data_path, data) {
            eprintln!("qobuz: failed to write cover cache: {e}");
            return;
        }
        let meta = CachedQobuzCoverMeta {
            stored_at_epoch_secs: now_epoch_secs(),
            mime: mime.to_string(),
        };
        match serde_json::to_string_pretty(&meta) {
            Ok(json) => {
                if let Err(e) = std::fs::write(meta_path, json) {
                    eprintln!("qobuz: failed to write cover cache metadata: {e}");
                }
            }
            Err(e) => eprintln!("qobuz: failed to serialize cover cache metadata: {e}"),
        }
    }

    pub(super) async fn cached_album_detail(
        &self,
        album_id: &str,
        require_fresh: bool,
    ) -> Option<QobuzAlbumDetail> {
        let user_email = self.current_home_cache_user().await;
        let cache = self.album_detail_cache.read().await;
        let cached = cache.get(album_id)?;
        if cached.user_email != user_email {
            return None;
        }
        if require_fresh && !cached.is_fresh() {
            return None;
        }
        let mut detail = cached.detail.clone();
        standardize_qobuz_album_detail_covers(&mut detail);
        Some(detail)
    }

    async fn store_album_detail_cache(&self, album_id: &str, mut detail: QobuzAlbumDetail) {
        standardize_qobuz_album_detail_covers(&mut detail);
        let user_email = self.current_home_cache_user().await;
        let snapshot = {
            let mut cache = self.album_detail_cache.write().await;
            cache.insert(
                album_id.to_string(),
                CachedQobuzAlbumDetail {
                    stored_at_epoch_secs: now_epoch_secs(),
                    user_email,
                    detail,
                },
            );
            prune_album_detail_cache(&mut cache);
            cache.clone()
        };

        let cache_dir = self.cache_dir.clone();
        tokio::task::spawn_blocking(move || {
            if let Err(e) = std::fs::create_dir_all(&cache_dir) {
                eprintln!("qobuz: failed to create album detail cache directory: {e}");
                return;
            }
            let path = cache_dir.join(ALBUM_DETAIL_CACHE_FILE);
            match serde_json::to_string_pretty(&snapshot) {
                Ok(json) => {
                    if let Err(e) = std::fs::write(path, json) {
                        eprintln!("qobuz: failed to write album detail cache: {e}");
                    }
                }
                Err(e) => eprintln!("qobuz: failed to serialize album detail cache: {e}"),
            }
        });
    }

    pub async fn home(&self) -> Result<QobuzHomeResponse, String> {
        if let Some(response) = self.cached_home_response(false).await {
            return Ok(response);
        }

        self.refresh_home_cache().await
    }

    pub async fn home_album_of_the_week(&self) -> Result<QobuzHomeResponse, String> {
        if let Some(response) = self.cached_home_response(false).await {
            let logged_in = response.logged_in;
            let partial_errors = response.partial_errors.clone();
            let sections = response
                .sections
                .into_iter()
                .filter(|section| section.id == "album-of-the-week")
                .collect::<Vec<_>>();
            if !sections.is_empty() {
                return Ok(QobuzHomeResponse {
                    logged_in,
                    sections,
                    partial_errors,
                });
            }
        }

        let logged_in = self.session.read().await.is_some();
        let mut sections = Vec::new();
        let mut partial_errors = Vec::new();
        let mut discovery_albums = Vec::new();

        if logged_in {
            match self
                .discover_albums("/discover/albumOfTheWeek", 6, 0, None)
                .await
            {
                Ok(albums) => push_album_home_section(
                    &mut sections,
                    &mut discovery_albums,
                    "album-of-the-week",
                    "Albums of the week",
                    Some("Qobuz editorial selection"),
                    albums,
                ),
                Err(err) => partial_errors.push(format!("album-of-the-week: {err}")),
            }
        }

        Ok(QobuzHomeResponse {
            logged_in,
            sections,
            partial_errors,
        })
    }

    async fn refresh_home_cache(&self) -> Result<QobuzHomeResponse, String> {
        let _refresh = self.home_cache_refresh.lock().await;
        let user_email = self.current_home_cache_user().await;
        if let Some(response) = self.cached_home_response_for_user(true, &user_email).await {
            return Ok(response);
        }

        let response = self.fetch_home().await?;
        self.store_home_cache(response.clone(), user_email).await;
        Ok(response)
    }

    async fn refresh_home_cache_force(&self) -> Result<QobuzHomeResponse, String> {
        let _refresh = self.home_cache_refresh.lock().await;
        let user_email = self.current_home_cache_user().await;
        let response = self.fetch_home().await?;
        self.store_home_cache(response.clone(), user_email).await;
        Ok(response)
    }

    async fn cached_home_response(&self, require_fresh: bool) -> Option<QobuzHomeResponse> {
        let user_email = self.current_home_cache_user().await;
        self.cached_home_response_for_user(require_fresh, &user_email)
            .await
    }

    async fn cached_home_response_for_user(
        &self,
        require_fresh: bool,
        user_email: &Option<String>,
    ) -> Option<QobuzHomeResponse> {
        let cache = self.home_cache.read().await;
        let cached = cache.as_ref()?;
        if &cached.user_email != user_email {
            return None;
        }
        if require_fresh {
            if !cached.is_current_schema() {
                return None;
            }
            if !cached.is_fresh() {
                return None;
            }
        }
        Some(cached.response.clone())
    }

    async fn current_home_cache_user(&self) -> Option<String> {
        self.session
            .read()
            .await
            .as_ref()
            .map(|session| session.user.email.clone())
    }

    async fn store_home_cache(&self, response: QobuzHomeResponse, user_email: Option<String>) {
        if self.current_home_cache_user().await != user_email {
            return;
        }
        let cached = CachedQobuzHome {
            schema_version: HOME_CACHE_SCHEMA_VERSION,
            stored_at_epoch_secs: now_epoch_secs(),
            user_email,
            response,
        };
        *self.home_cache.write().await = Some(cached.clone());

        if let Err(e) = std::fs::create_dir_all(&self.cache_dir) {
            eprintln!("qobuz: failed to create home cache directory: {e}");
            return;
        }
        let path = self.cache_dir.join(HOME_CACHE_FILE);
        match serde_json::to_string_pretty(&cached) {
            Ok(json) => {
                if let Err(e) = std::fs::write(path, json) {
                    eprintln!("qobuz: failed to write home cache: {e}");
                }
            }
            Err(e) => eprintln!("qobuz: failed to serialize home cache: {e}"),
        }
    }

    pub(super) async fn clear_home_cache(&self) {
        *self.home_cache.write().await = None;
        let path = self.cache_dir.join(HOME_CACHE_FILE);
        if let Err(e) = std::fs::remove_file(path)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!("qobuz: failed to remove home cache: {e}");
        }
    }

    pub(super) async fn clear_album_detail_cache(&self) {
        self.album_detail_cache.write().await.clear();
        let path = self.cache_dir.join(ALBUM_DETAIL_CACHE_FILE);
        if let Err(e) = std::fs::remove_file(path)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!("qobuz: failed to remove album detail cache: {e}");
        }
    }

    pub(super) async fn cached_artist_top_tracks(&self, artist_id: u64) -> Option<Vec<QobuzTrack>> {
        let cache = self.artist_top_tracks_cache.read().await;
        let cached = cache.get(&artist_id)?;
        if !cached.is_fresh() || cached.top_tracks.is_empty() {
            return None;
        }
        Some(cached.top_tracks.clone())
    }

    pub(super) async fn store_artist_top_tracks(
        &self,
        artist_id: u64,
        top_tracks: Vec<QobuzTrack>,
    ) {
        if top_tracks.is_empty() {
            return;
        }

        let snapshot = {
            let mut cache = self.artist_top_tracks_cache.write().await;
            cache.insert(
                artist_id,
                CachedArtistTopTracks {
                    schema_version: ARTIST_TOP_TRACKS_SCHEMA_VERSION,
                    stored_at_epoch_secs: now_epoch_secs(),
                    top_tracks,
                },
            );
            cache.clone()
        };

        if let Err(e) = std::fs::create_dir_all(&self.cache_dir) {
            eprintln!("qobuz: failed to create artist top tracks cache directory: {e}");
            return;
        }
        let path = self.cache_dir.join(ARTIST_TOP_TRACKS_CACHE_FILE);
        match serde_json::to_string_pretty(&snapshot) {
            Ok(json) => {
                if let Err(e) = std::fs::write(path, json) {
                    eprintln!("qobuz: failed to write artist top tracks cache: {e}");
                }
            }
            Err(e) => eprintln!("qobuz: failed to serialize artist top tracks cache: {e}"),
        }
    }

    pub(super) async fn cached_artist_image(&self, key: &str) -> Option<QobuzArtistImageResponse> {
        let cache = self.artist_image_cache.read().await;
        let cached = cache.get(key)?;
        if !cached.is_fresh() {
            return None;
        }
        Some(cached.response.clone())
    }

    pub(super) async fn store_artist_image(&self, key: &str, response: QobuzArtistImageResponse) {
        let snapshot = {
            let mut cache = self.artist_image_cache.write().await;
            cache.insert(
                key.to_string(),
                CachedArtistImage {
                    stored_at_epoch_secs: now_epoch_secs(),
                    response,
                },
            );
            cache.retain(|_, cached| cached.is_fresh());
            cache.clone()
        };

        if let Err(e) = std::fs::create_dir_all(&self.cache_dir) {
            eprintln!("qobuz: failed to create artist image cache directory: {e}");
            return;
        }
        let path = self.cache_dir.join(ARTIST_IMAGES_CACHE_FILE);
        match serde_json::to_string_pretty(&snapshot) {
            Ok(json) => {
                if let Err(e) = std::fs::write(path, json) {
                    eprintln!("qobuz: failed to write artist image cache: {e}");
                }
            }
            Err(e) => eprintln!("qobuz: failed to serialize artist image cache: {e}"),
        }
    }

    pub fn cache_info(&self) -> QobuzCacheInfo {
        let mut bytes: u64 = 0;
        let mut files: u64 = 0;
        collect_cache_info(&self.cache_dir, &mut bytes, &mut files);
        QobuzCacheInfo { bytes, files }
    }

    pub async fn clear_cache(&self) -> Result<QobuzCacheInfo, String> {
        self.home_cache.write().await.take();
        self.album_detail_cache.write().await.clear();
        self.artist_top_tracks_cache.write().await.clear();
        self.artist_detail_cache.write().await.clear();
        self.artist_image_cache.write().await.clear();

        let mut bytes: u64 = 0;
        let mut files: u64 = 0;
        let entries = match std::fs::read_dir(&self.cache_dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(QobuzCacheInfo { bytes: 0, files: 0 });
            }
            Err(e) => return Err(format!("read Qobuz cache: {e}")),
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let (size, count) = cache_path_info(&path);
            let result = if path.is_dir() {
                std::fs::remove_dir_all(&path)
            } else {
                std::fs::remove_file(&path)
            };
            if let Err(e) = result {
                eprintln!("qobuz: failed to remove cache file {:?}: {}", path, e);
                continue;
            }
            bytes = bytes.saturating_add(size);
            files = files.saturating_add(count);
        }
        Ok(QobuzCacheInfo { bytes, files })
    }
}

fn qobuz_cover_cache_key(url: &str) -> String {
    let digest = Sha256::digest(url.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn cover_cache_paths(cache_dir: &Path, key: &str) -> (PathBuf, PathBuf) {
    let dir = cache_dir.join(COVER_CACHE_DIR);
    (
        dir.join(format!("{key}.json")),
        dir.join(format!("{key}.bin")),
    )
}

fn artist_portrait_cache_paths(cache_dir: &Path, key: &str) -> (PathBuf, PathBuf) {
    let digest = Sha256::digest(key.as_bytes());
    let key = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let dir = cache_dir.join(ARTIST_PORTRAIT_CACHE_DIR);
    (
        dir.join(format!("{key}.json")),
        dir.join(format!("{key}.bin")),
    )
}

fn collect_cache_info(path: &Path, bytes: &mut u64, files: &mut u64) {
    let Ok(entries) = std::fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_cache_info(&path, bytes, files);
        } else if let Ok(meta) = entry.metadata() {
            *bytes = bytes.saturating_add(meta.len());
            *files += 1;
        }
    }
}

fn cache_path_info(path: &Path) -> (u64, u64) {
    if path.is_dir() {
        let mut bytes = 0;
        let mut files = 0;
        collect_cache_info(path, &mut bytes, &mut files);
        (bytes, files)
    } else {
        path.metadata()
            .map(|meta| (meta.len(), 1))
            .unwrap_or((0, 0))
    }
}
