//! Hi-res album cover fetching via the iTunes Search API.
//!
//! Apple's artwork CDN serves far larger covers than Cover Art Archive: the
//! `artworkUrl100` thumbnail URL can be rewritten to request the original
//! upload. Lookups prefer the release barcode (exact, no fuzzy matching);
//! text search is the fallback and only accepted on an exact title+artist
//! and track-count agreement. Fetched art is stored with source `"itunes"`
//! and only ever applied through `canonical_art_id` — file-derived art in
//! `art_id` is never overwritten, so this is always reversible.

use super::matching::normalize_for_match;
use super::*;
use crate::audio::player::TrackCover;
use reqwest::Url;
use reqwest::header::CONTENT_TYPE;
use serde_json::Value;
use std::time::{Duration, Instant};

/// Artwork sizes to request, best first. `100000x100000-999` returns the
/// original upload; the fixed sizes are fallbacks when the CDN rejects it.
const ITUNES_ART_SIZES: &[&str] = &["100000x100000-999", "3000x3000bb", "1200x1200bb"];
const ITUNES_COVER_MAX_BYTES: u64 = 5 * 1024 * 1024;

impl Library {
    /// Try to upgrade an album's cover from the iTunes Store. Uses the
    /// MusicBrainz barcode (or Qobuz UPC) when known, otherwise an exact
    /// title/artist search. The new art only becomes the canonical cover if
    /// it is larger than what the album currently displays.
    pub async fn improve_album_art(&self, album_id: i64) -> Result<Option<AlbumDetail>, String> {
        let Some(album) = self.album(album_id)? else {
            return Ok(None);
        };
        self.improve_album_art_inner(&album).await?;
        self.album_detail(album_id)
    }

    /// Bulk pass over every identified album (MusicBrainz-matched or
    /// Qobuz-linked). Rate-limited per request, so large libraries take a
    /// while; failures on individual albums are logged and skipped.
    pub async fn improve_all_album_art(&self) -> Result<ArtRefreshResult, String> {
        let albums = self.albums()?;
        let mut result = ArtRefreshResult {
            processed: 0,
            improved: 0,
        };
        for album in albums
            .iter()
            .filter(|a| a.match_status == "matched" || a.qobuz_album_id.is_some())
        {
            result.processed += 1;
            match self.improve_album_art_inner(album).await {
                Ok(true) => result.improved += 1,
                Ok(false) => {}
                Err(e) => eprintln!("itunes: art refresh for album {} failed: {e}", album.id),
            }
        }
        Ok(result)
    }

    /// Returns true when the album's canonical art was upgraded.
    async fn improve_album_art_inner(&self, album: &AlbumSummary) -> Result<bool, String> {
        let qobuz = self.qobuz_payload_for_album(album.id)?;
        let barcode = album
            .mb_barcode
            .clone()
            .or_else(|| qobuz.as_ref().and_then(|d| d.album.upc.clone()));
        // Prefer the canonical (Qobuz) title/artist — it's label-sourced and
        // matches what the iTunes catalog uses better than file tags do.
        let title = qobuz
            .as_ref()
            .map(|d| d.album.title.clone())
            .unwrap_or_else(|| album.title.clone());
        let artist = qobuz
            .as_ref()
            .map(|d| d.album.artist.clone())
            .or_else(|| album.album_artist.clone());
        let Some(art_id) = self
            .fetch_itunes_cover(
                barcode.as_deref(),
                artist.as_deref(),
                &title,
                album.track_count,
            )
            .await?
        else {
            return Ok(false);
        };
        self.apply_canonical_art_if_better(album.id, art_id)
    }

    pub(super) async fn fetch_itunes_cover(
        &self,
        barcode: Option<&str>,
        artist: Option<&str>,
        title: &str,
        track_count: i64,
    ) -> Result<Option<i64>, String> {
        let mut hit = None;
        if let Some(barcode) = barcode {
            let digits: String = barcode.chars().filter(char::is_ascii_digit).collect();
            if !digits.is_empty() {
                hit = self.itunes_lookup_by_upc(&digits).await?;
            }
        }
        if hit.is_none() {
            hit = self.itunes_search_album(artist, title, track_count).await?;
        }
        let Some(result) = hit else {
            return Ok(None);
        };
        let Some(url100) = result.get("artworkUrl100").and_then(Value::as_str) else {
            return Ok(None);
        };
        for size in ITUNES_ART_SIZES {
            let Some(url) = upgrade_artwork_url(url100, size) else {
                break;
            };
            if let Some(art_id) = self.download_itunes_image(&url).await? {
                return Ok(Some(art_id));
            }
        }
        Ok(None)
    }

    /// Exact lookup by barcode. No verification needed: the UPC identifies
    /// the release.
    async fn itunes_lookup_by_upc(&self, upc: &str) -> Result<Option<Value>, String> {
        self.wait_itunes_turn().await;
        eprintln!("itunes: upc lookup ({upc})");
        let response = self
            .http
            .get("https://itunes.apple.com/lookup")
            .query(&[("upc", upc), ("entity", "album")])
            .send()
            .await
            .map_err(|e| format!("itunes upc lookup: {e}"))?;
        if !response.status().is_success() {
            return Ok(None);
        }
        let body: Value = response
            .json()
            .await
            .map_err(|e| format!("itunes upc lookup json: {e}"))?;
        Ok(first_collection(&body))
    }

    /// Text search, accepted only on an exact normalized title+artist match
    /// and track-count agreement — a wrong cover is worse than no upgrade.
    async fn itunes_search_album(
        &self,
        artist: Option<&str>,
        title: &str,
        track_count: i64,
    ) -> Result<Option<Value>, String> {
        let term = match artist {
            Some(artist) if !artist.trim().is_empty() => format!("{artist} {title}"),
            _ => title.to_string(),
        };
        self.wait_itunes_turn().await;
        eprintln!("itunes: searching ({term})");
        let response = self
            .http
            .get("https://itunes.apple.com/search")
            .query(&[
                ("term", term.as_str()),
                ("entity", "album"),
                ("media", "music"),
                ("limit", "5"),
            ])
            .send()
            .await
            .map_err(|e| format!("itunes search: {e}"))?;
        if !response.status().is_success() {
            return Ok(None);
        }
        let body: Value = response
            .json()
            .await
            .map_err(|e| format!("itunes search json: {e}"))?;
        let results = body
            .get("results")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(results
            .into_iter()
            .find(|result| itunes_result_matches(result, artist, title, track_count)))
    }

    async fn download_itunes_image(&self, url: &str) -> Result<Option<i64>, String> {
        let Ok(url) = validate_itunes_artwork_url(url) else {
            return Ok(None);
        };
        self.wait_itunes_turn().await;
        let mut response = self
            .itunes_art_http
            .get(url)
            .send()
            .await
            .map_err(|e| format!("itunes artwork fetch: {e}"))?;
        if !response.status().is_success() {
            return Ok(None);
        }
        if itunes_content_length_exceeds_limit(response.content_length()) {
            return Ok(None);
        }
        let mime = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .and_then(canonical_itunes_image_mime);
        let Some(mime) = mime else {
            return Ok(None);
        };
        let mut data = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| format!("itunes artwork bytes: {e}"))?
        {
            if !append_itunes_artwork_chunk(&mut data, &chunk) {
                return Ok(None);
            }
        }
        // Reject anything that doesn't decode as an image (CDN error pages).
        if artwork::image_dimensions(&data).0.is_none() {
            return Ok(None);
        }
        self.save_artwork(
            &TrackCover {
                mime: mime.to_string(),
                data,
            },
            "itunes",
        )
        .map(Some)
    }

    async fn wait_itunes_turn(&self) {
        let mut guard = self.last_itunes_request.lock().await;
        if let Some(last) = *guard {
            let elapsed = last.elapsed();
            if elapsed < Duration::from_secs(3) {
                tokio::time::sleep(Duration::from_secs(3) - elapsed).await;
            }
        }
        *guard = Some(Instant::now());
    }
}

fn first_collection(body: &Value) -> Option<Value> {
    body.get("results")?
        .as_array()?
        .iter()
        .find(|r| {
            r.get("wrapperType").and_then(Value::as_str) == Some("collection")
                || r.get("collectionId").is_some()
        })
        .cloned()
}

pub(super) fn itunes_result_matches(
    result: &Value,
    artist: Option<&str>,
    title: &str,
    track_count: i64,
) -> bool {
    let Some(name) = result.get("collectionName").and_then(Value::as_str) else {
        return false;
    };
    if normalize_for_match(name) != normalize_for_match(title) {
        return false;
    }
    let Some(artist) = artist else {
        return false;
    };
    let artist_ok = result
        .get("artistName")
        .and_then(Value::as_str)
        .is_some_and(|name| normalize_for_match(name) == normalize_for_match(artist));
    if !artist_ok {
        return false;
    }
    if track_count > 0 {
        let count_ok = result
            .get("trackCount")
            .and_then(Value::as_i64)
            .is_some_and(|count| count == track_count);
        if !count_ok {
            return false;
        }
    }
    true
}

/// Rewrite an `artworkUrl100` thumbnail URL (".../100x100bb.jpg") to request
/// a larger rendition, keeping the original extension.
pub(super) fn upgrade_artwork_url(url100: &str, size: &str) -> Option<String> {
    if !is_sized_artwork_component(size) {
        return None;
    }
    let mut parsed = validate_itunes_artwork_url(url100).ok()?;
    let (base, file) = parsed.path().rsplit_once('/')?;
    let (dims, ext) = file.rsplit_once('.')?;
    // Sanity check this is a sized rendition like "100x100bb".
    if !is_sized_artwork_component(dims) || !is_allowed_itunes_artwork_ext(ext) {
        return None;
    }
    parsed.set_path(&format!("{base}/{size}.{}", ext.to_ascii_lowercase()));
    Some(parsed.into())
}

fn validate_itunes_artwork_url(url: &str) -> Result<Url, String> {
    let parsed = Url::parse(url).map_err(|e| format!("invalid iTunes artwork URL: {e}"))?;
    if parsed.scheme() != "https" {
        return Err("iTunes artwork URL must use https".to_string());
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("iTunes artwork URL must not include credentials".to_string());
    }
    if parsed.port().is_some_and(|port| port != 443) {
        return Err("iTunes artwork URL must use the default https port".to_string());
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err("iTunes artwork URL must not include a query or fragment".to_string());
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| "iTunes artwork URL is missing a host".to_string())?
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if host != "mzstatic.com" && !host.ends_with(".mzstatic.com") {
        return Err("iTunes artwork URL host is not trusted".to_string());
    }
    if !parsed.path().starts_with("/image/thumb/") {
        return Err("iTunes artwork URL path is not an artwork thumbnail".to_string());
    }
    Ok(parsed)
}

fn is_sized_artwork_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 32
        && value.contains('x')
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
}

fn is_allowed_itunes_artwork_ext(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "jpg" | "jpeg" | "png" | "webp"
    )
}

fn canonical_itunes_image_mime(mime: &str) -> Option<&'static str> {
    match mime
        .split(';')
        .next()
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("image/jpeg") | Some("image/jpg") => Some("image/jpeg"),
        Some("image/png") => Some("image/png"),
        Some("image/webp") => Some("image/webp"),
        _ => None,
    }
}

fn itunes_content_length_exceeds_limit(content_length: Option<u64>) -> bool {
    content_length.is_some_and(|length| length > ITUNES_COVER_MAX_BYTES)
}

fn append_itunes_artwork_chunk(data: &mut Vec<u8>, chunk: &[u8]) -> bool {
    let next_len = data.len().saturating_add(chunk.len());
    if next_len as u64 > ITUNES_COVER_MAX_BYTES {
        return false;
    }
    data.extend_from_slice(chunk);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn itunes_image_mime_allowlist_is_exact() {
        assert_eq!(
            canonical_itunes_image_mime("image/jpeg; charset=binary"),
            Some("image/jpeg")
        );
        assert_eq!(canonical_itunes_image_mime("image/png"), Some("image/png"));
        assert_eq!(
            canonical_itunes_image_mime("image/webp"),
            Some("image/webp")
        );
        assert_eq!(canonical_itunes_image_mime("image/svg+xml"), None);
        assert_eq!(canonical_itunes_image_mime("image/avif"), None);
        assert_eq!(canonical_itunes_image_mime("text/html"), None);
    }

    #[test]
    fn itunes_content_length_limit_rejects_oversized_responses() {
        assert!(!itunes_content_length_exceeds_limit(None));
        assert!(!itunes_content_length_exceeds_limit(Some(
            ITUNES_COVER_MAX_BYTES
        )));
        assert!(itunes_content_length_exceeds_limit(Some(
            ITUNES_COVER_MAX_BYTES + 1
        )));
    }

    #[test]
    fn itunes_chunk_limit_rejects_oversized_streams() {
        let mut data = vec![0; ITUNES_COVER_MAX_BYTES as usize - 1];
        assert!(append_itunes_artwork_chunk(&mut data, &[1]));
        assert_eq!(data.len(), ITUNES_COVER_MAX_BYTES as usize);
        assert!(!append_itunes_artwork_chunk(&mut data, &[2]));
        assert_eq!(data.len(), ITUNES_COVER_MAX_BYTES as usize);
    }
}
