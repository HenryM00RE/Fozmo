use crate::app::state::AppState;
use crate::audio::player::{QueueItem, TrackCover};
use crate::playback::error::ResolveError;
use crate::playback::source::{
    source_ref_with_playlist_context, source_ref_with_radio, source_ref_with_radio_context,
};
use crate::protocol::SourceRef;
use serde::Deserialize;
use std::path::{Component, Path as StdPath, PathBuf};

#[derive(Clone, Deserialize)]
#[serde(untagged)]
#[allow(clippy::large_enum_variant)]
pub(crate) enum QueueRequestItem {
    Source(SourceRef),
    Local {
        file_name: Option<String>,
        track_id: Option<i64>,
    },
}

pub(crate) fn source_ref_from_play_request(
    state: &AppState,
    track_id: Option<i64>,
    file_name: Option<&str>,
) -> Result<Option<SourceRef>, ResolveError> {
    let id = if let Some(id) = track_id {
        id
    } else if let Some(name) = file_name {
        match state.library().track_id_for_file_name(name) {
            Ok(Some(id)) => id,
            Ok(None) => return Err(ResolveError::TrackNotFound),
            Err(e) => {
                return Err(ResolveError::library(e));
            }
        }
    } else {
        return Ok(None);
    };
    state
        .library()
        .source_ref_for_track_id(id)
        .map_err(ResolveError::library)?
        .ok_or(ResolveError::TrackNotFound)
        .map(Some)
}

pub(crate) fn source_ref_from_queue_request(
    state: &AppState,
    item: &QueueRequestItem,
) -> Result<Option<SourceRef>, ResolveError> {
    match item {
        QueueRequestItem::Source(SourceRef::LocalTrack {
            track_id,
            file_name,
            playlist_context,
            radio,
            radio_context,
            ..
        }) => Ok(source_ref_from_play_request(
            state,
            (*track_id > 0).then_some(*track_id),
            file_name.as_deref(),
        )?
        .map(|source| {
            source_ref_with_playlist_context(
                source_ref_with_radio_context(
                    source_ref_with_radio(source, *radio),
                    radio_context.clone(),
                ),
                playlist_context.clone(),
            )
        })),
        QueueRequestItem::Source(source @ SourceRef::QobuzTrack { .. }) => Ok(Some(source.clone())),
        QueueRequestItem::Local {
            track_id,
            file_name,
        } => source_ref_from_play_request(state, *track_id, file_name.as_deref()),
    }
}

fn local_queue_item_from_source_ref(state: &AppState, source: &SourceRef) -> Option<QueueItem> {
    match source {
        SourceRef::LocalTrack {
            track_id,
            file_name,
            ..
        } => local_queue_entry_from_play_request(
            state,
            (*track_id > 0).then_some(*track_id),
            file_name.as_deref(),
        )
        .map(|(queue_item, _)| queue_item),
        SourceRef::QobuzTrack { .. } => None,
    }
}

pub(crate) fn local_player_queue_items_from_sources(
    state: &AppState,
    sources: &[SourceRef],
) -> Vec<QueueItem> {
    let mut queue_items = Vec::new();
    for source in sources {
        match source {
            SourceRef::LocalTrack { .. } => {
                if let Some(queue_item) = local_queue_item_from_source_ref(state, source) {
                    queue_items.push(queue_item);
                }
            }
            SourceRef::QobuzTrack { .. } => break,
        }
    }
    queue_items
}

fn local_queue_entry_from_play_request(
    state: &AppState,
    track_id: Option<i64>,
    file_name: Option<&str>,
) -> Option<(QueueItem, SourceRef)> {
    let path = resolve_track_path(state, track_id, file_name)
        .ok()
        .flatten()?;
    let source = source_ref_from_play_request(state, track_id, file_name)
        .ok()
        .flatten()?;
    let path_str = path.to_string_lossy().to_string();
    let fallback_cover = lookup_fallback_cover(state, &path_str);
    Some((
        QueueItem {
            file_path: path_str,
            fallback_cover,
            fallback_tags: None,
        },
        source,
    ))
}

pub(crate) fn resolve_track_path(
    state: &AppState,
    track_id: Option<i64>,
    file_name: Option<&str>,
) -> Result<Option<PathBuf>, ResolveError> {
    let path = if let Some(id) = track_id {
        match state.library().track_path(id) {
            Ok(Some(p)) => p,
            Ok(None) => return Err(ResolveError::TrackNotFound),
            Err(e) => {
                return Err(ResolveError::library(e));
            }
        }
    } else if let Some(name) = file_name {
        resolve_music_file_name(state.music_dir(), name)?
    } else {
        return Ok(None);
    };
    if !path.exists() {
        return Err(ResolveError::FileNotFound(format!(
            "File {:?} not found",
            path
        )));
    }
    Ok(Some(path))
}

pub(crate) fn resolve_music_file_name(
    music_dir: &StdPath,
    file_name: &str,
) -> Result<PathBuf, ResolveError> {
    let trimmed = file_name.trim();
    if trimmed.is_empty() {
        return Err(ResolveError::InvalidFileName);
    }
    let path = StdPath::new(trimmed);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(ResolveError::InvalidFileName);
    }
    Ok(music_dir.join(path))
}

/// Library-side cover so user-uploaded album art still shows when the audio file
/// has no embedded artwork and no cover.jpg sidecar.
pub(crate) fn lookup_fallback_cover(state: &AppState, path_str: &str) -> Option<TrackCover> {
    state
        .library()
        .cover_for_track_path(path_str)
        .ok()
        .flatten()
        .map(|(mime, data)| TrackCover { mime, data })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playback::test_support::{app_state, qobuz_source};

    #[test]
    fn resolve_music_file_name_rejects_traversal() {
        let music_dir = StdPath::new("/tmp/music");

        assert!(resolve_music_file_name(music_dir, "../secret.flac").is_err());
        assert!(resolve_music_file_name(music_dir, "/etc/passwd").is_err());
        assert!(resolve_music_file_name(music_dir, "Album/track.flac").is_ok());
    }

    #[test]
    fn queue_local_source_with_file_name_is_rehydrated_from_library() {
        let state = app_state("resolver-queue-local-file-name");
        write_music_file(&state, "Artist/Album/01 Alpha.wav");
        state.library().scan().unwrap();
        let item = QueueRequestItem::Source(SourceRef::LocalTrack {
            track_id: 0,
            file_name: Some("01 Alpha.wav".to_string()),
            title: Some("Stale title".to_string()),
            artist: None,
            album: None,
            album_artist: None,
            album_id: None,
            art_id: None,
            duration_secs: None,
            ext_hint: None,
            radio: false,
            radio_context: None,
            playlist_context: None,
        });

        let source = source_ref_from_queue_request(&state, &item)
            .unwrap()
            .unwrap();

        match source {
            SourceRef::LocalTrack {
                track_id,
                file_name,
                title,
                album,
                ext_hint,
                ..
            } => {
                assert!(track_id > 0);
                assert_eq!(file_name.as_deref(), Some("01 Alpha.wav"));
                assert_eq!(title.as_deref(), Some("Alpha"));
                assert_eq!(album.as_deref(), Some("Album"));
                assert_eq!(ext_hint.as_deref(), Some("wav"));
            }
            SourceRef::QobuzTrack { .. } => panic!("expected local source"),
        }
    }

    #[test]
    fn local_player_queue_items_stop_at_first_qobuz_source() {
        let state = app_state("resolver-local-queue-prefix");
        write_music_file(&state, "Artist/Album/01 Alpha.wav");
        write_music_file(&state, "Artist/Album/02 Beta.wav");
        state.library().scan().unwrap();
        let alpha_id = state
            .library()
            .track_id_for_file_name("01 Alpha.wav")
            .unwrap()
            .unwrap();
        let beta_id = state
            .library()
            .track_id_for_file_name("02 Beta.wav")
            .unwrap()
            .unwrap();
        let sources = vec![
            local_source_by_track_id(alpha_id, "01 Alpha.wav"),
            qobuz_source(42, false),
            local_source_by_track_id(beta_id, "02 Beta.wav"),
        ];

        let queue = local_player_queue_items_from_sources(&state, &sources);

        assert_eq!(queue.len(), 1);
        assert!(queue[0].file_path.ends_with("01 Alpha.wav"));
    }

    fn write_music_file(state: &AppState, relative_path: &str) {
        let path = state.music_dir().join(relative_path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"not a real wav").unwrap();
    }

    fn local_source_by_track_id(track_id: i64, file_name: &str) -> SourceRef {
        local_source(track_id, file_name)
    }

    fn local_source(track_id: i64, file_name: &str) -> SourceRef {
        SourceRef::LocalTrack {
            track_id,
            file_name: Some(file_name.to_string()),
            title: None,
            artist: None,
            album: None,
            album_artist: None,
            album_id: None,
            art_id: None,
            duration_secs: None,
            ext_hint: None,
            radio: false,
            radio_context: None,
            playlist_context: None,
        }
    }
}
