use crate::app::state::AppState;
use crate::audio::player::Player;
use crate::library::ZoneSettings;
use crate::protocol::SinkProtocol;
use std::sync::Arc;

const AIRPLAY_FALLBACK_START_VOLUME: f32 = 0.4;

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
        .and_then(|settings| airplay_default_start_volume(&settings))
        .unwrap_or(AIRPLAY_FALLBACK_START_VOLUME);
    player.set_airplay_device_volume(start_volume);
}

pub(crate) fn airplay_default_start_volume(settings: &ZoneSettings) -> Option<f32> {
    let normalize = |volume: f32| volume.is_finite().then(|| volume.clamp(0.0, 1.0));
    settings
        .airplay_default_volume
        .and_then(normalize)
        .or_else(|| settings.airplay_last_volume.and_then(normalize))
        .or(Some(AIRPLAY_FALLBACK_START_VOLUME))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn airplay_startup_volume_prefers_explicit_default() {
        let settings = ZoneSettings {
            airplay_default_volume: None,
            airplay_last_volume: Some(0.53),
            ..ZoneSettings::default()
        };
        assert_eq!(airplay_default_start_volume(&settings), Some(0.53));

        let settings = ZoneSettings {
            airplay_default_volume: Some(0.2),
            airplay_last_volume: Some(0.53),
            ..ZoneSettings::default()
        };
        assert_eq!(airplay_default_start_volume(&settings), Some(0.2));
    }

    #[test]
    fn airplay_startup_volume_falls_back_to_safe_default() {
        assert_eq!(
            airplay_default_start_volume(&ZoneSettings::default()),
            Some(0.4)
        );

        let settings = ZoneSettings {
            airplay_default_volume: Some(f32::NAN),
            airplay_last_volume: Some(0.35),
            ..ZoneSettings::default()
        };
        assert_eq!(airplay_default_start_volume(&settings), Some(0.35));
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
