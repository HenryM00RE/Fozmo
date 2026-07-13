use crate::app::state::AppState;
use crate::protocol::{SinkProtocol, SourceRef};
use std::path::Path as StdPath;

pub(crate) fn sonos_current_file_name(state: &AppState, zone_id: &str) -> Option<String> {
    state
        .sonos()
        .snapshot(zone_id)
        .and_then(|snapshot| snapshot.file_name)
}

pub(crate) fn sonos_current_matches(
    state: &AppState,
    zone_id: &str,
    expected: &Option<String>,
) -> bool {
    current_playback_matches_expected(state, zone_id, expected)
}

pub(crate) fn current_playback_matches_expected(
    state: &AppState,
    zone_id: &str,
    expected: &Option<String>,
) -> bool {
    let Some(expected) = expected.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
        return true;
    };
    if let Some(source) = state.listening().active_source(zone_id)
        && source_matches_expected(&source, expected)
    {
        return true;
    }
    if state.zones().zone_protocol(zone_id) == Some(SinkProtocol::UpnpAvRenderer)
        && state
            .upnp()
            .snapshot(zone_id)
            .and_then(|snapshot| snapshot.current_source)
            .is_some_and(|source| source_matches_expected(&source, expected))
    {
        return true;
    }
    current_file_name_for_zone(state, zone_id)
        .as_deref()
        .is_some_and(|current| playback_name_matches(expected, current))
}

fn current_file_name_for_zone(state: &AppState, zone_id: &str) -> Option<String> {
    match state.zones().zone_protocol(zone_id) {
        Some(SinkProtocol::SonosUpnp) => sonos_current_file_name(state, zone_id)
            .or_else(|| player_file_name(state.zones().player_for_zone(zone_id))),
        Some(SinkProtocol::RemoteAgent) => state
            .zones()
            .remote_snapshot_for_zone(zone_id)
            .and_then(|snapshot| snapshot.playback)
            .and_then(|playback| playback.file_name),
        Some(SinkProtocol::UpnpAvRenderer) => state
            .upnp()
            .snapshot(zone_id)
            .and_then(|snapshot| snapshot.file_name)
            .or_else(|| player_file_name(state.zones().player_for_zone(zone_id))),
        Some(_) => player_file_name(state.zones().player_for_zone(zone_id)),
        None => None,
    }
}

fn player_file_name(
    player: Option<std::sync::Arc<crate::audio::player::Player>>,
) -> Option<String> {
    player.and_then(|player| player.current_file_name())
}

fn playback_name_matches(expected: &str, current: &str) -> bool {
    if expected == current {
        return true;
    }
    let expected_name = StdPath::new(expected)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(expected);
    let current_name = StdPath::new(current)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(current);
    expected_name == current_name
}

fn source_matches_expected(source: &SourceRef, expected: &str) -> bool {
    if source.key() == expected {
        return true;
    }
    if let SourceRef::LocalTrack {
        file_name: Some(file_name),
        ..
    } = source
    {
        return playback_name_matches(expected, file_name);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::upnp::{
        UpnpAsset, UpnpRenderer, UpnpRendererTarget, receiver_zone_id, target_device_name,
    };
    use crate::playback::test_support::{app_state, qobuz_source};

    #[test]
    fn upnp_current_matches_source_key_and_display_title() {
        let state = app_state("upnp-current-match");
        let zone_id = seed_upnp_qobuz_playback(&state, 10, "Artist - Track 10");

        assert!(current_playback_matches_expected(
            &state,
            &zone_id,
            &Some("qobuz:10".to_string())
        ));
        assert!(current_playback_matches_expected(
            &state,
            &zone_id,
            &Some("Artist - Track 10".to_string())
        ));
    }

    #[test]
    fn upnp_current_rejects_unrelated_expected_current() {
        let state = app_state("upnp-current-mismatch");
        let zone_id = seed_upnp_qobuz_playback(&state, 10, "Artist - Track 10");

        assert!(!current_playback_matches_expected(
            &state,
            &zone_id,
            &Some("qobuz:99".to_string())
        ));
    }

    fn seed_upnp_qobuz_playback(state: &AppState, track_id: u64, title: &str) -> String {
        let target = upnp_target("renderer-1");
        let zone_id = receiver_zone_id(&target.id);
        state.zones().sync_upnp_renderers(vec![UpnpRenderer {
            target: target.clone(),
            online: true,
        }]);
        let source = qobuz_source(track_id, false);
        state.upnp().seed_playback_for_test(
            &zone_id,
            UpnpAsset {
                id: format!("qobuz-{track_id}-27"),
                source_ref: source.clone(),
                stream_url: format!("http://core.test/upnp/qobuz/{track_id}"),
                mime_type: "audio/flac".to_string(),
                byte_len: Some(1024),
                art_url: None,
                title: Some(title.to_string()),
                artist: Some("Artist".to_string()),
                album: Some("Album".to_string()),
                duration_secs: Some(180.0),
                source_rate: 44_100,
                target_rate: 44_100,
                source_bits: 16,
                target_bits: 24,
                active_output_mode: None,
                qobuz_resolve_ms: None,
                asset_registration_ms: None,
                render_signature: Some(format!("qobuz-{track_id}-sig")),
                configured_render_signature: Some(format!("qobuz-{track_id}-sig")),
                render_ms: None,
                prepare_ms: None,
                cache_hit: None,
                render_or_stream_plan: None,
                cache_lookup_ms: None,
                cache_wait_ms: None,
            },
            "Playing",
        );
        state.listening().start(
            state.library(),
            zone_id.clone(),
            "UPnP Test".to_string(),
            state.settings().active_profile_id(),
            source,
            Vec::new(),
        );
        zone_id
    }

    fn upnp_target(id: &str) -> UpnpRendererTarget {
        let target = UpnpRendererTarget {
            id: id.to_string(),
            name: "UPnP Test".to_string(),
            host: "127.0.0.1".to_string(),
            port: 1400,
            model: Some("Test Renderer".to_string()),
            manufacturer: Some("Test".to_string()),
            av_transport_control_url: "/MediaRenderer/AVTransport/Control".to_string(),
            rendering_control_url: Some("/MediaRenderer/RenderingControl/Control".to_string()),
            connection_manager_url: None,
            max_sample_rate: 192_000,
            max_bit_depth: 24,
            max_dsd_rate: None,
            capability_detection_source: crate::protocol::CapabilityDetectionSource::Advertised,
            capability_detection_status: crate::protocol::CapabilityDetectionStatus::Complete,
            capability_detection_message: None,
            protocol_info: Vec::new(),
            pcm_containers: Vec::new(),
        };
        assert!(target_device_name(&target).starts_with(crate::audio::upnp::UPNP_DEVICE_PREFIX));
        target
    }
}
