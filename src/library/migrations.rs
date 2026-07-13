use rusqlite::Connection;

pub(crate) const CURRENT_SCHEMA_VERSION: u32 = 1;

pub(super) fn migrate(conn: &Connection) -> Result<(), String> {
    let found = schema_version(conn)?;
    if found > CURRENT_SCHEMA_VERSION {
        return Err(format!(
            "library database schema {found} is newer than this Fozmo build supports ({CURRENT_SCHEMA_VERSION}); refusing to modify it"
        ));
    }
    if found == CURRENT_SCHEMA_VERSION {
        return Ok(());
    }

    conn.execute_batch("BEGIN IMMEDIATE")
        .map_err(|error| format!("begin library migration transaction: {error}"))?;
    let result = (|| {
        let mut version = found;
        if version < 1 {
            migrate_to_v1(conn)?;
            conn.pragma_update(None, "user_version", 1_u32)
                .map_err(|error| format!("record library schema version 1: {error}"))?;
            version = 1;
        }
        debug_assert_eq!(version, CURRENT_SCHEMA_VERSION);
        conn.execute_batch("COMMIT")
            .map_err(|error| format!("commit library migration: {error}"))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = conn.execute_batch("ROLLBACK");
    }
    result
}

pub(crate) fn schema_version(conn: &Connection) -> Result<u32, String> {
    conn.pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|error| format!("read library schema version: {error}"))
}

fn migrate_to_v1(conn: &Connection) -> Result<(), String> {
    // Version 1 adopts every idempotent migration that predated user_version.
    // Existing production databases at user_version=0 may already contain all
    // of these objects; rerunning them transactionally is intentional.
    apply_initial_schema(conn)?;
    apply_search_schema(conn)?;
    apply_additive_column_migrations(conn)?;
    apply_recording_schema(conn)?;
    apply_autometa_job_schema(conn)?;
    apply_playback_history_indexes(conn)?;
    apply_album_browse_indexes(conn)?;
    Ok(())
}

fn apply_initial_schema(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(INITIAL_SCHEMA_SQL)
        .map_err(|e| format!("migrate library db: {e}"))
}

fn apply_search_schema(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(SEARCH_FTS_SQL)
        .map_err(|e| format!("create search fts tables: {e}"))?;
    rebuild_search_indexes(conn)
}

fn apply_recording_schema(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(RECORDING_SCHEMA_SQL)
        .map_err(|e| format!("create recording tables: {e}"))
}

fn apply_additive_column_migrations(conn: &Connection) -> Result<(), String> {
    for migration in ADDITIVE_COLUMN_MIGRATIONS {
        add_column_if_missing(
            conn,
            migration.table,
            migration.column,
            migration.definition,
        )?;
    }
    Ok(())
}

fn apply_playback_history_indexes(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(PLAYBACK_HISTORY_INDEXES_SQL)
        .map_err(|e| format!("create playback history indexes: {e}"))
}

fn apply_album_browse_indexes(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(ALBUM_BROWSE_INDEXES_SQL)
        .map_err(|e| format!("create album browse indexes: {e}"))
}

fn apply_autometa_job_schema(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(AUTOMETA_JOB_SCHEMA_SQL)
        .map_err(|e| format!("create autometa job tables: {e}"))
}

struct ColumnMigration {
    table: &'static str,
    column: &'static str,
    definition: &'static str,
}

const ADDITIVE_COLUMN_MIGRATIONS: &[ColumnMigration] = &[
    ColumnMigration {
        table: "tracks",
        column: "mb_recording_id",
        definition: "mb_recording_id TEXT",
    },
    ColumnMigration {
        table: "tracks",
        column: "bit_depth",
        definition: "bit_depth INTEGER",
    },
    ColumnMigration {
        table: "tracks",
        column: "status",
        definition: "status TEXT NOT NULL DEFAULT 'available'",
    },
    ColumnMigration {
        table: "tracks",
        column: "missing_since",
        definition: "missing_since INTEGER",
    },
    ColumnMigration {
        table: "version_tracks",
        column: "recording_id",
        definition: "recording_id INTEGER",
    },
    ColumnMigration {
        table: "playback_history",
        column: "recording_id",
        definition: "recording_id INTEGER",
    },
    ColumnMigration {
        table: "albums",
        column: "art_locked",
        definition: "art_locked INTEGER NOT NULL DEFAULT 0",
    },
    ColumnMigration {
        table: "albums",
        column: "primary_version_id",
        definition: "primary_version_id INTEGER",
    },
    ColumnMigration {
        table: "artworks",
        column: "width",
        definition: "width INTEGER",
    },
    ColumnMigration {
        table: "artworks",
        column: "height",
        definition: "height INTEGER",
    },
    ColumnMigration {
        table: "albums",
        column: "qobuz_album_id",
        definition: "qobuz_album_id TEXT",
    },
    ColumnMigration {
        table: "albums",
        column: "qobuz_match_status",
        definition: "qobuz_match_status TEXT",
    },
    ColumnMigration {
        table: "albums",
        column: "qobuz_match_confidence",
        definition: "qobuz_match_confidence INTEGER",
    },
    ColumnMigration {
        table: "albums",
        column: "qobuz_payload_json",
        definition: "qobuz_payload_json TEXT",
    },
    ColumnMigration {
        table: "albums",
        column: "canonical_art_id",
        definition: "canonical_art_id INTEGER",
    },
    ColumnMigration {
        table: "albums",
        column: "original_year",
        definition: "original_year INTEGER",
    },
    ColumnMigration {
        table: "albums",
        column: "mb_barcode",
        definition: "mb_barcode TEXT",
    },
    ColumnMigration {
        table: "playback_history",
        column: "duration_secs",
        definition: "duration_secs REAL",
    },
    ColumnMigration {
        table: "playback_history",
        column: "counted",
        definition: "counted INTEGER NOT NULL DEFAULT 0",
    },
    ColumnMigration {
        table: "playback_history",
        column: "radio",
        definition: "radio INTEGER NOT NULL DEFAULT 0",
    },
    ColumnMigration {
        table: "playback_history",
        column: "profile_id",
        definition: "profile_id TEXT NOT NULL DEFAULT 'default'",
    },
    ColumnMigration {
        table: "album_versions",
        column: "musicbrainz_match_status",
        definition: "musicbrainz_match_status TEXT",
    },
    ColumnMigration {
        table: "album_versions",
        column: "musicbrainz_release_id",
        definition: "musicbrainz_release_id TEXT",
    },
    ColumnMigration {
        table: "album_versions",
        column: "musicbrainz_tagged_at",
        definition: "musicbrainz_tagged_at INTEGER",
    },
    ColumnMigration {
        table: "album_versions",
        column: "qobuz_match_status",
        definition: "qobuz_match_status TEXT",
    },
    ColumnMigration {
        table: "album_versions",
        column: "qobuz_tagged_at",
        definition: "qobuz_tagged_at INTEGER",
    },
    ColumnMigration {
        table: "album_versions",
        column: "autometa_message",
        definition: "autometa_message TEXT",
    },
];

const INITIAL_SCHEMA_SQL: &str = r#"
        PRAGMA foreign_keys = ON;
        CREATE TABLE IF NOT EXISTS artworks (
            id INTEGER PRIMARY KEY,
            hash TEXT NOT NULL UNIQUE,
            mime TEXT NOT NULL,
            path TEXT NOT NULL,
            source TEXT NOT NULL,
            width INTEGER,
            height INTEGER,
            created_at INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS albums (
            id INTEGER PRIMARY KEY,
            title TEXT NOT NULL,
            album_artist TEXT,
            sort_key TEXT NOT NULL UNIQUE,
            year INTEGER,
            original_year INTEGER,
            confidence INTEGER NOT NULL DEFAULT 0,
            match_status TEXT NOT NULL DEFAULT 'needs_review',
            mb_release_id TEXT,
            mb_release_group_id TEXT,
            mb_barcode TEXT,
            primary_version_id INTEGER,
            qobuz_album_id TEXT,
            qobuz_match_status TEXT,
            qobuz_match_confidence INTEGER,
            qobuz_payload_json TEXT,
            canonical_art_id INTEGER REFERENCES artworks(id),
            art_locked INTEGER NOT NULL DEFAULT 0,
            art_id INTEGER REFERENCES artworks(id),
            track_count INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS tracks (
            id INTEGER PRIMARY KEY,
            path TEXT NOT NULL UNIQUE,
            file_name TEXT NOT NULL,
            size_bytes INTEGER NOT NULL,
            modified_secs INTEGER NOT NULL,
            title TEXT NOT NULL,
            artist TEXT,
            album TEXT,
            album_artist TEXT,
            track_number INTEGER,
            disc_number INTEGER,
            year INTEGER,
            genre TEXT,
            composer TEXT,
            duration_secs REAL,
            sample_rate INTEGER,
            bit_depth INTEGER,
            channels INTEGER,
            format TEXT,
            album_id INTEGER REFERENCES albums(id) ON DELETE SET NULL,
            art_id INTEGER REFERENCES artworks(id),
            embedded_art INTEGER NOT NULL DEFAULT 0,
            status TEXT NOT NULL DEFAULT 'available',
            missing_since INTEGER,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS artists (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL UNIQUE,
            sort_name TEXT NOT NULL,
            created_at INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS recordings (
            id INTEGER PRIMARY KEY,
            album_id INTEGER NOT NULL REFERENCES albums(id) ON DELETE CASCADE,
            recording_key TEXT NOT NULL,
            title TEXT NOT NULL,
            artist TEXT,
            disc_number INTEGER,
            track_number INTEGER,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            UNIQUE(album_id, recording_key)
        );
        CREATE TABLE IF NOT EXISTS match_candidates (
            id INTEGER PRIMARY KEY,
            album_id INTEGER NOT NULL REFERENCES albums(id) ON DELETE CASCADE,
            provider TEXT NOT NULL,
            provider_id TEXT NOT NULL,
            title TEXT NOT NULL,
            artist TEXT,
            year INTEGER,
            score INTEGER NOT NULL,
            payload_json TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT 'pending',
            created_at INTEGER NOT NULL,
            UNIQUE(album_id, provider, provider_id)
        );
        CREATE TABLE IF NOT EXISTS album_versions (
            id INTEGER PRIMARY KEY,
            album_id INTEGER NOT NULL REFERENCES albums(id) ON DELETE CASCADE,
            provider TEXT NOT NULL,
            provider_id TEXT NOT NULL,
            title TEXT NOT NULL,
            artist TEXT,
            year INTEGER,
            track_count INTEGER NOT NULL DEFAULT 0,
            art_id INTEGER REFERENCES artworks(id),
            format TEXT,
            sample_rate INTEGER,
            bit_depth INTEGER,
            source_label TEXT,
            status TEXT NOT NULL DEFAULT 'available',
            payload_json TEXT,
            musicbrainz_match_status TEXT,
            musicbrainz_release_id TEXT,
            musicbrainz_tagged_at INTEGER,
            qobuz_match_status TEXT,
            qobuz_tagged_at INTEGER,
            autometa_message TEXT,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            UNIQUE(album_id, provider, provider_id)
        );
        CREATE TABLE IF NOT EXISTS version_tracks (
            id INTEGER PRIMARY KEY,
            version_id INTEGER NOT NULL REFERENCES album_versions(id) ON DELETE CASCADE,
            provider_track_id TEXT,
            local_track_id INTEGER REFERENCES tracks(id) ON DELETE CASCADE,
            recording_id INTEGER REFERENCES recordings(id) ON DELETE SET NULL,
            title TEXT NOT NULL,
            artist TEXT,
            track_number INTEGER,
            disc_number INTEGER,
            duration_secs REAL,
            sample_rate INTEGER,
            format TEXT,
            bit_depth INTEGER,
            status TEXT NOT NULL DEFAULT 'available',
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            UNIQUE(version_id, provider_track_id),
            UNIQUE(version_id, local_track_id)
        );
        CREATE TABLE IF NOT EXISTS version_track_links (
            id INTEGER PRIMARY KEY,
            album_id INTEGER NOT NULL REFERENCES albums(id) ON DELETE CASCADE,
            local_version_track_id INTEGER NOT NULL REFERENCES version_tracks(id) ON DELETE CASCADE,
            qobuz_version_track_id INTEGER NOT NULL REFERENCES version_tracks(id) ON DELETE CASCADE,
            confidence INTEGER NOT NULL,
            match_kind TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT 'linked',
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            UNIQUE(album_id, local_version_track_id, qobuz_version_track_id)
        );
        CREATE TABLE IF NOT EXISTS autometa_jobs (
            id INTEGER PRIMARY KEY,
            status TEXT NOT NULL,
            mode TEXT NOT NULL,
            link_qobuz INTEGER NOT NULL DEFAULT 0,
            total INTEGER NOT NULL DEFAULT 0,
            current_album_id INTEGER REFERENCES albums(id) ON DELETE SET NULL,
            current_version_id INTEGER REFERENCES album_versions(id) ON DELETE SET NULL,
            last_result TEXT,
            error TEXT,
            started_at INTEGER,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            finished_at INTEGER
        );
        CREATE TABLE IF NOT EXISTS autometa_job_items (
            id INTEGER PRIMARY KEY,
            job_id INTEGER NOT NULL REFERENCES autometa_jobs(id) ON DELETE CASCADE,
            album_id INTEGER NOT NULL REFERENCES albums(id) ON DELETE CASCADE,
            version_id INTEGER NOT NULL REFERENCES album_versions(id) ON DELETE CASCADE,
            phase TEXT NOT NULL DEFAULT 'queued',
            status TEXT NOT NULL DEFAULT 'pending',
            attempts INTEGER NOT NULL DEFAULT 0,
            musicbrainz_release_id TEXT,
            qobuz_album_id TEXT,
            message TEXT,
            started_at INTEGER,
            finished_at INTEGER,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            UNIQUE(job_id, version_id)
        );
        CREATE TABLE IF NOT EXISTS playback_zones (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            kind TEXT NOT NULL,
            device_name TEXT,
            enabled INTEGER NOT NULL DEFAULT 1,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS zone_settings (
            zone_id TEXT PRIMARY KEY REFERENCES playback_zones(id) ON DELETE CASCADE,
            settings_json TEXT NOT NULL,
            updated_at INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS zone_queue_items (
            zone_id TEXT NOT NULL REFERENCES playback_zones(id) ON DELETE CASCADE,
            position INTEGER NOT NULL,
            source_json TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            PRIMARY KEY (zone_id, position)
        );
        CREATE TABLE IF NOT EXISTS now_playing_queues (
            zone_id TEXT PRIMARY KEY REFERENCES playback_zones(id) ON DELETE CASCADE,
            state_json TEXT NOT NULL,
            updated_at INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS playback_history (
            id INTEGER PRIMARY KEY,
            profile_id TEXT NOT NULL DEFAULT 'default',
            source_key TEXT NOT NULL,
            recording_id INTEGER REFERENCES recordings(id) ON DELETE SET NULL,
            source_json TEXT NOT NULL,
            zone_id TEXT NOT NULL,
            zone_name TEXT NOT NULL,
            title TEXT,
            artist TEXT,
            album TEXT,
            image_url TEXT,
            played_secs REAL,
            duration_secs REAL,
            completed INTEGER NOT NULL DEFAULT 0,
            counted INTEGER NOT NULL DEFAULT 0,
            radio INTEGER NOT NULL DEFAULT 0,
            played_at INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS favorite_albums (
            provider TEXT NOT NULL,
            provider_id TEXT NOT NULL,
            title TEXT NOT NULL,
            album_artist TEXT,
            art_id INTEGER REFERENCES artworks(id),
            image_url TEXT,
            year INTEGER,
            hires INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            PRIMARY KEY (provider, provider_id)
        );
        CREATE TABLE IF NOT EXISTS recently_played_albums (
            profile_id TEXT NOT NULL DEFAULT 'default',
            item_key TEXT NOT NULL,
            provider TEXT NOT NULL,
            album_id TEXT,
            title TEXT NOT NULL,
            album_artist TEXT,
            art_id INTEGER REFERENCES artworks(id),
            image_url TEXT,
            source_track_id TEXT,
            played_at INTEGER NOT NULL,
            PRIMARY KEY (profile_id, item_key)
        );
        CREATE TABLE IF NOT EXISTS playlists (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            recently_played_at INTEGER
        );
        CREATE TABLE IF NOT EXISTS playlist_items (
            playlist_id TEXT NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
            position INTEGER NOT NULL,
            item_json TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            PRIMARY KEY (playlist_id, position)
        );
        CREATE INDEX IF NOT EXISTS idx_tracks_album ON tracks(album_id);
        CREATE INDEX IF NOT EXISTS idx_tracks_artist ON tracks(artist);
        CREATE INDEX IF NOT EXISTS idx_albums_artist ON albums(album_artist);
        CREATE INDEX IF NOT EXISTS idx_album_versions_album ON album_versions(album_id);
        CREATE INDEX IF NOT EXISTS idx_recordings_album ON recordings(album_id);
        CREATE INDEX IF NOT EXISTS idx_version_tracks_version ON version_tracks(version_id);
        CREATE INDEX IF NOT EXISTS idx_track_links_album ON version_track_links(album_id);
        CREATE INDEX IF NOT EXISTS idx_zone_queue_zone ON zone_queue_items(zone_id, position);
        CREATE INDEX IF NOT EXISTS idx_playback_history_played_at ON playback_history(played_at DESC);
        CREATE INDEX IF NOT EXISTS idx_playback_history_source ON playback_history(source_key, played_at DESC);
        CREATE INDEX IF NOT EXISTS idx_playback_history_profile_listened ON playback_history(profile_id, played_at DESC, id DESC) WHERE played_secs IS NOT NULL AND played_secs > 0;
        CREATE INDEX IF NOT EXISTS idx_playback_history_profile_radio_played ON playback_history(profile_id, radio, played_at DESC, id DESC);
        CREATE INDEX IF NOT EXISTS idx_favorite_albums_created_at ON favorite_albums(created_at DESC);
        CREATE INDEX IF NOT EXISTS idx_recently_played_albums_played_at ON recently_played_albums(profile_id, played_at DESC);
        CREATE INDEX IF NOT EXISTS idx_playlists_updated_at ON playlists(updated_at DESC);
        CREATE INDEX IF NOT EXISTS idx_playlists_recently_played_at ON playlists(recently_played_at DESC);
"#;

const RECORDING_SCHEMA_SQL: &str = r#"
        CREATE TABLE IF NOT EXISTS recordings (
            id INTEGER PRIMARY KEY,
            album_id INTEGER NOT NULL REFERENCES albums(id) ON DELETE CASCADE,
            recording_key TEXT NOT NULL,
            title TEXT NOT NULL,
            artist TEXT,
            disc_number INTEGER,
            track_number INTEGER,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            UNIQUE(album_id, recording_key)
        );
        CREATE INDEX IF NOT EXISTS idx_recordings_album ON recordings(album_id);
        CREATE INDEX IF NOT EXISTS idx_version_tracks_recording ON version_tracks(recording_id);
        CREATE INDEX IF NOT EXISTS idx_playback_history_recording ON playback_history(recording_id, played_at DESC);
"#;

const AUTOMETA_JOB_SCHEMA_SQL: &str = r#"
        CREATE TABLE IF NOT EXISTS autometa_jobs (
            id INTEGER PRIMARY KEY,
            status TEXT NOT NULL,
            mode TEXT NOT NULL,
            link_qobuz INTEGER NOT NULL DEFAULT 0,
            total INTEGER NOT NULL DEFAULT 0,
            current_album_id INTEGER REFERENCES albums(id) ON DELETE SET NULL,
            current_version_id INTEGER REFERENCES album_versions(id) ON DELETE SET NULL,
            last_result TEXT,
            error TEXT,
            started_at INTEGER,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            finished_at INTEGER
        );
        CREATE TABLE IF NOT EXISTS autometa_job_items (
            id INTEGER PRIMARY KEY,
            job_id INTEGER NOT NULL REFERENCES autometa_jobs(id) ON DELETE CASCADE,
            album_id INTEGER NOT NULL REFERENCES albums(id) ON DELETE CASCADE,
            version_id INTEGER NOT NULL REFERENCES album_versions(id) ON DELETE CASCADE,
            phase TEXT NOT NULL DEFAULT 'queued',
            status TEXT NOT NULL DEFAULT 'pending',
            attempts INTEGER NOT NULL DEFAULT 0,
            musicbrainz_release_id TEXT,
            qobuz_album_id TEXT,
            message TEXT,
            started_at INTEGER,
            finished_at INTEGER,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            UNIQUE(job_id, version_id)
        );
        CREATE INDEX IF NOT EXISTS idx_autometa_jobs_status
            ON autometa_jobs(status, updated_at);
        CREATE INDEX IF NOT EXISTS idx_autometa_job_items_job_status
            ON autometa_job_items(job_id, status, id);
"#;

const SEARCH_FTS_SQL: &str = r#"
        CREATE VIRTUAL TABLE IF NOT EXISTS tracks_fts USING fts5(
            track_id UNINDEXED,
            title,
            artist,
            album,
            album_artist,
            composer,
            genre,
            file_name
        );
        CREATE VIRTUAL TABLE IF NOT EXISTS albums_fts USING fts5(
            album_id UNINDEXED,
            title,
            album_artist
        );
        CREATE VIRTUAL TABLE IF NOT EXISTS artists_fts USING fts5(
            artist_id UNINDEXED,
            name
        );

        CREATE TRIGGER IF NOT EXISTS tracks_fts_ai AFTER INSERT ON tracks BEGIN
            DELETE FROM tracks_fts WHERE track_id = new.id;
            INSERT INTO tracks_fts (track_id, title, artist, album, album_artist, composer, genre, file_name)
            VALUES (new.id, new.title, new.artist, new.album, new.album_artist, new.composer, new.genre, new.file_name);
        END;
        CREATE TRIGGER IF NOT EXISTS tracks_fts_au AFTER UPDATE OF title, artist, album, album_artist, composer, genre, file_name ON tracks BEGIN
            DELETE FROM tracks_fts WHERE track_id = old.id;
            INSERT INTO tracks_fts (track_id, title, artist, album, album_artist, composer, genre, file_name)
            VALUES (new.id, new.title, new.artist, new.album, new.album_artist, new.composer, new.genre, new.file_name);
        END;
        CREATE TRIGGER IF NOT EXISTS tracks_fts_ad AFTER DELETE ON tracks BEGIN
            DELETE FROM tracks_fts WHERE track_id = old.id;
        END;

        CREATE TRIGGER IF NOT EXISTS albums_fts_ai AFTER INSERT ON albums BEGIN
            DELETE FROM albums_fts WHERE album_id = new.id;
            INSERT INTO albums_fts (album_id, title, album_artist)
            VALUES (new.id, new.title, new.album_artist);
        END;
        CREATE TRIGGER IF NOT EXISTS albums_fts_au AFTER UPDATE OF title, album_artist ON albums BEGIN
            DELETE FROM albums_fts WHERE album_id = old.id;
            INSERT INTO albums_fts (album_id, title, album_artist)
            VALUES (new.id, new.title, new.album_artist);
        END;
        CREATE TRIGGER IF NOT EXISTS albums_fts_ad AFTER DELETE ON albums BEGIN
            DELETE FROM albums_fts WHERE album_id = old.id;
        END;

        CREATE TRIGGER IF NOT EXISTS artists_fts_ai AFTER INSERT ON artists BEGIN
            DELETE FROM artists_fts WHERE artist_id = new.id;
            INSERT INTO artists_fts (artist_id, name)
            VALUES (new.id, new.name);
        END;
        CREATE TRIGGER IF NOT EXISTS artists_fts_au AFTER UPDATE OF name ON artists BEGIN
            DELETE FROM artists_fts WHERE artist_id = old.id;
            INSERT INTO artists_fts (artist_id, name)
            VALUES (new.id, new.name);
        END;
        CREATE TRIGGER IF NOT EXISTS artists_fts_ad AFTER DELETE ON artists BEGIN
            DELETE FROM artists_fts WHERE artist_id = old.id;
        END;
"#;

const PLAYBACK_HISTORY_INDEXES_SQL: &str = r#"
        CREATE INDEX IF NOT EXISTS idx_playback_history_counted ON playback_history(counted, played_at DESC);
        CREATE INDEX IF NOT EXISTS idx_playback_history_radio_played_at ON playback_history(radio, played_at DESC);
        CREATE INDEX IF NOT EXISTS idx_playback_history_profile_played_at ON playback_history(profile_id, played_at DESC);
        CREATE INDEX IF NOT EXISTS idx_playback_history_profile_source ON playback_history(profile_id, source_key, played_at DESC);
        CREATE INDEX IF NOT EXISTS idx_playback_history_profile_listened ON playback_history(profile_id, played_at DESC, id DESC) WHERE played_secs IS NOT NULL AND played_secs > 0;
        CREATE INDEX IF NOT EXISTS idx_playback_history_profile_radio_played ON playback_history(profile_id, radio, played_at DESC, id DESC);
"#;

const ALBUM_BROWSE_INDEXES_SQL: &str = r#"
        CREATE INDEX IF NOT EXISTS idx_tracks_album_genre ON tracks(album_id, genre);
        CREATE INDEX IF NOT EXISTS idx_albums_title_lower ON albums(lower(title));
        CREATE INDEX IF NOT EXISTS idx_albums_artist_title_lower ON albums(lower(album_artist), lower(title));
        CREATE INDEX IF NOT EXISTS idx_albums_original_year ON albums(original_year);
        CREATE INDEX IF NOT EXISTS idx_albums_year ON albums(year);
        CREATE INDEX IF NOT EXISTS idx_albums_qobuz_source ON albums(qobuz_match_status, qobuz_album_id);
        CREATE INDEX IF NOT EXISTS idx_playback_history_profile_source_listened ON playback_history(profile_id, source_key) WHERE played_secs IS NOT NULL;
"#;

fn add_column_if_missing(
    conn: &Connection,
    table: &str,
    column: &str,
    definition: &str,
) -> Result<(), String> {
    if has_column(conn, table, column)? {
        return Ok(());
    }
    conn.execute(&format!("ALTER TABLE {table} ADD COLUMN {definition}"), [])
        .map_err(|e| format!("add column {table}.{column}: {e}"))?;
    Ok(())
}

fn rebuild_search_indexes(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        r#"
        DELETE FROM tracks_fts;
        INSERT INTO tracks_fts (track_id, title, artist, album, album_artist, composer, genre, file_name)
        SELECT id, title, artist, album, album_artist, composer, genre, file_name
        FROM tracks;

        DELETE FROM albums_fts;
        INSERT INTO albums_fts (album_id, title, album_artist)
        SELECT id, title, album_artist
        FROM albums;

        DELETE FROM artists_fts;
        INSERT INTO artists_fts (artist_id, name)
        SELECT id, name
        FROM artists;
        "#,
    )
    .map_err(|e| format!("rebuild search indexes: {e}"))
}

fn has_column(conn: &Connection, table: &str, column: &str) -> Result<bool, String> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(|e| format!("inspect columns for {table}: {e}"))?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|e| format!("read columns for {table}: {e}"))?;
    for row in rows {
        if row
            .map_err(|e| format!("column row for {table}: {e}"))?
            .eq_ignore_ascii_case(column)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn property_arbitrary_newer_schema_versions_are_rejected_without_mutation(
            delta in 1_u32..10_000
        ) {
            let version = CURRENT_SCHEMA_VERSION.saturating_add(delta);
            let conn = Connection::open_in_memory().unwrap();
            conn.pragma_update(None, "user_version", version).unwrap();
            prop_assert!(migrate(&conn).is_err());
            prop_assert_eq!(schema_version(&conn).unwrap(), version);
        }
    }

    #[test]
    fn migration_records_version_and_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        assert_eq!(schema_version(&conn).unwrap(), CURRENT_SCHEMA_VERSION);
        migrate(&conn).unwrap();
        assert_eq!(schema_version(&conn).unwrap(), CURRENT_SCHEMA_VERSION);
    }

    #[test]
    fn newer_database_is_rejected_without_changes() {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "user_version", CURRENT_SCHEMA_VERSION + 1)
            .unwrap();
        let error = migrate(&conn).unwrap_err();
        assert!(error.contains("newer"));
        assert_eq!(schema_version(&conn).unwrap(), CURRENT_SCHEMA_VERSION + 1);
    }
}
