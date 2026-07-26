use super::{Library, now_secs};
#[cfg(all(target_os = "macos", feature = "apple_music_musickit"))]
use rusqlite::{Connection, OptionalExtension};
use rusqlite::{Transaction, params};
use std::collections::HashMap;

/// The decoder format Fozmo verified while a catalog track actually played.
/// Apple's catalog only advertises a coarse quality variant, so playback is the
/// only place an exact Apple Music rate and depth can come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppleMusicVerifiedFormat {
    pub codec: String,
    pub sample_rate: i64,
    pub bit_depth: Option<i64>,
}

/// The best format verified so far for a catalog album, mirroring how a local
/// or Qobuz version is stamped with its highest-quality track.
pub(super) fn apple_music_album_verified_format(
    conn: &rusqlite::Connection,
    album_id: &str,
) -> Result<Option<AppleMusicVerifiedFormat>, String> {
    use rusqlite::OptionalExtension as _;

    let album_id = album_id.trim();
    if album_id.is_empty() {
        return Ok(None);
    }
    conn.query_row(
        "SELECT codec, sample_rate, bit_depth
         FROM apple_music_track_formats
         WHERE album_id = ?1
         ORDER BY sample_rate DESC, COALESCE(bit_depth, 0) DESC
         LIMIT 1",
        [album_id],
        |row| {
            Ok(AppleMusicVerifiedFormat {
                codec: row.get(0)?,
                sample_rate: row.get(1)?,
                bit_depth: row.get(2)?,
            })
        },
    )
    .optional()
    .map_err(|error| format!("Apple Music verified album format: {error}"))
}

pub(super) struct ExternalAlbumVersionInput<'a> {
    pub album_id: i64,
    pub provider: &'a str,
    pub provider_id: &'a str,
    pub title: &'a str,
    pub artist: Option<&'a str>,
    pub year: Option<i32>,
    pub track_count: i64,
    pub art_id: Option<i64>,
    pub format: &'a str,
    pub sample_rate: Option<i64>,
    pub bit_depth: Option<i64>,
    pub source_label: &'a str,
    pub payload_json: &'a str,
}

#[derive(Debug, Clone)]
pub(super) struct ExternalVersionTrackInput {
    pub provider_track_id: String,
    pub title: String,
    pub artist: Option<String>,
    pub track_number: Option<i64>,
    pub disc_number: Option<i64>,
    pub duration_secs: Option<f64>,
    pub sample_rate: Option<i64>,
    pub format: Option<String>,
    pub bit_depth: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ExternalTrackPairing {
    pub local_track_id: i64,
    pub provider_track_id: String,
    pub confidence: i64,
    pub match_kind: String,
}

#[cfg(all(target_os = "macos", feature = "apple_music_musickit"))]
#[derive(Debug, Clone, PartialEq)]
pub(super) struct ExternalVersionTrackRow {
    pub id: i64,
    pub provider_track_id: String,
    pub recording_id: Option<i64>,
    pub title: String,
    pub artist: Option<String>,
    pub track_number: Option<i64>,
    pub disc_number: Option<i64>,
    pub duration_secs: Option<f64>,
}

#[cfg(all(target_os = "macos", feature = "apple_music_musickit"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ExternalAlbumVersionRow {
    pub id: i64,
    pub album_id: i64,
    pub provider: String,
    pub provider_id: String,
}

pub(super) fn upsert_external_album_version(
    tx: &Transaction<'_>,
    input: &ExternalAlbumVersionInput<'_>,
) -> Result<i64, String> {
    let now = now_secs();
    tx.execute(
        r#"
        INSERT INTO album_versions (
            album_id, provider, provider_id, title, artist, year, track_count,
            art_id, format, sample_rate, bit_depth, source_label, status,
            payload_json, created_at, updated_at
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 'available', ?13, ?14, ?14)
        ON CONFLICT(album_id, provider, provider_id) DO UPDATE SET
            title = excluded.title,
            artist = excluded.artist,
            year = excluded.year,
            track_count = excluded.track_count,
            art_id = COALESCE(excluded.art_id, album_versions.art_id),
            format = excluded.format,
            sample_rate = excluded.sample_rate,
            bit_depth = excluded.bit_depth,
            source_label = excluded.source_label,
            payload_json = excluded.payload_json,
            status = 'available',
            updated_at = excluded.updated_at
        "#,
        params![
            input.album_id,
            input.provider,
            input.provider_id,
            input.title,
            input.artist,
            input.year,
            input.track_count,
            input.art_id,
            input.format,
            input.sample_rate,
            input.bit_depth,
            input.source_label,
            input.payload_json,
            now,
        ],
    )
    .map_err(|error| format!("upsert {} album version: {error}", input.provider))?;
    tx.query_row(
        r#"
        SELECT id
        FROM album_versions
        WHERE album_id = ?1 AND provider = ?2 AND provider_id = ?3
        "#,
        params![input.album_id, input.provider, input.provider_id],
        |row| row.get(0),
    )
    .map_err(|error| format!("select {} album version: {error}", input.provider))
}

pub(super) fn upsert_external_version_tracks(
    tx: &Transaction<'_>,
    version_id: i64,
    tracks: &[ExternalVersionTrackInput],
) -> Result<(), String> {
    let now = now_secs();
    tx.execute(
        "UPDATE version_tracks SET status = 'unavailable', updated_at = ?2 WHERE version_id = ?1",
        params![version_id, now],
    )
    .map_err(|error| format!("mark stale external version tracks: {error}"))?;
    for track in tracks {
        tx.execute(
            r#"
            INSERT INTO version_tracks (
                version_id, provider_track_id, local_track_id, title, artist,
                track_number, disc_number, duration_secs, sample_rate, format,
                bit_depth, status, created_at, updated_at
            )
            VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'available', ?11, ?11)
            ON CONFLICT(version_id, provider_track_id) DO UPDATE SET
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
                track.provider_track_id,
                track.title,
                track.artist,
                track.track_number,
                track.disc_number,
                track.duration_secs,
                track.sample_rate,
                track.format,
                track.bit_depth,
                now,
            ],
        )
        .map_err(|error| format!("upsert external version track: {error}"))?;
    }
    Ok(())
}

pub(super) fn rebuild_external_track_links(
    tx: &Transaction<'_>,
    album_id: i64,
    provider: &str,
    pairings: &[ExternalTrackPairing],
) -> Result<(), String> {
    let now = now_secs();
    let local_version_tracks: HashMap<i64, i64> = {
        let mut statement = tx
            .prepare(
                r#"
                SELECT vt.local_track_id, vt.id
                FROM version_tracks vt
                JOIN album_versions v ON v.id = vt.version_id
                WHERE v.album_id = ?1 AND v.provider = 'local'
                  AND vt.local_track_id IS NOT NULL
                ORDER BY v.id, vt.id
                "#,
            )
            .map_err(|error| format!("load local version tracks: {error}"))?;
        let rows = statement
            .query_map([album_id], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(|error| format!("map local version tracks: {error}"))?;
        let mut tracks = HashMap::new();
        for row in rows {
            let (track_id, version_track_id) =
                row.map_err(|error| format!("read local version track: {error}"))?;
            tracks.entry(track_id).or_insert(version_track_id);
        }
        tracks
    };
    let provider_version_tracks: HashMap<String, i64> = {
        let mut statement = tx
            .prepare(
                r#"
                SELECT vt.provider_track_id, vt.id
                FROM version_tracks vt
                JOIN album_versions v ON v.id = vt.version_id
                WHERE v.album_id = ?1 AND v.provider = ?2
                  AND vt.provider_track_id IS NOT NULL
                  AND vt.status = 'available'
                "#,
            )
            .map_err(|error| format!("load {provider} version tracks: {error}"))?;
        let rows = statement
            .query_map(params![album_id, provider], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(|error| format!("map {provider} version tracks: {error}"))?;
        let mut tracks = HashMap::new();
        for row in rows {
            let (provider_track_id, version_track_id) =
                row.map_err(|error| format!("read {provider} version track: {error}"))?;
            tracks.insert(provider_track_id, version_track_id);
        }
        tracks
    };
    tx.execute(
        r#"
        UPDATE version_track_links
        SET status = 'unlinked', updated_at = ?3
        WHERE album_id = ?1
          AND provider_version_track_id IN (
              SELECT vt.id
              FROM version_tracks vt
              JOIN album_versions v ON v.id = vt.version_id
              WHERE v.album_id = ?1 AND v.provider = ?2
          )
        "#,
        params![album_id, provider, now],
    )
    .map_err(|error| format!("mark stale {provider} track links: {error}"))?;
    for pairing in pairings.iter().filter(|pairing| pairing.confidence >= 80) {
        let Some(local_version_track_id) =
            local_version_tracks.get(&pairing.local_track_id).copied()
        else {
            continue;
        };
        let Some(provider_version_track_id) = provider_version_tracks
            .get(&pairing.provider_track_id)
            .copied()
        else {
            continue;
        };
        tx.execute(
            r#"
            INSERT INTO version_track_links (
                album_id, local_version_track_id, provider_version_track_id,
                confidence, match_kind, status, created_at, updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, 'linked', ?6, ?6)
            ON CONFLICT(album_id, local_version_track_id, provider_version_track_id)
            DO UPDATE SET
                confidence = excluded.confidence,
                match_kind = excluded.match_kind,
                status = 'linked',
                updated_at = excluded.updated_at
            "#,
            params![
                album_id,
                local_version_track_id,
                provider_version_track_id,
                pairing.confidence,
                pairing.match_kind,
                now,
            ],
        )
        .map_err(|error| format!("insert {provider} track link: {error}"))?;
    }
    Ok(())
}

pub(super) fn assign_recording_ids_for_external_version(
    tx: &Transaction<'_>,
    album_id: i64,
) -> Result<(), String> {
    Library::sync_recording_identity_for_album_with_conn(tx, album_id)
}

#[cfg(all(target_os = "macos", feature = "apple_music_musickit"))]
pub(super) fn external_version_tracks(
    conn: &Connection,
    album_id: i64,
    provider: &str,
) -> Result<Vec<ExternalVersionTrackRow>, String> {
    let mut statement = conn
        .prepare(
            r#"
            SELECT vt.id, vt.provider_track_id, vt.recording_id, vt.title, vt.artist,
                   vt.track_number, vt.disc_number, vt.duration_secs
            FROM version_tracks vt
            JOIN album_versions v ON v.id = vt.version_id
            WHERE v.album_id = ?1 AND v.provider = ?2
              AND vt.provider_track_id IS NOT NULL
              AND vt.status = 'available'
            ORDER BY COALESCE(vt.disc_number, 1), COALESCE(vt.track_number, vt.id), vt.id
            "#,
        )
        .map_err(|error| format!("prepare external version track lookup: {error}"))?;
    let rows = statement
        .query_map(params![album_id, provider], |row| {
            Ok(ExternalVersionTrackRow {
                id: row.get(0)?,
                provider_track_id: row.get(1)?,
                recording_id: row.get(2)?,
                title: row.get(3)?,
                artist: row.get(4)?,
                track_number: row.get(5)?,
                disc_number: row.get(6)?,
                duration_secs: row.get(7)?,
            })
        })
        .map_err(|error| format!("query external version tracks: {error}"))?;
    let mut output = Vec::new();
    for row in rows {
        output.push(row.map_err(|error| format!("read external version track: {error}"))?);
    }
    Ok(output)
}

#[cfg(all(target_os = "macos", feature = "apple_music_musickit"))]
pub(super) fn external_album_version_by_provider_id(
    conn: &Connection,
    album_id: i64,
    provider: &str,
    provider_id: &str,
) -> Result<Option<ExternalAlbumVersionRow>, String> {
    conn.query_row(
        r#"
        SELECT id, album_id, provider, provider_id
        FROM album_versions
        WHERE album_id = ?1 AND provider = ?2 AND provider_id = ?3
        "#,
        params![album_id, provider, provider_id],
        |row| {
            Ok(ExternalAlbumVersionRow {
                id: row.get(0)?,
                album_id: row.get(1)?,
                provider: row.get(2)?,
                provider_id: row.get(3)?,
            })
        },
    )
    .optional()
    .map_err(|error| format!("lookup external album version: {error}"))
}
