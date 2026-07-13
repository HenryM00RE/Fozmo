use super::{Library, TrackSummary, collect_rows, matching::normalize_for_match, track_from_row};
use rusqlite::OptionalExtension;
use std::collections::HashMap;

type RepresentativeRank = (i64, i64, i64, std::cmp::Reverse<i64>);
type TrackRepresentative = (TrackSummary, RepresentativeRank);

impl Library {
    /// Insert a bare track row pointing at `path` so stream handlers can be
    /// exercised against a real file without running a library scan.
    #[cfg(test)]
    pub(crate) fn insert_track_for_test(&self, path: &std::path::Path) -> i64 {
        let conn = self.conn.lock().unwrap();
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("track.flac");
        conn.execute(
            r#"
            INSERT INTO tracks (
                path, file_name, size_bytes, modified_secs, title, artist,
                album, embedded_art, created_at, updated_at
            )
            VALUES (?1, ?2, 1, 1, 'Test Track', 'Test Artist', 'Test Album', 0, 1, 1)
            "#,
            rusqlite::params![path.to_string_lossy(), file_name],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    pub fn track_by_id(&self, track_id: i64) -> Result<Option<TrackSummary>, String> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            self.track_select_sql("WHERE t.id = ?1").as_str(),
            [track_id],
            track_from_row,
        )
        .optional()
        .map_err(|e| format!("track lookup: {e}"))
    }

    pub fn tracks(&self) -> Result<Vec<TrackSummary>, String> {
        let profile_id = self.active_profile_id();
        self.tracks_for_profile(&profile_id)
    }

    pub fn tracks_for_profile(&self, profile_id: &str) -> Result<Vec<TrackSummary>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                self.track_select_sql_for_profile(
                    profile_id,
                    "ORDER BY lower(t.artist), lower(t.album), t.disc_number, t.track_number, lower(t.title)",
                )
                .as_str(),
            )
            .map_err(|e| format!("tracks query: {e}"))?;
        let rows = stmt
            .query_map([], track_from_row)
            .map_err(|e| format!("tracks map: {e}"))?;
        collect_rows(rows)
    }

    /// Return one display track per recording. Local files which are not part
    /// of a version/recording retain their own row. For versioned recordings,
    /// prefer the track belonging to the album's primary local version, then
    /// the best-quality available local version.
    #[allow(dead_code)]
    pub(super) fn song_tracks(&self) -> Result<Vec<TrackSummary>, String> {
        let profile_id = self.active_profile_id();
        self.song_tracks_for_profile(&profile_id)
    }

    pub(super) fn song_tracks_for_profile(
        &self,
        profile_id: &str,
    ) -> Result<Vec<TrackSummary>, String> {
        let tracks = self.tracks_for_profile(profile_id)?;
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                r#"
                SELECT vt.local_track_id,
                       vt.recording_id,
                       CASE WHEN a.primary_version_id = vt.version_id THEN 1 ELSE 0 END,
                       COALESCE(v.sample_rate, vt.sample_rate, t.sample_rate, 0),
                       COALESCE(v.bit_depth, vt.bit_depth, t.bit_depth, 0),
                       vt.version_id
                FROM version_tracks vt
                JOIN album_versions v ON v.id = vt.version_id AND v.provider = 'local'
                JOIN albums a ON a.id = v.album_id
                JOIN tracks t ON t.id = vt.local_track_id
                WHERE vt.local_track_id IS NOT NULL
                  AND vt.recording_id IS NOT NULL
                  AND COALESCE(vt.status, 'available') = 'available'
                  AND COALESCE(t.status, 'available') = 'available'
                "#,
            )
            .map_err(|e| format!("song recording representatives: {e}"))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            })
            .map_err(|e| format!("song recording representatives map: {e}"))?;

        let mut identity_by_track = HashMap::new();
        for row in rows {
            let (track_id, recording_id, primary, sample_rate, bit_depth, version_id) =
                row.map_err(|e| format!("song recording representative row: {e}"))?;
            let rank = (
                primary,
                sample_rate,
                bit_depth,
                std::cmp::Reverse(version_id),
            );
            identity_by_track
                .entry(track_id)
                .and_modify(|value: &mut (i64, RepresentativeRank)| {
                    if rank > value.1 {
                        *value = (recording_id, rank);
                    }
                })
                .or_insert((recording_id, rank));
        }
        drop(stmt);
        drop(conn);

        let mut unversioned = Vec::new();
        let mut representatives: HashMap<i64, TrackRepresentative> = HashMap::new();
        for track in tracks {
            let Some((recording_id, rank)) = identity_by_track.get(&track.id).copied() else {
                unversioned.push(track);
                continue;
            };
            representatives
                .entry(recording_id)
                .and_modify(|current| {
                    if rank > current.1 {
                        *current = (track.clone(), rank);
                    }
                })
                .or_insert((track, rank));
        }
        unversioned.extend(representatives.into_values().map(|(track, _)| track));
        Ok(unversioned)
    }

    pub fn tracks_by_artist(&self, artist: &str) -> Result<Vec<TrackSummary>, String> {
        let target_artist = normalize_for_match(artist);
        if target_artist.is_empty() {
            return Ok(Vec::new());
        }

        let mut out = Vec::new();
        for track in self.tracks()? {
            let source = self.source_ref_for_track_id(track.id)?;
            let source_artist = match source {
                Some(crate::protocol::SourceRef::LocalTrack {
                    artist,
                    album_artist,
                    ..
                }) => [artist, album_artist],
                _ => [None, None],
            };
            let matches = [&track.artist, &track.album_artist]
                .into_iter()
                .flatten()
                .chain(source_artist.iter().flatten())
                .any(|value| normalize_for_match(value) == target_artist);
            if matches {
                out.push(track);
            }
        }
        Ok(out)
    }

    pub fn find_track_by_title_artist(
        &self,
        title: &str,
        artist: &str,
    ) -> Result<Option<TrackSummary>, String> {
        let target_title = normalize_for_match(title);
        let target_artist = normalize_for_match(artist);
        if target_title.is_empty() || target_artist.is_empty() {
            return Ok(None);
        }

        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                self.track_select_sql(
                    "ORDER BY COALESCE(ph.play_count, 0) DESC,
                             CASE WHEN ph.last_played_at IS NULL THEN 1 ELSE 0 END,
                             ph.last_played_at DESC,
                             lower(t.artist),
                             lower(t.album),
                             t.disc_number,
                             t.track_number",
                )
                .as_str(),
            )
            .map_err(|e| format!("lastfm local track match query: {e}"))?;
        let rows = stmt
            .query_map([], track_from_row)
            .map_err(|e| format!("lastfm local track match map: {e}"))?;

        for row in rows {
            let track = row.map_err(|e| format!("lastfm local track match row: {e}"))?;
            if normalize_for_match(&track.title) != target_title {
                continue;
            }
            let artist_match = track
                .artist
                .as_deref()
                .is_some_and(|value| normalize_for_match(value) == target_artist)
                || track
                    .album_artist
                    .as_deref()
                    .is_some_and(|value| normalize_for_match(value) == target_artist);
            if artist_match {
                return Ok(Some(track));
            }
        }

        Ok(None)
    }

    pub(super) fn track_select_sql(&self, tail: &str) -> String {
        let profile_id = self.active_profile_id();
        self.track_select_sql_for_profile(&profile_id, tail)
    }

    pub(super) fn track_select_sql_for_profile(&self, profile_id: &str, tail: &str) -> String {
        let profile_id = sql_string_literal(profile_id);
        format!(
            r#"
        SELECT t.id, t.file_name, t.title, t.artist, t.album, t.album_artist, t.track_number,
               t.disc_number, t.year, t.genre, t.composer, t.duration_secs, t.sample_rate,
               t.bit_depth, t.channels, t.format, t.album_id, COALESCE(t.art_id, a.art_id) AS art_id,
               COALESCE(ph.play_count, 0) AS play_count,
               ph.last_played_at,
               COALESCE(ph.listened_secs, 0.0) AS listened_secs
        FROM (SELECT * FROM tracks WHERE COALESCE(status, 'available') = 'available') t
        LEFT JOIN albums a ON a.id = t.album_id
        LEFT JOIN (
            SELECT local_track_id, MIN(recording_id) AS recording_id
            FROM version_tracks
            WHERE local_track_id IS NOT NULL
              AND recording_id IS NOT NULL
            GROUP BY local_track_id
        ) tr ON tr.local_track_id = t.id
        LEFT JOIN (
            SELECT CASE
                       WHEN recording_id IS NOT NULL THEN 'recording:' || recording_id
                       ELSE 'source:' || source_key
                   END AS history_key,
                   SUM(CASE WHEN counted = 1 THEN 1 ELSE 0 END) AS play_count,
                   MAX(CASE WHEN counted = 1 THEN played_at ELSE NULL END) AS last_played_at,
                   SUM(COALESCE(played_secs, 0.0)) AS listened_secs
            FROM playback_history
            WHERE profile_id = {profile_id} AND played_secs IS NOT NULL
            GROUP BY history_key
        ) ph ON ph.history_key = COALESCE('recording:' || tr.recording_id, 'source:local:' || t.id)
        {tail}
        "#
        )
    }
}

fn sql_string_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}
