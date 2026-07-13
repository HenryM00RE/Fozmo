use super::artwork::best_artwork_id_with_conn;
use super::matching::{levenshtein, normalize_for_match, pair_tracks};
use super::*;
use crate::audio::player::TrackCover;
use crate::services::qobuz::{QobuzAlbum, QobuzAlbumDetail, QobuzTrack};
use rusqlite::{OptionalExtension, params};
use std::collections::HashMap;

#[derive(Debug)]
struct PlaybackVersion {
    provider: String,
    sample_rate: Option<i64>,
    bit_depth: Option<i64>,
}

impl PlaybackVersion {
    fn qobuz_format_id(&self) -> Option<u32> {
        if self.provider != "qobuz" {
            return None;
        }
        match (self.sample_rate.unwrap_or(0), self.bit_depth.unwrap_or(0)) {
            (rate, depth) if depth <= 16 && rate <= 44_100 => Some(6),
            (rate, _) if rate > 96_000 => Some(27),
            _ => Some(7),
        }
    }
}

impl Library {
    pub(super) fn qobuz_payload_for_album(
        &self,
        album_id: i64,
    ) -> Result<Option<QobuzAlbumDetail>, String> {
        let conn = self.conn.lock().unwrap();
        let payload: Option<String> = conn
            .query_row(
                "SELECT qobuz_payload_json FROM albums WHERE id = ?1 AND qobuz_match_status = 'matched'",
                [album_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| format!("qobuz payload lookup: {e}"))?
            .flatten();
        payload
            .map(|json| {
                serde_json::from_str::<QobuzAlbumDetail>(&json)
                    .map_err(|e| format!("qobuz payload parse: {e}"))
            })
            .transpose()
    }

    pub(super) fn canonical_album(
        &self,
        album: &AlbumSummary,
    ) -> Result<Option<CanonicalAlbum>, String> {
        let Some(detail) = self.qobuz_payload_for_album(album.id)? else {
            return Ok(None);
        };
        let q = detail.album;
        Ok(Some(CanonicalAlbum {
            title: q.title,
            album_artist: Some(q.artist),
            release_date: q.release_date,
            year: q.year,
            track_count: detail.tracks.len() as i64,
            art_id: album.canonical_art_id.or(album.art_id),
            image_url: q.image_url,
            qobuz_album_id: q.id,
            maximum_sampling_rate: q.maximum_sampling_rate.map(|v| (v * 1000.0).round() as i64),
            maximum_bit_depth: q.maximum_bit_depth.map(|v| v as i64),
            hires: q.hires,
            description: q.description,
            genre: q.genre,
            label: q.label,
            duration_secs: q
                .duration
                .map(|v| v as f64)
                .or_else(|| Some(detail.tracks.iter().map(|t| t.duration as f64).sum())),
        }))
    }

    pub(super) fn qobuz_track_links(
        &self,
        album_id: i64,
    ) -> Result<Vec<QobuzTrackLinkSummary>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                r#"
                SELECT lvt.local_track_id, qvt.provider_track_id, l.confidence, l.match_kind, l.status
                FROM version_track_links l
                JOIN version_tracks lvt ON lvt.id = l.local_version_track_id
                JOIN version_tracks qvt ON qvt.id = l.qobuz_version_track_id
                WHERE l.album_id = ?1
                  AND lvt.local_track_id IS NOT NULL
                  AND qvt.provider_track_id IS NOT NULL
                ORDER BY qvt.disc_number, qvt.track_number, qvt.id
                "#,
            )
            .map_err(|e| format!("qobuz links query: {e}"))?;
        let rows = stmt
            .query_map([album_id], |row| {
                Ok(QobuzTrackLinkSummary {
                    local_track_id: row.get(0)?,
                    qobuz_track_id: row.get(1)?,
                    confidence: row.get(2)?,
                    match_kind: row.get(3)?,
                    status: row.get(4)?,
                })
            })
            .map_err(|e| format!("qobuz links map: {e}"))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| format!("qobuz link row: {e}"))?);
        }
        Ok(out)
    }

    pub fn primary_qobuz_track_for_local_track(
        &self,
        local_track_id: i64,
    ) -> Result<Option<QobuzTrack>, String> {
        #[derive(Debug)]
        struct PrimaryQobuzLookup {
            payload_json: String,
            provider_track_id: Option<String>,
            local_title: String,
            track_number: Option<u32>,
            disc_number: Option<u32>,
        }

        let lookup = {
            let conn = self.conn.lock().unwrap();
            conn.query_row(
                r#"
                SELECT a.qobuz_payload_json,
                       qvt.provider_track_id,
                       t.title,
                       t.track_number,
                       t.disc_number
                FROM tracks t
                JOIN albums a ON a.id = t.album_id
                JOIN album_versions primary_v
                  ON primary_v.id = a.primary_version_id
                 AND primary_v.album_id = a.id
                 AND primary_v.provider = 'qobuz'
                LEFT JOIN version_tracks lvt
                  ON lvt.local_track_id = t.id
                LEFT JOIN version_track_links link
                  ON link.local_version_track_id = lvt.id
                 AND link.album_id = a.id
                 AND link.status = 'linked'
                LEFT JOIN version_tracks qvt
                  ON qvt.id = link.qobuz_version_track_id
                 AND qvt.version_id = primary_v.id
                WHERE t.id = ?1
                  AND a.qobuz_match_status = 'matched'
                  AND a.qobuz_payload_json IS NOT NULL
                ORDER BY link.confidence DESC, qvt.id
                LIMIT 1
                "#,
                [local_track_id],
                |row| {
                    Ok(PrimaryQobuzLookup {
                        payload_json: row.get(0)?,
                        provider_track_id: row.get(1)?,
                        local_title: row.get(2)?,
                        track_number: row.get::<_, Option<i64>>(3)?.map(|v| v as u32),
                        disc_number: row.get::<_, Option<i64>>(4)?.map(|v| v as u32),
                    })
                },
            )
            .optional()
            .map_err(|e| format!("primary qobuz track lookup: {e}"))?
        };
        let Some(lookup) = lookup else {
            return Ok(None);
        };
        let detail: QobuzAlbumDetail = serde_json::from_str(&lookup.payload_json)
            .map_err(|e| format!("primary qobuz payload parse: {e}"))?;
        let by_provider_id = lookup.provider_track_id.as_deref().and_then(|id| {
            detail
                .tracks
                .iter()
                .find(|track| track.id.to_string() == id)
        });
        let by_position = lookup.track_number.and_then(|track_number| {
            let disc_number = lookup.disc_number.unwrap_or(1);
            detail.tracks.iter().find(|track| {
                track.track_number == Some(track_number)
                    && track.disc_number.unwrap_or(1) == disc_number
            })
        });
        let local_title_key = normalize_for_match(&lookup.local_title);
        let by_title = detail
            .tracks
            .iter()
            .find(|track| normalize_for_match(&track.title) == local_title_key);

        Ok(by_provider_id
            .or(by_position)
            .or(by_title)
            .filter(|track| track.streamable)
            .cloned())
    }

    pub fn preferred_play_source_for_local_track(
        &self,
        local_track_id: i64,
    ) -> Result<Option<ResolvedPlaySource>, String> {
        if let Some(qobuz) = self.primary_qobuz_track_for_local_track(local_track_id)? {
            let format_id = self.primary_qobuz_format_id_for_local_track(local_track_id)?;
            return Ok(Some(qobuz_source_from_track(&qobuz, format_id)));
        }

        Ok(self
            .track_by_id(local_track_id)?
            .map(|track| local_source_from_track(&track)))
    }

    pub fn qobuz_album_id_for_local_album(&self, album_id: i64) -> Result<Option<String>, String> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            r#"
            SELECT COALESCE(a.qobuz_album_id, primary_v.provider_id, qobuz_v.provider_id)
            FROM albums a
            LEFT JOIN album_versions primary_v
              ON primary_v.id = a.primary_version_id
             AND primary_v.album_id = a.id
             AND primary_v.provider = 'qobuz'
            LEFT JOIN album_versions qobuz_v
              ON qobuz_v.album_id = a.id
             AND qobuz_v.provider = 'qobuz'
            WHERE a.id = ?1
              AND (
                a.qobuz_match_status = 'matched'
                OR primary_v.id IS NOT NULL
                OR qobuz_v.id IS NOT NULL
              )
            ORDER BY
              CASE WHEN primary_v.id IS NOT NULL THEN 0 ELSE 1 END,
              qobuz_v.id
            LIMIT 1
            "#,
            [album_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
        .map(|value| value.flatten())
        .map_err(|e| format!("local album qobuz id lookup: {e}"))
    }

    fn primary_qobuz_format_id_for_local_track(
        &self,
        local_track_id: i64,
    ) -> Result<Option<u32>, String> {
        let conn = self.conn.lock().unwrap();
        let version = conn
            .query_row(
                r#"
                SELECT primary_v.provider, primary_v.sample_rate, primary_v.bit_depth
                FROM tracks t
                JOIN albums a ON a.id = t.album_id
                JOIN album_versions primary_v
                  ON primary_v.id = a.primary_version_id
                 AND primary_v.album_id = a.id
                 AND primary_v.provider = 'qobuz'
                WHERE t.id = ?1
                "#,
                [local_track_id],
                |row| {
                    Ok(PlaybackVersion {
                        provider: row.get(0)?,
                        sample_rate: row.get(1)?,
                        bit_depth: row.get(2)?,
                    })
                },
            )
            .optional()
            .map_err(|e| format!("primary qobuz format lookup: {e}"))?;
        Ok(version.as_ref().and_then(PlaybackVersion::qobuz_format_id))
    }

    pub(super) fn canonical_tracks(
        &self,
        album: &AlbumSummary,
        local_tracks: &[TrackSummary],
    ) -> Result<Vec<CanonicalTrack>, String> {
        let profile_id = self.active_profile_id();
        self.canonical_tracks_for_profile(&profile_id, album, local_tracks)
    }

    pub(super) fn canonical_tracks_for_profile(
        &self,
        profile_id: &str,
        album: &AlbumSummary,
        local_tracks: &[TrackSummary],
    ) -> Result<Vec<CanonicalTrack>, String> {
        let Some(detail) = self.qobuz_payload_for_album(album.id)? else {
            return Ok(Vec::new());
        };
        let links = self.qobuz_track_links(album.id)?;
        let links_by_qobuz: HashMap<String, &QobuzTrackLinkSummary> = links
            .iter()
            .filter(|l| l.status == "linked" && l.confidence >= 80)
            .map(|l| (l.qobuz_track_id.clone(), l))
            .collect();
        let locals: HashMap<i64, &TrackSummary> = local_tracks.iter().map(|t| (t.id, t)).collect();
        let mut tracks = Vec::with_capacity(detail.tracks.len());
        for (idx, q) in ordered_qobuz_tracks(&detail.tracks).into_iter().enumerate() {
            let qid = q.id.to_string();
            let local = links_by_qobuz
                .get(&qid)
                .and_then(|link| locals.get(&link.local_track_id).copied());
            let play_source = local
                .map(local_source_from_track)
                .or_else(|| Some(qobuz_source_from_track(q, None)));
            let qobuz_source = Some(qobuz_source_from_track(q, None));
            let qobuz_playback = self.qobuz_playback_summary_for_profile(profile_id, q.id);
            let playback = local
                .map(|track| PlaybackSummary {
                    play_count: track.play_count,
                    last_played_at: track.last_played_at,
                    listened_secs: track.listened_secs,
                })
                .unwrap_or(qobuz_playback);
            tracks.push(CanonicalTrack {
                title: q.title.clone(),
                artist: Some(q.artist.clone()),
                album: Some(q.album.clone()),
                track_number: Some(q.track_number.unwrap_or((idx + 1) as u32) as i64),
                disc_number: Some(q.disc_number.unwrap_or(1) as i64),
                duration_secs: Some(q.duration as f64)
                    .filter(|v| *v > 0.0)
                    .or_else(|| local.and_then(|t| t.duration_secs)),
                sample_rate: local
                    .and_then(|t| t.sample_rate)
                    .or_else(|| q.maximum_sampling_rate.map(|v| (v * 1000.0).round() as i64)),
                format: local
                    .and_then(|t| t.format.clone())
                    .or_else(|| Some("FLAC".to_string())),
                bit_depth: local
                    .and_then(|t| t.bit_depth)
                    .or_else(|| q.maximum_bit_depth.map(|v| v as i64)),
                image_url: q.image_url.clone(),
                qobuz_track_id: Some(qid),
                play_source,
                qobuz_source,
                composer: q.composer.clone(),
                work: q.work.clone(),
                isrc: q.isrc.clone(),
                copyright: q.copyright.clone(),
                performers_raw: q.performers_raw.clone(),
                credits: q.credits.clone(),
                play_count: playback.play_count,
                last_played_at: playback.last_played_at,
                listened_secs: playback.listened_secs,
            });
        }
        Ok(tracks)
    }

    fn qobuz_playback_summary_for_profile(
        &self,
        profile_id: &str,
        track_id: u64,
    ) -> PlaybackSummary {
        let key = format!("qobuz:{track_id}");
        self.playback_summaries_for_keys_for_profile(profile_id, std::slice::from_ref(&key))
            .ok()
            .and_then(|mut summaries| summaries.remove(&key))
            .unwrap_or_default()
    }

    pub fn add_manual_qobuz_version(
        &self,
        album_id: i64,
        req: ManualQobuzVersionRequest,
    ) -> Result<Option<AlbumDetail>, String> {
        let Some(album) = self.album(album_id)? else {
            return Ok(None);
        };
        let now = now_secs();
        let provider_id = req
            .provider_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("manual-{now}"));
        let title = req
            .title
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| album.title.clone());
        let artist = req.artist.or(album.album_artist.clone());
        let track_count = req.track_count.unwrap_or(album.track_count).max(0);
        let source_label = req
            .source_label
            .filter(|s| !s.trim().is_empty())
            .or_else(|| Some("Qobuz".to_string()));
        let conn = self.conn.lock().unwrap();
        conn.execute(
            r#"
            INSERT INTO album_versions (
                album_id, provider, provider_id, title, artist, year, track_count,
                art_id, format, sample_rate, bit_depth, source_label, status,
                payload_json, created_at, updated_at
            )
            VALUES (?1, 'qobuz', ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 'available', '{}', ?12, ?12)
            ON CONFLICT(album_id, provider, provider_id) DO UPDATE SET
                title = excluded.title,
                artist = excluded.artist,
                year = excluded.year,
                track_count = excluded.track_count,
                format = excluded.format,
                sample_rate = excluded.sample_rate,
                bit_depth = excluded.bit_depth,
                source_label = excluded.source_label,
                status = 'available',
                updated_at = excluded.updated_at
            "#,
            params![
                album_id,
                provider_id,
                title,
                artist,
                req.year.or(album.year),
                track_count,
                album.art_id,
                req.format,
                req.sample_rate,
                req.bit_depth,
                source_label,
                now
            ],
        )
        .map_err(|e| format!("add qobuz version: {e}"))?;
        drop(conn);
        self.album_detail(album_id)
    }

    pub fn link_qobuz_album(
        &self,
        album_id: i64,
        detail: &QobuzAlbumDetail,
        qobuz_cover: Option<TrackCover>,
        score: i64,
        status: &str,
    ) -> Result<Option<AlbumDetail>, String> {
        let Some(album) = self.album(album_id)? else {
            return Ok(None);
        };
        let now = now_secs();
        let qobuz_art_id = qobuz_cover
            .as_ref()
            .and_then(|cover| self.save_artwork(cover, "qobuz").ok());
        let canonical_art_id = self.choose_canonical_art(album_id, album.art_id, qobuz_art_id)?;
        let payload_json = serde_json::to_string(detail).map_err(|e| format!("qobuz json: {e}"))?;
        let q = &detail.album;

        {
            let mut conn = self.conn.lock().unwrap();
            let tx = conn
                .transaction()
                .map_err(|e| format!("begin qobuz link transaction: {e}"))?;
            Self::sync_local_versions_for_album_with_conn(&tx, album_id)?;
            tx.execute(
                r#"
                UPDATE albums
                SET qobuz_album_id = ?2,
                    qobuz_match_status = ?3,
                    qobuz_match_confidence = ?4,
                    qobuz_payload_json = ?5,
                    canonical_art_id = ?6,
                    updated_at = ?7
                WHERE id = ?1
                "#,
                params![
                    album_id,
                    q.id,
                    status,
                    score,
                    payload_json,
                    canonical_art_id,
                    now
                ],
            )
            .map_err(|e| format!("link qobuz album: {e}"))?;

            tx.execute(
                r#"
                INSERT INTO album_versions (
                    album_id, provider, provider_id, title, artist, year, track_count,
                    art_id, format, sample_rate, bit_depth, source_label, status,
                    payload_json, created_at, updated_at
                )
                VALUES (?1, 'qobuz', ?2, ?3, ?4, ?5, ?6, ?7, 'FLAC', ?8, ?9, ?10, 'available', ?11, ?12, ?12)
                ON CONFLICT(album_id, provider, provider_id) DO UPDATE SET
                    title = excluded.title,
                    artist = excluded.artist,
                    year = excluded.year,
                    track_count = excluded.track_count,
                    art_id = excluded.art_id,
                    format = excluded.format,
                    sample_rate = excluded.sample_rate,
                    bit_depth = excluded.bit_depth,
                    source_label = excluded.source_label,
                    payload_json = excluded.payload_json,
                    status = 'available',
                    updated_at = excluded.updated_at
                "#,
                params![
                    album_id,
                    q.id,
                    q.title,
                    q.artist,
                    q.year,
                    detail.tracks.len() as i64,
                    qobuz_art_id,
                    q.maximum_sampling_rate.map(|v| (v * 1000.0).round() as i64),
                    q.maximum_bit_depth.map(|v| v as i64),
                    if q.hires { "Qobuz Hi-Res" } else { "Qobuz" },
                    payload_json,
                    now
                ],
            )
            .map_err(|e| format!("upsert qobuz version: {e}"))?;

            let qobuz_version_id: i64 = tx
                .query_row(
                    "SELECT id FROM album_versions WHERE album_id = ?1 AND provider = 'qobuz' AND provider_id = ?2",
                    params![album_id, q.id],
                    |row| row.get(0),
                )
                .map_err(|e| format!("qobuz version id: {e}"))?;

            for (idx, track) in detail.tracks.iter().enumerate() {
                tx.execute(
                    r#"
                    INSERT INTO version_tracks (
                        version_id, provider_track_id, local_track_id, title, artist,
                        track_number, disc_number, duration_secs, sample_rate, format,
                        bit_depth, status, created_at, updated_at
                    )
                    VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6, ?7, ?8, 'FLAC', ?9, 'available', ?10, ?10)
                    ON CONFLICT(version_id, provider_track_id) DO UPDATE SET
                        title = excluded.title,
                        artist = excluded.artist,
                        track_number = excluded.track_number,
                        disc_number = excluded.disc_number,
                        duration_secs = excluded.duration_secs,
                        sample_rate = excluded.sample_rate,
                        bit_depth = excluded.bit_depth,
                        status = 'available',
                        updated_at = excluded.updated_at
                    "#,
                    params![
                        qobuz_version_id,
                        track.id.to_string(),
                        track.title,
                        track.artist,
                        track.track_number.unwrap_or((idx + 1) as u32) as i64,
                        track.disc_number.unwrap_or(1) as i64,
                        track.duration as f64,
                        track
                            .maximum_sampling_rate
                            .map(|v| (v * 1000.0).round() as i64),
                        track.maximum_bit_depth.map(|v| v as i64),
                        now
                    ],
                )
                .map_err(|e| format!("upsert qobuz version track: {e}"))?;
            }
            tx.commit()
                .map_err(|e| format!("commit qobuz link transaction: {e}"))?;
        }

        self.rebuild_qobuz_track_links(album_id, detail)?;
        self.album_detail(album_id)
    }

    pub fn set_qobuz_album_art(&self, album_id: i64, cover: &TrackCover) -> Result<(), String> {
        let Some(album) = self.album(album_id)? else {
            return Err("Album not found".to_string());
        };
        let art_id = self.save_artwork(cover, "qobuz")?;
        let canonical_art_id = self.choose_canonical_art(album_id, album.art_id, Some(art_id))?;
        let now = now_secs();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE albums SET canonical_art_id = ?2, updated_at = ?3 WHERE id = ?1",
            params![album_id, canonical_art_id, now],
        )
        .map_err(|e| format!("set qobuz album art: {e}"))?;
        conn.execute(
            "UPDATE album_versions SET art_id = ?2, updated_at = ?3 WHERE album_id = ?1 AND provider = 'qobuz'",
            params![album_id, art_id, now],
        )
        .map_err(|e| format!("set qobuz version art: {e}"))?;
        Ok(())
    }

    pub fn unlink_qobuz_album(&self, album_id: i64) -> Result<Option<AlbumDetail>, String> {
        if self.album(album_id)?.is_none() {
            return Ok(None);
        }
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM version_track_links WHERE album_id = ?1",
            [album_id],
        )
        .map_err(|e| format!("delete qobuz links: {e}"))?;
        conn.execute(
            "DELETE FROM album_versions WHERE album_id = ?1 AND provider = 'qobuz'",
            [album_id],
        )
        .map_err(|e| format!("delete qobuz versions: {e}"))?;
        conn.execute(
            r#"
            UPDATE albums
            SET qobuz_album_id = NULL,
                qobuz_match_status = NULL,
                qobuz_match_confidence = NULL,
                qobuz_payload_json = NULL,
                canonical_art_id = CASE
                    WHEN COALESCE(art_locked, 0) = 0
                     AND canonical_art_id IN (SELECT id FROM artworks WHERE source = 'qobuz')
                    THEN NULL
                    ELSE canonical_art_id
                END,
                updated_at = ?2
            WHERE id = ?1
            "#,
            params![album_id, now_secs()],
        )
        .map_err(|e| format!("unlink qobuz album: {e}"))?;
        drop(conn);
        self.album_detail(album_id)
    }

    pub fn mark_qobuz_candidate_for_review(
        &self,
        album_id: i64,
        detail: &QobuzAlbumDetail,
        score: i64,
    ) -> Result<Option<AlbumDetail>, String> {
        if self.album(album_id)?.is_none() {
            return Ok(None);
        }
        let payload_json = serde_json::to_string(detail).map_err(|e| format!("qobuz json: {e}"))?;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            r#"
            UPDATE albums
            SET qobuz_album_id = ?2,
                qobuz_match_status = 'needs_review',
                qobuz_match_confidence = ?3,
                qobuz_payload_json = ?4,
                updated_at = ?5
            WHERE id = ?1
            "#,
            params![album_id, detail.album.id, score, payload_json, now_secs()],
        )
        .map_err(|e| format!("mark qobuz review: {e}"))?;
        drop(conn);
        self.album_detail(album_id)
    }

    #[allow(dead_code)]
    pub fn album_by_qobuz_id(&self, qobuz_album_id: &str) -> Result<Option<AlbumDetail>, String> {
        let profile_id = self.active_profile_id();
        self.album_by_qobuz_id_for_profile(&profile_id, qobuz_album_id)
    }

    pub fn album_by_qobuz_id_for_profile(
        &self,
        profile_id: &str,
        qobuz_album_id: &str,
    ) -> Result<Option<AlbumDetail>, String> {
        let normalized_id = normalize_qobuz_album_id(qobuz_album_id);
        if normalized_id.is_empty() {
            return Ok(None);
        }
        let album_id: Option<i64> = {
            let conn = self.conn.lock().unwrap();
            conn.query_row(
                r#"
                SELECT a.id
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
                params![qobuz_album_id, normalized_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| format!("qobuz album lookup: {e}"))?
        };
        album_id
            .map(|id| self.album_detail_for_profile(profile_id, id))
            .transpose()
            .map(|v| v.flatten())
    }

    pub fn resolve_album_playback(
        &self,
        album_id: i64,
        start_index: usize,
        shuffle: bool,
        version_id: Option<i64>,
    ) -> Result<Option<AlbumPlaybackPlan>, String> {
        let Some(album) = self.album(album_id)? else {
            return Ok(None);
        };
        let effective_version_id = version_id.or(album.primary_version_id);
        let playback_version = if let Some(version_id) = effective_version_id {
            let conn = self.conn.lock().unwrap();
            conn.query_row(
                "SELECT provider, sample_rate, bit_depth FROM album_versions WHERE id = ?1 AND album_id = ?2",
                params![version_id, album_id],
                |row| {
                    Ok(PlaybackVersion {
                        provider: row.get(0)?,
                        sample_rate: row.get(1)?,
                        bit_depth: row.get(2)?,
                    })
                },
            )
            .optional()
            .map_err(|e| format!("playback version lookup: {e}"))?
        } else {
            None
        };
        let tracks = match (
            effective_version_id,
            playback_version.as_ref().map(|v| v.provider.as_str()),
        ) {
            (Some(version_id), Some("local")) => {
                self.local_album_tracks_for_version(album_id, version_id)?
            }
            _ => self.primary_local_album_tracks(&album)?,
        };
        let mut sources: Vec<ResolvedPlaySource> =
            match playback_version.as_ref().map(|v| v.provider.as_str()) {
                Some("local") => tracks.iter().map(local_source_from_track).collect(),
                Some("qobuz") => self
                    .qobuz_payload_for_album(album_id)?
                    .map(|detail| {
                        let format_id = playback_version
                            .as_ref()
                            .and_then(PlaybackVersion::qobuz_format_id);
                        ordered_qobuz_tracks(&detail.tracks)
                            .into_iter()
                            .map(|track| qobuz_source_from_track(track, format_id))
                            .collect()
                    })
                    .unwrap_or_default(),
                _ => {
                    if self.qobuz_payload_for_album(album_id)?.is_some() {
                        self.canonical_tracks(&album, &tracks)?
                            .into_iter()
                            .filter_map(|t| t.play_source)
                            .collect()
                    } else {
                        tracks.iter().map(local_source_from_track).collect()
                    }
                }
            };
        if sources.is_empty() {
            return Ok(Some(AlbumPlaybackPlan { album_id, sources }));
        }
        let start = start_index.min(sources.len() - 1);
        sources = sources.split_off(start);
        if shuffle && sources.len() > 2 {
            shuffle_sources(&mut sources[1..]);
        }
        Ok(Some(AlbumPlaybackPlan { album_id, sources }))
    }

    pub fn qobuz_match_score(
        &self,
        album_id: i64,
        detail: &QobuzAlbumDetail,
    ) -> Result<i64, String> {
        let Some(album) = self.album(album_id)? else {
            return Ok(0);
        };
        let tracks = self.primary_local_album_tracks(&album)?;
        Ok(score_qobuz_album_match(
            &album,
            &tracks,
            &detail.album,
            &detail.tracks,
        ))
    }

    /// Decide whether a Qobuz album may be auto-linked as this album's
    /// canonical version. The fuzzy score alone is not enough — auto-linking
    /// requires either a barcode match against the MusicBrainz release or
    /// complete evidence (exact title + artist, equal track count, every
    /// local track paired with high confidence). A barcode *conflict* vetoes
    /// auto-linking even when the text looks perfect: same-name editions are
    /// exactly the failure mode this guards against.
    pub fn qobuz_link_assessment(
        &self,
        album_id: i64,
        detail: &QobuzAlbumDetail,
    ) -> Result<Option<QobuzMatchAssessment>, String> {
        let Some(album) = self.album(album_id)? else {
            return Ok(None);
        };
        let tracks = self.primary_local_album_tracks(&album)?;
        Ok(Some(self.qobuz_link_assessment_for_metadata(
            &album, &tracks, detail,
        )))
    }

    pub fn qobuz_link_assessment_for_version(
        &self,
        album_id: i64,
        version_id: i64,
        detail: &QobuzAlbumDetail,
    ) -> Result<Option<QobuzMatchAssessment>, String> {
        let Some(album) = self.album(album_id)? else {
            return Ok(None);
        };
        let tracks = self.local_album_tracks_for_version(album_id, version_id)?;
        Ok(Some(self.qobuz_link_assessment_for_metadata(
            &album, &tracks, detail,
        )))
    }

    pub fn qobuz_link_assessment_for_metadata(
        &self,
        album: &AlbumSummary,
        tracks: &[TrackSummary],
        detail: &QobuzAlbumDetail,
    ) -> QobuzMatchAssessment {
        let score = score_qobuz_album_match(album, tracks, &detail.album, &detail.tracks);
        let barcode_match = match (album.mb_barcode.as_deref(), detail.album.upc.as_deref()) {
            (Some(mb), Some(upc)) => Some(normalize_barcode(mb) == normalize_barcode(upc)),
            _ => None,
        };
        let auto_link = match barcode_match {
            Some(true) => qobuz_barcode_match_has_supporting_evidence(album, tracks, detail),
            Some(false) => false,
            None => qobuz_evidence_complete(album, tracks, detail),
        };
        QobuzMatchAssessment {
            score,
            auto_link,
            barcode_match,
        }
    }

    fn choose_canonical_art(
        &self,
        album_id: i64,
        local_art_id: Option<i64>,
        qobuz_art_id: Option<i64>,
    ) -> Result<Option<i64>, String> {
        let conn = self.conn.lock().unwrap();
        let locked_current: Option<Option<i64>> = conn
            .query_row(
                "SELECT canonical_art_id FROM albums WHERE id = ?1 AND COALESCE(art_locked, 0) != 0",
                [album_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| format!("canonical art lock lookup: {e}"))?;
        if let Some(current) = locked_current {
            return Ok(current.or(local_art_id));
        }
        best_artwork_id_with_conn(&conn, local_art_id, qobuz_art_id)
    }

    fn rebuild_qobuz_track_links(
        &self,
        album_id: i64,
        detail: &QobuzAlbumDetail,
    ) -> Result<(), String> {
        let Some(album) = self.album(album_id)? else {
            return Ok(());
        };
        let local_tracks = self.primary_local_album_tracks(&album)?;
        let pairs = pair_qobuz_tracks(&local_tracks, &detail.tracks);
        let now = now_secs();
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction()
            .map_err(|e| format!("begin qobuz track link transaction: {e}"))?;
        let local_version_tracks: HashMap<i64, i64> = {
            let mut stmt = tx
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
                .map_err(|e| format!("load local version tracks: {e}"))?;
            let rows = stmt
                .query_map([album_id], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
                })
                .map_err(|e| format!("map local version tracks: {e}"))?;
            let mut tracks = HashMap::new();
            for row in rows {
                let (track_id, version_track_id) =
                    row.map_err(|e| format!("read local version track: {e}"))?;
                tracks.entry(track_id).or_insert(version_track_id);
            }
            tracks
        };
        let qobuz_version_tracks: HashMap<String, i64> = {
            let mut stmt = tx
                .prepare(
                    r#"
                    SELECT vt.provider_track_id, vt.id
                    FROM version_tracks vt
                    JOIN album_versions v ON v.id = vt.version_id
                    WHERE v.album_id = ?1 AND v.provider = 'qobuz'
                      AND vt.provider_track_id IS NOT NULL
                    "#,
                )
                .map_err(|e| format!("load qobuz version tracks: {e}"))?;
            let rows = stmt
                .query_map([album_id], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })
                .map_err(|e| format!("map qobuz version tracks: {e}"))?;
            let mut tracks = HashMap::new();
            for row in rows {
                let (provider_track_id, version_track_id) =
                    row.map_err(|e| format!("read qobuz version track: {e}"))?;
                tracks.insert(provider_track_id, version_track_id);
            }
            tracks
        };
        tx.execute(
            "UPDATE version_track_links SET status = 'unlinked', updated_at = ?2 WHERE album_id = ?1",
            params![album_id, now],
        )
        .map_err(|e| format!("mark stale qobuz track links: {e}"))?;
        for pair in pairs {
            if pair.confidence < 80 {
                continue;
            }
            let local_vt = local_version_tracks.get(&pair.local_track_id).copied();
            let qobuz_vt = qobuz_version_tracks.get(&pair.qobuz_track_id).copied();
            let (Some(local_vt), Some(qobuz_vt)) = (local_vt, qobuz_vt) else {
                continue;
            };
            tx.execute(
                r#"
                INSERT INTO version_track_links (
                    album_id, local_version_track_id, qobuz_version_track_id,
                    confidence, match_kind, status, created_at, updated_at
                )
                VALUES (?1, ?2, ?3, ?4, ?5, 'linked', ?6, ?6)
                ON CONFLICT(album_id, local_version_track_id, qobuz_version_track_id)
                DO UPDATE SET
                    confidence = excluded.confidence,
                    match_kind = excluded.match_kind,
                    status = 'linked',
                    updated_at = excluded.updated_at
                "#,
                params![
                    album_id,
                    local_vt,
                    qobuz_vt,
                    pair.confidence,
                    pair.match_kind,
                    now
                ],
            )
            .map_err(|e| format!("insert qobuz track link: {e}"))?;
        }
        Self::sync_recording_identity_for_album_with_conn(&tx, album_id)?;
        tx.commit()
            .map_err(|e| format!("commit qobuz track links: {e}"))?;
        Ok(())
    }
}

fn local_source_from_track(track: &TrackSummary) -> ResolvedPlaySource {
    ResolvedPlaySource::Local {
        track_id: track.id,
        title: track.title.clone(),
        artist: track.artist.clone().or_else(|| track.album_artist.clone()),
        album: track.album.clone(),
        art_id: track.art_id,
        duration_secs: track.duration_secs,
        file_name: track.file_name.clone(),
    }
}

fn qobuz_source_from_track(track: &QobuzTrack, format_id: Option<u32>) -> ResolvedPlaySource {
    ResolvedPlaySource::Qobuz {
        track_id: track.id,
        title: track.title.clone(),
        artist: Some(track.artist.clone()),
        album: Some(track.album.clone()),
        album_id: track.album_id.clone(),
        image_url: track.image_url.clone(),
        duration_secs: Some(track.duration as f64).filter(|v| *v > 0.0),
        format_id,
    }
}

pub(super) fn ordered_qobuz_tracks(tracks: &[QobuzTrack]) -> Vec<&QobuzTrack> {
    let mut ordered: Vec<(usize, &QobuzTrack)> = tracks.iter().enumerate().collect();
    ordered.sort_by_key(|(idx, track)| {
        (
            track.disc_number.unwrap_or(1),
            track.track_number.unwrap_or((*idx + 1) as u32),
            *idx,
        )
    });
    ordered.into_iter().map(|(_, track)| track).collect()
}

#[derive(Debug, Clone)]
pub(super) struct QobuzTrackPairing {
    pub(super) local_track_id: i64,
    pub(super) qobuz_track_id: String,
    pub(super) confidence: i64,
    pub(super) match_kind: String,
}

pub(super) fn pair_qobuz_tracks(
    local_tracks: &[TrackSummary],
    qobuz_tracks: &[QobuzTrack],
) -> Vec<QobuzTrackPairing> {
    let mb_tracks: Vec<MbTrack> = qobuz_tracks
        .iter()
        .enumerate()
        .map(|(idx, track)| MbTrack {
            recording_id: Some(track.id.to_string()),
            disc: track.disc_number.unwrap_or(1) as i64,
            position: track.track_number.unwrap_or((idx + 1) as u32) as i64,
            title: track.title.clone(),
            artist: Some(track.artist.clone()),
            length_secs: Some(track.duration as f64).filter(|v| *v > 0.0),
        })
        .collect();
    pair_tracks(local_tracks, &mb_tracks)
        .into_iter()
        .filter_map(|pair| {
            let local = local_tracks.get(pair.file_index)?;
            let qobuz = qobuz_tracks.get(pair.mb_index)?;
            let confidence = match pair.kind {
                "exact" => {
                    let local_title = normalize_for_match(&local.title);
                    let qobuz_title = normalize_for_match(&qobuz.title);
                    if local_title == qobuz_title { 100 } else { 90 }
                }
                "manual" => 100,
                _ => 84,
            };
            Some(QobuzTrackPairing {
                local_track_id: local.id,
                qobuz_track_id: qobuz.id.to_string(),
                confidence,
                match_kind: pair.kind.to_string(),
            })
        })
        .collect()
}

/// Strict evidence gate for auto-linking without a barcode: exact normalized
/// title and artist, equal track count, and every local track paired to a
/// Qobuz track at confidence ≥ 90. Deliberately *not* a score threshold — the
/// fuzzy score docks points for a ±1 year delta or a bonus track, which are
/// fine, while a deluxe edition can still score high, which is not.
pub(super) fn qobuz_evidence_complete(
    album: &AlbumSummary,
    local_tracks: &[TrackSummary],
    detail: &QobuzAlbumDetail,
) -> bool {
    if local_tracks.is_empty() || detail.tracks.len() != local_tracks.len() {
        return false;
    }
    if normalize_for_match(&album.title) != normalize_for_match(&detail.album.title) {
        return false;
    }
    let artist_ok = album
        .album_artist
        .as_deref()
        .map(normalize_for_match)
        .is_some_and(|local| local == normalize_for_match(&detail.album.artist));
    if !artist_ok {
        return false;
    }
    let pairings = pair_qobuz_tracks(local_tracks, &detail.tracks);
    if pairings.len() != local_tracks.len() || !pairings.iter().all(|p| p.confidence >= 90) {
        return false;
    }

    pairings.iter().all(|pair| {
        let Some(local) = local_tracks
            .iter()
            .find(|track| track.id == pair.local_track_id)
        else {
            return false;
        };
        let Some(qobuz) = detail
            .tracks
            .iter()
            .find(|track| track.id.to_string() == pair.qobuz_track_id)
        else {
            return false;
        };
        normalize_for_match(&local.title) == normalize_for_match(&qobuz.title)
    })
}

fn qobuz_barcode_match_has_supporting_evidence(
    album: &AlbumSummary,
    local_tracks: &[TrackSummary],
    detail: &QobuzAlbumDetail,
) -> bool {
    let local_title = normalize_for_match(&album.title);
    let qobuz_title = normalize_for_match(&detail.album.title);
    let title_ok = !local_title.is_empty() && local_title == qobuz_title;
    let local_artist = album
        .album_artist
        .as_deref()
        .map(normalize_for_match)
        .unwrap_or_default();
    let qobuz_artist = normalize_for_match(&detail.album.artist);
    let artist_ok = !local_artist.is_empty()
        && !qobuz_artist.is_empty()
        && (local_artist == qobuz_artist
            || local_artist.contains(&qobuz_artist)
            || qobuz_artist.contains(&local_artist));
    if title_ok && artist_ok {
        return true;
    }
    if local_tracks.is_empty() || local_tracks.len() != detail.tracks.len() {
        return false;
    }
    let pairings = pair_qobuz_tracks(local_tracks, &detail.tracks);
    !pairings.is_empty()
        && pairings.len() == local_tracks.len()
        && pairings.iter().all(|pair| pair.confidence >= 90)
}

/// Barcodes come in UPC-12 and EAN-13 flavors that differ only by leading
/// zeros, and sources sometimes include spacing/dashes.
pub(super) fn normalize_barcode(input: &str) -> String {
    let digits: String = input.chars().filter(char::is_ascii_digit).collect();
    digits.trim_start_matches('0').to_string()
}

pub(super) fn score_qobuz_album_match(
    local_album: &AlbumSummary,
    local_tracks: &[TrackSummary],
    qobuz_album: &QobuzAlbum,
    qobuz_tracks: &[QobuzTrack],
) -> i64 {
    let mut score = 0;
    let local_title = normalize_for_match(&local_album.title);
    let qobuz_title = normalize_for_match(&qobuz_album.title);
    if local_title == qobuz_title {
        score += 35;
    } else if !local_title.is_empty() && !qobuz_title.is_empty() {
        let tolerance = (local_title.len().max(qobuz_title.len()) / 8).clamp(1, 5);
        if levenshtein(&local_title, &qobuz_title) <= tolerance {
            score += 25;
        }
    }

    let local_artist = local_album
        .album_artist
        .as_deref()
        .map(normalize_for_match)
        .unwrap_or_default();
    let qobuz_artist = normalize_for_match(&qobuz_album.artist);
    if !local_artist.is_empty() && local_artist == qobuz_artist {
        score += 25;
    } else if !local_artist.is_empty()
        && !qobuz_artist.is_empty()
        && (local_artist.contains(&qobuz_artist) || qobuz_artist.contains(&local_artist))
    {
        score += 15;
    }

    if let (Some(local_year), Some(qobuz_year)) = (local_album.year, qobuz_album.year) {
        let delta = (local_year - qobuz_year).abs();
        if delta == 0 {
            score += 10;
        } else if delta <= 1 {
            score += 6;
        }
    }

    let local_count = local_tracks.len() as i64;
    let qobuz_count = qobuz_tracks.len() as i64;
    if local_count > 0 && qobuz_count > 0 {
        let diff = (local_count - qobuz_count).abs();
        if diff == 0 {
            score += 15;
        } else if diff <= 2 {
            score += 8;
        }
    }

    let pairings = pair_qobuz_tracks(local_tracks, qobuz_tracks);
    if !qobuz_tracks.is_empty() {
        let coverage = pairings.len() as f64 / qobuz_tracks.len().max(local_tracks.len()) as f64;
        score += (coverage * 15.0).round() as i64;
    }

    score.min(100)
}

fn shuffle_sources(sources: &mut [ResolvedPlaySource]) {
    let mut seed = now_secs() as usize ^ sources.len();
    for i in (1..sources.len()).rev() {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        let j = seed % (i + 1);
        sources.swap(i, j);
    }
}
pub(crate) fn normalize_qobuz_album_id(id: &str) -> String {
    id.trim()
        .strip_prefix("qobuz:album:")
        .or_else(|| id.trim().strip_prefix("qobuz:cd:"))
        .or_else(|| id.trim().strip_prefix("qobuz:hires:"))
        .unwrap_or_else(|| id.trim())
        .to_string()
}
