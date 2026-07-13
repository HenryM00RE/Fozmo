use super::LOCAL_ZONE_ID;
use super::active_zone_policy::ActiveZonePolicy;
use super::capabilities::{
    agent_backend_for_device, agent_capabilities_for_device, current_dsp_profile, local_backend,
    local_capabilities, local_protocol,
};
use super::registry::ZoneState;
use crate::audio::player::Player;
use crate::audio::{airplay, sonos, upnp};
use crate::protocol::{DspProfile, SinkProtocol, ZoneProfile, ZoneStatus};

pub(super) struct ZoneSnapshotBuilder;

impl ZoneSnapshotBuilder {
    pub(super) fn build(state: &ZoneState) -> Vec<ZoneProfile> {
        let mut zones = Vec::new();

        for (id, local) in &state.local_zones {
            let selected_device = local
                .device_name
                .clone()
                .or_else(|| local.player.selected_device_name());
            let airplay_target = local
                .device_name
                .as_deref()
                .and_then(airplay::parse_target_device_name);
            let sonos_target = local
                .device_name
                .as_deref()
                .and_then(sonos::parse_target_device_name);
            let upnp_target = local
                .device_name
                .as_deref()
                .and_then(upnp::parse_target_device_name);
            let airplay_unsupported_reason = airplay_target
                .as_ref()
                .and_then(|target| target.unsupported_reason());
            let status = if state.active_zone_id == *id
                && ActiveZonePolicy::local_zone_is_controllable(local)
            {
                ZoneStatus::Active
            } else if !local.online || airplay_unsupported_reason.is_some() {
                ZoneStatus::Offline
            } else if state.active_zone_id == *id {
                ZoneStatus::Active
            } else {
                ZoneStatus::Available
            };
            zones.push(ZoneProfile {
                id: id.clone(),
                name: local.name.clone(),
                protocol: local_protocol(local.device_name.as_deref()),
                agent_name: None,
                backend: Some(local_backend(local.device_name.as_deref()).to_string()),
                capabilities: local_capabilities(
                    selected_device.as_deref(),
                    selected_device
                        .as_deref()
                        .and_then(|device| state.local_device_capabilities.get(device))
                        .copied(),
                ),
                dsp_profile: current_dsp_profile(&local.player),
                status,
                enabled: local.enabled,
                device_name: local.device_name.clone(),
                network_address: sonos_target
                    .as_ref()
                    .map(|target| format!("{}:{}", target.host, target.port))
                    .or_else(|| {
                        upnp_target
                            .as_ref()
                            .map(|target| format!("{}:{}", target.host, target.port))
                    }),
                status_message: airplay_unsupported_reason
                    .or_else(|| {
                        sonos_target.as_ref().and_then(|target| {
                            (!target.coordinator).then(|| {
                                target
                                    .group_name
                                    .as_ref()
                                    .map(|name| format!("Sonos group member: {name}"))
                                    .unwrap_or_else(|| "Sonos group member".to_string())
                            })
                        })
                    })
                    .or_else(|| {
                        upnp_target
                            .as_ref()
                            .map(crate::audio::upnp::target_capability_status_message)
                    })
                    .or_else(|| local.status_message.clone()),
                playing_state: Some(player_state_label(&local.player)),
                track_title: current_track_title(&local.player),
                airplay_default_volume: None,
                airplay_last_volume: None,
                qobuz_hires_enabled: false,
                icon: None,
                device_type: None,
                hegel: None,
                upnp_calibrated_capabilities: None,
                browser: false,
                browser_stream: None,
            });
        }

        for (id, agent) in &state.agents {
            let track_title = agent
                .playback
                .as_ref()
                .and_then(|p| p.track_title.clone().or_else(|| p.file_name.clone()));
            zones.push(ZoneProfile {
                id: id.clone(),
                name: agent.name.clone(),
                protocol: SinkProtocol::RemoteAgent,
                agent_name: Some(agent.agent_name.clone()),
                backend: agent_backend_for_device(agent),
                capabilities: agent_capabilities_for_device(agent),
                dsp_profile: agent
                    .signal_path
                    .as_ref()
                    .map(|sig| DspProfile {
                        upsampling_enabled: sig.source_rate != sig.dsp_target_rate,
                        filter_type: sig.dsp_filter.clone(),
                        target_rate: sig.dsp_target_rate,
                        dither_mode: "Auto".to_string(),
                    })
                    .unwrap_or_default(),
                status: if agent.enabled && state.active_zone_id == *id {
                    ZoneStatus::Active
                } else {
                    ZoneStatus::Available
                },
                enabled: agent.enabled,
                device_name: agent.output_device.clone().or_else(|| {
                    agent
                        .signal_path
                        .as_ref()
                        .and_then(|sig| sig.output_device.clone())
                }),
                network_address: None,
                status_message: None,
                playing_state: agent.playback.as_ref().map(|p| p.state.clone()),
                track_title,
                airplay_default_volume: None,
                airplay_last_volume: None,
                qobuz_hires_enabled: false,
                icon: None,
                device_type: None,
                hegel: None,
                upnp_calibrated_capabilities: None,
                browser: agent.browser,
                browser_stream: None,
            });
        }
        zones.sort_by(|a, b| {
            let a_rank = if a.id == LOCAL_ZONE_ID { 0 } else { 1 };
            let b_rank = if b.id == LOCAL_ZONE_ID { 0 } else { 1 };
            a_rank.cmp(&b_rank).then_with(|| a.name.cmp(&b.name))
        });
        zones
    }
}

fn player_state_label(player: &Player) -> String {
    player.playback_state().as_name().to_string()
}

fn current_track_title(player: &Player) -> Option<String> {
    player
        .current_tags()
        .title
        .or_else(|| player.current_file_name())
}
