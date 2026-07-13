use crate::app::state::AppState;
use crate::library::{ResolvedPlaySource, TrackSummary, normalize_library_match_key};
use crate::playback::source::source_ref_with_radio;
use crate::playback::status::build_status_response_for_zone;
use crate::protocol::{RadioContext, RadioSeedContext, SourceRef};
use crate::services::lastfm::{LastFmSeed, LastFmTrack};
use crate::services::qobuz::QobuzTrack;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};

const LASTFM_RADIO_PROVIDER: &str = "lastfm";
const RADIO_ARTIST_COOLDOWN_PICKS: usize = 4;
const RADIO_TITLE_COOLDOWN_PICKS: usize = 20;
const INITIAL_ANCHOR_WEIGHT: f64 = 0.8;
const ANCHOR_WEIGHT_DECAY_PER_HOP: f64 = 0.05;
const MIN_ANCHOR_WEIGHT: f64 = 0.2;
const LOCAL_SOURCE_SCORE_MULTIPLIER: f64 = 0.9;
const QOBUZ_SOURCE_SCORE_MULTIPLIER: f64 = 1.08;
const PLAY_COUNT_BONUS_PER_PLAY: f64 = 0.02;
const PLAY_COUNT_BONUS_CAP: f64 = 0.15;
const LISTENED_HOUR_BONUS_PER_HOUR: f64 = 0.01;
const LISTENED_HOUR_BONUS_CAP: f64 = 0.05;
const RECENT_PLAY_PENALTY_MAX: f64 = -0.25;
const RECENT_PLAY_PENALTY_WINDOW_SECS: i64 = 14 * 24 * 60 * 60;
const CURRENT_ARTIST_REPEAT_PENALTY: f64 = -0.8;
const RECENT_ARTIST_REPEAT_PENALTIES: [f64; RADIO_ARTIST_COOLDOWN_PICKS] = [-0.8, -0.4, -0.2, -0.1];

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub(crate) struct LastFmSeedContext {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub artist: Option<String>,
    #[serde(default)]
    pub album: Option<String>,
    #[serde(default)]
    pub album_artist: Option<String>,
    #[serde(default)]
    pub local_album_id: Option<i64>,
    #[serde(default)]
    pub qobuz_album_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct LastFmResolvedCandidate {
    pub lastfm_track: LastFmTrack,
    pub local_match: Option<TrackSummary>,
    pub qobuz_match: Option<QobuzTrack>,
    pub qobuz_checked: bool,
    pub match_status: String,
    pub selected_source: Option<ResolvedPlaySource>,
    pub radio_score: f64,
    pub selection_score: f64,
    pub score_breakdown: LastFmCandidateScoreBreakdown,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct LastFmCandidateScoreBreakdown {
    pub base_radio_score: f64,
    pub source_multiplier: f64,
    pub favorite_bonus: f64,
    pub recency_penalty: f64,
    pub artist_penalty: f64,
    pub final_score: f64,
}

#[derive(Debug, Serialize)]
pub(crate) struct LastFmRadioResolution {
    pub seed: LastFmSeed,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub radio_context: Option<RadioContext>,
    pub candidates: Vec<LastFmResolvedCandidate>,
    pub best: Option<LastFmResolvedCandidate>,
    pub partial_errors: Vec<String>,
}

#[derive(Debug, Clone)]
struct LastFmRadioSession {
    anchor: LastFmSeed,
    current: LastFmSeed,
    context: LastFmSeedContext,
    radio_context: RadioContext,
}

#[derive(Debug, Clone)]
struct RankedLastFmTrack {
    track: LastFmTrack,
    score: f64,
    anchor_rank: Option<usize>,
    current_rank: Option<usize>,
}

#[derive(Debug, Clone, Default)]
struct RadioGuardrails {
    current_artist: Option<String>,
    current_title_key: Option<String>,
    recent_radio_artists: Vec<String>,
    recent_title_keys: HashSet<String>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct LastFmResolveOptions {
    pub limit: u32,
    pub resolve_limit: usize,
    pub qobuz_resolve_limit: usize,
}

impl LastFmResolveOptions {
    pub(crate) fn new(limit: u32, resolve_limit: u32, qobuz_resolve_limit: u32) -> Self {
        Self {
            limit: limit.clamp(1, 50),
            resolve_limit: resolve_limit.clamp(1, 50) as usize,
            qobuz_resolve_limit: qobuz_resolve_limit.clamp(0, 25) as usize,
        }
    }
}

impl Default for LastFmResolveOptions {
    fn default() -> Self {
        Self::new(30, 30, 12)
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct RadioExclusions {
    source_keys: HashSet<String>,
    local_track_ids: HashSet<i64>,
    qobuz_track_ids: HashSet<u64>,
    local_album_ids: HashSet<i64>,
    qobuz_album_ids: HashSet<String>,
    album_artist_keys: HashSet<String>,
}

impl RadioExclusions {
    fn for_zone(state: &AppState, zone_id: &str, active_source: Option<&SourceRef>) -> Self {
        let mut exclusions = Self::default();
        if let Some(active_source) = active_source {
            exclusions.add_source_with_state(state, active_source);
        }
        if let Ok(queue) = state.library().zone_queue(zone_id) {
            for entry in queue {
                exclusions.add_source_with_state(state, &entry.source);
            }
        }
        let live = state.listening().active_history_inputs();
        let profile_id = state
            .listening()
            .profile_id(zone_id)
            .unwrap_or_else(|| crate::settings::DEFAULT_PROFILE_ID.to_string());
        if let Ok(recent) = state
            .library()
            .recent_playback_history_with_live_for_profile(&profile_id, 100, &live, true)
        {
            for entry in recent {
                exclusions.add_source_with_state(state, &entry.source);
            }
        }
        exclusions
    }

    fn add_source_with_state(&mut self, state: &AppState, source: &SourceRef) {
        self.add_source(source);
        if let SourceRef::LocalTrack {
            album_id: Some(album_id),
            ..
        } = source
            && let Ok(Some(qobuz_album_id)) =
                state.library().qobuz_album_id_for_local_album(*album_id)
        {
            self.add_qobuz_album_id(&qobuz_album_id);
        }
    }

    fn add_source(&mut self, source: &SourceRef) {
        self.source_keys.insert(source.key());
        match source {
            SourceRef::LocalTrack {
                track_id,
                album,
                album_artist,
                artist,
                album_id,
                ..
            } => {
                if *track_id > 0 {
                    self.local_track_ids.insert(*track_id);
                }
                if let Some(album_id) = album_id {
                    self.local_album_ids.insert(*album_id);
                }
                self.add_album_artist_key(
                    album.as_deref(),
                    album_artist.as_deref().or(artist.as_deref()),
                );
            }
            SourceRef::QobuzTrack {
                track_id,
                album,
                artist,
                album_id,
                ..
            } => {
                if *track_id > 0 {
                    self.qobuz_track_ids.insert(*track_id);
                }
                if let Some(album_id) = album_id {
                    self.add_qobuz_album_id(album_id);
                }
                self.add_album_artist_key(album.as_deref(), artist.as_deref());
            }
        }
    }

    fn contains_local(&self, track: &TrackSummary) -> bool {
        self.local_track_ids.contains(&track.id)
            || self.source_keys.contains(&format!("local:{}", track.id))
            || track
                .album_id
                .is_some_and(|album_id| self.local_album_ids.contains(&album_id))
            || self.contains_album_artist(
                track.album.as_deref(),
                track.album_artist.as_deref().or(track.artist.as_deref()),
            )
    }

    fn contains_qobuz(&self, track: &QobuzTrack) -> bool {
        self.qobuz_track_ids.contains(&track.id)
            || self.source_keys.contains(&format!("qobuz:{}", track.id))
            || track
                .album_id
                .as_deref()
                .is_some_and(|album_id| self.contains_qobuz_album_id(album_id))
            || self.contains_album_artist(Some(&track.album), Some(&track.artist))
    }

    fn add_qobuz_album_id(&mut self, album_id: &str) {
        if let Some(album_id) = normalized_qobuz_album_id(album_id) {
            self.qobuz_album_ids.insert(album_id);
        }
    }

    fn contains_qobuz_album_id(&self, album_id: &str) -> bool {
        normalized_qobuz_album_id(album_id)
            .is_some_and(|album_id| self.qobuz_album_ids.contains(&album_id))
    }

    fn add_album_artist_key(&mut self, album: Option<&str>, artist: Option<&str>) {
        if let Some(key) = album_artist_key(album, artist) {
            self.album_artist_keys.insert(key);
        }
    }

    fn contains_album_artist(&self, album: Option<&str>, artist: Option<&str>) -> bool {
        album_artist_key(album, artist).is_some_and(|key| self.album_artist_keys.contains(&key))
    }
}

pub(crate) async fn lastfm_radio_next_source_for_zone(
    state: AppState,
    zone_id: &str,
) -> Result<Option<SourceRef>, String> {
    if !state.settings().lastfm_radio_enabled() {
        return Ok(None);
    }
    let Some(active_source) = state.listening().active_source(zone_id) else {
        return Ok(None);
    };
    if lastfm_radio_has_future_queue(&state, zone_id, &active_source) {
        return Ok(None);
    }
    lastfm_radio_next_source_from_source_for_zone(state, zone_id, active_source).await
}

pub(crate) async fn lastfm_radio_next_source_from_source_for_zone(
    state: AppState,
    zone_id: &str,
    active_source: SourceRef,
) -> Result<Option<SourceRef>, String> {
    if !state.settings().lastfm_radio_enabled() {
        return Ok(None);
    }
    if lastfm_radio_has_future_queue(&state, zone_id, &active_source) {
        return Ok(None);
    }
    let session = radio_session_from_source(&state, zone_id, &active_source)?;
    let exclusions = RadioExclusions::for_zone(&state, zone_id, Some(&active_source));
    let guardrails = RadioGuardrails {
        current_artist: source_artist(&active_source),
        current_title_key: source_title(&active_source)
            .as_deref()
            .and_then(canonical_song_title_key),
        recent_radio_artists: recent_radio_artists(&state, zone_id),
        recent_title_keys: recent_title_keys(&state, zone_id),
    };
    let resolution = resolve_lastfm_radio_session(
        &state,
        session.anchor,
        session.current,
        session.context,
        Some(session.radio_context.clone()),
        LastFmResolveOptions::default(),
        exclusions,
        guardrails,
    )
    .await?;
    Ok(resolution
        .best
        .as_ref()
        .and_then(|candidate| candidate_source_ref(candidate, resolution.radio_context.clone())))
}

pub(crate) fn lastfm_radio_has_future_queue(
    state: &AppState,
    zone_id: &str,
    active_source: &SourceRef,
) -> bool {
    state
        .library()
        .zone_queue(zone_id)
        .ok()
        .is_some_and(|queue| {
            queue
                .iter()
                .any(|entry| entry.source.key() != active_source.key())
        })
}

fn radio_session_from_source(
    state: &AppState,
    zone_id: &str,
    source: &SourceRef,
) -> Result<LastFmRadioSession, String> {
    let current = seed_from_status_and_source(state, zone_id, Some(source))?;
    let context = context_from_status_and_source(state, zone_id, Some(source));
    let existing = source_radio_context(source)
        .filter(|context| context.provider.eq_ignore_ascii_case(LASTFM_RADIO_PROVIDER));
    let anchor = existing
        .and_then(|context| lastfm_seed_from_radio_seed(&context.anchor).ok())
        .unwrap_or_else(|| current.clone());
    let hop = existing
        .map(|context| context.hop.saturating_add(1))
        .unwrap_or(0);
    let radio_context = RadioContext {
        provider: LASTFM_RADIO_PROVIDER.to_string(),
        anchor: radio_seed_from_lastfm_seed(&anchor),
        last_seed: radio_seed_from_lastfm_seed(&current),
        hop,
    };
    Ok(LastFmRadioSession {
        anchor,
        current,
        context,
        radio_context,
    })
}

fn source_radio_context(source: &SourceRef) -> Option<&RadioContext> {
    match source {
        SourceRef::LocalTrack { radio_context, .. }
        | SourceRef::QobuzTrack { radio_context, .. } => radio_context.as_ref(),
    }
}

fn lastfm_seed_from_radio_seed(seed: &RadioSeedContext) -> Result<LastFmSeed, String> {
    LastFmSeed {
        title: seed.title.clone(),
        artist: seed.artist.clone(),
        mbid: seed.mbid.clone(),
    }
    .normalized()
}

fn radio_seed_from_lastfm_seed(seed: &LastFmSeed) -> RadioSeedContext {
    RadioSeedContext {
        title: seed.title.clone(),
        artist: seed.artist.clone(),
        mbid: seed.mbid.clone(),
    }
}

fn recent_radio_artists(state: &AppState, zone_id: &str) -> Vec<String> {
    let live = state.listening().active_history_inputs();
    let profile_id = state
        .listening()
        .profile_id(zone_id)
        .unwrap_or_else(|| crate::settings::DEFAULT_PROFILE_ID.to_string());
    state
        .library()
        .recent_playback_history_with_live_for_profile(&profile_id, 20, &live, true)
        .ok()
        .into_iter()
        .flatten()
        .filter(|entry| entry.radio || entry.source.is_radio())
        .filter_map(|entry| {
            entry
                .artist
                .or_else(|| source_artist(&entry.source))
                .and_then(|artist| normalize_seed_field(Some(&artist)))
        })
        .take(RADIO_ARTIST_COOLDOWN_PICKS)
        .collect()
}

fn recent_title_keys(state: &AppState, zone_id: &str) -> HashSet<String> {
    let live = state.listening().active_history_inputs();
    let profile_id = state
        .listening()
        .profile_id(zone_id)
        .unwrap_or_else(|| crate::settings::DEFAULT_PROFILE_ID.to_string());
    state
        .library()
        .recent_playback_history_with_live_for_profile(
            &profile_id,
            RADIO_TITLE_COOLDOWN_PICKS as i64,
            &live,
            true,
        )
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            entry
                .title
                .or_else(|| source_title(&entry.source))
                .and_then(|title| canonical_song_title_key(&title))
        })
        .collect()
}

pub(crate) async fn resolve_lastfm_radio(
    state: &AppState,
    seed: LastFmSeed,
    context: LastFmSeedContext,
    options: LastFmResolveOptions,
    exclusions: RadioExclusions,
) -> Result<LastFmRadioResolution, String> {
    resolve_lastfm_radio_session(
        state,
        seed.clone(),
        seed,
        context,
        None,
        options,
        exclusions,
        RadioGuardrails::default(),
    )
    .await
}

pub(crate) async fn resolve_lastfm_radio_with_context(
    state: &AppState,
    current_seed: LastFmSeed,
    context: LastFmSeedContext,
    radio_context: Option<RadioContext>,
    options: LastFmResolveOptions,
    exclusions: RadioExclusions,
) -> Result<LastFmRadioResolution, String> {
    let anchor_seed = radio_context
        .as_ref()
        .filter(|context| context.provider.eq_ignore_ascii_case(LASTFM_RADIO_PROVIDER))
        .and_then(|context| lastfm_seed_from_radio_seed(&context.anchor).ok())
        .unwrap_or_else(|| current_seed.clone());
    resolve_lastfm_radio_session(
        state,
        anchor_seed,
        current_seed,
        context,
        radio_context,
        options,
        exclusions,
        RadioGuardrails::default(),
    )
    .await
}

// Last.fm radio resolution keeps seed context, options, exclusions, and guardrails explicit.
#[allow(clippy::too_many_arguments)]
async fn resolve_lastfm_radio_session(
    state: &AppState,
    anchor_seed: LastFmSeed,
    current_seed: LastFmSeed,
    context: LastFmSeedContext,
    radio_context: Option<RadioContext>,
    options: LastFmResolveOptions,
    exclusions: RadioExclusions,
    guardrails: RadioGuardrails,
) -> Result<LastFmRadioResolution, String> {
    let api_key = state
        .lastfm_api_key()
        .ok_or_else(|| "Last.fm API key is not configured".to_string())?;
    let anchor_similar = state
        .lastfm()
        .similar_tracks(&api_key, &anchor_seed, options.limit)
        .await?;
    let current_similar = if same_seed(&anchor_seed, &current_seed) {
        None
    } else {
        match state
            .lastfm()
            .similar_tracks(&api_key, &current_seed, options.limit)
            .await
        {
            Ok(similar) => Some(similar.tracks),
            Err(err) => {
                eprintln!("lastfm: current-seed radio lookup failed: {err}");
                None
            }
        }
    };
    let mut partial_errors = Vec::new();
    let mut candidates = Vec::new();
    let current_tracks = current_similar.unwrap_or_default();
    let hop = radio_context
        .as_ref()
        .map(|context| context.hop)
        .unwrap_or(0);
    let ranked_tracks = hybrid_ranked_tracks(
        anchor_similar.tracks,
        current_tracks,
        options.limit as usize,
        hop,
    );

    for (index, ranked_track) in ranked_tracks.into_iter().enumerate() {
        let should_resolve = index < options.resolve_limit;
        let local_match = if should_resolve {
            match state
                .library()
                .find_track_by_title_artist(&ranked_track.track.title, &ranked_track.track.artist)
            {
                Ok(track) => track,
                Err(err) => {
                    partial_errors.push(format!(
                        "Local match failed for {} - {}: {err}",
                        ranked_track.track.artist, ranked_track.track.title
                    ));
                    None
                }
            }
        } else {
            None
        };
        let primary_qobuz_match = match local_match.as_ref() {
            Some(track) => match state
                .library()
                .primary_qobuz_track_for_local_track(track.id)
            {
                Ok(Some(qobuz)) => Some(qobuz_match_with_local_playback_stats(qobuz, track)),
                Ok(None) => None,
                Err(err) => {
                    partial_errors.push(format!(
                        "Primary Qobuz lookup failed for {} - {}: {err}",
                        ranked_track.track.artist, ranked_track.track.title
                    ));
                    None
                }
            },
            None => None,
        };
        let qobuz_checked = primary_qobuz_match.is_some();

        candidates.push(resolved_candidate(
            ranked_track.track,
            local_match,
            primary_qobuz_match,
            &context,
            &exclusions,
            qobuz_checked,
            ranked_track.score,
        ));
    }

    if options.qobuz_resolve_limit > 0 {
        let qobuz_candidate_indexes: Vec<usize> = candidates
            .iter()
            .enumerate()
            .filter(|(_, candidate)| should_try_exact_qobuz_match(candidate))
            .take(options.qobuz_resolve_limit)
            .map(|(index, _)| index)
            .collect();
        for index in qobuz_candidate_indexes {
            let mut qobuz_match = find_exact_qobuz_match(
                state,
                &candidates[index].lastfm_track,
                &context,
                &exclusions,
                &mut partial_errors,
                options.qobuz_resolve_limit,
            )
            .await;
            let lastfm_track = candidates[index].lastfm_track.clone();
            let local_match = candidates[index].local_match.clone();
            if let Some(local) = local_match.as_ref() {
                qobuz_match =
                    qobuz_match.map(|qobuz| qobuz_match_with_local_playback_stats(qobuz, local));
            }
            let radio_score = candidates[index].radio_score;
            candidates[index] = resolved_candidate(
                lastfm_track,
                local_match,
                qobuz_match,
                &context,
                &exclusions,
                true,
                radio_score,
            );
        }
    }
    let now_secs = current_unix_secs();
    apply_candidate_selection_scores_at(&mut candidates, &guardrails, now_secs);
    let best = best_playable_candidate_at(&candidates, &guardrails, now_secs);

    Ok(LastFmRadioResolution {
        seed: anchor_similar.seed,
        radio_context,
        candidates,
        best,
        partial_errors,
    })
}

pub(crate) fn seed_context_from_current_status(
    state: &AppState,
    zone_id: &str,
) -> LastFmSeedContext {
    context_from_status_and_source(
        state,
        zone_id,
        state.listening().active_source(zone_id).as_ref(),
    )
}

pub(crate) fn seed_from_current_status(state: &AppState, zone_id: &str) -> Option<LastFmSeed> {
    seed_from_status_and_source(
        state,
        zone_id,
        state.listening().active_source(zone_id).as_ref(),
    )
    .ok()
}

pub(crate) fn merge_seed_context(
    mut fallback: LastFmSeedContext,
    context: Option<LastFmSeedContext>,
) -> LastFmSeedContext {
    if let Some(context) = context {
        fallback.title = normalize_seed_field(context.title.as_deref()).or(fallback.title);
        fallback.artist = normalize_seed_field(context.artist.as_deref()).or(fallback.artist);
        fallback.album = normalize_seed_field(context.album.as_deref()).or(fallback.album);
        fallback.album_artist =
            normalize_seed_field(context.album_artist.as_deref()).or(fallback.album_artist);
        fallback.local_album_id = context.local_album_id.or(fallback.local_album_id);
        fallback.qobuz_album_id =
            normalize_seed_field(context.qobuz_album_id.as_deref()).or(fallback.qobuz_album_id);
    }
    fallback
}

fn seed_from_status_and_source(
    state: &AppState,
    zone_id: &str,
    source: Option<&SourceRef>,
) -> Result<LastFmSeed, String> {
    let status = build_status_response_for_zone(state, zone_id).ok();
    let title = status
        .as_ref()
        .and_then(|status| normalize_seed_field(status.track_title.as_deref()))
        .or_else(|| source.and_then(source_title));
    let artist = status
        .as_ref()
        .and_then(|status| normalize_seed_field(status.track_artist.as_deref()))
        .or_else(|| source.and_then(source_artist));
    LastFmSeed {
        title,
        artist,
        mbid: None,
    }
    .normalized()
}

fn context_from_status_and_source(
    state: &AppState,
    zone_id: &str,
    source: Option<&SourceRef>,
) -> LastFmSeedContext {
    let status = build_status_response_for_zone(state, zone_id).ok();
    LastFmSeedContext {
        title: status
            .as_ref()
            .and_then(|status| normalize_seed_field(status.track_title.as_deref()))
            .or_else(|| source.and_then(source_title)),
        artist: status
            .as_ref()
            .and_then(|status| normalize_seed_field(status.track_artist.as_deref()))
            .or_else(|| source.and_then(source_artist)),
        album: status
            .as_ref()
            .and_then(|status| normalize_seed_field(status.track_album.as_deref()))
            .or_else(|| source.and_then(source_album)),
        album_artist: source.and_then(source_album_artist),
        local_album_id: source.and_then(source_local_album_id),
        qobuz_album_id: source.and_then(source_qobuz_album_id),
    }
}

fn source_title(source: &SourceRef) -> Option<String> {
    match source {
        SourceRef::LocalTrack { title, .. } | SourceRef::QobuzTrack { title, .. } => {
            normalize_seed_field(title.as_deref())
        }
    }
}

fn source_album(source: &SourceRef) -> Option<String> {
    match source {
        SourceRef::LocalTrack { album, .. } | SourceRef::QobuzTrack { album, .. } => {
            normalize_seed_field(album.as_deref())
        }
    }
}

fn source_album_artist(source: &SourceRef) -> Option<String> {
    match source {
        SourceRef::LocalTrack { album_artist, .. } => normalize_seed_field(album_artist.as_deref()),
        SourceRef::QobuzTrack { artist, .. } => normalize_seed_field(artist.as_deref()),
    }
}

fn source_local_album_id(source: &SourceRef) -> Option<i64> {
    match source {
        SourceRef::LocalTrack { album_id, .. } => *album_id,
        SourceRef::QobuzTrack { .. } => None,
    }
}

fn source_qobuz_album_id(source: &SourceRef) -> Option<String> {
    match source {
        SourceRef::QobuzTrack { album_id, .. } => normalize_seed_field(album_id.as_deref()),
        SourceRef::LocalTrack { .. } => None,
    }
}

fn source_artist(source: &SourceRef) -> Option<String> {
    match source {
        SourceRef::LocalTrack {
            artist,
            album_artist,
            ..
        } => normalize_seed_field(artist.as_deref())
            .or_else(|| normalize_seed_field(album_artist.as_deref())),
        SourceRef::QobuzTrack { artist, .. } => normalize_seed_field(artist.as_deref()),
    }
}

fn normalize_seed_field(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn resolved_candidate(
    lastfm_track: LastFmTrack,
    local_match: Option<TrackSummary>,
    qobuz_match: Option<QobuzTrack>,
    context: &LastFmSeedContext,
    exclusions: &RadioExclusions,
    qobuz_checked: bool,
    radio_score: f64,
) -> LastFmResolvedCandidate {
    let same_seed_track = is_seed_track(&lastfm_track, context);
    let same_album = local_match
        .as_ref()
        .is_some_and(|track| local_track_same_album(track, context))
        || qobuz_match
            .as_ref()
            .is_some_and(|track| qobuz_track_same_album(track, context));
    let excluded = local_match
        .as_ref()
        .is_some_and(|track| exclusions.contains_local(track))
        || qobuz_match
            .as_ref()
            .is_some_and(|track| exclusions.contains_qobuz(track));
    let playable = !same_seed_track && !same_album && !excluded;
    let selected_source = if playable {
        qobuz_match
            .as_ref()
            .map(qobuz_source_from_track)
            .or_else(|| local_match.as_ref().map(local_source_from_track))
    } else {
        None
    };
    let match_status = if same_seed_track {
        "seed_track"
    } else if same_album {
        "same_album"
    } else if excluded {
        "excluded"
    } else if qobuz_match.is_some() {
        "qobuz"
    } else if local_match.is_some() {
        "local"
    } else {
        "unmatched"
    }
    .to_string();

    LastFmResolvedCandidate {
        lastfm_track,
        local_match,
        qobuz_match,
        qobuz_checked,
        match_status,
        selected_source,
        radio_score,
        selection_score: radio_score,
        score_breakdown: LastFmCandidateScoreBreakdown::unscored(radio_score),
    }
}

#[cfg(test)]
fn first_playable_candidate(
    candidates: &[LastFmResolvedCandidate],
) -> Option<LastFmResolvedCandidate> {
    best_playable_candidate(candidates, &RadioGuardrails::default())
}

#[cfg(test)]
fn best_playable_candidate(
    candidates: &[LastFmResolvedCandidate],
    guardrails: &RadioGuardrails,
) -> Option<LastFmResolvedCandidate> {
    best_playable_candidate_at(candidates, guardrails, current_unix_secs())
}

fn best_playable_candidate_at(
    candidates: &[LastFmResolvedCandidate],
    guardrails: &RadioGuardrails,
    now_secs: i64,
) -> Option<LastFmResolvedCandidate> {
    candidates
        .iter()
        .filter(|candidate| matches!(candidate.match_status.as_str(), "local" | "qobuz"))
        .filter(|candidate| !guardrails.blocks_title(&candidate.lastfm_track.title))
        .cloned()
        .max_by(|left, right| {
            candidate_selection_score_at(left, guardrails, now_secs)
                .partial_cmp(&candidate_selection_score_at(right, guardrails, now_secs))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
}

fn should_try_exact_qobuz_match(candidate: &LastFmResolvedCandidate) -> bool {
    candidate.qobuz_match.is_none()
        && matches!(candidate.match_status.as_str(), "local" | "unmatched")
}

fn apply_candidate_selection_scores_at(
    candidates: &mut [LastFmResolvedCandidate],
    guardrails: &RadioGuardrails,
    now_secs: i64,
) {
    for candidate in candidates {
        let breakdown = candidate_score_breakdown_at(candidate, guardrails, now_secs);
        candidate.selection_score = breakdown.final_score;
        candidate.score_breakdown = breakdown;
    }
}

fn candidate_selection_score_at(
    candidate: &LastFmResolvedCandidate,
    guardrails: &RadioGuardrails,
    now_secs: i64,
) -> f64 {
    candidate_score_breakdown_at(candidate, guardrails, now_secs).final_score
}

fn candidate_score_breakdown_at(
    candidate: &LastFmResolvedCandidate,
    guardrails: &RadioGuardrails,
    now_secs: i64,
) -> LastFmCandidateScoreBreakdown {
    let source_multiplier = match candidate.match_status.as_str() {
        "qobuz" => QOBUZ_SOURCE_SCORE_MULTIPLIER,
        "local" => LOCAL_SOURCE_SCORE_MULTIPLIER,
        _ => 1.0,
    };
    let stats = candidate_playback_stats(candidate);
    let favorite_bonus = favorite_bonus(stats.play_count, stats.listened_secs);
    let recency_penalty = recency_penalty(stats.last_played_at, now_secs);
    let artist_penalty = guardrails.artist_repeat_penalty(&candidate.lastfm_track.artist);
    let final_score = (candidate.radio_score * source_multiplier)
        + favorite_bonus
        + recency_penalty
        + artist_penalty;

    LastFmCandidateScoreBreakdown {
        base_radio_score: candidate.radio_score,
        source_multiplier,
        favorite_bonus,
        recency_penalty,
        artist_penalty,
        final_score,
    }
}

impl LastFmCandidateScoreBreakdown {
    fn unscored(radio_score: f64) -> Self {
        Self {
            base_radio_score: radio_score,
            source_multiplier: 1.0,
            favorite_bonus: 0.0,
            recency_penalty: 0.0,
            artist_penalty: 0.0,
            final_score: radio_score,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct CandidatePlaybackStats {
    play_count: i64,
    last_played_at: Option<i64>,
    listened_secs: f64,
}

fn candidate_playback_stats(candidate: &LastFmResolvedCandidate) -> CandidatePlaybackStats {
    if let Some(track) = candidate.local_match.as_ref() {
        return CandidatePlaybackStats {
            play_count: track.play_count,
            last_played_at: track.last_played_at,
            listened_secs: track.listened_secs,
        };
    }

    candidate
        .qobuz_match
        .as_ref()
        .map(|track| CandidatePlaybackStats {
            play_count: track.play_count,
            last_played_at: track.last_played_at,
            listened_secs: track.listened_secs,
        })
        .unwrap_or_default()
}

fn favorite_bonus(play_count: i64, listened_secs: f64) -> f64 {
    let play_bonus =
        ((play_count.max(0) as f64) * PLAY_COUNT_BONUS_PER_PLAY).min(PLAY_COUNT_BONUS_CAP);
    let listened_hours = (listened_secs.max(0.0) / 3600.0).max(0.0);
    let listened_bonus =
        (listened_hours * LISTENED_HOUR_BONUS_PER_HOUR).min(LISTENED_HOUR_BONUS_CAP);
    play_bonus + listened_bonus
}

fn recency_penalty(last_played_at: Option<i64>, now_secs: i64) -> f64 {
    let Some(last_played_at) = last_played_at else {
        return 0.0;
    };
    let age_secs = now_secs.saturating_sub(last_played_at).max(0);
    if age_secs >= RECENT_PLAY_PENALTY_WINDOW_SECS {
        return 0.0;
    }
    let remaining = 1.0 - (age_secs as f64 / RECENT_PLAY_PENALTY_WINDOW_SECS as f64);
    RECENT_PLAY_PENALTY_MAX * remaining
}

impl RadioGuardrails {
    fn blocks_title(&self, title: &str) -> bool {
        let Some(key) = canonical_song_title_key(title) else {
            return false;
        };
        self.current_title_key.as_ref() == Some(&key) || self.recent_title_keys.contains(&key)
    }

    fn artist_repeat_penalty(&self, artist: &str) -> f64 {
        if same_optional_artist(Some(artist), self.current_artist.as_deref()) {
            return CURRENT_ARTIST_REPEAT_PENALTY;
        }
        self.recent_radio_artists
            .iter()
            .position(|recent| same_optional_artist(Some(artist), Some(recent)))
            .and_then(|index| RECENT_ARTIST_REPEAT_PENALTIES.get(index).copied())
            .unwrap_or(0.0)
    }
}

fn hybrid_ranked_tracks(
    anchor_tracks: Vec<LastFmTrack>,
    current_tracks: Vec<LastFmTrack>,
    limit: usize,
    hop: u32,
) -> Vec<RankedLastFmTrack> {
    let mut merged: HashMap<String, RankedLastFmTrack> = HashMap::new();
    let has_current_seed = !current_tracks.is_empty();
    let (anchor_weight, current_weight) = radio_blend_weights(hop, has_current_seed);
    merge_ranked_side(&mut merged, anchor_tracks, anchor_weight, true, limit);
    merge_ranked_side(&mut merged, current_tracks, current_weight, false, limit);
    let mut tracks = merged.into_values().collect::<Vec<_>>();
    tracks.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                left.anchor_rank
                    .unwrap_or(usize::MAX)
                    .cmp(&right.anchor_rank.unwrap_or(usize::MAX))
            })
            .then_with(|| {
                left.current_rank
                    .unwrap_or(usize::MAX)
                    .cmp(&right.current_rank.unwrap_or(usize::MAX))
            })
    });
    tracks
}

fn radio_blend_weights(hop: u32, has_current_seed: bool) -> (f64, f64) {
    if !has_current_seed {
        return (1.0, 0.0);
    }
    let anchor_weight =
        (INITIAL_ANCHOR_WEIGHT - (hop as f64 * ANCHOR_WEIGHT_DECAY_PER_HOP)).max(MIN_ANCHOR_WEIGHT);
    (anchor_weight, 1.0 - anchor_weight)
}

fn merge_ranked_side(
    merged: &mut HashMap<String, RankedLastFmTrack>,
    tracks: Vec<LastFmTrack>,
    side_weight: f64,
    anchor_side: bool,
    limit: usize,
) {
    let denominator = limit.max(tracks.len()).max(1) as f64;
    for (index, track) in tracks.into_iter().enumerate() {
        let key = lastfm_track_key(&track);
        let rank_score = ((denominator - index as f64) / denominator).clamp(0.0, 1.0);
        let match_score = normalized_match_score(track.match_score);
        let side_score = (rank_score * 0.75) + (match_score * 0.25);
        let weighted_score = side_score * side_weight;
        merged
            .entry(key)
            .and_modify(|existing| {
                existing.score += weighted_score;
                if anchor_side {
                    existing.anchor_rank =
                        Some(existing.anchor_rank.map_or(index, |rank| rank.min(index)));
                } else {
                    existing.current_rank =
                        Some(existing.current_rank.map_or(index, |rank| rank.min(index)));
                }
            })
            .or_insert_with(|| RankedLastFmTrack {
                track,
                score: weighted_score,
                anchor_rank: anchor_side.then_some(index),
                current_rank: (!anchor_side).then_some(index),
            });
    }
}

fn normalized_match_score(score: Option<f64>) -> f64 {
    let Some(score) = score else {
        return 0.5;
    };
    if score > 1.0 {
        (score / 100.0).clamp(0.0, 1.0)
    } else {
        score.clamp(0.0, 1.0)
    }
}

fn lastfm_track_key(track: &LastFmTrack) -> String {
    format!(
        "{}\u{0}{}",
        normalize_library_match_key(&track.artist),
        normalize_library_match_key(&track.title)
    )
}

fn same_seed(left: &LastFmSeed, right: &LastFmSeed) -> bool {
    match (left.mbid.as_deref(), right.mbid.as_deref()) {
        (Some(left), Some(right)) if !left.is_empty() && !right.is_empty() => {
            left.eq_ignore_ascii_case(right)
        }
        _ => {
            same_optional_artist(left.artist.as_deref(), right.artist.as_deref())
                && same_optional_text(left.title.as_deref(), right.title.as_deref())
        }
    }
}

fn same_optional_artist(left: Option<&str>, right: Option<&str>) -> bool {
    same_optional_text(left, right)
}

fn same_optional_text(left: Option<&str>, right: Option<&str>) -> bool {
    let Some(left) = left else {
        return false;
    };
    let Some(right) = right else {
        return false;
    };
    normalize_library_match_key(left) == normalize_library_match_key(right)
}

fn normalized_qobuz_album_id(value: &str) -> Option<String> {
    let value = value.trim().to_ascii_lowercase();
    (!value.is_empty()).then_some(value)
}

fn album_artist_key(album: Option<&str>, artist: Option<&str>) -> Option<String> {
    let album = album
        .map(normalize_library_match_key)
        .filter(|v| !v.is_empty())?;
    let artist = artist
        .map(normalize_library_match_key)
        .filter(|v| !v.is_empty())?;
    Some(format!("{album}\u{0}{artist}"))
}

fn canonical_song_title_key(title: &str) -> Option<String> {
    let without_bracket_versions = strip_version_brackets(title);
    let without_suffix_versions = strip_version_suffixes(&without_bracket_versions);
    let key = normalize_library_match_key(&without_suffix_versions);
    (!key.is_empty()).then_some(key)
}

fn strip_version_brackets(title: &str) -> String {
    let mut out = String::new();
    let mut chars = title.chars().peekable();
    while let Some(ch) = chars.next() {
        let Some(close) = matching_version_bracket(ch) else {
            out.push(ch);
            continue;
        };
        let mut inner = String::new();
        let mut closed = false;
        for inner_ch in chars.by_ref() {
            if inner_ch == close {
                closed = true;
                break;
            }
            inner.push(inner_ch);
        }
        if !closed || !is_version_descriptor(&inner) {
            out.push(ch);
            out.push_str(&inner);
            if closed {
                out.push(close);
            }
        }
    }
    out
}

fn matching_version_bracket(open: char) -> Option<char> {
    match open {
        '(' => Some(')'),
        '[' => Some(']'),
        '{' => Some('}'),
        _ => None,
    }
}

fn strip_version_suffixes(title: &str) -> String {
    let mut current = title.trim().to_string();
    while let Some((pos, separator)) = [" - ", " – ", " — "]
        .iter()
        .filter_map(|separator| current.rfind(separator).map(|pos| (pos, *separator)))
        .max_by_key(|(pos, _)| *pos)
    {
        let suffix = &current[pos + separator.len()..];
        if !is_version_descriptor(suffix) {
            break;
        }
        current.truncate(pos);
        current = current.trim().to_string();
    }
    current
}

fn is_version_descriptor(value: &str) -> bool {
    let key = normalize_library_match_key(value);
    if key.is_empty() {
        return false;
    }
    [
        "live",
        "remaster",
        "remastered",
        "version",
        "edit",
        "mix",
        "demo",
        "acoustic",
        "alternate",
        "take",
        "session",
        "rehearsal",
        "mono",
        "stereo",
        "anniversary",
        "deluxe",
        "bonus",
    ]
    .iter()
    .any(|marker| key.split_whitespace().any(|word| word == *marker))
}

async fn find_exact_qobuz_match(
    state: &AppState,
    lastfm_track: &LastFmTrack,
    context: &LastFmSeedContext,
    exclusions: &RadioExclusions,
    partial_errors: &mut Vec<String>,
    search_result_limit: usize,
) -> Option<QobuzTrack> {
    let query = format!("{} {}", lastfm_track.artist, lastfm_track.title);
    let response = match state.qobuz().search_tracks(&query).await {
        Ok(response) => response,
        Err(err) => {
            partial_errors.push(format!(
                "Qobuz search failed for {} - {}: {err}",
                lastfm_track.artist, lastfm_track.title
            ));
            return None;
        }
    };

    for track in response.tracks.into_iter().take(search_result_limit) {
        if !qobuz_track_exact_match(&track, lastfm_track)
            || !track.streamable
            || qobuz_track_same_album(&track, context)
            || exclusions.contains_qobuz(&track)
        {
            continue;
        }
        return Some(track);
    }

    None
}

fn qobuz_track_exact_match(qobuz: &QobuzTrack, lastfm: &LastFmTrack) -> bool {
    normalize_library_match_key(&qobuz.title) == normalize_library_match_key(&lastfm.title)
        && normalize_library_match_key(&qobuz.artist) == normalize_library_match_key(&lastfm.artist)
}

fn qobuz_match_with_local_playback_stats(
    mut qobuz: QobuzTrack,
    local: &TrackSummary,
) -> QobuzTrack {
    qobuz.play_count = local.play_count;
    qobuz.last_played_at = local.last_played_at;
    qobuz.listened_secs = local.listened_secs;
    qobuz
}

fn is_seed_track(track: &LastFmTrack, context: &LastFmSeedContext) -> bool {
    context.title.as_deref().is_some_and(|title| {
        normalize_library_match_key(title) == normalize_library_match_key(&track.title)
    }) && context.artist.as_deref().is_some_and(|artist| {
        normalize_library_match_key(artist) == normalize_library_match_key(&track.artist)
    })
}

fn local_track_same_album(track: &TrackSummary, context: &LastFmSeedContext) -> bool {
    if context
        .local_album_id
        .is_some_and(|album_id| track.album_id == Some(album_id))
    {
        return true;
    }
    album_artist_same(
        track.album.as_deref(),
        track.album_artist.as_deref().or(track.artist.as_deref()),
        context,
    )
}

fn qobuz_track_same_album(track: &QobuzTrack, context: &LastFmSeedContext) -> bool {
    if let (Some(context_album_id), Some(track_album_id)) =
        (context.qobuz_album_id.as_deref(), track.album_id.as_deref())
        && normalize_library_match_key(context_album_id)
            == normalize_library_match_key(track_album_id)
    {
        return true;
    }
    album_artist_same(Some(&track.album), Some(&track.artist), context)
}

fn album_artist_same(
    album: Option<&str>,
    artist: Option<&str>,
    context: &LastFmSeedContext,
) -> bool {
    let Some(context_album) = context.album.as_deref() else {
        return false;
    };
    let Some(album) = album else {
        return false;
    };
    if normalize_library_match_key(album) != normalize_library_match_key(context_album) {
        return false;
    }
    let Some(context_artist) = context
        .album_artist
        .as_deref()
        .or(context.artist.as_deref())
    else {
        return true;
    };
    artist.is_none_or(|artist| {
        normalize_library_match_key(artist) == normalize_library_match_key(context_artist)
    })
}

fn local_source_from_track(track: &TrackSummary) -> ResolvedPlaySource {
    ResolvedPlaySource::Local {
        track_id: track.id,
        title: track.title.clone(),
        artist: track.artist.clone(),
        album: track.album.clone(),
        art_id: track.art_id,
        duration_secs: track.duration_secs,
        file_name: track.file_name.clone(),
    }
}

fn qobuz_source_from_track(track: &QobuzTrack) -> ResolvedPlaySource {
    ResolvedPlaySource::Qobuz {
        track_id: track.id,
        title: track.title.clone(),
        artist: Some(track.artist.clone()),
        album: Some(track.album.clone()),
        album_id: track.album_id.clone(),
        image_url: track.image_url.clone(),
        duration_secs: (track.duration > 0).then_some(track.duration as f64),
        format_id: None,
    }
}

fn current_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

fn candidate_source_ref(
    candidate: &LastFmResolvedCandidate,
    radio_context: Option<RadioContext>,
) -> Option<SourceRef> {
    candidate
        .qobuz_match
        .as_ref()
        .filter(|_| candidate.match_status == "qobuz")
        .map(|track| qobuz_source_ref_from_track(track, true, radio_context.clone()))
        .or_else(|| {
            candidate
                .local_match
                .as_ref()
                .filter(|_| candidate.match_status == "local")
                .map(|track| local_source_ref_from_track(track, true, radio_context))
        })
}

fn local_source_ref_from_track(
    track: &TrackSummary,
    radio: bool,
    radio_context: Option<RadioContext>,
) -> SourceRef {
    SourceRef::LocalTrack {
        track_id: track.id,
        file_name: Some(track.file_name.clone()),
        title: Some(track.title.clone()),
        artist: track.artist.clone(),
        album: track.album.clone(),
        album_artist: track.album_artist.clone(),
        album_id: track.album_id,
        art_id: track.art_id,
        duration_secs: track.duration_secs,
        ext_hint: track.format.clone(),
        radio,
        radio_context,
        playlist_context: None,
    }
}

fn qobuz_source_ref_from_track(
    track: &QobuzTrack,
    radio: bool,
    radio_context: Option<RadioContext>,
) -> SourceRef {
    let source = SourceRef::QobuzTrack {
        track_id: track.id,
        title: Some(track.title.clone()),
        artist: Some(track.artist.clone()),
        album: Some(track.album.clone()),
        album_id: track.album_id.clone(),
        image_url: track.image_url.clone(),
        duration_secs: (track.duration > 0).then_some(track.duration as f64),
        radio: false,
        radio_context,
        playlist_context: None,
    };
    source_ref_with_radio(source, radio)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn best_playable_match_uses_lastfm_rank_not_source_type() {
        let qobuz = test_candidate_with_artist_score("qobuz", "Artist", "Higher ranked Qobuz", 2.0);
        let local = test_candidate_with_artist_score("local", "Artist", "Lower ranked local", 1.0);

        let best = first_playable_candidate(&[qobuz, local]).unwrap();

        assert_eq!(best.lastfm_track.title, "Higher ranked Qobuz");
        assert_eq!(best.match_status, "qobuz");
    }

    #[test]
    fn default_options_give_qobuz_enough_resolution_budget() {
        let options = LastFmResolveOptions::default();

        assert_eq!(options.limit, 30);
        assert_eq!(options.resolve_limit, 30);
        assert_eq!(options.qobuz_resolve_limit, 12);
    }

    #[test]
    fn radio_blend_weights_use_anchor_only_without_current_seed() {
        assert_eq!(radio_blend_weights(8, false), (1.0, 0.0));
    }

    #[test]
    fn radio_blend_weights_decay_anchor_with_hop() {
        let (anchor, current) = radio_blend_weights(0, true);
        assert_close(anchor, 0.8);
        assert_close(current, 0.2);

        let (anchor, current) = radio_blend_weights(2, true);
        assert_close(anchor, 0.7);
        assert_close(current, 0.3);
    }

    #[test]
    fn radio_blend_weights_floor_anchor_influence() {
        assert_eq!(radio_blend_weights(99, true), (0.2, 0.8));
    }

    #[test]
    fn selection_score_breaks_close_ties_toward_qobuz() {
        let local = test_candidate_with_artist_score("local", "Fresh Artist", "Local Copy", 1.0);
        let qobuz = test_candidate_with_artist_score("qobuz", "Fresh Artist", "Stream Copy", 0.95);

        let best = best_playable_candidate(&[local, qobuz], &RadioGuardrails::default()).unwrap();

        assert_eq!(best.match_status, "qobuz");
    }

    #[test]
    fn selection_score_preserves_stronger_local_recommendation() {
        let local = test_candidate_with_artist_score("local", "Fresh Artist", "Strong Local", 1.0);
        let qobuz = test_candidate_with_artist_score("qobuz", "Fresh Artist", "Weak Stream", 0.6);

        let best = best_playable_candidate(&[local, qobuz], &RadioGuardrails::default()).unwrap();

        assert_eq!(best.match_status, "local");
    }

    #[test]
    fn first_radio_pick_creates_anchor_context_from_non_radio_source() {
        let state = crate::playback::test_support::app_state("lastfm-first-anchor");
        let zone_id = state.zones().active_zone_id();
        let source = crate::playback::test_support::qobuz_source(7, false);

        let session = radio_session_from_source(&state, &zone_id, &source).unwrap();

        assert_eq!(session.anchor.title.as_deref(), Some("Track 7"));
        assert_eq!(session.anchor.artist.as_deref(), Some("Artist"));
        assert_eq!(session.current.title.as_deref(), Some("Track 7"));
        assert_eq!(
            session.radio_context.anchor.title.as_deref(),
            Some("Track 7")
        );
        assert_eq!(session.radio_context.hop, 0);
    }

    #[test]
    fn continuing_radio_reuses_original_anchor_and_advances_hop() {
        let state = crate::playback::test_support::app_state("lastfm-reuse-anchor");
        let zone_id = state.zones().active_zone_id();
        let mut source = crate::playback::test_support::qobuz_source(8, true);
        if let SourceRef::QobuzTrack { radio_context, .. } = &mut source {
            *radio_context = Some(test_radio_context());
        }

        let session = radio_session_from_source(&state, &zone_id, &source).unwrap();

        assert_eq!(session.anchor.title.as_deref(), Some("Anchor"));
        assert_eq!(session.current.title.as_deref(), Some("Track 8"));
        assert_eq!(
            session.radio_context.last_seed.title.as_deref(),
            Some("Track 8")
        );
        assert_eq!(session.radio_context.hop, 1);
    }

    #[test]
    fn hybrid_ranking_keeps_anchor_stronger_than_current_only_drift() {
        let ranked = hybrid_ranked_tracks(
            vec![lastfm_track_with_match(
                "Anchor Artist",
                "Strong Anchor",
                1.0,
            )],
            vec![lastfm_track_with_match("Far Artist", "Current Drift", 1.0)],
            20,
            0,
        );

        assert_eq!(ranked[0].track.title, "Strong Anchor");
        assert!(ranked[0].score > ranked[1].score);
    }

    #[test]
    fn hybrid_ranking_allows_current_seed_to_nudge_close_anchor_candidate() {
        let ranked = hybrid_ranked_tracks(
            vec![
                lastfm_track_with_match("Anchor Artist", "Anchor Top", 0.7),
                lastfm_track_with_match("Bridge Artist", "Bridge", 1.0),
            ],
            vec![lastfm_track_with_match("Bridge Artist", "Bridge", 1.0)],
            2,
            2,
        );

        assert_eq!(ranked[0].track.title, "Bridge");
        assert!(ranked[0].score > ranked[1].score);
    }

    #[test]
    fn later_hops_allow_current_seed_to_overtake_anchor_candidate() {
        let ranked = hybrid_ranked_tracks(
            vec![lastfm_track_with_match("Anchor Artist", "Anchor Hold", 1.0)],
            vec![lastfm_track_with_match(
                "Current Artist",
                "Current Drift",
                1.0,
            )],
            20,
            20,
        );

        assert_eq!(ranked[0].track.title, "Current Drift");
        assert!(ranked[0].score > ranked[1].score);
    }

    #[test]
    fn artist_repeat_penalty_prefers_fresh_close_match() {
        let recent = test_candidate_with_artist_score("local", "Recent Artist", "Repeat", 1.0);
        let fresh = test_candidate_with_artist_score("local", "Fresh Artist", "Fresh", 0.5);
        let guardrails = RadioGuardrails {
            current_artist: None,
            current_title_key: None,
            recent_radio_artists: vec!["Recent Artist".to_string()],
            recent_title_keys: HashSet::new(),
        };

        let best = best_playable_candidate(&[recent.clone(), fresh.clone()], &guardrails).unwrap();
        assert_eq!(best.lastfm_track.artist, "Fresh Artist");
    }

    #[test]
    fn artist_repeat_penalty_allows_overwhelming_match() {
        let recent = test_candidate_with_artist_score("local", "Recent Artist", "Repeat", 10.0);
        let fresh = test_candidate_with_artist_score("local", "Fresh Artist", "Fresh", 1.0);
        let guardrails = RadioGuardrails {
            current_artist: None,
            current_title_key: None,
            recent_radio_artists: vec!["Recent Artist".to_string()],
            recent_title_keys: HashSet::new(),
        };

        let best = best_playable_candidate(&[recent, fresh], &guardrails).unwrap();
        assert_eq!(best.lastfm_track.artist, "Recent Artist");
    }

    #[test]
    fn current_artist_penalty_prefers_close_alternative() {
        let current_artist =
            test_candidate_with_artist_score("local", "Current Artist", "Again", 1.0);
        let alternative =
            test_candidate_with_artist_score("local", "Other Artist", "Elsewhere", 0.5);
        let guardrails = RadioGuardrails {
            current_artist: Some("Current Artist".to_string()),
            current_title_key: None,
            recent_radio_artists: Vec::new(),
            recent_title_keys: HashSet::new(),
        };

        let best =
            best_playable_candidate(&[current_artist.clone(), alternative], &guardrails).unwrap();
        assert_eq!(best.lastfm_track.artist, "Other Artist");

        let fallback = best_playable_candidate(&[current_artist], &guardrails).unwrap();
        assert_eq!(fallback.lastfm_track.artist, "Current Artist");
    }

    #[test]
    fn favorite_bonus_uses_play_count_and_listened_time_caps() {
        assert_close(favorite_bonus(20, 10.0 * 3600.0), 0.2);
    }

    #[test]
    fn recency_penalty_fades_over_two_weeks() {
        let now = 1_000_000;
        assert_close(recency_penalty(Some(now), now), -0.25);
        assert_close(
            recency_penalty(Some(now - (RECENT_PLAY_PENALTY_WINDOW_SECS / 2)), now),
            -0.125,
        );
        assert_close(
            recency_penalty(Some(now - RECENT_PLAY_PENALTY_WINDOW_SECS), now),
            0.0,
        );
    }

    #[test]
    fn selection_score_boosts_favorite_local_match() {
        let favorite =
            test_candidate_with_local_stats("local", "Artist", "Favorite", 1.0, 10, 0.0, None);
        let plain = test_candidate_with_artist_score("local", "Artist", "Plain", 1.0);
        let now = 1_000_000;

        assert!(
            candidate_selection_score_at(&favorite, &RadioGuardrails::default(), now)
                > candidate_selection_score_at(&plain, &RadioGuardrails::default(), now)
        );
    }

    #[test]
    fn selection_score_penalizes_recently_played_match() {
        let now = 1_000_000;
        let recent =
            test_candidate_with_local_stats("local", "Artist", "Recent", 1.0, 0, 0.0, Some(now));
        let plain = test_candidate_with_artist_score("local", "Artist", "Plain", 1.0);

        assert!(
            candidate_selection_score_at(&recent, &RadioGuardrails::default(), now)
                < candidate_selection_score_at(&plain, &RadioGuardrails::default(), now)
        );
    }

    #[test]
    fn promoted_qobuz_match_inherits_local_playback_stats_for_scoring() {
        let mut local = track_summary(42, "Song", None);
        local.play_count = 10;
        local.last_played_at = Some(123);
        local.listened_secs = 7200.0;
        let qobuz = qobuz_track("Artist", "Song");

        let qobuz = qobuz_match_with_local_playback_stats(qobuz, &local);

        assert_eq!(qobuz.play_count, 10);
        assert_eq!(qobuz.last_played_at, Some(123));
        assert_eq!(qobuz.listened_secs, 7200.0);
    }

    #[test]
    fn title_guardrail_blocks_current_track_versions() {
        let live_version =
            test_candidate_with_artist_score("qobuz", "Bjork", "Venus as a Boy (Live)", 10.0);
        let alternative = test_candidate_with_artist_score("qobuz", "Bjork", "Come to Me", 1.0);
        let guardrails = RadioGuardrails {
            current_artist: None,
            current_title_key: canonical_song_title_key("Venus as a Boy"),
            recent_radio_artists: Vec::new(),
            recent_title_keys: HashSet::new(),
        };

        let best = best_playable_candidate(&[live_version, alternative], &guardrails).unwrap();

        assert_eq!(best.lastfm_track.title, "Come to Me");
    }

    #[test]
    fn title_guardrail_blocks_recent_studio_live_duplicates() {
        let live_version = test_candidate_with_artist_score(
            "local",
            "Radiohead",
            "The National Anthem - Live in Berlin",
            10.0,
        );
        let alternative = test_candidate_with_artist_score("qobuz", "Radiohead", "Optimistic", 1.0);
        let guardrails = RadioGuardrails {
            current_artist: None,
            current_title_key: None,
            recent_radio_artists: Vec::new(),
            recent_title_keys: HashSet::from([
                canonical_song_title_key("The National Anthem").unwrap()
            ]),
        };

        let best = best_playable_candidate(&[live_version, alternative], &guardrails).unwrap();

        assert_eq!(best.lastfm_track.title, "Optimistic");
    }

    #[test]
    fn canonical_title_key_removes_version_descriptors() {
        assert_eq!(
            canonical_song_title_key("The National Anthem - Live in Berlin"),
            canonical_song_title_key("The National Anthem")
        );
        assert_eq!(
            canonical_song_title_key("Venus as a Boy (Remastered 2003)"),
            canonical_song_title_key("Venus as a Boy")
        );
        assert_eq!(
            canonical_song_title_key("Song [Radio Edit]"),
            canonical_song_title_key("Song")
        );
    }

    #[test]
    fn same_album_local_match_is_not_playable() {
        let context = LastFmSeedContext {
            title: Some("Seed".to_string()),
            artist: Some("Artist".to_string()),
            album: Some("Album".to_string()),
            album_artist: Some("Artist".to_string()),
            local_album_id: Some(7),
            qobuz_album_id: None,
        };
        let lastfm = lastfm_track("Artist", "Neighbor");
        let track = track_summary(42, "Neighbor", Some(7));

        let candidate = resolved_candidate(
            lastfm,
            Some(track),
            None,
            &context,
            &RadioExclusions::default(),
            false,
            1.0,
        );

        assert_eq!(candidate.match_status, "same_album");
        assert!(candidate.selected_source.is_none());
        assert!(first_playable_candidate(&[candidate]).is_none());
    }

    #[test]
    fn same_album_qobuz_match_is_not_playable() {
        let context = LastFmSeedContext {
            title: Some("Seed".to_string()),
            artist: Some("Artist".to_string()),
            album: Some("Album".to_string()),
            album_artist: Some("Artist".to_string()),
            local_album_id: None,
            qobuz_album_id: Some("album-7".to_string()),
        };
        let lastfm = lastfm_track("Artist", "Neighbor");
        let track = qobuz_track("Artist", "Neighbor");

        let candidate = resolved_candidate(
            lastfm,
            None,
            Some(track),
            &context,
            &RadioExclusions::default(),
            true,
            1.0,
        );

        assert_eq!(candidate.match_status, "same_album");
        assert!(candidate.selected_source.is_none());
        assert!(first_playable_candidate(&[candidate]).is_none());
    }

    #[test]
    fn excluded_local_album_match_is_not_playable() {
        let context = LastFmSeedContext::default();
        let lastfm = lastfm_track("Artist", "Different Song");
        let track = track_summary(43, "Different Song", Some(7));
        let mut exclusions = RadioExclusions::default();
        exclusions.local_album_ids.insert(7);

        let candidate =
            resolved_candidate(lastfm, Some(track), None, &context, &exclusions, false, 1.0);

        assert_eq!(candidate.match_status, "excluded");
        assert!(candidate.selected_source.is_none());
    }

    #[test]
    fn excluded_qobuz_album_match_is_not_playable() {
        let context = LastFmSeedContext::default();
        let lastfm = lastfm_track("Artist", "Different Song");
        let mut track = qobuz_track("Artist", "Different Song");
        track.album_id = Some("qobuz-album".to_string());
        let mut exclusions = RadioExclusions::default();
        exclusions.add_qobuz_album_id("qobuz-album");

        let candidate =
            resolved_candidate(lastfm, None, Some(track), &context, &exclusions, true, 1.0);

        assert_eq!(candidate.match_status, "excluded");
        assert!(candidate.selected_source.is_none());
    }

    #[test]
    fn promoted_qobuz_candidate_keeps_local_album_exclusion_evidence() {
        let context = LastFmSeedContext::default();
        let lastfm = lastfm_track("Artist", "Song");
        let local = track_summary(42, "Song", Some(7));
        let qobuz = qobuz_track("Artist", "Song");
        let mut exclusions = RadioExclusions::default();
        exclusions.local_album_ids.insert(7);

        let candidate = resolved_candidate(
            lastfm,
            Some(local),
            Some(qobuz),
            &context,
            &exclusions,
            true,
            1.0,
        );

        assert_eq!(candidate.match_status, "excluded");
        assert!(candidate.local_match.is_some());
        assert!(candidate.qobuz_match.is_some());
        assert!(candidate.selected_source.is_none());
    }

    #[test]
    fn promoted_qobuz_candidate_selects_qobuz_source() {
        let context = LastFmSeedContext::default();
        let lastfm = lastfm_track("Artist", "Song");
        let local = track_summary(42, "Song", Some(7));
        let qobuz = qobuz_track("Artist", "Song");

        let candidate = resolved_candidate(
            lastfm,
            Some(local),
            Some(qobuz),
            &context,
            &RadioExclusions::default(),
            true,
            1.0,
        );

        assert_eq!(candidate.match_status, "qobuz");
        assert!(matches!(
            candidate.selected_source,
            Some(ResolvedPlaySource::Qobuz { track_id: 7, .. })
        ));
    }

    #[test]
    fn local_candidate_stays_local_without_qobuz_match() {
        let context = LastFmSeedContext::default();
        let lastfm = lastfm_track("Artist", "Song");
        let local = track_summary(42, "Song", Some(7));

        let candidate = resolved_candidate(
            lastfm,
            Some(local),
            None,
            &context,
            &RadioExclusions::default(),
            true,
            1.0,
        );

        assert_eq!(candidate.match_status, "local");
        assert!(candidate.qobuz_match.is_none());
        assert!(matches!(
            candidate.selected_source,
            Some(ResolvedPlaySource::Local { track_id: 42, .. })
        ));
    }

    #[test]
    fn exact_qobuz_lookup_includes_local_candidates_without_primary_match() {
        let context = LastFmSeedContext::default();
        let local_candidate = resolved_candidate(
            lastfm_track("Artist", "Song"),
            Some(track_summary(42, "Song", Some(7))),
            None,
            &context,
            &RadioExclusions::default(),
            false,
            1.0,
        );
        let primary_qobuz_candidate = resolved_candidate(
            lastfm_track("Artist", "Song"),
            Some(track_summary(42, "Song", Some(7))),
            Some(qobuz_track("Artist", "Song")),
            &context,
            &RadioExclusions::default(),
            true,
            1.0,
        );
        let unmatched_candidate = resolved_candidate(
            lastfm_track("Artist", "Song"),
            None,
            None,
            &context,
            &RadioExclusions::default(),
            false,
            1.0,
        );

        assert!(should_try_exact_qobuz_match(&local_candidate));
        assert!(should_try_exact_qobuz_match(&unmatched_candidate));
        assert!(!should_try_exact_qobuz_match(&primary_qobuz_candidate));
    }

    #[test]
    fn exact_qobuz_promotion_prefers_qobuz_and_preserves_local_stats() {
        let context = LastFmSeedContext::default();
        let lastfm = lastfm_track("Artist", "Song");
        let mut local = track_summary(42, "Song", Some(7));
        local.play_count = 9;
        local.last_played_at = Some(456);
        local.listened_secs = 1800.0;
        let qobuz = qobuz_match_with_local_playback_stats(qobuz_track("Artist", "Song"), &local);

        let candidate = resolved_candidate(
            lastfm,
            Some(local),
            Some(qobuz),
            &context,
            &RadioExclusions::default(),
            true,
            1.0,
        );

        let promoted = candidate.qobuz_match.as_ref().unwrap();
        assert_eq!(candidate.match_status, "qobuz");
        assert_eq!(promoted.play_count, 9);
        assert_eq!(promoted.last_played_at, Some(456));
        assert_eq!(promoted.listened_secs, 1800.0);
        assert!(matches!(
            candidate.selected_source,
            Some(ResolvedPlaySource::Qobuz { track_id: 7, .. })
        ));
    }

    #[test]
    fn excluded_local_match_is_not_playable() {
        let context = LastFmSeedContext::default();
        let lastfm = lastfm_track("Artist", "Song");
        let track = track_summary(42, "Song", None);
        let mut exclusions = RadioExclusions::default();
        exclusions.local_track_ids.insert(42);

        let candidate =
            resolved_candidate(lastfm, Some(track), None, &context, &exclusions, false, 1.0);

        assert_eq!(candidate.match_status, "excluded");
        assert!(candidate.selected_source.is_none());
    }

    #[test]
    fn local_candidate_source_ref_is_marked_radio() {
        let candidate = LastFmResolvedCandidate {
            lastfm_track: lastfm_track("Artist", "Song"),
            local_match: Some(track_summary(42, "Song", None)),
            qobuz_match: None,
            qobuz_checked: false,
            match_status: "local".to_string(),
            selected_source: None,
            radio_score: 1.0,
            selection_score: 1.0,
            score_breakdown: LastFmCandidateScoreBreakdown::unscored(1.0),
        };

        let source = candidate_source_ref(&candidate, Some(test_radio_context())).unwrap();

        assert!(source.is_radio());
        assert!(source_radio_context(&source).is_some());
    }

    #[test]
    fn future_queue_check_ignores_stale_current_source() {
        let state = crate::playback::test_support::app_state("lastfm-future-queue");
        let zone_id = state.zones().active_zone_id();
        let active = crate::playback::test_support::qobuz_source(1, true);
        let future = crate::playback::test_support::qobuz_source(2, true);
        state
            .library()
            .upsert_zone_definition(&zone_id, "Core", "local_coreaudio", None, true)
            .unwrap();

        state
            .library()
            .set_zone_queue(&zone_id, std::slice::from_ref(&active))
            .unwrap();
        assert!(!lastfm_radio_has_future_queue(&state, &zone_id, &active));

        state
            .library()
            .set_zone_queue(&zone_id, &[active.clone(), future])
            .unwrap();
        assert!(lastfm_radio_has_future_queue(&state, &zone_id, &active));
    }

    fn test_candidate_with_artist_score(
        status: &str,
        artist: &str,
        title: &str,
        radio_score: f64,
    ) -> LastFmResolvedCandidate {
        let mut candidate = LastFmResolvedCandidate {
            lastfm_track: lastfm_track(artist, title),
            local_match: None,
            qobuz_match: None,
            qobuz_checked: false,
            match_status: status.to_string(),
            selected_source: Some(ResolvedPlaySource::Local {
                track_id: 1,
                title: title.to_string(),
                artist: Some("Artist".to_string()),
                album: None,
                art_id: None,
                duration_secs: None,
                file_name: "song.flac".to_string(),
            }),
            radio_score,
            selection_score: radio_score,
            score_breakdown: LastFmCandidateScoreBreakdown::unscored(radio_score),
        };
        apply_candidate_selection_scores_at(
            std::slice::from_mut(&mut candidate),
            &RadioGuardrails::default(),
            current_unix_secs(),
        );
        candidate
    }

    fn test_candidate_with_local_stats(
        status: &str,
        artist: &str,
        title: &str,
        radio_score: f64,
        play_count: i64,
        listened_secs: f64,
        last_played_at: Option<i64>,
    ) -> LastFmResolvedCandidate {
        let mut candidate = test_candidate_with_artist_score(status, artist, title, radio_score);
        let mut track = track_summary(1, title, None);
        track.artist = Some(artist.to_string());
        track.play_count = play_count;
        track.listened_secs = listened_secs;
        track.last_played_at = last_played_at;
        candidate.local_match = Some(track);
        apply_candidate_selection_scores_at(
            std::slice::from_mut(&mut candidate),
            &RadioGuardrails::default(),
            current_unix_secs(),
        );
        candidate
    }

    fn assert_close(left: f64, right: f64) {
        assert!(
            (left - right).abs() < 0.000_001,
            "expected {left} to be close to {right}"
        );
    }

    fn test_radio_context() -> RadioContext {
        RadioContext {
            provider: LASTFM_RADIO_PROVIDER.to_string(),
            anchor: RadioSeedContext {
                title: Some("Anchor".to_string()),
                artist: Some("Anchor Artist".to_string()),
                mbid: None,
            },
            last_seed: RadioSeedContext {
                title: Some("Seed".to_string()),
                artist: Some("Seed Artist".to_string()),
                mbid: None,
            },
            hop: 0,
        }
    }

    fn lastfm_track(artist: &str, title: &str) -> LastFmTrack {
        lastfm_track_with_match(artist, title, 0.5)
    }

    fn lastfm_track_with_match(artist: &str, title: &str, match_score: f64) -> LastFmTrack {
        LastFmTrack {
            title: title.to_string(),
            artist: artist.to_string(),
            mbid: None,
            artist_mbid: None,
            url: None,
            match_score: Some(match_score),
            image_url: None,
        }
    }

    fn qobuz_track(artist: &str, title: &str) -> QobuzTrack {
        QobuzTrack {
            id: 7,
            title: title.to_string(),
            artist: artist.to_string(),
            artist_id: None,
            album: "Album".to_string(),
            album_id: Some("album-7".to_string()),
            track_number: Some(1),
            disc_number: Some(1),
            duration: 180,
            image_url: None,
            maximum_sampling_rate: None,
            maximum_bit_depth: None,
            hires: false,
            streamable: true,
            composer: None,
            work: None,
            isrc: None,
            copyright: None,
            performers_raw: None,
            credits: Vec::new(),
            play_count: 0,
            last_played_at: None,
            listened_secs: 0.0,
        }
    }

    fn track_summary(id: i64, title: &str, album_id: Option<i64>) -> TrackSummary {
        TrackSummary {
            id,
            file_name: format!("{title}.flac"),
            title: title.to_string(),
            artist: Some("Artist".to_string()),
            album: Some("Album".to_string()),
            album_artist: Some("Artist".to_string()),
            track_number: None,
            disc_number: None,
            year: None,
            genre: None,
            composer: None,
            duration_secs: Some(180.0),
            sample_rate: None,
            bit_depth: None,
            channels: None,
            format: Some("flac".to_string()),
            album_id,
            art_id: None,
            play_count: 0,
            last_played_at: None,
            listened_secs: 0.0,
            preferred_play_source: None,
        }
    }
}
