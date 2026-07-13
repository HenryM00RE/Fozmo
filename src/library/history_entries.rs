use super::{Library, PlaybackHistoryEntry, PlaybackHistoryInput, PlaybackSummary, now_secs};
use crate::protocol::SourceRef;
use crate::settings::DEFAULT_PROFILE_ID;
use rusqlite::{OptionalExtension, params};
use std::collections::HashMap;

impl Library {
    pub fn record_playback_history(&self, input: PlaybackHistoryInput) -> Result<(), String> {
        let profile_id = history_profile_id(input.profile_id.as_deref(), &self.active_profile_id());
        let (title, artist, album, image_url) = history_metadata(&input.source);
        let source_key = input.source.key();
        let source_json = serde_json::to_string(&input.source)
            .map_err(|e| format!("serialize history source: {e}"))?;
        let conn = self.conn.lock().unwrap();
        let recording_id = Self::recording_id_for_source_with_conn(&conn, &input.source)?;
        conn.execute(
            r#"
            INSERT INTO playback_history (
                profile_id, source_key, recording_id, source_json, zone_id, zone_name, title, artist, album, image_url,
                played_secs, duration_secs, completed, counted, radio, played_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
            "#,
            params![
                profile_id,
                source_key,
                recording_id,
                source_json,
                input.zone_id,
                input.zone_name,
                title,
                artist,
                album,
                image_url,
                input.played_secs,
                input.duration_secs,
                if input.completed { 1 } else { 0 },
                if input.counted { 1 } else { 0 },
                if input.radio || input.source.is_radio() {
                    1
                } else {
                    0
                },
                now_secs(),
            ],
        )
        .map_err(|e| format!("insert playback history: {e}"))?;
        Ok(())
    }

    pub(super) fn recording_id_for_source_with_conn(
        conn: &rusqlite::Connection,
        source: &SourceRef,
    ) -> Result<Option<i64>, String> {
        Self::recording_id_for_source_key_with_conn(conn, &source.key())
    }

    fn recording_id_for_source_key_with_conn(
        conn: &rusqlite::Connection,
        source_key: &str,
    ) -> Result<Option<i64>, String> {
        if source_key.starts_with("local:") {
            return conn
                .query_row(
                    r#"
                    SELECT recording_id
                    FROM version_tracks
                    WHERE local_track_id = CAST(substr(?1, 7) AS INTEGER)
                      AND recording_id IS NOT NULL
                    ORDER BY id
                    LIMIT 1
                    "#,
                    [source_key],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|e| format!("local source recording lookup: {e}"));
        }
        if source_key.starts_with("qobuz:") {
            return conn
                .query_row(
                    r#"
                SELECT vt.recording_id
                FROM version_tracks vt
                JOIN album_versions v ON v.id = vt.version_id
                WHERE v.provider = 'qobuz'
                  AND vt.provider_track_id = substr(?1, 7)
                  AND vt.recording_id IS NOT NULL
                ORDER BY CASE WHEN v.status = 'available' THEN 0 ELSE 1 END, vt.id
                LIMIT 1
                "#,
                    [source_key],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|e| format!("qobuz source recording lookup: {e}"));
        }
        Ok(None)
    }

    pub fn recent_playback_history(
        &self,
        limit: i64,
        include_radio: bool,
    ) -> Result<Vec<PlaybackHistoryEntry>, String> {
        let profile_id = self.active_profile_id();
        self.recent_playback_history_for_profile(&profile_id, limit, include_radio)
    }

    pub fn recent_playback_history_for_profile(
        &self,
        profile_id: &str,
        limit: i64,
        include_radio: bool,
    ) -> Result<Vec<PlaybackHistoryEntry>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                r#"
                SELECT h.id, h.profile_id, h.source_json, h.zone_id, h.zone_name,
                       COALESCE(NULLIF(TRIM(t.title), ''), h.title) AS title,
                       COALESCE(NULLIF(TRIM(t.album_artist), ''), NULLIF(TRIM(a.album_artist), ''), NULLIF(TRIM(t.artist), ''), h.artist) AS artist,
                       COALESCE(NULLIF(TRIM(t.album), ''), NULLIF(TRIM(a.title), ''), h.album) AS album,
                       t.album_id, COALESCE(t.art_id, a.canonical_art_id, a.art_id) AS art_id,
                       h.image_url, h.played_secs, h.duration_secs, h.completed, h.counted, h.radio,
                       h.played_at
                FROM (
                    SELECT id, profile_id, source_key, source_json, zone_id, zone_name, title, artist, album,
                           image_url, played_secs, duration_secs, completed, counted, radio, played_at
                    FROM playback_history
                    WHERE profile_id = ?2 AND (?3 != 0 OR radio = 0)
                    ORDER BY played_at DESC, id DESC
                    LIMIT ?1
                ) h
                LEFT JOIN tracks t ON h.source_key GLOB 'local:[0-9]*'
                                  AND t.id = CAST(substr(h.source_key, 7) AS INTEGER)
                LEFT JOIN albums a ON a.id = t.album_id
                ORDER BY h.played_at DESC, h.id DESC
                "#,
            )
            .map_err(|e| format!("recent history query: {e}"))?;
        let rows = stmt
            .query_map(
                params![
                    limit.clamp(1, 200),
                    profile_id,
                    if include_radio { 1 } else { 0 }
                ],
                |row| {
                    let source_json: String = row.get(2)?;
                    let source = serde_json::from_str::<SourceRef>(&source_json).map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            2,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?;
                    Ok(PlaybackHistoryEntry {
                        id: row.get(0)?,
                        profile_id: row.get(1)?,
                        source,
                        zone_id: row.get(3)?,
                        zone_name: row.get(4)?,
                        title: row.get(5)?,
                        artist: row.get(6)?,
                        album: row.get(7)?,
                        album_id: row.get(8)?,
                        art_id: row.get(9)?,
                        image_url: row.get(10)?,
                        played_secs: row.get(11)?,
                        duration_secs: row.get(12)?,
                        completed: row.get::<_, i64>(13)? != 0,
                        counted: row.get::<_, i64>(14)? != 0,
                        radio: row.get::<_, i64>(15)? != 0,
                        played_at: row.get(16)?,
                    })
                },
            )
            .map_err(|e| format!("recent history map: {e}"))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| format!("recent history row: {e}"))?);
        }
        Ok(out)
    }

    #[allow(dead_code)]
    pub fn recent_playback_history_with_live(
        &self,
        limit: i64,
        live: &[PlaybackHistoryInput],
        include_radio: bool,
    ) -> Result<Vec<PlaybackHistoryEntry>, String> {
        let profile_id = self.active_profile_id();
        self.recent_playback_history_with_live_for_profile(&profile_id, limit, live, include_radio)
    }

    pub fn recent_playback_history_with_live_for_profile(
        &self,
        profile_id: &str,
        limit: i64,
        live: &[PlaybackHistoryInput],
        include_radio: bool,
    ) -> Result<Vec<PlaybackHistoryEntry>, String> {
        let limit = limit.clamp(1, 200);
        let mut entries =
            self.recent_playback_history_for_profile(profile_id, limit, include_radio)?;
        let live_inputs = if include_radio {
            live.iter()
                .filter(|input| history_input_matches_profile(input, profile_id))
                .cloned()
                .collect::<Vec<_>>()
        } else {
            live.iter()
                .filter(|input| {
                    history_input_matches_profile(input, profile_id)
                        && !(input.radio || input.source.is_radio())
                })
                .cloned()
                .collect::<Vec<_>>()
        };
        entries.extend(self.live_history_entries_for_profile(
            profile_id,
            &live_inputs,
            now_secs(),
            true,
        )?);
        entries.sort_by(|a, b| {
            b.played_at
                .cmp(&a.played_at)
                .then_with(|| b.id.is_negative().cmp(&a.id.is_negative()))
                .then_with(|| b.counted.cmp(&a.counted))
                .then_with(|| b.id.cmp(&a.id))
        });
        entries.truncate(limit as usize);
        Ok(entries)
    }

    #[allow(dead_code)]
    pub fn playback_summaries_for_keys(
        &self,
        keys: &[String],
    ) -> Result<HashMap<String, PlaybackSummary>, String> {
        let profile_id = self.active_profile_id();
        self.playback_summaries_for_keys_for_profile(&profile_id, keys)
    }

    pub fn playback_summaries_for_keys_for_profile(
        &self,
        profile_id: &str,
        keys: &[String],
    ) -> Result<HashMap<String, PlaybackSummary>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                r#"
                SELECT
                    SUM(CASE WHEN counted = 1 THEN 1 ELSE 0 END) AS play_count,
                    MAX(CASE WHEN counted = 1 THEN played_at ELSE NULL END) AS last_played_at,
                    SUM(COALESCE(played_secs, 0.0)) AS listened_secs
                FROM playback_history
                WHERE profile_id = ?1 AND source_key = ?2 AND played_secs IS NOT NULL
                "#,
            )
            .map_err(|e| format!("playback summaries query: {e}"))?;
        let mut out = HashMap::new();
        for key in keys {
            let recording_id = Self::recording_id_for_source_key_with_conn(&conn, key)?;
            let summary = if let Some(recording_id) = recording_id {
                conn.query_row(
                    r#"
                    SELECT
                        SUM(CASE WHEN counted = 1 THEN 1 ELSE 0 END) AS play_count,
                        MAX(CASE WHEN counted = 1 THEN played_at ELSE NULL END) AS last_played_at,
                        SUM(COALESCE(played_secs, 0.0)) AS listened_secs
                    FROM playback_history
                    WHERE profile_id = ?1 AND recording_id = ?2 AND played_secs IS NOT NULL
                    "#,
                    params![profile_id, recording_id],
                    |row| {
                        Ok(PlaybackSummary {
                            play_count: row.get::<_, Option<i64>>(0)?.unwrap_or(0),
                            last_played_at: row.get(1)?,
                            listened_secs: row.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
                        })
                    },
                )
                .map_err(|e| format!("recording playback summary row: {e}"))?
            } else {
                stmt.query_row(params![profile_id, key], |row| {
                    Ok(PlaybackSummary {
                        play_count: row.get::<_, Option<i64>>(0)?.unwrap_or(0),
                        last_played_at: row.get(1)?,
                        listened_secs: row.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
                    })
                })
                .map_err(|e| format!("playback summary row: {e}"))?
            };
            out.insert(key.clone(), summary);
        }
        Ok(out)
    }

    #[allow(dead_code)]
    pub(super) fn live_history_entries(
        &self,
        live: &[PlaybackHistoryInput],
        played_at: i64,
        include_zero: bool,
    ) -> Result<Vec<PlaybackHistoryEntry>, String> {
        let profile_id = self.active_profile_id();
        self.live_history_entries_for_profile(&profile_id, live, played_at, include_zero)
    }

    pub(super) fn live_history_entries_for_profile(
        &self,
        profile_id: &str,
        live: &[PlaybackHistoryInput],
        played_at: i64,
        include_zero: bool,
    ) -> Result<Vec<PlaybackHistoryEntry>, String> {
        live.iter()
            .enumerate()
            .filter(|(_, input)| include_zero || input.played_secs.unwrap_or(0.0) > 0.0)
            .map(|(idx, input)| {
                let (title, artist, album, image_url) = history_metadata(&input.source);
                let (album_id, art_id) = self.history_album_art_for_source(&input.source.key())?;
                Ok(PlaybackHistoryEntry {
                    id: -((idx as i64) + 1),
                    profile_id: history_profile_id(input.profile_id.as_deref(), profile_id),
                    source: input.source.clone(),
                    zone_id: input.zone_id.clone(),
                    zone_name: input.zone_name.clone(),
                    title,
                    artist,
                    album,
                    album_id,
                    art_id,
                    image_url,
                    played_secs: input.played_secs,
                    duration_secs: input.duration_secs,
                    completed: input.completed,
                    counted: input.counted,
                    radio: input.radio || input.source.is_radio(),
                    played_at,
                })
            })
            .collect()
    }

    fn history_album_art_for_source(
        &self,
        source_key: &str,
    ) -> Result<(Option<i64>, Option<i64>), String> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            r#"
            SELECT t.album_id, COALESCE(t.art_id, a.canonical_art_id, a.art_id) AS art_id
            FROM tracks t
            LEFT JOIN albums a ON a.id = t.album_id
            WHERE ?1 GLOB 'local:[0-9]*'
              AND t.id = CAST(substr(?1, 7) AS INTEGER)
            "#,
            [source_key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|e| format!("history live art query: {e}"))
        .map(|row| row.unwrap_or((None, None)))
    }
}

pub(super) fn history_metadata(
    source: &SourceRef,
) -> (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
) {
    match source {
        SourceRef::LocalTrack {
            title,
            artist,
            album,
            ..
        } => (title.clone(), artist.clone(), album.clone(), None),
        SourceRef::QobuzTrack {
            title,
            artist,
            album,
            image_url,
            ..
        } => (
            title.clone(),
            artist.clone(),
            album.clone(),
            image_url.clone(),
        ),
    }
}

pub(super) fn history_profile_id(
    input_profile_id: Option<&str>,
    active_profile_id: &str,
) -> String {
    input_profile_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(active_profile_id)
        .to_string()
}

fn history_input_matches_profile(input: &PlaybackHistoryInput, profile_id: &str) -> bool {
    input
        .profile_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_PROFILE_ID)
        == profile_id
}

pub(super) fn playback_history_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<PlaybackHistoryEntry> {
    let source_json: String = row.get(2)?;
    let source = serde_json::from_str::<SourceRef>(&source_json).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(e))
    })?;
    Ok(PlaybackHistoryEntry {
        id: row.get(0)?,
        profile_id: row.get(1)?,
        source,
        zone_id: row.get(3)?,
        zone_name: row.get(4)?,
        title: row.get(5)?,
        artist: row.get(6)?,
        album: row.get(7)?,
        album_id: row.get(8)?,
        art_id: row.get(9)?,
        image_url: row.get(10)?,
        played_secs: row.get(11)?,
        duration_secs: row.get(12)?,
        completed: row.get::<_, i64>(13)? != 0,
        counted: row.get::<_, i64>(14)? != 0,
        radio: row.get::<_, i64>(15)? != 0,
        played_at: row.get(16)?,
    })
}
