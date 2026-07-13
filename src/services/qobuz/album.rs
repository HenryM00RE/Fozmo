use super::QobuzService;
use super::cache::normalize_qobuz_album_cache_key;
use super::model::{QobuzAlbum, QobuzAlbumDetail, QobuzAlbumSearchResponse, QobuzTrack};
use super::parser::{merge_qobuz_track_detail, parse_album, parse_track_in_album};
use futures_util::StreamExt;
use futures_util::stream as futures_stream;
use serde_json::Value;
use std::collections::HashSet;

const QOBUZ_FAVORITE_ALBUM_PAGE_LIMIT: u32 = 500;
const QOBUZ_FAVORITE_ALBUM_MAX_ITEMS: u32 = 10_000;

impl QobuzService {
    pub async fn favorite_albums(&self) -> Result<QobuzAlbumSearchResponse, String> {
        let mut albums = Vec::new();
        let mut seen = HashSet::new();
        let mut offset = 0_u32;

        while offset < QOBUZ_FAVORITE_ALBUM_MAX_ITEMS {
            let json = self
                .authenticated_get_value(
                    "/favorite/getUserFavorites",
                    vec![
                        ("type", "albums".to_string()),
                        ("limit", QOBUZ_FAVORITE_ALBUM_PAGE_LIMIT.to_string()),
                        ("offset", offset.to_string()),
                    ],
                )
                .await?;
            let total = json
                .get("albums")
                .and_then(|value| value.get("total"))
                .and_then(Value::as_u64)
                .map(|value| value as u32);
            let page = favorite_album_items(&json);
            let page_len = page.len() as u32;
            for album in page {
                if seen.insert(album.id.clone()) {
                    albums.push(album);
                }
            }

            if page_len == 0 {
                break;
            }
            offset = offset.saturating_add(page_len);
            if total.is_some_and(|total| offset >= total)
                || page_len < QOBUZ_FAVORITE_ALBUM_PAGE_LIMIT
            {
                break;
            }
        }

        Ok(QobuzAlbumSearchResponse { albums })
    }

    pub(super) async fn fetch_album_detail(
        &self,
        album_id: &str,
    ) -> Result<QobuzAlbumDetail, String> {
        let mut detail = self.fetch_album_detail_basic(album_id).await?;
        detail.tracks = self.enrich_album_tracks(detail.tracks).await;
        Ok(detail)
    }

    /// Fetches the album and its nested track list with a single Qobuz request.
    /// Matching and linking do not need the per-track credit enrichment performed
    /// by `fetch_album_detail`.
    pub async fn album_detail_basic(&self, album_id: &str) -> Result<QobuzAlbumDetail, String> {
        let album_id = normalize_qobuz_album_cache_key(album_id);
        if album_id.is_empty() {
            return Err("Qobuz album id is required".to_string());
        }
        if let Some(detail) = self.cached_album_detail(&album_id, true).await {
            return Ok(detail);
        }
        self.fetch_album_detail_basic(&album_id).await
    }

    async fn fetch_album_detail_basic(&self, album_id: &str) -> Result<QobuzAlbumDetail, String> {
        let json = self
            .optional_get_value(
                "/album/get",
                vec![("album_id", album_id.to_string())],
                "Qobuz album get failed",
                "Qobuz album get response was not JSON",
                "Qobuz album get failed",
            )
            .await?;

        // /album/get returns the album at the root, with `tracks.items[]` nested.
        let album = parse_album(&json)
            .ok_or_else(|| "Qobuz album response missing required fields".to_string())?;
        let tracks: Vec<QobuzTrack> = json
            .get("tracks")
            .and_then(|t| t.get("items"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            // The track items inside /album/get don't carry album metadata themselves
            // (it's at the parent level), so synthesise it for parse_track_with_album.
            .filter_map(|item| parse_track_in_album(item, &album))
            .collect();
        Ok(QobuzAlbumDetail { album, tracks })
    }

    async fn enrich_album_tracks(&self, tracks: Vec<QobuzTrack>) -> Vec<QobuzTrack> {
        futures_stream::iter(tracks)
            .map(|track| async move {
                match self.track_detail(track.id).await {
                    Ok(enriched) => merge_qobuz_track_detail(track, enriched),
                    Err(err) => {
                        eprintln!("qobuz: track/get({}) credits failed: {err}", track.id);
                        track
                    }
                }
            })
            .buffer_unordered(6)
            .collect()
            .await
    }
}

fn favorite_album_items(json: &Value) -> Vec<QobuzAlbum> {
    json.get("albums")
        .and_then(|albums| albums.get("items"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(parse_album)
        .collect()
}
