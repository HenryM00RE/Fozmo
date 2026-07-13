use super::QobuzService;
use super::model::{
    QobuzFeaturedPlaylistsResponse, QobuzGenre, QobuzPlaylistDetail, QobuzPlaylistTag,
};
use super::parser::{
    featured_playlists_response_from_featured_response, genres_from_response, parse_playlist,
    playlist_tags_from_response, tracks_from_playlist_response,
};

impl QobuzService {
    pub async fn featured_playlists(
        &self,
        limit: u32,
        offset: u32,
        genre_id: Option<u64>,
        tag: Option<&str>,
    ) -> Result<QobuzFeaturedPlaylistsResponse, String> {
        let limit = limit.clamp(1, 50);
        let mut params = vec![
            ("limit", limit.to_string()),
            ("offset", offset.to_string()),
            ("type", "editor-picks".to_string()),
        ];
        if let Some(genre_id) = genre_id {
            params.push(("genre_ids", genre_id.to_string()));
        }
        if let Some(tag) = tag.map(str::trim).filter(|tag| !tag.is_empty()) {
            params.push(("tags", tag.to_string()));
        }

        let json = self
            .signed_get_value(
                "/playlist/getFeatured",
                "playlistgetFeatured",
                params,
                false,
            )
            .await?;
        Ok(featured_playlists_response_from_featured_response(
            &json, limit, offset,
        ))
    }

    pub async fn playlist_tags(&self) -> Result<Vec<QobuzPlaylistTag>, String> {
        let json = self
            .optional_get_value(
                "/playlist/getTags",
                Vec::new(),
                "Qobuz playlist tags request failed",
                "Qobuz playlist tags response was not JSON",
                "Qobuz playlist tags failed",
            )
            .await?;
        Ok(playlist_tags_from_response(&json))
    }

    pub async fn genres(&self) -> Result<Vec<QobuzGenre>, String> {
        let json = self
            .optional_get_value(
                "/genre/list",
                Vec::new(),
                "Qobuz genres request failed",
                "Qobuz genres response was not JSON",
                "Qobuz genres failed",
            )
            .await?;
        Ok(genres_from_response(&json))
    }

    pub async fn playlist_detail(&self, playlist_id: &str) -> Result<QobuzPlaylistDetail, String> {
        let playlist_id = playlist_id.trim();
        if playlist_id.is_empty() {
            return Err("Qobuz playlist id is required".to_string());
        }

        let json = self
            .signed_get_value(
                "/playlist/get",
                "playlistget",
                vec![
                    ("playlist_id", playlist_id.to_string()),
                    ("extra", "tracks".to_string()),
                    ("limit", "500".to_string()),
                    ("offset", "0".to_string()),
                ],
                false,
            )
            .await?;
        let playlist = json
            .get("playlist")
            .and_then(parse_playlist)
            .or_else(|| parse_playlist(&json))
            .ok_or_else(|| "Qobuz playlist response missing required fields".to_string())?;
        let tracks = tracks_from_playlist_response(&json)
            .into_iter()
            .filter(|track| track.streamable)
            .collect();

        Ok(QobuzPlaylistDetail { playlist, tracks })
    }
}
