// Compatibility facade for older playback call sites; keep re-exports narrow and documented.
#[allow(unused_imports)]
pub(crate) use super::airplay_volume::{
    airplay_default_start_volume, prepare_airplay_volume_for_zone,
};
// Compatibility facade for older playback call sites; remove helpers after imports migrate.
#[allow(unused_imports)]
pub(crate) use super::apply_settings::{
    apply_active_eq_config, apply_active_zone_playback_settings,
    apply_active_zone_playback_settings_if_changed, apply_eq_config_for_zone,
    apply_playback_settings_for_zone, remember_active_zone_playback_settings_applied,
    select_active_output_device,
};
// Compatibility facade for older playback call sites; remove helpers after imports migrate.
#[allow(unused_imports)]
pub(crate) use super::config::{
    PlaybackConfigUpdate, effective_dsd_rules, effective_dsd_rules_from_zone_settings,
    effective_output_mode_for_upsampling, playback_config_for_zone, update_active_playback_config,
    update_playback_config_for_zone, validate_dsd_rules,
};
// Compatibility facade for older playback call sites; remove helpers after imports migrate.
#[allow(unused_imports)]
pub(crate) use super::hegel_control::{
    hegel_settings_for_zone, normalize_hegel_settings, prepare_hegel_for_zone,
    query_hegel_status_for_target, query_hegel_status_for_zone_target, set_hegel_input_for_target,
    set_hegel_mute_for_target, set_hegel_power_for_target, set_hegel_volume_for_target,
    should_apply_hegel_default_volume, update_hegel_settings, validated_hegel_target,
};
// Compatibility facade for older playback call sites; remove helpers after imports migrate.
#[allow(unused_imports)]
pub(crate) use super::zone_service::{
    ZoneSettingsUpdate, disable_playback_zone, enable_playback_zone,
    maybe_select_remote_zone_for_unavailable_active_device, persist_calibrated_upnp_capabilities,
    refresh_playback_zones, register_remote_agent_playback_zones, rename_playback_zone,
    select_playback_zone, spawn_playback_zone_cache_warmer, unregister_remote_agent_playback_zones,
    update_playback_zone_settings, update_remote_agent_buffer_state,
    update_remote_agent_playback_state, update_remote_agent_signal_path,
};
