use crate::app::state::AppState;
use crate::audio::upnp::UpnpRendererTarget;
use crate::diagnostics::status::DiagnosticActivity;
use crate::library::{
    BrowserStreamSettings, ZoneDefinition, ZoneHegelSettings, ZoneSettings, ZoneUpnpCapabilities,
};
use crate::playback::apply_settings::{
    apply_active_zone_playback_settings, apply_active_zone_playback_settings_if_changed,
};
use crate::playback::error::PlaybackError;
use crate::playback::hegel_control::{normalize_hegel_settings, validate_hegel_target_policy};
use crate::playback::output_devices::{output_device_available, output_device_names};
use crate::protocol::{
    AgentBufferState, AgentCapabilities, AgentPlaybackState, CoreToAgentCommand, SinkProtocol,
    SyncSignalPath, ZoneProfile, ZoneStatus,
};
use std::time::Duration;
use tokio::sync::mpsc;

const ZONE_CACHE_REFRESH_INTERVAL: Duration = Duration::from_secs(30);
const HEGEL_SAVED_ZONE_MESSAGE: &str = "Hegel USB is not currently detected; enable standby visibility to wake it from the network link.";
const HEGEL_STANDBY_ZONE_MESSAGE: &str =
    "Hegel network link is visible; USB will wake before playback starts.";

#[derive(Default)]
pub(crate) struct ZoneSettingsUpdate {
    pub airplay_default_volume_enabled: Option<bool>,
    pub airplay_default_volume: Option<f32>,
    pub airplay_max_volume: Option<f32>,
    pub qobuz_hires_enabled: Option<bool>,
    pub icon: Option<String>,
    pub device_type: Option<String>,
    pub hegel: Option<ZoneHegelSettings>,
    pub upnp_capabilities: Option<ZoneUpnpCapabilities>,
    pub browser_stream: Option<BrowserStreamSettings>,
}

/// Browser stream settings are user input from the zone settings modal:
/// normalize the format and pin the bitrate to the allowed encoder set.
fn validate_browser_stream_settings(
    settings: BrowserStreamSettings,
) -> Result<BrowserStreamSettings, PlaybackError> {
    let format = settings.format.trim().to_ascii_lowercase();
    if format != "flac" && format != "opus" {
        return Err(PlaybackError::bad_request(
            "Browser stream format must be \"flac\" or \"opus\"",
        ));
    }
    if !crate::audio::transcode::opus::ALLOWED_BITRATE_KBPS.contains(&settings.opus_kbps) {
        return Err(PlaybackError::bad_request(
            "Browser Opus bitrate must be 128, 256, or 320",
        ));
    }
    Ok(BrowserStreamSettings {
        format,
        opus_kbps: settings.opus_kbps,
    })
}

pub(crate) fn select_playback_zone(state: &AppState, zone_id: &str) -> Result<(), PlaybackError> {
    state
        .zones()
        .select_zone(zone_id)
        .map_err(PlaybackError::bad_request)?;
    state
        .settings()
        .update(|s| s.active_zone_id = Some(zone_id.to_string()))
        .map_err(PlaybackError::integration)?;
    apply_active_zone_playback_settings(state);
    Ok(())
}

pub(crate) fn enable_playback_zone(state: &AppState, zone_id: &str) -> Result<(), PlaybackError> {
    state
        .zones()
        .enable_zone(zone_id)
        .map_err(PlaybackError::bad_request)?;
    let _ = state.library().set_zone_enabled(zone_id, true);
    let preferred_zone_id = state.zones().preferred_active_zone_id();
    state
        .settings()
        .update(|s| s.active_zone_id = Some(preferred_zone_id))
        .map_err(PlaybackError::integration)?;
    apply_active_zone_playback_settings(state);
    Ok(())
}

pub(crate) fn disable_playback_zone(state: &AppState, zone_id: &str) -> Result<(), PlaybackError> {
    state
        .zones()
        .disable_zone(zone_id)
        .map_err(PlaybackError::bad_request)?;
    let _ = state.library().set_zone_enabled(zone_id, false);
    let preferred_zone_id = state.zones().preferred_active_zone_id();
    state
        .settings()
        .update(|s| s.active_zone_id = Some(preferred_zone_id))
        .map_err(PlaybackError::integration)?;
    apply_active_zone_playback_settings(state);
    Ok(())
}

pub(crate) fn rename_playback_zone(
    state: &AppState,
    zone_id: &str,
    name: &str,
) -> Result<(), PlaybackError> {
    state
        .zones()
        .rename_zone(zone_id, name)
        .map_err(PlaybackError::bad_request)?;
    state
        .library()
        .rename_zone(zone_id, name.trim())
        .map_err(PlaybackError::library)?;
    Ok(())
}

pub(crate) fn update_playback_zone_settings(
    state: &AppState,
    zone_id: &str,
    update: ZoneSettingsUpdate,
) -> Result<ZoneSettings, PlaybackError> {
    let zone = state
        .zones()
        .list_zones()
        .into_iter()
        .find(|zone| zone.id == zone_id)
        .ok_or_else(|| PlaybackError::not_found(format!("Zone '{zone_id}' is not available")))?;
    state
        .library()
        .upsert_zone_definition(
            &zone.id,
            &zone.name,
            zone_definition_kind(&zone.protocol),
            zone.device_name.as_deref(),
            zone.enabled,
        )
        .map_err(PlaybackError::library)?;
    let mut settings = state
        .library()
        .zone_settings(zone_id)
        .map_err(PlaybackError::library)?;
    let icon = update
        .icon
        .as_deref()
        .map(normalize_zone_icon)
        .transpose()?;
    if let Some(max_volume) = update.airplay_max_volume {
        settings = state
            .library()
            .set_zone_airplay_max_volume(zone_id, max_volume)
            .map_err(PlaybackError::library)?;
    }
    if let Some(enabled) = update.airplay_default_volume_enabled {
        let default_volume =
            if enabled {
                Some(update.airplay_default_volume.ok_or_else(|| {
                    PlaybackError::bad_request("Default AirPlay volume is required")
                })?)
            } else {
                None
            };
        settings = state
            .library()
            .set_zone_airplay_default_volume(zone_id, default_volume)
            .map_err(PlaybackError::library)?;
    } else if let Some(default_volume) = update.airplay_default_volume {
        settings = state
            .library()
            .set_zone_airplay_default_volume(zone_id, Some(default_volume))
            .map_err(PlaybackError::library)?;
    }
    if update.icon.is_some() {
        settings.icon = icon.flatten();
        settings = state
            .library()
            .set_zone_settings(zone_id, settings)
            .map_err(PlaybackError::library)?;
    }
    if let Some(enabled) = update.qobuz_hires_enabled {
        settings.qobuz_hires_enabled = enabled;
        settings = state
            .library()
            .set_zone_settings(zone_id, settings)
            .map_err(PlaybackError::library)?;
    }
    if let Some(browser_stream) = update.browser_stream {
        settings.browser_stream = Some(validate_browser_stream_settings(browser_stream)?);
        settings = state
            .library()
            .set_zone_settings(zone_id, settings.clone())
            .map_err(PlaybackError::library)?;
    }
    if update.device_type.is_some() || update.hegel.is_some() {
        let device_type = update
            .device_type
            .as_deref()
            .map(normalize_zone_device_type)
            .transpose()?
            .unwrap_or_else(|| {
                settings
                    .device_type
                    .as_deref()
                    .unwrap_or("none")
                    .to_string()
            });
        match device_type.as_str() {
            "none" => {
                settings.device_type = None;
                settings.hegel = None;
            }
            "hegel" => {
                let hegel = update
                    .hegel
                    .or_else(|| settings.hegel.clone())
                    .ok_or_else(|| PlaybackError::bad_request("Hegel settings are required"))?;
                let hegel = normalize_zone_hegel_settings(hegel);
                if let Some(host) = hegel.host.as_deref() {
                    validate_hegel_target_policy(host, hegel.port)?;
                }
                settings.device_type = Some("hegel".to_string());
                settings.hegel = Some(hegel);
            }
            _ => return Err(PlaybackError::bad_request("Unsupported device type")),
        }
        settings = state
            .library()
            .set_zone_settings(zone_id, settings)
            .map_err(PlaybackError::library)?;
    }
    if let Some(capabilities) = update.upnp_capabilities {
        if zone.protocol != SinkProtocol::UpnpAvRenderer {
            return Err(PlaybackError::bad_request(
                "UPnP capabilities can only be set on UPnP zones",
            ));
        }
        if settings.upnp_calibrated_capabilities.is_none() {
            settings.upnp_calibrated_capabilities = settings.upnp_capabilities.clone();
        }
        settings.upnp_capabilities = Some(normalize_zone_upnp_capabilities(capabilities)?);
        settings = state
            .library()
            .set_zone_settings(zone_id, settings)
            .map_err(PlaybackError::library)?;
    }
    Ok(settings)
}

pub(crate) fn persist_calibrated_upnp_capabilities(
    state: &AppState,
    zone_id: &str,
    target: &UpnpRendererTarget,
) -> Result<(), PlaybackError> {
    let zone = state
        .zones()
        .list_zones()
        .into_iter()
        .find(|zone| zone.id == zone_id);
    state
        .library()
        .upsert_zone_definition(
            zone_id,
            zone.as_ref()
                .map(|zone| zone.name.as_str())
                .unwrap_or(target.name.as_str()),
            "upnp_av_renderer",
            Some(&crate::audio::upnp::target_device_name(target)),
            zone.as_ref().map(|zone| zone.enabled).unwrap_or(true),
        )
        .map_err(PlaybackError::library)?;
    let mut settings = state
        .library()
        .zone_settings(zone_id)
        .map_err(PlaybackError::library)?;
    settings.upnp_capabilities = Some(ZoneUpnpCapabilities {
        max_sample_rate: target.max_sample_rate,
        max_bit_depth: target.max_bit_depth,
        max_dsd_rate: target.max_dsd_rate,
        pcm_containers: target.pcm_containers.clone(),
    });
    settings.upnp_calibrated_capabilities = settings.upnp_capabilities.clone();
    state
        .library()
        .set_zone_settings(zone_id, settings)
        .map_err(PlaybackError::library)?;
    Ok(())
}

fn normalize_zone_icon(value: &str) -> Result<Option<String>, PlaybackError> {
    let normalized = value.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "" | "auto" | "none" => Ok(None),
        "hegel" | "mac_mini" | "sonos" | "kef" | "airplay" | "computer" | "speaker" => {
            Ok(Some(normalized))
        }
        _ => Err(PlaybackError::bad_request("Unsupported output icon")),
    }
}

fn normalize_zone_device_type(value: &str) -> Result<String, PlaybackError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "hegel" => Ok("hegel".to_string()),
        "" | "none" => Ok("none".to_string()),
        _ => Err(PlaybackError::bad_request("Unsupported device type")),
    }
}

pub(crate) fn normalize_zone_hegel_settings(mut settings: ZoneHegelSettings) -> ZoneHegelSettings {
    settings.host = settings
        .host
        .map(|host| host.trim().to_string())
        .filter(|host| !host.is_empty());
    settings.linked_airplay_zone_id = settings
        .linked_airplay_zone_id
        .map(|zone| zone.trim().to_string())
        .filter(|zone| !zone.is_empty());
    settings.model = settings
        .model
        .map(|model| model.trim().to_ascii_lowercase())
        .filter(|model| !model.is_empty());
    if settings.port == 0 {
        settings.port = 50001;
    }
    settings.input = settings.input.clamp(1, 20);
    settings.max_volume = settings.max_volume.min(100);
    settings.default_volume = settings.default_volume.min(settings.max_volume);
    settings
}

fn normalize_zone_upnp_capabilities(
    capabilities: ZoneUpnpCapabilities,
) -> Result<ZoneUpnpCapabilities, PlaybackError> {
    const PCM_RATES: &[u32] = &[
        44_100, 48_000, 88_200, 96_000, 176_400, 192_000, 352_800, 384_000,
    ];
    const DSD_RATES: &[u16] = &[64, 128, 256];
    if !PCM_RATES.contains(&capabilities.max_sample_rate) {
        return Err(PlaybackError::bad_request("Unsupported UPnP PCM rate"));
    }
    let max_bit_depth = match capabilities.max_bit_depth {
        16 | 24 | 32 => capabilities.max_bit_depth,
        0 => 24,
        _ => return Err(PlaybackError::bad_request("Unsupported UPnP bit depth")),
    };
    if let Some(rate) = capabilities.max_dsd_rate
        && !DSD_RATES.contains(&rate)
    {
        return Err(PlaybackError::bad_request("Unsupported UPnP DSD rate"));
    }
    Ok(ZoneUpnpCapabilities {
        max_sample_rate: capabilities.max_sample_rate,
        max_bit_depth,
        max_dsd_rate: capabilities.max_dsd_rate,
        pcm_containers: normalize_upnp_pcm_containers(capabilities.pcm_containers)?,
    })
}

fn normalize_upnp_pcm_containers(
    containers: Vec<crate::protocol::UpnpPcmContainerCapability>,
) -> Result<Vec<crate::protocol::UpnpPcmContainerCapability>, PlaybackError> {
    use crate::protocol::UpnpPcmContainer;
    const PCM_RATES: &[u32] = &[
        44_100, 48_000, 88_200, 96_000, 176_400, 192_000, 352_800, 384_000,
    ];
    let mut normalized: Vec<crate::protocol::UpnpPcmContainerCapability> = Vec::new();
    for capability in containers {
        if !PCM_RATES.contains(&capability.max_sample_rate) {
            return Err(PlaybackError::bad_request(
                "Unsupported UPnP PCM container rate",
            ));
        }
        let max_bit_depth = match capability.max_bit_depth {
            16 | 24 | 32 => capability.max_bit_depth,
            0 => 24,
            _ => {
                return Err(PlaybackError::bad_request(
                    "Unsupported UPnP PCM container bit depth",
                ));
            }
        };
        if let Some(existing) = normalized
            .iter_mut()
            .find(|existing| existing.container == capability.container)
        {
            existing.max_sample_rate = existing.max_sample_rate.max(capability.max_sample_rate);
            existing.max_bit_depth = existing.max_bit_depth.max(max_bit_depth);
        } else {
            normalized.push(crate::protocol::UpnpPcmContainerCapability {
                container: capability.container,
                max_sample_rate: capability.max_sample_rate,
                max_bit_depth,
            });
        }
    }
    normalized.sort_by_key(|capability| match capability.container {
        UpnpPcmContainer::Flac => 0,
        UpnpPcmContainer::Wav => 1,
    });
    Ok(normalized)
}

pub(crate) fn refresh_playback_zones(state: &AppState) -> Vec<ZoneProfile> {
    refresh_playback_zones_inner(state)
}

fn refresh_playback_zones_inner(state: &AppState) -> Vec<ZoneProfile> {
    // CoreAudio can temporarily omit a USB DAC from enumeration while Fozmo's
    // direct DoP AudioUnit owns it. Treat that as an unsafe time to refresh for
    // both background and interactive callers: marking the selected zone
    // offline here strands it as soon as playback reaches EOF.
    let mut quiet_local_refresh = coreaudio_dop_output_owned(state);
    let local_devices = if quiet_local_refresh {
        Vec::new()
    } else {
        let _scan = state
            .diagnostics()
            .begin_activity(DiagnosticActivity::LocalAudioDeviceScan);
        let local_devices = output_device_names();
        // Close the race where playback acquires Hog Mode after the first
        // ownership check but before enumeration completes.
        if coreaudio_dop_output_owned(state) {
            quiet_local_refresh = true;
            Vec::new()
        } else {
            state.zones().sync_local_devices(local_devices.clone());
            local_devices
        }
    };
    state
        .zones()
        .sync_airplay_receivers(state.airplay().receivers());
    #[cfg(feature = "sonos")]
    {
        let mut sonos_speakers = state.sonos().speakers();
        #[cfg(feature = "upnp")]
        for renderer in state.upnp().renderers() {
            if !renderer.online
                || !renderer
                    .target
                    .manufacturer
                    .as_deref()
                    .is_some_and(|name| name.to_ascii_lowercase().contains("sonos"))
                || sonos_speakers
                    .iter()
                    .any(|speaker| speaker.target.id == renderer.target.id)
            {
                continue;
            }
            sonos_speakers.push(crate::audio::sonos::SonosSpeaker {
                target: crate::audio::sonos::SonosTarget {
                    id: renderer.target.id,
                    name: renderer.target.name,
                    host: renderer.target.host,
                    port: renderer.target.port,
                    model: renderer.target.model,
                    coordinator: true,
                    group_name: None,
                },
                online: true,
            });
        }
        state.zones().sync_sonos_speakers(sonos_speakers);
    }
    #[cfg(feature = "upnp")]
    state.zones().sync_upnp_renderers(state.upnp().renderers());

    let mut zones = state.zones().list_zones();
    persist_zone_definitions(state, &zones);
    if let Ok(definitions) = state.library().zone_definitions() {
        state.zones().apply_zone_definitions(definitions);
        #[cfg(feature = "hegel")]
        if let Ok(definitions) = state.library().zone_definitions() {
            sync_hegel_configured_zone(state, &definitions);
        }
        zones = state.zones().list_zones();
        persist_zone_definitions(state, &zones);
    }
    if !quiet_local_refresh {
        apply_active_zone_playback_settings_if_changed(state);
        apply_active_discovered_device_selection(state, &local_devices);
    }

    enrich_zone_settings(state, zones)
}

fn coreaudio_dop_output_owned(state: &AppState) -> bool {
    state.zones().coreaudio_dop_output_owned()
}

fn persist_zone_definitions(state: &AppState, zones: &[ZoneProfile]) {
    for zone in zones {
        let _ = state.library().upsert_zone_definition(
            &zone.id,
            &zone.name,
            zone_definition_kind(&zone.protocol),
            zone.device_name.as_deref(),
            zone.enabled,
        );
    }
}

fn sync_hegel_configured_zone(state: &AppState, definitions: &[ZoneDefinition]) {
    let settings = normalize_hegel_settings(state.settings().hegel_settings());
    if !settings.enabled {
        return;
    }
    let Some(zone_id) = settings.zone_id.as_deref() else {
        return;
    };
    let Some(definition) = definitions
        .iter()
        .find(|definition| definition.id == zone_id && definition_is_local_output(definition))
    else {
        return;
    };
    let Some(device_name) = definition
        .device_name
        .as_deref()
        .map(str::trim)
        .filter(|device_name| !device_name.is_empty())
    else {
        return;
    };

    state.zones().sync_saved_local_zone(
        &definition.id,
        &definition.name,
        device_name,
        definition.enabled,
        HEGEL_SAVED_ZONE_MESSAGE,
    );

    if !settings.standby_usb_visible || !hegel_network_link_visible(state) {
        return;
    }
    state.zones().sync_standby_local_zone(
        &definition.id,
        &definition.name,
        device_name,
        HEGEL_STANDBY_ZONE_MESSAGE,
    );
    let _ = state.library().set_zone_enabled(&definition.id, true);
}

fn hegel_network_link_visible(state: &AppState) -> bool {
    let settings = normalize_hegel_settings(state.settings().hegel_settings());
    if let Some(linked_zone_id) = settings.linked_airplay_zone_id.as_deref() {
        return state.zones().list_zones().into_iter().any(|zone| {
            zone.id == linked_zone_id
                && matches!(
                    zone.protocol,
                    SinkProtocol::AirPlayCoreAudio
                        | SinkProtocol::AirPlayRaop
                        | SinkProtocol::AirPlay2
                )
                && zone.status != ZoneStatus::Offline
        });
    }
    settings
        .host
        .as_deref()
        .is_some_and(|host| !host.trim().is_empty())
}

fn definition_is_local_output(definition: &ZoneDefinition) -> bool {
    !matches!(
        definition.kind.as_deref(),
        Some("airplay_coreaudio" | "airplay_raop" | "airplay2" | "sonos_upnp" | "remote_agent")
            | Some("upnp_av_renderer")
    )
}

pub(crate) fn spawn_playback_zone_cache_warmer(state: AppState) {
    tokio::spawn(async move {
        loop {
            let refresh_state = state.clone();
            if let Err(err) =
                tokio::task::spawn_blocking(move || refresh_playback_zones_inner(&refresh_state))
                    .await
            {
                eprintln!("zones: background refresh failed: {err}");
            }
            tokio::time::sleep(ZONE_CACHE_REFRESH_INTERVAL).await;
        }
    });
}

fn enrich_zone_settings(state: &AppState, mut zones: Vec<ZoneProfile>) -> Vec<ZoneProfile> {
    for zone in &mut zones {
        if let Ok(settings) = state.library().zone_settings(&zone.id) {
            zone.airplay_default_volume = settings.airplay_default_volume;
            zone.airplay_max_volume = settings.airplay_max_volume;
            zone.airplay_last_volume = settings.airplay_last_volume;
            zone.qobuz_hires_enabled = settings.qobuz_hires_enabled;
            zone.icon = settings.icon;
            zone.device_type = settings.device_type;
            zone.hegel = settings.hegel;
            zone.browser_stream = settings.browser_stream;
            zone.upnp_calibrated_capabilities = settings
                .upnp_calibrated_capabilities
                .or_else(|| settings.upnp_capabilities.clone());
            if zone.protocol == SinkProtocol::UpnpAvRenderer
                && let Some(capabilities) = settings.upnp_capabilities
            {
                zone.capabilities.max_sample_rate = capabilities.max_sample_rate;
                zone.capabilities.max_bit_depth = capabilities.max_bit_depth;
                zone.capabilities.max_dsd_rate = capabilities.max_dsd_rate;
                zone.capabilities.supports_dsd128 =
                    capabilities.max_dsd_rate.is_some_and(|rate| rate >= 128);
                zone.capabilities.supports_dsd256 =
                    capabilities.max_dsd_rate.is_some_and(|rate| rate >= 256);
                zone.capabilities.capability_detection_source =
                    crate::protocol::CapabilityDetectionSource::Probed;
                zone.capabilities.capability_detection_status =
                    crate::protocol::CapabilityDetectionStatus::Complete;
                zone.capabilities.capability_detection_message =
                    Some("Saved UPnP capability override".to_string());
            }
        }
    }
    zones
}

pub(crate) fn register_remote_agent_playback_zones(
    state: &AppState,
    agent_id: String,
    name: String,
    capabilities: AgentCapabilities,
    cmd_tx: mpsc::UnboundedSender<CoreToAgentCommand>,
) -> u64 {
    let connection_id = state
        .zones()
        .register_agent(agent_id, name, capabilities, cmd_tx);
    if let Ok(definitions) = state.library().zone_definitions() {
        state.zones().apply_zone_definitions(definitions);
    }
    if maybe_select_remote_zone_for_unavailable_active_device(state).is_none() {
        apply_active_zone_playback_settings_if_changed(state);
    }
    connection_id
}

pub(crate) fn update_remote_agent_playback_state(
    state: &AppState,
    agent_id: &str,
    playback: AgentPlaybackState,
) {
    state
        .zones()
        .update_playback(agent_id, playback, state.public_base_url());
}

pub(crate) fn update_remote_agent_buffer_state(
    state: &AppState,
    agent_id: &str,
    buffer: AgentBufferState,
) {
    state.zones().update_buffer(agent_id, buffer);
}

pub(crate) fn update_remote_agent_signal_path(
    state: &AppState,
    agent_id: &str,
    signal_path: SyncSignalPath,
) {
    state.zones().update_signal_path(agent_id, signal_path);
}

pub(crate) fn unregister_remote_agent_playback_zones(
    state: &AppState,
    agent_id: &str,
    connection_id: u64,
) {
    state
        .zones()
        .unregister_agent_connection(agent_id, connection_id);
    apply_active_zone_playback_settings_if_changed(state);
}

pub(crate) fn maybe_select_remote_zone_for_unavailable_active_device(
    state: &AppState,
) -> Option<String> {
    let active_zone_id = state.zones().active_zone_id();
    if state.zones().zone_protocol(&active_zone_id) == Some(SinkProtocol::RemoteAgent) {
        return None;
    }

    let saved_device = state
        .settings()
        .playback_for_zone(&active_zone_id)
        .device_name?;
    let saved_device = saved_device.trim();
    if saved_device.is_empty() || output_device_available(saved_device) {
        return None;
    }

    let zone_id = state
        .zones()
        .list_zones()
        .into_iter()
        .find(|zone| {
            zone.protocol == SinkProtocol::RemoteAgent
                && zone.enabled
                && zone.device_name.as_deref().map(str::trim) == Some(saved_device)
        })?
        .id;

    state.zones().select_zone(&zone_id).ok()?;
    if let Err(error) = state
        .settings()
        .update(|settings| settings.active_zone_id = Some(zone_id.clone()))
    {
        eprintln!("settings: failed to persist automatically selected zone: {error}");
        return None;
    }
    apply_active_zone_playback_settings(state);
    Some(zone_id)
}

fn apply_active_discovered_device_selection(
    state: &AppState,
    available_devices: &[String],
) -> bool {
    let zone_id = state.zones().active_zone_id();
    if state.zones().zone_protocol(&zone_id) == Some(SinkProtocol::RemoteAgent) {
        return false;
    }

    let configured_device = state
        .zones()
        .zone_bound_device_name(&zone_id)
        .or_else(|| state.settings().playback_for_zone(&zone_id).device_name)
        .map(|device| device.trim().to_string())
        .filter(|device| !device.is_empty());
    let Some(configured_device) = configured_device else {
        return false;
    };

    if !available_devices
        .iter()
        .any(|device| device.trim() == configured_device)
    {
        return false;
    }

    let player = state.zones().active_player();
    if player.selected_device_name().as_deref().map(str::trim) == Some(configured_device.as_str()) {
        return false;
    }

    player.select_device(Some(configured_device));
    true
}

fn zone_definition_kind(protocol: &SinkProtocol) -> &'static str {
    match protocol {
        SinkProtocol::RemoteAgent => "remote_agent",
        SinkProtocol::AirPlayCoreAudio => "airplay_coreaudio",
        SinkProtocol::AirPlayRaop => "airplay_raop",
        SinkProtocol::AirPlay2 => "airplay2",
        SinkProtocol::SonosUpnp => "sonos_upnp",
        SinkProtocol::UpnpAvRenderer => "upnp_av_renderer",
        SinkProtocol::AsioOutput => "asio",
        SinkProtocol::LocalCoreAudio => "local_coreaudio",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::player::OutputMode;
    use crate::playback::test_support::{agent_capabilities, app_state};
    use crate::protocol::CoreToAgentCommand;
    use crate::zones::local_device_zone_id;

    fn expected_dsd256_mode_for_build() -> OutputMode {
        if cfg!(feature = "experimental_dsd256") {
            OutputMode::Dsd256
        } else {
            OutputMode::Dsd128
        }
    }

    #[test]
    fn enabling_and_disabling_playback_zone_persists_preferred_active_zone() {
        let state = app_state("enable-disable-playback-zone");
        let device_name = "USB DAC";
        let zone_id = local_device_zone_id(device_name);
        state
            .zones()
            .sync_local_devices(vec![device_name.to_string()]);

        enable_playback_zone(&state, &zone_id).unwrap();

        assert_eq!(state.zones().active_zone_id(), zone_id);
        assert_eq!(
            state.settings().snapshot().active_zone_id.as_deref(),
            Some(zone_id.as_str())
        );

        disable_playback_zone(&state, &zone_id).unwrap();

        assert_eq!(state.zones().active_zone_id(), "local-core");
        assert_eq!(
            state.settings().snapshot().active_zone_id.as_deref(),
            Some("local-core")
        );
    }

    #[test]
    fn refreshing_playback_zones_applies_active_playback_settings() {
        let state = app_state("refresh-playback-zones");
        let zone_id = state.zones().active_zone_id();
        let _ = state
            .settings()
            .update_playback_for_zone(&zone_id, |settings| {
                settings.upsampling_enabled = Some(true);
                settings.output_mode = Some("Dsd256".to_string());
            });

        refresh_playback_zones(&state);

        assert_eq!(
            state.zones().active_player().output_mode(),
            expected_dsd256_mode_for_build()
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
    fn background_refresh_applies_active_playback_settings_when_not_coreaudio_dop_active() {
        let state = app_state("background-refresh-zones");
        let zone_id = state.zones().active_zone_id();
        let _ = state
            .settings()
            .update_playback_for_zone(&zone_id, |settings| {
                settings.upsampling_enabled = Some(true);
                settings.output_mode = Some("Dsd256".to_string());
            });

        refresh_playback_zones_inner(&state);

        assert_eq!(
            state.zones().active_player().output_mode(),
            expected_dsd256_mode_for_build()
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
    fn background_refresh_during_coreaudio_dop_skips_active_playback_settings() {
        let state = app_state("background-refresh-coreaudio-dop");
        let zone_id = state.zones().active_zone_id();
        let player = state.zones().active_player();
        player.set_playback_state_for_test(crate::audio::player::PlaybackState::Playing);
        player.set_coreaudio_dop_buffer_health_for_test(5_644_800, 65_536, 32_768, 32_768, 4096);
        let _ = state
            .settings()
            .update_playback_for_zone(&zone_id, |settings| {
                settings.output_mode = Some("Dsd256".to_string());
            });

        assert!(coreaudio_dop_output_owned(&state));

        refresh_playback_zones_inner(&state);

        assert_eq!(state.zones().active_player().output_mode(), OutputMode::Pcm);
        assert_eq!(state.playback_config_applicator().applied_zone_id(), None);
    }

    #[test]
    fn interactive_refresh_keeps_owned_coreaudio_dop_zone_available_after_eof() {
        let state = app_state("interactive-refresh-coreaudio-dop");
        let device_name = "Fozmo test DoP DAC that is absent from discovery";
        let zone_id = local_device_zone_id(device_name);
        state
            .zones()
            .sync_local_devices(vec![device_name.to_string()]);
        enable_playback_zone(&state, &zone_id).unwrap();

        let player = state.zones().active_player();
        player.set_coreaudio_dop_buffer_health_for_test(5_644_800, 65_536, 0, 32_768, 4096);
        player.set_playback_state_for_test(crate::audio::player::PlaybackState::Stopped);

        assert!(coreaudio_dop_output_owned(&state));
        refresh_playback_zones_inner(&state);

        assert!(state.zones().player_for_zone(&zone_id).is_some());
        assert!(
            crate::playback::status::build_status_response_for_zone(&state, &zone_id).is_ok(),
            "the status endpoint must not regress to a 404 after EOF"
        );
        assert_ne!(
            state
                .zones()
                .list_zones()
                .into_iter()
                .find(|zone| zone.id == zone_id)
                .expect("DoP zone should remain registered")
                .status,
            ZoneStatus::Offline
        );
    }

    #[test]
    fn discovered_device_selection_retries_saved_device_for_active_zone() {
        let state = app_state("retry-saved-device");
        let zone_id = state.zones().active_zone_id();
        let device_name = "Hegel H390 USB";
        let _ = state
            .settings()
            .update_playback_for_zone(&zone_id, |settings| {
                settings.device_name = Some(device_name.to_string());
            });

        assert!(apply_active_discovered_device_selection(
            &state,
            &[device_name.to_string()]
        ));
    }

    #[test]
    fn discovered_device_selection_waits_until_saved_device_is_available() {
        let state = app_state("retry-saved-device-unavailable");
        let zone_id = state.zones().active_zone_id();
        let _ = state
            .settings()
            .update_playback_for_zone(&zone_id, |settings| {
                settings.device_name = Some("Hegel H390 USB".to_string());
            });

        assert!(!apply_active_discovered_device_selection(
            &state,
            &["Mac mini Speakers".to_string()]
        ));
    }

    #[test]
    fn renaming_playback_zone_updates_manager_and_library_definition() {
        let state = app_state("rename-playback-zone");
        let zone_id = state.zones().active_zone_id();
        refresh_playback_zones(&state);

        rename_playback_zone(&state, &zone_id, " Listening Room ").unwrap();

        assert_eq!(state.zones().zone_name(&zone_id), "Listening Room");
        let definition = state
            .library()
            .zone_definitions()
            .unwrap()
            .into_iter()
            .find(|definition| definition.id == zone_id)
            .expect("active zone definition should be persisted");
        assert_eq!(definition.name, "Listening Room");
    }

    #[test]
    fn updating_playback_zone_airplay_default_volume_validates_and_persists() {
        let state = app_state("zone-airplay-settings");
        let zone_id = state.zones().active_zone_id();
        refresh_playback_zones(&state);

        let err = update_playback_zone_settings(
            &state,
            &zone_id,
            ZoneSettingsUpdate {
                airplay_default_volume_enabled: Some(true),
                airplay_default_volume: None,
                ..ZoneSettingsUpdate::default()
            },
        )
        .unwrap_err();

        assert!(matches!(err, PlaybackError::BadRequest(_)));

        let settings = update_playback_zone_settings(
            &state,
            &zone_id,
            ZoneSettingsUpdate {
                airplay_default_volume_enabled: Some(true),
                airplay_default_volume: Some(0.75),
                airplay_max_volume: Some(0.6),
                ..ZoneSettingsUpdate::default()
            },
        )
        .unwrap();

        assert_eq!(settings.airplay_default_volume, Some(0.6));
        assert_eq!(settings.airplay_max_volume, Some(0.6));
        assert_eq!(
            state
                .library()
                .zone_settings(&zone_id)
                .unwrap()
                .airplay_default_volume,
            Some(0.6)
        );

        let raised_limits = update_playback_zone_settings(
            &state,
            &zone_id,
            ZoneSettingsUpdate {
                airplay_default_volume_enabled: Some(true),
                airplay_default_volume: Some(0.75),
                airplay_max_volume: Some(0.8),
                ..ZoneSettingsUpdate::default()
            },
        )
        .unwrap();

        assert_eq!(raised_limits.airplay_default_volume, Some(0.75));
        assert_eq!(raised_limits.airplay_max_volume, Some(0.8));
    }

    #[test]
    fn updating_playback_zone_icon_persists_and_enriches_zone_profile() {
        let state = app_state("zone-icon-settings");
        let zone_id = state.zones().active_zone_id();

        let settings = update_playback_zone_settings(
            &state,
            &zone_id,
            ZoneSettingsUpdate {
                icon: Some(" hegel ".to_string()),
                ..ZoneSettingsUpdate::default()
            },
        )
        .unwrap();

        assert_eq!(settings.icon.as_deref(), Some("hegel"));
        assert_eq!(
            state
                .library()
                .zone_settings(&zone_id)
                .unwrap()
                .icon
                .as_deref(),
            Some("hegel")
        );

        let zone = enrich_zone_settings(&state, state.zones().list_zones())
            .into_iter()
            .find(|zone| zone.id == zone_id)
            .expect("zone should remain available");
        assert_eq!(zone.icon.as_deref(), Some("hegel"));
    }

    #[test]
    fn updating_playback_zone_qobuz_hires_persists_and_enriches_zone_profile() {
        let state = app_state("zone-qobuz-hires-settings");
        let zone_id = state.zones().active_zone_id();

        let settings = update_playback_zone_settings(
            &state,
            &zone_id,
            ZoneSettingsUpdate {
                qobuz_hires_enabled: Some(true),
                ..ZoneSettingsUpdate::default()
            },
        )
        .unwrap();

        assert!(settings.qobuz_hires_enabled);
        assert!(
            state
                .library()
                .zone_settings(&zone_id)
                .unwrap()
                .qobuz_hires_enabled
        );

        let zone = enrich_zone_settings(&state, state.zones().list_zones())
            .into_iter()
            .find(|zone| zone.id == zone_id)
            .expect("zone should remain available");
        assert!(zone.qobuz_hires_enabled);
    }

    #[test]
    fn updating_playback_zone_icon_auto_clears_persisted_icon() {
        let state = app_state("zone-icon-clear");
        let zone_id = state.zones().active_zone_id();

        update_playback_zone_settings(
            &state,
            &zone_id,
            ZoneSettingsUpdate {
                icon: Some("kef".to_string()),
                ..ZoneSettingsUpdate::default()
            },
        )
        .unwrap();

        let settings = update_playback_zone_settings(
            &state,
            &zone_id,
            ZoneSettingsUpdate {
                icon: Some("auto".to_string()),
                ..ZoneSettingsUpdate::default()
            },
        )
        .unwrap();

        assert_eq!(settings.icon, None);
        assert_eq!(state.library().zone_settings(&zone_id).unwrap().icon, None);
    }

    #[test]
    fn updating_playback_zone_icon_rejects_unsupported_values() {
        let state = app_state("zone-icon-invalid");
        let zone_id = state.zones().active_zone_id();

        let err = update_playback_zone_settings(
            &state,
            &zone_id,
            ZoneSettingsUpdate {
                icon: Some("turntable".to_string()),
                ..ZoneSettingsUpdate::default()
            },
        )
        .unwrap_err();

        assert!(matches!(err, PlaybackError::BadRequest(_)));
    }

    #[test]
    fn zone_settings_without_icon_deserializes_with_default() {
        let settings: ZoneSettings =
            serde_json::from_str(r#"{"airplay_default_volume":0.25}"#).unwrap();

        assert_eq!(settings.airplay_default_volume, Some(0.25));
        assert_eq!(settings.airplay_max_volume, None);
        assert!(!settings.qobuz_hires_enabled);
        assert_eq!(settings.icon, None);
    }

    #[test]
    fn updating_playback_zone_hegel_settings_persists_and_enriches_zone_profile() {
        let state = app_state("zone-hegel-settings");
        let zone_id = state.zones().active_zone_id();

        let settings = update_playback_zone_settings(
            &state,
            &zone_id,
            ZoneSettingsUpdate {
                device_type: Some("hegel".to_string()),
                hegel: Some(ZoneHegelSettings {
                    linked_airplay_zone_id: Some(" airplay-hegel ".to_string()),
                    host: Some(" 10.200.0.166 ".to_string()),
                    port: 0,
                    input: 1,
                    default_volume: 55,
                    max_volume: 50,
                    standby_usb_visible: true,
                    model: Some(" H390 ".to_string()),
                }),
                ..ZoneSettingsUpdate::default()
            },
        )
        .unwrap();

        assert_eq!(settings.device_type.as_deref(), Some("hegel"));
        let hegel = settings
            .hegel
            .as_ref()
            .expect("hegel settings should persist");
        assert_eq!(hegel.host.as_deref(), Some("10.200.0.166"));
        assert_eq!(
            hegel.linked_airplay_zone_id.as_deref(),
            Some("airplay-hegel")
        );
        assert_eq!(hegel.port, 50001);
        assert_eq!(hegel.default_volume, 50);
        assert_eq!(hegel.model.as_deref(), Some("h390"));

        let zone = enrich_zone_settings(&state, state.zones().list_zones())
            .into_iter()
            .find(|zone| zone.id == zone_id)
            .expect("zone should remain available");
        assert_eq!(zone.device_type.as_deref(), Some("hegel"));
        assert_eq!(
            zone.hegel.and_then(|settings| settings.host).as_deref(),
            Some("10.200.0.166")
        );
    }

    #[test]
    fn updating_playback_zone_hegel_settings_rejects_unsafe_targets() {
        let state = app_state("zone-hegel-settings-unsafe");
        let zone_id = state.zones().active_zone_id();

        let err = update_playback_zone_settings(
            &state,
            &zone_id,
            ZoneSettingsUpdate {
                device_type: Some("hegel".to_string()),
                hegel: Some(ZoneHegelSettings {
                    host: Some("127.0.0.1".to_string()),
                    port: 50001,
                    ..ZoneHegelSettings::default()
                }),
                ..ZoneSettingsUpdate::default()
            },
        )
        .unwrap_err();
        assert!(matches!(err, PlaybackError::BadRequest(_)));

        let err = update_playback_zone_settings(
            &state,
            &zone_id,
            ZoneSettingsUpdate {
                device_type: Some("hegel".to_string()),
                hegel: Some(ZoneHegelSettings {
                    host: Some("10.200.0.166".to_string()),
                    port: 22,
                    ..ZoneHegelSettings::default()
                }),
                ..ZoneSettingsUpdate::default()
            },
        )
        .unwrap_err();
        assert!(matches!(err, PlaybackError::BadRequest(_)));
    }

    #[test]
    fn updating_playback_zone_device_type_none_removes_hegel_settings() {
        let state = app_state("zone-hegel-settings-remove");
        let zone_id = state.zones().active_zone_id();

        update_playback_zone_settings(
            &state,
            &zone_id,
            ZoneSettingsUpdate {
                device_type: Some("hegel".to_string()),
                hegel: Some(ZoneHegelSettings {
                    host: Some("10.200.0.166".to_string()),
                    ..ZoneHegelSettings::default()
                }),
                ..ZoneSettingsUpdate::default()
            },
        )
        .unwrap();

        let settings = update_playback_zone_settings(
            &state,
            &zone_id,
            ZoneSettingsUpdate {
                device_type: Some("none".to_string()),
                ..ZoneSettingsUpdate::default()
            },
        )
        .unwrap();

        assert_eq!(settings.device_type, None);
        assert!(settings.hegel.is_none());
    }

    #[test]
    fn unavailable_saved_local_device_promotes_matching_agent_zone() {
        let state = app_state("remote-agent-promote-matching-device");
        let device_name = "Agent DAC Unique";
        let _ = state
            .settings()
            .update_playback_for_zone("local-core", |settings| {
                settings.device_name = Some(device_name.to_string());
            });

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        register_remote_agent_playback_zones(
            &state,
            "agent-1".to_string(),
            "Studio PC".to_string(),
            agent_capabilities(device_name),
            tx,
        );

        let selected_zone_id = state.zones().active_zone_id();
        let expected_zone_id = state
            .zones()
            .list_zones()
            .into_iter()
            .find(|zone| zone.device_name.as_deref() == Some(device_name))
            .expect("matching remote agent zone should be registered")
            .id;

        assert_eq!(selected_zone_id, expected_zone_id);
        assert_eq!(
            state.settings().snapshot().active_zone_id.as_deref(),
            Some(expected_zone_id.as_str())
        );
        assert!(matches!(
            rx.try_recv(),
            Ok(CoreToAgentCommand::SetPlaybackConfig { .. })
        ));
    }
}
