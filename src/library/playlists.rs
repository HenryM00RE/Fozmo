use super::{
    Library, PlaylistSaveRequest, PlaylistSummary, RecentPlaylistSummary, clean_optional_string,
    now_secs,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;

impl Library {
    pub fn playlists(&self) -> Result<Vec<PlaylistSummary>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT id, name, created_at, updated_at FROM playlists ORDER BY updated_at DESC, created_at DESC",
            )
            .map_err(|e| format!("playlists query: {e}"))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })
            .map_err(|e| format!("playlists map: {e}"))?;
        let mut playlists = Vec::new();
        for row in rows {
            let (id, name, created_at, updated_at) =
                row.map_err(|e| format!("playlist row: {e}"))?;
            let items = playlist_items_from_conn(&conn, &id)?;
            playlists.push(PlaylistSummary {
                id,
                name,
                created_at,
                updated_at,
                items,
            });
        }
        Ok(playlists)
    }

    pub fn save_playlist(
        &self,
        id: &str,
        req: PlaylistSaveRequest,
    ) -> Result<PlaylistSummary, String> {
        let id = clean_playlist_id(id)?;
        let name = clean_optional_string(req.name).unwrap_or_else(|| "New Playlist".to_string());
        let now = now_secs() * 1000;
        let created_at = req.created_at.filter(|v| *v > 0).unwrap_or(now);
        let updated_at = req.updated_at.filter(|v| *v > 0).unwrap_or(now);
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction()
            .map_err(|e| format!("playlist transaction: {e}"))?;
        tx.execute(
            r#"
            INSERT INTO playlists (id, name, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4)
            ON CONFLICT(id) DO UPDATE SET
                name = excluded.name,
                updated_at = excluded.updated_at
            "#,
            params![id, name, created_at, updated_at],
        )
        .map_err(|e| format!("save playlist: {e}"))?;
        tx.execute(
            "DELETE FROM playlist_items WHERE playlist_id = ?1",
            params![id],
        )
        .map_err(|e| format!("clear playlist items: {e}"))?;
        for (position, item) in req.items.iter().enumerate() {
            let item_json =
                serde_json::to_string(item).map_err(|e| format!("serialize playlist item: {e}"))?;
            tx.execute(
                r#"
                INSERT INTO playlist_items (playlist_id, position, item_json, created_at)
                VALUES (?1, ?2, ?3, ?4)
                "#,
                params![id, position as i64, item_json, now],
            )
            .map_err(|e| format!("save playlist item: {e}"))?;
        }
        tx.commit()
            .map_err(|e| format!("commit playlist save: {e}"))?;
        drop(conn);
        self.playlist(&id)?
            .ok_or_else(|| "Playlist was not saved".to_string())
    }

    pub fn delete_playlist(&self, id: &str) -> Result<bool, String> {
        let id = clean_playlist_id(id)?;
        let conn = self.conn.lock().unwrap();
        let removed = conn
            .execute("DELETE FROM playlists WHERE id = ?1", params![id])
            .map_err(|e| format!("delete playlist: {e}"))?;
        Ok(removed > 0)
    }

    pub fn record_playlist_played(&self, id: &str) -> Result<(), String> {
        let id = clean_playlist_id(id)?;
        let conn = self.conn.lock().unwrap();
        let updated = conn
            .execute(
                "UPDATE playlists SET recently_played_at = ?1 WHERE id = ?2",
                params![now_secs(), id],
            )
            .map_err(|e| format!("record playlist played: {e}"))?;
        if updated == 0 {
            Err("Playlist not found".to_string())
        } else {
            Ok(())
        }
    }

    pub fn recent_playlists(&self, limit: i64) -> Result<Vec<RecentPlaylistSummary>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, name, recently_played_at
                FROM playlists
                WHERE recently_played_at IS NOT NULL
                ORDER BY recently_played_at DESC
                LIMIT ?1
                "#,
            )
            .map_err(|e| format!("recent playlists query: {e}"))?;
        let rows = stmt
            .query_map([limit.clamp(1, 200)], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
            .map_err(|e| format!("recent playlists map: {e}"))?;
        let mut out = Vec::new();
        for row in rows {
            let (playlist_id, name, played_at) =
                row.map_err(|e| format!("recent playlist row: {e}"))?;
            let items = playlist_items_from_conn(&conn, &playlist_id)?;
            let count = items.len();
            out.push(RecentPlaylistSummary {
                recent_type: "playlist".to_string(),
                id: format!("playlist:{playlist_id}"),
                playlist_id,
                title: name,
                album_artist: format!(
                    "Playlist - {count} song{}",
                    if count == 1 { "" } else { "s" }
                ),
                played_at,
                is_playlist: true,
                items,
            });
        }
        Ok(out)
    }

    fn playlist(&self, id: &str) -> Result<Option<PlaylistSummary>, String> {
        let conn = self.conn.lock().unwrap();
        let row = conn
            .query_row(
                "SELECT id, name, created_at, updated_at FROM playlists WHERE id = ?1",
                params![id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                },
            )
            .optional()
            .map_err(|e| format!("playlist lookup: {e}"))?;
        let Some((id, name, created_at, updated_at)) = row else {
            return Ok(None);
        };
        let items = playlist_items_from_conn(&conn, &id)?;
        Ok(Some(PlaylistSummary {
            id,
            name,
            created_at,
            updated_at,
            items,
        }))
    }
}

fn clean_playlist_id(id: &str) -> Result<String, String> {
    let id = id.trim();
    if id.is_empty() {
        Err("Playlist id is required".to_string())
    } else {
        Ok(id.to_string())
    }
}

fn playlist_items_from_conn(conn: &Connection, playlist_id: &str) -> Result<Vec<Value>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT item_json FROM playlist_items WHERE playlist_id = ?1 ORDER BY position ASC",
        )
        .map_err(|e| format!("playlist items query: {e}"))?;
    let rows = stmt
        .query_map(params![playlist_id], |row| {
            let item_json: String = row.get(0)?;
            serde_json::from_str::<Value>(&item_json).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })
        })
        .map_err(|e| format!("playlist items map: {e}"))?;
    let mut items = Vec::new();
    for row in rows {
        items.push(row.map_err(|e| format!("playlist item row: {e}"))?);
    }
    Ok(items)
}
