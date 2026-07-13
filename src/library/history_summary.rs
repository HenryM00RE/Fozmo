use super::history_entries::playback_history_from_row;
use super::{
    Library, ListeningHistoryStats, ListeningRankItem, ListeningTimeBucket, ListeningTopSongItem,
    ListeningTopSongs, PlaybackHistoryEntry, PlaybackHistoryInput, collect_rows, now_secs,
};
use crate::protocol::SourceRef;
use rusqlite::params;
use std::collections::HashMap;

impl Library {
    #[allow(dead_code)]
    pub fn listening_history_stats_with_live(
        &self,
        range: &str,
        live: &[PlaybackHistoryInput],
    ) -> Result<ListeningHistoryStats, String> {
        let profile_id = self.active_profile_id();
        self.listening_history_stats_with_live_for_profile(&profile_id, range, live)
    }

    pub fn listening_history_stats_with_live_for_profile(
        &self,
        profile_id: &str,
        range: &str,
        live: &[PlaybackHistoryInput],
    ) -> Result<ListeningHistoryStats, String> {
        let now = now_secs();
        let (range_key, start) = match range {
            "week" => ("week".to_string(), Some(now - 7 * 86_400)),
            "month" => ("month".to_string(), Some(now - 30 * 86_400)),
            "year" => ("year".to_string(), Some(now - 365 * 86_400)),
            "all" => ("all".to_string(), None),
            _ => ("4w".to_string(), Some(now - 28 * 86_400)),
        };
        let mut rows = self.finalized_history_rows(profile_id, start)?;
        let live_entries = self.live_history_entries_for_profile(profile_id, live, now, false)?;
        for entry in live_entries.iter() {
            if entry.profile_id != profile_id {
                continue;
            }
            let played_secs = entry.played_secs.unwrap_or(0.0);
            if played_secs <= 0.0 || start.is_some_and(|start| entry.played_at < start) {
                continue;
            }
            let linked_qobuz_album_id = entry
                .album_id
                .and_then(|album_id| self.qobuz_album_id_for_local_album(album_id).ok().flatten());
            rows.push(FinalizedHistoryRow {
                source_key: entry.source.key(),
                qobuz_album_id: match &entry.source {
                    SourceRef::QobuzTrack { album_id, .. } => {
                        album_id.clone().filter(|v| !v.trim().is_empty())
                    }
                    _ => None,
                },
                linked_qobuz_album_id,
                title: entry.title.clone(),
                artist: entry.artist.clone(),
                album: entry.album.clone(),
                album_id: entry.album_id,
                genre: None,
                art_id: entry.art_id,
                image_url: entry.image_url.clone(),
                played_secs,
                counted: entry.counted,
                played_at: entry.played_at,
            });
        }
        let total_listened_secs = rows.iter().map(|row| row.played_secs).sum::<f64>().max(0.0);
        let weekly_buckets = weekly_history_buckets(&rows, start.unwrap_or(now - 28 * 86_400), now);
        let weekday_buckets = weekday_history_buckets(&rows);
        let top_artists = top_history_items(&rows, HistoryGroup::Artist);
        let top_albums = top_history_items(&rows, HistoryGroup::Album);
        let top_songs = top_history_items(&rows, HistoryGroup::Song);
        let top_genres = top_history_items(&rows, HistoryGroup::Genre);
        let mut recent_tracks = self.recent_listened_tracks(profile_id, start)?;
        recent_tracks.extend(live_entries.into_iter().filter(|entry| {
            entry.profile_id == profile_id && entry.played_secs.unwrap_or(0.0) > 0.0
        }));
        recent_tracks.sort_by(|a, b| b.played_at.cmp(&a.played_at).then_with(|| b.id.cmp(&a.id)));
        Ok(ListeningHistoryStats {
            range: range_key,
            total_listened_secs,
            weekly_buckets,
            weekday_buckets,
            top_artists,
            top_albums,
            top_songs,
            top_genres,
            recent_tracks,
        })
    }

    pub fn top_history_songs_for_profile(
        &self,
        profile_id: &str,
        range: &str,
        limit: i64,
        exclude_radio: bool,
    ) -> Result<ListeningTopSongs, String> {
        let now = now_secs();
        let (range_key, start) = history_range_start(range, now)?;
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(TOP_HISTORY_SONGS_SQL)
            .map_err(|e| format!("top history songs query: {e}"))?;
        let rows = stmt
            .query_map(
                params![
                    profile_id,
                    start,
                    start,
                    if exclude_radio { 1 } else { 0 },
                    limit.clamp(1, 100)
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<i64>>(4)?.unwrap_or(0),
                        row.get::<_, Option<f64>>(5)?.unwrap_or(0.0),
                        row.get::<_, i64>(6)?,
                    ))
                },
            )
            .map_err(|e| format!("top history songs map: {e}"))?;
        let mut items = Vec::new();
        for row in rows {
            let (title, artist, album, source_key, play_count, listened_secs, last_played_at) =
                row.map_err(|e| format!("top history songs row: {e}"))?;
            items.push(ListeningTopSongItem {
                rank: items.len() + 1,
                title,
                artist,
                album,
                source_key,
                play_count,
                listened_secs,
                last_played_at,
            });
        }
        Ok(ListeningTopSongs {
            range: range_key,
            items,
        })
    }

    fn finalized_history_rows(
        &self,
        profile_id: &str,
        start: Option<i64>,
    ) -> Result<Vec<FinalizedHistoryRow>, String> {
        let conn = self.conn.lock().unwrap();
        let sql = if start.is_some() {
            FINALIZED_HISTORY_SELECT_WITH_START
        } else {
            FINALIZED_HISTORY_SELECT_ALL
        };
        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| format!("history stats query: {e}"))?;
        let mut rows = if let Some(start) = start {
            stmt.query(params![profile_id, start])
                .map_err(|e| format!("history stats rows: {e}"))?
        } else {
            stmt.query([profile_id])
                .map_err(|e| format!("history stats rows: {e}"))?
        };
        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .map_err(|e| format!("history stats row next: {e}"))?
        {
            let source_key: String = row.get(0).map_err(|e| format!("source key: {e}"))?;
            let source_json: String = row.get(1).map_err(|e| format!("source json: {e}"))?;
            out.push(FinalizedHistoryRow {
                source_key: source_key.clone(),
                qobuz_album_id: qobuz_album_id_from_history_source(&source_key, &source_json)?,
                linked_qobuz_album_id: row
                    .get(2)
                    .map_err(|e| format!("history linked qobuz album id: {e}"))?,
                title: row.get(3).map_err(|e| format!("history title: {e}"))?,
                artist: row.get(4).map_err(|e| format!("history artist: {e}"))?,
                album: row.get(5).map_err(|e| format!("history album: {e}"))?,
                album_id: row.get(6).map_err(|e| format!("history album id: {e}"))?,
                genre: row.get(7).map_err(|e| format!("history genre: {e}"))?,
                art_id: row.get(8).map_err(|e| format!("history art: {e}"))?,
                image_url: row.get(9).map_err(|e| format!("history image: {e}"))?,
                played_secs: row
                    .get::<_, Option<f64>>(10)
                    .map_err(|e| format!("history played secs: {e}"))?
                    .unwrap_or(0.0),
                counted: row
                    .get::<_, i64>(11)
                    .map_err(|e| format!("history counted: {e}"))?
                    != 0,
                played_at: row.get(12).map_err(|e| format!("history played at: {e}"))?,
            });
        }
        Ok(out)
    }

    fn recent_listened_tracks(
        &self,
        profile_id: &str,
        start: Option<i64>,
    ) -> Result<Vec<PlaybackHistoryEntry>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                r#"
                SELECT h.id, h.profile_id, h.source_json, h.zone_id, h.zone_name,
                       COALESCE(NULLIF(TRIM(t.title), ''), h.title) AS title,
                       COALESCE(NULLIF(TRIM(t.album_artist), ''), NULLIF(TRIM(a.album_artist), ''), NULLIF(TRIM(t.artist), ''), h.artist) AS artist,
                       COALESCE(NULLIF(TRIM(t.album), ''), NULLIF(TRIM(a.title), ''), h.album) AS album,
                       t.album_id, COALESCE(t.art_id, a.canonical_art_id, a.art_id) AS art_id,
                       h.image_url, h.played_secs, h.duration_secs, h.completed, h.counted, h.radio,
                       h.played_at
                FROM playback_history h
                LEFT JOIN tracks t ON h.source_key GLOB 'local:[0-9]*'
                                  AND t.id = CAST(substr(h.source_key, 7) AS INTEGER)
                LEFT JOIN albums a ON a.id = t.album_id
                WHERE h.profile_id = ?1
                  AND h.played_secs IS NOT NULL
                  AND h.played_secs > 0
                  AND (?2 IS NULL OR h.played_at >= ?2)
                ORDER BY h.played_at DESC, h.id DESC
                "#,
            )
            .map_err(|e| format!("recent listened query: {e}"))?;
        let rows = stmt
            .query_map(params![profile_id, start], playback_history_from_row)
            .map_err(|e| format!("recent listened map: {e}"))?;
        collect_rows(rows)
    }
}

fn history_range_start(range: &str, now: i64) -> Result<(String, Option<i64>), String> {
    match range {
        "week" => Ok(("week".to_string(), Some(now - 7 * 86_400))),
        "month" => Ok(("month".to_string(), Some(now - 30 * 86_400))),
        "year" => Ok(("year".to_string(), Some(now - 365 * 86_400))),
        "all" => Ok(("all".to_string(), None)),
        "4w" => Ok(("4w".to_string(), Some(now - 28 * 86_400))),
        _ => Err("History range must be week, month, year, all, or 4w".to_string()),
    }
}

const FINALIZED_HISTORY_SELECT_ALL: &str = r#"
    SELECT h.source_key, h.source_json,
           COALESCE(
               NULLIF(TRIM(a.qobuz_album_id), ''),
               (
                   SELECT NULLIF(TRIM(v.provider_id), '')
                   FROM album_versions v
                   WHERE v.album_id = a.id AND v.provider = 'qobuz'
                   ORDER BY v.id
                   LIMIT 1
               )
           ) AS linked_qobuz_album_id,
           COALESCE(NULLIF(TRIM(t.title), ''), h.title) AS title,
           COALESCE(NULLIF(TRIM(t.album_artist), ''), NULLIF(TRIM(a.album_artist), ''), NULLIF(TRIM(t.artist), ''), h.artist) AS artist,
           COALESCE(NULLIF(TRIM(t.album), ''), NULLIF(TRIM(a.title), ''), h.album) AS album,
           t.album_id, t.genre,
           COALESCE(t.art_id, a.canonical_art_id, a.art_id) AS art_id,
           h.image_url, h.played_secs, h.counted, h.played_at
    FROM playback_history h
    LEFT JOIN tracks t ON h.source_key GLOB 'local:[0-9]*'
                      AND t.id = CAST(substr(h.source_key, 7) AS INTEGER)
    LEFT JOIN albums a ON a.id = t.album_id
    WHERE h.profile_id = ?1 AND h.played_secs IS NOT NULL AND h.played_secs > 0
    ORDER BY h.played_at DESC, h.id DESC
"#;

const FINALIZED_HISTORY_SELECT_WITH_START: &str = r#"
    SELECT h.source_key, h.source_json,
           COALESCE(
               NULLIF(TRIM(a.qobuz_album_id), ''),
               (
                   SELECT NULLIF(TRIM(v.provider_id), '')
                   FROM album_versions v
                   WHERE v.album_id = a.id AND v.provider = 'qobuz'
                   ORDER BY v.id
                   LIMIT 1
               )
           ) AS linked_qobuz_album_id,
           COALESCE(NULLIF(TRIM(t.title), ''), h.title) AS title,
           COALESCE(NULLIF(TRIM(t.album_artist), ''), NULLIF(TRIM(a.album_artist), ''), NULLIF(TRIM(t.artist), ''), h.artist) AS artist,
           COALESCE(NULLIF(TRIM(t.album), ''), NULLIF(TRIM(a.title), ''), h.album) AS album,
           t.album_id, t.genre,
           COALESCE(t.art_id, a.canonical_art_id, a.art_id) AS art_id,
           h.image_url, h.played_secs, h.counted, h.played_at
    FROM playback_history h
    LEFT JOIN tracks t ON h.source_key GLOB 'local:[0-9]*'
                      AND t.id = CAST(substr(h.source_key, 7) AS INTEGER)
    LEFT JOIN albums a ON a.id = t.album_id
    WHERE h.profile_id = ?1 AND h.played_secs IS NOT NULL AND h.played_secs > 0 AND h.played_at >= ?2
    ORDER BY h.played_at DESC, h.id DESC
"#;

const TOP_HISTORY_SONGS_SQL: &str = r#"
    WITH enriched AS (
        SELECT h.source_key,
               COALESCE(NULLIF(TRIM(t.title), ''), NULLIF(TRIM(h.title), '')) AS title,
               COALESCE(NULLIF(TRIM(t.album_artist), ''), NULLIF(TRIM(a.album_artist), ''), NULLIF(TRIM(t.artist), ''), NULLIF(TRIM(h.artist), '')) AS artist,
               COALESCE(NULLIF(TRIM(t.album), ''), NULLIF(TRIM(a.title), ''), NULLIF(TRIM(h.album), '')) AS album,
               h.played_secs, h.counted, h.played_at, h.id,
               LOWER(COALESCE(NULLIF(TRIM(t.title), ''), NULLIF(TRIM(h.title), ''))) AS title_key,
               LOWER(COALESCE(NULLIF(TRIM(t.album_artist), ''), NULLIF(TRIM(a.album_artist), ''), NULLIF(TRIM(t.artist), ''), NULLIF(TRIM(h.artist), ''), '')) AS artist_key
        FROM playback_history h
        LEFT JOIN tracks t ON h.source_key GLOB 'local:[0-9]*'
                          AND t.id = CAST(substr(h.source_key, 7) AS INTEGER)
        LEFT JOIN albums a ON a.id = t.album_id
        WHERE h.profile_id = ?1
          AND h.played_secs IS NOT NULL
          AND h.played_secs > 0
          AND (?2 IS NULL OR h.played_at >= ?3)
          AND (?4 = 0 OR h.radio = 0)
    ),
    ranked_rows AS (
        SELECT *,
               ROW_NUMBER() OVER (
                   PARTITION BY title_key, artist_key
                   ORDER BY played_at DESC, id DESC
               ) AS row_rank
        FROM enriched
        WHERE title_key IS NOT NULL AND title_key != ''
    ),
    grouped AS (
        SELECT title_key, artist_key,
               SUM(CASE WHEN counted = 1 THEN 1 ELSE 0 END) AS play_count,
               SUM(played_secs) AS listened_secs,
               MAX(played_at) AS last_played_at
        FROM ranked_rows
        GROUP BY title_key, artist_key
    ),
    representatives AS (
        SELECT title_key, artist_key, title, artist, album, source_key
        FROM (
            SELECT r.*,
                   ROW_NUMBER() OVER (
                       PARTITION BY r.title_key, r.artist_key
                       ORDER BY v.version_play_count DESC,
                                v.version_listened_secs DESC,
                                v.version_last_played_at DESC,
                                r.played_at DESC,
                                r.id DESC
                   ) AS representative_rank
            FROM ranked_rows r
            JOIN (
                SELECT title_key, artist_key, source_key,
                       SUM(CASE WHEN counted = 1 THEN 1 ELSE 0 END) AS version_play_count,
                       SUM(played_secs) AS version_listened_secs,
                       MAX(played_at) AS version_last_played_at
                FROM ranked_rows
                GROUP BY title_key, artist_key, source_key
            ) v
              ON v.title_key = r.title_key
             AND v.artist_key = r.artist_key
             AND v.source_key = r.source_key
        )
        WHERE representative_rank = 1
    )
    SELECT r.title, r.artist, r.album, r.source_key,
           g.play_count, g.listened_secs, g.last_played_at
    FROM grouped g
    JOIN representatives r
      ON r.title_key = g.title_key AND r.artist_key = g.artist_key
    ORDER BY g.play_count DESC,
             g.listened_secs DESC,
             g.last_played_at DESC,
             LOWER(r.title) ASC
    LIMIT ?5
"#;

#[derive(Debug, Clone)]
struct FinalizedHistoryRow {
    source_key: String,
    qobuz_album_id: Option<String>,
    linked_qobuz_album_id: Option<String>,
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
    album_id: Option<i64>,
    genre: Option<String>,
    art_id: Option<i64>,
    image_url: Option<String>,
    played_secs: f64,
    counted: bool,
    played_at: i64,
}

#[derive(Clone, Copy)]
enum HistoryGroup {
    Artist,
    Album,
    Song,
    Genre,
}

fn weekly_history_buckets(
    rows: &[FinalizedHistoryRow],
    start: i64,
    now: i64,
) -> Vec<ListeningTimeBucket> {
    let bucket_count = 4usize;
    let span = (now - start).max(1);
    let bucket_secs = ((span as f64) / bucket_count as f64).ceil().max(1.0) as i64;
    let mut buckets = (0..bucket_count)
        .map(|idx| {
            let bucket_start = start + (idx as i64 * bucket_secs);
            let bucket_end = if idx == bucket_count - 1 {
                now
            } else {
                (bucket_start + bucket_secs).min(now)
            };
            ListeningTimeBucket {
                key: format!("w{}", idx + 1),
                label: String::new(),
                start_at: Some(bucket_start),
                end_at: Some(bucket_end),
                listened_secs: 0.0,
            }
        })
        .collect::<Vec<_>>();
    for row in rows {
        let idx = ((row.played_at - start).max(0) / bucket_secs) as usize;
        let idx = idx.min(bucket_count - 1);
        buckets[idx].listened_secs += row.played_secs;
    }
    buckets
}

fn weekday_history_buckets(rows: &[FinalizedHistoryRow]) -> Vec<ListeningTimeBucket> {
    let labels = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    let mut buckets = labels
        .iter()
        .enumerate()
        .map(|(idx, label)| ListeningTimeBucket {
            key: idx.to_string(),
            label: (*label).to_string(),
            start_at: None,
            end_at: None,
            listened_secs: 0.0,
        })
        .collect::<Vec<_>>();
    for row in rows {
        // 1970-01-01 was a Thursday. This keeps weekday buckets deterministic
        // without adding a date/time dependency.
        let days = row.played_at.div_euclid(86_400);
        let monday_index = (days + 3).rem_euclid(7) as usize;
        buckets[monday_index].listened_secs += row.played_secs;
    }
    buckets
}

fn top_history_items(rows: &[FinalizedHistoryRow], group: HistoryGroup) -> Vec<ListeningRankItem> {
    let mut map: HashMap<String, HistoryRankAccumulator> = HashMap::new();
    for row in rows {
        let (name, subtitle) = match group {
            HistoryGroup::Artist => (row.artist.clone().filter(|v| !v.trim().is_empty()), None),
            HistoryGroup::Album => (
                row.album.clone().filter(|v| !v.trim().is_empty()),
                row.artist.clone().filter(|v| !v.trim().is_empty()),
            ),
            HistoryGroup::Song => (
                row.title.clone().filter(|v| !v.trim().is_empty()),
                row.artist.clone().filter(|v| !v.trim().is_empty()),
            ),
            HistoryGroup::Genre => (row.genre.clone().filter(|v| !v.trim().is_empty()), None),
        };
        let Some(name) = name else {
            continue;
        };
        let album = match group {
            HistoryGroup::Album => Some(name.clone()),
            HistoryGroup::Song => row.album.clone().filter(|v| !v.trim().is_empty()),
            _ => None,
        };
        let qobuz_album_id = row
            .qobuz_album_id
            .clone()
            .or_else(|| row.linked_qobuz_album_id.clone());
        let key = history_rank_key(group, row, &name, subtitle.as_deref());
        let accumulator = map.entry(key).or_insert_with(|| {
            HistoryRankAccumulator::new(ListeningRankItem {
                name,
                subtitle,
                album,
                album_id: row.album_id,
                qobuz_album_id,
                listened_secs: 0.0,
                play_count: 0,
                art_id: row.art_id,
                image_url: row.image_url.clone(),
            })
        });
        accumulator.item.listened_secs += row.played_secs;
        if row.counted {
            accumulator.item.play_count += 1;
        }
        if accumulator.item.art_id.is_none() {
            accumulator.item.art_id = row.art_id;
        }
        if accumulator.item.image_url.is_none() {
            accumulator.item.image_url = row.image_url.clone();
        }
        if accumulator.item.album_id.is_none() {
            accumulator.item.album_id = row.album_id;
        }
        if accumulator.item.qobuz_album_id.is_none() {
            accumulator.item.qobuz_album_id = row
                .qobuz_album_id
                .clone()
                .or_else(|| row.linked_qobuz_album_id.clone());
        }
        if matches!(group, HistoryGroup::Song) {
            accumulator.add_song_version_play(row);
        }
    }
    let mut items = map
        .into_values()
        .map(|accumulator| accumulator.item)
        .collect::<Vec<_>>();
    match group {
        HistoryGroup::Song => items.sort_by(|a, b| {
            b.play_count
                .cmp(&a.play_count)
                .then_with(|| {
                    b.listened_secs
                        .partial_cmp(&a.listened_secs)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .then_with(|| a.name.cmp(&b.name))
        }),
        _ => items.sort_by(|a, b| {
            b.listened_secs
                .partial_cmp(&a.listened_secs)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.play_count.cmp(&a.play_count))
                .then_with(|| a.name.cmp(&b.name))
        }),
    }
    items
}

struct HistoryRankAccumulator {
    item: ListeningRankItem,
    representative: Option<HistorySongVersionStats>,
    versions: HashMap<String, HistorySongVersionStats>,
}

impl HistoryRankAccumulator {
    fn new(item: ListeningRankItem) -> Self {
        Self {
            item,
            representative: None,
            versions: HashMap::new(),
        }
    }

    fn add_song_version_play(&mut self, row: &FinalizedHistoryRow) {
        let version_key = history_song_version_key(row);
        let version = self
            .versions
            .entry(version_key)
            .or_insert_with(|| HistorySongVersionStats::from_row(row));
        version.add_play(row);
        let should_replace = self
            .representative
            .as_ref()
            .map(|representative| version.is_better_representative_than(representative))
            .unwrap_or(true);
        if should_replace {
            let version = version.clone();
            version.apply_to_item(&mut self.item);
            self.representative = Some(version);
        }
    }
}

#[derive(Clone)]
struct HistorySongVersionStats {
    name: String,
    subtitle: Option<String>,
    album: Option<String>,
    album_id: Option<i64>,
    qobuz_album_id: Option<String>,
    art_id: Option<i64>,
    image_url: Option<String>,
    play_count: i64,
    listened_secs: f64,
    last_played_at: i64,
    display_played_at: i64,
}

impl HistorySongVersionStats {
    fn from_row(row: &FinalizedHistoryRow) -> Self {
        Self {
            name: row.title.clone().unwrap_or_default(),
            subtitle: row.artist.clone().filter(|v| !v.trim().is_empty()),
            album: row.album.clone().filter(|v| !v.trim().is_empty()),
            album_id: row.album_id,
            qobuz_album_id: row
                .qobuz_album_id
                .clone()
                .or_else(|| row.linked_qobuz_album_id.clone()),
            art_id: row.art_id,
            image_url: row.image_url.clone(),
            play_count: 0,
            listened_secs: 0.0,
            last_played_at: row.played_at,
            display_played_at: row.played_at,
        }
    }

    fn add_play(&mut self, row: &FinalizedHistoryRow) {
        self.listened_secs += row.played_secs;
        if row.counted {
            self.play_count += 1;
        }
        self.last_played_at = self.last_played_at.max(row.played_at);
        if row.played_at >= self.display_played_at {
            self.name = row.title.clone().unwrap_or_default();
            self.subtitle = row.artist.clone().filter(|v| !v.trim().is_empty());
            self.album = row.album.clone().filter(|v| !v.trim().is_empty());
            self.album_id = row.album_id;
            self.qobuz_album_id = row
                .qobuz_album_id
                .clone()
                .or_else(|| row.linked_qobuz_album_id.clone());
            self.art_id = row.art_id;
            self.image_url = row.image_url.clone();
            self.display_played_at = row.played_at;
        }
    }

    fn is_better_representative_than(&self, other: &Self) -> bool {
        self.play_count
            .cmp(&other.play_count)
            .then_with(|| {
                self.listened_secs
                    .partial_cmp(&other.listened_secs)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| self.last_played_at.cmp(&other.last_played_at))
            .then_with(|| self.name.cmp(&other.name).reverse())
            .is_gt()
    }

    fn apply_to_item(&self, item: &mut ListeningRankItem) {
        item.name = self.name.clone();
        item.subtitle = self.subtitle.clone();
        item.album = self.album.clone();
        item.album_id = self.album_id;
        item.qobuz_album_id = self.qobuz_album_id.clone();
        item.art_id = self.art_id;
        item.image_url = self.image_url.clone();
    }
}

fn history_song_version_key(row: &FinalizedHistoryRow) -> String {
    if !row.source_key.trim().is_empty() {
        return row.source_key.to_lowercase();
    }
    if let Some(qobuz_album_id) = row
        .qobuz_album_id
        .as_deref()
        .or(row.linked_qobuz_album_id.as_deref())
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        return format!("qobuz_album:{}", qobuz_album_id.to_lowercase());
    }
    if let Some(album_id) = row.album_id {
        return format!("local_album:{album_id}");
    }
    format!(
        "{}::{}",
        row.title.as_deref().unwrap_or_default().to_lowercase(),
        row.album.as_deref().unwrap_or_default().to_lowercase()
    )
}

fn history_rank_key(
    group: HistoryGroup,
    row: &FinalizedHistoryRow,
    name: &str,
    subtitle: Option<&str>,
) -> String {
    if let HistoryGroup::Album = group {
        if let Some(qobuz_album_id) = row
            .linked_qobuz_album_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
        {
            return format!("qobuz_album:{}", qobuz_album_id.to_lowercase());
        }
        if let Some(album_id) = row.album_id {
            return format!("local_album:{album_id}");
        }
        if let Some(qobuz_album_id) = row
            .qobuz_album_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
        {
            return format!("qobuz_album:{}", qobuz_album_id.to_lowercase());
        }
    }
    format!(
        "{}::{}",
        name.to_lowercase(),
        subtitle.unwrap_or_default().to_lowercase()
    )
}

fn qobuz_album_id_from_history_source(
    source_key: &str,
    source_json: &str,
) -> Result<Option<String>, String> {
    if !source_key.starts_with("qobuz:") {
        return Ok(None);
    }
    let source = serde_json::from_str::<SourceRef>(source_json)
        .map_err(|e| format!("history qobuz source parse: {e}"))?;
    Ok(match source {
        SourceRef::QobuzTrack { album_id, .. } => album_id.filter(|v| !v.trim().is_empty()),
        _ => None,
    })
}
