use reqwest::Client;
use reqwest::redirect::Policy;
use rusqlite::Connection;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub(crate) use matching::normalize_for_match as normalize_library_match_key;
pub(super) use qobuz_sync::normalize_qobuz_album_id;

mod albums;
mod artists;
mod artwork;
mod autometa;
mod browse;
mod catalog;
mod favorites;
mod history_entries;
mod history_summary;
mod history_transfer;
mod itunes_art;
mod matching;
mod media;
mod metadata;
mod migrations;
#[cfg_attr(not(feature = "hegel"), allow(dead_code))]
mod model;
mod musicbrainz;
mod persistence;
mod playlists;
mod qobuz_sync;
mod queue_store;
mod recent_albums;
mod scanner;
#[cfg(test)]
mod tests;
mod tracks;
mod versions;
mod zones;

const USER_AGENT: &str = crate::app::identity::USER_AGENT;
pub(crate) use artwork::{MAX_ARTWORK_BYTES, safe_raster_artwork_mime, sanitize_raster_artwork};
pub(crate) use autometa::is_valid_musicbrainz_release_id;
type DatabaseJob = Box<dyn FnOnce() + Send + 'static>;

struct DatabaseWorker {
    sender: mpsc::Sender<DatabaseJob>,
}

impl DatabaseWorker {
    fn new() -> Result<Self, String> {
        let (sender, receiver) = mpsc::channel::<DatabaseJob>();
        std::thread::Builder::new()
            .name("fozmo-library-db".to_string())
            .spawn(move || {
                while let Ok(job) = receiver.recv() {
                    job();
                }
            })
            .map_err(|error| format!("start library database worker: {error}"))?;
        Ok(Self { sender })
    }

    fn send(&self, job: DatabaseJob) -> Result<(), String> {
        self.sender
            .send(job)
            .map_err(|_| "library database worker stopped".to_string())
    }
}

pub struct Library {
    conn: Mutex<Connection>,
    database_worker: DatabaseWorker,
    music_dirs: Mutex<Vec<PathBuf>>,
    scan_running: AtomicBool,
    scan_progress: Mutex<LibraryScanProgress>,
    autometa_progress: Mutex<AutoMetaProgress>,
    art_dir: PathBuf,
    thumbnail_cache_dir: PathBuf,
    http: Client,
    itunes_art_http: Client,
    last_mb_request: tokio::sync::Mutex<Option<Instant>>,
    last_itunes_request: tokio::sync::Mutex<Option<Instant>>,
}

pub use model::*;

impl Library {
    /// Send a synchronous library operation to the dedicated database worker
    /// and asynchronously await its typed result. Async request and monitor
    /// code must use this boundary rather than executing rusqlite work on a
    /// Tokio worker thread.
    pub async fn run_blocking<T, F>(self: &Arc<Self>, operation: F) -> Result<T, String>
    where
        T: Send + 'static,
        F: FnOnce(&Library) -> Result<T, String> + Send + 'static,
    {
        let library = Arc::clone(self);
        let (reply, response) = tokio::sync::oneshot::channel();
        self.database_worker.send(Box::new(move || {
            let _ = reply.send(operation(&library));
        }))?;
        response
            .await
            .map_err(|_| "library database worker dropped its reply".to_string())?
    }

    #[allow(dead_code)]
    pub fn new(
        db_path: PathBuf,
        music_dirs: Vec<PathBuf>,
        art_dir: PathBuf,
    ) -> Result<Self, String> {
        let thumbnail_cache_dir = art_dir.join("thumbnails");
        Self::open(db_path, music_dirs, art_dir, thumbnail_cache_dir)
    }

    /// Open the installed-app database, taking a validated pre-migration
    /// snapshot whenever its schema is older than this binary.
    pub fn new_managed(
        db_path: PathBuf,
        music_dirs: Vec<PathBuf>,
        art_dir: PathBuf,
        thumbnail_cache_dir: PathBuf,
        settings_path: &Path,
        backups_dir: &Path,
    ) -> Result<Self, String> {
        persistence::prepare_database_for_open(&db_path, settings_path, backups_dir)?;
        Self::open(db_path, music_dirs, art_dir, thumbnail_cache_dir)
    }

    fn open(
        db_path: PathBuf,
        music_dirs: Vec<PathBuf>,
        art_dir: PathBuf,
        thumbnail_cache_dir: PathBuf,
    ) -> Result<Self, String> {
        if let Some(parent) = db_path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("create library dir: {e}"))?;
        }
        fs::create_dir_all(&art_dir).map_err(|e| format!("create art dir: {e}"))?;
        fs::create_dir_all(&thumbnail_cache_dir)
            .map_err(|e| format!("create thumbnail cache dir: {e}"))?;
        let conn = Connection::open(&db_path).map_err(|e| format!("open library db: {e}"))?;
        conn.execute_batch(
            r#"
            PRAGMA foreign_keys = ON;
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = NORMAL;
            "#,
        )
        .map_err(|e| format!("configure library db: {e}"))?;
        // Numbered schema migrations are deliberately separate from the
        // housekeeping below. Managed databases reach this point only after
        // their schema has been migrated and verified in a sibling stage.
        migrations::migrate(&conn)?;
        let library = Self {
            conn: Mutex::new(conn),
            database_worker: DatabaseWorker::new()?,
            music_dirs: Mutex::new(music_dirs),
            scan_running: AtomicBool::new(false),
            scan_progress: Mutex::new(LibraryScanProgress::default()),
            autometa_progress: Mutex::new(AutoMetaProgress::default()),
            art_dir,
            thumbnail_cache_dir,
            // A connect + request timeout so a hung MusicBrainz call surfaces
            // as an error instead of leaving the UI spinner stuck forever.
            http: Client::builder()
                .user_agent(USER_AGENT)
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(20))
                .build()
                .map_err(|e| format!("http client: {e}"))?,
            itunes_art_http: Client::builder()
                .user_agent(USER_AGENT)
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(10))
                .redirect(Policy::none())
                .build()
                .map_err(|e| format!("itunes artwork http client: {e}"))?,
            last_mb_request: tokio::sync::Mutex::new(None),
            last_itunes_request: tokio::sync::Mutex::new(None),
        };
        library.run_post_schema_housekeeping()?;
        Ok(library)
    }

    fn active_profile_id(&self) -> String {
        crate::settings::DEFAULT_PROFILE_ID.to_string()
    }

    pub fn music_dirs(&self) -> Vec<PathBuf> {
        self.music_dirs.lock().unwrap().clone()
    }

    pub fn set_music_dirs(&self, music_dirs: Vec<PathBuf>) {
        let mut guard = self.music_dirs.lock().unwrap();
        *guard = music_dirs;
    }

    fn run_post_schema_housekeeping(&self) -> Result<(), String> {
        self.normalize_artwork_paths()?;
        self.backfill_missing_track_bit_depth()?;
        {
            let conn = self.conn.lock().unwrap();
            Self::sync_local_versions_with_conn(&conn)?;
        }
        self.recover_interrupted_autometa_jobs()?;

        Ok(())
    }

    pub fn current_schema_version() -> u32 {
        migrations::CURRENT_SCHEMA_VERSION
    }

    /// Flush WAL content into the main database. Safe to call repeatedly
    /// during graceful shutdown before the process releases the data lock.
    pub fn checkpoint(&self) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        let result: (i64, i64, i64) = conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .map_err(|error| format!("checkpoint library database: {error}"))?;
        if result.0 == 0 {
            Ok(())
        } else {
            Err(format!(
                "checkpoint library database remained busy ({} frames, {} checkpointed)",
                result.1, result.2
            ))
        }
    }

    /// Create a consistent SQLite/settings snapshot and retain the latest
    /// three validated snapshots.
    pub fn create_backup(
        &self,
        settings_path: &Path,
        backups_dir: &Path,
        reason: &str,
    ) -> Result<PathBuf, String> {
        let conn = self.conn.lock().unwrap();
        persistence::create_backup_from_connection(&conn, settings_path, backups_dir, reason)
    }

    pub fn summary(&self) -> Result<LibrarySummary, String> {
        let conn = self.conn.lock().unwrap();
        Ok(LibrarySummary {
            albums: conn
                .query_row("SELECT COUNT(*) FROM albums", [], |r| r.get(0))
                .map_err(|error| format!("count library albums: {error}"))?,
            artists: conn
                .query_row("SELECT COUNT(*) FROM artists", [], |r| r.get(0))
                .map_err(|error| format!("count library artists: {error}"))?,
            tracks: conn
                .query_row(
                    "SELECT COUNT(*) FROM tracks WHERE COALESCE(status, 'available') = 'available'",
                    [],
                    |r| r.get(0),
                )
                .map_err(|error| format!("count available library tracks: {error}"))?,
            unmatched_albums: conn
                .query_row(
                    "SELECT COUNT(*) FROM albums
                     WHERE match_status IN ('needs_review', 'unmatched', 'local')
                       AND COALESCE(qobuz_match_status, '') != 'matched'",
                    [],
                    |r| r.get(0),
                )
                .map_err(|error| format!("count unmatched library albums: {error}"))?,
        })
    }
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn normalize_volume(volume: Option<f32>) -> Option<f32> {
    volume
        .filter(|value| value.is_finite())
        .map(|value| value.clamp(0.0, 1.0))
}

fn clean_display_value(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn clean_artist_display_value(value: Option<String>) -> Option<String> {
    clean_display_value(value).filter(|value| {
        !matches!(
            value.to_ascii_lowercase().as_str(),
            "unknown" | "unknown artist" | "unknown artists"
        )
    })
}

fn clean_album_display_value(value: Option<String>) -> Option<String> {
    clean_display_value(value).filter(|value| {
        !matches!(
            value.to_ascii_lowercase().as_str(),
            "unknown" | "unknown album"
        )
    })
}

fn resolve_local_track_display_tags(
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
    track_album_artist: Option<String>,
    album_artist: Option<String>,
    album_title: Option<String>,
) -> (Option<String>, Option<String>, Option<String>) {
    (
        clean_display_value(title),
        clean_artist_display_value(artist)
            .or_else(|| clean_artist_display_value(track_album_artist))
            .or_else(|| clean_artist_display_value(album_artist)),
        clean_album_display_value(album).or_else(|| clean_album_display_value(album_title)),
    )
}

fn path_file_name_and_ext(path: &str) -> (Option<String>, Option<String>) {
    let path = Path::new(path);
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_string);
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.trim_start_matches('.').to_ascii_lowercase())
        .filter(|ext| !ext.is_empty());
    (file_name, ext)
}

fn normalize_key(input: &str) -> String {
    input
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn album_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AlbumSummary> {
    Ok(AlbumSummary {
        id: row.get(0)?,
        title: row.get(1)?,
        album_artist: row.get(2)?,
        year: row.get(3)?,
        track_count: row.get(4)?,
        art_id: row.get(5)?,
        confidence: row.get(6)?,
        match_status: row.get(7)?,
        primary_version_id: row.get(8)?,
        qobuz_album_id: row.get(9)?,
        qobuz_match_status: row.get(10)?,
        qobuz_match_confidence: row.get(11)?,
        canonical_art_id: row.get(12)?,
        original_year: row.get(13)?,
        mb_barcode: row.get(14)?,
        image_url: row.get(15)?,
    })
}

fn track_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TrackSummary> {
    Ok(TrackSummary {
        id: row.get(0)?,
        file_name: row.get(1)?,
        title: row.get(2)?,
        artist: row.get(3)?,
        album: row.get(4)?,
        album_artist: row.get(5)?,
        track_number: row.get(6)?,
        disc_number: row.get(7)?,
        year: row.get(8)?,
        genre: row.get(9)?,
        composer: row.get(10)?,
        duration_secs: row.get(11)?,
        sample_rate: row.get(12)?,
        bit_depth: row.get(13)?,
        channels: row.get(14)?,
        format: row.get(15)?,
        album_id: row.get(16)?,
        art_id: row.get(17)?,
        play_count: row.get(18)?,
        last_played_at: row.get(19)?,
        listened_secs: row.get(20)?,
        preferred_play_source: None,
    })
}

fn album_version_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AlbumVersionSummary> {
    Ok(AlbumVersionSummary {
        id: row.get(0)?,
        album_id: row.get(1)?,
        provider: row.get(2)?,
        provider_id: row.get(3)?,
        title: row.get(4)?,
        artist: row.get(5)?,
        year: row.get(6)?,
        track_count: row.get(7)?,
        art_id: row.get(8)?,
        format: row.get(9)?,
        sample_rate: row.get(10)?,
        bit_depth: row.get(11)?,
        source_label: row.get(12)?,
        status: row.get(13)?,
        is_primary: row.get::<_, i64>(14)? != 0,
        musicbrainz_match_status: row.get(15)?,
        musicbrainz_release_id: row.get(16)?,
        musicbrainz_tagged_at: row.get(17)?,
        qobuz_match_status: row.get(18)?,
        qobuz_tagged_at: row.get(19)?,
        autometa_message: row.get(20)?,
    })
}

fn collect_rows<T>(
    rows: rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>>,
) -> Result<Vec<T>, String> {
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| format!("row: {e}"))?);
    }
    Ok(out)
}

fn clean_optional_string(value: Option<String>) -> Option<String> {
    value
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}
