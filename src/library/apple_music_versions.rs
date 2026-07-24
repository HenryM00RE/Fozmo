use super::matching::{normalize_for_match, pair_tracks};
use super::provider_versions::{
    ExternalAlbumVersionInput, ExternalTrackPairing, ExternalVersionTrackInput,
    assign_recording_ids_for_external_version, external_album_version_by_provider_id,
    external_version_tracks, rebuild_external_track_links, upsert_external_album_version,
    upsert_external_version_tracks,
};
use super::{
    AlbumVersionSummary, AppleMusicAlbumMatchPreview, AppleMusicTrackPairPreview, Library, MbTrack,
    ResolvedPlaySource,
};
use crate::services::apple_music_musickit::{AppleCatalogAlbum, AppleCatalogSong};
use rusqlite::{OptionalExtension, params};
use std::collections::HashSet;

const PROVIDER: &str = "apple_music";

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
        let title_match =
            normalize_for_match(&local_album.title) == normalize_for_match(&apple_album.title);
        let artist_match = local_album.album_artist.as_deref().is_some_and(|artist| {
            normalize_for_match(artist) == normalize_for_match(&apple_album.artist)
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
        let mut confidence = 0;
        let mut evidence = Vec::new();
        if title_match {
            confidence += 25;
            evidence.push("exact_normalized_title".to_string());
        }
        if artist_match {
            confidence += 20;
            evidence.push("exact_normalized_artist".to_string());
        }
        if track_count_match {
            confidence += 20;
            evidence.push("equal_track_count".to_string());
        }
        match barcode_match {
            Some(true) => {
                confidence += 25;
                evidence.push("upc_match".to_string());
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
        }
        confidence = confidence.clamp(0, 100);
        let safe_to_link = barcode_match != Some(false)
            && track_count_match
            && all_tracks_paired
            && (barcode_match == Some(true) || (title_match && artist_match));
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
        tx.commit()
            .map_err(|error| format!("commit Apple Music unlink: {error}"))?;
        drop(conn);
        Ok(Some(self.album_versions(album_id)?))
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
            let exact_title =
                normalize_for_match(&local.title) == normalize_for_match(&apple.title);
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
