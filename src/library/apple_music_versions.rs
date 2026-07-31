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
use crate::services::qobuz::{QobuzAlbumDetail, QobuzTrack};
use rusqlite::{OptionalExtension, params};
use std::collections::{HashMap, HashSet};

const PROVIDER: &str = "apple_music";
const QOBUZ_APPLE_MUSIC_MATCH_SCHEMA: &str = "qobuz-apple-match-v2-isrc";

/// A verified Apple Music decoder format with the context it was observed in.
///
/// Safe-island construction treats these as predictions rather than
/// guarantees — a live mismatch still enters a protected restart — but a
/// prediction is only usable while its storefront and observation time still
/// apply.
// Consumed by safe-island construction once the transition coordinator drives
// Apple preparation; the query and its row type land with the island builder.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq)]
pub struct AppleMusicTrackFormatRecord {
    pub song_id: String,
    pub storefront: Option<String>,
    pub codec: String,
    pub sample_rate: i64,
    pub bit_depth: Option<i64>,
    /// Unix seconds.
    pub observed_at: i64,
    pub format_context_fingerprint: Option<String>,
}

impl Library {
    /// A catalog album Fozmo already persisted while linking either a local or
    /// standalone Qobuz release. This is a durable catalog cache: opening the
    /// Apple route should not launch the helper just to reload an identical
    /// payload that playback already trusts as the linked version.
    pub fn cached_apple_music_album(
        &self,
        apple_album_id: &str,
    ) -> Result<Option<AppleCatalogAlbum>, String> {
        let apple_album_id = apple_album_id.trim();
        if apple_album_id.is_empty() {
            return Ok(None);
        }
        let payload: Option<String> = {
            let conn = self.conn.lock().unwrap();
            conn.query_row(
                r#"
                SELECT payload_json
                FROM (
                    SELECT payload_json, updated_at AS cached_at
                    FROM album_versions
                    WHERE provider = ?1 AND provider_id = ?2
                      AND status = 'available' AND payload_json IS NOT NULL
                    UNION ALL
                    SELECT payload_json, matched_at AS cached_at
                    FROM qobuz_apple_music_links
                    WHERE apple_album_id = ?2 AND payload_json IS NOT NULL
                )
                ORDER BY cached_at DESC
                LIMIT 1
                "#,
                params![PROVIDER, apple_album_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| format!("cached Apple Music album lookup: {error}"))?
        };
        payload
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .map_err(|error| format!("parse cached Apple Music album: {error}"))
    }

    /// Reuse a conclusive local miss only while every matcher input and the
    /// storefront are unchanged. The caller owns the fingerprint so matcher
    /// rule changes can invalidate old answers by bumping its schema marker.
    pub fn cached_apple_music_match_status(
        &self,
        album_id: i64,
        storefront: Option<&str>,
        input_fingerprint: &str,
        retry_after_secs: i64,
    ) -> Result<Option<String>, String> {
        let storefront = normalized_match_storefront(storefront);
        let input_fingerprint = input_fingerprint.trim();
        if input_fingerprint.is_empty() || retry_after_secs <= 0 {
            return Ok(None);
        }
        let oldest = super::now_secs().saturating_sub(retry_after_secs);
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            r#"
            SELECT status
            FROM apple_music_album_match_attempts
            WHERE album_id = ?1 AND storefront = ?2
              AND input_fingerprint = ?3 AND matched_at >= ?4
            LIMIT 1
            "#,
            params![album_id, storefront, input_fingerprint, oldest],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| format!("cached Apple Music album match: {error}"))
    }

    pub fn save_apple_music_match_status(
        &self,
        album_id: i64,
        storefront: Option<&str>,
        input_fingerprint: &str,
        status: &str,
    ) -> Result<(), String> {
        if !matches!(status, "no_match" | "needs_review") {
            return Err("only conclusive Apple Music misses may be cached".to_string());
        }
        let storefront = normalized_match_storefront(storefront);
        let input_fingerprint = input_fingerprint.trim();
        if input_fingerprint.is_empty() {
            return Ok(());
        }
        let conn = self.conn.lock().unwrap();
        conn.execute(
            r#"
            INSERT INTO apple_music_album_match_attempts (
                album_id, storefront, input_fingerprint, status, matched_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5)
            ON CONFLICT(album_id, storefront) DO UPDATE SET
                input_fingerprint = excluded.input_fingerprint,
                status = excluded.status,
                matched_at = excluded.matched_at
            "#,
            params![
                album_id,
                storefront,
                input_fingerprint,
                status,
                super::now_secs()
            ],
        )
        .map(|_| ())
        .map_err(|error| format!("save Apple Music album match status: {error}"))
    }

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
        self.record_apple_music_track_format_in_context(
            song_id,
            album_id,
            storefront,
            codec,
            sample_rate,
            bit_depth,
            None,
        )
    }

    pub fn record_apple_music_track_format_in_context(
        &self,
        song_id: &str,
        album_id: Option<&str>,
        storefront: Option<&str>,
        codec: &str,
        sample_rate: u32,
        bit_depth: Option<u32>,
        format_context_fingerprint: Option<&str>,
    ) -> Result<(), String> {
        let song_id = song_id.trim();
        if song_id.is_empty() || sample_rate == 0 {
            return Ok(());
        }
        let conn = self.conn.lock().unwrap();
        conn.execute(
            r#"
            INSERT INTO apple_music_track_formats (
                song_id, album_id, storefront, codec, sample_rate, bit_depth, observed_at,
                format_context_fingerprint
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            ON CONFLICT(song_id) DO UPDATE SET
                album_id = COALESCE(excluded.album_id, album_id),
                storefront = COALESCE(excluded.storefront, storefront),
                codec = excluded.codec,
                sample_rate = excluded.sample_rate,
                bit_depth = excluded.bit_depth,
                observed_at = excluded.observed_at,
                format_context_fingerprint = excluded.format_context_fingerprint
            "#,
            params![
                song_id,
                normalized_catalog_id(album_id),
                normalized_catalog_id(storefront),
                codec,
                i64::from(sample_rate),
                bit_depth.map(i64::from),
                super::now_secs(),
                normalized_catalog_id(format_context_fingerprint),
            ],
        )
        .map(|_| ())
        .map_err(|error| format!("record Apple Music verified format: {error}"))
    }

    /// A recent decoder observation usable for the exact current runtime and
    /// storefront. Rows written before v5 have no context and intentionally do
    /// not qualify.
    pub fn apple_music_track_verified_format_in_context(
        &self,
        song_id: &str,
        storefront: Option<&str>,
        format_context_fingerprint: &str,
    ) -> Result<Option<AppleMusicVerifiedFormat>, String> {
        let song_id = song_id.trim();
        let context = format_context_fingerprint.trim();
        if song_id.is_empty() || context.is_empty() {
            return Ok(None);
        }
        let storefront = normalized_catalog_id(storefront);
        let oldest = super::now_secs().saturating_sub(7 * 24 * 60 * 60);
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT codec, sample_rate, bit_depth
             FROM apple_music_track_formats
             WHERE song_id = ?1
               AND format_context_fingerprint = ?2
               AND observed_at >= ?3
               AND (?4 IS NULL OR storefront = ?4)
             LIMIT 1",
            params![song_id, context, oldest, storefront],
            |row| {
                Ok(AppleMusicVerifiedFormat {
                    codec: row.get(0)?,
                    sample_rate: row.get(1)?,
                    bit_depth: row.get(2)?,
                })
            },
        )
        .optional()
        .map_err(|error| format!("contextual Apple Music verified track format: {error}"))
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

    /// Full verified-format rows for `song_ids`, keyed by song ID.
    ///
    /// Unlike [`Library::apple_music_track_verified_format`] this exposes the
    /// storefront and observation time, which safe-island construction needs:
    /// a rate verified in one storefront does not predict another's mastering,
    /// and a rate observed long ago, or before the current macOS/Music.app
    /// context, is no longer a usable prediction. The columns already exist, so
    /// there is no migration here.
    #[allow(dead_code)]
    pub fn apple_music_track_format_records(
        &self,
        song_ids: &[String],
    ) -> Result<HashMap<String, AppleMusicTrackFormatRecord>, String> {
        let mut out = HashMap::new();
        if song_ids.is_empty() {
            return Ok(out);
        }
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT song_id, storefront, codec, sample_rate, bit_depth, observed_at,
                        format_context_fingerprint
                 FROM apple_music_track_formats
                 WHERE song_id = ?1",
            )
            .map_err(|error| format!("Apple Music verified format records: {error}"))?;
        for song_id in song_ids {
            let song_id = song_id.trim();
            if song_id.is_empty() {
                continue;
            }
            let record = stmt
                .query_row([song_id], |row| {
                    Ok(AppleMusicTrackFormatRecord {
                        song_id: row.get(0)?,
                        storefront: row.get(1)?,
                        codec: row.get(2)?,
                        sample_rate: row.get(3)?,
                        bit_depth: row.get(4)?,
                        observed_at: row.get(5)?,
                        format_context_fingerprint: row.get(6)?,
                    })
                })
                .optional()
                .map_err(|error| format!("Apple Music verified format record: {error}"))?;
            if let Some(record) = record {
                out.insert(record.song_id.clone(), record);
            }
        }
        Ok(out)
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

fn normalized_match_storefront(storefront: Option<&str>) -> String {
    storefront
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("current")
        .to_ascii_lowercase()
}

/// The album an Apple Music candidate is judged against.
///
/// A local album supplies its own scanned tracks; a Qobuz album that no local
/// album covers supplies catalog tracks shaped the same way. Both go through
/// one rule set so an Apple edition is accepted on the same evidence wherever
/// it is offered.
pub(crate) struct AppleMatchReference<'a> {
    pub title: &'a str,
    pub artist: Option<&'a str>,
    pub barcode: Option<&'a str>,
    pub tracks: &'a [super::TrackSummary],
    /// Recording identities supplied by a catalog provider, keyed by the
    /// corresponding reference track ID. Local files carry MusicBrainz
    /// recording IDs rather than ISRCs, so this is currently populated only
    /// for Qobuz-to-Apple matching.
    pub recording_ids: Option<&'a HashMap<i64, String>>,
}

pub(crate) struct AppleMatchAssessment {
    pub confidence: i64,
    pub evidence: Vec<String>,
    pub safe_to_link: bool,
    pub pairings: Vec<ExternalTrackPairing>,
    pub unmatched_reference_track_ids: Vec<i64>,
    pub unmatched_apple_song_ids: Vec<String>,
}

pub(crate) fn assess_apple_album_match(
    reference: &AppleMatchReference<'_>,
    apple_album: &AppleCatalogAlbum,
) -> AppleMatchAssessment {
    let reference_tracks = reference.tracks;
    let pairings = pair_apple_tracks(
        reference_tracks,
        &apple_album.tracks,
        reference.recording_ids,
    );
    let paired_reference = pairings
        .iter()
        .map(|pairing| pairing.local_track_id)
        .collect::<HashSet<_>>();
    let paired_apple = pairings
        .iter()
        .map(|pairing| pairing.provider_track_id.as_str())
        .collect::<HashSet<_>>();
    let unmatched_reference_track_ids = reference_tracks
        .iter()
        .filter(|track| !paired_reference.contains(&track.id))
        .map(|track| track.id)
        .collect::<Vec<_>>();
    let unmatched_apple_song_ids = apple_album
        .tracks
        .iter()
        .filter(|track| !paired_apple.contains(track.song_id.as_str()))
        .map(|track| track.song_id.clone())
        .collect::<Vec<_>>();
    let normalized_reference_title = normalize_apple_match_text(reference.title);
    let normalized_apple_title = normalize_apple_match_text(&apple_album.title);
    let title_match = normalized_reference_title == normalized_apple_title;
    let edition_title_match = !title_match
        && normalized_album_base_title(reference.title)
            == normalized_album_base_title(&apple_album.title);
    let artist_match = reference.artist.is_some_and(|artist| {
        normalize_apple_match_text(artist) == normalize_apple_match_text(&apple_album.artist)
    });
    let track_count_match =
        !reference_tracks.is_empty() && reference_tracks.len() == apple_album.tracks.len();
    let barcode_match = match (reference.barcode, apple_album.upc.as_deref()) {
        (Some(reference), Some(apple)) => {
            Some(normalize_barcode(reference) == normalize_barcode(apple))
        }
        _ => None,
    };
    let all_tracks_paired = unmatched_reference_track_ids.is_empty()
        && unmatched_apple_song_ids.is_empty()
        && !pairings.is_empty();
    let all_provider_tracks_paired = unmatched_apple_song_ids.is_empty()
        && pairings.len() == apple_album.tracks.len()
        && !pairings.is_empty();
    let provider_is_complete_local_subset = !track_count_match
        && all_provider_tracks_paired
        && unmatched_reference_track_ids.len() <= 2
        && pairings.len() * 5 >= reference_tracks.len() * 4;
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
    let isrc_pair_count = pairings
        .iter()
        .filter(|pairing| pairing.match_kind == "isrc")
        .count();
    if isrc_pair_count > 0 {
        evidence.push("isrc_track_identity".to_string());
    }
    if isrc_pair_count == apple_album.tracks.len() && !apple_album.tracks.is_empty() {
        evidence.push("all_tracks_isrc_matched".to_string());
    }
    let safe_to_link = (barcode_match != Some(false) || complete_release_evidence)
        && compatible_track_set
        && all_provider_tracks_paired
        && complete_track_evidence
        && (barcode_match == Some(true) || ((title_match || edition_title_match) && artist_match));
    AppleMatchAssessment {
        confidence: confidence.clamp(0, 100),
        evidence,
        safe_to_link,
        pairings,
        unmatched_reference_track_ids,
        unmatched_apple_song_ids,
    }
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
        let assessment = assess_apple_album_match(
            &AppleMatchReference {
                title: &local_album.title,
                artist: local_album.album_artist.as_deref(),
                barcode: local_album.mb_barcode.as_deref(),
                tracks: &local_tracks,
                recording_ids: None,
            },
            &apple_album,
        );
        let AppleMatchAssessment {
            confidence,
            evidence,
            safe_to_link,
            pairings,
            unmatched_reference_track_ids: unmatched_local_track_ids,
            unmatched_apple_song_ids,
        } = assessment;
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
        let pairings = pair_apple_tracks(&local_tracks, &apple_album.tracks, None);
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
            tx.execute(
                "DELETE FROM apple_music_album_match_attempts WHERE album_id = ?1",
                [album_id],
            )
            .map_err(|error| format!("clear cached Apple Music album match: {error}"))?;
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

/// How well one Apple Music catalog album matches a Qobuz album.
///
/// Qobuz albums that a local album covers inherit that album's Apple link, so
/// this is only ever asked about the standalone case, where there is no
/// `albums` row to pair tracks against and the Qobuz catalog listing stands in
/// for the local one.
#[derive(Debug, Clone)]
pub struct QobuzAppleMusicMatch {
    pub confidence: i64,
    pub evidence: Vec<String>,
    pub safe_to_link: bool,
    pub paired_track_count: usize,
}

/// A resolved Apple Music edition for a standalone Qobuz album, or the record
/// that Apple had nothing worth showing. `apple_album` is the frozen catalog
/// copy the version row is rendered from.
#[derive(Debug, Clone)]
pub struct QobuzAppleMusicLink {
    pub apple_album: Option<AppleCatalogAlbum>,
    pub storefront: Option<String>,
    match_schema: Option<String>,
    /// Unix seconds.
    pub matched_at: i64,
}

impl QobuzAppleMusicLink {
    /// Whether this answer should stand instead of searching Apple again. A
    /// resolved edition holds indefinitely; a remembered miss expires after
    /// `miss_retry_secs`, because Apple does add catalog editions. Misses from
    /// an older matcher are retried immediately so a rules fix cannot leave a
    /// false negative cached for the full retry window.
    pub fn is_current(&self, miss_retry_secs: i64) -> bool {
        self.apple_album.is_some()
            || (self.match_schema.as_deref() == Some(QOBUZ_APPLE_MUSIC_MATCH_SCHEMA)
                && super::now_secs() - self.matched_at < miss_retry_secs)
    }
}

pub fn apple_music_match_for_qobuz_album(
    qobuz: &QobuzAlbumDetail,
    apple_album: &AppleCatalogAlbum,
) -> QobuzAppleMusicMatch {
    let tracks = qobuz
        .tracks
        .iter()
        .enumerate()
        .map(|(index, track)| qobuz_reference_track(track, index))
        .collect::<Vec<_>>();
    let recording_ids = qobuz
        .tracks
        .iter()
        .filter_map(|track| {
            track
                .isrc
                .as_deref()
                .and_then(normalized_isrc)
                .map(|isrc| (track.id as i64, isrc))
        })
        .collect::<HashMap<_, _>>();
    let assessment = assess_apple_album_match(
        &AppleMatchReference {
            title: &qobuz.album.title,
            artist: Some(&qobuz.album.artist),
            barcode: qobuz.album.upc.as_deref(),
            tracks: &tracks,
            recording_ids: Some(&recording_ids),
        },
        apple_album,
    );
    QobuzAppleMusicMatch {
        confidence: assessment.confidence,
        evidence: assessment.evidence,
        safe_to_link: assessment.safe_to_link,
        paired_track_count: assessment.pairings.len(),
    }
}

/// Shape a Qobuz catalog track like a scanned local track so the shared Apple
/// matcher can pair it. Qobuz IDs stay as the track identity, and the empty
/// file name simply skips the filename pass — Qobuz publishes real titles, so
/// there is no filename evidence to recover.
fn qobuz_reference_track(track: &QobuzTrack, index: usize) -> super::TrackSummary {
    super::TrackSummary {
        id: track.id as i64,
        file_name: String::new(),
        title: track.title.clone(),
        artist: Some(track.artist.clone()),
        album: Some(track.album.clone()),
        album_artist: None,
        track_number: Some(track.track_number.unwrap_or((index + 1) as u32) as i64),
        disc_number: Some(track.disc_number.unwrap_or(1) as i64),
        year: None,
        genre: None,
        composer: None,
        duration_secs: Some(track.duration as f64).filter(|duration| *duration > 0.0),
        sample_rate: None,
        bit_depth: None,
        channels: None,
        format: None,
        album_id: None,
        art_id: None,
        play_count: 0,
        last_played_at: None,
        listened_secs: 0.0,
        preferred_play_source: None,
    }
}

impl Library {
    /// The Apple Music edition already resolved for a standalone Qobuz album,
    /// including a remembered "Apple has nothing" answer.
    pub fn qobuz_apple_music_link(
        &self,
        qobuz_album_id: &str,
    ) -> Result<Option<QobuzAppleMusicLink>, String> {
        let qobuz_album_id = qobuz_album_id.trim();
        if qobuz_album_id.is_empty() {
            return Ok(None);
        }
        let conn = self.conn.lock().unwrap();
        let row = conn
            .query_row(
                r#"
                SELECT payload_json, storefront, match_schema, matched_at
                FROM qobuz_apple_music_links
                WHERE qobuz_album_id = ?1
                "#,
                [qobuz_album_id],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| format!("Qobuz Apple Music link lookup: {error}"))?;
        let Some((payload_json, storefront, match_schema, matched_at)) = row else {
            return Ok(None);
        };
        let apple_album = payload_json
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .map_err(|error| format!("parse Qobuz Apple Music link payload: {error}"))?;
        Ok(Some(QobuzAppleMusicLink {
            apple_album,
            storefront,
            match_schema,
            matched_at,
        }))
    }

    /// Remember the Apple Music edition resolved for a standalone Qobuz album.
    /// `apple_album` of `None` records that the search found nothing, which is
    /// worth keeping so the next visit does not repeat catalog discovery and
    /// candidate hydration.
    pub fn save_qobuz_apple_music_link(
        &self,
        qobuz_album_id: &str,
        apple_album: Option<&AppleCatalogAlbum>,
        confidence: i64,
        requested_storefront: Option<&str>,
    ) -> Result<(), String> {
        let qobuz_album_id = qobuz_album_id.trim();
        if qobuz_album_id.is_empty() {
            return Ok(());
        }
        let payload_json = apple_album
            .map(serde_json::to_string)
            .transpose()
            .map_err(|error| format!("serialize Qobuz Apple Music link payload: {error}"))?;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            r#"
            INSERT INTO qobuz_apple_music_links (
                qobuz_album_id, apple_album_id, storefront, status, confidence,
                payload_json, match_schema, matched_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            ON CONFLICT(qobuz_album_id) DO UPDATE SET
                apple_album_id = excluded.apple_album_id,
                storefront = excluded.storefront,
                status = excluded.status,
                confidence = excluded.confidence,
                payload_json = excluded.payload_json,
                match_schema = excluded.match_schema,
                matched_at = excluded.matched_at
            "#,
            params![
                qobuz_album_id,
                apple_album.map(|album| album.album_id.as_str()),
                apple_album
                    .map(|album| album.storefront.as_str())
                    .filter(|storefront| !storefront.trim().is_empty())
                    .or_else(|| requested_storefront
                        .map(str::trim)
                        .filter(|value| !value.is_empty()))
                    .or(Some("current")),
                if apple_album.is_some() {
                    "linked"
                } else {
                    "no_match"
                },
                confidence,
                payload_json,
                QOBUZ_APPLE_MUSIC_MATCH_SCHEMA,
                super::now_secs(),
            ],
        )
        .map(|_| ())
        .map_err(|error| format!("save Qobuz Apple Music link: {error}"))
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
    recording_ids: Option<&HashMap<i64, String>>,
) -> Vec<ExternalTrackPairing> {
    let mut indexed_pairings = Vec::with_capacity(apple_tracks.len());
    let mut used_local_indices = HashSet::new();
    let mut used_apple_indices = HashSet::new();

    // Catalog display metadata is not a stable recording identity. Qobuz may
    // include a featured artist in the title where Apple puts it in the artist
    // credit, and providers occasionally disagree on a duration by many
    // seconds. An exact ISRC is stronger evidence than either field, so claim
    // those pairs before falling back to the conservative metadata matcher.
    if let Some(recording_ids) = recording_ids {
        let local_isrcs = local_tracks
            .iter()
            .map(|track| {
                recording_ids
                    .get(&track.id)
                    .and_then(|isrc| normalized_isrc(isrc))
            })
            .collect::<Vec<_>>();
        let apple_isrcs = apple_tracks
            .iter()
            .map(|track| track.isrc.as_deref().and_then(normalized_isrc))
            .collect::<Vec<_>>();
        let mut local_isrc_counts = HashMap::new();
        for isrc in local_isrcs.iter().flatten() {
            *local_isrc_counts.entry(isrc.as_str()).or_insert(0_usize) += 1;
        }
        let mut apple_isrc_counts = HashMap::new();
        for isrc in apple_isrcs.iter().flatten() {
            *apple_isrc_counts.entry(isrc.as_str()).or_insert(0_usize) += 1;
        }

        for (apple_index, apple) in apple_tracks.iter().enumerate() {
            let Some(apple_isrc) = apple_isrcs[apple_index].as_ref() else {
                continue;
            };
            // Repeated catalog placeholders are not recording identity. Only
            // let a valid ISRC override display metadata when it identifies
            // exactly one track on each side of this album comparison.
            if local_isrc_counts.get(apple_isrc.as_str()) != Some(&1)
                || apple_isrc_counts.get(apple_isrc.as_str()) != Some(&1)
            {
                continue;
            }
            let matching_local = local_tracks
                .iter()
                .enumerate()
                .filter(|(local_index, _)| {
                    !used_local_indices.contains(local_index)
                        && local_isrcs[*local_index].as_ref() == Some(apple_isrc)
                })
                .min_by_key(|(_, local)| {
                    let same_position = local.disc_number.unwrap_or(1)
                        == apple.disc_number.unwrap_or(1) as i64
                        && local.track_number
                            == Some(apple.track_number.unwrap_or((apple_index + 1) as u32) as i64);
                    !same_position
                });
            let Some((local_index, local)) = matching_local else {
                continue;
            };
            used_local_indices.insert(local_index);
            used_apple_indices.insert(apple_index);
            indexed_pairings.push((
                apple_index,
                ExternalTrackPairing {
                    local_track_id: local.id,
                    provider_track_id: apple.song_id.clone(),
                    confidence: 100,
                    match_kind: "isrc".to_string(),
                },
            ));
        }
    }

    let remaining_local_indices = (0..local_tracks.len())
        .filter(|index| !used_local_indices.contains(index))
        .collect::<Vec<_>>();
    let remaining_apple_indices = (0..apple_tracks.len())
        .filter(|index| !used_apple_indices.contains(index))
        .collect::<Vec<_>>();
    let remaining_local_tracks = remaining_local_indices
        .iter()
        .map(|index| local_tracks[*index].clone())
        .collect::<Vec<_>>();
    let remote_tracks = remaining_apple_indices
        .iter()
        .map(|apple_index| {
            let track = &apple_tracks[*apple_index];
            MbTrack {
                recording_id: track.isrc.clone().or_else(|| Some(track.song_id.clone())),
                disc: track.disc_number.unwrap_or(1) as i64,
                position: track.track_number.unwrap_or((*apple_index + 1) as u32) as i64,
                title: track.title.clone(),
                artist: Some(track.artist.clone()),
                length_secs: track.duration_secs,
            }
        })
        .collect::<Vec<_>>();
    for pairing in pair_tracks(&remaining_local_tracks, &remote_tracks) {
        let Some(local_index) = remaining_local_indices.get(pairing.file_index).copied() else {
            continue;
        };
        let Some(apple_index) = remaining_apple_indices.get(pairing.mb_index).copied() else {
            continue;
        };
        let Some(local) = local_tracks.get(local_index) else {
            continue;
        };
        let Some(apple) = apple_tracks.get(apple_index) else {
            continue;
        };
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
        indexed_pairings.push((
            apple_index,
            ExternalTrackPairing {
                local_track_id: local.id,
                provider_track_id: apple.song_id.clone(),
                confidence,
                match_kind: match_kind.to_string(),
            },
        ));
    }
    indexed_pairings.sort_by_key(|(apple_index, _)| *apple_index);
    indexed_pairings
        .into_iter()
        .map(|(_, pairing)| pairing)
        .collect()
}

fn normalized_isrc(value: &str) -> Option<String> {
    let normalized = value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .map(|character| character.to_ascii_uppercase())
        .collect::<String>();
    let bytes = normalized.as_bytes();
    (bytes.len() == 12
        && bytes[..2].iter().all(|byte| byte.is_ascii_alphabetic())
        && bytes[2..5].iter().all(|byte| byte.is_ascii_alphanumeric())
        && bytes[5..].iter().all(|byte| byte.is_ascii_digit())
        && bytes[7..].iter().any(|byte| *byte != b'0'))
    .then_some(normalized)
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

/// One release's barcode reaches Fozmo in several widths — Qobuz publishes
/// 13-digit EANs where Apple often publishes the same GTIN as a 12-digit UPC,
/// which is the EAN without its leading zero. Leading zeros carry no meaning in
/// a GTIN, so dropping them compares the identifier rather than its formatting.
fn normalize_barcode(value: &str) -> String {
    let digits = value
        .chars()
        .filter(|character| character.is_ascii_digit())
        .collect::<String>();
    let trimmed = digits.trim_start_matches('0');
    if trimmed.is_empty() {
        digits
    } else {
        trimmed.to_string()
    }
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
