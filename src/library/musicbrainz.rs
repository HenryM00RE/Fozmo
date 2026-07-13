use super::matching::{
    build_candidate_preview, confidence_score, dedupe_by_release_group, edition_score,
    extract_mb_tracks, filename_without_track_prefix, merge_pairings,
    metabrainz_evidence_for_release, normalize_for_match, pair_tracks, parse_year, release_artist,
    verify_release_against_tracks,
};
use super::*;
use crate::audio::player::TrackCover;
use rusqlite::{OptionalExtension, params};
use serde_json::Value;
use std::time::{Duration, Instant};

/// Candidates must score at least this well (text relevance + edition
/// evidence) before we spend a release-detail fetch verifying them.
const AUTO_APPLY_MIN_SCORE: i64 = 90;
/// How many top candidates to fetch and verify per `match_album` call before
/// giving up and leaving the album for manual review. Each fetch costs one
/// rate-limited MusicBrainz request.
const MAX_AUTO_VERIFY_FETCHES: usize = 3;
/// Score a candidate is demoted to when its release fails track-evidence
/// verification, keeping it visible in review but below the auto-apply bar.
const FAILED_EVIDENCE_SCORE: i64 = 70;
const METABRAINZ_TEST_SEARCH_LIMIT: usize = 12;
const METABRAINZ_TEST_DETAIL_FETCHES: usize = 5;

pub(super) fn infer_metabrainz_lookup_terms(
    album: &AlbumSummary,
    raw_folder_title: Option<String>,
) -> MetaBrainzInference {
    let raw = raw_folder_title
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(album.title.as_str());
    let (parsed_artist, parsed_album) = split_artist_album(raw);
    let artist = album
        .album_artist
        .clone()
        .filter(|s| !s.trim().is_empty())
        .or(parsed_artist);
    let lookup_album = clean_lookup_album_title(&parsed_album);
    let album_title = if lookup_album.is_empty() {
        album.title.clone()
    } else {
        lookup_album
    };
    let mut search_queries = Vec::new();
    let title_variants = album_title_search_variants(&album_title);
    if let Some(artist) = artist.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        for title in &title_variants {
            search_queries.push(format!(
                "release:\"{}\" AND artist:\"{}\"",
                escape_musicbrainz_query_value(title),
                escape_musicbrainz_query_value(artist)
            ));
        }
    }
    for title in &title_variants {
        search_queries.push(format!(
            "release:\"{}\"",
            escape_musicbrainz_query_value(title)
        ));
    }
    search_queries.dedup();

    MetaBrainzInference {
        artist,
        album: album_title,
        raw_folder_title,
        search_queries,
    }
}

fn split_artist_album(raw: &str) -> (Option<String>, String) {
    for (idx, ch) in raw.char_indices() {
        if !matches!(ch, '-' | '–' | '—') {
            continue;
        }
        let before = raw[..idx].chars().next_back();
        let after = raw[idx + ch.len_utf8()..].chars().next();
        if before.is_some_and(char::is_whitespace) || after.is_some_and(char::is_whitespace) {
            let artist = compact_lookup_text(&raw[..idx]);
            let album = compact_lookup_text(&raw[idx + ch.len_utf8()..]);
            if !artist.is_empty() && !album.is_empty() {
                return (Some(artist), album);
            }
        }
    }
    (None, compact_lookup_text(raw))
}

fn clean_lookup_album_title(raw: &str) -> String {
    let mut value = raw.replace('_', " ");
    loop {
        let trimmed = value.trim_end().to_string();
        let Some(last) = trimmed.chars().last() else {
            return String::new();
        };
        let (open, close) = match last {
            ')' => ('(', ')'),
            ']' => ('[', ']'),
            _ => break,
        };
        let Some(open_idx) = trimmed.rfind(open) else {
            break;
        };
        let content = trimmed[open_idx + open.len_utf8()..trimmed.len() - close.len_utf8()].trim();
        if !is_technical_lookup_suffix(content) {
            break;
        }
        value = trimmed[..open_idx].to_string();
    }
    compact_lookup_text(&value)
}

fn is_technical_lookup_suffix(content: &str) -> bool {
    let normalized = super::matching::normalize_for_match(content);
    let tokens: Vec<&str> = normalized.split_whitespace().collect();
    if tokens.is_empty() {
        return false;
    }
    let has_format_marker = tokens.iter().any(|token| {
        matches!(
            *token,
            "wav"
                | "wave"
                | "flac"
                | "alac"
                | "aiff"
                | "aif"
                | "mp3"
                | "m4a"
                | "dsd"
                | "dsf"
                | "bit"
                | "bits"
                | "khz"
                | "hz"
                | "hi"
                | "res"
                | "hires"
                | "lossless"
        )
    });
    has_format_marker
        && tokens.iter().all(|token| {
            token.parse::<i64>().is_ok()
                || matches!(
                    *token,
                    "wav"
                        | "wave"
                        | "flac"
                        | "alac"
                        | "aiff"
                        | "aif"
                        | "mp3"
                        | "m4a"
                        | "dsd"
                        | "dsf"
                        | "bit"
                        | "bits"
                        | "khz"
                        | "hz"
                        | "hi"
                        | "res"
                        | "hires"
                        | "lossless"
                )
        })
}

fn compact_lookup_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn album_title_search_variants(title: &str) -> Vec<String> {
    let mut variants = vec![title.to_string()];
    if let Some(variant) = parenthetical_subtitle_as_colon(title) {
        variants.push(variant);
    }
    variants.dedup();
    variants
}

fn parenthetical_subtitle_as_colon(title: &str) -> Option<String> {
    let trimmed = title.trim();
    if !trimmed.ends_with(')') {
        return None;
    }
    let open_idx = trimmed.rfind(" (")?;
    let base = compact_lookup_text(&trimmed[..open_idx]);
    let subtitle = compact_lookup_text(&trimmed[open_idx + 2..trimmed.len() - 1]);
    if base.is_empty() || subtitle.is_empty() {
        return None;
    }
    Some(format!("{base}: {subtitle}"))
}

pub(super) fn track_assisted_metabrainz_queries(
    inference: &MetaBrainzInference,
    tracks: &[TrackSummary],
) -> Vec<String> {
    if tracks.is_empty() {
        return Vec::new();
    }
    let Some(artist) = inference
        .artist
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return Vec::new();
    };
    let mut titles = Vec::new();
    let mut ordered_tracks: Vec<&TrackSummary> = tracks.iter().collect();
    ordered_tracks.sort_by_key(|track| {
        (
            track.disc_number.unwrap_or(1),
            track
                .track_number
                .or_else(|| track_number_from_file_name(&track.file_name))
                .unwrap_or(999_999),
            track.file_name.clone(),
        )
    });
    for track in ordered_tracks.into_iter().take(8) {
        let candidates = vec![
            track.title.clone(),
            filename_without_track_prefix(&track.file_name),
        ];
        for candidate in candidates {
            let cleaned = compact_lookup_text(&candidate);
            if cleaned.len() < 3 || normalize_for_match(&cleaned) == "unknown" {
                continue;
            }
            if !titles.iter().any(|existing: &String| {
                normalize_for_match(existing) == normalize_for_match(&cleaned)
            }) {
                titles.push(cleaned);
            }
            if titles.len() >= 3 {
                break;
            }
        }
        if titles.len() >= 3 {
            break;
        }
    }
    titles
        .into_iter()
        .map(|title| {
            format!(
                "artist:\"{}\" AND \"{}\"",
                escape_musicbrainz_query_value(artist),
                escape_musicbrainz_query_value(&title)
            )
        })
        .chain(std::iter::once(format!(
            "artist:\"{}\" AND tracks:{}",
            escape_musicbrainz_query_value(artist),
            tracks.len()
        )))
        .collect()
}

fn track_number_from_file_name(file_name: &str) -> Option<i64> {
    let stem = file_name
        .rsplit_once('.')
        .map(|(stem, _)| stem)
        .unwrap_or(file_name);
    let digits: String = stem
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse::<i64>().ok().filter(|value| *value > 0)
}

pub(super) fn complete_local_track_number_order(
    tracks: &[TrackSummary],
) -> Option<std::collections::HashMap<i64, i64>> {
    if tracks.is_empty() {
        return None;
    }

    let mut by_track_id = std::collections::HashMap::with_capacity(tracks.len());
    let mut seen = std::collections::HashSet::with_capacity(tracks.len());
    for track in tracks {
        let number = track_number_from_file_name(&track.file_name).or(track.track_number)?;
        if number <= 0 || !seen.insert(number) {
            return None;
        }
        by_track_id.insert(track.id, number);
    }

    let expected_len = tracks.len() as i64;
    let complete =
        seen.len() == tracks.len() && (1..=expected_len).all(|number| seen.contains(&number));
    complete.then_some(by_track_id)
}

fn escape_musicbrainz_query_value(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

impl Library {
    pub async fn test_metabrainz_album(
        &self,
        album_id: i64,
        req: MetaBrainzTestRequest,
    ) -> Result<Option<MetaBrainzTestResponse>, String> {
        let _refresh = req.refresh.unwrap_or(true);
        let Some(album) = self.album(album_id)? else {
            return Ok(None);
        };
        let tracks = self.primary_local_album_tracks(&album)?;
        let version = self.metabrainz_test_version(&album)?;
        let raw_folder_title = self.album_raw_folder_title(album_id)?;
        let mut inference = infer_metabrainz_lookup_terms(&album, raw_folder_title);
        inference
            .search_queries
            .extend(track_assisted_metabrainz_queries(&inference, &tracks));
        inference.search_queries.dedup();
        let mut best: Option<(i64, MatchCandidate, Value, MetaBrainzEvidence)> = None;

        if inference.album != "Unknown Album" {
            let releases = self
                .search_metabrainz_releases(&inference.search_queries)
                .await?;
            let mut scored: Vec<(i64, Value)> = releases
                .into_iter()
                .filter(|release| release.get("id").and_then(|v| v.as_str()).is_some())
                .map(|release| {
                    let title = release
                        .get("title")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Unknown Album");
                    let artist = release_artist(&release);
                    let base = release
                        .get("score")
                        .and_then(|v| v.as_i64())
                        .unwrap_or_else(|| {
                            confidence_score(
                                &inference.album,
                                title,
                                inference.artist.as_deref(),
                                artist.as_deref(),
                            )
                        });
                    (edition_score(base, &release, tracks.len()), release)
                })
                .collect();
            scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));

            for (score, search_release) in scored.into_iter().take(METABRAINZ_TEST_DETAIL_FETCHES) {
                let Some(provider_id) = search_release.get("id").and_then(|v| v.as_str()) else {
                    continue;
                };
                let release = self.fetch_release_detail(provider_id).await?;
                let evidence = metabrainz_evidence_for_release(&release, &tracks);
                if evidence.local_track_count == 0
                    || evidence.paired_tracks * 5 < evidence.local_track_count * 4
                {
                    continue;
                }
                let candidate = metabrainz_candidate_from_release(&release, provider_id, score);
                let rank = metabrainz_test_rank(score, &evidence);
                if best
                    .as_ref()
                    .is_none_or(|(best_rank, _, _, _)| rank > *best_rank)
                {
                    best = Some((rank, candidate, release, evidence.clone()));
                }
                if evidence.auto_apply_eligible {
                    break;
                }
            }
        }

        let (best_candidate, preview, evidence) =
            if let Some((_, candidate, release, mut evidence)) = best {
                if !evidence.auto_apply_eligible
                    && !evidence
                        .warnings
                        .iter()
                        .any(|warning| warning == "No safe match found")
                {
                    evidence.warnings.push("No safe match found".to_string());
                }
                let preview = build_candidate_preview(
                    candidate.clone(),
                    album.clone(),
                    tracks.clone(),
                    release,
                );
                (Some(candidate), Some(preview), evidence)
            } else {
                (None, None, empty_metabrainz_evidence(tracks.len()))
            };

        Ok(Some(MetaBrainzTestResponse {
            album,
            version,
            tracks,
            inference,
            best_candidate,
            preview,
            evidence,
            qobuz_match: None,
        }))
    }

    fn metabrainz_test_version(
        &self,
        album: &AlbumSummary,
    ) -> Result<Option<AlbumVersionSummary>, String> {
        let versions = self.album_versions(album.id)?;
        if let Some(primary_id) = album.primary_version_id
            && let Some(version) = versions
                .iter()
                .find(|version| version.id == primary_id && version.provider == "local")
                .cloned()
        {
            return Ok(Some(version));
        }
        Ok(versions
            .iter()
            .find(|version| version.provider == "local" && version.is_primary)
            .cloned()
            .or_else(|| {
                versions
                    .iter()
                    .find(|version| version.provider == "local")
                    .cloned()
            }))
    }

    pub async fn match_album(
        &self,
        album_id: i64,
        req: MatchRequest,
    ) -> Result<Option<MatchResponse>, String> {
        let replace_cover = req.replace_cover.unwrap_or(true);
        let manual = req.manual_pairings.clone();
        if let Some(candidate_id) = req.candidate_id {
            self.approve_candidate(album_id, candidate_id, replace_cover, manual)
                .await?;
            let album = self.album(album_id)?;
            let candidates = self.match_candidates(album_id)?;
            return Ok(album.map(|album| MatchResponse {
                album,
                candidates,
                applied: true,
            }));
        }

        if req.refresh.unwrap_or(true) || self.match_candidates(album_id)?.is_empty() {
            self.refresh_musicbrainz_candidates(album_id).await?;
        }

        // Auto-apply only with track-level evidence: fetch the full release
        // for the strongest candidates and check track count and durations
        // against the local files. A high search score alone is not enough —
        // MB routinely returns 100 for the wrong edition.
        let mut applied = false;
        let Some(album_for_tracks) = self.album(album_id)? else {
            return Ok(None);
        };
        let file_tracks = self.primary_local_album_tracks(&album_for_tracks)?;
        let candidates = self.match_candidates(album_id)?;
        let verifiable: Vec<&MatchCandidate> = candidates
            .iter()
            .filter(|c| c.status == "pending" && c.score >= AUTO_APPLY_MIN_SCORE)
            .take(MAX_AUTO_VERIFY_FETCHES)
            .collect();
        for candidate in verifiable {
            let release = self.fetch_release_detail(&candidate.provider_id).await?;
            let evidence = verify_release_against_tracks(&release, &file_tracks);
            if evidence.pass {
                // Auto-apply path has no manual pairings; those only come
                // from explicit Apply clicks in the modal.
                self.approve_candidate_with_release(
                    album_id,
                    candidate.id,
                    &candidate.provider_id,
                    &release,
                    replace_cover,
                    Vec::new(),
                )
                .await?;
                applied = true;
                break;
            }
            eprintln!(
                "musicbrainz: candidate {} ({}) failed track evidence: count_match={} paired={}/{} durations={}/{}",
                candidate.id,
                candidate.provider_id,
                evidence.track_count_match,
                evidence.paired,
                file_tracks.len(),
                evidence.duration_within,
                evidence.duration_checked,
            );
            self.set_candidate_score(candidate.id, candidate.score.min(FAILED_EVIDENCE_SCORE))?;
        }

        let album = self.album(album_id)?;
        let candidates = self.match_candidates(album_id)?;
        Ok(album.map(|album| MatchResponse {
            album,
            candidates,
            applied,
        }))
    }

    pub async fn autometa_match_musicbrainz_version(
        &self,
        album_id: i64,
        version_id: i64,
    ) -> Result<Option<AutoMetaMusicBrainzResult>, String> {
        let Some(album) = self.album(album_id)? else {
            return Ok(None);
        };
        let tracks = self.local_album_tracks_for_version(album_id, version_id)?;
        if tracks.is_empty() {
            return Ok(None);
        }
        let raw_folder_title = self.version_raw_folder_title(version_id)?;
        let mut inference = infer_metabrainz_lookup_terms(&album, raw_folder_title);
        inference
            .search_queries
            .extend(track_assisted_metabrainz_queries(&inference, &tracks));
        inference.search_queries.dedup();
        if inference.album == "Unknown Album" {
            return Ok(None);
        }
        let update_canonical_album = {
            let conn = self.conn.lock().unwrap();
            conn.query_row(
                "SELECT COALESCE(primary_version_id = ?2, 0) FROM albums WHERE id = ?1",
                params![album_id, version_id],
                |row| row.get::<_, bool>(0),
            )
            .map_err(|e| format!("AutoMetadata primary version check: {e}"))?
        };

        let mut verified_release_ids = std::collections::HashSet::new();
        for query in &inference.search_queries {
            let releases = self
                .search_metabrainz_releases(std::slice::from_ref(query))
                .await?;
            let mut scored: Vec<(i64, Value)> = releases
                .into_iter()
                .filter(|release| release.get("id").and_then(|v| v.as_str()).is_some())
                .map(|release| {
                    let title = release
                        .get("title")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Unknown Album");
                    let artist = release_artist(&release);
                    let base = release
                        .get("score")
                        .and_then(|v| v.as_i64())
                        .unwrap_or_else(|| {
                            confidence_score(
                                &inference.album,
                                title,
                                inference.artist.as_deref(),
                                artist.as_deref(),
                            )
                        });
                    (edition_score(base, &release, tracks.len()), release)
                })
                .collect();
            scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));

            for (_score, search_release) in dedupe_by_release_group(scored)
                .into_iter()
                .take(MAX_AUTO_VERIFY_FETCHES)
            {
                let Some(release_id) = search_release.get("id").and_then(|v| v.as_str()) else {
                    continue;
                };
                if !verified_release_ids.insert(release_id.to_string()) {
                    continue;
                }
                let release = self.fetch_release_detail(release_id).await?;
                let evidence = metabrainz_evidence_for_release(&release, &tracks);
                if !evidence.auto_apply_eligible {
                    continue;
                }
                self.approve_release_for_tracks(
                    album_id,
                    None,
                    release_id,
                    &release,
                    false,
                    Vec::new(),
                    tracks.clone(),
                    update_canonical_album,
                )
                .await?;
                return Ok(Some(AutoMetaMusicBrainzResult {
                    release_id: release_id.to_string(),
                }));
            }
        }

        Ok(None)
    }

    pub async fn manual_search(
        &self,
        album_id: i64,
        req: ManualSearchRequest,
    ) -> Result<Option<MatchResponse>, String> {
        let Some(album) = self.album(album_id)? else {
            return Ok(None);
        };

        let query = if let Some(q) = req
            .query
            .as_ref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
        {
            q.to_string()
        } else {
            let mut parts: Vec<String> = Vec::new();
            if let Some(a) = req
                .album
                .as_ref()
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
            {
                parts.push(format!("release:\"{}\"", a));
            }
            if let Some(a) = req
                .artist
                .as_ref()
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
            {
                parts.push(format!("artist:\"{}\"", a));
            }
            if let Some(y) = req.year {
                parts.push(format!("date:{}", y));
            }
            if parts.is_empty() {
                return Err("manual search needs at least one field".to_string());
            }
            parts.join(" AND ")
        };

        {
            let conn = self.conn.lock().unwrap();
            let _ = conn.execute(
                "DELETE FROM match_candidates WHERE album_id = ?1",
                [album_id],
            );
        }

        self.wait_musicbrainz_turn().await;
        eprintln!("musicbrainz: manual search ({query})");
        let started = Instant::now();
        let response = self
            .http
            .get("https://musicbrainz.org/ws/2/release/")
            .query(&[("fmt", "json"), ("limit", "12"), ("query", query.as_str())])
            .send()
            .await
            .map_err(|e| format!("musicbrainz manual search: {e}"))?;
        if !response.status().is_success() {
            return Err(format!(
                "musicbrainz manual search returned {}",
                response.status()
            ));
        }
        let body: Value = response
            .json()
            .await
            .map_err(|e| format!("musicbrainz manual search json: {e}"))?;
        let releases = body
            .get("releases")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        eprintln!(
            "musicbrainz: manual search done in {:.1}s, {} candidate(s)",
            started.elapsed().as_secs_f32(),
            releases.len()
        );

        self.store_release_candidates(&album, releases, 12)?;

        let candidates = self.match_candidates(album_id)?;
        Ok(Some(MatchResponse {
            album,
            candidates,
            applied: false,
        }))
    }

    pub async fn lookup_mbid(
        &self,
        album_id: i64,
        req: MbidLookupRequest,
    ) -> Result<Option<MatchResponse>, String> {
        let Some(album) = self.album(album_id)? else {
            return Ok(None);
        };

        let mbid = req.mbid.trim();
        if mbid.is_empty() {
            return Err("mbid is required".to_string());
        }
        if mbid.len() != 36 {
            return Err("mbid should be a 36-character UUID".to_string());
        }

        let release = self.fetch_release_detail(mbid).await?;
        let title = release
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown")
            .to_string();
        let r_artist = release_artist(&release);
        let year = release
            .get("date")
            .and_then(|v| v.as_str())
            .and_then(parse_year);
        let payload = serde_json::to_string(&release).unwrap_or_else(|_| "{}".to_string());

        {
            let conn = self.conn.lock().unwrap();
            conn.execute(
                r#"
                INSERT INTO match_candidates
                    (album_id, provider, provider_id, title, artist, year, score, payload_json, status, created_at)
                VALUES (?1, 'musicbrainz', ?2, ?3, ?4, ?5, 100, ?6, 'pending', ?7)
                ON CONFLICT(album_id, provider, provider_id) DO UPDATE SET
                    title = excluded.title,
                    artist = excluded.artist,
                    year = excluded.year,
                    score = 100,
                    payload_json = excluded.payload_json,
                    status = 'pending'
                "#,
                params![album_id, mbid, title, r_artist, year, payload, now_secs()],
            )
            .map_err(|e| format!("store mbid candidate: {e}"))?;
        }

        let candidates = self.match_candidates(album_id)?;
        Ok(Some(MatchResponse {
            album,
            candidates,
            applied: false,
        }))
    }

    async fn refresh_musicbrainz_candidates(&self, album_id: i64) -> Result<(), String> {
        let Some(album) = self.album(album_id)? else {
            return Ok(());
        };
        if album.title == "Unknown Album" {
            return Ok(());
        }
        self.wait_musicbrainz_turn().await;
        let artist = album.album_artist.clone().unwrap_or_default();
        let query = if artist.is_empty() {
            format!("release:\"{}\"", album.title)
        } else {
            format!("release:\"{}\" AND artist:\"{}\"", album.title, artist)
        };
        eprintln!("musicbrainz: searching ({query})");
        let started = Instant::now();
        // Fetch more than we keep: release-group dedupe collapses duplicate
        // pressings, so a wider net still yields a short list of distinct editions.
        let response = self
            .http
            .get("https://musicbrainz.org/ws/2/release/")
            .query(&[("fmt", "json"), ("limit", "10"), ("query", query.as_str())])
            .send()
            .await
            .map_err(|e| format!("musicbrainz search: {e}"))?;
        if !response.status().is_success() {
            return Err(format!("musicbrainz search returned {}", response.status()));
        }
        let body: Value = response
            .json()
            .await
            .map_err(|e| format!("musicbrainz json: {e}"))?;
        let releases = body
            .get("releases")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        eprintln!(
            "musicbrainz: search done in {:.1}s, {} candidate(s)",
            started.elapsed().as_secs_f32(),
            releases.len()
        );
        self.store_release_candidates(&album, releases, 8)
    }

    /// Score, dedupe, and store search results as match candidates. Scores
    /// combine MB's text relevance with edition evidence (track count,
    /// official status), and duplicate pressings of the same release group
    /// collapse to the best-scoring one so the list shows distinct editions.
    fn store_release_candidates(
        &self,
        album: &AlbumSummary,
        releases: Vec<Value>,
        limit: usize,
    ) -> Result<(), String> {
        let local_track_count = self.primary_local_album_tracks(album)?.len();
        let scored: Vec<(i64, Value)> = releases
            .into_iter()
            .filter(|release| release.get("id").and_then(|v| v.as_str()).is_some())
            .map(|release| {
                let title = release
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Unknown Album");
                let artist = release_artist(&release);
                let base = release
                    .get("score")
                    .and_then(|v| v.as_i64())
                    .unwrap_or_else(|| {
                        confidence_score(
                            &album.title,
                            title,
                            album.album_artist.as_deref(),
                            artist.as_deref(),
                        )
                    });
                let score = edition_score(base, &release, local_track_count);
                (score, release)
            })
            .collect();

        let now = now_secs();
        let conn = self.conn.lock().unwrap();
        for (score, release) in dedupe_by_release_group(scored).into_iter().take(limit) {
            let Some(provider_id) = release.get("id").and_then(|v| v.as_str()) else {
                continue;
            };
            let title = release
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown Album");
            let artist = release_artist(&release);
            let year = release
                .get("date")
                .and_then(|v| v.as_str())
                .and_then(parse_year);
            let payload = serde_json::to_string(&release).unwrap_or_else(|_| "{}".to_string());
            conn.execute(
                r#"
                INSERT INTO match_candidates
                    (album_id, provider, provider_id, title, artist, year, score, payload_json, status, created_at)
                VALUES (?1, 'musicbrainz', ?2, ?3, ?4, ?5, ?6, ?7, 'pending', ?8)
                ON CONFLICT(album_id, provider, provider_id) DO UPDATE SET
                    title = excluded.title,
                    artist = excluded.artist,
                    year = excluded.year,
                    score = excluded.score,
                    payload_json = excluded.payload_json,
                    status = 'pending'
                "#,
                params![album.id, provider_id, title, artist, year, score, payload, now],
            )
            .map_err(|e| format!("store musicbrainz candidate: {e}"))?;
        }
        Ok(())
    }

    async fn search_metabrainz_releases(&self, queries: &[String]) -> Result<Vec<Value>, String> {
        let mut releases = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for query in queries {
            self.wait_musicbrainz_turn().await;
            eprintln!("metabrainz: test search ({query})");
            let started = Instant::now();
            let response = self
                .http
                .get("https://musicbrainz.org/ws/2/release/")
                .query(&[
                    ("fmt", "json".to_string()),
                    ("limit", METABRAINZ_TEST_SEARCH_LIMIT.to_string()),
                    ("query", query.clone()),
                ])
                .send()
                .await
                .map_err(|e| format!("metabrainz test search: {e}"))?;
            if !response.status().is_success() {
                return Err(format!(
                    "metabrainz test search returned {}",
                    response.status()
                ));
            }
            let body: Value = response
                .json()
                .await
                .map_err(|e| format!("metabrainz test search json: {e}"))?;
            let found = body
                .get("releases")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            eprintln!(
                "metabrainz: test search done in {:.1}s, {} candidate(s)",
                started.elapsed().as_secs_f32(),
                found.len()
            );
            for release in found {
                let Some(id) = release.get("id").and_then(|v| v.as_str()) else {
                    continue;
                };
                if seen.insert(id.to_string()) {
                    releases.push(release);
                }
            }
        }
        Ok(releases)
    }

    fn album_raw_folder_title(&self, album_id: i64) -> Result<Option<String>, String> {
        let conn = self.conn.lock().unwrap();
        let path: Option<String> = conn
            .query_row(
                r#"
                SELECT path FROM tracks
                WHERE album_id = ?1
                ORDER BY COALESCE(disc_number, 1), COALESCE(track_number, 999999), file_name
                LIMIT 1
                "#,
                [album_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| format!("album folder lookup: {e}"))?;
        Ok(path.and_then(|path| raw_album_folder_title_from_path(std::path::Path::new(&path))))
    }

    fn set_candidate_score(&self, candidate_id: i64, score: i64) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE match_candidates SET score = ?2 WHERE id = ?1",
            params![candidate_id, score],
        )
        .map_err(|e| format!("update candidate score: {e}"))?;
        Ok(())
    }

    pub async fn preview_candidate(
        &self,
        album_id: i64,
        candidate_id: i64,
    ) -> Result<Option<CandidatePreview>, String> {
        let candidate = {
            let conn = self.conn.lock().unwrap();
            conn.query_row(
                "SELECT id, provider, provider_id, title, artist, year, score, status
                 FROM match_candidates WHERE id = ?1 AND album_id = ?2",
                params![candidate_id, album_id],
                |row| {
                    Ok(MatchCandidate {
                        id: row.get(0)?,
                        provider: row.get(1)?,
                        provider_id: row.get(2)?,
                        title: row.get(3)?,
                        artist: row.get(4)?,
                        year: row.get(5)?,
                        score: row.get(6)?,
                        status: row.get(7)?,
                    })
                },
            )
            .optional()
            .map_err(|e| format!("candidate lookup: {e}"))?
        };
        let Some(candidate) = candidate else {
            return Ok(None);
        };

        let release = self.fetch_release_detail(&candidate.provider_id).await?;
        let Some(album) = self.album(album_id)? else {
            return Ok(None);
        };
        let file_tracks = self.primary_local_album_tracks(&album)?;
        Ok(Some(build_candidate_preview(
            candidate,
            album,
            file_tracks,
            release,
        )))
    }

    async fn approve_candidate(
        &self,
        album_id: i64,
        candidate_id: i64,
        replace_cover: bool,
        manual_pairings: Vec<ManualPairing>,
    ) -> Result<(), String> {
        let candidate_row: Option<(String,)> = {
            let conn = self.conn.lock().unwrap();
            conn.query_row(
                "SELECT provider_id FROM match_candidates WHERE id = ?1 AND album_id = ?2",
                params![candidate_id, album_id],
                |row| Ok((row.get(0)?,)),
            )
            .optional()
            .map_err(|e| format!("candidate lookup: {e}"))?
        };
        let Some((release_id,)) = candidate_row else {
            return Ok(());
        };

        let release = self.fetch_release_detail(&release_id).await?;
        self.approve_candidate_with_release(
            album_id,
            candidate_id,
            &release_id,
            &release,
            replace_cover,
            manual_pairings,
        )
        .await
    }

    /// Apply an already-fetched release to the album. Split out from
    /// `approve_candidate` so the auto-apply path can reuse the release it
    /// just fetched for track-evidence verification instead of fetching twice.
    async fn approve_candidate_with_release(
        &self,
        album_id: i64,
        candidate_id: i64,
        release_id: &str,
        release: &Value,
        replace_cover: bool,
        manual_pairings: Vec<ManualPairing>,
    ) -> Result<(), String> {
        let Some(album) = self.album(album_id)? else {
            return Ok(());
        };
        let file_tracks = self.primary_local_album_tracks(&album)?;
        self.approve_release_for_tracks(
            album_id,
            Some(candidate_id),
            release_id,
            release,
            replace_cover,
            manual_pairings,
            file_tracks,
            true,
        )
        .await
    }

    // Release approval combines the selected release, cover policy, pairings, and file tracks.
    #[allow(clippy::too_many_arguments)]
    async fn approve_release_for_tracks(
        &self,
        album_id: i64,
        candidate_id: Option<i64>,
        release_id: &str,
        release: &Value,
        replace_cover: bool,
        manual_pairings: Vec<ManualPairing>,
        file_tracks: Vec<TrackSummary>,
        update_canonical_album: bool,
    ) -> Result<(), String> {
        let art_id = if replace_cover {
            self.fetch_cover_art(release_id).await.ok().flatten()
        } else {
            eprintln!("musicbrainz: skipping cover art fetch (replace_cover=false)");
            None
        };

        let mb_title = release
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown Album")
            .to_string();
        let mb_artist = release_artist(release);
        let mb_year = release
            .get("date")
            .and_then(|v| v.as_str())
            .and_then(parse_year);
        let mb_release_group_id = release
            .get("release-group")
            .and_then(|rg| rg.get("id"))
            .and_then(|id| id.as_str())
            .map(|s| s.to_string());
        // The release group's first-release-date is the album's original year;
        // the release date may be a reissue/remaster. Fall back to the release
        // year so the column is populated either way.
        let mb_original_year = release
            .get("release-group")
            .and_then(|rg| rg.get("first-release-date"))
            .and_then(|v| v.as_str())
            .and_then(parse_year)
            .or(mb_year);
        let mb_barcode = release
            .get("barcode")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        let mb_tracks = extract_mb_tracks(release);
        let pairings = merge_pairings(
            pair_tracks(&file_tracks, &mb_tracks),
            &manual_pairings,
            &file_tracks,
            &mb_tracks,
        );
        let local_track_numbers = complete_local_track_number_order(&file_tracks);

        let now = now_secs();
        // Lexical scope (not just drop()) so the guard provably ends before
        // the awaits below — required for the handler futures to stay Send.
        {
            let conn = self.conn.lock().unwrap();
            conn.execute(
                r#"
            UPDATE albums
            SET title = CASE WHEN ?11 THEN ?2 ELSE title END,
                album_artist = CASE WHEN ?11 THEN COALESCE(?3, album_artist) ELSE album_artist END,
                year = CASE WHEN ?11 THEN COALESCE(?4, year) ELSE year END,
                original_year = CASE WHEN ?11 THEN COALESCE(?9, original_year) ELSE original_year END,
                confidence = CASE WHEN ?11 THEN 100 ELSE confidence END,
                match_status = CASE WHEN ?11 THEN 'matched' ELSE match_status END,
                mb_release_id = CASE WHEN ?11 THEN ?5 ELSE mb_release_id END,
                mb_release_group_id = CASE WHEN ?11 THEN ?6 ELSE mb_release_group_id END,
                mb_barcode = CASE WHEN ?11 THEN ?10 ELSE mb_barcode END,
                art_id = CASE WHEN ?11 THEN COALESCE(?7, art_id) ELSE art_id END,
                updated_at = ?8
            WHERE id = ?1
            "#,
                params![
                    album_id,
                    mb_title,
                    mb_artist,
                    mb_year,
                    release_id,
                    mb_release_group_id,
                    art_id,
                    now,
                    mb_original_year,
                    mb_barcode,
                    update_canonical_album
                ],
            )
            .map_err(|e| format!("approve candidate album: {e}"))?;

            for pairing in &pairings {
                let mb = &mb_tracks[pairing.mb_index];
                let ft = &file_tracks[pairing.file_index];
                let track_number = local_track_numbers
                    .as_ref()
                    .and_then(|numbers| numbers.get(&ft.id).copied())
                    .unwrap_or(mb.position);
                conn.execute(
                    r#"
                UPDATE tracks
                SET title = ?2,
                    artist = COALESCE(?3, artist),
                    album = ?4,
                    album_artist = COALESCE(?5, album_artist),
                    year = COALESCE(?6, year),
                    track_number = ?7,
                    disc_number = ?8,
                    mb_recording_id = COALESCE(?9, mb_recording_id),
                    updated_at = ?10
                WHERE id = ?1
                "#,
                    params![
                        ft.id,
                        mb.title,
                        mb.artist,
                        mb_title,
                        mb_artist,
                        mb_year,
                        track_number,
                        mb.disc,
                        mb.recording_id,
                        now
                    ],
                )
                .map_err(|e| format!("approve candidate track {}: {e}", ft.id))?;

                let _ = conn.execute("DELETE FROM tracks_fts WHERE track_id = ?1", [ft.id]);
                let _ = conn.execute(
                "INSERT INTO tracks_fts (track_id, title, artist, album, album_artist, composer, genre, file_name)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    ft.id,
                    mb.title,
                    mb.artist.as_deref(),
                    mb_title.as_str(),
                    mb_artist.as_deref(),
                    ft.composer.as_deref(),
                    ft.genre.as_deref(),
                    ft.file_name.as_str(),
                ],
            );
            }

            if let Some(candidate_id) = candidate_id {
                conn.execute(
                    "UPDATE match_candidates SET status = CASE WHEN id = ?2 THEN 'approved' ELSE 'rejected' END WHERE album_id = ?1",
                    params![album_id, candidate_id],
                )
                .map_err(|e| format!("approve candidate state: {e}"))?;
            }

            Self::sync_local_versions_for_album_with_conn(&conn, album_id)?;
            Self::sync_recording_identity_for_album_with_conn(&conn, album_id)?;
        }

        if let Some(name) = mb_artist.as_deref().filter(|s| !s.trim().is_empty()) {
            self.upsert_artist(name)?;
        }

        // With identity settled, try to upgrade the cover from the iTunes
        // Store (barcode-first, hi-res). Non-fatal: the CAA cover fetched
        // above already landed in art_id. Skipped when the user asked to
        // keep their existing cover.
        if replace_cover && let Err(e) = self.improve_album_art(album_id).await {
            eprintln!("itunes: cover upgrade after match failed: {e}");
        }
        Ok(())
    }

    fn version_raw_folder_title(&self, version_id: i64) -> Result<Option<String>, String> {
        let conn = self.conn.lock().unwrap();
        let path: Option<String> = conn
            .query_row(
                r#"
                SELECT t.path
                FROM version_tracks vt
                JOIN tracks t ON t.id = vt.local_track_id
                WHERE vt.version_id = ?1
                ORDER BY COALESCE(t.disc_number, 1), COALESCE(t.track_number, 999999), t.file_name
                LIMIT 1
                "#,
                [version_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| format!("version folder lookup: {e}"))?;
        Ok(path.and_then(|path| raw_album_folder_title_from_path(std::path::Path::new(&path))))
    }

    async fn fetch_release_detail(&self, release_id: &str) -> Result<Value, String> {
        self.wait_musicbrainz_turn().await;
        eprintln!("musicbrainz: fetching release {release_id}");
        let started = Instant::now();
        let response = self
            .http
            .get(format!("https://musicbrainz.org/ws/2/release/{release_id}"))
            .query(&[
                ("fmt", "json"),
                ("inc", "recordings+artist-credits+release-groups+media"),
            ])
            .send()
            .await
            .map_err(|e| format!("musicbrainz release fetch: {e}"))?;
        if !response.status().is_success() {
            return Err(format!(
                "musicbrainz release returned {}",
                response.status()
            ));
        }
        let body = response
            .json::<Value>()
            .await
            .map_err(|e| format!("musicbrainz release json: {e}"))?;
        eprintln!(
            "musicbrainz: release fetched in {:.1}s",
            started.elapsed().as_secs_f32()
        );
        Ok(body)
    }

    async fn fetch_cover_art(&self, release_id: &str) -> Result<Option<i64>, String> {
        self.wait_musicbrainz_turn().await;
        let response = self
            .http
            .get(format!(
                "https://coverartarchive.org/release/{release_id}/front-500"
            ))
            .send()
            .await
            .map_err(|e| format!("cover art fetch: {e}"))?;
        if response.status().as_u16() == 404 {
            return Ok(None);
        }
        if !response.status().is_success() {
            return Err(format!("cover art returned {}", response.status()));
        }
        let mime = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("image/jpeg")
            .to_string();
        let data = response
            .bytes()
            .await
            .map_err(|e| format!("cover art bytes: {e}"))?
            .to_vec();
        let cover = TrackCover { mime, data };
        self.save_artwork(&cover, "coverartarchive").map(Some)
    }

    async fn wait_musicbrainz_turn(&self) {
        let mut guard = self.last_mb_request.lock().await;
        if let Some(last) = *guard {
            let elapsed = last.elapsed();
            if elapsed < Duration::from_secs(1) {
                tokio::time::sleep(Duration::from_secs(1) - elapsed).await;
            }
        }
        *guard = Some(Instant::now());
    }
}

fn metabrainz_candidate_from_release(
    release: &Value,
    provider_id: &str,
    score: i64,
) -> MatchCandidate {
    MatchCandidate {
        id: 0,
        provider: "musicbrainz".to_string(),
        provider_id: provider_id.to_string(),
        title: release
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown Album")
            .to_string(),
        artist: release_artist(release),
        year: release
            .get("date")
            .and_then(|v| v.as_str())
            .and_then(parse_year),
        score,
        status: "preview".to_string(),
    }
}

fn metabrainz_test_rank(score: i64, evidence: &MetaBrainzEvidence) -> i64 {
    let local_count = evidence.local_track_count.max(1) as i64;
    let mut rank = score;
    if evidence.auto_apply_eligible {
        rank += 1_000;
    }
    if evidence.release_status.as_deref() == Some("Official") {
        rank += 75;
    } else {
        rank -= 100;
    }
    if evidence.track_count_match {
        rank += 250;
    } else {
        rank -= 300;
    }
    match evidence.disc_count_match {
        Some(true) => rank += 100,
        Some(false) => rank -= 200,
        None => {}
    }
    rank += (evidence.paired_tracks as i64 * 100) / local_count;
    if evidence.duration_checked > 0 {
        rank += (evidence.duration_within as i64 * 75) / evidence.duration_checked as i64;
        if evidence.duration_within < evidence.duration_checked {
            rank -= 100;
        }
    }
    rank
}

fn empty_metabrainz_evidence(local_track_count: usize) -> MetaBrainzEvidence {
    MetaBrainzEvidence {
        auto_apply_eligible: false,
        release_status: None,
        track_count_match: false,
        disc_count_match: None,
        paired_tracks: 0,
        local_track_count,
        duration_checked: 0,
        duration_within: 0,
        warnings: vec!["No safe match found".to_string()],
    }
}

fn raw_album_folder_title_from_path(path: &std::path::Path) -> Option<String> {
    let parent = path.parent()?;
    let folder = if parent
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(is_disc_folder_name)
    {
        parent.parent().unwrap_or(parent)
    } else {
        parent
    };
    folder
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn is_disc_folder_name(name: &str) -> bool {
    let lower = name.trim().to_ascii_lowercase();
    for prefix in ["disc", "disk", "cd"] {
        if let Some(rest) = lower.strip_prefix(prefix) {
            let rest = rest.trim_start_matches(|c: char| {
                c.is_ascii_whitespace() || matches!(c, '-' | '_' | '.' | '#')
            });
            return rest.chars().next().is_some_and(|c| c.is_ascii_digit());
        }
    }
    false
}
