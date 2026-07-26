use super::matching::pair_tracks;
use super::provider_versions::{
    AppleMusicVerifiedFormat, ExternalAlbumVersionInput, ExternalTrackPairing,
    ExternalVersionTrackInput, apple_music_album_verified_format,
    assign_recording_ids_for_external_version, external_album_version_by_provider_id,
    external_version_tracks, rebuild_external_track_links, upsert_external_album_version,
    upsert_external_version_tracks,
};
use super::{
    AlbumDetail, AlbumVersionSummary, AppleMusicAlbumMatchPreview, AppleMusicTrackPairPreview,
    AppleMusicVersionDetail, Library, MbTrack, ResolvedPlaySource,
};
use crate::services::apple_music_musickit::{AppleCatalogAlbum, AppleCatalogSong};
use rusqlite::{OptionalExtension, params};
use std::collections::{HashMap, HashSet};

const PROVIDER: &str = "apple_music";

impl Library {
    /// Remember what Music.app's decoder reported for one catalog song.
    pub fn record_apple_music_track_format(
        &self,
        song_id: &str,
        album_id: Option<&str>,
        storefront: Option<&str>,
        codec: &str,
        sample_rate: u32,
        bit_depth: Option<u32>,
    ) -> Result<(), String> {
        let song_id = song_id.trim();
        if song_id.is_empty() || sample_rate == 0 {
            return Ok(());
        }
        let conn = self.conn.lock().unwrap();
        conn.execute(
            r#"
            INSERT INTO apple_music_track_formats (
                song_id, album_id, storefront, codec, sample_rate, bit_depth, observed_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ON CONFLICT(song_id) DO UPDATE SET
                album_id = COALESCE(excluded.album_id, album_id),
                storefront = COALESCE(excluded.storefront, storefront),
                codec = excluded.codec,
                sample_rate = excluded.sample_rate,
                bit_depth = excluded.bit_depth,
                observed_at = excluded.observed_at
            "#,
            params![
                song_id,
                normalized_catalog_id(album_id),
                normalized_catalog_id(storefront),
                codec,
                i64::from(sample_rate),
                bit_depth.map(i64::from),
                super::now_secs(),
            ],
        )
        .map(|_| ())
        .map_err(|error| format!("record Apple Music verified format: {error}"))
    }

    /// The best format verified so far for a catalog album, mirroring how a
    /// local or Qobuz version is stamped with its highest-quality track.
    pub fn apple_music_album_verified_format(
        &self,
        album_id: &str,
    ) -> Result<Option<AppleMusicVerifiedFormat>, String> {
        let conn = self.conn.lock().unwrap();
        apple_music_album_verified_format(&conn, album_id)
    }

    /// The last decoder format verified for one catalog song.
    pub fn apple_music_track_verified_format(
        &self,
        song_id: &str,
    ) -> Result<Option<AppleMusicVerifiedFormat>, String> {
        let song_id = song_id.trim();
        if song_id.is_empty() {
            return Ok(None);
        }
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT codec, sample_rate, bit_depth
             FROM apple_music_track_formats
             WHERE song_id = ?1
             LIMIT 1",
            [song_id],
            |row| {
                Ok(AppleMusicVerifiedFormat {
                    codec: row.get(0)?,
                    sample_rate: row.get(1)?,
                    bit_depth: row.get(2)?,
                })
            },
        )
        .optional()
        .map_err(|error| format!("Apple Music verified track format: {error}"))
    }

    /// Every catalog song on the album whose format Fozmo has verified, keyed
    /// by song ID.
    pub fn apple_music_track_verified_formats(
        &self,
        album_id: &str,
    ) -> Result<HashMap<String, AppleMusicVerifiedFormat>, String> {
        let album_id = album_id.trim();
        if album_id.is_empty() {
            return Ok(HashMap::new());
        }
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT song_id, codec, sample_rate, bit_depth
                 FROM apple_music_track_formats
                 WHERE album_id = ?1",
            )
            .map_err(|error| format!("Apple Music verified track formats: {error}"))?;
        let rows = stmt
            .query_map([album_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    AppleMusicVerifiedFormat {
                        codec: row.get(1)?,
                        sample_rate: row.get(2)?,
                        bit_depth: row.get(3)?,
                    },
                ))
            })
            .map_err(|error| format!("Apple Music verified track formats map: {error}"))?;
        let mut out = HashMap::new();
        for row in rows {
            let (song_id, format) =
                row.map_err(|error| format!("Apple Music verified track format row: {error}"))?;
            out.insert(song_id, format);
        }
        Ok(out)
    }
}

fn normalized_catalog_id(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

impl Library {
    pub fn preview_apple_music_album_version(
        &self,
        album_id: i64,
        apple_album: AppleCatalogAlbum,
    ) -> Result<Option<AppleMusicAlbumMatchPreview>, String> {
        let Some(local_album) = self.album(album_id)? else {
            return Ok(None);
        };
        let local_tracks = self.primary_local_album_tracks(&local_album)?;
        let pairings = pair_apple_tracks(&local_tracks, &apple_album.tracks);
        let paired_local = pairings
            .iter()
            .map(|pairing| pairing.local_track_id)
            .collect::<HashSet<_>>();
        let paired_apple = pairings
            .iter()
            .map(|pairing| pairing.provider_track_id.as_str())
            .collect::<HashSet<_>>();
        let unmatched_local_track_ids = local_tracks
            .iter()
            .filter(|track| !paired_local.contains(&track.id))
            .map(|track| track.id)
            .collect::<Vec<_>>();
        let unmatched_apple_song_ids = apple_album
            .tracks
            .iter()
            .filter(|track| !paired_apple.contains(track.song_id.as_str()))
            .map(|track| track.song_id.clone())
            .collect::<Vec<_>>();
        let normalized_local_title = normalize_apple_match_text(&local_album.title);
        let normalized_apple_title = normalize_apple_match_text(&apple_album.title);
        let title_match = normalized_local_title == normalized_apple_title;
        let edition_title_match = !title_match
            && normalized_album_base_title(&local_album.title)
                == normalized_album_base_title(&apple_album.title);
        let artist_match = local_album.album_artist.as_deref().is_some_and(|artist| {
            normalize_apple_match_text(artist) == normalize_apple_match_text(&apple_album.artist)
        });
        let track_count_match =
            !local_tracks.is_empty() && local_tracks.len() == apple_album.tracks.len();
        let barcode_match = match (
            local_album.mb_barcode.as_deref(),
            apple_album.upc.as_deref(),
        ) {
            (Some(local), Some(apple)) => {
                Some(normalize_barcode(local) == normalize_barcode(apple))
            }
            _ => None,
        };
        let all_tracks_paired = unmatched_local_track_ids.is_empty()
            && unmatched_apple_song_ids.is_empty()
            && !pairings.is_empty();
        let all_provider_tracks_paired = unmatched_apple_song_ids.is_empty()
            && pairings.len() == apple_album.tracks.len()
            && !pairings.is_empty();
        let provider_is_complete_local_subset = !track_count_match
            && all_provider_tracks_paired
            && unmatched_local_track_ids.len() <= 2
            && pairings.len() * 5 >= local_tracks.len() * 4;
        let compatible_track_set = track_count_match || provider_is_complete_local_subset;
        let complete_track_evidence = all_provider_tracks_paired
            && pairings.iter().all(|pairing| pairing.confidence >= 95)
            && pairings
                .iter()
                .filter(|pairing| pairing.confidence == 100)
                .count()
                * 10
                >= pairings.len() * 9;
        let complete_release_evidence = complete_track_evidence
            && compatible_track_set
            && (title_match || edition_title_match)
            && artist_match;
        let mut confidence = 0;
        let mut evidence = Vec::new();
        if title_match {
            confidence += 25;
            evidence.push("exact_normalized_title".to_string());
        } else if edition_title_match {
            confidence += 25;
            evidence.push("edition_compatible_title".to_string());
        }
        if artist_match {
            confidence += 20;
            evidence.push("exact_normalized_artist".to_string());
        }
        if track_count_match {
            confidence += 20;
            evidence.push("equal_track_count".to_string());
        } else if provider_is_complete_local_subset {
            confidence += 20;
            evidence.push("complete_provider_edition_with_local_bonus_tracks".to_string());
        }
        match barcode_match {
            Some(true) => {
                confidence += 25;
                evidence.push("upc_match".to_string());
            }
            Some(false) if complete_release_evidence => {
                evidence.push("upc_conflict_overridden_by_complete_track_evidence".to_string());
            }
            Some(false) => {
                confidence -= 40;
                evidence.push("upc_conflict".to_string());
            }
            None => {}
        }
        if all_tracks_paired {
            confidence += 10;
            evidence.push("all_tracks_paired".to_string());
        } else if provider_is_complete_local_subset {
            confidence += 10;
            evidence.push("all_provider_tracks_paired".to_string());
        }
        if complete_release_evidence {
            confidence += 25;
            evidence.push("complete_track_evidence".to_string());
        }
        confidence = confidence.clamp(0, 100);
        let safe_to_link = (barcode_match != Some(false) || complete_release_evidence)
            && compatible_track_set
            && all_provider_tracks_paired
            && complete_track_evidence
            && (barcode_match == Some(true)
                || ((title_match || edition_title_match) && artist_match));
        let pairings = pairings
            .into_iter()
            .filter_map(|pairing| {
                let local = local_tracks
                    .iter()
                    .find(|track| track.id == pairing.local_track_id)?;
                let apple = apple_album
                    .tracks
                    .iter()
                    .find(|track| track.song_id == pairing.provider_track_id)?;
                Some(AppleMusicTrackPairPreview {
                    local_track_id: local.id,
                    local_title: local.title.clone(),
                    apple_song_id: apple.song_id.clone(),
                    apple_title: apple.title.clone(),
                    confidence: pairing.confidence,
                    evidence: vec![pairing.match_kind],
                })
            })
            .collect();
        let resulting_version_id = {
            let conn = self.conn.lock().unwrap();
            external_album_version_by_provider_id(&conn, album_id, PROVIDER, &apple_album.album_id)?
                .map(|row| row.id)
        };
        let resulting_version = resulting_version_id.and_then(|version_id| {
            self.album_versions(album_id)
                .ok()?
                .into_iter()
                .find(|version| version.id == version_id)
        });
        Ok(Some(AppleMusicAlbumMatchPreview {
            local_album_id: album_id,
            apple_album,
            confidence,
            evidence,
            pairings,
            unmatched_local_track_ids,
            unmatched_apple_song_ids,
            safe_to_link,
            resulting_version,
        }))
    }

    pub fn link_apple_music_album(
        &self,
        album_id: i64,
        apple_album: &AppleCatalogAlbum,
    ) -> Result<Option<AlbumVersionSummary>, String> {
        let Some(local_album) = self.album(album_id)? else {
            return Ok(None);
        };
        let local_tracks = self.primary_local_album_tracks(&local_album)?;
        let pairings = pair_apple_tracks(&local_tracks, &apple_album.tracks);
        let payload_json = serde_json::to_string(apple_album)
            .map_err(|error| format!("serialize Apple Music album: {error}"))?;
        let year = apple_album
            .release_date
            .as_deref()
            .and_then(|date| date.get(..4))
            .and_then(|year| year.parse::<i32>().ok());
        let tracks = apple_album
            .tracks
            .iter()
            .enumerate()
            .map(|(index, track)| apple_version_track(track, index))
            .collect::<Vec<_>>();
        let version_id = {
            let mut conn = self.conn.lock().unwrap();
            let tx = conn
                .transaction()
                .map_err(|error| format!("begin Apple Music version transaction: {error}"))?;
            Self::sync_local_versions_for_album_with_conn(&tx, album_id)?;
            let apple_was_primary: bool = tx
                .query_row(
                    r#"
                    SELECT EXISTS(
                        SELECT 1
                        FROM albums a
                        JOIN album_versions v ON v.id = a.primary_version_id
                        WHERE a.id = ?1 AND v.provider = ?2
                    )
                    "#,
                    params![album_id, PROVIDER],
                    |row| row.get::<_, i64>(0),
                )
                .map(|value| value != 0)
                .map_err(|error| format!("load grouped Apple Music version: {error}"))?;
            tx.execute(
                r#"
                DELETE FROM album_versions
                WHERE album_id = ?1 AND provider = ?2 AND provider_id <> ?3
                "#,
                params![album_id, PROVIDER, apple_album.album_id],
            )
            .map_err(|error| format!("replace grouped Apple Music version: {error}"))?;
            let version_id = upsert_external_album_version(
                &tx,
                &ExternalAlbumVersionInput {
                    album_id,
                    provider: PROVIDER,
                    provider_id: &apple_album.album_id,
                    title: &apple_album.title,
                    artist: Some(&apple_album.artist),
                    year,
                    track_count: apple_album.tracks.len() as i64,
                    art_id: None,
                    format: "Apple Music",
                    sample_rate: None,
                    bit_depth: None,
                    source_label: "Apple Music",
                    payload_json: &payload_json,
                },
            )?;
            upsert_external_version_tracks(&tx, version_id, &tracks)?;
            rebuild_external_track_links(&tx, album_id, PROVIDER, &pairings)?;
            assign_recording_ids_for_external_version(&tx, album_id)?;
            if apple_was_primary {
                tx.execute(
                    "UPDATE albums SET primary_version_id = ?2, updated_at = ?3 WHERE id = ?1",
                    params![album_id, version_id, super::now_secs()],
                )
                .map_err(|error| format!("preserve Apple Music primary version: {error}"))?;
            }
            tx.commit()
                .map_err(|error| format!("commit Apple Music version: {error}"))?;
            version_id
        };
        Ok(self
            .album_versions(album_id)?
            .into_iter()
            .find(|version| version.id == version_id))
    }

    pub fn unlink_apple_music_album(
        &self,
        album_id: i64,
    ) -> Result<Option<Vec<AlbumVersionSummary>>, String> {
        if self.album(album_id)?.is_none() {
            return Ok(None);
        }
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction()
            .map_err(|error| format!("begin Apple Music unlink transaction: {error}"))?;
        let removed_primary: bool = tx
            .query_row(
                r#"
                SELECT EXISTS(
                    SELECT 1
                    FROM albums a
                    JOIN album_versions v ON v.id = a.primary_version_id
                    WHERE a.id = ?1 AND v.provider = ?2
                )
                "#,
                params![album_id, PROVIDER],
                |row| row.get::<_, i64>(0),
            )
            .map(|value| value != 0)
            .map_err(|error| format!("check Apple Music primary version: {error}"))?;
        tx.execute(
            r#"
            DELETE FROM version_track_links
            WHERE album_id = ?1
              AND provider_version_track_id IN (
                  SELECT vt.id
                  FROM version_tracks vt
                  JOIN album_versions v ON v.id = vt.version_id
                  WHERE v.album_id = ?1 AND v.provider = ?2
              )
            "#,
            params![album_id, PROVIDER],
        )
        .map_err(|error| format!("delete Apple Music track links: {error}"))?;
        tx.execute(
            "DELETE FROM album_versions WHERE album_id = ?1 AND provider = ?2",
            params![album_id, PROVIDER],
        )
        .map_err(|error| format!("delete Apple Music versions: {error}"))?;
        if removed_primary {
            tx.execute(
                r#"
                UPDATE albums
                SET primary_version_id = (
                    SELECT id
                    FROM album_versions
                    WHERE album_id = ?1 AND status = 'available'
                    ORDER BY
                        CASE
                          WHEN provider = 'local'
                           AND (COALESCE(bit_depth, 0) >= 24 OR COALESCE(sample_rate, 0) > 48000)
                          THEN 0
                          WHEN provider = 'qobuz'
                           AND (COALESCE(bit_depth, 0) >= 24 OR COALESCE(sample_rate, 0) > 48000)
                          THEN 1
                          WHEN provider = 'local' THEN 2
                          WHEN provider = 'qobuz' THEN 3
                          ELSE 4
                        END,
                        COALESCE(sample_rate, 0) DESC,
                        id
                    LIMIT 1
                ),
                updated_at = ?2
                WHERE id = ?1
                "#,
                params![album_id, super::now_secs()],
            )
            .map_err(|error| format!("choose primary after Apple Music unlink: {error}"))?;
        }
        tx.commit()
            .map_err(|error| format!("commit Apple Music unlink: {error}"))?;
        drop(conn);
        Ok(Some(self.album_versions(album_id)?))
    }

    pub fn album_by_apple_music_id(
        &self,
        apple_album_id: &str,
    ) -> Result<Option<AlbumDetail>, String> {
        let normalized = apple_album_id.trim();
        if normalized.is_empty() {
            return Ok(None);
        }
        let album_id: Option<i64> = {
            let conn = self.conn.lock().unwrap();
            conn.query_row(
                r#"
                SELECT album_id
                FROM album_versions
                WHERE provider = ?1 AND provider_id = ?2
                ORDER BY updated_at DESC, id DESC
                LIMIT 1
                "#,
                params![PROVIDER, normalized],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| format!("Apple Music album lookup: {error}"))?
        };
        album_id
            .map(|album_id| self.album_detail(album_id))
            .transpose()
            .map(Option::flatten)
    }

    pub fn apple_music_version_detail(
        &self,
        album_id: i64,
        version_id: i64,
    ) -> Result<Option<AppleMusicVersionDetail>, String> {
        let payload: Option<String> = {
            let conn = self.conn.lock().unwrap();
            conn.query_row(
                r#"
                SELECT payload_json
                FROM album_versions
                WHERE id = ?1 AND album_id = ?2 AND provider = ?3
                  AND status = 'available'
                "#,
                params![version_id, album_id, PROVIDER],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| format!("Apple Music version detail lookup: {error}"))?
            .flatten()
        };
        let Some(payload) = payload else {
            return Ok(None);
        };
        let apple_album = serde_json::from_str(&payload)
            .map_err(|error| format!("parse Apple Music version detail: {error}"))?;
        let Some(version) = self
            .album_versions(album_id)?
            .into_iter()
            .find(|version| version.id == version_id && version.provider == PROVIDER)
        else {
            return Ok(None);
        };
        Ok(Some(AppleMusicVersionDetail {
            version,
            apple_album,
        }))
    }

    pub(super) fn apple_music_sources_for_version(
        &self,
        album_id: i64,
        version_id: i64,
    ) -> Result<Vec<ResolvedPlaySource>, String> {
        let payload: Option<String> = {
            let conn = self.conn.lock().unwrap();
            conn.query_row(
                r#"
                SELECT payload_json
                FROM album_versions
                WHERE id = ?1 AND album_id = ?2 AND provider = ?3
                  AND status = 'available'
                "#,
                params![version_id, album_id, PROVIDER],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| format!("Apple Music version payload lookup: {error}"))?
            .flatten()
        };
        let Some(payload) = payload else {
            return Ok(Vec::new());
        };
        let album: AppleCatalogAlbum = serde_json::from_str(&payload)
            .map_err(|error| format!("parse Apple Music version payload: {error}"))?;
        let conn = self.conn.lock().unwrap();
        let ordered = external_version_tracks(&conn, album_id, PROVIDER)?;
        let by_id = album
            .tracks
            .iter()
            .map(|track| (track.song_id.as_str(), track))
            .collect::<std::collections::HashMap<_, _>>();
        Ok(ordered
            .into_iter()
            .filter_map(|row| by_id.get(row.provider_track_id.as_str()).copied())
            .map(|track| apple_play_source(&album, track))
            .collect())
    }
}

fn apple_version_track(track: &AppleCatalogSong, index: usize) -> ExternalVersionTrackInput {
    ExternalVersionTrackInput {
        provider_track_id: track.song_id.clone(),
        title: track.title.clone(),
        artist: Some(track.artist.clone()),
        track_number: Some(track.track_number.unwrap_or((index + 1) as u32) as i64),
        disc_number: Some(track.disc_number.unwrap_or(1) as i64),
        duration_secs: track.duration_secs.filter(|value| *value > 0.0),
        sample_rate: None,
        format: Some("Apple Music".to_string()),
        bit_depth: None,
    }
}

fn pair_apple_tracks(
    local_tracks: &[super::TrackSummary],
    apple_tracks: &[AppleCatalogSong],
) -> Vec<ExternalTrackPairing> {
    let remote_tracks = apple_tracks
        .iter()
        .enumerate()
        .map(|(index, track)| MbTrack {
            recording_id: track.isrc.clone().or_else(|| Some(track.song_id.clone())),
            disc: track.disc_number.unwrap_or(1) as i64,
            position: track.track_number.unwrap_or((index + 1) as u32) as i64,
            title: track.title.clone(),
            artist: Some(track.artist.clone()),
            length_secs: track.duration_secs,
        })
        .collect::<Vec<_>>();
    pair_tracks(local_tracks, &remote_tracks)
        .into_iter()
        .filter_map(|pairing| {
            let local = local_tracks.get(pairing.file_index)?;
            let apple = apple_tracks.get(pairing.mb_index)?;
            let exact_title = normalized_apple_track_title(&local.title)
                == normalized_apple_track_title(&apple.title);
            let duration_match = match (local.duration_secs, apple.duration_secs) {
                (Some(local), Some(apple)) => (local - apple).abs() <= 3.0,
                _ => false,
            };
            let confidence = match (exact_title, duration_match, pairing.kind) {
                (true, true, _) => 100,
                (true, false, _) => 95,
                (false, true, "exact") => 90,
                _ => 84,
            };
            let match_kind = if exact_title && duration_match {
                "position_title_duration"
            } else if exact_title {
                "position_title"
            } else {
                pairing.kind
            };
            Some(ExternalTrackPairing {
                local_track_id: local.id,
                provider_track_id: apple.song_id.clone(),
                confidence,
                match_kind: match_kind.to_string(),
            })
        })
        .collect()
}

fn apple_play_source(album: &AppleCatalogAlbum, track: &AppleCatalogSong) -> ResolvedPlaySource {
    ResolvedPlaySource::AppleMusic {
        song_id: track.song_id.clone(),
        storefront: track.storefront.clone(),
        title: track.title.clone(),
        artist: Some(track.artist.clone()),
        album: track
            .album_title
            .clone()
            .or_else(|| Some(album.title.clone())),
        album_artist: track
            .album_artist
            .clone()
            .or_else(|| Some(album.artist.clone())),
        album_id: track
            .album_id
            .clone()
            .or_else(|| Some(album.album_id.clone())),
        image_url: track
            .artwork_url
            .clone()
            .or_else(|| album.artwork_url.clone()),
        duration_secs: track.duration_secs,
        track_number: track.track_number,
        disc_number: track.disc_number,
        isrc: track.isrc.clone(),
    }
}

fn normalize_barcode(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_digit())
        .collect()
}

fn normalized_album_base_title(value: &str) -> String {
    let normalized = normalize_apple_match_text(value);
    for suffix in [
        " bonus track edition",
        " bonus tracks edition",
        " collector s edition",
        " collectors edition",
        " deluxe edition",
        " expanded edition",
        " special edition",
        " remaster",
        " remastered",
    ] {
        if let Some(base) = normalized.strip_suffix(suffix).map(str::trim)
            && !base.is_empty()
        {
            return base.to_string();
        }
    }
    let tokens = normalized.split_whitespace().collect::<Vec<_>>();
    if tokens.ends_with(&["anniversary", "edition"])
        && let Some(anniversary_index) = tokens.iter().rposition(|token| *token == "anniversary")
    {
        let mut base_end = anniversary_index;
        if base_end > 0
            && tokens[base_end - 1]
                .chars()
                .any(|character| character.is_ascii_digit())
        {
            base_end -= 1;
        }
        let base = tokens[..base_end].join(" ");
        if !base.is_empty() {
            return base;
        }
    }
    normalized
}

fn normalized_apple_track_title(value: &str) -> String {
    let trimmed = value.trim();
    for (opening, closing) in [('(', ')'), ('[', ']')] {
        if trimmed.ends_with(closing)
            && let Some(opening_index) = trimmed.rfind(opening)
        {
            let qualifier = &trimmed[opening_index + opening.len_utf8()..trimmed.len() - 1];
            let normalized_qualifier = normalize_apple_match_text(qualifier);
            if normalized_qualifier.starts_with("live")
                || normalized_qualifier.starts_with("recorded live")
            {
                let base = normalize_apple_match_text(trimmed[..opening_index].trim());
                if !base.is_empty() {
                    return base;
                }
            }
        }
    }
    let normalized = normalize_apple_match_text(trimmed);
    normalized
        .strip_suffix(" live")
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .unwrap_or(&normalized)
        .to_string()
}

fn normalize_apple_match_text(value: &str) -> String {
    let chars = value.chars().collect::<Vec<_>>();
    let mut folded = String::with_capacity(value.len());
    for (index, character) in chars.iter().copied().enumerate() {
        if index > 0
            && character.is_uppercase()
            && chars[index - 1].is_lowercase()
            && chars.get(index + 1).is_some_and(|next| next.is_lowercase())
        {
            folded.push(' ');
        }
        for lowered in character.to_lowercase() {
            match lowered {
                '&' => folded.push_str(" and "),
                '\'' | '’' | '‘' | 'ʼ' => {}
                'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => folded.push('a'),
                'æ' => folded.push_str("ae"),
                'ç' | 'ć' | 'č' => folded.push('c'),
                'ď' | 'ð' => folded.push('d'),
                'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => folded.push('e'),
                'ì' | 'í' | 'î' | 'ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' => folded.push('i'),
                'ĺ' | 'ļ' | 'ľ' | 'ł' => folded.push('l'),
                'ñ' | 'ń' | 'ņ' | 'ň' => folded.push('n'),
                'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ŏ' | 'ő' => folded.push('o'),
                'œ' => folded.push_str("oe"),
                'ŕ' | 'ŗ' | 'ř' => folded.push('r'),
                'ś' | 'ş' | 'š' => folded.push('s'),
                'ß' => folded.push_str("ss"),
                'ť' => folded.push('t'),
                'þ' => folded.push_str("th"),
                'ù' | 'ú' | 'û' | 'ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => {
                    folded.push('u')
                }
                'ý' | 'ÿ' => folded.push('y'),
                'ź' | 'ż' | 'ž' => folded.push('z'),
                value if value.is_alphanumeric() => folded.push(value),
                _ => folded.push(' '),
            }
        }
    }
    folded.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::{
        normalize_apple_match_text, normalized_album_base_title, normalized_apple_track_title,
    };

    #[test]
    fn album_title_matching_ignores_trailing_edition_qualifiers() {
        assert_eq!(
            normalized_album_base_title("Dots and Loops (Expanded Edition)"),
            "dots and loops"
        );
        assert_eq!(
            normalized_album_base_title("OK Computer – 20th Anniversary Edition"),
            "ok computer"
        );
    }

    #[test]
    fn album_title_matching_preserves_meaningful_title_words() {
        assert_eq!(normalized_album_base_title("The Deluxe"), "the deluxe");
        assert_eq!(
            normalized_album_base_title("Expanded Universe"),
            "expanded universe"
        );
    }

    #[test]
    fn apple_track_title_matching_ignores_live_qualifiers() {
        assert_eq!(
            normalized_apple_track_title("2 + 2 = 5 (Live)"),
            normalized_apple_track_title("2 + 2 = 5")
        );
        assert_eq!(
            normalized_apple_track_title("I Will (Live at Le Réservoir, Paris)"),
            normalized_apple_track_title("I Will")
        );
        assert_eq!(normalized_apple_track_title("Live Forever"), "live forever");
        assert_eq!(
            normalized_apple_track_title(
                "There’s More to Life Than This (recorded live at the Milk Bar toilets)"
            ),
            normalized_apple_track_title(
                "There's More to Life Than This (Live at the Milk Bar Toilets)"
            )
        );
    }

    #[test]
    fn apple_matching_folds_latin_diacritics_and_apostrophes() {
        assert_eq!(normalize_apple_match_text("Vökuró"), "vokuro");
        assert_eq!(normalize_apple_match_text("Miðvikudags"), "midvikudags");
        assert_eq!(
            normalize_apple_match_text("Mouth’s Cradle"),
            normalize_apple_match_text("Mouths Cradle")
        );
    }
}
