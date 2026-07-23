use super::runtime::{AgentRuntimeState, EnginePrefetchedSource};
use super::stream_source::AgentStreamSource;
use crate::audio::player::{Player, StreamQueueItem, TrackTags};
use crate::protocol::SourceRef;
use reqwest::{Client, Url};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Instant;

const AGENT_MAX_PREFETCHED_STREAMS: usize = 2;

pub(super) struct AgentStreamHandle {
    source: AgentStreamSource,
    ext_hint: Option<String>,
    display_name: String,
    fallback_tags: TrackTags,
}

impl AgentStreamHandle {
    pub(super) fn byte_len(&self) -> u64 {
        self.source.byte_len.unwrap_or(0)
    }

    fn into_stream_queue_item(self) -> StreamQueueItem {
        StreamQueueItem {
            source: Box::new(self.source),
            ext_hint: self.ext_hint,
            display_name: self.display_name,
            fallback_cover: None,
            fallback_tags: Some(self.fallback_tags),
        }
    }
}

// Agent playback hands off source, auth, runtime, and epoch data from the remote-control loop.
#[allow(clippy::too_many_arguments)]
pub(super) async fn play_source(
    player: &Player,
    _http: &Client,
    token: &str,
    runtime: &Arc<Mutex<AgentRuntimeState>>,
    base_url: &str,
    source: SourceRef,
    generation: u64,
    play_epoch: u64,
) -> Result<(), String> {
    let key = source.key();
    let prefetched = {
        let mut rt = runtime.lock().unwrap();
        retain_relevant_prefetches_with_preferred(&mut rt, Some(&key));
        rt.prefetched.remove(&key)
    };
    let handle = match prefetched {
        Some(handle) => handle,
        None => open_stream_source(token, base_url, &source).await?,
    };
    let still_current = {
        let rt = runtime.lock().unwrap();
        rt.generation == generation
            && rt
                .current_source
                .as_ref()
                .is_some_and(|current| current.key() == key)
    };
    if !still_current {
        return Ok(());
    }
    if !player.play_stream_if_epoch(
        play_epoch,
        Box::new(handle.source),
        handle.ext_hint,
        handle.display_name,
        None,
        Some(handle.fallback_tags),
        Vec::new(),
    ) {
        let mut rt = runtime.lock().unwrap();
        if rt.generation == generation {
            rt.loading_generation = None;
        }
        return Ok(());
    }
    let mut rt = runtime.lock().unwrap();
    if rt.generation == generation {
        rt.current_started_at = Some(Instant::now());
        rt.loading_generation = None;
        rt.was_active = false;
    }
    Ok(())
}

pub(super) async fn prefetch_source(
    player: &Player,
    _http: &Client,
    token: &str,
    runtime: &Arc<Mutex<AgentRuntimeState>>,
    base_url: &str,
    source: SourceRef,
) -> Result<(), String> {
    let key = source.key();
    {
        let mut rt = runtime.lock().unwrap();
        retain_relevant_prefetches(&mut rt);
        if !prefetch_key_is_relevant(&rt, &key)
            || rt.prefetched.contains_key(&key)
            || rt
                .engine_prefetched
                .as_ref()
                .is_some_and(|prefetched| prefetched.source.key() == key)
            || rt.prefetching_key.as_deref() == Some(key.as_str())
        {
            return Ok(());
        }
        rt.prefetching_key = Some(key.clone());
    }
    let result = open_stream_source(token, base_url, &source).await;
    let mut rt = runtime.lock().unwrap();
    if rt.prefetching_key.as_deref() == Some(key.as_str()) {
        rt.prefetching_key = None;
    }
    match result {
        Ok(handle) => {
            retain_relevant_prefetches(&mut rt);
            let can_arm_engine_queue = !rt.repeat_one
                && rt.queue.front().is_some_and(|queued| queued.key() == key)
                && rt.current_source.is_some();
            if can_arm_engine_queue {
                arm_handle_for_gapless(player, &mut rt, source, handle);
            } else if prefetch_key_is_relevant(&rt, &key) {
                insert_prefetched(&mut rt, key, handle);
            }
            Ok(())
        }
        Err(e) => Err(e),
    }
}

pub(super) fn arm_buffered_front_for_gapless(player: &Player, rt: &mut AgentRuntimeState) {
    if rt.repeat_one || rt.engine_prefetched.is_some() {
        return;
    }
    let Some(source) = rt.queue.front().cloned() else {
        return;
    };
    let Some(handle) = rt.prefetched.remove(&source.key()) else {
        return;
    };
    arm_handle_for_gapless(player, rt, source, handle);
}

fn arm_handle_for_gapless(
    player: &Player,
    rt: &mut AgentRuntimeState,
    source: SourceRef,
    handle: AgentStreamHandle,
) {
    let expected_current = rt.current_source.as_ref().map(source_display_name);
    let buffered_bytes = handle.byte_len();
    rt.engine_prefetched = Some(EnginePrefetchedSource {
        source,
        buffered_bytes,
        observed_in_player_queue: false,
    });
    player.set_stream_queue_if_epoch(
        vec![handle.into_stream_queue_item()],
        expected_current,
        Some(player.playback_epoch()),
    );
}

fn prefetch_key_is_relevant(rt: &AgentRuntimeState, key: &str) -> bool {
    rt.current_source
        .as_ref()
        .is_some_and(|source| source.key() == key)
        || rt.queue.iter().any(|source| source.key() == key)
}

pub(super) fn agent_source_matches_player_file(
    source: Option<&SourceRef>,
    player_file_name: Option<&str>,
) -> bool {
    source.is_some_and(|source| {
        player_file_name.is_some_and(|file_name| source_display_name(source) == file_name)
    })
}

pub(super) fn synchronize_gapless_engine_advance(
    rt: &mut AgentRuntimeState,
    player_file_name: Option<&str>,
    player_stream_queue_len: usize,
) -> bool {
    let current_matches =
        agent_source_matches_player_file(rt.current_source.as_ref(), player_file_name);
    let Some(prefetched) = rt.engine_prefetched.as_mut() else {
        return false;
    };
    if player_stream_queue_len > 0 {
        prefetched.observed_in_player_queue = true;
    }
    if !agent_source_matches_player_file(Some(&prefetched.source), player_file_name) {
        return false;
    }
    let observed_queue_consumed =
        prefetched.observed_in_player_queue && player_stream_queue_len == 0;
    if current_matches && !observed_queue_consumed {
        return false;
    }

    let advanced = prefetched.source.clone();
    let advanced_key = advanced.key();
    if let Some(index) = rt
        .queue
        .iter()
        .position(|source| source.key() == advanced_key)
    {
        rt.queue.drain(..=index);
    }
    rt.engine_prefetched = None;
    rt.generation = rt.generation.wrapping_add(1);
    rt.current_source = Some(advanced);
    rt.current_started_at = Some(Instant::now());
    rt.loading_generation = None;
    rt.was_active = true;
    rt.skip_requested = false;
    retain_relevant_prefetches(rt);
    true
}

pub(super) fn retain_relevant_prefetches(rt: &mut AgentRuntimeState) {
    retain_relevant_prefetches_with_preferred(rt, None);
}

pub(super) fn retain_relevant_prefetches_with_preferred(
    rt: &mut AgentRuntimeState,
    preferred_key: Option<&str>,
) {
    let relevant_keys = relevant_prefetch_keys(rt);
    rt.prefetched
        .retain(|key, _| relevant_keys.contains(key.as_str()));
    trim_prefetched_to_limit(rt, preferred_key);
}

fn relevant_prefetch_keys(rt: &AgentRuntimeState) -> HashSet<String> {
    let mut keys = HashSet::new();
    if let Some(source) = &rt.current_source {
        keys.insert(source.key());
    }
    keys.extend(rt.queue.iter().map(SourceRef::key));
    keys
}

fn insert_prefetched(rt: &mut AgentRuntimeState, key: String, handle: AgentStreamHandle) {
    rt.prefetched.insert(key.clone(), handle);
    trim_prefetched_to_limit(rt, Some(&key));
}

fn trim_prefetched_to_limit(rt: &mut AgentRuntimeState, preferred_key: Option<&str>) {
    while rt.prefetched.len() > AGENT_MAX_PREFETCHED_STREAMS {
        let victim = rt
            .prefetched
            .keys()
            .find(|key| preferred_key != Some(key.as_str()))
            .cloned()
            .or_else(|| rt.prefetched.keys().next().cloned());
        let Some(victim) = victim else {
            break;
        };
        rt.prefetched.remove(&victim);
    }
}

async fn open_stream_source(
    token: &str,
    base_url: &str,
    source: &SourceRef,
) -> Result<AgentStreamHandle, String> {
    let url = source_stream_url(base_url, source);
    let url = Url::parse(&url).map_err(|e| format!("parse stream URL: {e}"))?;
    let source_handle = AgentStreamSource::open(token, url).await?;
    Ok(AgentStreamHandle {
        source: source_handle,
        ext_hint: source_ext_hint(source),
        display_name: source_display_name(source),
        fallback_tags: source_fallback_tags(source),
    })
}

fn source_stream_url(base_url: &str, source: &SourceRef) -> String {
    let base = base_url.trim_end_matches('/');
    match source {
        SourceRef::LocalTrack { track_id, .. } => format!("{base}/api/stream/local/{track_id}"),
        SourceRef::QobuzTrack { track_id, .. } => format!("{base}/api/stream/qobuz/{track_id}"),
    }
}

pub(super) fn reachable_stream_base_url(advertised_base_url: &str, core_url: &str) -> String {
    let advertised = advertised_base_url.trim();
    if advertised.is_empty() {
        return core_url.trim_end_matches('/').to_string();
    }
    if url_uses_loopback(advertised) && !url_uses_loopback(core_url) {
        core_url.trim_end_matches('/').to_string()
    } else {
        advertised.trim_end_matches('/').to_string()
    }
}

fn url_uses_loopback(url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    let Some(host) = parsed.host_str() else {
        return false;
    };
    host.eq_ignore_ascii_case("localhost") || host == "::1" || host.starts_with("127.")
}

fn source_ext_hint(source: &SourceRef) -> Option<String> {
    match source {
        SourceRef::LocalTrack { ext_hint, .. } => ext_hint.clone(),
        SourceRef::QobuzTrack { .. } => Some("flac".to_string()),
    }
}

pub(super) fn source_display_name(source: &SourceRef) -> String {
    fn from_parts(title: Option<&str>, artist: Option<&str>, fallback: String) -> String {
        match (title, artist) {
            (Some(title), Some(artist)) if !title.is_empty() && !artist.is_empty() => {
                format!("{artist} - {title}")
            }
            (Some(title), _) if !title.is_empty() => title.to_string(),
            _ => fallback,
        }
    }

    match source {
        SourceRef::LocalTrack {
            title,
            artist,
            track_id,
            ..
        } => from_parts(
            title.as_deref(),
            artist.as_deref(),
            format!("local-{track_id}"),
        ),
        SourceRef::QobuzTrack {
            title,
            artist,
            track_id,
            ..
        } => from_parts(
            title.as_deref(),
            artist.as_deref(),
            format!("qobuz-{track_id}"),
        ),
    }
}

fn source_fallback_tags(source: &SourceRef) -> TrackTags {
    let mut tags = TrackTags::default();
    match source {
        SourceRef::LocalTrack {
            title,
            artist,
            album,
            ..
        }
        | SourceRef::QobuzTrack {
            title,
            artist,
            album,
            ..
        } => {
            tags.title = title.clone();
            tags.artist = artist.clone();
            tags.album = album.clone();
            tags.album_artist = artist.clone();
        }
    }
    tags
}

#[cfg(test)]
mod tests {
    use super::super::runtime::{AgentRuntimeState, EnginePrefetchedSource};
    use super::{
        agent_source_matches_player_file, reachable_stream_base_url,
        synchronize_gapless_engine_advance,
    };
    use crate::protocol::SourceRef;

    #[test]
    fn loopback_stream_base_uses_non_loopback_core_url() {
        let rewritten =
            reachable_stream_base_url("http://localhost:9090", "https://music.example.com");

        assert_eq!(rewritten, "https://music.example.com");
    }

    #[test]
    fn loopback_stream_base_does_not_preserve_advertised_port_or_path() {
        let rewritten = reachable_stream_base_url(
            "http://127.0.0.1:9090/audio",
            "https://music.example.com/proxy",
        );

        assert_eq!(rewritten, "https://music.example.com/proxy");
    }

    #[test]
    fn non_loopback_stream_base_is_left_alone() {
        let base = reachable_stream_base_url("http://10.0.0.50:9090", "http://10.0.0.12:3000");

        assert_eq!(base, "http://10.0.0.50:9090");
    }

    #[test]
    fn agent_source_active_check_requires_requested_display_name() {
        let fuses = SourceRef::QobuzTrack {
            track_id: 1,
            title: Some("Fuses".to_string()),
            artist: Some("Stereolab".to_string()),
            album: None,
            album_id: None,
            image_url: None,
            duration_secs: None,
            radio: false,
            radio_context: None,
            playlist_context: None,
        };

        assert!(agent_source_matches_player_file(
            Some(&fuses),
            Some("Stereolab - Fuses")
        ));
        assert!(!agent_source_matches_player_file(
            Some(&fuses),
            Some("Stereolab - People Do It All The Time")
        ));
        assert!(!agent_source_matches_player_file(Some(&fuses), None));
    }

    #[test]
    fn engine_gapless_advance_updates_agent_source_and_consumes_queue() {
        let current = qobuz_source_with_title(1, "Fuses");
        let next = qobuz_source_with_title(2, "People Do It All The Time");
        let later = qobuz_source_with_title(3, "The Free Design");
        let mut rt = AgentRuntimeState::new();
        rt.current_source = Some(current);
        rt.queue = [next.clone(), later.clone()].into();
        rt.engine_prefetched = Some(EnginePrefetchedSource {
            source: next.clone(),
            buffered_bytes: 1024,
            observed_in_player_queue: true,
        });

        assert!(synchronize_gapless_engine_advance(
            &mut rt,
            Some("Stereolab - People Do It All The Time"),
            0,
        ));
        assert_eq!(
            rt.current_source.as_ref().map(SourceRef::key),
            Some(next.key())
        );
        assert_eq!(rt.queue.front().map(SourceRef::key), Some(later.key()));
        assert!(rt.engine_prefetched.is_none());
        assert!(rt.was_active);
    }

    #[test]
    fn unrelated_player_metadata_does_not_consume_gapless_queue() {
        let current = qobuz_source_with_title(1, "Fuses");
        let next = qobuz_source_with_title(2, "People Do It All The Time");
        let mut rt = AgentRuntimeState::new();
        rt.current_source = Some(current);
        rt.queue = [next.clone()].into();
        rt.engine_prefetched = Some(EnginePrefetchedSource {
            source: next,
            buffered_bytes: 1024,
            observed_in_player_queue: true,
        });

        assert!(!synchronize_gapless_engine_advance(
            &mut rt,
            Some("Another Artist - Another Track"),
            0,
        ));
        assert_eq!(rt.queue.len(), 1);
        assert!(rt.engine_prefetched.is_some());
    }

    #[test]
    fn repeated_display_name_uses_consumed_player_queue_as_advance_signal() {
        let current = qobuz_source_with_title(1, "Interlude");
        let next = qobuz_source_with_title(2, "Interlude");
        let mut rt = AgentRuntimeState::new();
        rt.current_source = Some(current);
        rt.queue = [next.clone()].into();
        rt.engine_prefetched = Some(EnginePrefetchedSource {
            source: next.clone(),
            buffered_bytes: 1024,
            observed_in_player_queue: true,
        });

        assert!(synchronize_gapless_engine_advance(
            &mut rt,
            Some("Stereolab - Interlude"),
            0,
        ));
        assert_eq!(
            rt.current_source.as_ref().map(SourceRef::key),
            Some(next.key())
        );
        assert!(rt.queue.is_empty());
    }

    fn qobuz_source_with_title(track_id: u64, title: &str) -> SourceRef {
        SourceRef::QobuzTrack {
            track_id,
            title: Some(title.to_string()),
            artist: Some("Stereolab".to_string()),
            album: None,
            album_id: None,
            image_url: None,
            duration_secs: None,
            radio: false,
            radio_context: None,
            playlist_context: None,
        }
    }
}
