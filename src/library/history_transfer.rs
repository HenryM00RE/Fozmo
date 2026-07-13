use super::history_entries::history_metadata;
use super::{
    Library, PlaybackHistoryDataEntry, PlaybackHistoryDataExport, PlaybackHistoryImportResult,
    now_secs,
};
use crate::protocol::SourceRef;
use rusqlite::params;

impl Library {
    #[allow(dead_code)]
    pub fn export_playback_history(&self) -> Result<PlaybackHistoryDataExport, String> {
        let profile_id = self.active_profile_id();
        self.export_playback_history_for_profile(&profile_id)
    }

    pub fn export_playback_history_for_profile(
        &self,
        profile_id: &str,
    ) -> Result<PlaybackHistoryDataExport, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                r#"
                SELECT profile_id, source_json, zone_id, zone_name, title, artist, album, image_url,
                       played_secs, duration_secs, completed, counted, radio, played_at
                FROM playback_history
                WHERE profile_id = ?1
                ORDER BY played_at ASC, id ASC
                "#,
            )
            .map_err(|e| format!("export history query: {e}"))?;
        let rows = stmt
            .query_map([profile_id], |row| {
                let source_json: String = row.get(1)?;
                let source = serde_json::from_str::<SourceRef>(&source_json).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        1,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?;
                Ok(PlaybackHistoryDataEntry {
                    profile_id: row.get(0)?,
                    source,
                    zone_id: row.get(2)?,
                    zone_name: row.get(3)?,
                    title: row.get(4)?,
                    artist: row.get(5)?,
                    album: row.get(6)?,
                    image_url: row.get(7)?,
                    played_secs: row.get(8)?,
                    duration_secs: row.get(9)?,
                    completed: row.get::<_, i64>(10)? != 0,
                    counted: row.get::<_, i64>(11)? != 0,
                    radio: row.get::<_, i64>(12)? != 0,
                    played_at: row.get(13)?,
                })
            })
            .map_err(|e| format!("export history map: {e}"))?;
        let mut entries = Vec::new();
        for row in rows {
            entries.push(row.map_err(|e| format!("export history row: {e}"))?);
        }
        Ok(PlaybackHistoryDataExport {
            schema_version: 2,
            exported_at: now_secs(),
            entries,
        })
    }

    #[allow(dead_code)]
    pub fn import_playback_history(
        &self,
        entries: &[PlaybackHistoryDataEntry],
        replace: bool,
    ) -> Result<PlaybackHistoryImportResult, String> {
        let profile_id = self.active_profile_id();
        self.import_playback_history_for_profile(&profile_id, entries, replace)
    }

    pub fn import_playback_history_for_profile(
        &self,
        profile_id: &str,
        entries: &[PlaybackHistoryDataEntry],
        replace: bool,
    ) -> Result<PlaybackHistoryImportResult, String> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction()
            .map_err(|e| format!("start history import: {e}"))?;
        if replace {
            tx.execute(
                "DELETE FROM playback_history WHERE profile_id = ?1",
                [profile_id],
            )
            .map_err(|e| format!("clear playback history: {e}"))?;
        }

        let mut imported = 0usize;
        let mut skipped = 0usize;
        for entry in entries {
            if !entry.played_at.is_positive() {
                skipped += 1;
                continue;
            }
            let source_key = entry.source.key();
            let radio = entry.radio || entry.source.is_radio();
            if !replace {
                let exists: i64 = tx
                    .query_row(
                        r#"
                        SELECT COUNT(*)
                        FROM playback_history
                        WHERE profile_id = ?1
                          AND source_key = ?2
                          AND zone_id = ?3
                          AND played_at = ?4
                          AND COALESCE(played_secs, -1.0) = COALESCE(?5, -1.0)
                          AND COALESCE(duration_secs, -1.0) = COALESCE(?6, -1.0)
                          AND radio = ?7
                        "#,
                        params![
                            &profile_id,
                            &source_key,
                            &entry.zone_id,
                            entry.played_at,
                            entry.played_secs,
                            entry.duration_secs,
                            if radio { 1 } else { 0 }
                        ],
                        |row| row.get(0),
                    )
                    .map_err(|e| format!("history import duplicate check: {e}"))?;
                if exists > 0 {
                    skipped += 1;
                    continue;
                }
            }

            let source_json = serde_json::to_string(&entry.source)
                .map_err(|e| format!("serialize imported history source: {e}"))?;
            let (fallback_title, fallback_artist, fallback_album, fallback_image_url) =
                history_metadata(&entry.source);
            tx.execute(
                r#"
                INSERT INTO playback_history (
                    profile_id, source_key, source_json, zone_id, zone_name, title, artist, album, image_url,
                    played_secs, duration_secs, completed, counted, radio, played_at
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
                "#,
                params![
                    &profile_id,
                    &source_key,
                    &source_json,
                    &entry.zone_id,
                    &entry.zone_name,
                    entry.title.as_ref().or(fallback_title.as_ref()),
                    entry.artist.as_ref().or(fallback_artist.as_ref()),
                    entry.album.as_ref().or(fallback_album.as_ref()),
                    entry.image_url.as_ref().or(fallback_image_url.as_ref()),
                    entry.played_secs,
                    entry.duration_secs,
                    if entry.completed { 1 } else { 0 },
                    if entry.counted { 1 } else { 0 },
                    if radio { 1 } else { 0 },
                    entry.played_at,
                ],
            )
            .map_err(|e| format!("insert imported history: {e}"))?;
            imported += 1;
        }
        tx.commit()
            .map_err(|e| format!("commit history import: {e}"))?;
        Ok(PlaybackHistoryImportResult {
            imported,
            skipped,
            replaced: replace,
        })
    }
}
