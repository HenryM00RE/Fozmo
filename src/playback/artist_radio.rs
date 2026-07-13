use crate::app::state::AppState;
use crate::library::{TrackSummary, normalize_library_match_key};
use crate::playback::commands::{PlaybackRequestSequence, accept_playback_request_sequence};
use crate::playback::error::PlaybackError;
use crate::playback::intent::{PlaybackGuard, PlaybackIntent};
use crate::playback::lastfm::lastfm_radio_next_source_from_source_for_zone;
use crate::playback::router::PlaybackRouter;
use crate::playback::source::{
    qobuz_play_request_from_source_ref, qobuz_source_ref_from_track, source_ref_with_radio,
    source_ref_with_radio_context,
};
use crate::protocol::{RadioContext, RadioSeedContext, SourceRef};
use rand::seq::SliceRandom;
use std::collections::HashSet;

const LOCAL_ARTIST_RADIO_PROVIDER: &str = "local_artist";

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum ArtistRadioMode {
    Auto,
    Local,
    Qobuz,
}

impl ArtistRadioMode {
    pub(crate) fn parse(value: Option<&str>) -> Result<Self, PlaybackError> {
        match value
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("auto")
            .to_ascii_lowercase()
            .as_str()
        {
            "auto" => Ok(Self::Auto),
            "local" => Ok(Self::Local),
            "qobuz" => Ok(Self::Qobuz),
            _ => Err(PlaybackError::bad_request("Invalid artist radio mode")),
        }
    }
}

#[allow(dead_code)]
pub(crate) async fn play_artist_radio_for_active_zone(
    state: AppState,
    sequence: Option<PlaybackRequestSequence>,
    artist_name: &str,
    mode: ArtistRadioMode,
) -> Result<(), PlaybackError> {
    let profile_id = state.settings().active_profile_id();
    play_artist_radio_for_active_zone_with_profile(state, &profile_id, sequence, artist_name, mode)
        .await
}

pub(crate) async fn play_artist_radio_for_active_zone_with_profile(
    state: AppState,
    profile_id: &str,
    sequence: Option<PlaybackRequestSequence>,
    artist_name: &str,
    mode: ArtistRadioMode,
) -> Result<(), PlaybackError> {
    let zone_id = state.zones().active_zone_id();
    play_artist_radio_for_zone_with_profile(
        state,
        &zone_id,
        profile_id,
        sequence,
        artist_name,
        mode,
    )
    .await
}

#[allow(dead_code)]
pub(crate) async fn play_artist_radio_for_zone(
    state: AppState,
    zone_id: &str,
    sequence: Option<PlaybackRequestSequence>,
    artist_name: &str,
    mode: ArtistRadioMode,
) -> Result<(), PlaybackError> {
    let profile_id = state.settings().active_profile_id();
    play_artist_radio_for_zone_with_profile(
        state,
        zone_id,
        &profile_id,
        sequence,
        artist_name,
        mode,
    )
    .await
}

pub(crate) async fn play_artist_radio_for_zone_with_profile(
    state: AppState,
    zone_id: &str,
    profile_id: &str,
    sequence: Option<PlaybackRequestSequence>,
    artist_name: &str,
    mode: ArtistRadioMode,
) -> Result<(), PlaybackError> {
    if !accept_playback_request_sequence(&state, sequence.as_ref()) {
        return Err(PlaybackError::conflict("Playback changed"));
    }
    let artist_name = normalized_artist_name(Some(artist_name))
        .ok_or_else(|| PlaybackError::bad_request("artist_name is required"))?;
    let source = artist_radio_source_for_zone(state.clone(), zone_id, &artist_name, mode)
        .await?
        .ok_or_else(|| PlaybackError::not_found("No playable Artist Radio track found"))?;
    let radio_auto = source.is_radio();
    let qobuz_request = qobuz_play_request_from_source_ref(&source, &[], radio_auto).map(Box::new);
    PlaybackRouter::new(&state)
        .execute(
            zone_id,
            PlaybackIntent::Play {
                profile_id: profile_id.to_string(),
                source,
                queue: Vec::new(),
                radio_auto,
                guard: PlaybackGuard::from_expected_sequence(sequence),
                qobuz_request,
            },
        )
        .await
        .map(|_| ())
}

async fn artist_radio_source_for_zone(
    state: AppState,
    zone_id: &str,
    artist_name: &str,
    mode: ArtistRadioMode,
) -> Result<Option<SourceRef>, PlaybackError> {
    match mode {
        ArtistRadioMode::Qobuz => qobuz_artist_radio_source(state, artist_name).await,
        ArtistRadioMode::Local => {
            local_artist_radio_source_for_zone(&state, zone_id, artist_name, None)
        }
        ArtistRadioMode::Auto => {
            if state.lastfm_radio_active() {
                match lastfm_artist_radio_source_for_zone(state.clone(), zone_id, artist_name).await
                {
                    Ok(Some(source)) => return Ok(Some(source)),
                    Ok(None) => {}
                    Err(error) => eprintln!("artist-radio: Last.fm seed failed: {error}"),
                }
            }
            if let Some(source) =
                local_artist_radio_source_for_zone(&state, zone_id, artist_name, None)?
            {
                return Ok(Some(source));
            }
            qobuz_artist_radio_source(state, artist_name).await
        }
    }
}

async fn lastfm_artist_radio_source_for_zone(
    state: AppState,
    zone_id: &str,
    artist_name: &str,
) -> Result<Option<SourceRef>, String> {
    let Some(seed_source) = representative_local_source_for_artist(&state, artist_name)? else {
        return Ok(None);
    };
    lastfm_radio_next_source_from_source_for_zone(state, zone_id, seed_source).await
}

pub(crate) fn local_artist_radio_next_source_from_source_for_zone(
    state: &AppState,
    zone_id: &str,
    active_source: &SourceRef,
) -> Result<Option<SourceRef>, String> {
    let Some(artist_name) = source_artist(active_source) else {
        return Ok(None);
    };
    local_artist_radio_source_for_zone(state, zone_id, &artist_name, Some(active_source))
        .map_err(|error| error.message().to_string())
}

fn local_artist_radio_source_for_zone(
    state: &AppState,
    zone_id: &str,
    artist_name: &str,
    active_source: Option<&SourceRef>,
) -> Result<Option<SourceRef>, PlaybackError> {
    let tracks = state
        .library()
        .tracks_by_artist(artist_name)
        .map_err(PlaybackError::library)?;
    let Some(track) = choose_local_artist_radio_track(state, zone_id, tracks, active_source)?
    else {
        return Ok(None);
    };
    let source = state
        .library()
        .source_ref_for_track_id(track.id)
        .map_err(PlaybackError::library)?
        .ok_or_else(|| PlaybackError::not_found("Track not found"))?;
    let source = source_ref_with_radio(source, true);
    Ok(Some(source_ref_with_radio_context(
        source,
        Some(local_artist_radio_context(artist_name, &track)),
    )))
}

fn choose_local_artist_radio_track(
    state: &AppState,
    zone_id: &str,
    mut tracks: Vec<TrackSummary>,
    active_source: Option<&SourceRef>,
) -> Result<Option<TrackSummary>, PlaybackError> {
    let mut excluded_source_keys = HashSet::new();
    let mut excluded_album_ids = HashSet::new();
    let mut recent_title_keys = HashSet::new();
    if let Some(source) = active_source {
        add_source_exclusions(source, &mut excluded_source_keys, &mut excluded_album_ids);
        if let Some(title) = source_title(source).and_then(|title| canonical_song_title_key(&title))
        {
            recent_title_keys.insert(title);
        }
    }
    if let Some(source) = state.listening().active_source(zone_id) {
        add_source_exclusions(&source, &mut excluded_source_keys, &mut excluded_album_ids);
    }
    if let Ok(queue) = state.library().zone_queue(zone_id) {
        for entry in queue {
            add_source_exclusions(
                &entry.source,
                &mut excluded_source_keys,
                &mut excluded_album_ids,
            );
        }
    }
    let live = state.listening().active_history_inputs();
    let profile_id = state
        .listening()
        .profile_id(zone_id)
        .unwrap_or_else(|| crate::settings::DEFAULT_PROFILE_ID.to_string());
    if let Ok(recent) = state
        .library()
        .recent_playback_history_with_live_for_profile(&profile_id, 50, &live, true)
    {
        for entry in recent {
            add_source_exclusions(
                &entry.source,
                &mut excluded_source_keys,
                &mut excluded_album_ids,
            );
            if let Some(title) = entry
                .title
                .or_else(|| source_title(&entry.source))
                .and_then(|title| canonical_song_title_key(&title))
            {
                recent_title_keys.insert(title);
            }
        }
    }

    tracks.shuffle(&mut rand::thread_rng());
    let mut seen_titles = HashSet::new();
    let mut fallback = None;
    for track in tracks {
        if excluded_source_keys.contains(&format!("local:{}", track.id)) {
            continue;
        }
        if track
            .album_id
            .is_some_and(|album_id| excluded_album_ids.contains(&album_id))
        {
            continue;
        }
        let title_key = canonical_song_title_key(&track.title);
        if title_key
            .as_ref()
            .is_some_and(|key| !seen_titles.insert(key.clone()))
        {
            continue;
        }
        if title_key
            .as_ref()
            .is_some_and(|key| recent_title_keys.contains(key))
        {
            fallback.get_or_insert(track);
            continue;
        }
        return Ok(Some(track));
    }
    Ok(fallback)
}

async fn qobuz_artist_radio_source(
    state: AppState,
    artist_name: &str,
) -> Result<Option<SourceRef>, PlaybackError> {
    let recommendation = state
        .qobuz()
        .radio_next_for_artist_name(artist_name, &[], 50)
        .await
        .map_err(qobuz_artist_radio_error)?;
    Ok(recommendation.map(|recommendation| {
        let source = qobuz_source_ref_from_track(&recommendation.track, true);
        source_ref_with_radio_context(
            source,
            Some(qobuz_artist_radio_context(
                artist_name,
                &recommendation.track,
            )),
        )
    }))
}

fn qobuz_artist_radio_error(error: String) -> PlaybackError {
    if error.to_ascii_lowercase().contains("log in to qobuz") {
        PlaybackError::forbidden(error)
    } else {
        PlaybackError::integration(error)
    }
}

fn representative_local_source_for_artist(
    state: &AppState,
    artist_name: &str,
) -> Result<Option<SourceRef>, String> {
    let tracks = state.library().tracks_by_artist(artist_name)?;
    let Some(track) = tracks.into_iter().next() else {
        return Ok(None);
    };
    state.library().source_ref_for_track_id(track.id)
}

fn add_source_exclusions(
    source: &SourceRef,
    source_keys: &mut HashSet<String>,
    album_ids: &mut HashSet<i64>,
) {
    source_keys.insert(source.key());
    if let SourceRef::LocalTrack {
        album_id: Some(album_id),
        ..
    } = source
    {
        album_ids.insert(*album_id);
    }
}

fn local_artist_radio_context(artist_name: &str, track: &TrackSummary) -> RadioContext {
    RadioContext {
        provider: LOCAL_ARTIST_RADIO_PROVIDER.to_string(),
        anchor: RadioSeedContext {
            title: None,
            artist: Some(artist_name.to_string()),
            mbid: None,
        },
        last_seed: RadioSeedContext {
            title: Some(track.title.clone()),
            artist: track
                .artist
                .clone()
                .or_else(|| track.album_artist.clone())
                .or_else(|| Some(artist_name.to_string())),
            mbid: None,
        },
        hop: 0,
    }
}

fn qobuz_artist_radio_context(
    artist_name: &str,
    track: &crate::services::qobuz::QobuzTrack,
) -> RadioContext {
    RadioContext {
        provider: "qobuz_artist".to_string(),
        anchor: RadioSeedContext {
            title: None,
            artist: Some(artist_name.to_string()),
            mbid: None,
        },
        last_seed: RadioSeedContext {
            title: Some(track.title.clone()),
            artist: Some(track.artist.clone()),
            mbid: None,
        },
        hop: 0,
    }
}

fn source_artist(source: &SourceRef) -> Option<String> {
    match source {
        SourceRef::LocalTrack {
            artist,
            album_artist,
            ..
        } => normalized_artist_name(artist.as_deref())
            .or_else(|| normalized_artist_name(album_artist.as_deref())),
        SourceRef::QobuzTrack { artist, .. } => normalized_artist_name(artist.as_deref()),
    }
}

fn source_title(source: &SourceRef) -> Option<String> {
    match source {
        SourceRef::LocalTrack { title, .. } | SourceRef::QobuzTrack { title, .. } => {
            normalized_artist_name(title.as_deref())
        }
    }
}

fn normalized_artist_name(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn canonical_song_title_key(title: &str) -> Option<String> {
    let key = normalize_library_match_key(title);
    (!key.is_empty()).then_some(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playback::test_support::app_state;

    #[test]
    fn local_artist_radio_context_marks_artist_anchor_and_track_seed() {
        let track = track_summary(7, "Optimistic", Some("Radiohead"), None);

        let context = local_artist_radio_context("Radiohead", &track);

        assert_eq!(context.provider, "local_artist");
        assert_eq!(context.anchor.artist.as_deref(), Some("Radiohead"));
        assert_eq!(context.anchor.title, None);
        assert_eq!(context.last_seed.title.as_deref(), Some("Optimistic"));
        assert_eq!(context.last_seed.artist.as_deref(), Some("Radiohead"));
    }

    #[test]
    fn local_artist_radio_choice_excludes_current_and_dedupes_titles() {
        let state = app_state("artist-radio-local-choice");
        let zone_id = state.zones().active_zone_id();
        let active = local_source(1, "Current Song", Some("Radiohead"), Some(1));
        let tracks = vec![
            track_summary(1, "Current Song", Some("Radiohead"), Some(1)),
            track_summary(2, "Fresh Song", Some("Radiohead"), Some(2)),
            track_summary(3, "Fresh Song", Some("Radiohead"), Some(3)),
        ];

        let chosen = choose_local_artist_radio_track(&state, &zone_id, tracks, Some(&active))
            .unwrap()
            .unwrap();

        assert_ne!(chosen.id, 1);
        assert_eq!(chosen.title, "Fresh Song");
    }

    #[test]
    fn local_artist_radio_choice_uses_recent_title_only_as_fallback() {
        let state = app_state("artist-radio-recent-title-fallback");
        let zone_id = state.zones().active_zone_id();
        let tracks = vec![track_summary(
            2,
            "Recently Played",
            Some("Radiohead"),
            Some(2),
        )];
        let active = local_source(1, "Recently Played", Some("Radiohead"), Some(1));

        let chosen = choose_local_artist_radio_track(&state, &zone_id, tracks, Some(&active))
            .unwrap()
            .unwrap();

        assert_eq!(chosen.title, "Recently Played");
    }

    fn local_source(
        track_id: i64,
        title: &str,
        artist: Option<&str>,
        album_id: Option<i64>,
    ) -> SourceRef {
        SourceRef::LocalTrack {
            track_id,
            file_name: Some(format!("{title}.flac")),
            title: Some(title.to_string()),
            artist: artist.map(str::to_string),
            album: Some("Album".to_string()),
            album_artist: artist.map(str::to_string),
            album_id,
            art_id: None,
            duration_secs: Some(180.0),
            ext_hint: Some("flac".to_string()),
            radio: false,
            radio_context: None,
            playlist_context: None,
        }
    }

    fn track_summary(
        id: i64,
        title: &str,
        artist: Option<&str>,
        album_id: Option<i64>,
    ) -> TrackSummary {
        TrackSummary {
            id,
            file_name: format!("{title}.flac"),
            title: title.to_string(),
            artist: artist.map(str::to_string),
            album: Some("Album".to_string()),
            album_artist: artist.map(str::to_string),
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
