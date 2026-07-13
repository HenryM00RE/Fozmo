use crate::app::state::AppState;
use crate::audio::player::TrackTags;
use crate::audio::resampler::FilterType;
use crate::audio::sonos::{self, SonosAsset, SonosSource, SonosTarget};
use crate::playback::commands::is_current_playback_sequence;
use crate::playback::error::PlaybackError;
use crate::playback::now_playing::sonos_current_matches;
use crate::playback::resolver::lookup_fallback_cover;
use crate::playback::sequencer::PlaybackRequestSequence;
use crate::playback::service::playback_config_for_zone;
use crate::protocol::{PlaybackConfig, SourceRef};

const SONOS_PREFETCH_WINDOW: usize = 2;

pub(crate) async fn play_sonos_source_for_zone(
    state: AppState,
    zone_id: &str,
    profile_id: String,
    expected_playback_sequence: Option<PlaybackRequestSequence>,
    source_ref: SourceRef,
    queue_sources: Vec<SourceRef>,
    radio_auto: bool,
) -> Result<(), PlaybackError> {
    let target = sonos_target_for_zone(&state, zone_id)?;
    let playback_config =
        playback_config_for_zone(&state, zone_id, state.zones().active_player().as_ref());
    let asset = prepare_sonos_asset(&state, &source_ref, &playback_config).await?;
    if expected_playback_sequence
        .as_ref()
        .is_some_and(|expected| !is_current_playback_sequence(&state, expected))
    {
        return Err(PlaybackError::conflict("Playback changed"));
    }
    let expected_current = Some(sonos_asset_file_name(&asset));
    state
        .sonos()
        .play(zone_id, &target, asset)
        .await
        .map_err(PlaybackError::integration)?;
    let _ = state.library().set_zone_queue(zone_id, &queue_sources);
    state.listening().start_with_radio(
        state.library(),
        zone_id.to_string(),
        state.zones().zone_name(zone_id),
        profile_id,
        source_ref,
        queue_sources.clone(),
        radio_auto,
    );
    spawn_sonos_next_prefetch(
        state,
        zone_id.to_string(),
        target,
        expected_current,
        expected_playback_sequence,
        queue_sources,
        playback_config,
    );
    Ok(())
}

pub(crate) fn spawn_sonos_next_prefetch(
    state: AppState,
    zone_id: String,
    target: SonosTarget,
    expected_current: Option<String>,
    expected_playback_sequence: Option<PlaybackRequestSequence>,
    queue_sources: Vec<SourceRef>,
    playback_config: PlaybackConfig,
) {
    tokio::spawn(async move {
        prefetch_sonos_next(
            state,
            zone_id,
            target,
            expected_current,
            expected_playback_sequence,
            queue_sources,
            playback_config,
        )
        .await;
    });
}

pub(crate) async fn prefetch_sonos_next(
    state: AppState,
    zone_id: String,
    target: SonosTarget,
    expected_current: Option<String>,
    expected_playback_sequence: Option<PlaybackRequestSequence>,
    queue_sources: Vec<SourceRef>,
    playback_config: PlaybackConfig,
) {
    let mut queue_sources = queue_sources.into_iter();
    let Some(next_source) = queue_sources.next() else {
        return;
    };
    match prepare_sonos_asset(&state, &next_source, &playback_config).await {
        Ok(asset) => {
            if expected_playback_sequence
                .as_ref()
                .is_some_and(|expected| !is_current_playback_sequence(&state, expected))
                || !sonos_current_matches(&state, &zone_id, &expected_current)
            {
                return;
            }
            let armed_asset_id = asset.id.clone();
            if let Err(e) = state.sonos().set_next(&zone_id, &target, asset).await {
                state.sonos().mark_notice(&zone_id, e);
                return;
            }
            let mut expected_tail_id = armed_asset_id;
            for next_source in queue_sources.take(SONOS_PREFETCH_WINDOW.saturating_sub(1)) {
                let Ok(asset) = prepare_sonos_asset(&state, &next_source, &playback_config).await
                else {
                    continue;
                };
                if expected_playback_sequence
                    .as_ref()
                    .is_some_and(|expected| !is_current_playback_sequence(&state, expected))
                    || !sonos_current_matches(&state, &zone_id, &expected_current)
                {
                    return;
                }
                let asset_id = asset.id.clone();
                if !state
                    .sonos()
                    .append_next_if_tail_matches(&zone_id, &expected_tail_id, asset)
                {
                    return;
                }
                expected_tail_id = asset_id;
            }
        }
        Err(e) => state.sonos().mark_notice(&zone_id, e.message().to_string()),
    }
}

fn sonos_asset_file_name(asset: &SonosAsset) -> String {
    asset
        .title
        .clone()
        .unwrap_or_else(|| format!("sonos:{}", asset.id))
}

pub(crate) fn sonos_target_for_zone(
    state: &AppState,
    zone_id: &str,
) -> Result<SonosTarget, PlaybackError> {
    state
        .zones()
        .zone_bound_device_name(zone_id)
        .as_deref()
        .and_then(sonos::parse_target_device_name)
        .ok_or_else(|| PlaybackError::not_found("Sonos zone not available"))
}

pub(crate) async fn prepare_sonos_asset(
    state: &AppState,
    source_ref: &SourceRef,
    playback_config: &PlaybackConfig,
) -> Result<SonosAsset, PlaybackError> {
    let filter =
        FilterType::from_name(&playback_config.filter_type).unwrap_or(FilterType::Minimum16k);
    let source = sonos_source_from_ref(state, source_ref).await?;
    state
        .sonos()
        .prepare_source(source, filter)
        .await
        .map_err(PlaybackError::library)
}

async fn sonos_source_from_ref(
    state: &AppState,
    source_ref: &SourceRef,
) -> Result<SonosSource, PlaybackError> {
    match source_ref {
        SourceRef::LocalTrack {
            track_id,
            title,
            artist,
            album,
            ..
        } => {
            let path = state
                .library()
                .track_path(*track_id)
                .map_err(PlaybackError::library)?
                .ok_or_else(|| PlaybackError::not_found("Track not found"))?;
            let path_str = path.to_string_lossy().to_string();
            let cover = lookup_fallback_cover(state, &path_str);
            Ok(SonosSource::LocalFile {
                path,
                tags: TrackTags {
                    title: title.clone(),
                    artist: artist.clone(),
                    album: album.clone(),
                    ..TrackTags::default()
                },
                cover,
            })
        }
        SourceRef::QobuzTrack {
            track_id,
            title,
            artist,
            album,
            image_url,
            duration_secs,
            ..
        } => {
            let stream = state
                .qobuz()
                .sonos_cd_quality_stream(*track_id)
                .await
                .map_err(PlaybackError::integration)?;
            let cover = match image_url.as_deref() {
                Some(url) => state.qobuz().fetch_cover_public(url).await.ok().and_then(
                    |(mime, data)| match (mime, data) {
                        (Some(mime), Some(data)) => {
                            Some(crate::audio::player::TrackCover { mime, data })
                        }
                        _ => None,
                    },
                ),
                None => None,
            };
            let (asset_id, token) = state.sonos().register_qobuz_remote_stream(
                *track_id,
                stream.format_id,
                cover.clone(),
            );
            let stream_url = format!(
                "{}/sonos/qobuz/{}?asset={}&token={}",
                state.public_base_url().trim_end_matches('/'),
                track_id,
                urlencoding::encode(&asset_id),
                urlencoding::encode(&token)
            );
            let proxied_art_url = cover.as_ref().map(|_| {
                format!(
                    "{}/sonos/art/{}?token={}",
                    state.public_base_url().trim_end_matches('/'),
                    urlencoding::encode(&asset_id),
                    urlencoding::encode(&token)
                )
            });
            Ok(SonosSource::RemoteStream {
                id: asset_id,
                stream_url,
                mime_type: stream.mime_type,
                art_url: proxied_art_url.or_else(|| image_url.clone()),
                tags: TrackTags {
                    title: title.clone(),
                    artist: artist.clone(),
                    album: album.clone(),
                    album_artist: artist.clone(),
                    duration_secs: *duration_secs,
                    ..TrackTags::default()
                },
                source_rate: stream.sample_rate_hz,
                source_bits: stream.bit_depth,
            })
        }
    }
}
