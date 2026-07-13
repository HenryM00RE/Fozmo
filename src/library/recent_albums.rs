use super::history_entries::history_profile_id;
use super::{
    Library, PlaybackHistoryEntry, RecentAlbumSummary, normalize_key, normalize_qobuz_album_id,
    now_secs,
};
use crate::protocol::SourceRef;
use rusqlite::{OptionalExtension, params};
use std::collections::HashSet;

type RecentAlbumTrackRow = (
    Option<i64>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<i64>,
);

impl Library {
    pub fn record_recent_album_for_source(
        &self,
        profile_id: Option<&str>,
        source: &SourceRef,
    ) -> Result<(), String> {
        if source.is_radio() {
            return Ok(());
        }
        let profile_id = history_profile_id(profile_id, &self.active_profile_id());
        let played_at = now_secs();
        match source {
            SourceRef::QobuzTrack {
                track_id,
                title,
                artist,
                album,
                album_id,
                image_url,
                ..
            } => {
                if let Some(linked_album) =
                    self.linked_recent_album_for_qobuz_id(album_id.as_deref())?
                {
                    let item_key = format!("local:album:{}", linked_album.id);
                    let local_album_id = linked_album.id.to_string();
                    let source_track_id = track_id.to_string();
                    let title = clean_recent_text(Some(&linked_album.title))
                        .or_else(|| clean_recent_text(album.as_deref()))
                        .unwrap_or_else(|| "Unknown album".to_string());
                    let album_artist = clean_recent_text(linked_album.album_artist.as_deref())
                        .or_else(|| clean_recent_text(artist.as_deref()))
                        .unwrap_or_else(|| "Unknown artist".to_string());
                    self.upsert_recent_album(
                        &profile_id,
                        &item_key,
                        "local",
                        Some(local_album_id.as_str()),
                        &title,
                        &album_artist,
                        linked_album.art_id,
                        image_url.as_deref(),
                        Some(source_track_id.as_str()),
                        played_at,
                    )?;
                    if let Some(qobuz_album_id) = album_id.as_deref() {
                        self.delete_recent_album_key(
                            &profile_id,
                            &format!("qobuz:album:{qobuz_album_id}"),
                        )?;
                    }
                    return Ok(());
                }
                let title = clean_recent_text(album.as_deref())
                    .or_else(|| clean_recent_text(title.as_deref()))
                    .unwrap_or_else(|| "Unknown album".to_string());
                let album_artist = clean_recent_text(artist.as_deref())
                    .unwrap_or_else(|| "Unknown artist".to_string());
                let item_key = album_id
                    .as_ref()
                    .filter(|id| !id.trim().is_empty())
                    .map(|id| format!("qobuz:album:{id}"))
                    .unwrap_or_else(|| format!("qobuz:track:{track_id}"));
                let source_track_id = track_id.to_string();
                self.upsert_recent_album(
                    &profile_id,
                    &item_key,
                    "qobuz",
                    album_id.as_deref(),
                    &title,
                    &album_artist,
                    None,
                    image_url.as_deref(),
                    Some(source_track_id.as_str()),
                    played_at,
                )
            }
            SourceRef::LocalTrack {
                track_id,
                title,
                artist,
                album,
                ..
            } => {
                let conn = self.conn.lock().unwrap();
                let row: Option<RecentAlbumTrackRow> = conn
                    .query_row(
                        r#"
                        SELECT t.album_id, t.album, a.title,
                               COALESCE(NULLIF(TRIM(t.album_artist), ''), NULLIF(TRIM(a.album_artist), ''), NULLIF(TRIM(t.artist), '')),
                               a.album_artist,
                               COALESCE(t.art_id, a.canonical_art_id, a.art_id) AS art_id
                        FROM tracks t
                        LEFT JOIN albums a ON a.id = t.album_id
                        WHERE t.id = ?1
                        "#,
                        [track_id],
                        |row| {
                            Ok((
                                row.get(0)?,
                                row.get(1)?,
                                row.get(2)?,
                                row.get(3)?,
                                row.get(4)?,
                                row.get(5)?,
                            ))
                        },
                    )
                    .optional()
                    .map_err(|e| format!("recent album source query: {e}"))?;
                drop(conn);

                let (album_id, track_album, library_album, track_artist, album_artist, art_id) =
                    row.unwrap_or((None, None, None, None, None, None));
                let title = clean_recent_text(library_album.as_deref())
                    .or_else(|| clean_recent_text(track_album.as_deref()))
                    .or_else(|| clean_recent_text(album.as_deref()))
                    .or_else(|| clean_recent_text(title.as_deref()))
                    .unwrap_or_else(|| "Unknown album".to_string());
                let album_artist = clean_recent_text(album_artist.as_deref())
                    .or_else(|| clean_recent_text(track_artist.as_deref()))
                    .or_else(|| clean_recent_text(artist.as_deref()))
                    .unwrap_or_else(|| "Unknown artist".to_string());
                let album_id_string = album_id.map(|id| id.to_string());
                let item_key = album_id_string
                    .as_ref()
                    .map(|id| format!("local:album:{id}"))
                    .unwrap_or_else(|| {
                        format!(
                            "local:album:{}:{}",
                            normalize_key(&title),
                            normalize_key(&album_artist)
                        )
                    });
                let source_track_id = track_id.to_string();
                self.upsert_recent_album(
                    &profile_id,
                    &item_key,
                    "local",
                    album_id_string.as_deref(),
                    &title,
                    &album_artist,
                    art_id,
                    None,
                    Some(source_track_id.as_str()),
                    played_at,
                )
            }
        }
    }

    // Recent-album writes preserve the provider, artwork, source, and timestamp columns together.
    #[allow(clippy::too_many_arguments)]
    fn upsert_recent_album(
        &self,
        profile_id: &str,
        item_key: &str,
        provider: &str,
        album_id: Option<&str>,
        title: &str,
        album_artist: &str,
        art_id: Option<i64>,
        image_url: Option<&str>,
        source_track_id: Option<&str>,
        played_at: i64,
    ) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            r#"
            INSERT INTO recently_played_albums (
                profile_id, item_key, provider, album_id, title, album_artist,
                art_id, image_url, source_track_id, played_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            ON CONFLICT(profile_id, item_key) DO UPDATE SET
                provider = excluded.provider,
                album_id = excluded.album_id,
                title = excluded.title,
                album_artist = excluded.album_artist,
                art_id = excluded.art_id,
                image_url = excluded.image_url,
                source_track_id = excluded.source_track_id,
                played_at = excluded.played_at
            "#,
            params![
                profile_id,
                item_key,
                provider,
                album_id,
                title,
                album_artist,
                art_id,
                image_url,
                source_track_id,
                played_at
            ],
        )
        .map_err(|e| format!("record recent album: {e}"))?;
        conn.execute(
            r#"
            DELETE FROM recently_played_albums
            WHERE profile_id = ?1
              AND item_key NOT IN (
                  SELECT item_key
                  FROM recently_played_albums
                  WHERE profile_id = ?1
                  ORDER BY played_at DESC, rowid DESC
                  LIMIT 50
              )
            "#,
            [profile_id],
        )
        .map_err(|e| format!("prune recent albums: {e}"))?;
        Ok(())
    }

    fn delete_recent_album_key(&self, profile_id: &str, item_key: &str) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM recently_played_albums WHERE profile_id = ?1 AND item_key = ?2",
            params![profile_id, item_key],
        )
        .map_err(|e| format!("delete recent album alias: {e}"))?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn recent_albums(&self, limit: i64) -> Result<Vec<RecentAlbumSummary>, String> {
        let profile_id = self.active_profile_id();
        self.recent_albums_for_profile(&profile_id, limit)
    }

    pub fn recent_albums_for_profile(
        &self,
        profile_id: &str,
        limit: i64,
    ) -> Result<Vec<RecentAlbumSummary>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                r#"
                SELECT provider, album_id, title, album_artist, art_id, image_url,
                       source_track_id, played_at
                FROM recently_played_albums
                WHERE profile_id = ?1
                ORDER BY played_at DESC, rowid DESC
                LIMIT ?2
                "#,
            )
            .map_err(|e| format!("recent albums query: {e}"))?;
        let rows = stmt
            .query_map(params![profile_id, limit.clamp(1, 200)], |row| {
                let provider: String = row.get(0)?;
                let album_id: Option<String> = row.get(1)?;
                let source_track_id: Option<String> = row.get(6)?;
                let is_qobuz = provider == "qobuz";
                let id = if is_qobuz {
                    album_id
                        .clone()
                        .or_else(|| {
                            source_track_id
                                .as_ref()
                                .map(|id| format!("qobuz:track:{id}"))
                        })
                        .unwrap_or_else(|| "qobuz:album:unknown".to_string())
                } else {
                    album_id
                        .clone()
                        .or_else(|| {
                            source_track_id
                                .as_ref()
                                .map(|id| format!("local:track:{id}"))
                        })
                        .unwrap_or_else(|| "local:album:unknown".to_string())
                };
                Ok(RecentAlbumSummary {
                    recent_type: "album".to_string(),
                    id,
                    title: row.get(2)?,
                    album_artist: row
                        .get::<_, Option<String>>(3)?
                        .unwrap_or_else(|| "Unknown artist".to_string()),
                    art_id: row.get(4)?,
                    image_url: row.get(5)?,
                    year: None,
                    is_qobuz,
                    qobuz_album_id: is_qobuz.then(|| album_id.clone()).flatten(),
                    source_track_id,
                    album_id: (!is_qobuz).then(|| album_id.clone()).flatten(),
                    hires: false,
                    match_status: None,
                    played_at: row.get(7)?,
                })
            })
            .map_err(|e| format!("recent albums map: {e}"))?;
        let mut raw = Vec::new();
        for row in rows {
            let album = row.map_err(|e| format!("recent album row: {e}"))?;
            raw.push(album);
        }
        drop(stmt);
        drop(conn);

        let mut out = Vec::new();
        for album in raw {
            out.push(self.canonicalize_recent_album_summary(album)?);
        }
        out = dedupe_recent_album_summaries(out);

        let limit = limit.clamp(1, 200) as usize;
        if out.len() < limit {
            let mut seen = out
                .iter()
                .map(recent_album_summary_key)
                .collect::<HashSet<_>>();
            for entry in self.recent_playback_history((limit * 4) as i64, false)? {
                if entry.radio {
                    continue;
                }
                if let Some(album) = recent_album_from_history_entry(entry) {
                    let album = self.canonicalize_recent_album_summary(album)?;
                    if seen.insert(recent_album_summary_key(&album)) {
                        out.push(album);
                    }
                }
                if out.len() >= limit {
                    break;
                }
            }
            out.sort_by_key(|album| std::cmp::Reverse(album.played_at));
            out.truncate(limit);
        }
        Ok(out)
    }

    fn canonicalize_recent_album_summary(
        &self,
        album: RecentAlbumSummary,
    ) -> Result<RecentAlbumSummary, String> {
        if !album.is_qobuz {
            return Ok(album);
        }
        let Some(linked) =
            self.linked_recent_album_for_qobuz_id(album.qobuz_album_id.as_deref())?
        else {
            return Ok(album);
        };
        Ok(RecentAlbumSummary {
            id: linked.id.to_string(),
            title: linked.title,
            album_artist: linked
                .album_artist
                .unwrap_or_else(|| album.album_artist.clone()),
            art_id: linked.art_id.or(album.art_id),
            image_url: album.image_url.clone(),
            is_qobuz: false,
            qobuz_album_id: album.qobuz_album_id.clone(),
            album_id: Some(linked.id.to_string()),
            ..album
        })
    }

    fn linked_recent_album_for_qobuz_id(
        &self,
        qobuz_album_id: Option<&str>,
    ) -> Result<Option<LinkedRecentAlbum>, String> {
        let Some(raw_id) = qobuz_album_id.map(str::trim).filter(|id| !id.is_empty()) else {
            return Ok(None);
        };
        let normalized_id = normalize_qobuz_album_id(raw_id);
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            r#"
            SELECT a.id, a.title, a.album_artist, COALESCE(a.canonical_art_id, a.art_id)
            FROM albums a
            LEFT JOIN album_versions v ON v.album_id = a.id AND v.provider = 'qobuz'
            WHERE (
                a.qobuz_match_status = 'matched'
                AND (a.qobuz_album_id = ?1 OR a.qobuz_album_id = ?2)
            )
            OR (v.provider_id = ?1 OR v.provider_id = ?2)
            ORDER BY
                CASE
                    WHEN a.qobuz_match_status = 'matched'
                     AND (a.qobuz_album_id = ?1 OR a.qobuz_album_id = ?2)
                    THEN 0
                    ELSE 1
                END,
                a.id
            LIMIT 1
            "#,
            params![raw_id, normalized_id],
            |row| {
                Ok(LinkedRecentAlbum {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    album_artist: row.get(2)?,
                    art_id: row.get(3)?,
                })
            },
        )
        .optional()
        .map_err(|e| format!("linked recent qobuz album lookup: {e}"))
    }
}

#[derive(Debug)]
struct LinkedRecentAlbum {
    id: i64,
    title: String,
    album_artist: Option<String>,
    art_id: Option<i64>,
}

fn dedupe_recent_album_summaries(albums: Vec<RecentAlbumSummary>) -> Vec<RecentAlbumSummary> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for album in albums {
        if seen.insert(recent_album_summary_key(&album)) {
            out.push(album);
        }
    }
    out
}

fn clean_recent_text(input: Option<&str>) -> Option<String> {
    input
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn recent_album_summary_key(album: &RecentAlbumSummary) -> String {
    if album.is_qobuz {
        return album
            .qobuz_album_id
            .as_ref()
            .map(|id| format!("qobuz:{id}"))
            .unwrap_or_else(|| {
                format!(
                    "qobuz:{}:{}",
                    normalize_key(&album.title),
                    normalize_key(&album.album_artist)
                )
            });
    }
    album
        .album_id
        .as_ref()
        .map(|id| format!("local:{id}"))
        .unwrap_or_else(|| {
            format!(
                "local:{}:{}",
                normalize_key(&album.title),
                normalize_key(&album.album_artist)
            )
        })
}

fn recent_album_from_history_entry(entry: PlaybackHistoryEntry) -> Option<RecentAlbumSummary> {
    let title = clean_recent_text(entry.album.as_deref())
        .or_else(|| clean_recent_text(entry.title.as_deref()))?;
    let album_artist =
        clean_recent_text(entry.artist.as_deref()).unwrap_or_else(|| "Unknown artist".to_string());
    match entry.source {
        SourceRef::QobuzTrack {
            track_id, album_id, ..
        } => {
            let id = album_id
                .clone()
                .unwrap_or_else(|| format!("qobuz:track:{track_id}"));
            Some(RecentAlbumSummary {
                recent_type: "album".to_string(),
                id,
                title,
                album_artist,
                art_id: entry.art_id,
                image_url: entry.image_url,
                year: None,
                is_qobuz: true,
                qobuz_album_id: album_id,
                source_track_id: Some(track_id.to_string()),
                album_id: None,
                hires: false,
                match_status: None,
                played_at: entry.played_at,
            })
        }
        SourceRef::LocalTrack { track_id, .. } => {
            let album_id = entry.album_id.map(|id| id.to_string());
            let id = album_id
                .clone()
                .unwrap_or_else(|| format!("local:track:{track_id}"));
            Some(RecentAlbumSummary {
                recent_type: "album".to_string(),
                id,
                title,
                album_artist,
                art_id: entry.art_id,
                image_url: entry.image_url,
                year: None,
                is_qobuz: false,
                qobuz_album_id: None,
                source_track_id: Some(track_id.to_string()),
                album_id,
                hires: false,
                match_status: None,
                played_at: entry.played_at,
            })
        }
    }
}
