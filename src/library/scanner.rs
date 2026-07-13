use super::artwork::{PreparedArtwork, read_folder_cover};
use super::*;
use crate::audio::player::{TrackTags, read_track_metadata};
use rusqlite::{OptionalExtension, params};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

const SUPPORTED_EXTENSIONS: &[&str] = &[
    "wav", "flac", "mp3", "m4a", "ogg", "caf", "aac", "aiff", "aif", "opus",
];
const SCAN_WRITE_BATCH_SIZE: usize = 200;
const PREPARE_PROGRESS_INTERVAL: usize = 250;
#[derive(Debug)]
pub(super) struct AlbumSeed {
    pub(super) title: String,
    pub(super) album_artist: Option<String>,
    pub(super) year: Option<i32>,
    pub(super) confidence: i64,
    pub(super) match_status: String,
    pub(super) sort_key: String,
}

#[derive(Debug, Clone, Default)]
pub(super) struct PathAlbumFallback {
    pub(super) title: Option<String>,
    pub(super) album_dir: Option<PathBuf>,
    pub(super) disc_number: Option<i64>,
    pub(super) disc_folder_name: Option<String>,
}

#[derive(Debug, Clone)]
struct SplitDiscAlbumRow {
    id: i64,
    album_artist: Option<String>,
    year: Option<i32>,
    confidence: i64,
    match_status: String,
    art_id: Option<i64>,
    base_title: String,
    disc_number: i64,
}

#[derive(Debug)]
struct MoveCandidate {
    id: i64,
    path: String,
    title: String,
    album: Option<String>,
    album_artist: Option<String>,
    track_number: Option<i64>,
    disc_number: Option<i64>,
    duration_secs: Option<f64>,
}

type PathAlbumRefreshRow = (
    Option<String>,
    Option<String>,
    Option<i64>,
    Option<String>,
    Option<String>,
    Option<i64>,
    Option<String>,
);

struct PreparedScanFile {
    music_root: PathBuf,
    path: PathBuf,
    path_str: String,
    size_bytes: i64,
    modified_secs: i64,
    path_fallback: PathAlbumFallback,
    folder_cover_dir: Option<PathBuf>,
    action: PreparedScanAction,
}

enum PreparedScanAction {
    Pending,
    Fresh {
        album_id: Option<i64>,
        repair_cover: Option<PreparedArtwork>,
        should_read_repair_cover: bool,
    },
    Changed {
        tags: TrackTags,
        cover: Option<PreparedArtwork>,
        embedded_art: bool,
    },
}

impl Library {
    pub fn scan(&self) -> Result<LibraryScanResult, String> {
        if !self.try_begin_scan() {
            return Err("Library scan already running".to_string());
        }
        let result = self.run_active_scan_files()?;
        self.finish_active_scan(result.clone());
        Ok(result)
    }

    pub fn try_begin_scan(&self) -> bool {
        if self
            .scan_running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return false;
        }
        self.begin_scan_progress();
        true
    }

    pub fn run_active_scan_files(&self) -> Result<LibraryScanResult, String> {
        match self.scan_inner() {
            Ok(result) => Ok(result),
            Err(error) => {
                self.fail_active_scan(&error);
                Err(error)
            }
        }
    }

    pub fn finish_active_scan(&self, result: LibraryScanResult) {
        self.finish_scan_progress(result);
        self.scan_running.store(false, Ordering::SeqCst);
    }

    pub fn fail_active_scan(&self, error: &str) {
        self.fail_scan_progress(error);
        self.scan_running.store(false, Ordering::SeqCst);
    }

    fn scan_inner(&self) -> Result<LibraryScanResult, String> {
        let music_dirs = self.music_dirs();
        if music_dirs.is_empty() {
            return Err("No music folders configured".to_string());
        }
        let mut files = Vec::new();
        self.set_scan_progress_phase("preparing", "Finding audio files");
        for music_dir in &music_dirs {
            if !music_dir.is_dir() {
                return Err(format!("music folder does not exist: {:?}", music_dir));
            }
            let music_root = fs::canonicalize(music_dir).unwrap_or_else(|_| music_dir.clone());
            let found_before = files.len();
            let root_files = collect_audio_files(&music_root, |found, path| {
                self.update_scan_progress_preparing(found_before + found, path);
            })?;
            let found_after = found_before + root_files.len();
            if found_after > found_before
                && let Some(path) = root_files.last()
            {
                self.update_scan_progress_preparing(found_after, path);
            }
            files.extend(
                root_files
                    .into_iter()
                    .map(|path| (music_root.clone(), path)),
            );
        }
        let total = files.len();
        self.set_scan_progress_total(total);

        let mut seen_paths = HashSet::new();
        let mut scanned = 0;
        let mut updated = 0;
        // Persisted ids (including a known absence) are safe to retain for the
        // scan. Cover bytes themselves never survive their preparation batch.
        let mut folder_art_id_cache: HashMap<PathBuf, Option<i64>> = HashMap::new();
        let mut applied_folder_art: HashSet<(i64, i64)> = HashSet::new();

        for batch in files.chunks(SCAN_WRITE_BATCH_SIZE) {
            let mut folder_cover_cache: HashMap<PathBuf, Option<PreparedArtwork>> = HashMap::new();
            let mut prepared = Vec::with_capacity(batch.len());
            for (music_root, path) in batch {
                scanned += 1;
                self.update_scan_progress_file(scanned, total, updated, path);
                let path_str = path.to_string_lossy().to_string();
                seen_paths.insert(path_str.clone());
                let metadata = match fs::metadata(path) {
                    Ok(metadata) => metadata,
                    Err(_) => continue,
                };
                let size_bytes = metadata.len() as i64;
                let modified_secs = modified_secs(&metadata).unwrap_or(0);
                let path_fallback = path_album_fallback(music_root, path);
                let folder_cover_dir = folder_cover_dirs_for_path(path, &path_fallback)
                    .into_iter()
                    .find(|dir| {
                        if let Some(art_id) = folder_art_id_cache.get(dir) {
                            return art_id.is_some();
                        }
                        if let Some(cover) = folder_cover_cache.get(dir) {
                            return cover.is_some();
                        }
                        let prepared = read_folder_cover(dir)
                            .as_ref()
                            .and_then(|cover| self.prepare_artwork(cover).ok());
                        let found = prepared.is_some();
                        if !found {
                            folder_art_id_cache.insert(dir.clone(), None);
                        }
                        folder_cover_cache.insert(dir.clone(), prepared);
                        found
                    });
                prepared.push(PreparedScanFile {
                    music_root: music_root.clone(),
                    path: path.clone(),
                    path_str,
                    size_bytes,
                    modified_secs,
                    path_fallback,
                    folder_cover_dir,
                    action: PreparedScanAction::Pending,
                });
            }

            // Snapshot freshness and the uncommon embedded-art repair need in
            // one short read-only lock. Media probing happens only after this
            // guard is released.
            {
                let conn = self.conn.lock().unwrap();
                for file in &mut prepared {
                    let fresh = Self::is_track_fresh_with_conn(
                        &conn,
                        &file.path_str,
                        file.size_bytes,
                        file.modified_secs,
                    )? && !Self::track_needs_path_album_refresh_with_conn(
                        &conn,
                        &file.path_str,
                        &file.path_fallback,
                    )?;
                    if !fresh {
                        continue;
                    }
                    let album_id = Self::album_id_for_track_path_with_conn(&conn, &file.path_str)?;
                    let should_read_repair_cover = match (file.folder_cover_dir.is_none(), album_id)
                    {
                        (true, Some(album_id)) => {
                            Self::album_needs_art_with_conn(&conn, album_id)?
                                && Self::track_art_id_with_conn(&conn, &file.path_str)?.is_none()
                        }
                        _ => false,
                    };
                    file.action = PreparedScanAction::Fresh {
                        album_id,
                        repair_cover: None,
                        should_read_repair_cover,
                    };
                }
            }

            for file in &mut prepared {
                match &mut file.action {
                    PreparedScanAction::Pending => {
                        let (tags, cover) = read_track_metadata(&file.path);
                        let embedded_art = cover.is_some();
                        let cover = cover
                            .as_ref()
                            .and_then(|cover| self.prepare_artwork(cover).ok());
                        file.action = PreparedScanAction::Changed {
                            tags,
                            cover,
                            embedded_art,
                        };
                    }
                    PreparedScanAction::Fresh {
                        repair_cover,
                        should_read_repair_cover: true,
                        ..
                    } => {
                        *repair_cover = read_track_metadata(&file.path)
                            .1
                            .as_ref()
                            .and_then(|cover| self.prepare_artwork(cover).ok());
                    }
                    PreparedScanAction::Fresh { .. } | PreparedScanAction::Changed { .. } => {}
                }
            }

            let mut conn = self.conn.lock().unwrap();
            let tx = conn
                .transaction()
                .map_err(|e| format!("begin scan transaction: {e}"))?;
            for file in prepared {
                let folder_art_id = match file.folder_cover_dir.as_ref() {
                    Some(dir) => match folder_art_id_cache.get(dir) {
                        Some(art_id) => *art_id,
                        None => {
                            let art_id = folder_cover_cache
                                .get(dir)
                                .and_then(Option::as_ref)
                                .and_then(|cover| {
                                    Self::save_prepared_artwork_with_conn(&tx, cover, "folder").ok()
                                });
                            folder_art_id_cache.insert(dir.clone(), art_id);
                            art_id
                        }
                    },
                    None => None,
                };
                match file.action {
                    PreparedScanAction::Fresh {
                        album_id,
                        repair_cover,
                        ..
                    } => {
                        if let Some(album_id) = album_id {
                            if let Some(folder_art_id) = folder_art_id {
                                apply_folder_album_art_once(
                                    &tx,
                                    &mut applied_folder_art,
                                    album_id,
                                    folder_art_id,
                                )?;
                            } else {
                                self.repair_empty_album_art_from_prepared_cover_with_conn(
                                    &tx,
                                    album_id,
                                    &file.path_str,
                                    repair_cover.as_ref(),
                                )?;
                            }
                        }
                    }
                    PreparedScanAction::Changed {
                        tags,
                        cover,
                        embedded_art,
                    } => {
                        let file_name = file
                            .path
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("Unknown")
                            .to_string();
                        let title = tags
                            .title
                            .clone()
                            .filter(|s| !s.trim().is_empty())
                            .unwrap_or_else(|| title_from_file_name(&file_name));
                        let extension = file
                            .path
                            .extension()
                            .and_then(|e| e.to_str())
                            .map(|e| e.to_ascii_uppercase());
                        let album_seed = album_seed_for_path(
                            &file.music_root,
                            &file.path,
                            &tags.album,
                            &tags.album_artist,
                            &tags.artist,
                            tags.year,
                        );
                        let album_id = Self::upsert_album_with_conn(&tx, &album_seed)?;
                        let disc_number = tags
                            .disc_number
                            .map(|v| v as i64)
                            .or(file.path_fallback.disc_number);
                        Self::rebind_moved_track_with_conn(
                            &tx,
                            &seen_paths,
                            &file.path_str,
                            &file_name,
                            file.size_bytes,
                            file.modified_secs,
                            &title,
                            Some(&album_seed.title),
                            tags.album_artist
                                .as_deref()
                                .or(album_seed.album_artist.as_deref()),
                            tags.track_number.map(|v| v as i64),
                            disc_number,
                            tags.duration_secs,
                        )?;
                        // Prefer folder-level artwork, checking the album folder before a disc subfolder.
                        // Fall back to embedded artwork pulled from the file itself.
                        let embedded_art_id = cover.as_ref().and_then(|cover| {
                            Self::save_prepared_artwork_with_conn(&tx, cover, "embedded").ok()
                        });
                        let art_id = folder_art_id.or(embedded_art_id);
                        Self::upsert_track_with_conn(
                            &tx,
                            &file.path_str,
                            &file_name,
                            file.size_bytes,
                            file.modified_secs,
                            &title,
                            tags.artist.as_deref(),
                            Some(&album_seed.title),
                            tags.album_artist
                                .as_deref()
                                .or(album_seed.album_artist.as_deref()),
                            tags.track_number.map(|v| v as i64),
                            disc_number,
                            tags.year,
                            tags.genre.as_deref(),
                            tags.composer.as_deref(),
                            tags.duration_secs,
                            tags.sample_rate.map(|v| v as i64),
                            tags.bits_per_sample.map(|v| v as i64),
                            tags.channels.map(|v| v as i64),
                            extension.as_deref(),
                            album_id,
                            art_id,
                            embedded_art,
                        )?;
                        // Folder cover always wins for the album cover; otherwise only fill if empty.
                        if let Some(folder_art_id) = folder_art_id {
                            apply_folder_album_art_once(
                                &tx,
                                &mut applied_folder_art,
                                album_id,
                                folder_art_id,
                            )?;
                        } else if let Some(art_id) = art_id {
                            Self::apply_album_art_if_empty_with_conn(&tx, album_id, art_id)?;
                        }
                        if let Some(artist) = tags.artist.as_deref() {
                            Self::upsert_artist_with_conn(&tx, artist)?;
                        }
                        if let Some(album_artist) = tags.album_artist.as_deref() {
                            Self::upsert_artist_with_conn(&tx, album_artist)?;
                        }
                        if let Some(album_artist) = album_seed.album_artist.as_deref() {
                            Self::upsert_artist_with_conn(&tx, album_artist)?;
                        }
                        updated += 1;
                        self.update_scan_progress_file(scanned, total, updated, &file.path);
                    }
                    PreparedScanAction::Pending => unreachable!("scan file should be prepared"),
                }
            }
            tx.commit()
                .map_err(|e| format!("commit scan transaction: {e}"))?;
        }

        self.set_scan_progress_phase("cleanup", "Removing missing files");
        let removed = {
            let mut conn = self.conn.lock().unwrap();
            let tx = conn
                .transaction()
                .map_err(|e| format!("begin scan cleanup transaction: {e}"))?;
            let removed = Self::remove_missing_tracks_with_conn(&tx, &seen_paths)?;
            Self::refresh_album_rollups_with_conn(&tx)?;
            tx.commit()
                .map_err(|e| format!("commit scan cleanup transaction: {e}"))?;
            removed
        };
        self.set_scan_progress_cleanup(scanned, total, updated, removed);
        Ok(LibraryScanResult {
            scanned,
            updated,
            removed,
        })
    }

    pub(super) fn repair_empty_album_art_from_tracks(&self, album_id: i64) -> Result<(), String> {
        let track_paths = {
            let conn = self.conn.lock().unwrap();
            let mut stmt = conn
                .prepare(
                    r#"
                    SELECT path
                    FROM tracks
                    WHERE album_id = ?1
                      AND COALESCE(status, 'available') = 'available'
                    ORDER BY COALESCE(disc_number, 1), COALESCE(track_number, 999999), id
                    "#,
                )
                .map_err(|e| format!("album embedded art fallback tracks: {e}"))?;
            let rows = stmt
                .query_map([album_id], |row| row.get::<_, String>(0))
                .map_err(|e| format!("album embedded art fallback tracks map: {e}"))?;
            let mut paths = Vec::new();
            for row in rows {
                paths.push(row.map_err(|e| format!("album embedded art fallback track row: {e}"))?);
            }
            paths
        };
        for path in track_paths {
            self.repair_empty_album_art_from_track_metadata(album_id, &path, Path::new(&path))?;
        }
        Ok(())
    }

    /// Album-detail repair reads media without holding the SQLite mutex. A
    /// malformed or slow file must not stall unrelated database-backed APIs.
    fn repair_empty_album_art_from_track_metadata(
        &self,
        album_id: i64,
        path_str: &str,
        path: &Path,
    ) -> Result<(), String> {
        let stored_art_id = {
            let conn = self.conn.lock().unwrap();
            if !Self::album_needs_art_with_conn(&conn, album_id)? {
                return Ok(());
            }
            Self::track_art_id_with_conn(&conn, path_str)?
        };
        let cover = if stored_art_id.is_none() {
            read_track_metadata(path)
                .1
                .as_ref()
                .and_then(|cover| self.prepare_artwork(cover).ok())
        } else {
            None
        };

        let conn = self.conn.lock().unwrap();
        if !Self::album_needs_art_with_conn(&conn, album_id)? {
            return Ok(());
        }
        let art_id = Self::track_art_id_with_conn(&conn, path_str)?
            .or(stored_art_id)
            .or_else(|| {
                cover.as_ref().and_then(|cover| {
                    Self::save_prepared_artwork_with_conn(&conn, cover, "embedded").ok()
                })
            });
        self.apply_repaired_track_art_with_conn(&conn, album_id, path_str, art_id)
    }

    fn repair_empty_album_art_from_prepared_cover_with_conn(
        &self,
        conn: &Connection,
        album_id: i64,
        path_str: &str,
        cover: Option<&PreparedArtwork>,
    ) -> Result<(), String> {
        if !Self::album_needs_art_with_conn(conn, album_id)? {
            return Ok(());
        }

        let stored_art_id = Self::track_art_id_with_conn(conn, path_str)?;
        let art_id = match stored_art_id {
            Some(art_id) => Some(art_id),
            None => cover.and_then(|cover| {
                Self::save_prepared_artwork_with_conn(conn, cover, "embedded").ok()
            }),
        };
        self.apply_repaired_track_art_with_conn(conn, album_id, path_str, art_id)
    }

    fn album_needs_art_with_conn(conn: &Connection, album_id: i64) -> Result<bool, String> {
        let needs_art = conn
            .query_row(
                r#"
                SELECT COALESCE(canonical_art_id, art_id) IS NULL
                       AND COALESCE(art_locked, 0) = 0
                FROM albums
                WHERE id = ?1
                "#,
                [album_id],
                |row| row.get::<_, bool>(0),
            )
            .optional()
            .map_err(|e| format!("album embedded art fallback lookup: {e}"))?;
        Ok(needs_art.unwrap_or(false))
    }

    fn track_art_id_with_conn(conn: &Connection, path_str: &str) -> Result<Option<i64>, String> {
        let art_id = conn
            .query_row(
                "SELECT art_id FROM tracks WHERE path = ?1",
                [path_str],
                |row| row.get::<_, Option<i64>>(0),
            )
            .optional()
            .map_err(|e| format!("track embedded art fallback lookup: {e}"))?;
        Ok(art_id.flatten())
    }

    fn apply_repaired_track_art_with_conn(
        &self,
        conn: &Connection,
        album_id: i64,
        path_str: &str,
        art_id: Option<i64>,
    ) -> Result<(), String> {
        let Some(art_id) = art_id else {
            return Ok(());
        };

        conn.execute(
            "UPDATE tracks SET art_id = ?2, embedded_art = 1, updated_at = ?3 WHERE path = ?1",
            params![path_str, art_id, now_secs()],
        )
        .map_err(|e| format!("track embedded art fallback update: {e}"))?;
        Self::apply_album_art_if_empty_with_conn(conn, album_id, art_id)
    }

    pub fn scan_progress(&self) -> LibraryScanProgress {
        self.scan_progress.lock().unwrap().clone()
    }

    pub fn set_scan_progress_phase(&self, phase: &str, message: &str) {
        let mut progress = self.scan_progress.lock().unwrap();
        progress.running = true;
        progress.phase = phase.to_string();
        progress.message = message.to_string();
        progress.error = None;
    }

    fn begin_scan_progress(&self) {
        let mut progress = self.scan_progress.lock().unwrap();
        *progress = LibraryScanProgress {
            running: true,
            phase: "preparing".to_string(),
            message: "Finding audio files".to_string(),
            ..LibraryScanProgress::default()
        };
    }

    fn set_scan_progress_total(&self, total: usize) {
        let mut progress = self.scan_progress.lock().unwrap();
        progress.running = true;
        progress.phase = "scanning".to_string();
        progress.total = total;
        progress.message = if total == 0 {
            "No audio files found".to_string()
        } else {
            format!("Scanning 0 / {total} files")
        };
    }

    fn update_scan_progress_preparing(&self, found: usize, path: &Path) {
        let mut progress = self.scan_progress.lock().unwrap();
        progress.running = true;
        progress.phase = "preparing".to_string();
        progress.scanned = found;
        progress.total = 0;
        progress.current_path = path
            .file_name()
            .and_then(|name| name.to_str())
            .map(String::from);
        progress.message = format!("Found {found} audio files");
    }

    fn update_scan_progress_file(&self, scanned: usize, total: usize, updated: usize, path: &Path) {
        let mut progress = self.scan_progress.lock().unwrap();
        progress.running = true;
        progress.phase = "scanning".to_string();
        progress.scanned = scanned;
        progress.total = total;
        progress.updated = updated;
        progress.current_path = path
            .file_name()
            .and_then(|name| name.to_str())
            .map(String::from);
        progress.message = if total > 0 {
            format!("Scanning {scanned} / {total} files")
        } else {
            "Scanning files".to_string()
        };
    }

    fn set_scan_progress_cleanup(
        &self,
        scanned: usize,
        total: usize,
        updated: usize,
        removed: usize,
    ) {
        let mut progress = self.scan_progress.lock().unwrap();
        progress.running = true;
        progress.phase = "cleanup".to_string();
        progress.scanned = scanned;
        progress.total = total;
        progress.updated = updated;
        progress.removed = removed;
        progress.current_path = None;
        progress.message = "Finalizing library index".to_string();
    }

    pub fn finish_scan_progress(&self, result: LibraryScanResult) {
        let mut progress = self.scan_progress.lock().unwrap();
        progress.running = false;
        progress.phase = "complete".to_string();
        progress.scanned = result.scanned;
        progress.total = result.scanned;
        progress.updated = result.updated;
        progress.removed = result.removed;
        progress.current_path = None;
        progress.message = format!(
            "Indexed {} files, updated {}, removed {}",
            result.scanned, result.updated, result.removed
        );
        progress.last_result = Some(result);
        progress.error = None;
    }

    fn fail_scan_progress(&self, error: &str) {
        let mut progress = self.scan_progress.lock().unwrap();
        progress.running = false;
        progress.phase = "error".to_string();
        progress.current_path = None;
        progress.message = "Scan failed".to_string();
        progress.error = Some(error.to_string());
    }

    fn is_track_fresh_with_conn(
        conn: &Connection,
        path: &str,
        size_bytes: i64,
        modified_secs: i64,
    ) -> Result<bool, String> {
        let fresh: Option<i64> = conn
            .query_row(
                r#"
                SELECT id
                FROM tracks
                WHERE path = ?1
                  AND size_bytes = ?2
                  AND modified_secs = ?3
                  AND (
                      bit_depth IS NOT NULL
                      OR (
                          COALESCE(sample_rate, 0) <= 48000
                          AND upper(COALESCE(format, '')) NOT IN ('FLAC', 'WAV', 'AIFF', 'AIF', 'CAF')
                      )
                  )
                "#,
                params![path, size_bytes, modified_secs],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| format!("track freshness: {e}"))?;
        Ok(fresh.is_some())
    }

    pub(super) fn backfill_missing_track_bit_depth(&self) -> Result<(), String> {
        let rows: Vec<(i64, String)> = {
            let conn = self.conn.lock().unwrap();
            let mut stmt = conn
                .prepare(
                    r#"
                    SELECT id, path
                    FROM tracks
                    WHERE bit_depth IS NULL
                      AND path IS NOT NULL
                      AND upper(COALESCE(format, '')) IN ('FLAC', 'WAV', 'AIFF', 'AIF', 'CAF')
                    "#,
                )
                .map_err(|e| format!("query tracks missing bit depth: {e}"))?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|e| format!("map tracks missing bit depth: {e}"))?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row.map_err(|e| format!("track missing bit depth row: {e}"))?);
            }
            out
        };
        self.backfill_track_bit_depth_rows(rows).map(|_| ())
    }

    pub(super) fn backfill_missing_album_track_bit_depth(
        &self,
        album_id: i64,
    ) -> Result<(), String> {
        let rows: Vec<(i64, String)> = {
            let conn = self.conn.lock().unwrap();
            let mut stmt = conn
                .prepare(
                    r#"
                    SELECT id, path
                    FROM tracks
                    WHERE album_id = ?1
                      AND bit_depth IS NULL
                      AND path IS NOT NULL
                      AND upper(COALESCE(format, '')) IN ('FLAC', 'WAV', 'AIFF', 'AIF', 'CAF')
                    "#,
                )
                .map_err(|e| format!("query album tracks missing bit depth: {e}"))?;
            let rows = stmt
                .query_map([album_id], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|e| format!("map album tracks missing bit depth: {e}"))?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row.map_err(|e| format!("album track missing bit depth row: {e}"))?);
            }
            out
        };
        let changed = self.backfill_track_bit_depth_rows(rows)?;
        if changed {
            let conn = self.conn.lock().unwrap();
            Self::sync_local_versions_with_conn(&conn)?;
        }
        Ok(())
    }

    fn backfill_track_bit_depth_rows(&self, rows: Vec<(i64, String)>) -> Result<bool, String> {
        let mut changed = false;
        for (id, path) in rows {
            let (tags, _) = read_track_metadata(Path::new(&path));
            let Some(bit_depth) = tags.bits_per_sample else {
                continue;
            };
            let conn = self.conn.lock().unwrap();
            conn.execute(
                r#"
                UPDATE tracks
                SET bit_depth = ?1,
                    sample_rate = COALESCE(sample_rate, ?2),
                    channels = COALESCE(channels, ?3),
                    updated_at = strftime('%s','now')
                WHERE id = ?4
                  AND bit_depth IS NULL
                "#,
                params![
                    bit_depth as i64,
                    tags.sample_rate.map(|v| v as i64),
                    tags.channels.map(|v| v as i64),
                    id
                ],
            )
            .map_err(|e| format!("backfill track bit depth: {e}"))?;
            changed = true;
        }

        Ok(changed)
    }

    fn upsert_album_with_conn(conn: &Connection, seed: &AlbumSeed) -> Result<i64, String> {
        let now = now_secs();
        conn.execute(
            r#"
            INSERT INTO albums (title, album_artist, sort_key, year, confidence, match_status, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
            ON CONFLICT(sort_key) DO UPDATE SET
                title = CASE
                    WHEN albums.match_status IN ('matched', 'user_edited') THEN albums.title
                    ELSE excluded.title
                END,
                album_artist = CASE
                    WHEN albums.match_status IN ('matched', 'user_edited') THEN albums.album_artist
                    ELSE COALESCE(excluded.album_artist, albums.album_artist)
                END,
                year = CASE
                    WHEN albums.match_status IN ('matched', 'user_edited') THEN albums.year
                    ELSE COALESCE(albums.year, excluded.year)
                END,
                confidence = MAX(albums.confidence, excluded.confidence),
                updated_at = excluded.updated_at
            "#,
            params![
                seed.title,
                seed.album_artist,
                seed.sort_key,
                seed.year,
                seed.confidence,
                seed.match_status,
                now
            ],
        )
        .map_err(|e| format!("upsert album: {e}"))?;
        conn.query_row(
            "SELECT id FROM albums WHERE sort_key = ?1",
            [&seed.sort_key],
            |row| row.get(0),
        )
        .map_err(|e| format!("select album: {e}"))
    }

    // Track upserts mirror file metadata and tag columns collected during scanning.
    #[allow(clippy::too_many_arguments)]
    fn upsert_track_with_conn(
        conn: &Connection,
        path: &str,
        file_name: &str,
        size_bytes: i64,
        modified_secs: i64,
        title: &str,
        artist: Option<&str>,
        album: Option<&str>,
        album_artist: Option<&str>,
        track_number: Option<i64>,
        disc_number: Option<i64>,
        year: Option<i32>,
        genre: Option<&str>,
        composer: Option<&str>,
        duration_secs: Option<f64>,
        sample_rate: Option<i64>,
        bit_depth: Option<i64>,
        channels: Option<i64>,
        format: Option<&str>,
        album_id: i64,
        art_id: Option<i64>,
        embedded_art: bool,
    ) -> Result<(), String> {
        let now = now_secs();
        conn.execute(
            r#"
            INSERT INTO tracks (
                path, file_name, size_bytes, modified_secs, title, artist, album, album_artist,
                track_number, disc_number, year, genre, composer, duration_secs, sample_rate,
                bit_depth, channels, format, album_id, art_id, embedded_art, created_at, updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?22)
            ON CONFLICT(path) DO UPDATE SET
                file_name = excluded.file_name,
                size_bytes = excluded.size_bytes,
                modified_secs = excluded.modified_secs,
                title = CASE
                    WHEN EXISTS (
                        SELECT 1 FROM albums a
                        WHERE a.id = tracks.album_id
                          AND a.match_status IN ('matched', 'user_edited')
                    ) THEN tracks.title
                    ELSE excluded.title
                END,
                artist = CASE
                    WHEN EXISTS (
                        SELECT 1 FROM albums a
                        WHERE a.id = tracks.album_id
                          AND a.match_status IN ('matched', 'user_edited')
                    ) THEN tracks.artist
                    ELSE excluded.artist
                END,
                album = CASE
                    WHEN EXISTS (
                        SELECT 1 FROM albums a
                        WHERE a.id = tracks.album_id
                          AND a.match_status IN ('matched', 'user_edited')
                    ) THEN tracks.album
                    ELSE excluded.album
                END,
                album_artist = CASE
                    WHEN EXISTS (
                        SELECT 1 FROM albums a
                        WHERE a.id = tracks.album_id
                          AND a.match_status IN ('matched', 'user_edited')
                    ) THEN tracks.album_artist
                    ELSE excluded.album_artist
                END,
                track_number = CASE
                    WHEN EXISTS (
                        SELECT 1 FROM albums a
                        WHERE a.id = tracks.album_id
                          AND a.match_status IN ('matched', 'user_edited')
                    ) THEN tracks.track_number
                    ELSE excluded.track_number
                END,
                disc_number = CASE
                    WHEN EXISTS (
                        SELECT 1 FROM albums a
                        WHERE a.id = tracks.album_id
                          AND a.match_status IN ('matched', 'user_edited')
                    ) THEN tracks.disc_number
                    ELSE excluded.disc_number
                END,
                year = CASE
                    WHEN EXISTS (
                        SELECT 1 FROM albums a
                        WHERE a.id = tracks.album_id
                          AND a.match_status IN ('matched', 'user_edited')
                    ) THEN tracks.year
                    ELSE excluded.year
                END,
                genre = excluded.genre,
                composer = excluded.composer,
                duration_secs = excluded.duration_secs,
                sample_rate = excluded.sample_rate,
                bit_depth = excluded.bit_depth,
                channels = excluded.channels,
                format = excluded.format,
                album_id = CASE
                    WHEN EXISTS (
                        SELECT 1 FROM albums a
                        WHERE a.id = tracks.album_id
                          AND a.match_status IN ('matched', 'user_edited')
                    ) THEN tracks.album_id
                    ELSE excluded.album_id
                END,
                art_id = excluded.art_id,
                embedded_art = excluded.embedded_art,
                status = 'available',
                missing_since = NULL,
                updated_at = excluded.updated_at
            "#,
            params![
                path,
                file_name,
                size_bytes,
                modified_secs,
                title,
                artist,
                album,
                album_artist,
                track_number,
                disc_number,
                year,
                genre,
                composer,
                duration_secs,
                sample_rate,
                bit_depth,
                channels,
                format,
                album_id,
                art_id,
                if embedded_art { 1 } else { 0 },
                now
            ],
        )
        .map_err(|e| format!("upsert track: {e}"))?;
        let track_id: i64 = conn
            .query_row("SELECT id FROM tracks WHERE path = ?1", [path], |row| {
                row.get(0)
            })
            .map_err(|e| format!("select track: {e}"))?;
        let _ = conn.execute("DELETE FROM tracks_fts WHERE track_id = ?1", [track_id]);
        let _ = conn.execute(
            r#"
            INSERT INTO tracks_fts (track_id, title, artist, album, album_artist, composer, genre, file_name)
            SELECT id, title, artist, album, album_artist, composer, genre, file_name
            FROM tracks
            WHERE id = ?1
            "#,
            [track_id],
        );
        Ok(())
    }

    pub(super) fn upsert_artist(&self, name: &str) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        Self::upsert_artist_with_conn(&conn, name)
    }

    fn upsert_artist_with_conn(conn: &Connection, name: &str) -> Result<(), String> {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Ok(());
        }
        let now = now_secs();
        conn.execute(
            "INSERT OR IGNORE INTO artists (name, sort_name, created_at) VALUES (?1, ?2, ?3)",
            params![trimmed, normalize_key(trimmed), now],
        )
        .map_err(|e| format!("upsert artist: {e}"))?;
        Ok(())
    }

    fn album_id_for_track_path_with_conn(
        conn: &Connection,
        path: &str,
    ) -> Result<Option<i64>, String> {
        conn.query_row(
            "SELECT album_id FROM tracks WHERE path = ?1",
            [path],
            |row| row.get::<_, Option<i64>>(0),
        )
        .optional()
        .map(|opt| opt.flatten())
        .map_err(|e| format!("album lookup for track: {e}"))
    }

    fn track_needs_path_album_refresh_with_conn(
        conn: &Connection,
        path: &str,
        fallback: &PathAlbumFallback,
    ) -> Result<bool, String> {
        let has_disc_folder = fallback.disc_folder_name.is_some();
        let parsed_folder = fallback
            .title
            .as_deref()
            .and_then(parse_artist_album_folder);
        if !has_disc_folder && fallback.disc_number.is_none() && parsed_folder.is_none() {
            return Ok(false);
        }

        let row: Option<PathAlbumRefreshRow> = conn
            .query_row(
                r#"
                SELECT t.album, t.album_artist, t.disc_number, a.title, a.album_artist, a.confidence, a.match_status
                FROM tracks t
                LEFT JOIN albums a ON a.id = t.album_id
                WHERE t.path = ?1
                "#,
                [path],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                    ))
                },
            )
            .optional()
            .map_err(|e| format!("path album refresh lookup: {e}"))?;

        let Some((
            track_album,
            track_album_artist,
            disc_number,
            album_title,
            album_artist,
            album_confidence,
            album_match_status,
        )) = row
        else {
            return Ok(false);
        };
        let low_confidence_path_guess = album_confidence.unwrap_or(0) <= 45;
        let reviewable_album = album_match_status
            .as_deref()
            .is_none_or(|status| !matches!(status, "matched" | "user_edited" | "user_confirmed"));
        if let Some((parsed_artist, parsed_album)) = parsed_folder {
            let missing_album_artist = track_album_artist
                .as_deref()
                .is_none_or(|value| value.trim().is_empty())
                && album_artist
                    .as_deref()
                    .is_none_or(|value| value.trim().is_empty());
            let stored_title_is_raw_folder = fallback.title.as_deref().is_some_and(|raw| {
                track_album
                    .as_deref()
                    .is_some_and(|value| normalize_key(value) == normalize_key(raw))
                    || album_title
                        .as_deref()
                        .is_some_and(|value| normalize_key(value) == normalize_key(raw))
            });
            let stored_title_is_parsed_album = track_album
                .as_deref()
                .is_some_and(|value| normalize_key(value) == normalize_key(&parsed_album))
                || album_title
                    .as_deref()
                    .is_some_and(|value| normalize_key(value) == normalize_key(&parsed_album));
            let stored_artist_is_missing_or_different = missing_album_artist
                || album_artist
                    .as_deref()
                    .is_none_or(|value| normalize_key(value) != normalize_key(&parsed_artist));
            if reviewable_album
                && (stored_title_is_raw_folder
                    || (stored_title_is_parsed_album && stored_artist_is_missing_or_different))
            {
                return Ok(true);
            }
        }

        let missing_disc_number = fallback.disc_number.is_some() && disc_number.is_none();
        let Some(disc_folder_name) = fallback.disc_folder_name.as_deref() else {
            return Ok(missing_disc_number);
        };
        let disc_key = normalize_key(disc_folder_name);
        let stored_album_is_disc_folder = track_album
            .as_deref()
            .is_some_and(|value| normalize_key(value) == disc_key)
            || album_title
                .as_deref()
                .is_some_and(|value| normalize_key(value) == disc_key);
        let path_has_better_album_title = fallback
            .title
            .as_deref()
            .is_some_and(|value| normalize_key(value) != disc_key);

        Ok(missing_disc_number
            || (stored_album_is_disc_folder
                && path_has_better_album_title
                && low_confidence_path_guess))
    }

    #[allow(clippy::too_many_arguments)]
    fn rebind_moved_track_with_conn(
        conn: &Connection,
        seen_paths: &HashSet<String>,
        new_path: &str,
        new_file_name: &str,
        size_bytes: i64,
        modified_secs: i64,
        title: &str,
        album: Option<&str>,
        album_artist: Option<&str>,
        track_number: Option<i64>,
        disc_number: Option<i64>,
        duration_secs: Option<f64>,
    ) -> Result<(), String> {
        let already_exists: Option<i64> = conn
            .query_row("SELECT id FROM tracks WHERE path = ?1", [new_path], |row| {
                row.get(0)
            })
            .optional()
            .map_err(|e| format!("moved track existing path lookup: {e}"))?;
        if already_exists.is_some() {
            return Ok(());
        }

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, path, title, album, album_artist, track_number, disc_number, duration_secs
                FROM tracks
                WHERE path != ?1
                  AND size_bytes = ?2
                  AND modified_secs = ?3
                "#,
            )
            .map_err(|e| format!("moved track candidate query: {e}"))?;
        let rows = stmt
            .query_map(params![new_path, size_bytes, modified_secs], |row| {
                Ok(MoveCandidate {
                    id: row.get(0)?,
                    path: row.get(1)?,
                    title: row.get(2)?,
                    album: row.get(3)?,
                    album_artist: row.get(4)?,
                    track_number: row.get(5)?,
                    disc_number: row.get(6)?,
                    duration_secs: row.get(7)?,
                })
            })
            .map_err(|e| format!("moved track candidate map: {e}"))?;
        let mut best: Option<(i64, i64)> = None;
        for row in rows {
            let candidate = row.map_err(|e| format!("moved track candidate row: {e}"))?;
            if seen_paths.contains(&candidate.path)
                || candidate_path_exists_elsewhere(&candidate.path, new_path)
            {
                continue;
            }
            let score = if candidate_paths_refer_to_same_file(&candidate.path, new_path) {
                100
            } else {
                moved_track_match_score(
                    &candidate,
                    title,
                    album,
                    album_artist,
                    track_number,
                    disc_number,
                    duration_secs,
                )
            };
            if score >= 80 && best.is_none_or(|(_, best_score)| score > best_score) {
                best = Some((candidate.id, score));
            }
        }
        drop(stmt);

        let Some((track_id, _score)) = best else {
            return Ok(());
        };
        conn.execute(
            r#"
            UPDATE tracks
            SET path = ?2,
                file_name = ?3,
                status = 'available',
                missing_since = NULL,
                updated_at = ?4
            WHERE id = ?1
            "#,
            params![track_id, new_path, new_file_name, now_secs()],
        )
        .map_err(|e| format!("rebind moved track: {e}"))?;
        Ok(())
    }

    fn remove_missing_tracks_with_conn(
        conn: &Connection,
        seen_paths: &HashSet<String>,
    ) -> Result<usize, String> {
        let mut stmt = conn
            .prepare("SELECT id, path FROM tracks")
            .map_err(|e| format!("list existing tracks: {e}"))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| format!("map existing tracks: {e}"))?;
        let mut removed = 0;
        let mut stale_ids = Vec::new();
        for row in rows {
            let (id, path) = row.map_err(|e| format!("existing track row: {e}"))?;
            if !seen_paths.contains(&path) {
                stale_ids.push(id);
            }
        }
        drop(stmt);
        let now = now_secs();
        for id in stale_ids {
            removed += conn
                .execute(
                    r#"
                    UPDATE tracks
                    SET status = 'missing',
                        missing_since = COALESCE(missing_since, ?2),
                        updated_at = ?2
                    WHERE id = ?1
                      AND COALESCE(status, 'available') != 'missing'
                    "#,
                    params![id, now],
                )
                .map_err(|e| format!("mark stale track missing: {e}"))?;
        }
        Ok(removed)
    }

    fn refresh_album_rollups_with_conn(conn: &Connection) -> Result<(), String> {
        Self::merge_split_disc_album_titles_with_conn(conn)?;
        conn.execute_batch(
            r#"
            UPDATE albums
            SET track_count = (
                    SELECT COUNT(*)
                    FROM tracks
                    WHERE tracks.album_id = albums.id
                      AND COALESCE(tracks.status, 'available') = 'available'
                ),
                art_id = COALESCE(
                    art_id,
                    (
                        SELECT art_id
                        FROM tracks
                        WHERE tracks.album_id = albums.id
                          AND COALESCE(tracks.status, 'available') = 'available'
                          AND art_id IS NOT NULL
                        LIMIT 1
                    )
                ),
                updated_at = strftime('%s','now');
            DELETE FROM albums WHERE NOT EXISTS (SELECT 1 FROM tracks WHERE tracks.album_id = albums.id);
        "#,
        )
        .map_err(|e| format!("refresh album rollups: {e}"))?;
        Self::sync_local_versions_with_conn(conn)?;
        Ok(())
    }

    fn merge_split_disc_album_titles_with_conn(conn: &Connection) -> Result<(), String> {
        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, title, album_artist, year, confidence, match_status, art_id
                FROM albums
                WHERE track_count > 0
                "#,
            )
            .map_err(|e| format!("query split-disc albums: {e}"))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<i32>>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Option<i64>>(6)?,
                ))
            })
            .map_err(|e| format!("map split-disc albums: {e}"))?;

        let mut groups: HashMap<String, Vec<SplitDiscAlbumRow>> = HashMap::new();
        for row in rows {
            let (id, title, album_artist, year, confidence, match_status, art_id) =
                row.map_err(|e| format!("split-disc album row: {e}"))?;
            if is_protected_album_status(&match_status) {
                continue;
            }
            let Some((base_title, disc_number)) = parse_album_title_disc_suffix(&title) else {
                continue;
            };
            let artist_key = album_artist
                .as_deref()
                .map(normalize_key)
                .unwrap_or_else(|| "unknown-artist".to_string());
            let group_key = format!("{}|{}", artist_key, normalize_key(&base_title));
            groups
                .entry(group_key)
                .or_default()
                .push(SplitDiscAlbumRow {
                    id,
                    album_artist,
                    year,
                    confidence,
                    match_status,
                    art_id,
                    base_title,
                    disc_number,
                });
        }
        drop(stmt);

        for (sort_key, mut group) in groups {
            let unique_discs: HashSet<i64> = group.iter().map(|row| row.disc_number).collect();
            if unique_discs.len() < 2 {
                continue;
            }
            group.sort_by_key(|row| (row.disc_number, row.id));
            let Some(first) = group.first().cloned() else {
                continue;
            };
            let existing_target = conn
                .query_row(
                    "SELECT id, match_status FROM albums WHERE sort_key = ?1",
                    [&sort_key],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()
                .map_err(|e| format!("split-disc target lookup: {e}"))?;
            if existing_target
                .as_ref()
                .is_some_and(|(_, status)| is_protected_album_status(status))
            {
                continue;
            }
            let target_id = existing_target.map(|(id, _)| id).unwrap_or(first.id);
            let best_confidence = group
                .iter()
                .map(|row| row.confidence)
                .max()
                .unwrap_or(first.confidence);
            let year = first.year.or_else(|| group.iter().find_map(|row| row.year));
            let art_id = first
                .art_id
                .or_else(|| group.iter().find_map(|row| row.art_id));
            let match_status = if group.iter().any(|row| row.match_status == "local") {
                "local"
            } else {
                first.match_status.as_str()
            };

            conn.execute(
                r#"
                UPDATE albums
                SET title = ?2,
                    album_artist = ?3,
                    sort_key = ?4,
                    year = COALESCE(year, ?5),
                    confidence = MAX(confidence, ?6),
                    match_status = ?7,
                    art_id = COALESCE(art_id, ?8),
                    updated_at = strftime('%s','now')
                WHERE id = ?1
                  AND match_status NOT IN ('matched', 'user_edited', 'user_confirmed')
                "#,
                params![
                    target_id,
                    first.base_title.as_str(),
                    first.album_artist.as_deref(),
                    sort_key.as_str(),
                    year,
                    best_confidence,
                    match_status,
                    art_id,
                ],
            )
            .map_err(|e| format!("update split-disc target album: {e}"))?;

            for row in &group {
                conn.execute(
                    r#"
                    UPDATE tracks
                    SET album_id = ?2,
                        album = ?3,
                        album_artist = COALESCE(album_artist, ?4),
                        disc_number = COALESCE(disc_number, ?5),
                        updated_at = strftime('%s','now')
                    WHERE album_id = ?1
                    "#,
                    params![
                        row.id,
                        target_id,
                        first.base_title.as_str(),
                        first.album_artist.as_deref(),
                        row.disc_number
                    ],
                )
                .map_err(|e| format!("merge split-disc tracks: {e}"))?;
                let _ = conn.execute("DELETE FROM match_candidates WHERE album_id = ?1", [row.id]);
            }
            let _ = conn.execute(
                "DELETE FROM tracks_fts WHERE track_id IN (SELECT id FROM tracks WHERE album_id = ?1)",
                [target_id],
            );
            let _ = conn.execute(
                r#"
                INSERT INTO tracks_fts (track_id, title, artist, album, album_artist, composer, genre, file_name)
                SELECT id, title, artist, album, album_artist, composer, genre, file_name
                FROM tracks
                WHERE album_id = ?1
                "#,
                [target_id],
            );
            let _ = conn.execute(
                "DELETE FROM match_candidates WHERE album_id = ?1",
                [target_id],
            );

            for row in group.iter().filter(|row| row.id != target_id) {
                let _ = conn.execute("DELETE FROM albums WHERE id = ?1", [row.id]);
            }
        }

        Ok(())
    }
}

fn collect_audio_files<F>(root: &Path, mut on_progress: F) -> Result<Vec<PathBuf>, String>
where
    F: FnMut(usize, &Path),
{
    // The configured root is the trust boundary. Symlinks may make an album
    // layout more convenient, but they must not expand a scan into arbitrary
    // filesystem locations. Canonical directory identities also stop cycles
    // and ensure aliases are traversed only once.
    let canonical_root =
        fs::canonicalize(root).map_err(|e| format!("resolve music root {:?}: {e}", root))?;
    let mut files = Vec::new();
    let mut stack = vec![canonical_root.clone()];
    let mut visited_dirs = HashSet::new();
    while let Some(dir) = stack.pop() {
        let Ok(dir) = fs::canonicalize(&dir) else {
            continue;
        };
        if !dir.starts_with(&canonical_root) || !visited_dirs.insert(dir.clone()) {
            continue;
        }
        for entry in fs::read_dir(&dir).map_err(|e| format!("read music dir {:?}: {e}", dir))? {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => continue,
            };
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(_) => continue,
            };
            let path = entry.path();
            if file_type.is_dir() {
                stack.push(path);
            } else if file_type.is_symlink() {
                let Ok(resolved) = fs::canonicalize(&path) else {
                    continue;
                };
                if !resolved.starts_with(&canonical_root) {
                    continue;
                }
                if resolved.is_dir() {
                    stack.push(resolved);
                } else if is_supported_audio(&resolved) {
                    files.push(resolved);
                    if files.len() % PREPARE_PROGRESS_INTERVAL == 0 {
                        on_progress(files.len(), files.last().unwrap());
                    }
                }
            } else if is_supported_audio(&path) {
                files.push(path);
                if files.len() % PREPARE_PROGRESS_INTERVAL == 0 {
                    on_progress(files.len(), files.last().unwrap());
                }
            }
        }
    }
    files.sort();
    files.dedup();
    Ok(files)
}

#[cfg(all(test, unix))]
mod collect_audio_file_tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

    struct TestTree(PathBuf);

    impl TestTree {
        fn new(label: &str) -> Self {
            let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("fozmo-scanner-{label}-{}-{id}", std::process::id()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestTree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn recursive_scan_follows_internal_directory_symlinks_once_without_cycles() {
        let tree = TestTree::new("cycle");
        let root = tree.0.join("music");
        let album = root.join("Artist/Album");
        fs::create_dir_all(&album).unwrap();
        let track = album.join("song.flac");
        fs::write(&track, b"not-real-audio").unwrap();
        symlink(&album, root.join("album-alias")).unwrap();
        symlink(&root, album.join("back-to-root")).unwrap();

        let files = collect_audio_files(&root, |_, _| {}).unwrap();

        assert_eq!(files, vec![fs::canonicalize(track).unwrap()]);
    }

    #[test]
    fn recursive_scan_confines_file_and_directory_symlinks_to_music_root() {
        let tree = TestTree::new("confinement");
        let root = tree.0.join("music");
        let outside = tree.0.join("outside");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&outside).unwrap();
        let local = root.join("local.wav");
        let secret = outside.join("secret.flac");
        fs::write(&local, b"local").unwrap();
        fs::write(&secret, b"outside").unwrap();
        symlink(&outside, root.join("outside-dir")).unwrap();
        symlink(&secret, root.join("outside-file.flac")).unwrap();

        let files = collect_audio_files(&root, |_, _| {}).unwrap();

        assert_eq!(files, vec![fs::canonicalize(local).unwrap()]);
    }
}

fn is_supported_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|ext| SUPPORTED_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

fn apply_folder_album_art_once(
    conn: &Connection,
    applied_folder_art: &mut HashSet<(i64, i64)>,
    album_id: i64,
    art_id: i64,
) -> Result<(), String> {
    if applied_folder_art.insert((album_id, art_id)) {
        Library::set_album_art_with_conn(conn, album_id, art_id)?;
    }
    Ok(())
}

fn moved_track_match_score(
    candidate: &MoveCandidate,
    title: &str,
    album: Option<&str>,
    album_artist: Option<&str>,
    track_number: Option<i64>,
    disc_number: Option<i64>,
    duration_secs: Option<f64>,
) -> i64 {
    let mut score = 0;
    if normalize_key(&candidate.title) == normalize_key(title) {
        score += 35;
    }
    if option_normalized_eq(candidate.album.as_deref(), album) {
        score += 18;
    }
    if option_normalized_eq(candidate.album_artist.as_deref(), album_artist) {
        score += 12;
    }
    if candidate.track_number.is_some() && candidate.track_number == track_number {
        score += 18;
    }
    if candidate.disc_number == disc_number {
        score += 7;
    }
    match (candidate.duration_secs, duration_secs) {
        (Some(left), Some(right)) if (left - right).abs() <= 0.5 => score += 10,
        (None, None) => score += 10,
        _ => {}
    }
    score
}

fn option_normalized_eq(left: Option<&str>, right: Option<&str>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => normalize_key(left) == normalize_key(right),
        (None, None) => true,
        _ => false,
    }
}

fn candidate_path_exists_elsewhere(candidate_path: &str, new_path: &str) -> bool {
    let candidate = Path::new(candidate_path);
    if !candidate.exists() {
        return false;
    }
    !candidate_paths_refer_to_same_file(candidate_path, new_path)
}

fn candidate_paths_refer_to_same_file(candidate_path: &str, new_path: &str) -> bool {
    let candidate = Path::new(candidate_path);
    let Ok(candidate_canonical) = fs::canonicalize(candidate) else {
        return false;
    };
    let Ok(new_canonical) = fs::canonicalize(new_path) else {
        return false;
    };
    candidate_canonical == new_canonical
}

pub(super) fn modified_secs(metadata: &fs::Metadata) -> Option<i64> {
    metadata
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs() as i64)
}

pub(super) fn album_seed_for_path(
    music_dir: &Path,
    path: &Path,
    album: &Option<String>,
    album_artist: &Option<String>,
    artist: &Option<String>,
    year: Option<i32>,
) -> AlbumSeed {
    let fallback = path_album_fallback(music_dir, path);
    let embedded_album = album.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty());
    let parsed_folder = fallback
        .title
        .as_deref()
        .and_then(parse_artist_album_folder);
    let parsed_embedded_album =
        embedded_album.and_then(|value| parse_artist_album_tag(value, album_artist, artist));
    let parsed_folder_matches_embedded_title = embedded_album.is_none_or(|value| {
        parsed_folder
            .as_ref()
            .is_some_and(|(_, album)| normalize_key(value) == normalize_key(album))
    });
    let parsed_artist_album = parsed_embedded_album.as_ref().or_else(|| {
        parsed_folder_matches_embedded_title
            .then_some(parsed_folder.as_ref())
            .flatten()
    });
    let title = embedded_album
        .and_then(|s| {
            if parsed_embedded_album.is_some() {
                None
            } else {
                Some(s.to_string())
            }
        })
        .or_else(|| parsed_artist_album.map(|(_, album)| album.clone()))
        .or(fallback.title.clone())
        .unwrap_or_else(|| "Unknown Album".to_string());
    let album_artist = album_artist
        .clone()
        .or_else(|| artist.clone())
        .or_else(|| parsed_artist_album.map(|(artist, _)| artist.clone()))
        .filter(|s| !s.trim().is_empty());
    let confidence = if embedded_album.is_some() && album_artist.is_some() {
        80
    } else if embedded_album.is_some() {
        62
    } else if fallback.title.is_some() {
        45
    } else {
        20
    };
    let match_status = if confidence >= 80 {
        "local"
    } else {
        "needs_review"
    }
    .to_string();
    let artist_key = album_artist
        .as_deref()
        .map(normalize_key)
        .unwrap_or_else(|| "unknown-artist".to_string());
    let folder_key = path
        .parent()
        .map(|p| normalize_key(&p.to_string_lossy()))
        .unwrap_or_else(|| "root".to_string());
    let sort_key = if title == "Unknown Album" {
        format!("unknown|{folder_key}")
    } else {
        format!("{}|{}", artist_key, normalize_key(&title))
    };
    AlbumSeed {
        title,
        album_artist,
        year,
        confidence,
        match_status,
        sort_key,
    }
}

fn parse_artist_album_tag(
    value: &str,
    album_artist: &Option<String>,
    artist: &Option<String>,
) -> Option<(String, String)> {
    let parsed = parse_artist_album_folder(value)?;
    let known_artist = album_artist
        .as_deref()
        .or(artist.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if known_artist.is_some_and(|artist| normalize_key(artist) != normalize_key(&parsed.0)) {
        return None;
    }
    Some(parsed)
}

pub(super) fn parse_artist_album_folder(value: &str) -> Option<(String, String)> {
    for (idx, ch) in value.char_indices() {
        if !matches!(ch, '-' | '–' | '—') {
            continue;
        }
        let before = value[..idx].chars().next_back();
        let after = value[idx + ch.len_utf8()..].chars().next();
        if before.is_some_and(char::is_whitespace) || after.is_some_and(char::is_whitespace) {
            let artist = compact_folder_text(&value[..idx]);
            let album = clean_folder_album_title(&value[idx + ch.len_utf8()..]);
            if !artist.is_empty() && !album.is_empty() {
                return Some((artist, album));
            }
        }
    }
    None
}

fn clean_folder_album_title(value: &str) -> String {
    let mut text = value.replace('_', " ");
    loop {
        let trimmed = text.trim_end().to_string();
        let Some(last) = trimmed.chars().last() else {
            return String::new();
        };
        let (open, close) = match last {
            ')' => ('(', ')'),
            ']' => ('[', ']'),
            _ => break,
        };
        let Some(open_idx) = trimmed.rfind(open) else {
            break;
        };
        let suffix = trimmed[open_idx + open.len_utf8()..trimmed.len() - close.len_utf8()].trim();
        if !is_technical_folder_suffix(suffix) {
            break;
        }
        text = trimmed[..open_idx].to_string();
    }
    compact_folder_text(&text)
}

fn is_technical_folder_suffix(value: &str) -> bool {
    let normalized = super::matching::normalize_for_match(value);
    let tokens: Vec<&str> = normalized.split_whitespace().collect();
    if tokens.is_empty() {
        return false;
    }
    let has_format_marker = tokens.iter().any(|token| {
        matches!(
            *token,
            "wav"
                | "wave"
                | "flac"
                | "alac"
                | "aiff"
                | "aif"
                | "mp3"
                | "m4a"
                | "dsd"
                | "dsf"
                | "bit"
                | "bits"
                | "khz"
                | "hz"
                | "hi"
                | "res"
                | "hires"
                | "lossless"
        )
    });
    has_format_marker
        && tokens.iter().all(|token| {
            token.parse::<i64>().is_ok()
                || matches!(
                    *token,
                    "wav"
                        | "wave"
                        | "flac"
                        | "alac"
                        | "aiff"
                        | "aif"
                        | "mp3"
                        | "m4a"
                        | "dsd"
                        | "dsf"
                        | "bit"
                        | "bits"
                        | "khz"
                        | "hz"
                        | "hi"
                        | "res"
                        | "hires"
                        | "lossless"
                )
        })
}

fn compact_folder_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(super) fn path_album_fallback(music_dir: &Path, path: &Path) -> PathAlbumFallback {
    let parent = path.parent().filter(|parent| parent != &music_dir);
    let parent_name = parent
        .and_then(|parent| parent.file_name())
        .and_then(|n| n.to_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let parent_dir = parent.map(Path::to_path_buf);
    let mut fallback = PathAlbumFallback {
        title: parent_name.clone(),
        album_dir: parent_dir.clone(),
        disc_number: None,
        disc_folder_name: None,
    };

    let Some(parent_name) = parent_name else {
        return fallback;
    };
    let Some(disc_number) = parse_disc_folder_number(&parent_name) else {
        return fallback;
    };

    fallback.disc_number = Some(disc_number);
    fallback.disc_folder_name = Some(parent_name);
    if let Some(album_dir) = parent
        .and_then(Path::parent)
        .filter(|album_dir| album_dir != &music_dir)
        && let Some(album_name) = album_dir
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
    {
        fallback.title = Some(album_name.to_string());
        fallback.album_dir = Some(album_dir.to_path_buf());
    }

    fallback
}

pub(super) fn path_album_fallback_for_dirs(
    music_dirs: &[PathBuf],
    path: &Path,
) -> PathAlbumFallback {
    if let Some(music_dir) = matching_music_dir(music_dirs, path) {
        path_album_fallback(&music_dir, path)
    } else {
        PathAlbumFallback {
            title: path
                .parent()
                .and_then(|parent| parent.file_name())
                .and_then(|n| n.to_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
            album_dir: path.parent().map(Path::to_path_buf),
            disc_number: path
                .parent()
                .and_then(|parent| parent.file_name())
                .and_then(|n| n.to_str())
                .and_then(parse_disc_folder_number),
            disc_folder_name: path
                .parent()
                .and_then(|parent| parent.file_name())
                .and_then(|n| n.to_str())
                .map(|s| s.trim().to_string())
                .filter(|s| parse_disc_folder_number(s).is_some()),
        }
    }
}

fn matching_music_dir(music_dirs: &[PathBuf], path: &Path) -> Option<PathBuf> {
    if let Some(music_dir) = music_dirs
        .iter()
        .map(PathBuf::as_path)
        .filter(|dir| path.starts_with(dir))
        .max_by_key(|dir| dir.components().count())
    {
        return Some(music_dir.to_path_buf());
    }

    music_dirs
        .iter()
        .filter_map(|dir| fs::canonicalize(dir).ok())
        .filter(|dir| path.starts_with(dir))
        .max_by_key(|dir| dir.components().count())
}

fn parse_disc_folder_number(name: &str) -> Option<i64> {
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

fn parse_album_title_disc_suffix(title: &str) -> Option<(String, i64)> {
    let trimmed = title.trim();
    let (open, close) = match trimmed.chars().last()? {
        ')' => ('(', ')'),
        ']' => ('[', ']'),
        _ => return None,
    };
    let open_idx = trimmed.rfind(open)?;
    let suffix = trimmed[open_idx + open.len_utf8()..trimmed.len() - close.len_utf8()].trim();
    let disc_number = parse_disc_folder_number(suffix)?;
    let base = compact_folder_text(trimmed[..open_idx].trim());
    if base.is_empty() {
        return None;
    }
    Some((base, disc_number))
}

fn is_protected_album_status(status: &str) -> bool {
    matches!(status, "matched" | "user_edited" | "user_confirmed")
}

pub(super) fn folder_cover_dirs_for_path(
    path: &Path,
    fallback: &PathAlbumFallback,
) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(album_dir) = fallback.album_dir.as_ref() {
        dirs.push(album_dir.clone());
    }
    if let Some(parent) = path.parent()
        && !dirs.iter().any(|dir| dir == parent)
    {
        dirs.push(parent.to_path_buf());
    }
    dirs
}

pub(super) fn title_from_file_name(name: &str) -> String {
    let stem = name.rsplit_once('.').map(|(stem, _)| stem).unwrap_or(name);
    let trimmed = stem.trim();
    let digit_end = trimmed
        .char_indices()
        .take_while(|(_, character)| character.is_ascii_digit())
        .map(|(index, character)| index + character.len_utf8())
        .last()
        .unwrap_or(0);
    let remainder = &trimmed[digit_end..];
    let without_number = if digit_end > 0
        && remainder.starts_with(|character: char| {
            character == '.' || character == '-' || character == '_' || character == ' '
        }) {
        remainder.trim_start_matches(|character: char| {
            character == '.' || character == '-' || character == '_' || character == ' '
        })
    } else {
        trimmed
    };
    if without_number.trim().is_empty() {
        trimmed.to_string()
    } else {
        without_number.trim().to_string()
    }
}
