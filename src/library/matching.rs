use super::{
    AlbumPreview, AlbumSummary, CandidatePreview, FieldDiff, ManualPairing, MatchCandidate,
    MbTrack, MetaBrainzEvidence, TrackPreview, TrackSummary, normalize_key,
};
use serde_json::Value;

pub(super) fn release_artist(release: &Value) -> Option<String> {
    release
        .get("artist-credit")
        .and_then(|v| v.as_array())
        .and_then(|credits| {
            let names: Vec<String> = credits
                .iter()
                .filter_map(|credit| {
                    credit
                        .get("artist")
                        .and_then(|artist| artist.get("name"))
                        .and_then(|name| name.as_str())
                        .map(|name| name.to_string())
                })
                .collect();
            if names.is_empty() {
                None
            } else {
                Some(names.join(", "))
            }
        })
}

pub(crate) fn confidence_score(
    local_title: &str,
    remote_title: &str,
    local_artist: Option<&str>,
    remote_artist: Option<&str>,
) -> i64 {
    let mut score = if normalize_key(local_title) == normalize_key(remote_title) {
        70
    } else {
        35
    };
    if let (Some(local), Some(remote)) = (local_artist, remote_artist)
        && normalize_key(local) == normalize_key(remote)
    {
        score += 25;
    }
    score.min(100)
}

pub(super) fn release_group_id(release: &Value) -> Option<&str> {
    release
        .get("release-group")
        .and_then(|rg| rg.get("id"))
        .and_then(|v| v.as_str())
}

/// Total track count for a release. Search results carry a top-level
/// `track-count`; full release lookups carry per-medium counts instead.
pub(super) fn release_track_count(release: &Value) -> Option<i64> {
    if let Some(count) = release.get("track-count").and_then(|v| v.as_i64()) {
        return Some(count);
    }
    let media = release.get("media")?.as_array()?;
    let mut total = 0;
    let mut any = false;
    for medium in media {
        let count = medium
            .get("track-count")
            .and_then(|v| v.as_i64())
            .or_else(|| {
                medium
                    .get("tracks")
                    .and_then(|t| t.as_array())
                    .map(|t| t.len() as i64)
            });
        if let Some(count) = count {
            total += count;
            any = true;
        }
    }
    any.then_some(total)
}

/// Combine MB's text-relevance score with the edition evidence available in
/// search results: a release whose track count matches the local files is far
/// more likely to be the pressing the user actually owns, and official
/// releases beat bootlegs/promos. The text score is capped at 75 so edition
/// evidence always separates candidates MB rates identically — only a release
/// with a matching track count can reach the auto-apply band.
pub(super) fn edition_score(base: i64, release: &Value, local_track_count: usize) -> i64 {
    let mut score = base.min(75);
    if local_track_count > 0
        && let Some(count) = release_track_count(release)
    {
        score += match (count - local_track_count as i64).abs() {
            0 => 20,
            1 => 5,
            _ => -20,
        };
    }
    match release.get("status").and_then(|v| v.as_str()) {
        Some("Official") => score += 5,
        Some(_) => score -= 10,
        None => {}
    }
    score.clamp(0, 100)
}

/// Keep only the best-scoring release per release group so the candidate list
/// shows distinct editions rather than five pressings of the same album.
/// Releases without a release group are kept individually. Returns the
/// surviving `(score, release)` pairs sorted by score descending.
pub(super) fn dedupe_by_release_group(mut scored: Vec<(i64, Value)>) -> Vec<(i64, Value)> {
    scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
    let mut seen = std::collections::HashSet::new();
    scored.retain(|(_, release)| match release_group_id(release) {
        Some(rg) => seen.insert(rg.to_string()),
        None => true,
    });
    scored
}

/// Maximum per-track drift between the MB length and the local file duration
/// before the pair counts against the candidate.
const DURATION_TOLERANCE_SECS: f64 = 3.0;

#[derive(Debug)]
pub(super) struct TrackEvidence {
    pub(super) track_count_match: bool,
    pub(super) paired: usize,
    pub(super) duration_checked: usize,
    pub(super) duration_within: usize,
    pub(super) pass: bool,
}

/// Verify a fully-fetched MB release against the local files before it is
/// allowed to auto-apply: the track counts must match, at least 80% of the
/// local tracks must pair up, and of the pairs where both durations are
/// known, at least 80% must agree within `DURATION_TOLERANCE_SECS`. Albums
/// with no usable durations fall back to count + pairing alone.
pub(super) fn verify_release_against_tracks(
    release: &Value,
    file_tracks: &[TrackSummary],
) -> TrackEvidence {
    let mb_tracks = extract_mb_tracks(release);
    let track_count_match = !file_tracks.is_empty() && mb_tracks.len() == file_tracks.len();
    let pairings = pair_tracks(file_tracks, &mb_tracks);
    let mut duration_checked = 0;
    let mut duration_within = 0;
    for pairing in &pairings {
        let (Some(mb_len), Some(file_len)) = (
            mb_tracks[pairing.mb_index].length_secs,
            file_tracks[pairing.file_index].duration_secs,
        ) else {
            continue;
        };
        duration_checked += 1;
        if (mb_len - file_len).abs() <= DURATION_TOLERANCE_SECS {
            duration_within += 1;
        }
    }
    let paired = pairings.len();
    let pairing_ok = paired * 5 >= file_tracks.len() * 4;
    let duration_ok = duration_checked == 0 || duration_within * 5 >= duration_checked * 4;
    TrackEvidence {
        track_count_match,
        paired,
        duration_checked,
        duration_within,
        pass: track_count_match && pairing_ok && duration_ok,
    }
}

pub(super) fn metabrainz_evidence_for_release(
    release: &Value,
    file_tracks: &[TrackSummary],
) -> MetaBrainzEvidence {
    let mb_tracks = extract_mb_tracks(release);
    let release_status = release
        .get("status")
        .and_then(|v| v.as_str())
        .map(String::from);
    let official = release_status.as_deref() == Some("Official");
    let track_count_match = !file_tracks.is_empty() && mb_tracks.len() == file_tracks.len();
    let local_disc_count = local_disc_count(file_tracks);
    let remote_disc_count = remote_disc_count(release, &mb_tracks);
    let disc_count_match = local_disc_count.map(|local| remote_disc_count == local);
    let pairings = pair_tracks(file_tracks, &mb_tracks);
    let paired_tracks = pairings.len();
    let paired_all = track_count_match && paired_tracks == file_tracks.len();
    let mut duration_checked = 0;
    let mut duration_within = 0;
    let mut title_matched = 0;
    let mut strict_pairs = 0;

    for pairing in &pairings {
        let file = &file_tracks[pairing.file_index];
        let mb = &mb_tracks[pairing.mb_index];
        let (duration_known, duration_ok) = match (file.duration_secs, mb.length_secs) {
            (Some(file_len), Some(mb_len)) => {
                duration_checked += 1;
                let ok = (file_len - mb_len).abs() <= DURATION_TOLERANCE_SECS;
                if ok {
                    duration_within += 1;
                }
                (true, ok)
            }
            _ => (false, false),
        };
        let title_ok = track_title_match_kind(file, mb).is_some();
        if title_ok {
            title_matched += 1;
        }
        if title_ok && (!duration_known || duration_ok) {
            strict_pairs += 1;
        }
    }

    let duration_mismatch = duration_checked > 0 && duration_within < duration_checked;
    let title_mismatch = paired_tracks > 0 && title_matched * 5 < paired_tracks * 4;
    let strict_pairing = paired_all && strict_pairs == file_tracks.len();
    let auto_apply_eligible = official
        && track_count_match
        && disc_count_match != Some(false)
        && strict_pairing
        && !duration_mismatch;

    let mut warnings = Vec::new();
    if !track_count_match {
        warnings.push("Track count mismatch".to_string());
    }
    if disc_count_match == Some(false) {
        warnings.push("Disc count mismatch".to_string());
    }
    if paired_tracks < file_tracks.len() {
        warnings.push("Unmatched local tracks".to_string());
    }
    if paired_tracks < mb_tracks.len() {
        warnings.push("Unmatched MetaBrainz tracks".to_string());
    }
    if duration_mismatch {
        warnings.push("Duration mismatch".to_string());
    }
    if title_mismatch {
        warnings.push("Track title mismatch".to_string());
    }
    if !official {
        warnings.push("Bootleg / review only".to_string());
    }
    if !auto_apply_eligible {
        warnings.push("No safe match found".to_string());
    }

    MetaBrainzEvidence {
        auto_apply_eligible,
        release_status,
        track_count_match,
        disc_count_match,
        paired_tracks,
        local_track_count: file_tracks.len(),
        duration_checked,
        duration_within,
        warnings,
    }
}

fn local_disc_count(file_tracks: &[TrackSummary]) -> Option<usize> {
    let discs: std::collections::HashSet<i64> =
        file_tracks.iter().filter_map(|t| t.disc_number).collect();
    (!discs.is_empty()).then_some(discs.len())
}

fn remote_disc_count(release: &Value, mb_tracks: &[MbTrack]) -> usize {
    if let Some(media) = release.get("media").and_then(|v| v.as_array())
        && !media.is_empty()
    {
        return media.len();
    }
    mb_tracks
        .iter()
        .map(|track| track.disc)
        .collect::<std::collections::HashSet<_>>()
        .len()
        .max(1)
}

/// Result of pairing a file track index to an MB track index.
pub(crate) struct Pairing {
    pub(crate) file_index: usize,
    pub(crate) mb_index: usize,
    pub(crate) kind: &'static str, // "exact" | "fuzzy"
}

pub(super) fn extract_mb_tracks(release: &Value) -> Vec<MbTrack> {
    let mut out = Vec::new();
    let Some(media) = release.get("media").and_then(|v| v.as_array()) else {
        return out;
    };
    for medium in media {
        let disc = medium.get("position").and_then(|v| v.as_i64()).unwrap_or(1);
        let Some(tracks) = medium.get("tracks").and_then(|v| v.as_array()) else {
            continue;
        };
        for t in tracks {
            let position = t
                .get("position")
                .and_then(|v| v.as_i64())
                .or_else(|| {
                    t.get("number")
                        .and_then(|v| v.as_str())
                        .and_then(|s| s.parse::<i64>().ok())
                })
                .unwrap_or(0);
            let title = t
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let artist = mb_track_artist(t);
            // Prefer the recording id (stable across releases) over the track id.
            let recording_id = t
                .get("recording")
                .and_then(|r| r.get("id"))
                .and_then(|v| v.as_str())
                .map(String::from);
            // Track length is in milliseconds; the track-level value reflects
            // this release's edit, the recording length is the fallback.
            let length_secs = t
                .get("length")
                .and_then(|v| v.as_i64())
                .or_else(|| {
                    t.get("recording")
                        .and_then(|r| r.get("length"))
                        .and_then(|v| v.as_i64())
                })
                .map(|ms| ms as f64 / 1000.0);
            out.push(MbTrack {
                recording_id,
                disc,
                position,
                title,
                artist,
                length_secs,
            });
        }
    }
    out
}

fn mb_track_artist(track: &Value) -> Option<String> {
    let credits = track.get("artist-credit").and_then(|v| v.as_array())?;
    // Each credit slot is `{ name?, joinphrase?, artist: { name } }`. MB sometimes
    // duplicates `name` at the top level for display; fall back to the nested
    // artist.name when missing.
    let mut buf = String::new();
    for credit in credits {
        let name = credit
            .get("name")
            .and_then(|v| v.as_str())
            .or_else(|| {
                credit
                    .get("artist")
                    .and_then(|a| a.get("name"))
                    .and_then(|v| v.as_str())
            })
            .unwrap_or("");
        if name.is_empty() {
            continue;
        }
        buf.push_str(name);
        if let Some(join) = credit.get("joinphrase").and_then(|v| v.as_str()) {
            buf.push_str(join);
        }
    }
    let trimmed = buf.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

/// Pair each MB track with a file track. Multi-pass:
///   1. Position match only when title/filename evidence also agrees.
///   2. Strict title match (normalized, `&`<->`and`, `/`/`_` -> space).
///   3. Filename match (strips leading track-digit prefix from the file name).
///   4. Token-overlap match for live/remaster suffixes and small formatting drift.
///   5. Levenshtein-distance match for typo-level differences.
///
/// MB tracks with no match end up in `unmatched_mb_tracks`; file tracks with
/// no match end up in `unmatched_file_tracks` (computed by the caller).
pub(crate) fn pair_tracks(file_tracks: &[TrackSummary], mb_tracks: &[MbTrack]) -> Vec<Pairing> {
    let mut pairings = Vec::with_capacity(mb_tracks.len());
    let mut used = vec![false; file_tracks.len()];

    struct FileKeys {
        title: String,
        filename: String,
    }
    let file_keys: Vec<FileKeys> = file_tracks
        .iter()
        .map(|t| FileKeys {
            title: normalize_for_match(&t.title),
            filename: normalize_for_match(&filename_without_track_prefix(&t.file_name)),
        })
        .collect();

    for (mb_idx, mb) in mb_tracks.iter().enumerate() {
        if let Some((file_idx, _)) = file_tracks.iter().enumerate().find(|(i, t)| {
            !used[*i]
                && t.disc_number.unwrap_or(1) == mb.disc
                && t.track_number == Some(mb.position)
                && track_title_match_kind(t, mb).is_some()
        }) {
            used[file_idx] = true;
            pairings.push(Pairing {
                file_index: file_idx,
                mb_index: mb_idx,
                kind: track_title_match_kind(&file_tracks[file_idx], mb).unwrap_or("fuzzy"),
            });
        }
    }

    fn find_unused<F: FnMut(usize) -> bool>(
        used: &[bool],
        len: usize,
        mut pred: F,
    ) -> Option<usize> {
        (0..len).find(|i| !used[*i] && pred(*i))
    }

    let still_open = |pairings: &[Pairing]| -> std::collections::HashSet<usize> {
        pairings.iter().map(|p| p.mb_index).collect()
    };

    let already = still_open(&pairings);
    let pending: Vec<usize> = (0..mb_tracks.len())
        .filter(|i| !already.contains(i))
        .collect();
    for mb_idx in pending {
        let target = normalize_for_match(&mb_tracks[mb_idx].title);
        if target.is_empty() {
            continue;
        }
        if let Some(file_idx) =
            find_unused(&used, file_keys.len(), |i| file_keys[i].title == target)
        {
            used[file_idx] = true;
            pairings.push(Pairing {
                file_index: file_idx,
                mb_index: mb_idx,
                kind: "fuzzy",
            });
        }
    }

    let already = still_open(&pairings);
    let pending: Vec<usize> = (0..mb_tracks.len())
        .filter(|i| !already.contains(i))
        .collect();
    for mb_idx in pending {
        let target = normalize_for_match(&mb_tracks[mb_idx].title);
        if target.is_empty() {
            continue;
        }
        if let Some(file_idx) =
            find_unused(&used, file_keys.len(), |i| file_keys[i].filename == target)
        {
            used[file_idx] = true;
            pairings.push(Pairing {
                file_index: file_idx,
                mb_index: mb_idx,
                kind: "fuzzy",
            });
        }
    }

    let already = still_open(&pairings);
    let pending: Vec<usize> = (0..mb_tracks.len())
        .filter(|i| !already.contains(i))
        .collect();
    for mb_idx in pending {
        let best = (0..file_keys.len())
            .filter(|i| !used[*i])
            .find(|i| track_title_match_kind(&file_tracks[*i], &mb_tracks[mb_idx]).is_some());
        if let Some(file_idx) = best {
            used[file_idx] = true;
            pairings.push(Pairing {
                file_index: file_idx,
                mb_index: mb_idx,
                kind: "fuzzy",
            });
        }
    }

    let already = still_open(&pairings);
    let pending: Vec<usize> = (0..mb_tracks.len())
        .filter(|i| !already.contains(i))
        .collect();
    for mb_idx in pending {
        let target = normalize_for_match(&mb_tracks[mb_idx].title);
        if target.len() < 4 {
            continue;
        }
        let tolerance = (target.len() / 8).clamp(1, 4);
        let best = (0..file_keys.len())
            .filter(|i| !used[*i])
            .filter_map(|i| {
                let by_title = levenshtein(&file_keys[i].title, &target);
                let by_name = levenshtein(&file_keys[i].filename, &target);
                let d = by_title.min(by_name);
                if d <= tolerance { Some((i, d)) } else { None }
            })
            .min_by_key(|(_, d)| *d);
        if let Some((file_idx, _)) = best {
            used[file_idx] = true;
            pairings.push(Pairing {
                file_index: file_idx,
                mb_index: mb_idx,
                kind: "fuzzy",
            });
        }
    }

    pairings
}

pub(super) fn merge_pairings(
    mut auto: Vec<Pairing>,
    manual: &[ManualPairing],
    file_tracks: &[TrackSummary],
    mb_tracks: &[MbTrack],
) -> Vec<Pairing> {
    for m in manual {
        let Some(file_index) = file_tracks.iter().position(|t| t.id == m.file_track_id) else {
            continue;
        };
        let Some(mb_index) = mb_tracks
            .iter()
            .position(|mb| mb.disc == m.mb_disc && mb.position == m.mb_position)
        else {
            continue;
        };
        auto.retain(|p| p.file_index != file_index && p.mb_index != mb_index);
        auto.push(Pairing {
            file_index,
            mb_index,
            kind: "manual",
        });
    }
    auto
}

pub(crate) fn normalize_for_match(input: &str) -> String {
    let mut spaced = String::with_capacity(input.len());
    let chars: Vec<char> = input.chars().collect();
    for (idx, c) in chars.iter().enumerate() {
        if idx > 0
            && c.is_uppercase()
            && chars[idx - 1].is_lowercase()
            && chars.get(idx + 1).is_some_and(|next| next.is_lowercase())
        {
            spaced.push(' ');
        }
        spaced.push(*c);
    }
    let lowered = spaced.to_lowercase();
    let expanded = lowered.replace('&', " and ");
    expanded
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn track_title_match_kind(file: &TrackSummary, mb: &MbTrack) -> Option<&'static str> {
    let target = normalize_for_match(&mb.title);
    if target.is_empty() {
        return None;
    }
    let local_title = normalize_for_match(&file.title);
    let local_file_name = normalize_for_match(&filename_without_track_prefix(&file.file_name));
    if local_title == target || local_file_name == target {
        return Some("exact");
    }
    if title_tokens_compatible(&local_title, &target)
        || title_tokens_compatible(&local_file_name, &target)
    {
        return Some("fuzzy");
    }
    None
}

fn title_tokens_compatible(local: &str, remote: &str) -> bool {
    let local_tokens = title_tokens(local);
    let remote_tokens = title_tokens(remote);
    if local_tokens.is_empty() || remote_tokens.is_empty() {
        return false;
    }
    let local_set: std::collections::HashSet<&str> =
        local_tokens.iter().map(String::as_str).collect();
    let remote_set: std::collections::HashSet<&str> =
        remote_tokens.iter().map(String::as_str).collect();
    let common = local_set.intersection(&remote_set).count();
    let smaller = local_set.len().min(remote_set.len());
    let larger = local_set.len().max(remote_set.len());

    if smaller == 1 {
        return common == 1 && larger <= 2;
    }
    common >= 2 && common * 2 >= smaller
}

fn title_tokens(value: &str) -> Vec<String> {
    value
        .split_whitespace()
        .filter(|token| !weak_title_token(token))
        .map(str::to_string)
        .collect()
}

fn weak_title_token(token: &str) -> bool {
    matches!(
        token,
        "a" | "an"
            | "and"
            | "at"
            | "bonus"
            | "edit"
            | "for"
            | "from"
            | "in"
            | "live"
            | "mix"
            | "mono"
            | "of"
            | "on"
            | "remaster"
            | "remastered"
            | "stereo"
            | "the"
            | "to"
            | "version"
    )
}

pub(crate) fn filename_without_track_prefix(file_name: &str) -> String {
    let stem = file_name
        .rsplit_once('.')
        .map(|(stem, _)| stem)
        .unwrap_or(file_name);
    let trimmed = stem.trim();
    let without_number = trimmed.trim_start_matches(|c: char| {
        c.is_ascii_digit() || c == '.' || c == '-' || c == '_' || c == ' '
    });
    if without_number.trim().is_empty() {
        trimmed.to_string()
    } else {
        without_number.trim().to_string()
    }
}

pub(crate) fn levenshtein(a: &str, b: &str) -> usize {
    let av: Vec<char> = a.chars().collect();
    let bv: Vec<char> = b.chars().collect();
    if av.is_empty() {
        return bv.len();
    }
    if bv.is_empty() {
        return av.len();
    }
    let (short, long) = if av.len() <= bv.len() {
        (&av, &bv)
    } else {
        (&bv, &av)
    };
    let mut prev: Vec<usize> = (0..=short.len()).collect();
    let mut curr: Vec<usize> = vec![0; short.len() + 1];
    for (i, lc) in long.iter().enumerate() {
        curr[0] = i + 1;
        for (j, sc) in short.iter().enumerate() {
            let cost = if lc == sc { 0 } else { 1 };
            curr[j + 1] = (curr[j] + 1).min(prev[j + 1] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[short.len()]
}

pub(super) fn build_candidate_preview(
    candidate: MatchCandidate,
    album: AlbumSummary,
    file_tracks: Vec<TrackSummary>,
    release: Value,
) -> CandidatePreview {
    let mb_title = release
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown Album")
        .to_string();
    let mb_artist = release_artist(&release);
    let mb_year = release
        .get("date")
        .and_then(|v| v.as_str())
        .and_then(parse_year);
    let mb_release_group_id = release
        .get("release-group")
        .and_then(|rg| rg.get("id"))
        .and_then(|v| v.as_str())
        .map(String::from);
    let mb_barcode = release
        .get("barcode")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);
    let country = release
        .get("country")
        .and_then(|v| v.as_str())
        .map(String::from);
    let date = release
        .get("date")
        .and_then(|v| v.as_str())
        .map(String::from);

    let album_preview = AlbumPreview {
        title: FieldDiff::new(album.title.clone(), mb_title),
        album_artist: FieldDiff::new(album.album_artist.clone(), mb_artist),
        year: FieldDiff::new(album.year, mb_year),
        mb_release_id: candidate.provider_id.clone(),
        mb_release_group_id,
        mb_barcode,
        country,
        date,
    };

    let mb_tracks = extract_mb_tracks(&release);
    let pairings = pair_tracks(&file_tracks, &mb_tracks);
    let paired_mb_indices: std::collections::HashSet<usize> =
        pairings.iter().map(|p| p.mb_index).collect();
    let paired_file_indices: std::collections::HashSet<usize> =
        pairings.iter().map(|p| p.file_index).collect();

    let mut tracks: Vec<TrackPreview> = pairings
        .iter()
        .map(|p| {
            let mb = &mb_tracks[p.mb_index];
            let ft = &file_tracks[p.file_index];
            TrackPreview {
                file_track_id: ft.id,
                mb_disc: mb.disc,
                mb_position: mb.position,
                mb_recording_id: mb.recording_id.clone(),
                title: FieldDiff::new(ft.title.clone(), mb.title.clone()),
                artist: FieldDiff::new(ft.artist.clone(), mb.artist.clone()),
                track_number: FieldDiff::new(ft.track_number, Some(mb.position)),
                disc_number: FieldDiff::new(ft.disc_number, Some(mb.disc)),
                match_kind: p.kind.to_string(),
            }
        })
        .collect();
    tracks.sort_by_key(|track| (track.mb_disc, track.mb_position));

    let unmatched_file_tracks: Vec<TrackSummary> = file_tracks
        .iter()
        .enumerate()
        .filter(|(i, _)| !paired_file_indices.contains(i))
        .map(|(_, t)| t.clone())
        .collect();

    let unmatched_mb_tracks: Vec<MbTrack> = mb_tracks
        .into_iter()
        .enumerate()
        .filter(|(i, _)| !paired_mb_indices.contains(i))
        .map(|(_, t)| t)
        .collect();

    CandidatePreview {
        candidate,
        album: album_preview,
        tracks,
        unmatched_file_tracks,
        unmatched_mb_tracks,
    }
}

pub(super) fn parse_year(value: &str) -> Option<i32> {
    let digits: String = value.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.len() >= 4 {
        digits[..4].parse::<i32>().ok()
    } else {
        None
    }
}
