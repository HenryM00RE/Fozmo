use super::active_zone_policy::ActiveZonePolicy;
use super::registry::ZoneState;
use crate::library::ZoneDefinition;

pub(super) struct ZonePersistence;

impl ZonePersistence {
    pub(super) fn apply_definitions(state: &mut ZoneState, definitions: Vec<ZoneDefinition>) {
        for def in definitions {
            if let Some(zone) = state.local_zones.get_mut(&def.id) {
                zone.name = def.name;
                zone.enabled = def.enabled;
            } else if let Some(agent) = state.agents.get_mut(&def.id) {
                agent.name = def.name;
                agent.enabled = def.enabled;
            }
        }
        ActiveZonePolicy::restore_preferred_or_fallback(state);
    }
}
