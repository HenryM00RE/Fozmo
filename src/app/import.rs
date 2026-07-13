use crate::app::paths::{AppPaths, atomic_write, write_install_metadata};
use crate::settings::PersistedSettings;
use rusqlite::{Connection, OpenFlags, params};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LegacyImportProgress {
    pub stage: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LegacyImportReport {
    pub source_workspace: String,
    pub destination_data_dir: String,
    pub copied_music_files: u64,
    pub copied_artwork_files: u64,
    pub copied_preset_files: u64,
    pub copied_font: bool,
    pub copied_remote_certificate: bool,
    #[serde(default)]
    pub copied_tls_files: u64,
    #[serde(default)]
    pub preserved_installation_id: bool,
    pub rebased_track_paths: u64,
    pub preserved_external_track_paths: u64,
    pub skipped_cache_directories: Vec<String>,
    pub source_preserved: bool,
    #[serde(default)]
    pub validation: LegacyImportValidation,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LegacyImportValidation {
    pub database_integrity_check_passed: bool,
    pub database_row_counts: Vec<LegacyImportTableValidation>,
    pub settings_validated: bool,
    pub copied_files_verified: u64,
    pub copied_bytes_verified: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LegacyImportTableValidation {
    pub table: String,
    pub source_rows: u64,
    pub staged_rows: u64,
}

#[derive(Debug, Clone, Copy, Default)]
struct CopyStats {
    files: u64,
    bytes: u64,
}

impl CopyStats {
    fn add(&mut self, other: Self) {
        self.files += other.files;
        self.bytes += other.bytes;
    }
}

const REPRESENTATIVE_TABLES: &[&str] = &[
    "tracks",
    "albums",
    "artworks",
    "recordings",
    "playback_history",
    "playlists",
    "playlist_items",
    "playback_zones",
    "zone_settings",
    "zone_queue_items",
    "now_playing_queues",
    "favorite_albums",
    "recently_played_albums",
];

pub fn import_legacy_workspace<F>(
    source_workspace: &Path,
    destination: &AppPaths,
    mut progress: F,
) -> Result<LegacyImportReport, String>
where
    F: FnMut(LegacyImportProgress),
{
    let source_workspace = source_workspace
        .canonicalize()
        .map_err(|error| format!("resolve legacy workspace {:?}: {error}", source_workspace))?;
    if destination.data_dir.exists() {
        return Err(format!(
            "destination data directory {:?} already exists; import is only allowed before Fozmo creates new state",
            destination.data_dir
        ));
    }
    if same_path(&source_workspace, &destination.data_dir) {
        return Err("legacy source and destination data directory are the same".to_string());
    }

    progress_event(
        &mut progress,
        "validate",
        "Checking that the legacy workspace is stopped",
    );
    let _source_lock = lock_legacy_workspace_if_supported(&source_workspace)?;
    let source_db = source_workspace.join("library").join("library.db");
    if !source_db.is_file() {
        return Err(format!(
            "legacy library database not found at {:?}",
            source_db
        ));
    }
    refuse_live_database_handles(&source_db)?;

    let parent = destination
        .data_dir
        .parent()
        .ok_or_else(|| format!("destination {:?} has no parent", destination.data_dir))?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("create destination parent {:?}: {error}", parent))?;
    let stage = parent.join(format!(
        ".{}-import-{}-{}",
        destination
            .data_dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("Fozmo"),
        std::process::id(),
        now_unix_nanos()
    ));
    fs::create_dir(&stage)
        .map_err(|error| format!("create import staging directory {:?}: {error}", stage))?;

    let result = import_into_stage(
        &source_workspace,
        &source_db,
        destination,
        &stage,
        &mut progress,
    );
    let mut report = match result {
        Ok(report) => report,
        Err(error) => {
            let _ = fs::remove_dir_all(&stage);
            return Err(error);
        }
    };

    progress_event(
        &mut progress,
        "commit",
        "Atomically installing imported data",
    );
    fs::rename(&stage, &destination.data_dir).map_err(|error| {
        let _ = fs::remove_dir_all(&stage);
        format!(
            "atomically install imported data at {:?}: {error}",
            destination.data_dir
        )
    })?;
    report.source_preserved = source_workspace.exists();
    progress_event(
        &mut progress,
        "complete",
        "Legacy workspace import completed",
    );
    Ok(report)
}

fn import_into_stage<F>(
    source_workspace: &Path,
    source_db: &Path,
    destination: &AppPaths,
    stage: &Path,
    progress: &mut F,
) -> Result<LegacyImportReport, String>
where
    F: FnMut(LegacyImportProgress),
{
    let stage_library = stage.join("library");
    let stage_art = stage_library.join("art");
    let stage_music = stage.join("music");
    let stage_presets = stage.join("presets");
    let stage_appearance = stage.join("appearance");
    let stage_tls = stage.join("tls");
    for directory in [
        &stage_library,
        &stage_art,
        &stage_music,
        &stage_presets,
        &stage_appearance,
        &stage_tls,
        &stage.join("backups"),
    ] {
        fs::create_dir_all(directory)
            .map_err(|error| format!("create import directory {:?}: {error}", directory))?;
    }

    let packaged_tls = source_workspace.join("tls");
    let legacy_tls = source_workspace.join("library").join("tls");
    let source_tls = if packaged_tls.is_dir() {
        &packaged_tls
    } else {
        &legacy_tls
    };

    progress_event(progress, "database", "Creating a WAL-aware SQLite snapshot");
    let stage_db = stage_library.join("library.db");
    let source_conn = Connection::open_with_flags(source_db, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| format!("open legacy database: {error}"))?;
    source_conn
        .execute("VACUUM INTO ?1", [stage_db.to_string_lossy().as_ref()])
        .map_err(|error| format!("snapshot legacy SQLite database: {error}"))?;
    validate_database(&stage_db)?;

    progress_event(progress, "settings", "Validating and rebasing settings");
    let source_settings = source_workspace.join("settings.json");
    let mut settings_validated = false;
    if source_settings.exists() {
        let body = fs::read_to_string(&source_settings)
            .map_err(|error| format!("read legacy settings: {error}"))?;
        let mut settings = crate::settings::parse_settings_read_only(&source_settings, &body)
            .map_err(|error| format!("legacy settings are invalid: {error}"))?;
        rebase_settings_paths(
            &mut settings,
            &source_workspace.join("music"),
            &destination.music_dir,
            source_tls,
            &destination.tls_dir,
        );
        settings.save(&stage.join("settings.json"))?;
        settings_validated = true;
    }

    progress_event(
        progress,
        "files",
        "Copying user music, artwork, presets and appearance data",
    );
    let copied_music =
        copy_tree_regular_files(&source_workspace.join("music"), &stage_music, |_| true)?;
    let copied_artwork = copy_tree_regular_files(
        &source_workspace.join("library").join("art"),
        &stage_art,
        |relative| !relative.starts_with("thumbnails"),
    )?;
    let copied_presets = copy_tree_regular_files(
        &source_workspace.join("presets"),
        &stage_presets,
        |relative| relative.extension().and_then(|ext| ext.to_str()) == Some("json"),
    )?;

    // Packaged data roots store durable appearance/TLS data directly under
    // Application Support. Older development workspaces kept the same data
    // below static/user-fonts and library/tls. Prefer the packaged layout
    // when it exists, but retain the legacy fallback for first DMG imports.
    let packaged_font = source_workspace
        .join("appearance")
        .join("custom-display.ttf");
    let legacy_font = source_workspace
        .join("static")
        .join("user-fonts")
        .join("custom-display.ttf");
    let source_font = if packaged_font.is_file() {
        &packaged_font
    } else {
        &legacy_font
    };
    let copied_font_stats =
        copy_optional_file(source_font, &stage_appearance.join("custom-display.ttf"))?;
    // Copy the complete durable TLS directory. The generated private key is
    // intentionally held in the OS secret store, while user-managed cert/key
    // files may live here and must move together with their rebased settings.
    let copied_tls = copy_tree_regular_files(source_tls, &stage_tls, |_| true)?;
    let copied_remote_certificate = stage_tls.join("remote-cert.pem").is_file();

    progress_event(
        progress,
        "rebase",
        "Rebasing managed paths while preserving external music paths",
    );
    let (rebased_track_paths, preserved_external_track_paths, copied_referenced_art) =
        rebase_database_paths(
            &stage_db,
            source_workspace,
            &stage_art,
            &destination.music_dir,
        )?;
    validate_database(&stage_db)?;

    progress_event(
        progress,
        "validation",
        "Comparing database row counts and verifying copied files",
    );
    let database_row_counts = compare_representative_row_counts(&source_conn, &stage_db)?;
    let mut copied_totals = CopyStats::default();
    for stats in [
        copied_music,
        copied_artwork,
        copied_presets,
        copied_font_stats,
        copied_tls,
        copied_referenced_art,
    ] {
        copied_totals.add(stats);
    }

    let staged_paths = AppPaths::from_roots(
        &destination.resource_dir,
        stage,
        &destination.cache_dir,
        &destination.log_dir,
    );
    let source_paths = AppPaths::from_workspace_dir(source_workspace);
    let preserved_installation_id = if let Some(metadata) = source_paths
        .load_install_metadata()
        .map_err(|error| error.to_string())?
    {
        write_install_metadata(&staged_paths.install_path, &metadata)?;
        true
    } else {
        false
    };
    let _install = staged_paths
        .load_or_create_install_metadata()
        .map_err(|error| error.to_string())?;

    let report = LegacyImportReport {
        source_workspace: source_workspace.to_string_lossy().to_string(),
        destination_data_dir: destination.data_dir.to_string_lossy().to_string(),
        copied_music_files: copied_music.files,
        copied_artwork_files: copied_artwork.files + copied_referenced_art.files,
        copied_preset_files: copied_presets.files,
        copied_font: copied_font_stats.files == 1,
        copied_remote_certificate,
        copied_tls_files: copied_tls.files,
        preserved_installation_id,
        rebased_track_paths,
        preserved_external_track_paths,
        skipped_cache_directories: vec![
            "library/qobuz-cache".to_string(),
            "library/sonos-cache".to_string(),
            "library/transcode-cache".to_string(),
            "library/art/thumbnails".to_string(),
            "logs".to_string(),
        ],
        source_preserved: true,
        validation: LegacyImportValidation {
            database_integrity_check_passed: true,
            database_row_counts,
            settings_validated,
            copied_files_verified: copied_totals.files,
            copied_bytes_verified: copied_totals.bytes,
        },
    };
    let manifest = serde_json::to_vec_pretty(&report)
        .map_err(|error| format!("serialize import manifest: {error}"))?;
    atomic_write(&stage.join("import.json"), &manifest)?;
    Ok(report)
}

fn rebase_database_paths(
    db_path: &Path,
    source_workspace: &Path,
    stage_art: &Path,
    final_music_dir: &Path,
) -> Result<(u64, u64, CopyStats), String> {
    let conn = Connection::open(db_path)
        .map_err(|error| format!("open staged database for path migration: {error}"))?;
    let source_music = source_workspace
        .join("music")
        .canonicalize()
        .unwrap_or_else(|_| source_workspace.join("music"));
    let tracks = query_id_paths(&conn, "tracks")?;
    let mut rebased = 0_u64;
    let mut external = 0_u64;
    for (id, stored) in tracks {
        let path = PathBuf::from(&stored);
        let comparable_path = path.canonicalize().unwrap_or_else(|_| path.clone());
        if let Ok(relative) = comparable_path.strip_prefix(&source_music) {
            let destination = final_music_dir.join(relative);
            conn.execute(
                "UPDATE tracks SET path = ?2 WHERE id = ?1",
                params![id, destination.to_string_lossy()],
            )
            .map_err(|error| format!("rebase imported track path: {error}"))?;
            rebased += 1;
        } else {
            external += 1;
        }
    }

    let artworks = query_id_paths(&conn, "artworks")?;
    let mut copied_referenced = CopyStats::default();
    for (id, stored) in artworks {
        let stored_path = PathBuf::from(&stored);
        let source = if stored_path.is_absolute() {
            stored_path
        } else {
            source_workspace
                .join("library")
                .join("art")
                .join(&stored_path)
        };
        let Some(file_name) = source.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        // A bare relative filename already names a single managed object. Any
        // nested or absolute legacy path gets an ID-scoped flat filename so
        // two distinct images named e.g. cover.jpg can never be aliased when
        // the database is rebased into the managed artwork root.
        let already_managed = !Path::new(&stored).is_absolute()
            && Path::new(&stored)
                .parent()
                .is_some_and(|parent| parent.as_os_str().is_empty());
        let managed_name = if already_managed {
            file_name.to_string()
        } else {
            unique_imported_artwork_name(stage_art, id, file_name)
        };
        let destination = stage_art.join(&managed_name);
        if !destination.exists() && source.is_file() {
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent).map_err(|error| {
                    format!("create imported artwork parent {:?}: {error}", parent)
                })?;
            }
            copied_referenced.add(copy_regular_file_verified(&source, &destination)?);
        }
        if destination.is_file() {
            conn.execute(
                "UPDATE artworks SET path = ?2 WHERE id = ?1",
                params![id, managed_name],
            )
            .map_err(|error| format!("rebase imported artwork path: {error}"))?;
        }
    }
    Ok((rebased, external, copied_referenced))
}

fn unique_imported_artwork_name(stage_art: &Path, id: i64, file_name: &str) -> String {
    let mut candidate = format!("imported-{id}-{file_name}");
    let mut suffix = 2_u32;
    while stage_art.join(&candidate).exists() {
        candidate = format!("imported-{id}-{suffix}-{file_name}");
        suffix += 1;
    }
    candidate
}

fn query_id_paths(conn: &Connection, table: &str) -> Result<Vec<(i64, String)>, String> {
    let mut stmt = conn
        .prepare(&format!("SELECT id, path FROM {table}"))
        .map_err(|error| format!("inspect imported {table} paths: {error}"))?;
    let rows = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(|error| format!("read imported {table} paths: {error}"))?;
    let mut values = Vec::new();
    for row in rows {
        values.push(row.map_err(|error| format!("read imported {table} path row: {error}"))?);
    }
    Ok(values)
}

fn rebase_settings_paths(
    settings: &mut PersistedSettings,
    source_music: &Path,
    final_music: &Path,
    source_tls: &Path,
    final_tls: &Path,
) {
    if let Some(directories) = &mut settings.music_dirs {
        for directory in directories {
            let path = PathBuf::from(directory.as_str());
            let comparable_path = path.canonicalize().unwrap_or_else(|_| path.clone());
            let comparable_source = source_music
                .canonicalize()
                .unwrap_or_else(|_| source_music.to_path_buf());
            if let Ok(relative) = comparable_path.strip_prefix(&comparable_source) {
                *directory = final_music.join(relative).to_string_lossy().to_string();
            }
        }
    }
    rebase_optional_managed_path(
        &mut settings.remote_access.custom_cert_path,
        source_tls,
        final_tls,
    );
    rebase_optional_managed_path(
        &mut settings.remote_access.custom_key_path,
        source_tls,
        final_tls,
    );
}

fn rebase_optional_managed_path(value: &mut Option<String>, source: &Path, destination: &Path) {
    let Some(stored) = value.as_mut() else {
        return;
    };
    let path = PathBuf::from(stored.as_str());
    let comparable_path = path.canonicalize().unwrap_or_else(|_| path.clone());
    let comparable_source = source
        .canonicalize()
        .unwrap_or_else(|_| source.to_path_buf());
    if let Ok(relative) = comparable_path.strip_prefix(comparable_source) {
        *stored = destination.join(relative).to_string_lossy().to_string();
    }
}

fn copy_tree_regular_files(
    source: &Path,
    destination: &Path,
    include: impl Fn(&Path) -> bool + Copy,
) -> Result<CopyStats, String> {
    if !source.exists() {
        return Ok(CopyStats::default());
    }
    let mut stats = CopyStats::default();
    copy_tree_inner(source, source, destination, include, &mut stats)?;
    Ok(stats)
}

fn copy_tree_inner(
    root: &Path,
    current: &Path,
    destination: &Path,
    include: impl Fn(&Path) -> bool + Copy,
    stats: &mut CopyStats,
) -> Result<(), String> {
    for entry in fs::read_dir(current)
        .map_err(|error| format!("read import source directory {:?}: {error}", current))?
    {
        let entry = entry.map_err(|error| format!("read import source entry: {error}"))?;
        let source_path = entry.path();
        let relative = source_path
            .strip_prefix(root)
            .map_err(|error| format!("resolve import relative path: {error}"))?;
        if !include(relative) {
            continue;
        }
        let metadata = fs::symlink_metadata(&source_path)
            .map_err(|error| format!("inspect import source {:?}: {error}", source_path))?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        let destination_path = destination.join(relative);
        if metadata.is_dir() {
            fs::create_dir_all(&destination_path).map_err(|error| {
                format!("create import directory {:?}: {error}", destination_path)
            })?;
            copy_tree_inner(root, &source_path, destination, include, stats)?;
        } else if metadata.is_file() {
            if let Some(parent) = destination_path.parent() {
                fs::create_dir_all(parent)
                    .map_err(|error| format!("create import file parent {:?}: {error}", parent))?;
            }
            stats.add(copy_regular_file_verified(&source_path, &destination_path)?);
        }
    }
    Ok(())
}

fn copy_optional_file(source: &Path, destination: &Path) -> Result<CopyStats, String> {
    if !source.is_file() {
        return Ok(CopyStats::default());
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("create import file parent {:?}: {error}", parent))?;
    }
    copy_regular_file_verified(source, destination)
}

fn copy_regular_file_verified(source: &Path, destination: &Path) -> Result<CopyStats, String> {
    let source_size = fs::metadata(source)
        .map_err(|error| format!("inspect import file {:?}: {error}", source))?
        .len();
    let copied = fs::copy(source, destination)
        .map_err(|error| format!("copy import file {:?}: {error}", source))?;
    let destination_size = fs::metadata(destination)
        .map_err(|error| format!("inspect copied import file {:?}: {error}", destination))?
        .len();
    if copied != source_size || destination_size != source_size {
        return Err(format!(
            "copied import file size mismatch for {:?}: source {source_size}, copy reported {copied}, destination {destination_size}",
            source
        ));
    }
    Ok(CopyStats {
        files: 1,
        bytes: source_size,
    })
}

fn validate_database(path: &Path) -> Result<(), String> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| format!("open staged database for validation: {error}"))?;
    let result: String = conn
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .map_err(|error| format!("validate staged database: {error}"))?;
    if result.eq_ignore_ascii_case("ok") {
        Ok(())
    } else {
        Err(format!("staged database integrity check failed: {result}"))
    }
}

fn compare_representative_row_counts(
    source: &Connection,
    staged_path: &Path,
) -> Result<Vec<LegacyImportTableValidation>, String> {
    let staged = Connection::open_with_flags(staged_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| format!("open staged database for row-count validation: {error}"))?;
    let mut validations = Vec::new();
    for table in REPRESENTATIVE_TABLES {
        if !table_exists(source, table)? {
            continue;
        }
        if !table_exists(&staged, table)? {
            return Err(format!(
                "staged database is missing source table {table} during import validation"
            ));
        }
        let source_rows = table_row_count(source, table)?;
        let staged_rows = table_row_count(&staged, table)?;
        if source_rows != staged_rows {
            return Err(format!(
                "staged database row-count mismatch for {table}: source {source_rows}, staged {staged_rows}"
            ));
        }
        validations.push(LegacyImportTableValidation {
            table: (*table).to_string(),
            source_rows,
            staged_rows,
        });
    }
    Ok(validations)
}

fn table_exists(conn: &Connection, table: &str) -> Result<bool, String> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1)",
        [table],
        |row| row.get(0),
    )
    .map_err(|error| format!("inspect import table {table}: {error}"))
}

fn table_row_count(conn: &Connection, table: &str) -> Result<u64, String> {
    let count: i64 = conn
        .query_row(&format!("SELECT COUNT(*) FROM \"{table}\""), [], |row| {
            row.get(0)
        })
        .map_err(|error| format!("count import table {table}: {error}"))?;
    u64::try_from(count).map_err(|_| format!("import table {table} returned a negative row count"))
}

fn refuse_live_database_handles(db_path: &Path) -> Result<(), String> {
    let name = db_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("library.db");
    let wal = db_path.with_file_name(format!("{name}-wal"));
    let shm = db_path.with_file_name(format!("{name}-shm"));
    #[cfg(unix)]
    {
        let mut command = Command::new("lsof");
        command.arg("-F").arg("p").arg("--").arg(db_path);
        if wal.exists() {
            command.arg(&wal);
        }
        if shm.exists() {
            command.arg(&shm);
        }
        match command.output() {
            Ok(output) if output.status.success() && !output.stdout.is_empty() => {
                return Err(format!(
                    "legacy database {:?} is open by another process; quit the old Fozmo server before importing",
                    db_path
                ));
            }
            // lsof exits 1 when no process has a matching file open.
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                eprintln!(
                    "import: lsof is unavailable; relying on the data-root lock and SQLite snapshot isolation"
                );
            }
            Err(error) => return Err(format!("inspect legacy database handles: {error}")),
        }
    }
    Ok(())
}

fn lock_legacy_workspace_if_supported(source: &Path) -> Result<Option<File>, String> {
    let lock_path = source.join(".fozmo.lock");
    if !lock_path.exists() {
        return Ok(None);
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(|error| format!("open legacy data lock {:?}: {error}", lock_path))?;
    file.try_lock().map_err(|_| {
        format!(
            "legacy workspace {:?} is in use; stop its Fozmo server before importing",
            source
        )
    })?;
    Ok(Some(file))
}

fn progress_event<F>(progress: &mut F, stage: &str, message: &str)
where
    F: FnMut(LegacyImportProgress),
{
    progress(LegacyImportProgress {
        stage: stage.to_string(),
        message: message.to_string(),
    });
}

fn same_path(left: &Path, right: &Path) -> bool {
    left.canonicalize().ok().as_deref() == right.canonicalize().ok().as_deref()
}

fn now_unix_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("fozmo-import-{name}-{}", now_unix_nanos()))
    }

    #[test]
    fn importer_copies_state_rebases_managed_paths_and_skips_caches() {
        let root = temp_root("legacy");
        let source = root.join("old-workspace");
        let destination_root = root.join("new-data");
        fs::create_dir_all(source.join("library/art")).unwrap();
        fs::create_dir_all(source.join("library/art/a")).unwrap();
        fs::create_dir_all(source.join("library/art/b")).unwrap();
        fs::create_dir_all(source.join("library/qobuz-cache")).unwrap();
        fs::create_dir_all(source.join("music/Album")).unwrap();
        fs::create_dir_all(source.join("presets")).unwrap();
        fs::create_dir_all(source.join("static/user-fonts")).unwrap();
        fs::create_dir_all(source.join("library/tls")).unwrap();
        fs::write(source.join("music/Album/track.flac"), b"music").unwrap();
        fs::write(source.join("library/art/cover.jpg"), b"art").unwrap();
        fs::write(source.join("library/art/a/cover.jpg"), b"art-a").unwrap();
        fs::write(source.join("library/art/b/cover.jpg"), b"art-b").unwrap();
        fs::write(source.join("library/qobuz-cache/skip"), b"cache").unwrap();
        fs::write(source.join("presets/Room.json"), b"{}").unwrap();
        fs::write(source.join("static/user-fonts/custom-display.ttf"), b"font").unwrap();
        fs::write(
            source.join("library/tls/remote-cert.pem"),
            b"generated-cert",
        )
        .unwrap();
        fs::write(source.join("library/tls/custom-cert.pem"), b"custom-cert").unwrap();
        fs::write(source.join("library/tls/custom-key.pem"), b"custom-key").unwrap();
        let external = root.join("external.flac");
        fs::write(&external, b"external").unwrap();

        let db_path = source.join("library/library.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE tracks(id INTEGER PRIMARY KEY, path TEXT);
             CREATE TABLE artworks(id INTEGER PRIMARY KEY, path TEXT);
             CREATE TABLE albums(id INTEGER PRIMARY KEY);
             CREATE TABLE playback_history(id INTEGER PRIMARY KEY);
             CREATE TABLE playlists(id INTEGER PRIMARY KEY);
             CREATE TABLE zone_queue_items(id INTEGER PRIMARY KEY);
             INSERT INTO albums DEFAULT VALUES;
             INSERT INTO playback_history DEFAULT VALUES;
             INSERT INTO playlists DEFAULT VALUES;
             INSERT INTO zone_queue_items DEFAULT VALUES;",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tracks(path) VALUES (?1)",
            [source
                .join("music/Album/track.flac")
                .to_string_lossy()
                .as_ref()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tracks(path) VALUES (?1)",
            [external.to_string_lossy().as_ref()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO artworks(path) VALUES (?1)",
            [source
                .join("library/art/cover.jpg")
                .to_string_lossy()
                .as_ref()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO artworks(path) VALUES (?1)",
            [source
                .join("library/art/a/cover.jpg")
                .to_string_lossy()
                .as_ref()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO artworks(path) VALUES (?1)",
            [source
                .join("library/art/b/cover.jpg")
                .to_string_lossy()
                .as_ref()],
        )
        .unwrap();
        drop(conn);

        let mut settings = PersistedSettings {
            music_dirs: Some(vec![source.join("music").to_string_lossy().to_string()]),
            ..PersistedSettings::default()
        };
        settings.remote_access.custom_cert_path = Some(
            source
                .join("library/tls/custom-cert.pem")
                .to_string_lossy()
                .to_string(),
        );
        settings.remote_access.custom_key_path = Some(
            source
                .join("library/tls/custom-key.pem")
                .to_string_lossy()
                .to_string(),
        );
        settings.save(&source.join("settings.json")).unwrap();

        let paths = AppPaths::from_roots(
            root.join("resources"),
            &destination_root,
            root.join("cache"),
            root.join("logs"),
        );
        let report = import_legacy_workspace(&source, &paths, |_| {}).unwrap();
        assert_eq!(report.rebased_track_paths, 1);
        assert_eq!(report.preserved_external_track_paths, 1);
        assert!(destination_root.join("music/Album/track.flac").is_file());
        assert!(destination_root.join("library/art/cover.jpg").is_file());
        assert!(destination_root.join("tls/remote-cert.pem").is_file());
        assert!(destination_root.join("tls/custom-cert.pem").is_file());
        assert!(destination_root.join("tls/custom-key.pem").is_file());
        assert!(!destination_root.join("library/qobuz-cache").exists());
        assert!(
            destination_root
                .join("appearance/custom-display.ttf")
                .is_file()
        );
        assert!(destination_root.join("import.json").is_file());
        assert!(source.join("settings.json").is_file());
        assert!(report.validation.database_integrity_check_passed);
        assert!(report.validation.settings_validated);
        assert_eq!(report.copied_tls_files, 3);
        assert!(report.validation.copied_files_verified >= 7);
        assert!(report.validation.copied_bytes_verified > 0);
        for table in [
            "tracks",
            "albums",
            "playback_history",
            "playlists",
            "zone_queue_items",
        ] {
            let validation = report
                .validation
                .database_row_counts
                .iter()
                .find(|validation| validation.table == table)
                .unwrap();
            assert_eq!(validation.source_rows, validation.staged_rows);
        }

        let imported_settings =
            PersistedSettings::try_load(&destination_root.join("settings.json")).unwrap();
        assert_eq!(
            imported_settings.remote_access.custom_cert_path.as_deref(),
            Some(
                destination_root
                    .join("tls/custom-cert.pem")
                    .to_string_lossy()
                    .as_ref()
            )
        );
        assert_eq!(
            imported_settings.remote_access.custom_key_path.as_deref(),
            Some(
                destination_root
                    .join("tls/custom-key.pem")
                    .to_string_lossy()
                    .as_ref()
            )
        );

        let imported = Connection::open(destination_root.join("library/library.db")).unwrap();
        let managed: String = imported
            .query_row("SELECT path FROM tracks WHERE id = 1", [], |row| row.get(0))
            .unwrap();
        let external_after: String = imported
            .query_row("SELECT path FROM tracks WHERE id = 2", [], |row| row.get(0))
            .unwrap();
        let art: String = imported
            .query_row("SELECT path FROM artworks WHERE id = 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        let art_a: String = imported
            .query_row("SELECT path FROM artworks WHERE id = 2", [], |row| {
                row.get(0)
            })
            .unwrap();
        let art_b: String = imported
            .query_row("SELECT path FROM artworks WHERE id = 3", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            managed,
            destination_root
                .join("music/Album/track.flac")
                .to_string_lossy()
        );
        assert_eq!(external_after, external.to_string_lossy());
        assert_eq!(
            fs::read(destination_root.join("library/art").join(&art)).unwrap(),
            b"art"
        );
        assert_ne!(art_a, art_b);
        assert_eq!(
            fs::read(destination_root.join("library/art").join(&art_a)).unwrap(),
            b"art-a"
        );
        assert_eq!(
            fs::read(destination_root.join("library/art").join(&art_b)).unwrap(),
            b"art-b"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn importer_includes_committed_rows_from_a_stopped_leftover_wal() {
        let root = temp_root("wal");
        let builder_dir = root.join("builder");
        let source = root.join("old-workspace");
        fs::create_dir_all(&builder_dir).unwrap();
        fs::create_dir_all(source.join("library/art")).unwrap();
        let builder_db = builder_dir.join("library.db");
        let conn = Connection::open(&builder_db).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; CREATE TABLE tracks(id INTEGER PRIMARY KEY, path TEXT); CREATE TABLE artworks(id INTEGER PRIMARY KEY, path TEXT); PRAGMA wal_checkpoint(TRUNCATE); INSERT INTO tracks(path) VALUES ('/external/from-wal.flac');",
        )
        .unwrap();
        let builder_wal = builder_dir.join("library.db-wal");
        let builder_shm = builder_dir.join("library.db-shm");
        assert!(fs::metadata(&builder_wal).unwrap().len() > 0);
        fs::copy(&builder_db, source.join("library/library.db")).unwrap();
        fs::copy(&builder_wal, source.join("library/library.db-wal")).unwrap();
        fs::copy(&builder_shm, source.join("library/library.db-shm")).unwrap();
        drop(conn);

        let paths = AppPaths::from_roots(
            root.join("resources"),
            root.join("new-data"),
            root.join("cache"),
            root.join("logs"),
        );
        let report = import_legacy_workspace(&source, &paths, |_| {}).unwrap();
        assert!(!report.preserved_installation_id);
        let imported = Connection::open(paths.library_dir.join("library.db")).unwrap();
        let count: i64 = imported
            .query_row("SELECT COUNT(*) FROM tracks", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn importer_preserves_an_existing_installation_uuid() {
        let root = temp_root("install-id");
        let source = root.join("existing-data-root");
        fs::create_dir_all(source.join("library/art")).unwrap();
        fs::create_dir_all(source.join("tls")).unwrap();
        fs::create_dir_all(source.join("appearance")).unwrap();
        fs::write(source.join("tls/custom-cert.pem"), b"packaged-cert").unwrap();
        fs::write(source.join("tls/custom-key.pem"), b"packaged-key").unwrap();
        fs::write(
            source.join("appearance/custom-display.ttf"),
            b"packaged-font",
        )
        .unwrap();
        let conn = Connection::open(source.join("library/library.db")).unwrap();
        conn.execute_batch(
            "CREATE TABLE tracks(id INTEGER PRIMARY KEY, path TEXT);
             CREATE TABLE artworks(id INTEGER PRIMARY KEY, path TEXT);",
        )
        .unwrap();
        drop(conn);

        let source_paths = AppPaths::from_workspace_dir(&source);
        let source_install = source_paths.load_or_create_install_metadata().unwrap();
        let mut settings = PersistedSettings::default();
        settings.remote_access.custom_cert_path = Some(
            source
                .join("tls/custom-cert.pem")
                .to_string_lossy()
                .to_string(),
        );
        settings.remote_access.custom_key_path = Some(
            source
                .join("tls/custom-key.pem")
                .to_string_lossy()
                .to_string(),
        );
        settings.save(&source.join("settings.json")).unwrap();
        let destination = AppPaths::from_roots(
            root.join("resources"),
            root.join("relocated-data-root"),
            root.join("cache"),
            root.join("logs"),
        );

        let report = import_legacy_workspace(&source, &destination, |_| {}).unwrap();
        let imported_install = destination.load_install_metadata().unwrap().unwrap();

        assert!(report.preserved_installation_id);
        assert_eq!(
            imported_install.installation_id,
            source_install.installation_id
        );
        assert!(destination.tls_dir.join("custom-cert.pem").is_file());
        assert!(destination.tls_dir.join("custom-key.pem").is_file());
        assert!(
            destination
                .appearance_assets_dir
                .join("custom-display.ttf")
                .is_file()
        );
        let imported_settings = PersistedSettings::try_load(&destination.settings_path).unwrap();
        assert_eq!(
            imported_settings.remote_access.custom_cert_path.as_deref(),
            Some(
                destination
                    .tls_dir
                    .join("custom-cert.pem")
                    .to_string_lossy()
                    .as_ref()
            )
        );
        assert!(source.join("install.json").is_file());
        let _ = fs::remove_dir_all(root);
    }
}
