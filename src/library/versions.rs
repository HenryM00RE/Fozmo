use super::{
    AlbumDetail, AlbumSummary, AlbumVersionSummary, Library, TrackSummary, album_version_from_row,
    albums::sort_album_tracks, collect_rows, normalize_key, now_secs, track_from_row,
};
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::{HashMap, HashSet};

type LocalVersionAlbumRow = (i64, String, Option<String>, Option<i32>, i64, Option<i64>);

impl Library {
    pub fn album_versions(&self, album_id: i64) -> Result<Vec<AlbumVersionSummary>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                r#"
                SELECT v.id, v.album_id, v.provider, v.provider_id, v.title, v.artist,
                       v.year, v.track_count, v.art_id, v.format, v.sample_rate,
                       v.bit_depth, v.source_label, v.status,
                       CASE WHEN a.primary_version_id = v.id THEN 1 ELSE 0 END AS is_primary,
                       v.musicbrainz_match_status, v.musicbrainz_release_id,
                       v.musicbrainz_tagged_at, v.qobuz_match_status,
                       v.qobuz_tagged_at, v.autometa_message
                FROM album_versions v
                JOIN albums a ON a.id = v.album_id
                WHERE v.album_id = ?1
                ORDER BY is_primary DESC,
                         CASE WHEN v.provider = 'local' THEN 0 WHEN v.provider = 'qobuz' THEN 1 ELSE 2 END,
                         COALESCE(v.sample_rate, 0) DESC,
                         v.id
                "#,
            )
            .map_err(|e| format!("album versions: {e}"))?;
        collect_rows(
            stmt.query_map([album_id], album_version_from_row)
                .map_err(|e| format!("album versions map: {e}"))?,
        )
    }

    pub fn set_primary_version(
        &self,
        album_id: i64,
        version_id: i64,
    ) -> Result<Option<AlbumDetail>, String> {
        let belongs: Option<i64> = {
            let conn = self.conn.lock().unwrap();
            conn.query_row(
                "SELECT id FROM album_versions WHERE id = ?1 AND album_id = ?2",
                params![version_id, album_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| format!("primary version lookup: {e}"))?
        };
        if belongs.is_none() {
            if self.album(album_id)?.is_none() {
                return Ok(None);
            }
            return Err("version does not belong to album".to_string());
        }
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE albums SET primary_version_id = ?2, updated_at = ?3 WHERE id = ?1",
            params![album_id, version_id, now_secs()],
        )
        .map_err(|e| format!("set primary version: {e}"))?;
        drop(conn);
        self.album_detail(album_id)
    }

    pub(super) fn primary_local_album_tracks(
        &self,
        album: &AlbumSummary,
    ) -> Result<Vec<TrackSummary>, String> {
        let profile_id = self.active_profile_id();
        self.primary_local_album_tracks_for_profile(&profile_id, album)
    }

    pub(super) fn primary_local_album_tracks_for_profile(
        &self,
        profile_id: &str,
        album: &AlbumSummary,
    ) -> Result<Vec<TrackSummary>, String> {
        let conn = self.conn.lock().unwrap();
        let version_id = Self::primary_local_version_id_with_conn(&conn, album)?;
        drop(conn);
        match version_id {
            Some(version_id) => {
                self.local_album_tracks_for_version_for_profile(profile_id, album.id, version_id)
            }
            None => self.album_tracks_for_profile(profile_id, album.id),
        }
    }

    pub(super) fn local_album_tracks_for_version(
        &self,
        album_id: i64,
        version_id: i64,
    ) -> Result<Vec<TrackSummary>, String> {
        let profile_id = self.active_profile_id();
        self.local_album_tracks_for_version_for_profile(&profile_id, album_id, version_id)
    }

    pub(super) fn local_album_tracks_for_version_for_profile(
        &self,
        profile_id: &str,
        album_id: i64,
        version_id: i64,
    ) -> Result<Vec<TrackSummary>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                self.track_select_sql_for_profile(
                    profile_id,
                    r#"
                    JOIN version_tracks vt ON vt.local_track_id = t.id
                    WHERE t.album_id = ?1
                      AND vt.version_id = ?2
                    ORDER BY t.disc_number, t.track_number, lower(t.title)
                    "#,
                )
                .as_str(),
            )
            .map_err(|e| format!("primary local tracks: {e}"))?;
        let mut tracks = collect_rows(
            stmt.query_map(params![album_id, version_id], track_from_row)
                .map_err(|e| format!("primary local tracks map: {e}"))?,
        )?;
        sort_album_tracks(&mut tracks);
        Ok(tracks)
    }

    pub(super) fn sync_local_versions_with_conn(conn: &Connection) -> Result<(), String> {
        Self::sync_local_versions_inner_with_conn(conn, None)
    }

    pub(super) fn sync_local_versions_for_album_with_conn(
        conn: &Connection,
        album_id: i64,
    ) -> Result<(), String> {
        Self::sync_local_versions_inner_with_conn(conn, Some(album_id))
    }

    fn sync_local_versions_inner_with_conn(
        conn: &Connection,
        requested_album_id: Option<i64>,
    ) -> Result<(), String> {
        let now = now_secs();
        let album_rows: Vec<LocalVersionAlbumRow> = {
            let mut stmt = conn
                .prepare(
                    "SELECT id, title, album_artist, year, track_count, art_id FROM albums WHERE ?1 IS NULL OR id = ?1",
                )
                .map_err(|e| format!("sync versions albums: {e}"))?;
            let rows = stmt
                .query_map([requested_album_id], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<i32>>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, Option<i64>>(5)?,
                    ))
                })
                .map_err(|e| format!("sync versions album map: {e}"))?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row.map_err(|e| format!("sync versions album row: {e}"))?);
            }
            out
        };

        for (album_id, title, artist, year, track_count, art_id) in album_rows {
            let mut track_stmt = conn
                .prepare(
                    r#"
                    SELECT id, title, artist, track_number, disc_number, duration_secs, sample_rate, format, bit_depth, art_id, path
                    FROM tracks
                    WHERE album_id = ?1
                      AND COALESCE(status, 'available') = 'available'
                    ORDER BY disc_number, track_number, lower(title)
                    "#,
                )
                .map_err(|e| format!("sync local version tracks query: {e}"))?;
            let track_rows = track_stmt
                .query_map([album_id], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                        row.get::<_, Option<i64>>(4)?,
                        row.get::<_, Option<f64>>(5)?,
                        row.get::<_, Option<i64>>(6)?,
                        row.get::<_, Option<String>>(7)?,
                        row.get::<_, Option<i64>>(8)?,
                        row.get::<_, Option<i64>>(9)?,
                        row.get::<_, String>(10)?,
                    ))
                })
                .map_err(|e| format!("sync local version tracks map: {e}"))?;
            let mut local_tracks = Vec::new();
            for row in track_rows {
                local_tracks.push(row.map_err(|e| format!("sync local version track row: {e}"))?);
            }
            drop(track_stmt);

            let mut tracks_by_folder: HashMap<String, Vec<LocalVersionTrackRow>> = HashMap::new();
            for row in local_tracks {
                tracks_by_folder
                    .entry(local_version_provider_id(&row.10))
                    .or_default()
                    .push(row);
            }

            let active_provider_ids: HashSet<String> = tracks_by_folder.keys().cloned().collect();
            if let Some(preferred_provider_id) =
                preferred_local_version_provider_id(&tracks_by_folder)
            {
                Self::rename_legacy_local_version_with_conn(
                    conn,
                    album_id,
                    &preferred_provider_id,
                )?;
            }
            for (provider_id, tracks) in tracks_by_folder {
                let (format, sample_rate, bit_depth) =
                    best_local_version_quality(&tracks).unwrap_or((None, None, None));
                let version_art_id = best_local_version_art(&tracks, art_id);
                let source_label = local_version_source_label(sample_rate, bit_depth);
                let active_track_ids: HashSet<i64> =
                    tracks.iter().map(|(track_id, ..)| *track_id).collect();
                conn.execute(
                    r#"
                    INSERT INTO album_versions (
                        album_id, provider, provider_id, title, artist, year, track_count,
                        art_id, format, sample_rate, bit_depth, source_label, status,
                        payload_json, created_at, updated_at
                    )
                    VALUES (?1, 'local', ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 'available', '{}', ?12, ?12)
                    ON CONFLICT(album_id, provider, provider_id) DO UPDATE SET
                        title = excluded.title,
                        artist = excluded.artist,
                        year = excluded.year,
                        track_count = excluded.track_count,
                        art_id = excluded.art_id,
                        format = excluded.format,
                        sample_rate = excluded.sample_rate,
                        bit_depth = excluded.bit_depth,
                        source_label = excluded.source_label,
                        updated_at = excluded.updated_at
                    "#,
                    params![
                        album_id,
                        provider_id,
                        title,
                        artist,
                        year,
                        tracks.len() as i64,
                        version_art_id,
                        format,
                        sample_rate,
                        bit_depth,
                        source_label,
                        now
                    ],
                )
                .map_err(|e| format!("sync local version: {e}"))?;

                let version_id: i64 = conn
                    .query_row(
                        "SELECT id FROM album_versions WHERE album_id = ?1 AND provider = 'local' AND provider_id = ?2",
                        params![album_id, provider_id],
                        |row| row.get(0),
                    )
                    .map_err(|e| format!("sync local version id: {e}"))?;
                Self::delete_stale_local_version_tracks_with_conn(
                    conn,
                    version_id,
                    &active_track_ids,
                )?;

                for (
                    track_id,
                    track_title,
                    track_artist,
                    track_number,
                    disc_number,
                    duration_secs,
                    sample_rate,
                    format,
                    bit_depth,
                    _track_art_id,
                    _path,
                ) in tracks
                {
                    conn.execute(
                        r#"
                        INSERT INTO version_tracks (
                            version_id, provider_track_id, local_track_id, title, artist,
                            track_number, disc_number, duration_secs, sample_rate, format,
                            bit_depth, status, created_at, updated_at
                        )
                        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 'available', ?12, ?12)
                        ON CONFLICT(version_id, local_track_id) DO UPDATE SET
                            title = excluded.title,
                            artist = excluded.artist,
                            track_number = excluded.track_number,
                            disc_number = excluded.disc_number,
                            duration_secs = excluded.duration_secs,
                            sample_rate = excluded.sample_rate,
                            format = excluded.format,
                            bit_depth = excluded.bit_depth,
                            status = 'available',
                            updated_at = excluded.updated_at
                        "#,
                        params![
                            version_id,
                            track_id.to_string(),
                            track_id,
                            track_title,
                            track_artist,
                            track_number,
                            disc_number,
                            duration_secs,
                            sample_rate,
                            format,
                            bit_depth,
                            now
                        ],
                    )
                    .map_err(|e| format!("sync local version track: {e}"))?;
                }
            }

            Self::delete_stale_local_versions_with_conn(conn, album_id, &active_provider_ids)?;
            let primary_version_id = Self::primary_local_version_id_with_conn(
                conn,
                &AlbumSummary {
                    id: album_id,
                    title: title.clone(),
                    album_artist: artist.clone(),
                    year,
                    original_year: None,
                    track_count,
                    art_id,
                    confidence: 0,
                    match_status: String::new(),
                    primary_version_id: None,
                    qobuz_album_id: None,
                    qobuz_match_status: None,
                    qobuz_match_confidence: None,
                    canonical_art_id: None,
                    image_url: None,
                    mb_barcode: None,
                },
            )?;
            conn.execute(
                r#"
                UPDATE albums
                SET primary_version_id = ?2
                WHERE id = ?1
                  AND (
                    primary_version_id IS NULL
                    OR NOT EXISTS (SELECT 1 FROM album_versions WHERE id = primary_version_id AND album_id = ?1)
                  )
                "#,
                params![album_id, primary_version_id],
            )
            .map_err(|e| format!("sync primary version: {e}"))?;
        }
        if requested_album_id.is_none() {
            Self::sync_recording_identity_with_conn(conn)?;
        }
        Ok(())
    }

    pub(super) fn sync_recording_identity_with_conn(conn: &Connection) -> Result<(), String> {
        Self::sync_recording_identity_inner_with_conn(conn, None)
    }

    pub(super) fn sync_recording_identity_for_album_with_conn(
        conn: &Connection,
        album_id: i64,
    ) -> Result<(), String> {
        Self::sync_recording_identity_inner_with_conn(conn, Some(album_id))
    }

    fn sync_recording_identity_inner_with_conn(
        conn: &Connection,
        requested_album_id: Option<i64>,
    ) -> Result<(), String> {
        let now = now_secs();
        let rows: Vec<RecordingVersionTrackRow> = {
            let mut stmt = conn
                .prepare(
                    r#"
                    SELECT vt.id, v.album_id, vt.title, vt.artist, vt.disc_number, vt.track_number
                    FROM version_tracks vt
                    JOIN album_versions v ON v.id = vt.version_id
                    WHERE COALESCE(vt.status, 'available') = 'available'
                      AND (?1 IS NULL OR v.album_id = ?1)
                    "#,
                )
                .map_err(|e| format!("recording version track query: {e}"))?;
            let rows = stmt
                .query_map([requested_album_id], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<i64>>(4)?,
                        row.get::<_, Option<i64>>(5)?,
                    ))
                })
                .map_err(|e| format!("recording version track map: {e}"))?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row.map_err(|e| format!("recording version track row: {e}"))?);
            }
            out
        };

        for (version_track_id, album_id, title, artist, disc_number, track_number) in rows {
            let recording_key = recording_key_for_track(disc_number, track_number, &title);
            conn.execute(
                r#"
                INSERT INTO recordings (
                    album_id, recording_key, title, artist, disc_number, track_number,
                    created_at, updated_at
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
                ON CONFLICT(album_id, recording_key) DO UPDATE SET
                    title = CASE
                        WHEN recordings.title IS NULL OR TRIM(recordings.title) = ''
                        THEN excluded.title
                        ELSE recordings.title
                    END,
                    artist = COALESCE(recordings.artist, excluded.artist),
                    disc_number = COALESCE(recordings.disc_number, excluded.disc_number),
                    track_number = COALESCE(recordings.track_number, excluded.track_number),
                    updated_at = excluded.updated_at
                "#,
                params![
                    album_id,
                    recording_key,
                    title,
                    artist,
                    disc_number,
                    track_number,
                    now
                ],
            )
            .map_err(|e| format!("upsert recording: {e}"))?;
            let recording_id: i64 = conn
                .query_row(
                    "SELECT id FROM recordings WHERE album_id = ?1 AND recording_key = ?2",
                    params![album_id, recording_key],
                    |row| row.get(0),
                )
                .map_err(|e| format!("select recording id: {e}"))?;
            conn.execute(
                "UPDATE version_tracks SET recording_id = ?2, updated_at = ?3 WHERE id = ?1 AND COALESCE(recording_id, -1) != ?2",
                params![version_track_id, recording_id, now],
            )
            .map_err(|e| format!("assign version track recording: {e}"))?;
        }

        conn.execute(
            r#"
            UPDATE version_tracks
            SET recording_id = (
                    SELECT lvt.recording_id
                    FROM version_track_links link
                    JOIN version_tracks lvt ON lvt.id = link.local_version_track_id
                    WHERE link.qobuz_version_track_id = version_tracks.id
                      AND link.status = 'linked'
                      AND lvt.recording_id IS NOT NULL
                    ORDER BY link.confidence DESC, link.id
                    LIMIT 1
                ),
                updated_at = ?1
            WHERE id IN (
                SELECT link.qobuz_version_track_id
                FROM version_track_links link
                JOIN version_tracks qvt ON qvt.id = link.qobuz_version_track_id
                JOIN album_versions qv ON qv.id = qvt.version_id
                WHERE link.status = 'linked'
                  AND (?2 IS NULL OR qv.album_id = ?2)
            )
              AND COALESCE(recording_id, -1) != COALESCE((
                    SELECT lvt.recording_id
                    FROM version_track_links link
                    JOIN version_tracks lvt ON lvt.id = link.local_version_track_id
                    WHERE link.qobuz_version_track_id = version_tracks.id
                      AND link.status = 'linked'
                      AND lvt.recording_id IS NOT NULL
                    ORDER BY link.confidence DESC, link.id
                    LIMIT 1
                ), -1)
            "#,
            params![now, requested_album_id],
        )
        .map_err(|e| format!("sync linked qobuz recording ids: {e}"))?;

        Self::backfill_playback_history_recordings_with_conn(conn)
    }

    pub(super) fn backfill_playback_history_recordings_with_conn(
        conn: &Connection,
    ) -> Result<(), String> {
        conn.execute_batch(
            r#"
            UPDATE playback_history
            SET recording_id = (
                SELECT vt.recording_id
                FROM version_tracks vt
                WHERE playback_history.source_key GLOB 'local:[0-9]*'
                  AND vt.local_track_id = CAST(substr(playback_history.source_key, 7) AS INTEGER)
                  AND vt.recording_id IS NOT NULL
                ORDER BY vt.id
                LIMIT 1
            )
            WHERE source_key GLOB 'local:[0-9]*'
              AND (
                recording_id IS NULL
                OR recording_id != COALESCE((
                    SELECT vt.recording_id
                    FROM version_tracks vt
                    WHERE vt.local_track_id = CAST(substr(playback_history.source_key, 7) AS INTEGER)
                      AND vt.recording_id IS NOT NULL
                    ORDER BY vt.id
                    LIMIT 1
                ), -1)
              );

            UPDATE playback_history
            SET recording_id = (
                SELECT vt.recording_id
                FROM version_tracks vt
                JOIN album_versions v ON v.id = vt.version_id
                WHERE playback_history.source_key GLOB 'qobuz:[0-9]*'
                  AND v.provider = 'qobuz'
                  AND vt.provider_track_id = substr(playback_history.source_key, 7)
                  AND vt.recording_id IS NOT NULL
                ORDER BY CASE WHEN v.status = 'available' THEN 0 ELSE 1 END, vt.id
                LIMIT 1
            )
            WHERE source_key GLOB 'qobuz:[0-9]*'
              AND (
                recording_id IS NULL
                OR recording_id != COALESCE((
                    SELECT vt.recording_id
                    FROM version_tracks vt
                    JOIN album_versions v ON v.id = vt.version_id
                    WHERE v.provider = 'qobuz'
                      AND vt.provider_track_id = substr(playback_history.source_key, 7)
                      AND vt.recording_id IS NOT NULL
                    ORDER BY CASE WHEN v.status = 'available' THEN 0 ELSE 1 END, vt.id
                    LIMIT 1
                ), -1)
              );
            "#,
        )
        .map_err(|e| format!("backfill playback history recordings: {e}"))?;
        Ok(())
    }

    fn rename_legacy_local_version_with_conn(
        conn: &Connection,
        album_id: i64,
        preferred_provider_id: &str,
    ) -> Result<(), String> {
        conn.execute(
            r#"
            UPDATE album_versions
            SET provider_id = ?2,
                updated_at = ?3
            WHERE album_id = ?1
              AND provider = 'local'
              AND provider_id = 'local'
              AND NOT EXISTS (
                SELECT 1
                FROM album_versions existing
                WHERE existing.album_id = ?1
                  AND existing.provider = 'local'
                  AND existing.provider_id = ?2
              )
            "#,
            params![album_id, preferred_provider_id, now_secs()],
        )
        .map_err(|e| format!("rename legacy local version: {e}"))?;
        Ok(())
    }

    fn delete_stale_local_version_tracks_with_conn(
        conn: &Connection,
        version_id: i64,
        active_track_ids: &HashSet<i64>,
    ) -> Result<(), String> {
        let stale_ids: Vec<i64> = {
            let mut stmt = conn
                .prepare(
                    "SELECT id, local_track_id FROM version_tracks WHERE version_id = ?1 AND local_track_id IS NOT NULL",
                )
                .map_err(|e| format!("stale local version tracks query: {e}"))?;
            let rows = stmt
                .query_map([version_id], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
                })
                .map_err(|e| format!("stale local version tracks map: {e}"))?;
            let mut out = Vec::new();
            for row in rows {
                let (version_track_id, local_track_id) =
                    row.map_err(|e| format!("stale local version tracks row: {e}"))?;
                if !active_track_ids.contains(&local_track_id) {
                    out.push(version_track_id);
                }
            }
            out
        };
        for id in stale_ids {
            conn.execute("DELETE FROM version_tracks WHERE id = ?1", [id])
                .map_err(|e| format!("delete stale local version track: {e}"))?;
        }
        Ok(())
    }

    fn delete_stale_local_versions_with_conn(
        conn: &Connection,
        album_id: i64,
        active_provider_ids: &HashSet<String>,
    ) -> Result<(), String> {
        let stale_ids: Vec<i64> = {
            let mut stmt = conn
                .prepare("SELECT id, provider_id FROM album_versions WHERE album_id = ?1 AND provider = 'local'")
                .map_err(|e| format!("stale local versions query: {e}"))?;
            let rows = stmt
                .query_map([album_id], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|e| format!("stale local versions map: {e}"))?;
            let mut out = Vec::new();
            for row in rows {
                let (id, provider_id) =
                    row.map_err(|e| format!("stale local versions row: {e}"))?;
                if !active_provider_ids.contains(&provider_id) {
                    out.push(id);
                }
            }
            out
        };
        for id in stale_ids {
            conn.execute("DELETE FROM album_versions WHERE id = ?1", [id])
                .map_err(|e| format!("delete stale local version: {e}"))?;
        }
        Ok(())
    }

    fn primary_local_version_id_with_conn(
        conn: &Connection,
        album: &AlbumSummary,
    ) -> Result<Option<i64>, String> {
        if let Some(version_id) = album.primary_version_id {
            let local_id = conn
                .query_row(
                    "SELECT id FROM album_versions WHERE id = ?1 AND album_id = ?2 AND provider = 'local'",
                    params![version_id, album.id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|e| format!("primary local version lookup: {e}"))?;
            if local_id.is_some() {
                return Ok(local_id);
            }
        }

        conn.query_row(
            r#"
            SELECT id
            FROM album_versions
            WHERE album_id = ?1 AND provider = 'local'
            ORDER BY COALESCE(sample_rate, 0) DESC, COALESCE(bit_depth, 0) DESC, id
            LIMIT 1
            "#,
            [album.id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| format!("best local version lookup: {e}"))
    }
}

type LocalVersionTrackRow = (
    i64,
    String,
    Option<String>,
    Option<i64>,
    Option<i64>,
    Option<f64>,
    Option<i64>,
    Option<String>,
    Option<i64>,
    Option<i64>,
    String,
);

type RecordingVersionTrackRow = (i64, i64, String, Option<String>, Option<i64>, Option<i64>);

fn recording_key_for_track(
    disc_number: Option<i64>,
    track_number: Option<i64>,
    title: &str,
) -> String {
    format!(
        "{}|{}|{}",
        disc_number.unwrap_or(0),
        track_number.unwrap_or(0),
        normalize_key(title)
    )
}

fn best_local_version_quality(
    tracks: &[LocalVersionTrackRow],
) -> Option<(Option<String>, Option<i64>, Option<i64>)> {
    tracks
        .iter()
        .max_by_key(|row| (row.6.unwrap_or(0), row.8.unwrap_or(0), row.0))
        .map(|row| (row.7.clone(), row.6, row.8))
}

fn best_local_version_art(
    tracks: &[LocalVersionTrackRow],
    fallback_art_id: Option<i64>,
) -> Option<i64> {
    tracks
        .iter()
        .filter(|row| row.9.is_some())
        .max_by_key(|row| (row.6.unwrap_or(0), row.8.unwrap_or(0), row.0))
        .and_then(|row| row.9)
        .or(fallback_art_id)
}

fn preferred_local_version_provider_id(
    tracks_by_quality: &HashMap<String, Vec<LocalVersionTrackRow>>,
) -> Option<String> {
    tracks_by_quality
        .iter()
        .filter_map(|(provider_id, tracks)| {
            best_local_version_quality(tracks).map(|(_, sample_rate, bit_depth)| {
                (
                    provider_id.clone(),
                    sample_rate.unwrap_or(0),
                    bit_depth.unwrap_or(0),
                )
            })
        })
        .max_by_key(|(_, sample_rate, bit_depth)| (*sample_rate, *bit_depth))
        .map(|(provider_id, _, _)| provider_id)
}

fn local_version_provider_id(path: &str) -> String {
    format!("local:{}", local_version_folder_key(path))
}

fn local_version_folder_key(path: &str) -> String {
    let path = std::path::Path::new(path);
    let parent = path.parent();
    let album_dir = parent
        .filter(|dir| {
            dir.file_name()
                .and_then(|name| name.to_str())
                .and_then(parse_version_disc_folder_number)
                .is_none()
        })
        .or_else(|| parent.and_then(std::path::Path::parent));
    album_dir
        .map(|dir| normalize_key(&dir.to_string_lossy()))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "unknown-folder".to_string())
}

fn parse_version_disc_folder_number(name: &str) -> Option<i64> {
    let lower = name.trim().to_ascii_lowercase();
    for prefix in ["disc", "disk", "cd"] {
        if let Some(rest) = lower.strip_prefix(prefix) {
            let rest = rest.trim_start_matches(|c: char| {
                c.is_ascii_whitespace() || matches!(c, '-' | '_' | '.' | '#')
            });
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if digits.is_empty() {
                continue;
            }
            let number = digits.parse::<i64>().ok()?;
            if number > 0 {
                return Some(number);
            }
        }
    }
    None
}

fn local_version_source_label(sample_rate: Option<i64>, bit_depth: Option<i64>) -> String {
    match (bit_depth, sample_rate) {
        (Some(depth), Some(rate)) if rate > 0 => {
            let khz = rate as f64 / 1000.0;
            let rate_label = if rate % 1000 == 0 {
                format!("{:.0}", khz)
            } else {
                format!("{:.1}", khz)
            };
            format!("Library {depth}/{rate_label}")
        }
        _ => "Library".to_string(),
    }
}
