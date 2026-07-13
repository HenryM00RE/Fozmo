use super::migrations;
use crate::app::paths::atomic_write;
use crate::settings::PersistedSettings;
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

const BACKUP_FORMAT_VERSION: u32 = 1;
const BACKUPS_TO_KEEP: usize = 3;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BackupManifest {
    format_version: u32,
    reason: String,
    created_at_unix_millis: u128,
    app_version: String,
    database_schema_version: u32,
    database_file: String,
    settings_file: Option<String>,
}

pub(crate) fn database_needs_migration(db_path: &Path) -> Result<bool, String> {
    if !db_path.exists()
        || fs::metadata(db_path)
            .map_err(|error| format!("inspect library database {:?}: {error}", db_path))?
            .len()
            == 0
    {
        return Ok(false);
    }
    let conn = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| format!("open library database for version check: {error}"))?;
    let version = migrations::schema_version(&conn)?;
    if version > migrations::CURRENT_SCHEMA_VERSION {
        return Err(format!(
            "library database schema {version} is newer than this Fozmo build supports ({})",
            migrations::CURRENT_SCHEMA_VERSION
        ));
    }
    Ok(version < migrations::CURRENT_SCHEMA_VERSION)
}

/// Bring an installed database to the schema supported by this binary without
/// ever running a numbered migration against the live file.
///
/// The pre-migration backup is both the user's retained recovery point and the
/// source for a sibling staged database. The live database is replaced only
/// after the stage has migrated transactionally, passed an integrity check,
/// and been durably flushed.
pub(crate) fn prepare_database_for_open(
    db_path: &Path,
    settings_path: &Path,
    backup_root: &Path,
) -> Result<(), String> {
    if !database_needs_migration(db_path)? {
        return Ok(());
    }

    let backup_dir = create_backup_from_path(db_path, settings_path, backup_root, "pre-migration")?;
    install_migrated_snapshot(db_path, &backup_dir.join("library.db"))
}

fn install_migrated_snapshot(db_path: &Path, snapshot_path: &Path) -> Result<(), String> {
    validate_database(snapshot_path)?;
    let stage_path = migration_stage_path(db_path)?;
    let result = (|| -> Result<(), String> {
        copy_snapshot_to_stage(snapshot_path, &stage_path)?;

        {
            let conn = Connection::open(&stage_path)
                .map_err(|error| format!("open staged library database: {error}"))?;
            conn.execute_batch(
                r#"
                PRAGMA foreign_keys = ON;
                PRAGMA journal_mode = DELETE;
                PRAGMA synchronous = FULL;
                "#,
            )
            .map_err(|error| format!("configure staged library database: {error}"))?;
            migrations::migrate(&conn)?;
            validate_connection(&conn, "staged migration")?;
            let version = migrations::schema_version(&conn)?;
            if version != migrations::CURRENT_SCHEMA_VERSION {
                return Err(format!(
                    "staged library database schema is {version}, expected {}",
                    migrations::CURRENT_SCHEMA_VERSION
                ));
            }
        }

        // Reopen read-only after the migration connection has closed so the
        // file that will actually be installed is checked independently.
        validate_database(&stage_path)?;
        File::open(&stage_path)
            .and_then(|file| file.sync_all())
            .map_err(|error| format!("sync staged library database {:?}: {error}", stage_path))?;

        prepare_stopped_source_for_replacement(db_path)?;
        fs::rename(&stage_path, db_path).map_err(|error| {
            format!(
                "atomically install migrated library database {:?}: {error}",
                db_path
            )
        })?;
        sync_parent_directory(db_path);
        Ok(())
    })();

    if result.is_err() {
        cleanup_sqlite_files(&stage_path);
    }
    result
}

fn migration_stage_path(db_path: &Path) -> Result<PathBuf, String> {
    let parent = db_path
        .parent()
        .ok_or_else(|| format!("library database {:?} has no parent directory", db_path))?;
    let file_name = db_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("library.db");
    Ok(parent.join(format!(
        ".{file_name}.migration-{}-{}",
        std::process::id(),
        now_unix_nanos()
    )))
}

fn copy_snapshot_to_stage(snapshot_path: &Path, stage_path: &Path) -> Result<(), String> {
    let mut source = File::open(snapshot_path)
        .map_err(|error| format!("open pre-migration snapshot {:?}: {error}", snapshot_path))?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut stage = options
        .open(stage_path)
        .map_err(|error| format!("create staged library database {:?}: {error}", stage_path))?;
    io::copy(&mut source, &mut stage)
        .map_err(|error| format!("copy pre-migration snapshot into stage: {error}"))?;
    stage
        .flush()
        .and_then(|()| stage.sync_all())
        .map_err(|error| format!("sync staged library database {:?}: {error}", stage_path))
}

fn prepare_stopped_source_for_replacement(db_path: &Path) -> Result<(), String> {
    // A stopped source may still have a durable WAL left behind. Checkpoint it
    // before removing sidecars so a failed final rename still leaves a
    // complete, readable source database with the same data and user_version.
    let conn = Connection::open(db_path)
        .map_err(|error| format!("open stopped library database before replacement: {error}"))?;
    let result: (i64, i64, i64) = conn
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .map_err(|error| format!("checkpoint stopped library database: {error}"))?;
    if result.0 != 0 {
        return Err(format!(
            "library database is still in use; checkpoint remained busy ({} frames, {} checkpointed)",
            result.1, result.2
        ));
    }
    drop(conn);

    remove_sqlite_sidecar(db_path, "-wal")?;
    remove_sqlite_sidecar(db_path, "-shm")?;
    remove_sqlite_sidecar(db_path, "-journal")?;
    Ok(())
}

fn remove_sqlite_sidecar(db_path: &Path, suffix: &str) -> Result<(), String> {
    let sidecar = sqlite_sidecar_path(db_path, suffix);
    match fs::remove_file(&sidecar) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("remove SQLite sidecar {:?}: {error}", sidecar)),
    }
}

fn cleanup_sqlite_files(path: &Path) {
    let _ = fs::remove_file(path);
    for suffix in ["-wal", "-shm", "-journal"] {
        let _ = fs::remove_file(sqlite_sidecar_path(path, suffix));
    }
}

fn sqlite_sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

fn sync_parent_directory(path: &Path) {
    if let Some(parent) = path.parent()
        && let Ok(directory) = File::open(parent)
    {
        let _ = directory.sync_all();
    }
}

pub(crate) fn create_backup_from_path(
    db_path: &Path,
    settings_path: &Path,
    backup_root: &Path,
    reason: &str,
) -> Result<PathBuf, String> {
    let conn = Connection::open(db_path)
        .map_err(|error| format!("open library database for backup: {error}"))?;
    create_backup_from_connection(&conn, settings_path, backup_root, reason)
}

pub(crate) fn create_backup_from_connection(
    conn: &Connection,
    settings_path: &Path,
    backup_root: &Path,
    reason: &str,
) -> Result<PathBuf, String> {
    fs::create_dir_all(backup_root)
        .map_err(|error| format!("create backup root {:?}: {error}", backup_root))?;
    let now = now_unix_millis();
    let unique_timestamp = now_unix_nanos();
    let reason = sanitize_reason(reason);
    let backup_dir = backup_root.join(format!(
        "{unique_timestamp:020}-{reason}-{}",
        std::process::id()
    ));
    fs::create_dir(&backup_dir)
        .map_err(|error| format!("create backup directory {:?}: {error}", backup_dir))?;

    let result = (|| -> Result<(), String> {
        let database_path = backup_dir.join("library.db");
        conn.execute("VACUUM INTO ?1", [database_path.to_string_lossy().as_ref()])
            .map_err(|error| format!("create consistent SQLite backup: {error}"))?;
        validate_database(&database_path)?;

        let settings_file = if settings_path.exists() {
            let body = fs::read_to_string(settings_path)
                .map_err(|error| format!("read settings for backup: {error}"))?;
            serde_json::from_str::<PersistedSettings>(&body)
                .map_err(|error| format!("refuse backup with invalid settings: {error}"))?;
            atomic_write(&backup_dir.join("settings.json"), body.as_bytes())?;
            Some("settings.json".to_string())
        } else {
            None
        };

        let manifest = BackupManifest {
            format_version: BACKUP_FORMAT_VERSION,
            reason,
            created_at_unix_millis: now,
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            database_schema_version: database_schema_version(&database_path)?,
            database_file: "library.db".to_string(),
            settings_file,
        };
        let manifest_json = serde_json::to_vec_pretty(&manifest)
            .map_err(|error| format!("serialize backup manifest: {error}"))?;
        atomic_write(&backup_dir.join("manifest.json"), &manifest_json)?;
        validate_backup_dir(&backup_dir)?;
        Ok(())
    })();

    if let Err(error) = result {
        let _ = fs::remove_dir_all(&backup_dir);
        return Err(error);
    }
    prune_old_backups(backup_root, &backup_dir)?;
    Ok(backup_dir)
}

fn database_schema_version(path: &Path) -> Result<u32, String> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| format!("open backup database: {error}"))?;
    migrations::schema_version(&conn)
}

fn validate_database(path: &Path) -> Result<(), String> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| format!("open backup database for validation: {error}"))?;
    validate_connection(&conn, "backup")
}

fn validate_connection(conn: &Connection, description: &str) -> Result<(), String> {
    let result: String = conn
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .map_err(|error| format!("validate {description} database: {error}"))?;
    if result.eq_ignore_ascii_case("ok") {
        Ok(())
    } else {
        Err(format!(
            "{description} database integrity check failed: {result}"
        ))
    }
}

fn validate_backup_dir(path: &Path) -> Result<(), String> {
    let manifest_path = path.join("manifest.json");
    let manifest_body = fs::read_to_string(&manifest_path)
        .map_err(|error| format!("read backup manifest: {error}"))?;
    let manifest: BackupManifest = serde_json::from_str(&manifest_body)
        .map_err(|error| format!("parse backup manifest: {error}"))?;
    if manifest.format_version != BACKUP_FORMAT_VERSION {
        return Err("backup manifest has an unsupported version".to_string());
    }
    validate_database(&path.join(&manifest.database_file))?;
    if let Some(settings_file) = manifest.settings_file {
        let body = fs::read_to_string(path.join(settings_file))
            .map_err(|error| format!("read backed-up settings: {error}"))?;
        serde_json::from_str::<PersistedSettings>(&body)
            .map_err(|error| format!("validate backed-up settings: {error}"))?;
    }
    Ok(())
}

fn prune_old_backups(backup_root: &Path, new_backup: &Path) -> Result<(), String> {
    // Deletion is permitted only after the new backup has passed every
    // validation above.
    validate_backup_dir(new_backup)?;
    let mut backups = fs::read_dir(backup_root)
        .map_err(|error| format!("list backups: {error}"))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir() && path.join("manifest.json").is_file())
        .collect::<Vec<_>>();
    backups.sort();
    let remove_count = backups.len().saturating_sub(BACKUPS_TO_KEEP);
    for old in backups.into_iter().take(remove_count) {
        fs::remove_dir_all(&old)
            .map_err(|error| format!("remove old backup {:?}: {error}", old))?;
    }
    Ok(())
}

fn sanitize_reason(reason: &str) -> String {
    let value = reason
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if value.is_empty() {
        "manual".to_string()
    } else {
        value.chars().take(48).collect()
    }
}

fn now_unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
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
    use crate::settings::PersistedSettings;

    fn temp_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("fozmo-backup-{name}-{}", now_unix_millis()))
    }

    #[test]
    fn backup_contains_consistent_database_and_settings() {
        let root = temp_root("consistent");
        fs::create_dir_all(&root).unwrap();
        let db_path = root.join("library.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; CREATE TABLE sample(value TEXT); INSERT INTO sample VALUES ('kept');",
        )
        .unwrap();
        let settings_path = root.join("settings.json");
        PersistedSettings::default().save(&settings_path).unwrap();

        let backup =
            create_backup_from_connection(&conn, &settings_path, &root.join("backups"), "test")
                .unwrap();
        let copied = Connection::open(backup.join("library.db")).unwrap();
        let value: String = copied
            .query_row("SELECT value FROM sample", [], |row| row.get(0))
            .unwrap();
        assert_eq!(value, "kept");
        assert!(backup.join("settings.json").is_file());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn only_three_valid_backups_are_retained() {
        let root = temp_root("retention");
        fs::create_dir_all(&root).unwrap();
        let conn = Connection::open(root.join("library.db")).unwrap();
        conn.execute_batch("CREATE TABLE sample(value TEXT);")
            .unwrap();
        let settings_path = root.join("settings.json");
        PersistedSettings::default().save(&settings_path).unwrap();
        for index in 0..4 {
            create_backup_from_connection(
                &conn,
                &settings_path,
                &root.join("backups"),
                &format!("test-{index}"),
            )
            .unwrap();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let count = fs::read_dir(root.join("backups"))
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.path().is_dir())
            .count();
        assert_eq!(count, 3);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn failed_staged_migration_leaves_source_data_and_version_untouched() {
        let root = temp_root("staged-migration-failure");
        fs::create_dir_all(&root).unwrap();
        let db_path = root.join("library.db");
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                r#"
                CREATE TABLE sentinel(value TEXT NOT NULL);
                INSERT INTO sentinel VALUES ('source-kept');
                CREATE VIEW tracks AS SELECT value AS id FROM sentinel;
                PRAGMA user_version = 0;
                "#,
            )
            .unwrap();
        }
        let settings_path = root.join("settings.json");
        PersistedSettings::default().save(&settings_path).unwrap();

        let error =
            prepare_database_for_open(&db_path, &settings_path, &root.join("backups")).unwrap_err();
        assert!(error.contains("migrate library db"), "{error}");

        let source =
            Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        assert_eq!(migrations::schema_version(&source).unwrap(), 0);
        let value: String = source
            .query_row("SELECT value FROM sentinel", [], |row| row.get(0))
            .unwrap();
        assert_eq!(value, "source-kept");
        let albums_created: bool = source
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'albums')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!albums_created);
        drop(source);

        assert!(
            fs::read_dir(&root)
                .unwrap()
                .filter_map(Result::ok)
                .all(|entry| !entry.file_name().to_string_lossy().contains(".migration-")),
            "failed migration left a sibling stage behind"
        );
        assert_eq!(
            fs::read_dir(root.join("backups"))
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry.path().is_dir())
                .count(),
            1,
            "the verified pre-migration recovery point must be retained"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn successful_staged_migration_installs_current_schema() {
        let root = temp_root("staged-migration-success");
        fs::create_dir_all(&root).unwrap();
        let db_path = root.join("library.db");
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                r#"
                CREATE TABLE sentinel(value TEXT NOT NULL);
                INSERT INTO sentinel VALUES ('survives-migration');
                PRAGMA user_version = 0;
                "#,
            )
            .unwrap();
        }
        let settings_path = root.join("settings.json");
        PersistedSettings::default().save(&settings_path).unwrap();

        prepare_database_for_open(&db_path, &settings_path, &root.join("backups")).unwrap();

        let migrated =
            Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        assert_eq!(
            migrations::schema_version(&migrated).unwrap(),
            migrations::CURRENT_SCHEMA_VERSION
        );
        let value: String = migrated
            .query_row("SELECT value FROM sentinel", [], |row| row.get(0))
            .unwrap();
        assert_eq!(value, "survives-migration");
        let tracks_created: bool = migrated
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'tracks')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(tracks_created);
        drop(migrated);

        assert!(
            fs::read_dir(&root)
                .unwrap()
                .filter_map(Result::ok)
                .all(|entry| !entry.file_name().to_string_lossy().contains(".migration-")),
            "successful migration left a sibling stage behind"
        );
        let _ = fs::remove_dir_all(root);
    }
}
