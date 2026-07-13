use super::registry::AgentEntry;
use crate::audio::dither::DitherPreference;
use crate::audio::output::device_caps::AudioDeviceCapabilities;
use crate::audio::player::Player;
use crate::audio::resampler::DEFAULT_FILTER_NAME;
use crate::audio::{airplay, device_caps, sonos, upnp};
use crate::protocol::{
    CapabilityDetectionSource, CapabilityDetectionStatus, DspProfile, OutputDeviceCapabilities,
    PlaybackConfig, SinkProtocol, ZoneCapabilities, system_audio_backend,
};
use crate::settings::DsdSourceRule;

pub fn current_playback_config(player: &Player, dsd_rules: Vec<DsdSourceRule>) -> PlaybackConfig {
    let snapshot = player.snapshot();
    let config = &snapshot.config;
    PlaybackConfig {
        filter_type: config
            .filter_type
            .map(|f| f.as_name().to_string())
            .unwrap_or_else(|| DEFAULT_FILTER_NAME.to_string()),
        target_rate: config.configured_target_rate,
        target_bit_depth: 24,
        upsampling_enabled: config.upsampling_enabled,
        exclusive: config.exclusive,
        dither_mode: DitherPreference::from_id(config.dither_mode_id)
            .map(|d| d.as_name().to_string())
            .unwrap_or_else(|| "Auto".to_string()),
        output_mode: config.output_mode.as_name().to_string(),
        dsd_modulator: config.dsd_modulator.as_name().to_string(),
        dsd_isi_penalty: config.dsd_isi_penalty,
        dsd_rules,
        headroom_db: config.headroom_db,
        dsp_buffer_ms: config.dsp_buffer_ms,
        volume: config.volume,
        eq: snapshot.eq_config,
        output_device: snapshot.device_name,
    }
}

pub(super) fn current_dsp_profile(player: &Player) -> DspProfile {
    let cfg = current_playback_config(player, Vec::new());
    DspProfile {
        upsampling_enabled: cfg.upsampling_enabled,
        filter_type: cfg.filter_type,
        target_rate: cfg.target_rate,
        dither_mode: cfg.dither_mode,
    }
}

pub(super) fn local_capabilities(
    device_name: Option<&str>,
    cached_capabilities: Option<AudioDeviceCapabilities>,
) -> ZoneCapabilities {
    let upnp_target = device_name.and_then(upnp::parse_target_device_name);
    let is_network_or_airplay = device_name.is_some_and(|name| {
        airplay::is_airplay_device_name(name)
            || sonos::is_sonos_device_name(name)
            || upnp::is_upnp_device_name(name)
    });
    let caps = if is_network_or_airplay {
        device_caps::output_device_capabilities(device_name)
    } else {
        let caps = cached_capabilities.unwrap_or_default();
        device_caps::apply_known_device_capability(device_name, caps)
    };
    let backend = local_backend(device_name);
    let supports_dsd128 = caps.supports_dsd128
        || (!is_network_or_airplay
            && device_caps::dop_dsd128_supported_for_backend(
                Some(backend),
                caps.max_sample_rate,
                caps.max_bit_depth,
            ));
    let supports_dsd256 = caps.supports_dsd256
        || (!is_network_or_airplay
            && device_caps::dop_dsd256_supported_for_backend(
                Some(backend),
                caps.max_sample_rate,
                caps.max_bit_depth,
            ));
    ZoneCapabilities {
        max_sample_rate: caps.max_sample_rate,
        max_bit_depth: caps.max_bit_depth,
        max_dsd_rate: caps.max_dsd_rate.max(device_caps::max_dsd_rate_from_flags(
            supports_dsd128,
            supports_dsd256,
        )),
        exclusive_supported: !device_name.is_some_and(|name| {
            airplay::is_airplay_device_name(name)
                || sonos::is_sonos_device_name(name)
                || upnp::is_upnp_device_name(name)
        }),
        gapless_supported: true,
        supports_dsd128,
        supports_dsd256,
        capability_detection_source: upnp_target
            .as_ref()
            .map(|target| target.capability_detection_source)
            .unwrap_or(CapabilityDetectionSource::Advertised),
        capability_detection_status: upnp_target
            .as_ref()
            .map(|target| target.capability_detection_status)
            .unwrap_or(CapabilityDetectionStatus::Complete),
        capability_detection_message: upnp_target
            .as_ref()
            .and_then(|target| target.capability_detection_message.clone()),
    }
}

pub(super) fn agent_capabilities_for_device(agent: &AgentEntry) -> ZoneCapabilities {
    let device_caps = agent_output_device_capabilities(agent);

    ZoneCapabilities {
        max_sample_rate: device_caps
            .map(|caps| caps.max_sample_rate)
            .unwrap_or(agent.capabilities.max_sample_rate),
        max_bit_depth: device_caps
            .map(|caps| caps.max_bit_depth)
            .unwrap_or(agent.capabilities.max_bit_depth),
        max_dsd_rate: device_caps
            .map(|caps| {
                device_caps::max_dsd_rate_from_flags(
                    output_caps_support_dsd128(caps),
                    output_caps_support_dsd256(caps),
                )
            })
            .unwrap_or_else(|| {
                device_caps::max_dsd_rate_from_flags(
                    agent.capabilities.supports_dsd128,
                    agent.capabilities.supports_dsd256,
                )
            }),
        exclusive_supported: agent.capabilities.exclusive_supported,
        gapless_supported: true,
        supports_dsd128: device_caps
            .map(output_caps_support_dsd128)
            .unwrap_or(agent.capabilities.supports_dsd128),
        supports_dsd256: device_caps
            .map(output_caps_support_dsd256)
            .unwrap_or(agent.capabilities.supports_dsd256),
        capability_detection_source: CapabilityDetectionSource::Advertised,
        capability_detection_status: CapabilityDetectionStatus::Complete,
        capability_detection_message: None,
    }
}

fn agent_output_device_capabilities(agent: &AgentEntry) -> Option<&OutputDeviceCapabilities> {
    agent.output_device.as_ref().and_then(|device| {
        agent
            .capabilities
            .output_device_capabilities
            .iter()
            .find(|caps| caps.name == *device)
    })
}

fn output_caps_support_dsd128(caps: &OutputDeviceCapabilities) -> bool {
    caps.supports_dsd128
        || caps.supports_dsd256
        || device_caps::dop_dsd128_supported_for_backend(
            caps.backend.as_deref(),
            caps.max_sample_rate,
            caps.max_bit_depth,
        )
}

fn output_caps_support_dsd256(caps: &OutputDeviceCapabilities) -> bool {
    caps.supports_dsd256
        || device_caps::dop_dsd256_supported_for_backend(
            caps.backend.as_deref(),
            caps.max_sample_rate,
            caps.max_bit_depth,
        )
}

pub(super) fn agent_backend_for_device(agent: &AgentEntry) -> Option<String> {
    agent_output_device_capabilities(agent)
        .and_then(|caps| caps.backend.clone())
        .or_else(|| {
            agent.output_device.as_deref().map(|device| {
                if device.starts_with("ASIO: ") {
                    "asio"
                } else {
                    system_audio_backend()
                }
                .to_string()
            })
        })
}

pub(super) fn local_protocol(device_name: Option<&str>) -> SinkProtocol {
    if device_name.is_some_and(|name| name.starts_with("ASIO: ")) {
        SinkProtocol::AsioOutput
    } else if device_name.is_some_and(sonos::is_sonos_device_name) {
        SinkProtocol::SonosUpnp
    } else if device_name.is_some_and(upnp::is_upnp_device_name) {
        SinkProtocol::UpnpAvRenderer
    } else if let Some(target) = device_name.and_then(airplay::parse_target_device_name) {
        if target.prefers_airplay2_transport() {
            SinkProtocol::AirPlay2
        } else {
            SinkProtocol::AirPlayRaop
        }
    } else if device_name.is_some_and(|name| name.to_ascii_lowercase().contains("airplay")) {
        SinkProtocol::AirPlayCoreAudio
    } else {
        SinkProtocol::LocalCoreAudio
    }
}

pub(super) fn local_backend(device_name: Option<&str>) -> &'static str {
    if device_name.is_some_and(|name| name.starts_with("ASIO: ")) {
        "asio"
    } else if device_name.is_some_and(sonos::is_sonos_device_name) {
        "sonos"
    } else if device_name.is_some_and(upnp::is_upnp_device_name) {
        "upnp"
    } else if device_name.is_some_and(airplay::is_airplay_device_name) {
        "airplay"
    } else {
        system_audio_backend()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output_caps(
        backend: Option<&str>,
        max_sample_rate: u32,
        max_bit_depth: u8,
    ) -> OutputDeviceCapabilities {
        OutputDeviceCapabilities {
            name: "DAC".to_string(),
            backend: backend.map(str::to_string),
            max_sample_rate,
            max_bit_depth,
            supports_dsd128: false,
            supports_dsd256: false,
        }
    }

    #[test]
    fn dop_carrier_rate_counts_as_dsd_support_for_agent_outputs() {
        let caps = output_caps(Some("coreaudio"), 705_600, 32);

        assert!(output_caps_support_dsd128(&caps));
        assert!(output_caps_support_dsd256(&caps));
    }

    #[test]
    fn non_dop_backends_do_not_gain_dsd_from_pcm_rate_alone() {
        let caps = output_caps(Some("alsa"), 705_600, 32);

        assert!(!output_caps_support_dsd128(&caps));
        assert!(!output_caps_support_dsd256(&caps));
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    #[test]
    fn local_hegel_h390_usb_snapshot_allows_dop_without_cached_advertised_dsd() {
        let caps = local_capabilities(Some("Hegel H390 USB"), None);

        assert_eq!(caps.max_sample_rate, 768_000);
        assert_eq!(caps.max_dsd_rate, Some(256));
        assert!(caps.supports_dsd128);
        assert!(caps.supports_dsd256);
    }
}
