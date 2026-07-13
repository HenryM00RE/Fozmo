use super::artwork::validate_uploaded_cover;
use super::scanner::{
    album_seed_for_path, folder_cover_dirs_for_path, path_album_fallback_for_dirs,
    title_from_file_name,
};
use super::*;
use crate::audio::player::read_track_metadata;
use rusqlite::{OptionalExtension, params};

impl Library {
    pub fn update_album(
        &self,
        album_id: i64,
        edit: AlbumEdit,
    ) -> Result<Option<AlbumDetail>, String> {
        let Some(_existing) = self.album(album_id)? else {
            return Ok(None);
        };
        let now = now_secs();
        {
            let conn = self.conn.lock().unwrap();

            if let Some(title) = edit.title.as_deref() {
                conn.execute(
                    "UPDATE albums SET title = ?2, updated_at = ?3 WHERE id = ?1",
                    params![album_id, title, now],
                )
                .map_err(|e| format!("update album title: {e}"))?;
            }
            if edit.album_artist.is_some() {
                conn.execute(
                    "UPDATE albums SET album_artist = ?2, updated_at = ?3 WHERE id = ?1",
                    params![album_id, edit.album_artist, now],
                )
                .map_err(|e| format!("update album artist: {e}"))?;
            }
            if let Some(year) = edit.year {
                conn.execute(
                    "UPDATE albums SET year = ?2, updated_at = ?3 WHERE id = ?1",
                    params![album_id, year, now],
                )
                .map_err(|e| format!("update album year: {e}"))?;
            }

            // Recompute the sort_key so the album sorts correctly under its new identity.
            // Skip when this would clash with another album's key.
            let (title, album_artist): (String, Option<String>) = conn
                .query_row(
                    "SELECT title, album_artist FROM albums WHERE id = ?1",
                    [album_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(|e| format!("reread album: {e}"))?;
            let artist_key = album_artist
                .as_deref()
                .map(normalize_key)
                .unwrap_or_else(|| "unknown-artist".to_string());
            let new_sort_key = format!("{}|{}", artist_key, normalize_key(&title));
            let clash: Option<i64> = conn
                .query_row(
                    "SELECT id FROM albums WHERE sort_key = ?1 AND id != ?2",
                    params![new_sort_key, album_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|e| format!("sort_key clash check: {e}"))?;
            if clash.is_none() {
                let _ = conn.execute(
                    "UPDATE albums SET sort_key = ?2 WHERE id = ?1",
                    params![album_id, new_sort_key],
                );
            }

            // Bump confidence to reflect a manual edit.
            let _ = conn.execute(
                "UPDATE albums SET match_status = 'user_edited', confidence = MAX(confidence, 90) WHERE id = ?1",
                [album_id],
            );

            // Mirror album-level fields onto every track in the album so per-track
            // metadata stays in sync with what the user just typed.
            if let Some(title) = edit.title.as_deref() {
                conn.execute(
                    "UPDATE tracks SET album = ?2, updated_at = ?3 WHERE album_id = ?1",
                    params![album_id, title, now],
                )
                .map_err(|e| format!("propagate album title: {e}"))?;
            }
            if edit.album_artist.is_some() {
                conn.execute(
                    "UPDATE tracks SET album_artist = ?2, updated_at = ?3 WHERE album_id = ?1",
                    params![album_id, edit.album_artist, now],
                )
                .map_err(|e| format!("propagate album artist: {e}"))?;
            }
            if let Some(year) = edit.year {
                conn.execute(
                    "UPDATE tracks SET year = ?2, updated_at = ?3 WHERE album_id = ?1",
                    params![album_id, year, now],
                )
                .map_err(|e| format!("propagate year: {e}"))?;
            }

            for track in &edit.tracks {
                conn.execute(
                    r#"UPDATE tracks
                       SET title = ?2,
                           artist = ?3,
                           track_number = ?4,
                           disc_number = ?5,
                           updated_at = ?6
                       WHERE id = ?1 AND album_id = ?7"#,
                    params![
                        track.id,
                        track.title,
                        track.artist,
                        track.track_number,
                        track.disc_number,
                        now,
                        album_id,
                    ],
                )
                .map_err(|e| format!("update track: {e}"))?;
            }

            conn.execute(
                "DELETE FROM tracks_fts WHERE track_id IN (SELECT id FROM tracks WHERE album_id = ?1)",
                [album_id],
            )
            .map_err(|e| format!("clear album search index: {e}"))?;
            conn.execute(
                r#"
                INSERT INTO tracks_fts (
                    track_id, title, artist, album, album_artist, composer, genre, file_name
                )
                SELECT id, title, artist, album, album_artist, composer, genre, file_name
                FROM tracks
                WHERE album_id = ?1
                "#,
                [album_id],
            )
            .map_err(|e| format!("reindex album tracks: {e}"))?;

            Self::sync_local_versions_for_album_with_conn(&conn, album_id)?;
            Self::sync_recording_identity_for_album_with_conn(&conn, album_id)?;
        }

        if let Some(artist) = edit
            .album_artist
            .as_deref()
            .filter(|s| !s.trim().is_empty())
        {
            self.upsert_artist(artist)?;
        }

        self.album_detail(album_id)
    }

    pub fn set_album_cover(
        &self,
        album_id: i64,
        data: Vec<u8>,
        mime: &str,
    ) -> Result<Option<AlbumDetail>, String> {
        if self.album(album_id)?.is_none() {
            return Ok(None);
        }
        let supplied_mime = (!mime.trim().is_empty()).then_some(mime);
        let cover = validate_uploaded_cover(data, supplied_mime)?;
        let art_id = self.save_artwork(&cover, "user_upload")?;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE albums SET art_id = ?2, canonical_art_id = ?2, art_locked = 1, updated_at = ?3 WHERE id = ?1",
            params![album_id, art_id, now_secs()],
        )
        .map_err(|e| format!("album cover set: {e}"))?;
        // Also point every track in the album at the new cover so song lists pick it up.
        conn.execute(
            "UPDATE tracks SET art_id = ?2, updated_at = ?3 WHERE album_id = ?1",
            params![album_id, art_id, now_secs()],
        )
        .map_err(|e| format!("propagate track art: {e}"))?;
        Self::sync_local_versions_for_album_with_conn(&conn, album_id)?;
        drop(conn);
        self.album_detail(album_id)
    }

    /// Re-read every track in the album from disk and overwrite its DB row
    /// with the file's own tags — undoes any MusicBrainz apply or hand edit.
    /// Also clears MusicBrainz/Qobuz match identity, drops stored match
    /// candidates, recomputes the album title/artist from a majority-vote of
    /// the track tags, and re-derives the album cover from folder art or the
    /// most recent embedded image (preferring folder).
    ///
    /// Useful when a MusicBrainz apply got the wrong release and the user
    /// wants to start over from the file tags. Does NOT touch the audio files
    /// themselves — only the library DB.
    pub fn reset_album_to_file_tags(&self, album_id: i64) -> Result<Option<AlbumDetail>, String> {
        if self.album(album_id)?.is_none() {
            return Ok(None);
        }

        // Pull (id, path, file_name) for every track in this album. We need
        // the path on disk to re-read the file, and the basename for the
        // filename fallback when there's no embedded title.
        let tracks: Vec<(i64, PathBuf, String)> = {
            let conn = self.conn.lock().unwrap();
            let mut stmt = conn
                .prepare("SELECT id, path, file_name FROM tracks WHERE album_id = ?1")
                .map_err(|e| format!("reset list tracks: {e}"))?;
            let rows = stmt
                .query_map([album_id], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })
                .map_err(|e| format!("reset map tracks: {e}"))?;
            let mut out = Vec::new();
            for r in rows {
                let (id, path, file_name) = r.map_err(|e| format!("reset row: {e}"))?;
                out.push((id, PathBuf::from(path), file_name));
            }
            out
        };

        if tracks.is_empty() {
            return self.album_detail(album_id);
        }

        // Walk each file, re-read tags from disk, write the DB row.
        let now = now_secs();
        let mut album_votes: std::collections::HashMap<String, i32> =
            std::collections::HashMap::new();
        let mut album_artist_votes: std::collections::HashMap<String, i32> =
            std::collections::HashMap::new();
        let mut year_votes: std::collections::HashMap<i32, i32> = std::collections::HashMap::new();
        let mut folder_cover_id: Option<i64> = None;
        let mut first_dir: Option<PathBuf> = None;
        let mut first_album_dir: Option<PathBuf> = None;
        let music_dirs = self.music_dirs();

        for (track_id, path, file_name) in &tracks {
            let (tags, cover) = read_track_metadata(path);
            let path_fallback = path_album_fallback_for_dirs(&music_dirs, path);
            let music_root = matching_music_root_for_path(&music_dirs, path)
                .or_else(|| path_fallback.album_dir.clone())
                .or_else(|| path.parent().map(PathBuf::from))
                .unwrap_or_else(|| PathBuf::from("/"));
            let album_seed = album_seed_for_path(
                &music_root,
                path,
                &tags.album,
                &tags.album_artist,
                &tags.artist,
                tags.year,
            );

            // Folder-cover resolution happens once per album; when tracks live
            // under Disc/Disk subfolders, prefer artwork from the album folder.
            if first_dir.is_none()
                && let Some(dir) = path.parent()
            {
                first_dir = Some(dir.to_path_buf());
                first_album_dir = path_fallback
                    .album_dir
                    .clone()
                    .or_else(|| Some(dir.to_path_buf()));
                folder_cover_id = folder_cover_dirs_for_path(path, &path_fallback)
                    .into_iter()
                    .find_map(|dir| self.load_folder_cover(&dir).ok().flatten());
            }
            let embedded_art_id = cover
                .as_ref()
                .and_then(|c| self.save_artwork(c, "embedded").ok());
            let art_id = folder_cover_id.or(embedded_art_id);

            // Collect consensus signal for the album-level fields below.
            if !album_seed.title.trim().is_empty() && album_seed.title != "Unknown Album" {
                *album_votes.entry(album_seed.title.clone()).or_insert(0) += 1;
            }
            if let Some(v) = album_seed
                .album_artist
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                *album_artist_votes.entry(v.to_string()).or_insert(0) += 1;
            }
            if let Some(y) = tags.year {
                *year_votes.entry(y).or_insert(0) += 1;
            }

            let title = tags
                .title
                .clone()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| title_from_file_name(file_name));

            let conn = self.conn.lock().unwrap();
            conn.execute(
                r#"
                UPDATE tracks
                SET title = ?2,
                    artist = ?3,
                    album = ?4,
                    album_artist = ?5,
                    track_number = ?6,
                    disc_number = ?7,
                    year = ?8,
                    genre = ?9,
                    composer = ?10,
                    art_id = COALESCE(?11, art_id),
                    embedded_art = ?12,
                    mb_recording_id = NULL,
                    updated_at = ?13
                WHERE id = ?1
                "#,
                params![
                    track_id,
                    title,
                    tags.artist,
                    Some(album_seed.title.as_str()),
                    album_seed.album_artist.as_deref(),
                    tags.track_number.map(|v| v as i64),
                    tags.disc_number
                        .map(|v| v as i64)
                        .or(path_fallback.disc_number),
                    tags.year,
                    tags.genre,
                    tags.composer,
                    art_id,
                    if cover.is_some() { 1 } else { 0 },
                    now,
                ],
            )
            .map_err(|e| format!("reset update track {track_id}: {e}"))?;

            // Rebuild the FTS row so search reflects the reset.
            let _ = conn.execute("DELETE FROM tracks_fts WHERE track_id = ?1", [*track_id]);
            let _ = conn.execute(
                "INSERT INTO tracks_fts (track_id, title, artist, album, album_artist, composer, genre, file_name)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    track_id,
                    title,
                    tags.artist,
                    Some(album_seed.title.as_str()),
                    album_seed.album_artist.as_deref(),
                    tags.composer,
                    tags.genre,
                    file_name,
                ],
            );
        }

        // Pick the album-level winner: most common value, or fall back to
        // folder name for title and None for artist/year.
        let mode_str = |m: &std::collections::HashMap<String, i32>| -> Option<String> {
            m.iter().max_by_key(|(_, c)| *c).map(|(k, _)| k.clone())
        };
        let mode_year = |m: &std::collections::HashMap<i32, i32>| -> Option<i32> {
            m.iter().max_by_key(|(_, c)| *c).map(|(k, _)| *k)
        };
        let new_title = mode_str(&album_votes)
            .or_else(|| {
                first_album_dir
                    .as_ref()
                    .and_then(|d| d.file_name().and_then(|n| n.to_str()).map(String::from))
            })
            .unwrap_or_else(|| "Unknown Album".to_string());
        let new_album_artist = mode_str(&album_artist_votes);
        let new_year = mode_year(&year_votes);

        // Recompute sort_key. If it would collide with another album, keep the
        // existing one rather than risking a UNIQUE constraint violation.
        let artist_key = new_album_artist
            .as_deref()
            .map(normalize_key)
            .unwrap_or_else(|| "unknown-artist".to_string());
        let candidate_sort_key = format!("{}|{}", artist_key, normalize_key(&new_title));

        {
            let conn = self.conn.lock().unwrap();
            let clash: Option<i64> = conn
                .query_row(
                    "SELECT id FROM albums WHERE sort_key = ?1 AND id != ?2",
                    params![candidate_sort_key, album_id],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| format!("reset sort_key check: {e}"))?;
            let sort_key_to_use = if clash.is_none() {
                candidate_sort_key
            } else {
                conn.query_row(
                    "SELECT sort_key FROM albums WHERE id = ?1",
                    [album_id],
                    |r| r.get::<_, String>(0),
                )
                .map_err(|e| format!("reset reread sort_key: {e}"))?
            };

            // The confidence is a rough proxy for "how complete is this album's
            // metadata"; mirror what `album_seed_for_path` does on first scan.
            let confidence: i64 = if !album_votes.is_empty() && new_album_artist.is_some() {
                80
            } else if !album_votes.is_empty() {
                62
            } else {
                40
            };

            conn.execute(
                r#"
                UPDATE albums
                SET title = ?2,
                    album_artist = ?3,
                    year = ?4,
                    sort_key = ?5,
                    confidence = ?6,
                    match_status = 'needs_review',
                    original_year = NULL,
                    mb_release_id = NULL,
                    mb_release_group_id = NULL,
                    mb_barcode = NULL,
                    qobuz_album_id = NULL,
                    qobuz_match_status = NULL,
                    qobuz_match_confidence = NULL,
                    qobuz_payload_json = NULL,
                    art_id = COALESCE(?7, art_id),
                    canonical_art_id = NULL,
                    art_locked = 0,
                    updated_at = ?8
                WHERE id = ?1
                "#,
                params![
                    album_id,
                    new_title,
                    new_album_artist,
                    new_year,
                    sort_key_to_use,
                    confidence,
                    folder_cover_id,
                    now,
                ],
            )
            .map_err(|e| format!("reset update album: {e}"))?;

            // Wipe stored match candidates so the MusicBrainz panel starts
            // clean — the old candidates were tied to the prior album title.
            let _ = conn.execute(
                "DELETE FROM match_candidates WHERE album_id = ?1",
                [album_id],
            );
        }

        // Make sure the artist row exists for the (possibly new) album_artist.
        if let Some(name) = new_album_artist.as_deref().filter(|s| !s.trim().is_empty()) {
            self.upsert_artist(name)?;
        }

        self.album_detail(album_id)
    }

    /// Flip an album's `match_status` to `'user_confirmed'` — used when the
    /// user has reviewed the file-tag metadata and is happy with it without
    /// needing a MusicBrainz match. The album won't show up under "needs
    /// review" anymore. No-op for albums that are already `'matched'` or
    /// `'user_confirmed'`.
    pub fn mark_album_reviewed(&self, album_id: i64) -> Result<Option<AlbumDetail>, String> {
        if self.album(album_id)?.is_none() {
            return Ok(None);
        }
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE albums
             SET match_status = 'user_confirmed',
                 confidence = MAX(confidence, 75),
                 updated_at = ?2
             WHERE id = ?1 AND match_status IN ('needs_review', 'unmatched', 'local')",
            params![album_id, now_secs()],
        )
        .map_err(|e| format!("mark reviewed: {e}"))?;
        drop(conn);
        self.album_detail(album_id)
    }
}

fn matching_music_root_for_path(music_dirs: &[PathBuf], path: &Path) -> Option<PathBuf> {
    music_dirs.iter().find_map(|dir| {
        let root = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.clone());
        path.starts_with(&root).then_some(root)
    })
}
