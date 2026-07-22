use super::LOCAL_ZONE_ID;
use super::registry::{LocalZoneEntry, ZoneState};
use crate::audio::player::OutputTransport;
use crate::audio::{airplay, sonos, upnp};

pub(super) struct ActiveZonePolicy;

impl ActiveZonePolicy {
    pub(super) fn first_enabled_zone_id(state: &ZoneState) -> String {
        state
            .local_zones
            .iter()
            .find_map(|(id, zone)| Self::local_zone_is_usable(zone).then(|| id.clone()))
            .or_else(|| {
                // Browser zones are private to their own browser session and
                // must never be promoted to the shared active zone.
                state
                    .agents
                    .iter()
                    .find_map(|(id, agent)| (agent.enabled && !agent.browser).then(|| id.clone()))
            })
            .unwrap_or_else(|| LOCAL_ZONE_ID.to_string())
    }

    pub(super) fn restore_preferred_or_fallback(state: &mut ZoneState) {
        let active_controllable = Self::active_zone_is_controllable(state);
        if let Some(preferred) = state.preferred_active_zone_id.clone() {
            let should_restore_preferred = !active_controllable
                || state.active_zone_id == LOCAL_ZONE_ID
                || state.active_zone_id == preferred;
            if should_restore_preferred && Self::zone_is_controllable(state, &preferred) {
                if state.active_zone_id != preferred {
                    Self::activate_zone_unchecked(state, &preferred);
                }
                return;
            }
        }
        if !active_controllable {
            state.active_zone_id = Self::first_enabled_zone_id(state);
        }
    }

    pub(super) fn activate_zone_unchecked(state: &mut ZoneState, zone_id: &str) {
        if let Some(zone) = state.local_zones.get_mut(zone_id)
            && let Some(device) = zone.device_name.clone()
            && !sonos::is_sonos_device_name(&device)
            && !upnp::is_upnp_device_name(&device)
        {
            zone.player.select_device(Some(device));
        }
        state.active_zone_id = zone_id.to_string();
    }

    pub(super) fn zone_is_controllable(state: &ZoneState, zone_id: &str) -> bool {
        state
            .local_zones
            .get(zone_id)
            .is_some_and(Self::local_zone_is_controllable)
            || state.agents.get(zone_id).is_some_and(|agent| agent.enabled)
    }

    pub(super) fn active_zone_is_controllable(state: &ZoneState) -> bool {
        Self::zone_is_controllable(state, &state.active_zone_id)
    }

    pub(super) fn local_zone_is_usable(zone: &LocalZoneEntry) -> bool {
        zone.enabled && zone.online && Self::local_zone_airplay_unsupported_reason(zone).is_none()
    }

    pub(super) fn local_zone_is_controllable(zone: &LocalZoneEntry) -> bool {
        Self::local_zone_is_usable(zone)
            || (zone.enabled && Self::local_zone_has_running_or_owned_output(zone))
    }

    pub(super) fn local_zone_has_running_or_owned_output(zone: &LocalZoneEntry) -> bool {
        !zone.player.playback_state().is_stopped()
            || zone.player.output_transport() == OutputTransport::DopCoreAudio
    }

    pub(super) fn local_zone_airplay_unsupported_reason(zone: &LocalZoneEntry) -> Option<String> {
        zone.device_name
            .as_deref()
            .and_then(airplay::parse_target_device_name)
            .and_then(|target| target.unsupported_reason())
    }
}
