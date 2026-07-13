use crate::protocol::{PlaylistContext, RadioContext, SourceRef};
use crate::services::qobuz::{QobuzPlayRequest, QobuzQueueTrack, QobuzTrack};

pub(crate) fn qobuz_source_ref_from_play_request(req: &QobuzPlayRequest) -> SourceRef {
    SourceRef::QobuzTrack {
        track_id: req.track_id,
        title: req.title.clone(),
        artist: req.artist.clone(),
        album: req.album.clone(),
        album_id: req.album_id.clone(),
        image_url: req.image_url.clone(),
        duration_secs: req.duration_secs,
        radio: req.radio_auto,
        radio_context: None,
        playlist_context: req.playlist_context.clone(),
    }
}

pub(crate) fn qobuz_source_ref_from_queue_track(
    track: &QobuzQueueTrack,
    fallback_radio: bool,
) -> SourceRef {
    SourceRef::QobuzTrack {
        track_id: track.track_id,
        title: track.title.clone(),
        artist: track.artist.clone(),
        album: track.album.clone(),
        album_id: track.album_id.clone(),
        image_url: track.image_url.clone(),
        duration_secs: track.duration_secs,
        radio: track.radio || fallback_radio,
        radio_context: None,
        playlist_context: track.playlist_context.clone(),
    }
}

pub(crate) fn qobuz_source_ref_from_track(track: &QobuzTrack, radio: bool) -> SourceRef {
    SourceRef::QobuzTrack {
        track_id: track.id,
        title: Some(track.title.clone()),
        artist: Some(track.artist.clone()),
        album: Some(track.album.clone()),
        album_id: track.album_id.clone(),
        image_url: track.image_url.clone(),
        duration_secs: (track.duration > 0).then_some(track.duration as f64),
        radio,
        radio_context: None,
        playlist_context: None,
    }
}

pub(crate) fn qobuz_queue_track_from_source_ref(source: &SourceRef) -> Option<QobuzQueueTrack> {
    match source {
        SourceRef::QobuzTrack {
            track_id,
            title,
            artist,
            album,
            album_id,
            image_url,
            duration_secs,
            radio,
            playlist_context,
            ..
        } => Some(QobuzQueueTrack {
            track_id: *track_id,
            title: title.clone(),
            artist: artist.clone(),
            album: album.clone(),
            album_id: album_id.clone(),
            image_url: image_url.clone(),
            duration_secs: *duration_secs,
            format_id: None,
            radio: *radio,
            playlist_context: playlist_context.clone(),
        }),
        SourceRef::LocalTrack { .. } => None,
    }
}

pub(crate) fn qobuz_play_request_from_source_ref(
    source: &SourceRef,
    queue_sources: &[SourceRef],
    radio_auto: bool,
) -> Option<QobuzPlayRequest> {
    let SourceRef::QobuzTrack {
        track_id,
        title,
        artist,
        album,
        album_id,
        image_url,
        duration_secs,
        radio,
        playlist_context,
        ..
    } = source
    else {
        return None;
    };
    Some(QobuzPlayRequest {
        track_id: *track_id,
        title: title.clone(),
        artist: artist.clone(),
        album: album.clone(),
        album_id: album_id.clone(),
        image_url: image_url.clone(),
        duration_secs: *duration_secs,
        format_id: None,
        expected_current: None,
        radio_auto: radio_auto || *radio,
        replace_current: true,
        playlist_context: playlist_context.clone(),
        queue: queue_sources
            .iter()
            .filter_map(qobuz_queue_track_from_source_ref)
            .collect(),
    })
}

pub(crate) fn qobuz_queue_source_refs(req: &QobuzPlayRequest) -> Vec<SourceRef> {
    req.queue
        .iter()
        .map(|track| qobuz_source_ref_from_queue_track(track, req.radio_auto))
        .collect()
}

pub(crate) fn qobuz_track_id_from_source(source: &SourceRef) -> Option<u64> {
    match source {
        SourceRef::QobuzTrack { track_id, .. } => Some(*track_id),
        SourceRef::LocalTrack { .. } => None,
    }
}

pub(crate) fn source_ref_with_radio(mut source: SourceRef, radio: bool) -> SourceRef {
    match &mut source {
        SourceRef::LocalTrack {
            radio: source_radio,
            ..
        }
        | SourceRef::QobuzTrack {
            radio: source_radio,
            ..
        } => {
            *source_radio = *source_radio || radio;
        }
    }
    source
}

pub(crate) fn source_ref_with_radio_context(
    mut source: SourceRef,
    radio_context: Option<RadioContext>,
) -> SourceRef {
    if radio_context.is_none() {
        return source;
    }
    match &mut source {
        SourceRef::LocalTrack {
            radio_context: source_context,
            ..
        }
        | SourceRef::QobuzTrack {
            radio_context: source_context,
            ..
        } => {
            *source_context = radio_context;
        }
    }
    source
}

pub(crate) fn source_ref_with_playlist_context(
    mut source: SourceRef,
    playlist_context: Option<PlaylistContext>,
) -> SourceRef {
    if playlist_context.is_none() {
        return source;
    }
    match &mut source {
        SourceRef::LocalTrack {
            playlist_context: source_context,
            ..
        }
        | SourceRef::QobuzTrack {
            playlist_context: source_context,
            ..
        } => {
            *source_context = playlist_context;
        }
    }
    source
}
