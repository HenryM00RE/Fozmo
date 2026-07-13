use super::{
    FavoriteAlbumRemoveRequest, FavoriteAlbumRequest, FavoriteAlbumSummary, Library,
    clean_optional_string, collect_rows, normalize_qobuz_album_id, now_secs,
};
use rusqlite::{OptionalExtension, params};

impl Library {
    pub fn favorite_albums(&self) -> Result<Vec<FavoriteAlbumSummary>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                r#"
                SELECT f.provider,
                       f.provider_id,
                       COALESCE(a.title, f.title) AS title,
                       COALESCE(a.album_artist, f.album_artist) AS album_artist,
                       COALESCE(a.canonical_art_id, a.art_id, f.art_id) AS art_id,
                       f.image_url,
                       COALESCE(a.year, f.year) AS year,
                       f.hires,
                       f.created_at
                FROM favorite_albums f
                LEFT JOIN albums a
                  ON f.provider = 'local'
                 AND a.id = CAST(f.provider_id AS INTEGER)
                ORDER BY f.created_at DESC
                "#,
            )
            .map_err(|e| format!("favorite albums query: {e}"))?;
        let rows = stmt
            .query_map([], favorite_album_from_row)
            .map_err(|e| format!("favorite albums map: {e}"))?;
        collect_rows(rows)
    }

    pub fn add_favorite_album(
        &self,
        req: FavoriteAlbumRequest,
    ) -> Result<FavoriteAlbumSummary, String> {
        let provider = favorite_album_provider(
            req.provider.as_deref(),
            req.is_qobuz,
            req.qobuz_id.as_deref().or(req.qobuz_album_id.as_deref()),
            &req.id,
        );
        let provider_id = favorite_album_provider_id(
            &provider,
            &req.id,
            req.qobuz_id.as_deref().or(req.qobuz_album_id.as_deref()),
        )?;
        let title = clean_optional_string(req.title).unwrap_or_else(|| "Unknown album".to_string());
        let album_artist =
            clean_optional_string(req.album_artist).or_else(|| clean_optional_string(req.artist));
        let image_url = clean_optional_string(req.image_url);
        let now = now_secs();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            r#"
            INSERT INTO favorite_albums (
                provider, provider_id, title, album_artist, art_id, image_url, year, hires,
                created_at, updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)
            ON CONFLICT(provider, provider_id) DO UPDATE SET
                title = excluded.title,
                album_artist = excluded.album_artist,
                art_id = excluded.art_id,
                image_url = excluded.image_url,
                year = excluded.year,
                hires = excluded.hires,
                updated_at = excluded.updated_at
            "#,
            params![
                provider,
                provider_id,
                title,
                album_artist,
                req.art_id,
                image_url,
                req.year,
                if req.hires { 1 } else { 0 },
                now,
            ],
        )
        .map_err(|e| format!("add favorite album: {e}"))?;
        drop(conn);
        self.favorite_album(&provider, &provider_id)?
            .ok_or_else(|| "Favorite album was not saved".to_string())
    }

    pub fn remove_favorite_album(&self, req: FavoriteAlbumRemoveRequest) -> Result<bool, String> {
        let provider = favorite_album_provider(
            req.provider.as_deref(),
            req.is_qobuz,
            req.qobuz_id.as_deref().or(req.qobuz_album_id.as_deref()),
            &req.id,
        );
        let provider_id = favorite_album_provider_id(
            &provider,
            &req.id,
            req.qobuz_id.as_deref().or(req.qobuz_album_id.as_deref()),
        )?;
        let conn = self.conn.lock().unwrap();
        let removed = conn
            .execute(
                "DELETE FROM favorite_albums WHERE provider = ?1 AND provider_id = ?2",
                params![provider, provider_id],
            )
            .map_err(|e| format!("remove favorite album: {e}"))?;
        Ok(removed > 0)
    }

    fn favorite_album(
        &self,
        provider: &str,
        provider_id: &str,
    ) -> Result<Option<FavoriteAlbumSummary>, String> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            r#"
            SELECT f.provider,
                   f.provider_id,
                   COALESCE(a.title, f.title) AS title,
                   COALESCE(a.album_artist, f.album_artist) AS album_artist,
                   COALESCE(a.canonical_art_id, a.art_id, f.art_id) AS art_id,
                   f.image_url,
                   COALESCE(a.year, f.year) AS year,
                   f.hires,
                   f.created_at
            FROM favorite_albums f
            LEFT JOIN albums a
              ON f.provider = 'local'
             AND a.id = CAST(f.provider_id AS INTEGER)
            WHERE f.provider = ?1 AND f.provider_id = ?2
            "#,
            params![provider, provider_id],
            favorite_album_from_row,
        )
        .optional()
        .map_err(|e| format!("favorite album lookup: {e}"))
    }
}

fn favorite_album_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<FavoriteAlbumSummary> {
    let provider: String = row.get(0)?;
    let provider_id: String = row.get(1)?;
    let is_qobuz = provider == "qobuz";
    Ok(FavoriteAlbumSummary {
        id: provider_id.clone(),
        provider,
        title: row.get(2)?,
        album_artist: row.get(3)?,
        art_id: row.get(4)?,
        image_url: row.get(5)?,
        year: row.get(6)?,
        is_qobuz,
        qobuz_id: is_qobuz.then(|| provider_id.clone()),
        qobuz_album_id: is_qobuz.then(|| provider_id.clone()),
        hires: row.get::<_, i64>(7)? != 0,
        favorited_at: row.get(8)?,
    })
}

fn favorite_album_provider(
    provider: Option<&str>,
    is_qobuz: bool,
    qobuz_id: Option<&str>,
    id: &str,
) -> String {
    let provider = provider.unwrap_or("").trim().to_lowercase();
    if provider == "qobuz"
        || is_qobuz
        || qobuz_id.is_some_and(|id| !id.trim().is_empty())
        || id.trim().starts_with("qobuz:")
    {
        "qobuz".to_string()
    } else {
        "local".to_string()
    }
}

fn favorite_album_provider_id(
    provider: &str,
    id: &str,
    qobuz_id: Option<&str>,
) -> Result<String, String> {
    let raw = if provider == "qobuz" {
        qobuz_id.filter(|id| !id.trim().is_empty()).unwrap_or(id)
    } else {
        id
    };
    let provider_id = if provider == "qobuz" {
        normalize_qobuz_album_id(raw)
    } else {
        raw.trim().to_string()
    };
    if provider_id.is_empty() {
        Err("Favorite album id is required".to_string())
    } else {
        Ok(provider_id)
    }
}
