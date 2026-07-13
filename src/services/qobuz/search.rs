use super::QobuzService;
use super::model::{
    QobuzAlbumSearchResponse, QobuzArtistImageResponse, QobuzArtistSearchResponse,
    QobuzSearchResponse,
};
use super::parser::{parse_album, parse_artist, parse_track};
use serde_json::Value;

impl QobuzService {
    pub async fn search_tracks(&self, query: &str) -> Result<QobuzSearchResponse, String> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(QobuzSearchResponse { tracks: Vec::new() });
        }

        let limit = 25_u32;
        let offset = 0_u32;
        let json = self
            .signed_search_value(
                "/track/search",
                "tracksearch",
                query,
                limit,
                offset,
                None,
                "Qobuz search failed",
                "Qobuz search response was not JSON",
                "Qobuz search failed",
            )
            .await?;

        let tracks_json = json
            .get("tracks")
            .ok_or_else(|| format!("No tracks in Qobuz search response: {json}"))?;
        let tracks = tracks_json
            .get("items")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(parse_track)
            .collect();

        Ok(QobuzSearchResponse { tracks })
    }

    pub async fn search_albums(&self, query: &str) -> Result<QobuzAlbumSearchResponse, String> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(QobuzAlbumSearchResponse { albums: Vec::new() });
        }

        let limit = 25_u32;
        let offset = 0_u32;
        let json = self
            .signed_search_value(
                "/album/search",
                "albumsearch",
                query,
                limit,
                offset,
                None,
                "Qobuz album search failed",
                "Qobuz album search response was not JSON",
                "Qobuz album search failed",
            )
            .await?;

        let albums_json = json
            .get("albums")
            .ok_or_else(|| format!("No albums in Qobuz search response: {json}"))?;
        let albums = albums_json
            .get("items")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(parse_album)
            .collect();

        Ok(QobuzAlbumSearchResponse { albums })
    }

    pub async fn search_artists(
        &self,
        query: &str,
        limit: u32,
    ) -> Result<QobuzArtistSearchResponse, String> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(QobuzArtistSearchResponse {
                artists: Vec::new(),
            });
        }

        let limit = limit.clamp(1, 50);

        let json = self
            .optional_get_value(
                "/artist/search",
                vec![
                    ("query", query.to_string()),
                    ("limit", limit.to_string()),
                    ("offset", "0".to_string()),
                ],
                "Qobuz artist search failed",
                "Qobuz artist search response was not JSON",
                "Qobuz artist search failed",
            )
            .await?;

        let artists = json
            .get("artists")
            .and_then(|a| a.get("items"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(parse_artist)
            .collect();

        Ok(QobuzArtistSearchResponse { artists })
    }

    pub async fn artist_image(&self, query: &str) -> Result<QobuzArtistImageResponse, String> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(QobuzArtistImageResponse {
                name: String::new(),
                artist_id: None,
                image_url: None,
                local_image_url: None,
            });
        }

        let key = normalize_artist_image_cache_key(query);
        if let Some(response) = self.cached_artist_image(&key).await {
            return Ok(self.decorate_artist_image_response(&key, query, response));
        }

        let search = self.search_artists(query, 3).await?;
        let artist = search
            .artists
            .iter()
            .find(|artist| normalize_artist_image_cache_key(&artist.name) == key)
            .or_else(|| search.artists.first());

        let response = match artist {
            Some(artist) => QobuzArtistImageResponse {
                name: artist.name.clone(),
                artist_id: Some(artist.id),
                image_url: artist.image_url.clone(),
                local_image_url: None,
            },
            None => QobuzArtistImageResponse {
                name: query.to_string(),
                artist_id: None,
                image_url: None,
                local_image_url: None,
            },
        };
        self.store_artist_image(&key, response.clone()).await;
        Ok(self.decorate_artist_image_response(&key, query, response))
    }

    pub async fn cache_artist_portrait(
        &self,
        query: &str,
    ) -> Result<QobuzArtistImageResponse, String> {
        let mut response = self.artist_image(query).await?;
        let key = normalize_artist_image_cache_key(query);
        let Some(image_url) = response.image_url.clone() else {
            return Ok(response);
        };
        self.cached_artist_portrait_public(&key, &image_url).await?;
        response.name = query.trim().to_string();
        response.local_image_url = Some(artist_portrait_cache_url(query));
        Ok(response)
    }

    pub fn cached_artist_portrait(&self, query: &str) -> Option<(String, Vec<u8>)> {
        let key = normalize_artist_image_cache_key(query);
        if key.is_empty() {
            return None;
        }
        self.cached_artist_portrait_from_disk(&key)
    }

    fn decorate_artist_image_response(
        &self,
        key: &str,
        query: &str,
        mut response: QobuzArtistImageResponse,
    ) -> QobuzArtistImageResponse {
        if !key.is_empty() && self.cached_artist_portrait_from_disk(key).is_some() {
            response.local_image_url = Some(artist_portrait_cache_url(query));
        }
        response
    }
}

pub(crate) fn artist_portrait_cache_url(query: &str) -> String {
    format!(
        "/api/qobuz/artists/image-cache?q={}",
        urlencoding::encode(query.trim())
    )
}

fn normalize_artist_image_cache_key(value: &str) -> String {
    value
        .chars()
        .flat_map(char::to_lowercase)
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
