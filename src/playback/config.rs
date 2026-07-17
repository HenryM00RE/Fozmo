use crate::app::state::AppState;
use crate::audio::dither::DitherPreference;
use crate::audio::dsd::delta_sigma::DsdModulator;
use crate::audio::dsd::dsd_render::ecbeam2_filter_supported;
use crate::audio::player::{DEFAULT_HEADROOM_DB, MAX_DSP_BUFFER_MS, OutputMode, Player};
use crate::audio::resampler::FilterType;
use crate::audio::upnp::UpnpRendererTarget;
use crate::playback::apply_settings::apply_playback_settings_for_zone;
use crate::playback::error::PlaybackError;
use crate::playback::upnp::{enqueue_upnp_config_reapply_for_zone, upnp_target_for_zone};
use crate::protocol::{PlaybackConfig, SinkProtocol, UpnpPcmContainer};
use crate::settings::{DsdSourceRule, ZonePlaybackSettings};
use crate::zones::current_playback_config;

const DSD_RULE_SOURCE_RATES: [u32; 6] = [44_100, 48_000, 88_200, 96_000, 176_400, 192_000];
const DEFAULT_TARGET_BIT_DEPTH: u32 = 24;
pub(crate) struct PlaybackConfigUpdate {
    pub filter_type: String,
    pub target_rate: u32,
    pub target_bit_depth: u32,
    pub upsampling_enabled: bool,
    pub exclusive: bool,
    pub output_mode: Option<String>,
    pub dsd_modulator: Option<String>,
    pub dsd_isi_penalty: f32,
    pub dsd_rules_enabled: bool,
    pub dsd_rules: Vec<DsdSourceRule>,
    pub headroom_db: f32,
    pub dsp_buffer_ms: u32,
}

pub(crate) fn dsp_is_unavailable_for_zone(state: &AppState, zone_id: &str) -> bool {
    state
        .zones()
        .zone_protocol(zone_id)
        .is_some_and(|protocol| !protocol.supports_dsp())
        || state.zones().browser_zone_agent_id(zone_id).is_some()
}

pub(crate) fn validate_dsd_rules(rules: &[DsdSourceRule]) -> Result<(), PlaybackError> {
    for rule in rules {
        if !DSD_RULE_SOURCE_RATES.contains(&rule.source_rate) {
            return Err(PlaybackError::bad_request(format!(
                "Unsupported DSD source sample rate {}",
                rule.source_rate
            )));
        }
        FilterType::from_name(&rule.filter_type)
            .ok_or_else(|| PlaybackError::bad_request("Invalid DSD filter type"))?;
        match OutputMode::from_name(&rule.output_mode) {
            Some(OutputMode::Dsd256) if !cfg!(feature = "experimental_dsd256") => {
                return Err(PlaybackError::bad_request(
                    "DSD256 requires the experimental_dsd256 feature",
                ));
            }
            Some(mode) if mode.is_dsd() => {}
            _ => {
                return Err(PlaybackError::bad_request("Invalid DSD output mode"));
            }
        }
    }
    Ok(())
}

pub(crate) fn effective_output_mode_for_upsampling(
    upsampling_enabled: bool,
    output_mode: OutputMode,
) -> OutputMode {
    let output_mode = supported_output_mode(output_mode);
    if upsampling_enabled {
        output_mode
    } else {
        OutputMode::Pcm
    }
}

fn supported_output_mode(output_mode: OutputMode) -> OutputMode {
    if output_mode == OutputMode::Dsd256 && !cfg!(feature = "experimental_dsd256") {
        OutputMode::Dsd128
    } else {
        output_mode
    }
}

pub(crate) fn effective_dsd_rules(
    upsampling_enabled: bool,
    output_mode: OutputMode,
    dsd_rules_enabled: bool,
    rules: &[DsdSourceRule],
) -> Vec<DsdSourceRule> {
    if upsampling_enabled && output_mode.is_dsd() && dsd_rules_enabled {
        rules
            .iter()
            .filter(|rule| {
                OutputMode::from_name(&rule.output_mode)
                    .is_some_and(|mode| supported_output_mode(mode) == mode)
            })
            .cloned()
            .collect()
    } else {
        Vec::new()
    }
}

pub(crate) fn effective_dsd_rules_from_zone_settings(
    settings: &ZonePlaybackSettings,
) -> Vec<DsdSourceRule> {
    let upsampling_enabled = settings.upsampling_enabled.unwrap_or(false);
    let output_mode = settings
        .output_mode
        .as_deref()
        .and_then(OutputMode::from_name)
        .unwrap_or(OutputMode::Pcm);
    effective_dsd_rules(
        upsampling_enabled,
        effective_output_mode_for_upsampling(upsampling_enabled, output_mode),
        settings.dsd_rules_enabled,
        &settings.dsd_rules,
    )
}

// Validation intentionally mirrors the complete persisted DSD selection surface.
#[allow(clippy::too_many_arguments)]
fn validate_ecbeam2_playback_config(
    modulator: DsdModulator,
    filter_type: FilterType,
    upsampling_enabled: bool,
    output_mode: OutputMode,
    dsd_rules_enabled: bool,
    dsd_rules: &[DsdSourceRule],
    isi_penalty: f32,
    headroom_db: f32,
) -> Result<(), PlaybackError> {
    if !upsampling_enabled {
        return Ok(());
    }
    if modulator == DsdModulator::Standard {
        if (headroom_db - DEFAULT_HEADROOM_DB).abs() > 1.0e-6 {
            return Err(PlaybackError::bad_request(
                "Standard requires -4 dB headroom",
            ));
        }
        return Ok(());
    }
    if modulator != DsdModulator::EcBeam2 {
        return Ok(());
    }
    if isi_penalty != 0.0 {
        return Err(PlaybackError::bad_request(
            "EcBeam2 requires a zero DSD ISI penalty",
        ));
    }
    if (headroom_db + 2.0).abs() > 1.0e-6 {
        return Err(PlaybackError::bad_request(
            "EcBeam2 requires -2 dB headroom",
        ));
    }
    if !ecbeam2_filter_supported(filter_type) {
        return Err(PlaybackError::bad_request(
            "7th Order Search supports only the four selectable 128k filters",
        ));
    }

    // Validate EcBeam2 against the requested mode before generic feature
    // fallback can normalize an unavailable DSD256 selection to DSD128.
    let effective_output_mode = if upsampling_enabled {
        output_mode
    } else {
        OutputMode::Pcm
    };
    if effective_output_mode.is_dsd()
        && !matches!(
            effective_output_mode,
            OutputMode::Dsd64 | OutputMode::Dsd128 | OutputMode::Dsd256
        )
    {
        return Err(PlaybackError::bad_request(
            "EcBeam2 supports only DSD64, DSD128, and DSD256 output",
        ));
    }
    if effective_output_mode.is_dsd()
        && dsd_rules_enabled
        && dsd_rules.iter().any(|rule| {
            !matches!(
                OutputMode::from_name(&rule.output_mode),
                Some(OutputMode::Dsd64 | OutputMode::Dsd128 | OutputMode::Dsd256)
            )
        })
    {
        return Err(PlaybackError::bad_request(
            "EcBeam2 requires every enabled DSD rule to use DSD64, DSD128, or DSD256",
        ));
    }
    if effective_output_mode.is_dsd()
        && dsd_rules_enabled
        && dsd_rules.iter().any(|rule| {
            !FilterType::from_name(&rule.filter_type).is_some_and(ecbeam2_filter_supported)
        })
    {
        return Err(PlaybackError::bad_request(
            "7th Order Search requires every enabled DSD rule to use one of the four selectable 128k filters",
        ));
    }
    Ok(())
}

fn normalize_target_bit_depth(bits: u32) -> u32 {
    match bits {
        16 | 24 | 32 => bits,
        _ => DEFAULT_TARGET_BIT_DEPTH,
    }
}

pub(crate) fn playback_config_for_zone(
    state: &AppState,
    zone_id: &str,
    player: &Player,
) -> PlaybackConfig {
    let settings = state.settings().playback_for_zone(zone_id);
    let is_upnp = state.zones().zone_protocol(zone_id) == Some(SinkProtocol::UpnpAvRenderer);
    let upnp_target = if is_upnp {
        upnp_target_for_zone(state, zone_id).ok()
    } else {
        None
    };
    let fallback = current_playback_config(player, Vec::new());
    let dsp_unavailable = dsp_is_unavailable_for_zone(state, zone_id);
    let upsampling_enabled = !dsp_unavailable
        && settings
            .upsampling_enabled
            .unwrap_or(fallback.upsampling_enabled);
    let requested_output_mode = settings
        .output_mode
        .as_deref()
        .and_then(OutputMode::from_name)
        .or_else(|| OutputMode::from_name(&fallback.output_mode))
        .unwrap_or(OutputMode::Pcm);
    let output_mode =
        effective_output_mode_for_upsampling(upsampling_enabled, requested_output_mode);
    let output_mode = if is_upnp && !upnp_output_mode_allowed(output_mode, upnp_target.as_ref()) {
        OutputMode::Pcm
    } else {
        output_mode
    };
    let mut dsd_rules = effective_dsd_rules(
        upsampling_enabled,
        output_mode,
        settings.dsd_rules_enabled,
        &settings.dsd_rules,
    );
    if is_upnp {
        dsd_rules.retain(|rule| {
            OutputMode::from_name(&rule.output_mode)
                .is_some_and(|mode| upnp_output_mode_allowed(mode, upnp_target.as_ref()))
        });
    }
    let configured_dsd_modulator = settings
        .dsd_modulator
        .as_deref()
        .unwrap_or(&fallback.dsd_modulator);
    let dsd_modulator = DsdModulator::from_name(configured_dsd_modulator).unwrap_or_default();
    let configured_headroom_db = settings
        .headroom_db
        .unwrap_or(fallback.headroom_db.min(DEFAULT_HEADROOM_DB));
    PlaybackConfig {
        filter_type: settings
            .filter_type
            .clone()
            .unwrap_or_else(|| fallback.filter_type.clone()),
        target_rate: settings.target_rate.unwrap_or(fallback.target_rate),
        target_bit_depth: normalize_target_bit_depth(
            settings
                .target_bit_depth
                .unwrap_or(fallback.target_bit_depth),
        ),
        upsampling_enabled,
        exclusive: !dsp_unavailable && settings.exclusive.unwrap_or(fallback.exclusive),
        dither_mode: DitherPreference::Auto.as_name().to_string(),
        output_mode: output_mode.as_name().to_string(),
        dsd_modulator: dsd_modulator.as_name().to_string(),
        dsd_isi_penalty: if dsp_unavailable {
            0.0
        } else {
            settings.dsd_isi_penalty.unwrap_or(fallback.dsd_isi_penalty)
        },
        dsd_rules,
        headroom_db: if dsp_unavailable {
            0.0
        } else {
            match dsd_modulator {
                DsdModulator::Standard => DEFAULT_HEADROOM_DB,
                DsdModulator::EcBeam2 => -2.0,
                _ => configured_headroom_db,
            }
        },
        dsp_buffer_ms: if dsp_unavailable {
            0
        } else {
            settings.dsp_buffer_ms.unwrap_or(fallback.dsp_buffer_ms)
        },
        volume: settings.volume.unwrap_or(fallback.volume),
        eq: settings.eq.clone().unwrap_or(fallback.eq),
        output_device: settings.device_name.clone(),
    }
}

pub(crate) fn update_active_playback_config(
    state: &AppState,
    update: PlaybackConfigUpdate,
) -> Result<(), PlaybackError> {
    let zone_id = state.zones().active_zone_id();
    update_playback_config_for_zone(state, &zone_id, update)
}

pub(crate) fn update_playback_config_for_zone(
    state: &AppState,
    zone_id: &str,
    mut update: PlaybackConfigUpdate,
) -> Result<(), PlaybackError> {
    let protocol = state.zones().zone_protocol(zone_id);
    if protocol.is_none() {
        return Err(PlaybackError::not_found(format!(
            "Zone '{zone_id}' is not available"
        )));
    }
    let dsp_unavailable = dsp_is_unavailable_for_zone(state, zone_id);
    if dsp_unavailable {
        update.upsampling_enabled = false;
        update.exclusive = false;
        update.output_mode = Some(OutputMode::Pcm.as_name().to_string());
        update.dsd_isi_penalty = 0.0;
        update.dsd_rules_enabled = false;
        update.dsd_rules.clear();
        update.headroom_db = 0.0;
        update.dsp_buffer_ms = 0;
    }
    let filter_type = FilterType::from_name(&update.filter_type)
        .ok_or_else(|| PlaybackError::bad_request("Invalid filter type"))?;
    let dither = DitherPreference::Auto;

    if ![
        0, 44100, 48000, 88200, 96000, 176400, 192000, 352800, 384000,
    ]
    .contains(&update.target_rate)
    {
        return Err(PlaybackError::bad_request("Unsupported target sample rate"));
    }
    if ![16, 24, 32].contains(&update.target_bit_depth) {
        return Err(PlaybackError::bad_request("Unsupported target bit depth"));
    }

    let requested_output_mode = match update.output_mode.as_deref() {
        Some(name) => OutputMode::from_name(name)
            .ok_or_else(|| PlaybackError::bad_request("Invalid output mode"))?,
        None => OutputMode::Pcm,
    };
    if requested_output_mode == OutputMode::Dsd256 && !cfg!(feature = "experimental_dsd256") {
        return Err(PlaybackError::bad_request(
            "DSD256 requires the experimental_dsd256 feature",
        ));
    }
    validate_upnp_dsd_capability(
        state,
        zone_id,
        requested_output_mode,
        update.dsd_rules_enabled,
        &update.dsd_rules,
    )?;
    let legacy_ecbeam = update
        .dsd_modulator
        .as_deref()
        .is_some_and(DsdModulator::is_legacy_ecbeam_name);
    let dsd_modulator = match update.dsd_modulator.as_deref() {
        Some(name) => DsdModulator::from_name(name)
            .ok_or_else(|| PlaybackError::bad_request("Invalid DSD modulator"))?,
        None => DsdModulator::default(),
    };
    validate_dsd_rules(&update.dsd_rules)?;
    if !update.headroom_db.is_finite() {
        return Err(PlaybackError::bad_request("Invalid headroom attenuation"));
    }
    let headroom_db =
        if !dsp_unavailable && (dsd_modulator == DsdModulator::Standard || legacy_ecbeam) {
            DEFAULT_HEADROOM_DB
        } else {
            update.headroom_db.clamp(-24.0, 0.0)
        };
    if !update.dsd_isi_penalty.is_finite() {
        return Err(PlaybackError::bad_request("Invalid DSD ISI penalty"));
    }
    validate_ecbeam2_playback_config(
        dsd_modulator,
        filter_type,
        update.upsampling_enabled,
        requested_output_mode,
        update.dsd_rules_enabled,
        &update.dsd_rules,
        update.dsd_isi_penalty,
        headroom_db,
    )?;
    let dsd_isi_penalty = update.dsd_isi_penalty.clamp(0.0, 0.05);
    if update.dsp_buffer_ms > MAX_DSP_BUFFER_MS {
        return Err(PlaybackError::bad_request("Unsupported DSP buffer size"));
    }

    let dsd_rules_enabled = update.dsd_rules_enabled;
    state
        .settings()
        .update_playback_for_zone(zone_id, |s| {
            s.filter_type = Some(update.filter_type.clone());
            s.target_rate = Some(update.target_rate);
            s.target_bit_depth = Some(update.target_bit_depth);
            s.upsampling_enabled = Some(update.upsampling_enabled);
            s.exclusive = Some(update.exclusive);
            s.dither_mode = Some(dither.as_name().to_string());
            s.output_mode = Some(requested_output_mode.as_name().to_string());
            s.dsd_modulator = Some(dsd_modulator.as_name().to_string());
            s.dsd_isi_penalty = Some(dsd_isi_penalty);
            s.dsd_rules_enabled = dsd_rules_enabled;
            s.dsd_rules = update.dsd_rules.clone();
            s.headroom_db = Some(headroom_db);
            s.dsp_buffer_ms = Some(update.dsp_buffer_ms);
        })
        .map_err(PlaybackError::integration)?;
    state.playback_config_applicator().remember_applied(zone_id);
    if state.zones().zone_protocol(zone_id) == Some(SinkProtocol::UpnpAvRenderer) {
        enqueue_upnp_config_reapply_for_zone(state.clone(), zone_id);
        return Ok(());
    }
    apply_playback_settings_for_zone(state, zone_id);
    Ok(())
}

fn validate_upnp_dsd_capability(
    state: &AppState,
    zone_id: &str,
    output_mode: OutputMode,
    dsd_rules_enabled: bool,
    dsd_rules: &[DsdSourceRule],
) -> Result<(), PlaybackError> {
    if state.zones().zone_protocol(zone_id) != Some(SinkProtocol::UpnpAvRenderer) {
        return Ok(());
    }
    let target = upnp_target_for_zone(state, zone_id)?;
    ensure_upnp_output_mode_allowed(output_mode, &target)?;
    if dsd_rules_enabled {
        for rule in dsd_rules {
            let Some(rule_mode) = OutputMode::from_name(&rule.output_mode) else {
                continue;
            };
            ensure_upnp_output_mode_allowed(rule_mode, &target)?;
        }
    }
    Ok(())
}

fn ensure_upnp_output_mode_allowed(
    output_mode: OutputMode,
    target: &UpnpRendererTarget,
) -> Result<(), PlaybackError> {
    if upnp_output_mode_allowed(output_mode, Some(target)) {
        return Ok(());
    }
    let requested = output_mode.as_name().to_uppercase();
    Err(PlaybackError::bad_request(format!(
        "{requested} is not available for this UPnP renderer"
    )))
}

fn output_mode_allowed_by_dsd_cap(output_mode: OutputMode, max_dsd_rate: Option<u16>) -> bool {
    match output_mode {
        OutputMode::Pcm => true,
        OutputMode::Dsd64 => max_dsd_rate.is_some_and(|rate| rate >= 64),
        OutputMode::Dsd128 => max_dsd_rate.is_some_and(|rate| rate >= 128),
        OutputMode::Dsd256 => max_dsd_rate.is_some_and(|rate| rate >= 256),
    }
}

fn upnp_output_mode_allowed(output_mode: OutputMode, target: Option<&UpnpRendererTarget>) -> bool {
    if output_mode == OutputMode::Pcm {
        return true;
    }
    let Some(target) = target else {
        return false;
    };
    output_mode_allowed_by_dsd_cap(output_mode, target.max_dsd_rate)
        || upnp_dop_output_mode_allowed(output_mode, target)
}

fn upnp_dop_output_mode_allowed(output_mode: OutputMode, target: &UpnpRendererTarget) -> bool {
    let required_rate = match output_mode {
        OutputMode::Pcm => return true,
        OutputMode::Dsd64 => 192_000,
        OutputMode::Dsd128 => 384_000,
        OutputMode::Dsd256 => 768_000,
    };
    target.pcm_containers.iter().any(|capability| {
        capability.container == UpnpPcmContainer::Wav
            && capability.max_sample_rate >= required_rate
            && capability.max_bit_depth >= 24
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::upnp::{UpnpRenderer, UpnpRendererTarget, receiver_zone_id};
    use crate::playback::test_support::app_state;
    use crate::protocol::{CapabilityDetectionSource, CapabilityDetectionStatus};
    use crate::settings::ZonePlaybackSettings;

    #[test]
    fn disabled_upsampling_forces_pcm_output_mode() {
        assert_eq!(
            effective_output_mode_for_upsampling(false, OutputMode::Dsd256),
            OutputMode::Pcm
        );
        assert_eq!(
            effective_output_mode_for_upsampling(true, OutputMode::Dsd256),
            if cfg!(feature = "experimental_dsd256") {
                OutputMode::Dsd256
            } else {
                OutputMode::Dsd128
            }
        );
    }

    #[test]
    fn disabled_upsampling_ignores_saved_dsd_rules() {
        let settings = ZonePlaybackSettings {
            upsampling_enabled: Some(false),
            output_mode: Some("Dsd256".to_string()),
            dsd_rules_enabled: true,
            dsd_rules: vec![DsdSourceRule {
                source_rate: 44_100,
                filter_type: "Minimum16k".to_string(),
                output_mode: "Dsd256".to_string(),
            }],
            ..ZonePlaybackSettings::default()
        };

        assert!(effective_dsd_rules_from_zone_settings(&settings).is_empty());
    }

    #[test]
    fn enabled_dsd_rules_keep_requested_output_modes() {
        let settings = ZonePlaybackSettings {
            upsampling_enabled: Some(true),
            output_mode: Some("Dsd256".to_string()),
            dsd_rules_enabled: true,
            dsd_rules: vec![
                DsdSourceRule {
                    source_rate: 44_100,
                    filter_type: "Minimum16k".to_string(),
                    output_mode: "Dsd256".to_string(),
                },
                DsdSourceRule {
                    source_rate: 192_000,
                    filter_type: "Minimum16k".to_string(),
                    output_mode: "Dsd128".to_string(),
                },
            ],
            ..ZonePlaybackSettings::default()
        };

        let rules = effective_dsd_rules_from_zone_settings(&settings);

        if cfg!(feature = "experimental_dsd256") {
            assert_eq!(rules.len(), 2);
            assert_eq!(rules[0].output_mode, "Dsd256");
            assert_eq!(rules[1].output_mode, "Dsd128");
        } else {
            assert_eq!(rules.len(), 1);
            assert_eq!(rules[0].output_mode, "Dsd128");
        }
    }

    #[test]
    fn dsd_rules_can_request_higher_mode_than_the_default_dsd_mode() {
        let settings = ZonePlaybackSettings {
            upsampling_enabled: Some(true),
            output_mode: Some("Dsd128".to_string()),
            dsd_rules_enabled: true,
            dsd_rules: vec![DsdSourceRule {
                source_rate: 44_100,
                filter_type: "Minimum16k".to_string(),
                output_mode: "Dsd256".to_string(),
            }],
            ..ZonePlaybackSettings::default()
        };

        let rules = effective_dsd_rules_from_zone_settings(&settings);

        if cfg!(feature = "experimental_dsd256") {
            assert_eq!(rules.len(), 1);
            assert_eq!(rules[0].output_mode, "Dsd256");
        } else {
            assert!(rules.is_empty());
        }
    }

    #[test]
    fn playback_config_defaults_to_measured_headroom() {
        let state = crate::playback::test_support::app_state("default-headroom");
        let zone_id = state.zones().active_zone_id();

        let config = playback_config_for_zone(&state, &zone_id, &state.zones().active_player());

        assert_eq!(config.headroom_db, DEFAULT_HEADROOM_DB);
    }

    #[test]
    fn playback_config_persists_linear128k_for_standard() {
        let state = crate::playback::test_support::app_state("linear128k-standard");
        let update = PlaybackConfigUpdate {
            filter_type: "LinearPhase128k".to_string(),
            target_rate: 0,
            target_bit_depth: 24,
            upsampling_enabled: true,
            exclusive: true,
            output_mode: Some("Dsd128".to_string()),
            dsd_modulator: Some("Standard".to_string()),
            dsd_isi_penalty: 0.0,
            dsd_rules_enabled: false,
            dsd_rules: Vec::new(),
            headroom_db: -4.0,
            dsp_buffer_ms: 0,
        };

        update_active_playback_config(&state, update).expect("Linear128k config should update");

        let zone_id = state.zones().active_zone_id();
        let settings = state.settings().playback_for_zone(&zone_id);
        assert_eq!(settings.filter_type.as_deref(), Some("LinearPhase128k"));
        assert_eq!(settings.dsd_modulator.as_deref(), Some("Standard"));
        assert_eq!(settings.headroom_db, Some(-4.0));
    }

    #[test]
    fn playback_config_normalizes_retired_ecbeam_to_standard_with_safe_headroom() {
        let state = crate::playback::test_support::app_state("retired-ecbeam");
        let update = PlaybackConfigUpdate {
            filter_type: "Split128k".to_string(),
            target_rate: 0,
            target_bit_depth: 24,
            upsampling_enabled: true,
            exclusive: true,
            output_mode: Some("Pcm".to_string()),
            dsd_modulator: Some("7th Order ECB".to_string()),
            dsd_isi_penalty: 0.0,
            dsd_rules_enabled: false,
            dsd_rules: Vec::new(),
            headroom_db: -2.0,
            dsp_buffer_ms: 0,
        };

        update_active_playback_config(&state, update).expect("legacy ECB config should update");

        let zone_id = state.zones().active_zone_id();
        let settings = state.settings().playback_for_zone(&zone_id);
        assert_eq!(settings.dsd_modulator.as_deref(), Some("Standard"));
        assert_eq!(settings.headroom_db, Some(DEFAULT_HEADROOM_DB));
        let config = playback_config_for_zone(&state, &zone_id, &state.zones().active_player());
        assert_eq!(config.dsd_modulator, "Standard");
        assert_eq!(config.headroom_db, DEFAULT_HEADROOM_DB);
    }

    #[test]
    fn playback_config_reads_saved_ecbeam_as_standard_with_safe_headroom() {
        let state = crate::playback::test_support::app_state("saved-retired-ecbeam");
        let zone_id = state.zones().active_zone_id();
        state
            .settings()
            .update_playback_for_zone(&zone_id, |settings| {
                settings.dsd_modulator = Some("EcBeam".to_string());
                settings.headroom_db = Some(-2.0);
            })
            .expect("legacy settings should be writable");

        let config = playback_config_for_zone(&state, &zone_id, &state.zones().active_player());

        assert_eq!(config.dsd_modulator, "Standard");
        assert_eq!(config.headroom_db, DEFAULT_HEADROOM_DB);
    }

    #[test]
    fn playback_config_persists_selectable_ecbeam2_at_dsd64() {
        let state = crate::playback::test_support::app_state("selectable-ecbeam2");
        let update = PlaybackConfigUpdate {
            filter_type: "Split128k".to_string(),
            target_rate: 0,
            target_bit_depth: 24,
            upsampling_enabled: true,
            exclusive: true,
            output_mode: Some("Dsd64".to_string()),
            dsd_modulator: Some("7th Order ECB2".to_string()),
            dsd_isi_penalty: 0.0,
            dsd_rules_enabled: false,
            dsd_rules: Vec::new(),
            headroom_db: -2.0,
            dsp_buffer_ms: 0,
        };

        update_active_playback_config(&state, update).expect("ECB2 config should update");

        let zone_id = state.zones().active_zone_id();
        let settings = state.settings().playback_for_zone(&zone_id);
        assert_eq!(settings.dsd_modulator.as_deref(), Some("EcBeam2"));
        assert_eq!(settings.dsd_isi_penalty, Some(0.0));
        assert_eq!(settings.headroom_db, Some(-2.0));
    }

    #[test]
    fn playback_config_persists_production_ecbeam2_at_dsd128() {
        let state = crate::playback::test_support::app_state("selectable-ecbeam2-dsd128");
        let update = PlaybackConfigUpdate {
            filter_type: "MinimumPhaseCompact128kV2".to_string(),
            target_rate: 0,
            target_bit_depth: 24,
            upsampling_enabled: true,
            exclusive: true,
            output_mode: Some("Dsd128".to_string()),
            dsd_modulator: Some("EcBeam2".to_string()),
            dsd_isi_penalty: 0.0,
            dsd_rules_enabled: false,
            dsd_rules: Vec::new(),
            headroom_db: -2.0,
            dsp_buffer_ms: 0,
        };

        update_active_playback_config(&state, update)
            .expect("production EcBeam2 DSD128 config should update");

        let zone_id = state.zones().active_zone_id();
        let settings = state.settings().playback_for_zone(&zone_id);
        assert_eq!(
            settings.filter_type.as_deref(),
            Some("MinimumPhaseCompact128kV2")
        );
        assert_eq!(settings.output_mode.as_deref(), Some("Dsd128"));
        assert_eq!(settings.dsd_modulator.as_deref(), Some("EcBeam2"));
        assert_eq!(settings.dsd_isi_penalty, Some(0.0));
        assert_eq!(settings.headroom_db, Some(-2.0));
    }

    #[test]
    fn playback_config_persists_smooth_phase_ecbeam2_at_dsd128() {
        let state = crate::playback::test_support::app_state("ecbeam2-smooth-phase-dsd128");
        let update = PlaybackConfigUpdate {
            filter_type: "SmoothPhase128k".to_string(),
            target_rate: 0,
            target_bit_depth: 24,
            upsampling_enabled: true,
            exclusive: true,
            output_mode: Some("Dsd128".to_string()),
            dsd_modulator: Some("EcBeam2".to_string()),
            dsd_isi_penalty: 0.0,
            dsd_rules_enabled: false,
            dsd_rules: Vec::new(),
            headroom_db: -2.0,
            dsp_buffer_ms: 0,
        };

        update_active_playback_config(&state, update)
            .expect("Smooth Phase EcBeam2 DSD128 config should update");

        let zone_id = state.zones().active_zone_id();
        let settings = state.settings().playback_for_zone(&zone_id);
        assert_eq!(settings.filter_type.as_deref(), Some("SmoothPhase128k"));
        assert_eq!(settings.output_mode.as_deref(), Some("Dsd128"));
        assert_eq!(settings.dsd_modulator.as_deref(), Some("EcBeam2"));
        assert_eq!(settings.dsd_isi_penalty, Some(0.0));
        assert_eq!(settings.headroom_db, Some(-2.0));
    }

    #[test]
    fn playback_config_rejects_ecbeam2_with_sinc_extreme_filter() {
        let state = crate::playback::test_support::app_state("ecbeam2-sinc-rejected");
        let update = PlaybackConfigUpdate {
            filter_type: "SincExtreme32k".to_string(),
            target_rate: 0,
            target_bit_depth: 24,
            upsampling_enabled: true,
            exclusive: true,
            output_mode: Some("Dsd64".to_string()),
            dsd_modulator: Some("EcBeam2".to_string()),
            dsd_isi_penalty: 0.0,
            dsd_rules_enabled: false,
            dsd_rules: Vec::new(),
            headroom_db: -2.0,
            dsp_buffer_ms: 0,
        };

        assert_eq!(
            update_active_playback_config(&state, update)
                .expect_err("SincExtreme32k must not be persisted for EcBeam2")
                .message(),
            "7th Order Search supports only the four selectable 128k filters"
        );
    }

    #[test]
    fn ecbeam2_validation_accepts_dsd64_dsd128_and_dsd256() {
        assert!(
            validate_ecbeam2_playback_config(
                DsdModulator::EcBeam2,
                FilterType::Split128k,
                true,
                OutputMode::Dsd64,
                false,
                &[],
                0.0,
                -2.0,
            )
            .is_ok()
        );
        assert!(
            validate_ecbeam2_playback_config(
                DsdModulator::EcBeam2,
                FilterType::LinearPhase128k,
                true,
                OutputMode::Dsd128,
                false,
                &[],
                0.0,
                -2.0,
            )
            .is_ok()
        );
        assert!(
            validate_ecbeam2_playback_config(
                DsdModulator::EcBeam2,
                FilterType::SmoothPhase128k,
                true,
                OutputMode::Dsd128,
                false,
                &[],
                0.0,
                -2.0,
            )
            .is_ok()
        );
        assert!(
            validate_ecbeam2_playback_config(
                DsdModulator::EcBeam2,
                FilterType::Minimum16k,
                true,
                OutputMode::Dsd64,
                false,
                &[],
                0.0,
                -2.0,
            )
            .is_ok()
        );
        assert_eq!(
            validate_ecbeam2_playback_config(
                DsdModulator::EcBeam2,
                FilterType::SincExtreme32k,
                true,
                OutputMode::Dsd64,
                false,
                &[],
                0.0,
                -2.0,
            )
            .expect_err("SincExtreme32k must be rejected")
            .message(),
            "7th Order Search supports only the four selectable 128k filters"
        );
        assert!(
            validate_ecbeam2_playback_config(
                DsdModulator::EcBeam2,
                FilterType::Split128k,
                true,
                OutputMode::Dsd128,
                false,
                &[],
                0.0,
                -2.0,
            )
            .is_ok()
        );
        assert!(
            validate_ecbeam2_playback_config(
                DsdModulator::EcBeam2,
                FilterType::Split128k,
                true,
                OutputMode::Dsd256,
                false,
                &[],
                0.0,
                -2.0,
            )
            .is_ok()
        );
        assert_eq!(
            validate_ecbeam2_playback_config(
                DsdModulator::EcBeam2,
                FilterType::Split128k,
                true,
                OutputMode::Dsd64,
                false,
                &[],
                0.001,
                -2.0,
            )
            .expect_err("nonzero ISI must be rejected")
            .message(),
            "EcBeam2 requires a zero DSD ISI penalty"
        );
        assert_eq!(
            validate_ecbeam2_playback_config(
                DsdModulator::EcBeam2,
                FilterType::Split128k,
                true,
                OutputMode::Dsd64,
                false,
                &[],
                0.0,
                -3.0,
            )
            .expect_err("wrong headroom must be rejected")
            .message(),
            "EcBeam2 requires -2 dB headroom"
        );
    }

    #[test]
    fn standard_validation_locks_headroom_to_minus_four_when_enabled() {
        assert!(
            validate_ecbeam2_playback_config(
                DsdModulator::Standard,
                FilterType::SincExtreme32k,
                true,
                OutputMode::Dsd256,
                false,
                &[],
                0.01,
                -4.0,
            )
            .is_ok()
        );
        assert_eq!(
            validate_ecbeam2_playback_config(
                DsdModulator::Standard,
                FilterType::SincExtreme32k,
                true,
                OutputMode::Dsd128,
                false,
                &[],
                0.0,
                -2.0,
            )
            .expect_err("Standard must reject unlocked headroom")
            .message(),
            "Standard requires -4 dB headroom"
        );
        assert!(
            validate_ecbeam2_playback_config(
                DsdModulator::Standard,
                FilterType::SincExtreme32k,
                false,
                OutputMode::Pcm,
                false,
                &[],
                0.0,
                0.0,
            )
            .is_ok(),
            "inactive DSP settings remain editable"
        );
    }

    #[test]
    fn ecbeam2_validation_checks_only_enabled_effective_rules() {
        let dsd128_rule = DsdSourceRule {
            source_rate: 44_100,
            filter_type: "Split128k".to_string(),
            output_mode: "Dsd128".to_string(),
        };
        let dsd256_rule = DsdSourceRule {
            source_rate: 96_000,
            filter_type: "Minimum16k".to_string(),
            output_mode: "Dsd256".to_string(),
        };
        let smooth_dsd128_rule = DsdSourceRule {
            source_rate: 48_000,
            filter_type: "SmoothPhase128k".to_string(),
            output_mode: "Dsd128".to_string(),
        };
        let sinc_dsd64_rule = DsdSourceRule {
            source_rate: 48_000,
            filter_type: "SincExtreme32k".to_string(),
            output_mode: "Dsd64".to_string(),
        };

        assert!(
            validate_ecbeam2_playback_config(
                DsdModulator::EcBeam2,
                FilterType::Split128k,
                true,
                OutputMode::Dsd64,
                false,
                std::slice::from_ref(&dsd128_rule),
                0.0,
                -2.0,
            )
            .is_ok()
        );
        assert!(
            validate_ecbeam2_playback_config(
                DsdModulator::EcBeam2,
                FilterType::Split128k,
                true,
                OutputMode::Dsd64,
                true,
                std::slice::from_ref(&dsd128_rule),
                0.0,
                -2.0,
            )
            .is_ok(),
            "mixed DSD64/DSD128 rules are qualified"
        );
        assert!(
            validate_ecbeam2_playback_config(
                DsdModulator::EcBeam2,
                FilterType::SmoothPhase128k,
                true,
                OutputMode::Dsd128,
                true,
                &[dsd128_rule.clone(), smooth_dsd128_rule],
                0.0,
                -2.0,
            )
            .is_ok(),
            "Smooth Phase rules are qualified"
        );
        assert!(
            validate_ecbeam2_playback_config(
                DsdModulator::EcBeam2,
                FilterType::Split128k,
                true,
                OutputMode::Dsd64,
                true,
                &[dsd256_rule],
                0.0,
                -2.0,
            )
            .is_ok(),
            "DSD256 rules are qualified"
        );
        assert_eq!(
            validate_ecbeam2_playback_config(
                DsdModulator::EcBeam2,
                FilterType::Split128k,
                true,
                OutputMode::Dsd64,
                true,
                &[sinc_dsd64_rule],
                0.0,
                -2.0,
            )
            .expect_err("enabled SincExtreme32k rule must be rejected")
            .message(),
            "7th Order Search requires every enabled DSD rule to use one of the four selectable 128k filters"
        );
        assert!(
            validate_ecbeam2_playback_config(
                DsdModulator::EcBeam2,
                FilterType::Split128k,
                false,
                OutputMode::Dsd128,
                true,
                &[dsd128_rule],
                0.0,
                -2.0,
            )
            .is_ok(),
            "inactive saved DSD settings must remain editable while upsampling is disabled"
        );
    }

    #[test]
    fn disabling_upsampling_preserves_requested_dsp_settings() {
        let state = crate::playback::test_support::app_state("disable-preserves-dsp-settings");
        let update = PlaybackConfigUpdate {
            filter_type: "Minimum16k".to_string(),
            target_rate: 384_000,
            target_bit_depth: 24,
            upsampling_enabled: false,
            exclusive: true,
            output_mode: Some("Dsd128".to_string()),
            dsd_modulator: Some("EC depth 2".to_string()),
            dsd_isi_penalty: 0.012,
            dsd_rules_enabled: true,
            dsd_rules: vec![DsdSourceRule {
                source_rate: 44_100,
                filter_type: "Split16k".to_string(),
                output_mode: "Dsd128".to_string(),
            }],
            headroom_db: -6.0,
            dsp_buffer_ms: 250,
        };

        update_active_playback_config(&state, update).expect("config should update");

        let zone_id = state.zones().active_zone_id();
        let settings = state.settings().playback_for_zone(&zone_id);
        assert_eq!(settings.upsampling_enabled, Some(false));
        assert_eq!(settings.output_mode.as_deref(), Some("Dsd128"));
        assert_eq!(settings.dsd_modulator.as_deref(), Some("Standard"));
        assert_eq!(settings.dsd_isi_penalty, Some(0.012));
        assert_eq!(settings.dither_mode.as_deref(), Some("Auto"));
        assert!(settings.dsd_rules_enabled);
        assert_eq!(settings.dsd_rules.len(), 1);
        assert_eq!(settings.headroom_db, Some(DEFAULT_HEADROOM_DB));
        assert_eq!(state.zones().active_player().output_mode(), OutputMode::Pcm);
    }

    #[tokio::test]
    async fn upnp_config_update_enqueues_reapply_without_reconfiguring_local_player() {
        let state = app_state("upnp-config-update-route");
        let local_player = state.zones().active_player();
        local_player.set_output_mode(OutputMode::Pcm);
        let target = upnp_target("renderer-config");
        let zone_id = receiver_zone_id(&target.id);
        state.zones().sync_upnp_renderers(vec![UpnpRenderer {
            target,
            online: true,
        }]);

        update_playback_config_for_zone(
            &state,
            &zone_id,
            PlaybackConfigUpdate {
                filter_type: "Minimum16k".to_string(),
                target_rate: 192_000,
                target_bit_depth: 24,
                upsampling_enabled: true,
                exclusive: false,
                output_mode: Some("Dsd128".to_string()),
                dsd_modulator: Some("EcDepth2".to_string()),
                dsd_isi_penalty: 0.0,
                dsd_rules_enabled: false,
                dsd_rules: Vec::new(),
                headroom_db: -3.0,
                dsp_buffer_ms: 0,
            },
        )
        .expect("UPnP config update");

        assert_eq!(local_player.output_mode(), OutputMode::Pcm);
        let snapshot = state.upnp().snapshot(&zone_id).expect("UPnP snapshot");
        assert!(snapshot.restart_pending);
        assert_eq!(snapshot.render_status, "pending");
        assert_eq!(
            state.settings().playback_for_zone(&zone_id).target_rate,
            Some(192_000)
        );
    }

    #[test]
    fn upnp_dsd64_rejects_generated_dsd_output_config() {
        let state = app_state("upnp-dsd64-rejects-dsd");
        let target = upnp_target_with_dsd("renderer-dsd64", Some(64));
        let zone_id = receiver_zone_id(&target.id);
        state.zones().sync_upnp_renderers(vec![UpnpRenderer {
            target,
            online: true,
        }]);

        let result = update_playback_config_for_zone(
            &state,
            &zone_id,
            PlaybackConfigUpdate {
                filter_type: "Minimum16k".to_string(),
                target_rate: 192_000,
                target_bit_depth: 24,
                upsampling_enabled: true,
                exclusive: false,
                output_mode: Some("Dsd128".to_string()),
                dsd_modulator: Some("EcDepth2".to_string()),
                dsd_isi_penalty: 0.0,
                dsd_rules_enabled: false,
                dsd_rules: Vec::new(),
                headroom_db: -3.0,
                dsp_buffer_ms: 0,
            },
        );

        let error = result.expect_err("UPnP DSD64 renderer should reject generated DSD128");
        assert!(format!("{error:?}").contains("DSD128 is not available"));
    }

    #[tokio::test]
    async fn upnp_dsd64_accepts_generated_dsd64_output_config() {
        let state = app_state("upnp-dsd64-accepts-dsd64");
        let target = upnp_target_with_dsd("renderer-dsd64-ok", Some(64));
        let zone_id = receiver_zone_id(&target.id);
        state.zones().sync_upnp_renderers(vec![UpnpRenderer {
            target,
            online: true,
        }]);

        update_playback_config_for_zone(
            &state,
            &zone_id,
            PlaybackConfigUpdate {
                filter_type: "Minimum16k".to_string(),
                target_rate: 192_000,
                target_bit_depth: 24,
                upsampling_enabled: true,
                exclusive: false,
                output_mode: Some("Dsd64".to_string()),
                dsd_modulator: Some("EcDepth2".to_string()),
                dsd_isi_penalty: 0.0,
                dsd_rules_enabled: false,
                dsd_rules: Vec::new(),
                headroom_db: -3.0,
                dsp_buffer_ms: 0,
            },
        )
        .expect("UPnP DSD64 renderer should accept generated DSD64");

        let config = playback_config_for_zone(&state, &zone_id, &state.zones().active_player());
        assert_eq!(config.output_mode, "Dsd64");
    }

    #[test]
    fn upnp_dsd64_saved_dsd64_mode_survives_capability_filter() {
        let state = app_state("upnp-dsd64-keeps-dsd64");
        let target = upnp_target_with_dsd("renderer-dsd64-saved", Some(64));
        let zone_id = receiver_zone_id(&target.id);
        state.zones().sync_upnp_renderers(vec![UpnpRenderer {
            target,
            online: true,
        }]);
        let _ = state
            .settings()
            .update_playback_for_zone(&zone_id, |settings| {
                settings.upsampling_enabled = Some(true);
                settings.output_mode = Some("Dsd64".to_string());
                settings.dsd_rules_enabled = true;
                settings.dsd_rules = vec![
                    DsdSourceRule {
                        source_rate: 44_100,
                        filter_type: "Minimum16k".to_string(),
                        output_mode: "Dsd64".to_string(),
                    },
                    DsdSourceRule {
                        source_rate: 48_000,
                        filter_type: "Minimum16k".to_string(),
                        output_mode: "Dsd128".to_string(),
                    },
                ];
            });

        let config = playback_config_for_zone(&state, &zone_id, &state.zones().active_player());

        assert_eq!(config.output_mode, "Dsd64");
        assert_eq!(config.dsd_rules.len(), 1);
        assert_eq!(config.dsd_rules[0].output_mode, "Dsd64");
    }

    #[test]
    fn upnp_wav_carrier_allows_dop_dsd64_without_native_dsd_capability() {
        let state = app_state("upnp-dop-dsd64");
        let mut target = upnp_target_with_dsd("renderer-dop-dsd64", None);
        target
            .pcm_containers
            .push(crate::protocol::UpnpPcmContainerCapability {
                container: UpnpPcmContainer::Wav,
                max_sample_rate: 192_000,
                max_bit_depth: 24,
            });
        let zone_id = receiver_zone_id(&target.id);
        state.zones().sync_upnp_renderers(vec![UpnpRenderer {
            target,
            online: true,
        }]);
        let _ = state
            .settings()
            .update_playback_for_zone(&zone_id, |settings| {
                settings.upsampling_enabled = Some(true);
                settings.output_mode = Some("Dsd64".to_string());
            });

        let config = playback_config_for_zone(&state, &zone_id, &state.zones().active_player());

        assert_eq!(config.output_mode, "Dsd64");
    }

    #[test]
    fn upnp_dsd64_saved_mode_is_bypassed_when_upsampling_toggle_is_disabled() {
        let state = app_state("upnp-dsd64-upsampling-off");
        let target = upnp_target_with_dsd("renderer-dsd64-upsampling-off", Some(64));
        let zone_id = receiver_zone_id(&target.id);
        state.zones().sync_upnp_renderers(vec![UpnpRenderer {
            target,
            online: true,
        }]);
        let _ = state
            .settings()
            .update_playback_for_zone(&zone_id, |settings| {
                settings.upsampling_enabled = Some(false);
                settings.output_mode = Some("Dsd64".to_string());
            });

        let config = playback_config_for_zone(&state, &zone_id, &state.zones().active_player());

        assert!(!config.upsampling_enabled);
        assert_eq!(config.output_mode, "Pcm");
        assert!(config.dsd_rules.is_empty());
        assert_eq!(
            state
                .settings()
                .playback_for_zone(&zone_id)
                .output_mode
                .as_deref(),
            Some("Dsd64")
        );
    }

    #[test]
    fn upnp_dsd64_legacy_saved_dsd_resolves_to_pcm() {
        let state = app_state("upnp-dsd64-sanitizes-legacy-dsd");
        let target = upnp_target_with_dsd("renderer-dsd64-legacy", Some(64));
        let zone_id = receiver_zone_id(&target.id);
        state.zones().sync_upnp_renderers(vec![UpnpRenderer {
            target,
            online: true,
        }]);
        let _ = state
            .settings()
            .update_playback_for_zone(&zone_id, |settings| {
                settings.upsampling_enabled = Some(true);
                settings.output_mode = Some("Dsd128".to_string());
                settings.dsd_rules_enabled = true;
                settings.dsd_rules = vec![DsdSourceRule {
                    source_rate: 44_100,
                    filter_type: "Minimum16k".to_string(),
                    output_mode: "Dsd128".to_string(),
                }];
            });

        let config = playback_config_for_zone(&state, &zone_id, &state.zones().active_player());

        assert_eq!(config.output_mode, "Pcm");
        assert!(config.dsd_rules.is_empty());
    }

    fn upnp_target(id: &str) -> UpnpRendererTarget {
        upnp_target_with_dsd(id, Some(128))
    }

    fn upnp_target_with_dsd(id: &str, max_dsd_rate: Option<u16>) -> UpnpRendererTarget {
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
            max_dsd_rate,
            capability_detection_source: CapabilityDetectionSource::Probed,
            capability_detection_status: CapabilityDetectionStatus::Complete,
            capability_detection_message: None,
            protocol_info: Vec::new(),
            pcm_containers: Vec::new(),
        }
    }
}
