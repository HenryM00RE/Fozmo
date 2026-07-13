use crate::library::{Library, PlaybackHistoryInput};
use crate::protocol::SourceRef;
use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::Instant;

const COUNTED_PLAY_SECONDS: f64 = 30.0;
const COMPLETION_RATIO: f64 = 0.95;
const COMPLETION_TAIL_SECONDS: f64 = 2.0;
const PENDING_MATCH_GRACE_SECONDS: f64 = 20.0;

#[derive(Debug, Clone, Default, Serialize)]
pub struct PlaybackObservation {
    pub state: String,
    pub current_source: Option<SourceRef>,
    pub file_name: Option<String>,
    pub track_title: Option<String>,
    pub track_artist: Option<String>,
    pub track_album: Option<String>,
    pub zone_name: Option<String>,
    pub position_secs: f64,
    pub duration_secs: f64,
}

#[derive(Debug, Clone)]
struct ActiveListen {
    profile_id: String,
    source: SourceRef,
    zone_id: String,
    zone_name: String,
    queue: Vec<QueuedListen>,
    listened_secs: f64,
    duration_secs: Option<f64>,
    last_position_secs: f64,
    last_tick: Instant,
    started_at: Instant,
    waiting_for_queue_advance_since: Option<Instant>,
    pending_match: bool,
    is_playing: bool,
    radio: bool,
}

#[derive(Debug, Clone)]
struct QueuedListen {
    profile_id: String,
    source: SourceRef,
    radio: bool,
}

impl QueuedListen {
    fn new(source: SourceRef, radio: bool, profile_id: String) -> Self {
        Self {
            profile_id,
            source,
            radio,
        }
    }
}

#[derive(Default)]
pub struct ListeningTracker {
    active: Mutex<HashMap<String, ActiveListen>>,
    pending_queues: Mutex<HashMap<String, Vec<QueuedListen>>>,
    transferred_sources: Mutex<HashMap<String, SourceRef>>,
}

impl ListeningTracker {
    pub fn start(
        &self,
        library: &Library,
        zone_id: String,
        zone_name: String,
        profile_id: String,
        source: SourceRef,
        queue: Vec<SourceRef>,
    ) {
        self.start_with_radio(
            library, zone_id, zone_name, profile_id, source, queue, false,
        );
    }

    // Listening sessions persist the full zone/profile/source context at the start boundary.
    #[allow(clippy::too_many_arguments)]
    pub fn start_with_radio(
        &self,
        library: &Library,
        zone_id: String,
        zone_name: String,
        profile_id: String,
        source: SourceRef,
        queue: Vec<SourceRef>,
        radio: bool,
    ) {
        let mut active = self.active.lock().unwrap();
        self.pending_queues.lock().unwrap().remove(&zone_id);
        self.transferred_sources.lock().unwrap().remove(&zone_id);
        if let Some(previous) = active.remove(&zone_id) {
            finalize_listen(library, previous, false);
        }
        record_recent_source(library, &profile_id, &source, radio);
        let now = Instant::now();
        active.insert(
            zone_id.clone(),
            ActiveListen {
                profile_id: profile_id.clone(),
                source,
                zone_id,
                zone_name,
                queue: queue
                    .into_iter()
                    .map(|source| QueuedListen::new(source, radio, profile_id.clone()))
                    .collect(),
                listened_secs: 0.0,
                duration_secs: None,
                last_position_secs: 0.0,
                last_tick: now,
                started_at: now,
                waiting_for_queue_advance_since: None,
                pending_match: true,
                is_playing: false,
                radio,
            },
        );
    }

    pub fn set_queue(&self, zone_id: &str, profile_id: String, queue: Vec<SourceRef>) {
        self.set_queue_with_radio(zone_id, profile_id, queue, false);
    }

    pub fn set_queue_with_radio(
        &self,
        zone_id: &str,
        profile_id: String,
        queue: Vec<SourceRef>,
        radio: bool,
    ) {
        let queue = queue
            .into_iter()
            .map(|source| QueuedListen::new(source, radio, profile_id.clone()))
            .collect::<Vec<_>>();
        if let Some(active) = self.active.lock().unwrap().get_mut(zone_id) {
            active.queue = queue;
        } else if queue.is_empty() {
            self.pending_queues.lock().unwrap().remove(zone_id);
        } else {
            self.pending_queues
                .lock()
                .unwrap()
                .insert(zone_id.to_string(), queue);
        }
    }

    pub fn append_queue_with_radio(
        &self,
        zone_id: &str,
        profile_id: String,
        queue: Vec<SourceRef>,
        radio: bool,
    ) {
        if queue.is_empty() {
            return;
        }
        let queue = queue
            .into_iter()
            .map(|source| QueuedListen::new(source, radio, profile_id.clone()))
            .collect::<Vec<_>>();
        if let Some(active) = self.active.lock().unwrap().get_mut(zone_id) {
            active.queue.extend(queue);
        } else {
            self.pending_queues
                .lock()
                .unwrap()
                .entry(zone_id.to_string())
                .or_default()
                .extend(queue);
        }
    }

    pub fn active_source(&self, zone_id: &str) -> Option<SourceRef> {
        self.active
            .lock()
            .unwrap()
            .get(zone_id)
            .map(|active| active.source.clone())
    }

    pub fn profile_id(&self, zone_id: &str) -> Option<String> {
        self.active
            .lock()
            .unwrap()
            .get(zone_id)
            .map(|active| active.profile_id.clone())
            .or_else(|| {
                self.pending_queues
                    .lock()
                    .unwrap()
                    .get(zone_id)
                    .and_then(|queue| queue.first())
                    .map(|queued| queued.profile_id.clone())
            })
    }

    pub fn queued_sources(&self, zone_id: &str) -> Vec<SourceRef> {
        self.active
            .lock()
            .unwrap()
            .get(zone_id)
            .map(|active| {
                active
                    .queue
                    .iter()
                    .map(|queued| queued.source.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn transfer_to_zone(
        &self,
        source_zone_id: &str,
        destination_zone_id: String,
        destination_zone_name: String,
        position_secs: f64,
        duration_secs: f64,
        is_playing: bool,
    ) -> bool {
        if source_zone_id == destination_zone_id {
            return false;
        }
        let mut active = self.active.lock().unwrap();
        let Some(mut listen) = active.remove(source_zone_id) else {
            return false;
        };
        self.transferred_sources
            .lock()
            .unwrap()
            .insert(source_zone_id.to_string(), listen.source.clone());
        listen.zone_id = destination_zone_id.clone();
        listen.zone_name = destination_zone_name;
        if position_secs.is_finite() {
            listen.last_position_secs = position_secs.max(0.0);
        }
        if duration_secs.is_finite() && duration_secs > 0.0 {
            listen.duration_secs = Some(duration_secs);
        }
        listen.is_playing = is_playing;
        listen.last_tick = Instant::now();
        active.insert(destination_zone_id, listen);
        true
    }

    pub fn source_for_key(&self, zone_id: &str, key: &str) -> Option<SourceRef> {
        {
            let active = self.active.lock().unwrap();
            if let Some(current) = active.get(zone_id) {
                if current.source.key() == key {
                    return Some(current.source.clone());
                }
                if let Some(queued) = current.queue.iter().find(|q| q.source.key() == key) {
                    return Some(queued.source.clone());
                }
            }
        }
        self.pending_queues
            .lock()
            .unwrap()
            .get(zone_id)
            .and_then(|queue| queue.iter().find(|q| q.source.key() == key))
            .map(|queued| queued.source.clone())
    }

    pub fn source_for_file_name(
        &self,
        library: &Library,
        zone_id: &str,
        file_name: &str,
    ) -> Option<SourceRef> {
        let active = self.active.lock().unwrap();
        let current = active.get(zone_id)?;
        if source_file_name_matches(library, &current.source, file_name) {
            return Some(current.source.clone());
        }
        current
            .queue
            .iter()
            .find(|queued| source_file_name_matches(library, &queued.source, file_name))
            .map(|queued| queued.source.clone())
    }

    pub fn observe(
        &self,
        library: &Library,
        zone_id: &str,
        profile_id: String,
        observation: PlaybackObservation,
    ) {
        let mut finalize = Vec::new();
        let mut active = self.active.lock().unwrap();
        let Some(mut current) = active.remove(zone_id) else {
            drop(active);
            if self.observation_matches_transferred_source(library, zone_id, &observation) {
                return;
            }
            if observation.state == "Playing"
                && let Some(recovered) =
                    self.recover_active_listen(library, zone_id, profile_id, &observation)
            {
                record_recent_source(
                    library,
                    &recovered.profile_id,
                    &recovered.source,
                    recovered.radio,
                );
                self.active
                    .lock()
                    .unwrap()
                    .insert(zone_id.to_string(), recovered);
            }
            return;
        };
        self.transferred_sources.lock().unwrap().remove(zone_id);

        let now = Instant::now();
        let is_playing = observation.state == "Playing";
        let matches_current = source_matches_observation(library, &current.source, &observation);

        if observation.state == "Stopped" {
            merge_observed_progress(&mut current, &observation);
            if current.pending_match
                && now.duration_since(current.started_at).as_secs_f64()
                    < PENDING_MATCH_GRACE_SECONDS
            {
                current.last_tick = now;
                active.insert(zone_id.to_string(), current);
                return;
            }
            let completed = is_complete(current.last_position_secs, current.duration_secs);
            if completed && !current.queue.is_empty() {
                let waiting_since = *current.waiting_for_queue_advance_since.get_or_insert(now);
                if now.duration_since(waiting_since).as_secs_f64() < PENDING_MATCH_GRACE_SECONDS {
                    current.last_tick = now;
                    active.insert(zone_id.to_string(), current);
                    return;
                }
            }
            finalize.push((current, completed));
            drop(active);
            for (listen, completed) in finalize {
                finalize_listen(library, listen, completed);
            }
            return;
        }

        if !matches_current {
            if current.pending_match
                && now.duration_since(current.started_at).as_secs_f64()
                    < PENDING_MATCH_GRACE_SECONDS
            {
                current.last_tick = now;
                active.insert(zone_id.to_string(), current);
                return;
            }
            if let Some(next_index) = current.queue.iter().position(|queued| {
                source_matches_observation(library, &queued.source, &observation)
            }) {
                let mut queue = current.queue.split_off(next_index);
                let next = queue.remove(0);
                let zone_name = current.zone_name.clone();
                let radio = next.radio;
                let remaining_sources = queue
                    .iter()
                    .map(|queued| queued.source.clone())
                    .collect::<Vec<_>>();
                finalize.push((current, true));
                record_recent_source(library, &next.profile_id, &next.source, next.radio);
                active.insert(
                    zone_id.to_string(),
                    ActiveListen {
                        profile_id: next.profile_id,
                        source: next.source,
                        zone_id: zone_id.to_string(),
                        zone_name,
                        queue,
                        listened_secs: 0.0,
                        duration_secs: duration_option(observation.duration_secs),
                        last_position_secs: observation.position_secs.max(0.0),
                        last_tick: now,
                        started_at: now,
                        waiting_for_queue_advance_since: None,
                        pending_match: false,
                        is_playing,
                        radio,
                    },
                );
                let _ = library.set_zone_queue(zone_id, &remaining_sources);
            } else {
                // A manual play probably arrived through another path. Close out
                // the old session. If the explicit start call was missed, recover
                // a best-effort session from the current observation.
                finalize.push((current, false));
                if observation.state == "Playing"
                    && let Some(recovered) =
                        self.recover_active_listen(library, zone_id, profile_id, &observation)
                {
                    record_recent_source(
                        library,
                        &recovered.profile_id,
                        &recovered.source,
                        recovered.radio,
                    );
                    active.insert(zone_id.to_string(), recovered);
                }
            }
            drop(active);
            for (listen, completed) in finalize {
                finalize_listen(library, listen, completed);
            }
            return;
        }

        current.pending_match = false;
        current.waiting_for_queue_advance_since = None;
        if is_playing {
            let delta = now.duration_since(current.last_tick).as_secs_f64();
            if delta.is_finite() && delta > 0.0 && delta < 5.0 {
                current.listened_secs += delta;
            }
        }
        current.last_tick = now;
        update_observed_progress(&mut current, &observation);
        current.is_playing = is_playing;

        active.insert(zone_id.to_string(), current);
        drop(active);
        for (listen, completed) in finalize {
            finalize_listen(library, listen, completed);
        }
    }

    pub fn stop(&self, library: &Library, zone_id: &str) {
        self.transferred_sources.lock().unwrap().remove(zone_id);
        if let Some(active) = self.active.lock().unwrap().remove(zone_id) {
            let completed = is_complete(active.last_position_secs, active.duration_secs);
            finalize_listen(library, active, completed);
        }
    }

    pub fn next(&self, library: &Library, zone_id: &str) {
        self.transferred_sources.lock().unwrap().remove(zone_id);
        let mut active = self.active.lock().unwrap();
        let Some(mut current) = active.remove(zone_id) else {
            return;
        };
        let next = if current.queue.is_empty() {
            None
        } else {
            Some(current.queue.remove(0))
        };
        let now = Instant::now();
        let zone_name = current.zone_name.clone();
        let rest = current.queue.clone();
        let remaining_sources = rest
            .iter()
            .map(|queued| queued.source.clone())
            .collect::<Vec<_>>();
        if let Some(next) = next {
            record_recent_source(library, &next.profile_id, &next.source, next.radio);
            active.insert(
                zone_id.to_string(),
                ActiveListen {
                    profile_id: next.profile_id,
                    source: next.source,
                    zone_id: zone_id.to_string(),
                    zone_name,
                    queue: rest,
                    listened_secs: 0.0,
                    duration_secs: None,
                    last_position_secs: 0.0,
                    last_tick: now,
                    started_at: now,
                    waiting_for_queue_advance_since: None,
                    pending_match: true,
                    is_playing: false,
                    radio: next.radio,
                },
            );
        }
        drop(active);
        let _ = library.set_zone_queue(zone_id, &remaining_sources);
        finalize_listen(library, current, false);
    }

    fn recover_active_listen(
        &self,
        library: &Library,
        zone_id: &str,
        profile_id: String,
        observation: &PlaybackObservation,
    ) -> Option<ActiveListen> {
        let (queued, queue) = self
            .recover_from_pending_queue(library, zone_id, observation)
            .or_else(|| {
                recover_local_source(library, observation)
                    .map(|source| (QueuedListen::new(source, false, profile_id), Vec::new()))
            })?;
        let radio = queued.radio;
        let now = Instant::now();
        Some(ActiveListen {
            profile_id: queued.profile_id,
            source: queued.source,
            zone_id: zone_id.to_string(),
            zone_name: observation
                .zone_name
                .clone()
                .unwrap_or_else(|| zone_id.to_string()),
            queue,
            listened_secs: recovered_listened_secs(observation),
            duration_secs: duration_option(observation.duration_secs),
            last_position_secs: observation.position_secs.max(0.0),
            last_tick: now,
            started_at: now,
            waiting_for_queue_advance_since: None,
            pending_match: false,
            is_playing: observation.state == "Playing",
            radio,
        })
    }

    fn recover_from_pending_queue(
        &self,
        library: &Library,
        zone_id: &str,
        observation: &PlaybackObservation,
    ) -> Option<(QueuedListen, Vec<QueuedListen>)> {
        let mut pending = self.pending_queues.lock().unwrap();
        let mut queue = pending.remove(zone_id)?;
        let Some(index) = queue
            .iter()
            .position(|queued| source_matches_observation(library, &queued.source, observation))
        else {
            pending.insert(zone_id.to_string(), queue);
            return None;
        };
        let mut rest = queue.split_off(index);
        let source = rest.remove(0);
        Some((source, rest))
    }

    fn observation_matches_transferred_source(
        &self,
        library: &Library,
        zone_id: &str,
        observation: &PlaybackObservation,
    ) -> bool {
        let mut transferred = self.transferred_sources.lock().unwrap();
        let Some(source) = transferred.get(zone_id) else {
            return false;
        };
        if observation.state == "Playing"
            && source_matches_observation(library, source, observation)
        {
            return true;
        }
        transferred.remove(zone_id);
        false
    }

    pub fn active_history_inputs(&self) -> Vec<PlaybackHistoryInput> {
        let now = Instant::now();
        self.active
            .lock()
            .unwrap()
            .values()
            .map(|listen| {
                let mut played_secs = listen.listened_secs.max(0.0);
                let delta = now.duration_since(listen.last_tick).as_secs_f64();
                if listen.is_playing && delta.is_finite() && delta > 0.0 && delta < 5.0 {
                    played_secs += delta;
                }
                PlaybackHistoryInput {
                    profile_id: Some(listen.profile_id.clone()),
                    source: listen.source.clone(),
                    zone_id: listen.zone_id.clone(),
                    zone_name: listen.zone_name.clone(),
                    played_secs: Some(played_secs),
                    duration_secs: listen.duration_secs,
                    completed: false,
                    counted: counted_play(played_secs, listen.duration_secs, false),
                    radio: listen.radio,
                }
            })
            .collect()
    }
}

fn record_recent_source(library: &Library, profile_id: &str, source: &SourceRef, radio: bool) {
    if radio || source.is_radio() {
        return;
    }
    if let Some(playlist_id) = source.playlist_id() {
        let _ = library.record_playlist_played(playlist_id);
        return;
    }
    let _ = library.record_recent_album_for_source(Some(profile_id), source);
}

fn finalize_listen(library: &Library, listen: ActiveListen, completed: bool) {
    let played_secs = listen.listened_secs.max(0.0);
    let duration_secs = listen.duration_secs;
    let counted = counted_play(played_secs, duration_secs, completed);
    if played_secs <= 0.25 && !counted {
        return;
    }
    let _ = library.record_playback_history(PlaybackHistoryInput {
        profile_id: Some(listen.profile_id),
        source: listen.source,
        zone_id: listen.zone_id,
        zone_name: listen.zone_name,
        played_secs: Some(played_secs),
        duration_secs,
        completed,
        counted,
        radio: listen.radio,
    });
}

fn counted_play(played_secs: f64, duration_secs: Option<f64>, completed: bool) -> bool {
    played_secs >= COUNTED_PLAY_SECONDS
        || (completed && duration_secs.is_some_and(|duration| duration < COUNTED_PLAY_SECONDS))
}

fn is_complete(position_secs: f64, duration_secs: Option<f64>) -> bool {
    let Some(duration_secs) = duration_secs.filter(|duration| *duration > 0.0) else {
        return false;
    };
    position_secs >= duration_secs * COMPLETION_RATIO
        || duration_secs - position_secs <= COMPLETION_TAIL_SECONDS
}

fn duration_option(duration_secs: f64) -> Option<f64> {
    (duration_secs > 0.0 && duration_secs.is_finite()).then_some(duration_secs)
}

fn merge_observed_progress(current: &mut ActiveListen, observation: &PlaybackObservation) {
    if observation.position_secs.is_finite() {
        current.last_position_secs = current
            .last_position_secs
            .max(observation.position_secs.max(0.0));
    }
    if let Some(duration_secs) = duration_option(observation.duration_secs) {
        current.duration_secs = Some(duration_secs);
    }
}

fn update_observed_progress(current: &mut ActiveListen, observation: &PlaybackObservation) {
    if observation.position_secs.is_finite() {
        current.last_position_secs = observation.position_secs.max(0.0);
    }
    if let Some(duration_secs) = duration_option(observation.duration_secs) {
        current.duration_secs = Some(duration_secs);
    }
}

fn recovered_listened_secs(observation: &PlaybackObservation) -> f64 {
    let position = observation.position_secs;
    if !position.is_finite() || position <= 0.0 {
        return 0.0;
    }
    match duration_option(observation.duration_secs) {
        Some(duration) => position.min(duration).max(0.0),
        None => position.max(0.0),
    }
}

fn recover_local_source(library: &Library, observation: &PlaybackObservation) -> Option<SourceRef> {
    let file_name = observation.file_name.as_deref()?;
    let track_id = library.track_id_for_file_name(file_name).ok().flatten()?;
    Some(SourceRef::LocalTrack {
        track_id,
        file_name: Some(file_name.to_string()),
        title: observation.track_title.clone(),
        artist: observation.track_artist.clone(),
        album: observation.track_album.clone(),
        album_artist: None,
        album_id: None,
        art_id: None,
        duration_secs: None,
        ext_hint: Path::new(file_name)
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.trim_start_matches('.').to_ascii_lowercase())
            .filter(|ext| !ext.is_empty()),
        radio: false,
        radio_context: None,
        playlist_context: None,
    })
}

fn source_matches_observation(
    library: &Library,
    source: &SourceRef,
    observation: &PlaybackObservation,
) -> bool {
    if let Some(observed_source) = observation.current_source.as_ref() {
        return observed_source.key() == source.key();
    }
    match source {
        SourceRef::LocalTrack {
            track_id,
            title,
            artist,
            album,
            ..
        } => {
            if let Some(file_name) = observation.file_name.as_deref()
                && library
                    .track_path(*track_id)
                    .ok()
                    .flatten()
                    .and_then(|path| {
                        path.file_name()
                            .and_then(|name| name.to_str())
                            .map(str::to_string)
                    })
                    .as_deref()
                    == Some(
                        Path::new(file_name)
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or(file_name),
                    )
            {
                return true;
            }
            loose_metadata_match(
                title.as_deref(),
                artist.as_deref(),
                album.as_deref(),
                &observation.track_title,
                &observation.track_artist,
                &observation.track_album,
            )
        }
        SourceRef::QobuzTrack {
            track_id,
            title,
            artist,
            album,
            ..
        } => {
            if let Some(file_name) = observation.file_name.as_deref() {
                if file_name == format!("qobuz:{track_id}") {
                    return true;
                }
                if let (Some(artist), Some(title)) = (artist.as_deref(), title.as_deref())
                    && text_eq(Some(file_name), Some(&format!("{artist} - {title}")))
                {
                    return true;
                }
                if text_eq(Some(file_name), title.as_deref()) {
                    return true;
                }
            }
            loose_metadata_match(
                title.as_deref(),
                artist.as_deref(),
                album.as_deref(),
                &observation.track_title,
                &observation.track_artist,
                &observation.track_album,
            )
        }
    }
}

fn source_file_name_matches(library: &Library, source: &SourceRef, file_name: &str) -> bool {
    match source {
        SourceRef::LocalTrack { track_id, .. } => {
            library
                .track_path(*track_id)
                .ok()
                .flatten()
                .and_then(|path| {
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .map(str::to_string)
                })
                .as_deref()
                == Some(
                    Path::new(file_name)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or(file_name),
                )
        }
        SourceRef::QobuzTrack {
            track_id,
            title,
            artist,
            ..
        } => {
            if file_name == format!("qobuz:{track_id}") {
                return true;
            }
            if let (Some(artist), Some(title)) = (artist.as_deref(), title.as_deref())
                && text_eq(Some(file_name), Some(&format!("{artist} - {title}")))
            {
                return true;
            }
            text_eq(Some(file_name), title.as_deref())
        }
    }
}

fn loose_metadata_match(
    title: Option<&str>,
    artist: Option<&str>,
    album: Option<&str>,
    observed_title: &Option<String>,
    observed_artist: &Option<String>,
    observed_album: &Option<String>,
) -> bool {
    let title_matches = text_eq(title, observed_title.as_deref());
    if !title_matches {
        return false;
    }
    let artist_matches = comparable_text_match(artist, observed_artist.as_deref());
    let album_matches = comparable_text_match(album, observed_album.as_deref());
    if artist_matches == Some(false) || album_matches == Some(false) {
        return false;
    }
    artist_matches == Some(true) || album_matches == Some(true) || {
        artist_matches.is_none()
            && album_matches.is_none()
            && no_context(
                title,
                artist,
                album,
                observed_title,
                observed_artist,
                observed_album,
            )
    }
}

fn text_eq(a: Option<&str>, b: Option<&str>) -> bool {
    let normalize = |value: &str| value.trim().to_lowercase();
    a.zip(b)
        .map(|(a, b)| !a.trim().is_empty() && normalize(a) == normalize(b))
        .unwrap_or(false)
}

fn comparable_text_match(a: Option<&str>, b: Option<&str>) -> Option<bool> {
    let a = a.map(str::trim).filter(|value| !value.is_empty())?;
    let b = b.map(str::trim).filter(|value| !value.is_empty())?;
    Some(text_eq(Some(a), Some(b)))
}

fn no_context(
    _title: Option<&str>,
    artist: Option<&str>,
    album: Option<&str>,
    _observed_title: &Option<String>,
    observed_artist: &Option<String>,
    observed_album: &Option<String>,
) -> bool {
    artist.is_none_or(|value| value.trim().is_empty())
        && album.is_none_or(|value| value.trim().is_empty())
        && observed_artist
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
        && observed_album
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counted_after_thirty_seconds() {
        assert!(!counted_play(29.9, Some(180.0), false));
        assert!(counted_play(30.0, Some(180.0), false));
    }

    #[test]
    fn short_tracks_count_when_completed() {
        assert!(!counted_play(10.0, Some(20.0), false));
        assert!(counted_play(10.0, Some(20.0), true));
    }

    #[test]
    fn completion_allows_tail_or_ratio() {
        assert!(is_complete(95.0, Some(100.0)));
        assert!(is_complete(118.1, Some(120.0)));
        assert!(!is_complete(50.0, Some(120.0)));
    }

    #[test]
    fn metadata_match_requires_title_and_some_context_when_present() {
        let observed_title = Some("Song".to_string());
        let observed_artist = Some("Artist".to_string());
        let observed_album = Some("Album".to_string());
        assert!(!loose_metadata_match(
            Some("song"),
            Some("artist"),
            Some("different"),
            &observed_title,
            &observed_artist,
            &observed_album
        ));
        assert!(!loose_metadata_match(
            Some("other"),
            Some("artist"),
            Some("album"),
            &observed_title,
            &observed_artist,
            &observed_album
        ));
        assert!(loose_metadata_match(
            Some("song"),
            Some("artist"),
            Some("album"),
            &observed_title,
            &observed_artist,
            &observed_album
        ));
        assert!(!loose_metadata_match(
            Some("song"),
            Some("other artist"),
            None,
            &observed_title,
            &observed_artist,
            &None
        ));
        assert!(loose_metadata_match(
            Some("song"),
            None,
            None,
            &observed_title,
            &None,
            &None
        ));
    }

    #[test]
    fn qobuz_match_accepts_stream_display_name_without_tags() {
        let source = qobuz_test_source(231920122, "Wall Of Eyes");
        let observation = PlaybackObservation {
            state: "Playing".to_string(),
            file_name: Some("The Smile - Wall Of Eyes".to_string()),
            ..PlaybackObservation::default()
        };

        assert!(source_matches_observation(
            &test_library("qobuz-display-match"),
            &source,
            &observation
        ));
    }

    #[test]
    fn exact_reported_source_rejects_duplicate_metadata_match() {
        let first = qobuz_test_source(1, "Duplicate");
        let second = qobuz_test_source(2, "Duplicate");
        let observation = PlaybackObservation {
            state: "Playing".to_string(),
            current_source: Some(second.clone()),
            file_name: Some("Artist - Duplicate".to_string()),
            track_title: Some("Duplicate".to_string()),
            track_artist: Some("Artist".to_string()),
            track_album: Some("Album".to_string()),
            ..PlaybackObservation::default()
        };

        assert!(!source_matches_observation(
            &test_library("exact-source-duplicate-first"),
            &first,
            &observation
        ));
        assert!(source_matches_observation(
            &test_library("exact-source-duplicate-second"),
            &second,
            &observation
        ));
    }

    #[test]
    fn pending_qobuz_start_survives_fozmo_stopped_observation() {
        let library = test_library("qobuz-pending-stopped");
        let tracker = ListeningTracker::default();
        let source = qobuz_test_source(231920122, "Wall Of Eyes");
        tracker.start(
            &library,
            "local-core".to_string(),
            "Local".to_string(),
            "default".to_string(),
            source,
            Vec::new(),
        );

        tracker.observe(
            &library,
            "local-core",
            "default".to_string(),
            PlaybackObservation {
                state: "Stopped".to_string(),
                ..PlaybackObservation::default()
            },
        );

        let live = tracker.active_history_inputs();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].played_secs, Some(0.0));
    }

    #[test]
    fn stopped_observation_final_position_can_mark_track_completed() {
        let library = test_library("stopped-final-position");
        let tracker = ListeningTracker::default();
        let source = qobuz_test_source(1, "Final Seconds");
        tracker.start(
            &library,
            "local-core".to_string(),
            "Local".to_string(),
            "default".to_string(),
            source,
            Vec::new(),
        );
        tracker.observe(
            &library,
            "local-core",
            "default".to_string(),
            PlaybackObservation {
                state: "Playing".to_string(),
                file_name: Some("The Smile - Final Seconds".to_string()),
                position_secs: 5.0,
                duration_secs: 10.0,
                ..PlaybackObservation::default()
            },
        );

        tracker.observe(
            &library,
            "local-core",
            "default".to_string(),
            PlaybackObservation {
                state: "Stopped".to_string(),
                position_secs: 9.6,
                duration_secs: 10.0,
                ..PlaybackObservation::default()
            },
        );

        let recent = library.recent_playback_history(1, true).unwrap();
        assert_eq!(recent.len(), 1);
        assert!(recent[0].completed);
        assert!(recent[0].counted);
    }

    #[test]
    fn transfer_to_zone_preserves_active_listen_without_history() {
        let library = test_library("transfer-active-listen");
        let tracker = ListeningTracker::default();
        let source = qobuz_test_source(1, "One");
        let next = qobuz_test_source(2, "Two");
        tracker.start(
            &library,
            "lounge".to_string(),
            "Lounge".to_string(),
            "profile-a".to_string(),
            source.clone(),
            vec![next.clone()],
        );
        tracker.observe(
            &library,
            "lounge",
            "profile-b".to_string(),
            observation_for_source("Playing", &source, 42.0, 120.0),
        );
        assert!(tracker.transfer_to_zone(
            "lounge",
            "kitchen".to_string(),
            "Kitchen".to_string(),
            119.0,
            120.0,
            true
        ));

        assert!(tracker.active_source("lounge").is_none());
        assert_eq!(
            tracker.active_source("kitchen").unwrap().key(),
            source.key()
        );
        assert_eq!(
            tracker
                .queued_sources("kitchen")
                .into_iter()
                .map(|source| source.key())
                .collect::<Vec<_>>(),
            vec![next.key()]
        );
        assert!(
            library
                .recent_playback_history(10, true)
                .unwrap()
                .is_empty()
        );
        let live = tracker.active_history_inputs();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].zone_id, "kitchen");
        assert_eq!(live[0].zone_name, "Kitchen");
        assert_eq!(live[0].profile_id.as_deref(), Some("profile-a"));
    }

    #[test]
    fn playlist_sourced_play_records_playlist_without_recent_album() {
        let library = test_library("playlist-recent-source");
        let tracker = ListeningTracker::default();
        let playlist_id = "playlist-recent-source-id";
        library
            .save_playlist(
                playlist_id,
                crate::library::PlaylistSaveRequest {
                    name: Some("Stereothom".to_string()),
                    created_at: Some(1),
                    updated_at: Some(1),
                    items: Vec::new(),
                },
            )
            .unwrap();
        let mut source = qobuz_test_source(231920122, "Wall Of Eyes");
        let SourceRef::QobuzTrack {
            playlist_context, ..
        } = &mut source
        else {
            panic!("expected qobuz source");
        };
        *playlist_context = Some(crate::protocol::PlaylistContext {
            playlist_id: playlist_id.to_string(),
        });

        tracker.start(
            &library,
            "local-core".to_string(),
            "Local".to_string(),
            "default".to_string(),
            source,
            Vec::new(),
        );

        assert!(library.recent_albums(50).unwrap().is_empty());
        let playlists = library.recent_playlists(50).unwrap();
        assert_eq!(playlists.len(), 1);
        assert_eq!(playlists[0].playlist_id, playlist_id);
        assert_eq!(playlists[0].title, "Stereothom");
    }

    #[test]
    fn completed_local_stop_keeps_queue_for_worker_auto_advance() {
        let library = test_library("local-stopped-keeps-queue");
        let tracker = ListeningTracker::default();
        let zone_id = "local-core";
        let current = local_test_source(1, "Coldplay Song", "Coldplay");
        let next = local_test_source(2, "Radiohead One", "Radiohead");
        let remaining = local_test_source(3, "Radiohead Two", "Radiohead");
        library
            .upsert_zone_definition(zone_id, "Local", "local_coreaudio", None, true)
            .unwrap();
        library
            .set_zone_queue(zone_id, &[next.clone(), remaining.clone()])
            .unwrap();
        tracker.start(
            &library,
            zone_id.to_string(),
            "Local".to_string(),
            "default".to_string(),
            current.clone(),
            vec![next.clone(), remaining.clone()],
        );
        tracker.observe(
            &library,
            zone_id,
            "default".to_string(),
            observation_for_source("Playing", &current, 5.0, 10.0),
        );

        tracker.observe(
            &library,
            zone_id,
            "default".to_string(),
            observation_for_source("Stopped", &current, 10.0, 10.0),
        );

        assert_eq!(tracker.active_source(zone_id).unwrap().key(), current.key());

        tracker.observe(
            &library,
            zone_id,
            "default".to_string(),
            observation_for_source("Playing", &next, 0.25, 10.0),
        );

        assert_eq!(tracker.active_source(zone_id).unwrap().key(), next.key());
        let saved_queue = library.zone_queue(zone_id).unwrap();
        assert_eq!(saved_queue.len(), 1);
        assert_eq!(saved_queue[0].source.key(), remaining.key());
        let active = tracker.active.lock().unwrap();
        let listen = active.get(zone_id).unwrap();
        assert_eq!(listen.queue.len(), 1);
        assert_eq!(listen.queue[0].source.key(), remaining.key());
    }

    #[test]
    fn recovered_pending_queue_moves_remaining_items_to_active_only() {
        let library = test_library("pending-queue-ownership");
        let tracker = ListeningTracker::default();
        let zone_id = "local-core";
        let first = qobuz_test_source(1, "One");
        let second = qobuz_test_source(2, "Two");
        let third = qobuz_test_source(3, "Three");
        tracker.set_queue_with_radio(
            zone_id,
            "default".to_string(),
            vec![first, second.clone(), third.clone()],
            true,
        );

        tracker.observe(
            &library,
            zone_id,
            "default".to_string(),
            PlaybackObservation {
                state: "Playing".to_string(),
                file_name: Some("The Smile - Two".to_string()),
                position_secs: 1.0,
                duration_secs: 10.0,
                ..PlaybackObservation::default()
            },
        );

        assert!(
            tracker
                .pending_queues
                .lock()
                .unwrap()
                .get(zone_id)
                .is_none()
        );
        let active = tracker.active.lock().unwrap();
        let listen = active.get(zone_id).unwrap();
        assert_eq!(listen.source.key(), second.key());
        assert_eq!(listen.queue.len(), 1);
        assert_eq!(listen.queue[0].source.key(), third.key());
    }

    #[test]
    fn next_persists_remaining_queue_after_consuming_prefetched_item() {
        let library = test_library("next-persist-remaining");
        let tracker = ListeningTracker::default();
        let zone_id = "sonos-zone";
        let first = qobuz_test_source(1, "One");
        let second = qobuz_test_source(2, "Two");
        library
            .upsert_zone_definition(zone_id, "Sonos", "sonos_upnp", None, true)
            .unwrap();
        library
            .set_zone_queue(zone_id, std::slice::from_ref(&second))
            .unwrap();
        tracker.start_with_radio(
            &library,
            zone_id.to_string(),
            "Sonos".to_string(),
            "default".to_string(),
            first,
            vec![second.clone()],
            true,
        );

        tracker.next(&library, zone_id);

        assert!(library.zone_queue(zone_id).unwrap().is_empty());
        assert_eq!(tracker.active_source(zone_id).unwrap().key(), second.key());
    }

    #[test]
    fn append_queue_with_radio_extends_active_queue() {
        let library = test_library("append-radio-queue");
        let tracker = ListeningTracker::default();
        let zone_id = "local-core";
        let current = qobuz_test_source(1, "One");
        let first = qobuz_test_source(2, "Two");
        let second = qobuz_test_source(3, "Three");
        tracker.start(
            &library,
            zone_id.to_string(),
            "Local".to_string(),
            "default".to_string(),
            current,
            vec![first.clone()],
        );

        tracker.append_queue_with_radio(zone_id, "default".to_string(), vec![second.clone()], true);

        let queued = tracker.queued_sources(zone_id);
        assert_eq!(queued.len(), 2);
        assert_eq!(queued[0].key(), first.key());
        assert_eq!(queued[1].key(), second.key());
        let active = tracker.active.lock().unwrap();
        let listen = active.get(zone_id).unwrap();
        assert!(listen.queue[1].radio);
    }

    #[test]
    fn pending_match_times_out_for_continuously_mismatched_playing_track() {
        let library = test_library("pending-match-timeout");
        let tracker = ListeningTracker::default();
        let zone_id = "local-core";
        let first = qobuz_test_source(1, "One");
        let second = qobuz_test_source(2, "Two");
        tracker.start(
            &library,
            zone_id.to_string(),
            "Local".to_string(),
            "default".to_string(),
            first.clone(),
            vec![second],
        );
        tracker.next(&library, zone_id);
        {
            let mut active = tracker.active.lock().unwrap();
            active.get_mut(zone_id).unwrap().started_at =
                Instant::now() - std::time::Duration::from_secs(30);
        }

        tracker.observe(
            &library,
            zone_id,
            "default".to_string(),
            PlaybackObservation {
                state: "Playing".to_string(),
                file_name: Some("The Smile - One".to_string()),
                position_secs: 2.0,
                duration_secs: 10.0,
                ..PlaybackObservation::default()
            },
        );

        assert!(tracker.active_source(zone_id).is_none());
    }

    fn qobuz_test_source(track_id: u64, title: &str) -> SourceRef {
        SourceRef::QobuzTrack {
            track_id,
            title: Some(title.to_string()),
            artist: Some("The Smile".to_string()),
            album: Some("Wall Of Eyes".to_string()),
            album_id: Some("ogmrf6hyzd6ja".to_string()),
            image_url: None,
            duration_secs: Some(10.0),
            radio: false,
            radio_context: None,
            playlist_context: None,
        }
    }

    fn local_test_source(track_id: i64, title: &str, artist: &str) -> SourceRef {
        SourceRef::LocalTrack {
            track_id,
            file_name: Some(format!("{artist} - {title}.flac")),
            title: Some(title.to_string()),
            artist: Some(artist.to_string()),
            album: Some("Album".to_string()),
            album_artist: Some(artist.to_string()),
            album_id: None,
            art_id: None,
            duration_secs: Some(10.0),
            ext_hint: Some("flac".to_string()),
            radio: false,
            radio_context: None,
            playlist_context: None,
        }
    }

    fn observation_for_source(
        state: &str,
        source: &SourceRef,
        position_secs: f64,
        duration_secs: f64,
    ) -> PlaybackObservation {
        match source {
            SourceRef::LocalTrack {
                file_name,
                title,
                artist,
                album,
                ..
            } => PlaybackObservation {
                state: state.to_string(),
                file_name: file_name.clone(),
                track_title: title.clone(),
                track_artist: artist.clone(),
                track_album: album.clone(),
                position_secs,
                duration_secs,
                ..PlaybackObservation::default()
            },
            SourceRef::QobuzTrack {
                track_id,
                title,
                artist,
                album,
                ..
            } => PlaybackObservation {
                state: state.to_string(),
                file_name: artist
                    .as_deref()
                    .zip(title.as_deref())
                    .map(|(artist, title)| format!("{artist} - {title}"))
                    .or_else(|| Some(format!("qobuz:{track_id}"))),
                track_title: title.clone(),
                track_artist: artist.clone(),
                track_album: album.clone(),
                position_secs,
                duration_secs,
                ..PlaybackObservation::default()
            },
        }
    }

    fn test_library(name: &str) -> Library {
        let root = std::env::temp_dir().join(format!(
            "fozmo-listening-{name}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        Library::new(
            root.join("library.db"),
            vec![root.join("music")],
            root.join("art"),
        )
        .unwrap()
    }
}
