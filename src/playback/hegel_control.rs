use crate::app::state::AppState;
use crate::diagnostics::logging::{error_kind, sanitize_error};
use crate::library::ZoneHegelSettings;
use crate::playback::error::PlaybackError;
use crate::playback::output_devices::output_device_available;
use crate::services::hegel;
use crate::settings::HegelSettings;
use std::future::Future;
use std::net::IpAddr;
use std::time::{Duration, Instant};
use tracing::{debug, warn};

const HEGEL_USB_WAKE_TIMEOUT: Duration = Duration::from_secs(15);
const HEGEL_USB_WAKE_POLL_INTERVAL: Duration = Duration::from_millis(250);
const HEGEL_READY_STATUS_MAX_AGE: Duration = Duration::from_secs(5);

pub(crate) fn update_hegel_settings(
    state: &AppState,
    settings: HegelSettings,
) -> Result<HegelSettings, PlaybackError> {
    let settings = normalize_hegel_settings(settings);
    if settings.enabled {
        let Some(host) = settings.host.as_deref() else {
            return Err(PlaybackError::bad_request("Hegel host is required"));
        };
        validate_hegel_target_policy(host, settings.port)?;
        let zone_id = settings.zone_id.as_deref().unwrap_or("").trim();
        if zone_id.is_empty() || state.zones().zone_protocol(zone_id).is_none() {
            return Err(PlaybackError::bad_request("Choose a valid Hegel zone"));
        }
    }
    state
        .settings()
        .update(|persisted| {
            persisted.hegel = settings.clone();
        })
        .map_err(PlaybackError::integration)?;
    Ok(settings)
}

pub(crate) fn validated_hegel_target(
    state: &AppState,
    host: &str,
    port: Option<u16>,
) -> Result<(String, u16, HegelSettings), PlaybackError> {
    let settings = normalize_hegel_settings(state.settings().hegel_settings());
    if !settings.enabled {
        return Err(PlaybackError::bad_request(
            "Save Hegel setup before sending commands",
        ));
    }
    let Some(configured_host) = settings.host.as_deref() else {
        return Err(PlaybackError::bad_request(
            "Save Hegel setup before sending commands",
        ));
    };
    validate_saved_hegel_target(configured_host, settings.port)?;
    let requested_host = host.trim();
    let requested_port = hegel::default_port(port);
    if !requested_host.eq_ignore_ascii_case(configured_host) || requested_port != settings.port {
        return Err(PlaybackError::forbidden(
            "Hegel target must match saved settings",
        ));
    }
    Ok((configured_host.to_string(), settings.port, settings))
}

pub(crate) fn validated_hegel_target_for_zone(
    state: &AppState,
    zone_id: &str,
    host: &str,
    port: Option<u16>,
) -> Result<(String, u16, HegelSettings), PlaybackError> {
    let settings = configured_hegel_settings_for_zone(state, zone_id).ok_or_else(|| {
        PlaybackError::bad_request("Save Hegel setup for this output before sending commands")
    })?;
    let Some(configured_host) = settings.host.as_deref() else {
        return Err(PlaybackError::bad_request(
            "Save Hegel setup for this output before sending commands",
        ));
    };
    validate_saved_hegel_target(configured_host, settings.port)?;
    let requested_host = host.trim();
    let requested_port = hegel::default_port(port);
    if !requested_host.eq_ignore_ascii_case(configured_host) || requested_port != settings.port {
        return Err(PlaybackError::forbidden(
            "Hegel target must match saved output settings",
        ));
    }
    Ok((configured_host.to_string(), settings.port, settings))
}

pub(crate) fn hegel_settings_for_zone(state: &AppState, zone_id: &str) -> Option<HegelSettings> {
    let settings = configured_hegel_settings_for_zone(state, zone_id)?;
    let host = settings.host.as_deref()?;
    validate_hegel_target_policy(host, settings.port).ok()?;
    Some(settings)
}

fn configured_hegel_settings_for_zone(state: &AppState, zone_id: &str) -> Option<HegelSettings> {
    if !cfg!(feature = "hegel") {
        return None;
    }
    if let Some(settings) = zone_hegel_settings_for_zone(state, zone_id) {
        return Some(settings);
    }
    let settings = normalize_hegel_settings(state.settings().hegel_settings());
    if !settings.enabled || settings.zone_id.as_deref() != Some(zone_id) {
        return None;
    }
    settings
        .host
        .as_deref()
        .is_some_and(|host| !host.trim().is_empty())
        .then_some(settings)
}

pub(crate) fn validate_hegel_target_policy(host: &str, port: u16) -> Result<(), PlaybackError> {
    const HEGEL_CONTROL_PORT: u16 = 50001;
    if port != HEGEL_CONTROL_PORT {
        return Err(PlaybackError::bad_request(
            "Hegel target must use port 50001",
        ));
    }
    let host = host.trim();
    if host.is_empty() {
        return Err(PlaybackError::bad_request("Hegel host is required"));
    }
    let ip = host
        .parse::<IpAddr>()
        .map_err(|_| PlaybackError::bad_request("Hegel host must be a private LAN IPv4 address"))?;
    match ip {
        IpAddr::V4(ip) if ip.is_private() => Ok(()),
        IpAddr::V4(_) | IpAddr::V6(_) => Err(PlaybackError::bad_request(
            "Hegel host must be a private LAN IPv4 address",
        )),
    }
}

fn validate_saved_hegel_target(host: &str, port: u16) -> Result<(), PlaybackError> {
    validate_hegel_target_policy(host, port).map_err(|_| {
        PlaybackError::bad_request("Saved Hegel target is not allowed; reconfigure Hegel setup")
    })
}

fn zone_hegel_settings_for_zone(state: &AppState, zone_id: &str) -> Option<HegelSettings> {
    let zone_settings = state.library().zone_settings(zone_id).ok()?;
    if zone_settings.device_type.as_deref() != Some("hegel") {
        return None;
    }
    let settings = hegel_settings_from_zone(zone_id, zone_settings.hegel?)?;
    settings
        .host
        .as_deref()
        .is_some_and(|host| !host.trim().is_empty())
        .then_some(settings)
}

fn hegel_settings_from_zone(zone_id: &str, settings: ZoneHegelSettings) -> Option<HegelSettings> {
    Some(normalize_hegel_settings(HegelSettings {
        enabled: true,
        zone_id: Some(zone_id.to_string()),
        linked_airplay_zone_id: settings.linked_airplay_zone_id,
        host: settings.host,
        port: settings.port,
        input: settings.input,
        default_volume: settings.default_volume,
        max_volume: settings.max_volume,
        standby_usb_visible: settings.standby_usb_visible,
    }))
}

pub(crate) fn normalize_hegel_settings(mut settings: HegelSettings) -> HegelSettings {
    settings.host = settings
        .host
        .map(|host| host.trim().to_string())
        .filter(|host| !host.is_empty());
    settings.zone_id = settings
        .zone_id
        .map(|zone| zone.trim().to_string())
        .filter(|zone| !zone.is_empty());
    settings.linked_airplay_zone_id = settings
        .linked_airplay_zone_id
        .map(|zone| zone.trim().to_string())
        .filter(|zone| !zone.is_empty());
    if settings.port == 0 {
        settings.port = 50001;
    }
    settings.input = settings.input.clamp(1, 20);
    settings.max_volume = settings.max_volume.min(100);
    settings.default_volume = settings.default_volume.min(settings.max_volume);
    settings
}

pub(crate) async fn query_hegel_status_for_target(
    state: &AppState,
    host: &str,
    port: Option<u16>,
) -> Result<hegel::HegelStatus, PlaybackError> {
    let (host, port, _) = validated_hegel_target(state, host, port)?;
    hegel::query_status(&host, port)
        .await
        .map(|status| cache_hegel_status(state, status))
        .map_err(hegel_gateway_error)
}

pub(crate) async fn query_hegel_status_for_zone_target(
    state: &AppState,
    zone_id: &str,
    host: &str,
    port: Option<u16>,
) -> Result<hegel::HegelStatus, PlaybackError> {
    let (host, port, _) = validated_hegel_target_for_zone(state, zone_id, host, port)?;
    hegel::query_status(&host, port)
        .await
        .map(|status| cache_hegel_status(state, status))
        .map_err(hegel_gateway_error)
}

pub(crate) async fn set_hegel_power_for_target(
    state: &AppState,
    host: &str,
    port: Option<u16>,
    on: bool,
) -> Result<hegel::HegelStatus, PlaybackError> {
    let (host, port, _) = validated_hegel_target(state, host, port)?;
    hegel::set_power(&host, port, on)
        .await
        .map(|status| cache_hegel_status(state, status))
        .map_err(hegel_gateway_error)
}

pub(crate) async fn set_hegel_input_for_target(
    state: &AppState,
    host: &str,
    port: Option<u16>,
    input: u8,
) -> Result<hegel::HegelStatus, PlaybackError> {
    let (host, port, _) = validated_hegel_target(state, host, port)?;
    hegel::set_input(&host, port, input)
        .await
        .map(|status| cache_hegel_status(state, status))
        .map_err(hegel_gateway_error)
}

pub(crate) async fn set_hegel_volume_for_target(
    state: &AppState,
    host: &str,
    port: Option<u16>,
    volume: Option<u8>,
    direction: Option<&str>,
) -> Result<hegel::HegelStatus, PlaybackError> {
    let (host, port, settings) = validated_hegel_target(state, host, port)?;
    let result = match direction {
        Some("up") | Some("down") => match hegel::query_status(&host, port).await {
            Ok(status) => {
                let current = status.volume.unwrap_or(settings.default_volume);
                let next = if direction == Some("up") {
                    current.saturating_add(1).min(settings.max_volume)
                } else {
                    current.saturating_sub(1)
                };
                hegel::set_volume(&host, port, next).await
            }
            Err(e) => Err(e),
        },
        Some(_) => {
            return Err(PlaybackError::bad_request(
                "Volume direction must be up or down",
            ));
        }
        None => {
            let Some(volume) = volume else {
                return Err(PlaybackError::bad_request("Volume is required"));
            };
            hegel::set_volume(&host, port, volume.min(settings.max_volume)).await
        }
    };
    result
        .map(|status| cache_hegel_status(state, status))
        .map_err(hegel_gateway_error)
}

pub(crate) async fn set_hegel_mute_for_target(
    state: &AppState,
    host: &str,
    port: Option<u16>,
    muted: bool,
) -> Result<hegel::HegelStatus, PlaybackError> {
    let (host, port, _) = validated_hegel_target(state, host, port)?;
    hegel::set_mute(&host, port, muted)
        .await
        .map(|status| cache_hegel_status(state, status))
        .map_err(hegel_gateway_error)
}

fn cache_hegel_status(state: &AppState, status: hegel::HegelStatus) -> hegel::HegelStatus {
    state.hegel_status().remember(status)
}

fn hegel_gateway_error(e: String) -> PlaybackError {
    PlaybackError::retryable_network(e)
}

pub(crate) fn should_apply_hegel_default_volume(
    previous_power: Option<bool>,
    previous_input: Option<u8>,
    desired_input: u8,
) -> bool {
    previous_power == Some(false) || previous_input.is_some_and(|input| input != desired_input)
}

#[derive(Debug)]
enum InitialHegelReadiness {
    Ready,
    NeedsPreparation(Option<hegel::HegelStatus>),
    QueryFailed(String),
}

async fn initial_hegel_readiness<F, Fut>(
    cache: &hegel::HegelStatusCache,
    desired_input: u8,
    output_ready: bool,
    query_status: F,
) -> InitialHegelReadiness
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<hegel::HegelStatus, String>>,
{
    if let Some(status) = cache
        .fresh_direct_power_input(HEGEL_READY_STATUS_MAX_AGE)
        .filter(|status| hegel_status_directly_ready_for_playback(status, desired_input))
    {
        return if output_ready {
            InitialHegelReadiness::Ready
        } else {
            // The amplifier observation is still useful while CoreAudio waits
            // for USB enumeration; do not spend an extra network round trip.
            InitialHegelReadiness::NeedsPreparation(Some(status))
        };
    }

    match query_status().await {
        Ok(status) => {
            let directly_ready = hegel_status_directly_ready_for_playback(&status, desired_input);
            cache.remember(status.clone());
            if output_ready && directly_ready {
                InitialHegelReadiness::Ready
            } else {
                InitialHegelReadiness::NeedsPreparation(Some(status))
            }
        }
        Err(error) => InitialHegelReadiness::QueryFailed(error),
    }
}

fn bound_hegel_output_device(state: &AppState, zone_id: &str) -> Option<String> {
    state
        .zones()
        .zone_bound_device_name(zone_id)
        .filter(|device| !device.trim().is_empty())
}

fn bound_hegel_output_available(device_name: Option<&str>) -> bool {
    device_name.map(output_device_available).unwrap_or(true)
}

#[derive(Debug, Default)]
struct HegelReadinessProgress {
    readiness_command_issued: bool,
}

impl HegelReadinessProgress {
    fn power_command_required(status: Option<&hegel::HegelStatus>) -> bool {
        !status.is_some_and(|status| {
            status.power == Some(true) && status.raw.iter().any(|line| line == "-p.1")
        })
    }

    fn input_command_required(status: Option<&hegel::HegelStatus>, desired_input: u8) -> bool {
        let expected_input = format!("-i.{desired_input}");
        !status.is_some_and(|status| {
            status.input == Some(desired_input)
                && status.raw.iter().any(|line| line == &expected_input)
        })
    }

    fn record_readiness_command(&mut self) {
        self.readiness_command_issued = true;
    }

    fn requires_confirmation_poll(&self, output_ready: bool) -> bool {
        self.readiness_command_issued || !output_ready
    }
}

fn select_bound_hegel_output(state: &AppState, zone_id: &str, device_name: Option<&str>) {
    if let (Some(player), Some(device_name)) = (state.zones().player_for_zone(zone_id), device_name)
    {
        player.select_device(Some(device_name.to_string()));
    }
}

pub(crate) async fn prepare_hegel_for_zone(
    state: &AppState,
    zone_id: &str,
) -> Result<(), PlaybackError> {
    if !cfg!(feature = "hegel") {
        return Ok(());
    }
    let Some(settings) = hegel_settings_for_zone(state, zone_id) else {
        return Ok(());
    };
    let host = settings.host.as_deref().unwrap_or_default();
    let default_volume = settings.default_volume.min(settings.max_volume);
    let device_name = bound_hegel_output_device(state, zone_id);
    let mut status = match initial_hegel_readiness(
        state.hegel_status(),
        settings.input,
        bound_hegel_output_available(device_name.as_deref()),
        || hegel::query_status(host, settings.port),
    )
    .await
    {
        InitialHegelReadiness::Ready => {
            select_bound_hegel_output(state, zone_id, device_name.as_deref());
            return Ok(());
        }
        InitialHegelReadiness::NeedsPreparation(status) => status,
        InitialHegelReadiness::QueryFailed(e) => {
            warn!(
                event = "external_service_failure",
                service = "hegel",
                zone_id,
                error_kind = error_kind(&e),
                error = %sanitize_error(&e),
                "Hegel status query failed"
            );
            None
        }
    };
    let mut readiness_progress = HegelReadinessProgress::default();
    let needs_power_command = HegelReadinessProgress::power_command_required(status.as_ref());
    let mut force_output_reopen = needs_power_command;
    let mut set_default_volume = needs_power_command
        || should_apply_hegel_default_volume(
            status.as_ref().and_then(|s| s.power),
            None,
            settings.input,
        );

    if needs_power_command {
        match hegel::set_power(host, settings.port, true).await {
            Ok(next_status) => {
                readiness_progress.record_readiness_command();
                state.hegel_status().remember(next_status.clone());
                status = Some(next_status);
            }
            Err(e) => {
                warn!(
                    event = "external_service_failure",
                    service = "hegel",
                    zone_id,
                    error_kind = error_kind(&e),
                    error = %sanitize_error(&e),
                    "Hegel power command failed"
                );
                return Err(hegel_gateway_error(e));
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }

    let input_before_select = status.as_ref().and_then(|s| s.input);
    set_default_volume |=
        should_apply_hegel_default_volume(Some(true), input_before_select, settings.input);

    let mut input_confirmed = input_before_select == Some(settings.input);
    if HegelReadinessProgress::input_command_required(status.as_ref(), settings.input) {
        force_output_reopen = true;
        match hegel::set_input(host, settings.port, settings.input).await {
            Ok(next_status) => {
                readiness_progress.record_readiness_command();
                input_confirmed = hegel_input_confirmed(&next_status, settings.input);
                state.hegel_status().remember(next_status);
            }
            Err(e) => {
                warn!(
                    event = "external_service_failure",
                    service = "hegel",
                    zone_id,
                    error_kind = error_kind(&e),
                    error = %sanitize_error(&e),
                    "Hegel input command failed"
                );
                return Err(hegel_gateway_error(e));
            }
        }
    }
    if !input_confirmed && crate::audio::debug::audio_debug_enabled() {
        debug!(
            event = "external_service_warning",
            service = "hegel",
            zone_id,
            input = settings.input,
            "Hegel input was not confirmed by command response"
        );
    }
    if readiness_progress
        .requires_confirmation_poll(bound_hegel_output_available(device_name.as_deref()))
    {
        wait_for_hegel_playback_ready(state, zone_id, &settings).await?;
    } else {
        select_bound_hegel_output(state, zone_id, device_name.as_deref());
    }

    if set_default_volume {
        match hegel::set_volume(host, settings.port, default_volume).await {
            Ok(next_status) => {
                state.hegel_status().remember(next_status);
            }
            Err(e) => {
                warn!(
                    event = "external_service_failure",
                    service = "hegel",
                    zone_id,
                    error_kind = error_kind(&e),
                    error = %sanitize_error(&e),
                    "Hegel default volume command failed"
                );
                return Err(hegel_gateway_error(e));
            }
        }
    }
    if force_output_reopen && let Some(player) = state.zones().player_for_zone(zone_id) {
        player.reopen_output();
    }
    Ok(())
}

async fn wait_for_hegel_playback_ready(
    state: &AppState,
    zone_id: &str,
    settings: &HegelSettings,
) -> Result<(), PlaybackError> {
    let device_name = bound_hegel_output_device(state, zone_id);
    let host = settings.host.as_deref().unwrap_or_default();
    let deadline = Instant::now() + HEGEL_USB_WAKE_TIMEOUT;
    let mut last_status_error = None;
    let mut fresh_ready_polls = 0_u8;
    loop {
        let usb_ready = device_name
            .as_deref()
            .map(output_device_available)
            .unwrap_or(true);
        let hegel_ready = match hegel::query_status(host, settings.port).await {
            Ok(status) => {
                let ready = hegel_status_directly_ready_for_playback(&status, settings.input);
                state.hegel_status().remember(status);
                ready
            }
            Err(err) => {
                last_status_error = Some(err);
                false
            }
        };
        if usb_ready && hegel_ready {
            fresh_ready_polls = fresh_ready_polls.saturating_add(1);
        } else {
            fresh_ready_polls = 0;
        }
        if fresh_ready_polls >= 2 {
            select_bound_hegel_output(state, zone_id, device_name.as_deref());
            return Ok(());
        }
        if Instant::now() >= deadline {
            let detail = last_status_error
                .map(|err| format!(" Last Hegel status error: {err}"))
                .unwrap_or_default();
            return Err(PlaybackError::integration(format!(
                "Hegel was not ready for playback before the timeout.{detail}"
            )));
        }
        tokio::time::sleep(HEGEL_USB_WAKE_POLL_INTERVAL).await;
    }
}

fn hegel_status_ready_for_playback(status: &hegel::HegelStatus, desired_input: u8) -> bool {
    status.power == Some(true) && status.input == Some(desired_input)
}

fn hegel_status_directly_ready_for_playback(
    status: &hegel::HegelStatus,
    desired_input: u8,
) -> bool {
    hegel_status_ready_for_playback(status, desired_input)
        && status.raw.iter().any(|line| line == "-p.1")
        && status
            .raw
            .iter()
            .any(|line| line == &format!("-i.{desired_input}"))
}

fn hegel_input_confirmed(status: &hegel::HegelStatus, desired_input: u8) -> bool {
    status.input == Some(desired_input)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "hegel")]
    use crate::library::{ZoneHegelSettings, ZoneSettings};
    use crate::playback::test_support::app_state;
    use std::cell::Cell;

    fn directly_ready_status(input: u8) -> hegel::HegelStatus {
        hegel::HegelStatus {
            power: Some(true),
            input: Some(input),
            raw: vec!["-p.1".to_string(), format!("-i.{input}")],
            ..hegel::HegelStatus::default()
        }
    }

    #[test]
    fn hegel_default_volume_only_on_power_or_input_transition() {
        assert!(should_apply_hegel_default_volume(Some(false), Some(9), 9));
        assert!(should_apply_hegel_default_volume(Some(true), Some(3), 9));
        assert!(!should_apply_hegel_default_volume(Some(true), Some(9), 9));
        assert!(!should_apply_hegel_default_volume(Some(true), None, 9));
    }

    #[test]
    fn hegel_ready_requires_power_and_matching_input() {
        let mut status = hegel::HegelStatus {
            power: Some(true),
            input: Some(9),
            ..hegel::HegelStatus::default()
        };

        assert!(hegel_status_ready_for_playback(&status, 9));
        assert!(!hegel_status_ready_for_playback(&status, 3));
        status.input = None;
        assert!(!hegel_status_ready_for_playback(&status, 9));
        status.power = Some(false);
        assert!(!hegel_status_ready_for_playback(&status, 9));
    }

    #[test]
    fn hegel_direct_readiness_requires_current_raw_power_and_input() {
        let mut status = directly_ready_status(9);
        assert!(hegel_status_directly_ready_for_playback(&status, 9));

        status.raw = vec!["-p.1".to_string()];
        assert!(!hegel_status_directly_ready_for_playback(&status, 9));
        status.raw = vec!["-p.1".to_string(), "-i.3".to_string()];
        assert!(!hegel_status_directly_ready_for_playback(&status, 9));
    }

    #[tokio::test]
    async fn fresh_direct_ready_status_skips_the_live_query() {
        let cache = hegel::HegelStatusCache::default();
        cache.remember(directly_ready_status(9));
        let query_count = Cell::new(0_u8);

        let readiness = initial_hegel_readiness(&cache, 9, true, || {
            query_count.set(query_count.get() + 1);
            std::future::ready(Err("unexpected query".to_string()))
        })
        .await;

        assert!(matches!(readiness, InitialHegelReadiness::Ready));
        assert_eq!(query_count.get(), 0);
    }

    #[tokio::test]
    async fn stale_status_performs_one_live_query_and_returns_ready() {
        let cache = hegel::HegelStatusCache::default();
        let query_count = Cell::new(0_u8);

        let readiness = initial_hegel_readiness(&cache, 9, true, || {
            query_count.set(query_count.get() + 1);
            std::future::ready(Ok(directly_ready_status(9)))
        })
        .await;

        assert!(matches!(readiness, InitialHegelReadiness::Ready));
        assert_eq!(query_count.get(), 1);
        assert!(
            cache
                .fresh_direct_power_input(HEGEL_READY_STATUS_MAX_AGE)
                .is_some()
        );
    }

    #[tokio::test]
    async fn fresh_ready_status_waits_for_missing_output_without_requerying() {
        let cache = hegel::HegelStatusCache::default();
        cache.remember(directly_ready_status(9));
        let query_count = Cell::new(0_u8);

        let readiness = initial_hegel_readiness(&cache, 9, false, || {
            query_count.set(query_count.get() + 1);
            std::future::ready(Err("unexpected query".to_string()))
        })
        .await;

        assert!(matches!(
            readiness,
            InitialHegelReadiness::NeedsPreparation(Some(_))
        ));
        assert_eq!(query_count.get(), 0);
    }

    #[test]
    fn power_and_input_transitions_require_post_command_confirmation() {
        let ready = directly_ready_status(9);
        let untouched = HegelReadinessProgress::default();
        assert!(!HegelReadinessProgress::power_command_required(Some(
            &ready
        )));
        assert!(!HegelReadinessProgress::input_command_required(
            Some(&ready),
            9
        ));
        assert!(!untouched.requires_confirmation_poll(true));

        let mut standby = directly_ready_status(9);
        standby.power = Some(false);
        standby.raw[0] = "-p.0".to_string();
        let mut power_transition = HegelReadinessProgress::default();
        assert!(HegelReadinessProgress::power_command_required(Some(
            &standby
        )));
        power_transition.record_readiness_command();
        assert!(power_transition.requires_confirmation_poll(true));

        let wrong_input = directly_ready_status(3);
        let mut input_transition = HegelReadinessProgress::default();
        assert!(!HegelReadinessProgress::power_command_required(Some(
            &wrong_input
        )));
        assert!(HegelReadinessProgress::input_command_required(
            Some(&wrong_input),
            9
        ));
        input_transition.record_readiness_command();
        assert!(input_transition.requires_confirmation_poll(true));

        let merged_only = hegel::HegelStatus {
            power: Some(true),
            input: Some(9),
            raw: vec!["-v.35".to_string()],
            ..hegel::HegelStatus::default()
        };
        assert!(HegelReadinessProgress::power_command_required(Some(
            &merged_only
        )));
        assert!(HegelReadinessProgress::input_command_required(
            Some(&merged_only),
            9
        ));

        assert!(untouched.requires_confirmation_poll(false));
    }

    #[test]
    fn hegel_input_confirmation_requires_known_matching_input() {
        let mut status = hegel::HegelStatus {
            input: Some(9),
            ..hegel::HegelStatus::default()
        };

        assert!(hegel_input_confirmed(&status, 9));
        assert!(!hegel_input_confirmed(&status, 3));
        status.input = None;
        assert!(!hegel_input_confirmed(&status, 9));
    }

    #[test]
    fn hegel_target_must_match_saved_settings() {
        let state = app_state("hegel-target");
        let _ = state.settings().update(|persisted| {
            persisted.hegel = HegelSettings {
                enabled: true,
                zone_id: Some(state.zones().active_zone_id()),
                linked_airplay_zone_id: None,
                host: Some("192.168.1.50".to_string()),
                port: 50001,
                input: 9,
                default_volume: 20,
                max_volume: 50,
                standby_usb_visible: false,
            };
        });

        assert!(validated_hegel_target(&state, "192.168.1.50", Some(50001)).is_ok());
        assert!(matches!(
            validated_hegel_target(&state, "127.0.0.1", Some(50001)).unwrap_err(),
            PlaybackError::Forbidden(_)
        ));
        assert!(matches!(
            validated_hegel_target(&state, "192.168.1.50", Some(22)).unwrap_err(),
            PlaybackError::Forbidden(_)
        ));
    }

    #[test]
    fn hegel_target_policy_rejects_probe_targets() {
        for host in [
            "127.0.0.1",
            "::1",
            "0.0.0.0",
            "169.254.1.20",
            "224.0.0.1",
            "8.8.8.8",
            "hegel.local",
        ] {
            assert!(
                validate_hegel_target_policy(host, 50001).is_err(),
                "{host} should be rejected"
            );
        }

        assert!(validate_hegel_target_policy("192.168.1.50", 50001).is_ok());
        assert!(validate_hegel_target_policy("10.0.0.9", 50001).is_ok());
        assert!(validate_hegel_target_policy("172.16.0.9", 50001).is_ok());
        assert!(validate_hegel_target_policy("192.168.1.50", 22).is_err());
    }

    #[test]
    fn update_hegel_settings_rejects_unsafe_targets() {
        let state = app_state("hegel-global-unsafe-target");
        let zone_id = state.zones().active_zone_id();

        let err = update_hegel_settings(
            &state,
            HegelSettings {
                enabled: true,
                zone_id: Some(zone_id.clone()),
                host: Some("127.0.0.1".to_string()),
                port: 50001,
                input: 9,
                default_volume: 20,
                max_volume: 50,
                linked_airplay_zone_id: None,
                standby_usb_visible: false,
            },
        )
        .unwrap_err();
        assert!(matches!(err, PlaybackError::BadRequest(_)));

        let err = update_hegel_settings(
            &state,
            HegelSettings {
                enabled: true,
                zone_id: Some(zone_id),
                host: Some("192.168.1.50".to_string()),
                port: 22,
                input: 9,
                default_volume: 20,
                max_volume: 50,
                linked_airplay_zone_id: None,
                standby_usb_visible: false,
            },
        )
        .unwrap_err();
        assert!(matches!(err, PlaybackError::BadRequest(_)));
    }

    #[cfg(feature = "hegel")]
    #[test]
    fn hegel_settings_for_zone_prefers_zone_assignment_over_global_settings() {
        let state = app_state("hegel-zone-precedence");
        let zone_id = state.zones().active_zone_id();
        let _ = state.settings().update(|persisted| {
            persisted.hegel = HegelSettings {
                enabled: true,
                zone_id: Some(zone_id.clone()),
                linked_airplay_zone_id: None,
                host: Some("192.168.1.50".to_string()),
                port: 50001,
                input: 9,
                default_volume: 20,
                max_volume: 50,
                standby_usb_visible: false,
            };
        });
        let zone = state
            .zones()
            .list_zones()
            .into_iter()
            .find(|zone| zone.id == zone_id)
            .expect("active zone should exist");
        state
            .library()
            .upsert_zone_definition(
                &zone.id,
                &zone.name,
                "local_coreaudio",
                zone.device_name.as_deref(),
                zone.enabled,
            )
            .unwrap();
        state
            .library()
            .set_zone_settings(
                &zone_id,
                ZoneSettings {
                    device_type: Some("hegel".to_string()),
                    hegel: Some(ZoneHegelSettings {
                        host: Some("10.200.0.166".to_string()),
                        port: 50001,
                        input: 1,
                        default_volume: 18,
                        max_volume: 42,
                        standby_usb_visible: true,
                        ..ZoneHegelSettings::default()
                    }),
                    ..ZoneSettings::default()
                },
            )
            .unwrap();

        let settings = hegel_settings_for_zone(&state, &zone_id).unwrap();

        assert_eq!(settings.host.as_deref(), Some("10.200.0.166"));
        assert_eq!(settings.port, 50001);
        assert_eq!(settings.input, 1);
        assert_eq!(settings.default_volume, 18);
        assert_eq!(settings.max_volume, 42);
        assert!(settings.standby_usb_visible);
    }

    #[cfg(feature = "hegel")]
    #[test]
    fn hegel_settings_for_zone_ignores_legacy_unsafe_targets() {
        let state = app_state("hegel-zone-legacy-unsafe");
        let zone_id = state.zones().active_zone_id();
        let zone = state
            .zones()
            .list_zones()
            .into_iter()
            .find(|zone| zone.id == zone_id)
            .expect("active zone should exist");
        state
            .library()
            .upsert_zone_definition(
                &zone.id,
                &zone.name,
                "local_coreaudio",
                zone.device_name.as_deref(),
                zone.enabled,
            )
            .unwrap();
        state
            .library()
            .set_zone_settings(
                &zone_id,
                ZoneSettings {
                    device_type: Some("hegel".to_string()),
                    hegel: Some(ZoneHegelSettings {
                        host: Some("127.0.0.1".to_string()),
                        port: 50001,
                        ..ZoneHegelSettings::default()
                    }),
                    ..ZoneSettings::default()
                },
            )
            .unwrap();

        assert!(hegel_settings_for_zone(&state, &zone_id).is_none());
    }

    #[cfg(feature = "hegel")]
    #[tokio::test]
    async fn zone_status_rejects_legacy_loopback_without_connecting() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let state = app_state("hegel-zone-loopback-no-connect");
        let zone_id = state.zones().active_zone_id();
        let zone = state
            .zones()
            .list_zones()
            .into_iter()
            .find(|zone| zone.id == zone_id)
            .expect("active zone should exist");
        state
            .library()
            .upsert_zone_definition(
                &zone.id,
                &zone.name,
                "local_coreaudio",
                zone.device_name.as_deref(),
                zone.enabled,
            )
            .unwrap();
        state
            .library()
            .set_zone_settings(
                &zone_id,
                ZoneSettings {
                    device_type: Some("hegel".to_string()),
                    hegel: Some(ZoneHegelSettings {
                        host: Some("127.0.0.1".to_string()),
                        port,
                        ..ZoneHegelSettings::default()
                    }),
                    ..ZoneSettings::default()
                },
            )
            .unwrap();

        let err = query_hegel_status_for_zone_target(&state, &zone_id, "127.0.0.1", Some(port))
            .await
            .unwrap_err();
        assert!(matches!(err, PlaybackError::BadRequest(_)));
        assert!(
            tokio::time::timeout(Duration::from_millis(150), listener.accept())
                .await
                .is_err()
        );
    }
}
