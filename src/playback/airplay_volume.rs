use crate::app::state::AppState;
use crate::audio::player::Player;
use crate::library::ZoneSettings;
use crate::protocol::SinkProtocol;
use std::sync::Arc;

pub(crate) fn prepare_airplay_volume_for_zone(
    state: &AppState,
    zone_id: &str,
    player: &Arc<Player>,
) {
    if !matches!(
        state.zones().zone_protocol(zone_id),
        Some(SinkProtocol::AirPlayRaop | SinkProtocol::AirPlay2)
    ) {
        return;
    }
    if player.airplay_device_volume().is_some() {
        return;
    }
    let start_volume = state
        .library()
        .zone_settings(zone_id)
        .ok()
        .and_then(|settings| airplay_default_start_volume(&settings));
    if let Some(volume) = start_volume {
        player.set_airplay_device_volume(volume);
    }
}

pub(crate) fn airplay_default_start_volume(settings: &ZoneSettings) -> Option<f32> {
    settings
        .airplay_default_volume
        .filter(|volume| volume.is_finite())
        .map(|volume| volume.clamp(0.0, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn airplay_startup_volume_uses_explicit_default_only() {
        let settings = ZoneSettings {
            airplay_default_volume: None,
            airplay_last_volume: Some(0.53),
            ..ZoneSettings::default()
        };
        assert_eq!(airplay_default_start_volume(&settings), None);

        let settings = ZoneSettings {
            airplay_default_volume: Some(0.2),
            airplay_last_volume: Some(0.53),
            ..ZoneSettings::default()
        };
        assert_eq!(airplay_default_start_volume(&settings), Some(0.2));
    }

    #[test]
    fn airplay_startup_volume_clamps_default() {
        let settings = ZoneSettings {
            airplay_default_volume: Some(2.0),
            airplay_last_volume: None,
            ..ZoneSettings::default()
        };
        assert_eq!(airplay_default_start_volume(&settings), Some(1.0));
    }
}
