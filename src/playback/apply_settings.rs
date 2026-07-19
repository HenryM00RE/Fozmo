use crate::app::state::AppState;
use crate::audio::airplay;
use crate::audio::dither::DitherPreference;
use crate::audio::dsd::delta_sigma::DsdModulator;
use crate::audio::eq::EqConfig;
use crate::audio::player::{
    DEFAULT_HEADROOM_DB, LivePlaybackConfig, MAX_DSP_BUFFER_MS, OutputMode, Player,
};
use crate::audio::resampler::{DEFAULT_FILTER_TYPE, FilterType};
use crate::playback::config::{
    dsp_is_unavailable_for_zone, effective_dsd_rules_from_zone_settings,
    effective_output_mode_for_upsampling, playback_config_for_zone,
};
use crate::playback::error::PlaybackError;
use crate::playback::output_devices::output_device_available;
use crate::playback::upnp::enqueue_upnp_config_reapply_for_zone;
use crate::protocol::{CoreToAgentCommand, SinkProtocol};
use crate::settings::ZonePlaybackSettings;

pub(crate) fn apply_active_zone_playback_settings(state: &AppState) {
    let zone_id = state.zones().active_zone_id();
    apply_playback_settings_for_zone(state, &zone_id);
    state.playback_config_applicator().remember_applied(zone_id);
}

pub(crate) fn apply_active_zone_playback_settings_if_changed(state: &AppState) {
    let zone_id = state.zones().active_zone_id();
    if !state.playback_config_applicator().mark_if_changed(&zone_id) {
        return;
    }
    apply_playback_settings_for_zone(state, &zone_id);
}

pub(crate) fn remember_active_zone_playback_settings_applied(state: &AppState) {
    let zone_id = state.zones().active_zone_id();
    state.playback_config_applicator().remember_applied(zone_id);
}

pub(crate) fn select_active_output_device(
    state: &AppState,
    device_name: Option<String>,
) -> Result<(), PlaybackError> {
    if let Some(name) = device_name.as_deref() {
        validate_output_device_selection(state, name)?;
    }
    let zone_id = state.zones().active_zone_id();
    state
        .zones()
        .active_player()
        .select_device(device_name.clone());
    state
        .settings()
        .update_playback_for_zone(&zone_id, |s| s.device_name = device_name)
        .map_err(PlaybackError::integration)?;
    remember_active_zone_playback_settings_applied(state);
    Ok(())
}

fn validate_output_device_selection(
    state: &AppState,
    device_name: &str,
) -> Result<(), PlaybackError> {
    let trimmed = device_name.trim();
    if trimmed.is_empty() {
        return Err(PlaybackError::bad_request(
            "Output device name cannot be empty",
        ));
    }
    if airplay::is_airplay_device_name(trimmed) {
        if state.airplay().is_trusted_device_name(trimmed) {
            return Ok(());
        }
        return Err(PlaybackError::bad_request(
            "AirPlay receiver is not currently available",
        ));
    }
    if state.zones().known_local_output_device_name(trimmed) || output_device_available(trimmed) {
        return Ok(());
    }
    Err(PlaybackError::bad_request(
        "Output device is not currently available",
    ))
}

pub(crate) fn apply_active_eq_config(state: &AppState, eq: EqConfig) -> Result<(), PlaybackError> {
    let zone_id = state.zones().active_zone_id();
    apply_eq_config_for_zone(state, &zone_id, eq)
}

pub(crate) fn apply_eq_config_for_zone(
    state: &AppState,
    zone_id: &str,
    eq: EqConfig,
) -> Result<(), PlaybackError> {
    state
        .settings()
        .update_playback_for_zone(zone_id, |s| s.eq = Some(eq.clone()))
        .map_err(PlaybackError::integration)?;
    state.playback_config_applicator().remember_applied(zone_id);

    if state.zones().zone_protocol(zone_id) == Some(SinkProtocol::RemoteAgent) {
        let fallback_player = state.zones().active_player();
        let _ = state.zones().send_to_zone(
            zone_id,
            CoreToAgentCommand::SetPlaybackConfig {
                playback_config: playback_config_for_zone(state, zone_id, &fallback_player),
            },
        );
        return Ok(());
    }

    let Some(player) = state.zones().player_for_zone(zone_id) else {
        return Ok(());
    };
    player.update_eq(eq);
    Ok(())
}

pub(crate) fn apply_playback_settings_for_zone(state: &AppState, zone_id: &str) {
    let mut settings = state.settings().playback_for_zone(zone_id);
    let protocol = state.zones().zone_protocol(zone_id);
    if protocol == Some(SinkProtocol::RemoteAgent) {
        let fallback_player = state.zones().active_player();
        let _ = state.zones().send_to_zone(
            zone_id,
            CoreToAgentCommand::SetPlaybackConfig {
                playback_config: playback_config_for_zone(state, zone_id, &fallback_player),
            },
        );
        return;
    }
    let Some(player) = state.zones().player_for_zone(zone_id) else {
        return;
    };
    if protocol == Some(SinkProtocol::SonosUpnp) {
        if let Some(v) = settings.volume {
            player.set_volume(v.clamp(0.0, 1.0));
        }
        return;
    }
    if protocol == Some(SinkProtocol::UpnpAvRenderer) {
        enqueue_upnp_config_reapply_for_zone(state.clone(), zone_id);
        return;
    }
    if dsp_is_unavailable_for_zone(state, zone_id) {
        settings.upsampling_enabled = Some(false);
        settings.exclusive = Some(false);
        settings.output_mode = Some(OutputMode::Pcm.as_name().to_string());
        settings.dsd_isi_penalty = Some(0.0);
        settings.dsd_rules_enabled = false;
        settings.dsd_rules.clear();
        settings.headroom_db = Some(0.0);
        settings.dsp_buffer_ms = Some(0);
    }
    let bound_device = state.zones().zone_bound_device_name(zone_id);
    let can_apply_saved_device = bound_device.is_none();
    apply_playback_settings_to_player(state, &player, &settings, can_apply_saved_device);
    if let Some(device_name) = bound_device {
        player.select_device(Some(device_name));
    }
}

fn apply_playback_settings_to_player(
    state: &AppState,
    player: &Player,
    settings: &ZonePlaybackSettings,
    apply_saved_device: bool,
) {
    let filter = settings
        .filter_type
        .as_deref()
        .and_then(FilterType::from_name)
        .unwrap_or(DEFAULT_FILTER_TYPE);
    let target_rate = settings.target_rate.unwrap_or(0);
    let upsampling_enabled = settings.upsampling_enabled.unwrap_or(false);
    let exclusive = settings.exclusive.unwrap_or(true);
    let dsp_buffer_ms = settings.dsp_buffer_ms.unwrap_or(0).min(MAX_DSP_BUFFER_MS);
    let output_mode = settings
        .output_mode
        .as_deref()
        .and_then(OutputMode::from_name)
        .unwrap_or(OutputMode::Pcm);
    let output_mode = effective_output_mode_for_upsampling(upsampling_enabled, output_mode);
    let dsd_modulator = settings
        .dsd_modulator
        .as_deref()
        .and_then(DsdModulator::from_name)
        .unwrap_or_default();
    player.apply_playback_config(LivePlaybackConfig {
        filter_type: filter,
        target_rate,
        upsampling_enabled,
        exclusive,
        dsp_buffer_ms,
        output_mode,
        dsd_modulator,
        dsd_isi_penalty: settings.dsd_isi_penalty.unwrap_or(0.0),
        dsd_rules: effective_dsd_rules_from_zone_settings(settings),
        eq: settings.eq.clone(),
    });
    player.set_dither_mode(DitherPreference::Auto.as_id());
    player.set_headroom_db(if upsampling_enabled {
        settings.headroom_db.unwrap_or(DEFAULT_HEADROOM_DB)
    } else {
        0.0
    });

    if let Some(v) = settings.volume {
        player.set_volume(v);
    }
    if apply_saved_device {
        match settings.device_name.as_deref() {
            Some(name) if validate_output_device_selection(state, name).is_ok() => {
                player.select_device(Some(name.to_string()));
            }
            Some(name) => {
                eprintln!(
                    "settings: saved audio device '{}' is not currently available; using zone default",
                    name
                );
            }
            None => player.select_device(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::airplay::{AirPlayReceiver, AirPlayServiceKind, AirPlayTarget};
    use crate::audio::player::OutputMode;
    use crate::audio::upnp::{UpnpRenderer, UpnpRendererTarget, receiver_zone_id};
    use crate::playback::test_support::{agent_capabilities, app_state};
    use crate::protocol::{
        CapabilityDetectionSource, CapabilityDetectionStatus, CoreToAgentCommand,
    };
    use tokio::sync::mpsc;

    fn airplay_target(id: &str, _host: &str, _port: u16) -> AirPlayTarget {
        AirPlayTarget {
            id: id.to_string(),
            name: "Living Room".to_string(),
            service_kind: AirPlayServiceKind::Raop,
            supported: true,
            unsupported_reason: None,
        }
    }

    fn upnp_target(id: &str) -> UpnpRendererTarget {
        UpnpRendererTarget {
            id: id.to_string(),
            name: "UPnP Test".to_string(),
            host: "127.0.0.1".to_string(),
            port: 1400,
            model: None,
            manufacturer: None,
            av_transport_control_url: "/AVTransport".to_string(),
            rendering_control_url: None,
            connection_manager_url: None,
            max_sample_rate: 192_000,
            max_bit_depth: 24,
            max_dsd_rate: Some(128),
            capability_detection_source: CapabilityDetectionSource::Probed,
            capability_detection_status: CapabilityDetectionStatus::Complete,
            capability_detection_message: None,
            protocol_info: Vec::new(),
            pcm_containers: Vec::new(),
        }
    }

    fn wait_for_selected_device(player: &Player, expected: &str) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while std::time::Instant::now() < deadline {
            if player.selected_device_name().as_deref() == Some(expected) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(player.selected_device_name().as_deref(), Some(expected));
    }

    #[test]
    fn selecting_active_output_device_persists_active_zone_device_setting() {
        let state = app_state("select-output-device");
        let zone_id = state.zones().active_zone_id();
        state
            .zones()
            .sync_local_devices(vec!["External DAC".to_string()]);

        select_active_output_device(&state, Some("External DAC".to_string())).unwrap();

        assert_eq!(
            state
                .settings()
                .playback_for_zone(&zone_id)
                .device_name
                .as_deref(),
            Some("External DAC")
        );
        assert_eq!(
            state
                .playback_config_applicator()
                .applied_zone_id()
                .as_deref(),
            Some(zone_id.as_str())
        );
    }

    #[test]
    fn selecting_active_output_device_rejects_forged_airplay_target() {
        let state = app_state("reject-forged-airplay-output-device");
        let zone_id = state.zones().active_zone_id();
        let target = airplay_target("aa:bb:cc:dd:ee:52", "127.0.0.1", 5000);
        let forged_name = airplay::target_device_name(&target);

        let result = select_active_output_device(&state, Some(forged_name.clone()));

        assert!(matches!(result, Err(PlaybackError::BadRequest(_))));
        assert_ne!(
            state
                .zones()
                .active_player()
                .selected_device_name()
                .as_deref(),
            Some(forged_name.as_str())
        );
        assert_eq!(
            state
                .settings()
                .playback_for_zone(&zone_id)
                .device_name
                .as_deref(),
            None
        );
    }

    #[test]
    fn selecting_active_output_device_accepts_discovered_airplay_target() {
        let state = app_state("accept-discovered-airplay-output-device");
        let zone_id = state.zones().active_zone_id();
        let target = airplay_target("aa:bb:cc:dd:ee:53", "127.0.0.1", 5000);
        let device_name = airplay::target_device_name(&target);
        state
            .airplay()
            .set_receivers_for_test(vec![AirPlayReceiver {
                target,
                online: true,
            }]);

        select_active_output_device(&state, Some(device_name.clone())).unwrap();

        assert_eq!(
            state
                .settings()
                .playback_for_zone(&zone_id)
                .device_name
                .as_deref(),
            Some(device_name.as_str())
        );
    }

    #[test]
    fn applying_saved_settings_restores_discovered_airplay_target() {
        let state = app_state("restore-saved-discovered-airplay-output-device");
        let player = state.zones().active_player();
        let target = airplay_target("aa:bb:cc:dd:ee:54", "127.0.0.1", 5000);
        let device_name = airplay::target_device_name(&target);
        state
            .airplay()
            .set_receivers_for_test(vec![AirPlayReceiver {
                target,
                online: true,
            }]);
        let _ = state.settings().update_playback_for_zone(
            &state.zones().active_zone_id(),
            |settings| {
                settings.device_name = Some(device_name.clone());
            },
        );

        apply_active_zone_playback_settings(&state);

        wait_for_selected_device(&player, &device_name);
    }

    #[test]
    fn applying_saved_settings_rejects_forged_airplay_target() {
        let state = app_state("reject-saved-forged-airplay-output-device");
        let player = state.zones().active_player();
        let target = airplay_target("aa:bb:cc:dd:ee:55", "127.0.0.1", 5000);
        let forged_name = airplay::target_device_name(&target);
        let _ = state.settings().update_playback_for_zone(
            &state.zones().active_zone_id(),
            |settings| {
                settings.device_name = Some(forged_name.clone());
            },
        );

        apply_active_zone_playback_settings(&state);

        assert_ne!(
            player.selected_device_name().as_deref(),
            Some(forged_name.as_str())
        );
    }

    #[test]
    fn applying_airplay_settings_bypasses_saved_dsp_config() {
        let state = app_state("airplay-bypasses-saved-dsp");
        let target = airplay_target("aa:bb:cc:dd:ee:56", "127.0.0.1", 5000);
        let zone_id = airplay::receiver_zone_id(&target.id);
        state.zones().sync_airplay_receivers(vec![AirPlayReceiver {
            target,
            online: true,
        }]);
        state
            .zones()
            .enable_zone(&zone_id)
            .expect("enable AirPlay zone");
        let _ = state
            .settings()
            .update_playback_for_zone(&zone_id, |settings| {
                settings.upsampling_enabled = Some(true);
                settings.exclusive = Some(true);
                settings.output_mode = Some("Dsd128".to_string());
                settings.dsd_isi_penalty = Some(0.012);
                settings.dsd_rules_enabled = true;
                settings.dsd_rules = vec![crate::settings::DsdSourceRule {
                    source_rate: 44_100,
                    filter_type: "Split128k".to_string(),
                    output_mode: "Dsd128".to_string(),
                }];
                settings.headroom_db = Some(-6.0);
                settings.dsp_buffer_ms = Some(250);
            });

        apply_playback_settings_for_zone(&state, &zone_id);

        let player = state
            .zones()
            .player_for_zone(&zone_id)
            .expect("AirPlay zone player");
        let config = player.snapshot().config;
        assert!(!config.upsampling_enabled);
        assert!(!config.exclusive);
        assert_eq!(config.output_mode, OutputMode::Pcm);
        assert_eq!(config.dsd_isi_penalty, 0.0);
        assert_eq!(config.headroom_db, 0.0);
        assert_eq!(config.dsp_buffer_ms, 0);
    }

    #[test]
    fn applying_remote_agent_settings_does_not_reconfigure_local_player() {
        let state = app_state("remote-agent-settings-local-player");
        let local_player = state.zones().active_player();
        local_player.set_output_mode(OutputMode::Pcm);

        let (tx, mut rx) = mpsc::unbounded_channel();
        state.zones().register_agent(
            "agent-1".to_string(),
            "Studio PC".to_string(),
            agent_capabilities("Agent DAC"),
            tx,
        );
        let zone_id = state
            .zones()
            .list_zones()
            .into_iter()
            .find(|zone| zone.agent_name.as_deref() == Some("Studio PC"))
            .expect("remote agent zone should be registered")
            .id;
        state.zones().select_zone(&zone_id).unwrap();
        let _ = state
            .settings()
            .update_playback_for_zone(&zone_id, |settings| {
                settings.upsampling_enabled = Some(true);
                settings.output_mode = Some("Dsd256".to_string());
                settings.target_rate = Some(192_000);
            });

        apply_active_zone_playback_settings(&state);

        assert_eq!(local_player.output_mode(), OutputMode::Pcm);
        match rx.try_recv() {
            Ok(CoreToAgentCommand::SetPlaybackConfig { playback_config }) => {
                let expected_output_mode = if cfg!(feature = "experimental_dsd256") {
                    "Dsd256"
                } else {
                    "Dsd128"
                };
                assert_eq!(playback_config.output_mode, expected_output_mode);
                assert_eq!(playback_config.target_rate, 192_000);
            }
            other => panic!("expected remote playback config, got {other:?}"),
        }
    }

    #[test]
    fn applying_unavailable_local_zone_settings_does_not_reconfigure_active_player() {
        let state = app_state("offline-local-settings-local-player");
        let local_player = state.zones().active_player();
        local_player.set_output_mode(OutputMode::Pcm);
        let device_name = "KEF LSX";
        let zone_id = crate::zones::local_device_zone_id(device_name);
        state.zones().sync_saved_local_zone(
            &zone_id,
            "KEF LSX",
            device_name,
            true,
            "Output missing",
        );
        let _ = state
            .settings()
            .update_playback_for_zone(&zone_id, |settings| {
                settings.output_mode = Some("Dsd128".to_string());
                settings.target_rate = Some(192_000);
            });

        apply_playback_settings_for_zone(&state, &zone_id);

        assert_eq!(local_player.output_mode(), OutputMode::Pcm);
    }

    #[tokio::test]
    async fn applying_upnp_settings_enqueues_renderer_reapply_without_reconfiguring_local_player() {
        let state = app_state("upnp-settings-local-player");
        let local_player = state.zones().active_player();
        local_player.set_output_mode(OutputMode::Pcm);
        let target = upnp_target("renderer-apply");
        let zone_id = receiver_zone_id(&target.id);
        state.zones().sync_upnp_renderers(vec![UpnpRenderer {
            target,
            online: true,
        }]);
        state
            .zones()
            .enable_zone(&zone_id)
            .expect("enable UPnP zone");
        let _ = state
            .settings()
            .update_playback_for_zone(&zone_id, |settings| {
                settings.output_mode = Some("Dsd128".to_string());
                settings.target_rate = Some(192_000);
            });

        apply_playback_settings_for_zone(&state, &zone_id);

        assert_eq!(local_player.output_mode(), OutputMode::Pcm);
        let snapshot = state.upnp().snapshot(&zone_id).expect("UPnP snapshot");
        assert!(snapshot.restart_pending);
        assert_eq!(snapshot.render_status, "pending");
    }

    #[tokio::test]
    async fn active_upnp_settings_apply_if_changed_enqueues_renderer_reapply() {
        let state = app_state("active-upnp-settings-apply-if-changed");
        let local_player = state.zones().active_player();
        local_player.set_output_mode(OutputMode::Pcm);
        let target = upnp_target("renderer-active-apply");
        let zone_id = receiver_zone_id(&target.id);
        state.zones().sync_upnp_renderers(vec![UpnpRenderer {
            target,
            online: true,
        }]);
        state.zones().enable_zone(&zone_id).unwrap();
        let _ = state
            .settings()
            .update_playback_for_zone(&zone_id, |settings| {
                settings.output_mode = Some("Dsd128".to_string());
                settings.target_rate = Some(192_000);
            });

        apply_active_zone_playback_settings_if_changed(&state);

        assert_eq!(local_player.output_mode(), OutputMode::Pcm);
        assert_eq!(
            state
                .playback_config_applicator()
                .applied_zone_id()
                .as_deref(),
            Some(zone_id.as_str())
        );
        let snapshot = state.upnp().snapshot(&zone_id).expect("UPnP snapshot");
        assert!(snapshot.restart_pending);
        assert_eq!(snapshot.render_status, "pending");
    }

    #[test]
    fn removed_filter_names_fall_back_to_default_filter() {
        let state = app_state("removed-filter-names");
        let player = Player::new();
        let settings = ZonePlaybackSettings {
            filter_type: Some("Apodizing".to_string()),
            ..ZonePlaybackSettings::default()
        };

        apply_playback_settings_to_player(&state, &player, &settings, false);

        assert_eq!(
            player.snapshot().config.filter_type,
            Some(DEFAULT_FILTER_TYPE)
        );
    }
}
