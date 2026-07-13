use super::{
    AlbumDetail, AlbumSummary, Library, MatchCandidate, TrackSummary, album_from_row, collect_rows,
    track_from_row,
};
use rusqlite::OptionalExtension;

impl Library {
    pub fn albums(&self) -> Result<Vec<AlbumSummary>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT id, title, album_artist, year, track_count, COALESCE(canonical_art_id, art_id), confidence, match_status, primary_version_id,
                        qobuz_album_id, qobuz_match_status, qobuz_match_confidence, canonical_art_id, original_year, mb_barcode,
                        CASE WHEN COALESCE(canonical_art_id, art_id) IS NULL
                                  AND qobuz_album_id IS NOT NULL
                                  AND (qobuz_match_status = 'matched'
                                       OR (qobuz_match_status = 'needs_review'
                                           AND COALESCE(qobuz_match_confidence, 0) >= 80))
                             THEN json_extract(qobuz_payload_json, '$.album.image_url') END
                 FROM albums ORDER BY lower(album_artist), lower(title)",
            )
            .map_err(|e| format!("albums query: {e}"))?;
        let rows = stmt
            .query_map([], album_from_row)
            .map_err(|e| format!("albums map: {e}"))?;
        collect_rows(rows)
    }

    pub fn album_detail(&self, album_id: i64) -> Result<Option<AlbumDetail>, String> {
        let profile_id = self.active_profile_id();
        self.album_detail_for_profile(&profile_id, album_id)
    }

    pub fn album_detail_for_profile(
        &self,
        profile_id: &str,
        album_id: i64,
    ) -> Result<Option<AlbumDetail>, String> {
        self.repair_empty_album_art_from_tracks(album_id)?;
        let Some(album) = self.album(album_id)? else {
            return Ok(None);
        };
        self.backfill_missing_album_track_bit_depth(album_id)?;
        let tracks = self.primary_local_album_tracks_for_profile(profile_id, &album)?;
        let candidates = self.match_candidates(album_id)?;
        let versions = self.album_versions(album_id)?;
        let canonical_album = self.canonical_album(&album)?;
        let qobuz_track_links = self.qobuz_track_links(album_id)?;
        let canonical_tracks = self.canonical_tracks_for_profile(profile_id, &album, &tracks)?;
        Ok(Some(AlbumDetail {
            album,
            tracks,
            candidates,
            versions,
            canonical_album,
            canonical_tracks,
            qobuz_track_links,
        }))
    }

    pub(super) fn album(&self, album_id: i64) -> Result<Option<AlbumSummary>, String> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT id, title, album_artist, year, track_count, COALESCE(canonical_art_id, art_id), confidence, match_status, primary_version_id,
                    qobuz_album_id, qobuz_match_status, qobuz_match_confidence, canonical_art_id, original_year, mb_barcode,
                    CASE WHEN COALESCE(canonical_art_id, art_id) IS NULL
                              AND qobuz_album_id IS NOT NULL
                              AND (qobuz_match_status = 'matched'
                                   OR (qobuz_match_status = 'needs_review'
                                       AND COALESCE(qobuz_match_confidence, 0) >= 80))
                         THEN json_extract(qobuz_payload_json, '$.album.image_url') END
             FROM albums WHERE id = ?1",
            [album_id],
            album_from_row,
        )
        .optional()
        .map_err(|e| format!("album lookup: {e}"))
    }

    #[allow(dead_code)]
    pub(super) fn album_tracks(&self, album_id: i64) -> Result<Vec<TrackSummary>, String> {
        let profile_id = self.active_profile_id();
        self.album_tracks_for_profile(&profile_id, album_id)
    }

    pub(super) fn album_tracks_for_profile(
        &self,
        profile_id: &str,
        album_id: i64,
    ) -> Result<Vec<TrackSummary>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                self.track_select_sql_for_profile(
                    profile_id,
                    "WHERE t.album_id = ?1 ORDER BY t.disc_number, t.track_number, lower(t.title)",
                )
                .as_str(),
            )
            .map_err(|e| format!("album tracks: {e}"))?;
        let mut tracks = collect_rows(
            stmt.query_map([album_id], track_from_row)
                .map_err(|e| format!("album tracks map: {e}"))?,
        )?;
        sort_album_tracks(&mut tracks);
        Ok(tracks)
    }

    pub(super) fn match_candidates(&self, album_id: i64) -> Result<Vec<MatchCandidate>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT id, provider, provider_id, title, artist, year, score, status
                 FROM match_candidates WHERE album_id = ?1 ORDER BY score DESC, title LIMIT 10",
            )
            .map_err(|e| format!("match candidates: {e}"))?;
        collect_rows(
            stmt.query_map([album_id], |row| {
                Ok(MatchCandidate {
                    id: row.get(0)?,
                    provider: row.get(1)?,
                    provider_id: row.get(2)?,
                    title: row.get(3)?,
                    artist: row.get(4)?,
                    year: row.get(5)?,
                    score: row.get(6)?,
                    status: row.get(7)?,
                })
            })
            .map_err(|e| format!("match candidates map: {e}"))?,
        )
    }
}

pub(super) fn sort_album_tracks(tracks: &mut [TrackSummary]) {
    tracks.sort_by(|left, right| {
        left.disc_number
            .unwrap_or(1)
            .cmp(&right.disc_number.unwrap_or(1))
            .then_with(|| {
                album_track_number(left)
                    .unwrap_or(i64::MAX)
                    .cmp(&album_track_number(right).unwrap_or(i64::MAX))
            })
            .then_with(|| {
                left.file_name
                    .to_lowercase()
                    .cmp(&right.file_name.to_lowercase())
            })
            .then_with(|| left.id.cmp(&right.id))
    });
}

fn album_track_number(track: &TrackSummary) -> Option<i64> {
    track.track_number.or_else(|| {
        let stem = track
            .file_name
            .rsplit_once('.')
            .map(|(stem, _)| stem)
            .unwrap_or(&track.file_name);
        let digits: String = stem
            .trim_start()
            .chars()
            .take_while(|character| character.is_ascii_digit())
            .collect();
        digits.parse::<i64>().ok().filter(|number| *number > 0)
    })
}
