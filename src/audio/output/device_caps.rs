use cpal::traits::{DeviceTrait, HostTrait};

pub const DEFAULT_MAX_SAMPLE_RATE: u32 = 384_000;
pub const DEFAULT_MAX_BIT_DEPTH: u8 = 32;
pub const HEGEL_H390_USB_DOP_MAX_SAMPLE_RATE: u32 = 768_000;
pub const HEGEL_H390_USB_DOP_MAX_BIT_DEPTH: u8 = 32;

const DOP_DSD128_MIN_SAMPLE_RATE: u32 = 352_800;
const DOP_DSD256_MIN_SAMPLE_RATE: u32 = 705_600;
const RATES_44_FAMILY: [u32; 4] = [44_100, 88_200, 176_400, 352_800];
const RATES_48_FAMILY: [u32; 4] = [48_000, 96_000, 192_000, 384_000];
const STANDARD_TARGET_RATES: [u32; 8] = [
    44_100, 48_000, 88_200, 96_000, 176_400, 192_000, 352_800, 384_000,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioDeviceCapabilities {
    pub max_sample_rate: u32,
    pub max_bit_depth: u8,
    pub max_dsd_rate: Option<u16>,
    pub supports_dsd128: bool,
    pub supports_dsd256: bool,
}

impl Default for AudioDeviceCapabilities {
    fn default() -> Self {
        Self {
            max_sample_rate: DEFAULT_MAX_SAMPLE_RATE,
            max_bit_depth: DEFAULT_MAX_BIT_DEPTH,
            max_dsd_rate: None,
            supports_dsd128: false,
            supports_dsd256: false,
        }
    }
}

pub fn output_device_capabilities(name: Option<&str>) -> AudioDeviceCapabilities {
    let trimmed = name.map(str::trim).filter(|name| !name.is_empty());

    if trimmed.is_some_and(crate::audio::sinks::airplay::is_airplay_device_name) {
        return AudioDeviceCapabilities {
            max_sample_rate: crate::audio::sinks::airplay::AIRPLAY_SAMPLE_RATE,
            max_bit_depth: crate::audio::sinks::airplay::AIRPLAY_BIT_DEPTH,
            max_dsd_rate: None,
            supports_dsd128: false,
            supports_dsd256: false,
        };
    }
    if trimmed.is_some_and(crate::audio::sinks::sonos::is_sonos_device_name) {
        return AudioDeviceCapabilities {
            max_sample_rate: crate::audio::sinks::sonos::SONOS_SAMPLE_RATE,
            max_bit_depth: crate::audio::sinks::sonos::SONOS_BIT_DEPTH,
            max_dsd_rate: None,
            supports_dsd128: false,
            supports_dsd256: false,
        };
    }
    if let Some(target) = trimmed.and_then(crate::audio::sinks::upnp::parse_target_device_name) {
        return AudioDeviceCapabilities {
            max_sample_rate: target.max_sample_rate,
            max_bit_depth: target.max_bit_depth,
            max_dsd_rate: target.max_dsd_rate,
            supports_dsd128: target.max_dsd_rate.is_some_and(|rate| rate >= 128),
            supports_dsd256: target.max_dsd_rate.is_some_and(|rate| rate >= 256),
        };
    }

    #[cfg(all(target_os = "windows", feature = "asio"))]
    if let Some(_driver_name) = trimmed.and_then(|name| name.strip_prefix("ASIO: ")) {
        return AudioDeviceCapabilities {
            max_sample_rate: DEFAULT_MAX_SAMPLE_RATE,
            max_bit_depth: DEFAULT_MAX_BIT_DEPTH,
            max_dsd_rate: None,
            supports_dsd128: false,
            supports_dsd256: false,
        };
    }

    let caps = cpal_output_device_capabilities(trimmed).unwrap_or_default();
    apply_known_device_capability(trimmed, caps)
}

pub fn capabilities_for_cpal_device(device: &cpal::Device) -> Option<AudioDeviceCapabilities> {
    let mut max_sample_rate = 0;
    let mut max_bit_depth = 0;

    if let Ok(configs) = device.supported_output_configs() {
        for config in configs {
            max_sample_rate = max_sample_rate.max(config.max_sample_rate().0);
            max_bit_depth = max_bit_depth.max(sample_format_bit_depth(config.sample_format()));
        }
    }

    if max_sample_rate == 0
        && let Ok(config) = device.default_output_config()
    {
        max_sample_rate = config.sample_rate().0;
        max_bit_depth = sample_format_bit_depth(config.sample_format());
    }

    if max_sample_rate == 0 {
        None
    } else {
        let probed_max_bit_depth = max_bit_depth;
        let max_bit_depth = max_bit_depth.max(DEFAULT_MAX_BIT_DEPTH);
        let supports_dsd128 = cpal_dop_dsd128_supported(max_sample_rate, probed_max_bit_depth);
        let supports_dsd256 = cpal_dop_dsd256_supported(max_sample_rate, probed_max_bit_depth);
        Some(AudioDeviceCapabilities {
            max_sample_rate,
            max_bit_depth,
            max_dsd_rate: max_dsd_rate_from_flags(supports_dsd128, supports_dsd256),
            supports_dsd128,
            supports_dsd256,
        })
    }
}

pub fn apply_known_device_capability(
    name: Option<&str>,
    caps: AudioDeviceCapabilities,
) -> AudioDeviceCapabilities {
    let Some(name) = name else {
        return caps;
    };
    if !is_hegel_h390_usb_device_name(name) {
        return caps;
    }
    let max_sample_rate = caps.max_sample_rate.max(HEGEL_H390_USB_DOP_MAX_SAMPLE_RATE);
    let max_bit_depth = caps.max_bit_depth.max(HEGEL_H390_USB_DOP_MAX_BIT_DEPTH);
    let supports_dsd128 = caps.supports_dsd128
        || cpal_dop_dsd128_supported(max_sample_rate, HEGEL_H390_USB_DOP_MAX_BIT_DEPTH);
    let supports_dsd256 = caps.supports_dsd256
        || cpal_dop_dsd256_supported(max_sample_rate, HEGEL_H390_USB_DOP_MAX_BIT_DEPTH);
    AudioDeviceCapabilities {
        max_sample_rate,
        max_bit_depth,
        max_dsd_rate: max_dsd_rate_from_flags(supports_dsd128, supports_dsd256),
        supports_dsd128,
        supports_dsd256,
    }
}

fn is_hegel_h390_usb_device_name(name: &str) -> bool {
    let normalized = name.trim().to_ascii_lowercase();
    normalized.contains("hegel") && normalized.contains("h390") && normalized.contains("usb")
}

pub fn max_dsd_rate_from_flags(supports_dsd128: bool, supports_dsd256: bool) -> Option<u16> {
    if supports_dsd256 {
        Some(256)
    } else if supports_dsd128 {
        Some(128)
    } else {
        None
    }
}

pub fn dop_dsd128_supported_for_backend(
    backend: Option<&str>,
    max_sample_rate: u32,
    max_bit_depth: u8,
) -> bool {
    backend.is_some_and(backend_supports_dop)
        && max_bit_depth >= 24
        && max_sample_rate >= DOP_DSD128_MIN_SAMPLE_RATE
}

pub fn dop_dsd256_supported_for_backend(
    backend: Option<&str>,
    max_sample_rate: u32,
    max_bit_depth: u8,
) -> bool {
    backend.is_some_and(backend_supports_dop)
        && max_bit_depth >= 24
        && max_sample_rate >= DOP_DSD256_MIN_SAMPLE_RATE
}

fn cpal_dop_dsd128_supported(max_sample_rate: u32, max_bit_depth: u8) -> bool {
    cpal_backend_supports_dop()
        && max_bit_depth >= 24
        && max_sample_rate >= DOP_DSD128_MIN_SAMPLE_RATE
}

fn cpal_dop_dsd256_supported(max_sample_rate: u32, max_bit_depth: u8) -> bool {
    cpal_backend_supports_dop()
        && max_bit_depth >= 24
        && max_sample_rate >= DOP_DSD256_MIN_SAMPLE_RATE
}

fn cpal_backend_supports_dop() -> bool {
    cfg!(any(target_os = "macos", target_os = "windows"))
}

fn backend_supports_dop(backend: &str) -> bool {
    matches!(
        backend.trim().to_ascii_lowercase().as_str(),
        "coreaudio" | "wasapi"
    )
}

pub fn auto_target_rate(source_rate: u32, device_max_sample_rate: u32) -> u32 {
    let max_rate = if device_max_sample_rate == 0 {
        DEFAULT_MAX_SAMPLE_RATE
    } else {
        device_max_sample_rate.min(DEFAULT_MAX_SAMPLE_RATE)
    };

    if source_rate == 0 {
        return best_rate_from(&RATES_48_FAMILY, max_rate)
            .or_else(|| best_standard_rate(max_rate))
            .unwrap_or(DEFAULT_MAX_SAMPLE_RATE.min(max_rate));
    }

    let preferred_family = if in_rate_family(source_rate, 44_100) {
        Some(&RATES_44_FAMILY[..])
    } else if in_rate_family(source_rate, 48_000) {
        Some(&RATES_48_FAMILY[..])
    } else {
        None
    };

    if let Some(rate) = preferred_family.and_then(|family| best_rate_from(family, max_rate)) {
        return rate;
    }

    best_standard_rate(max_rate)
        .or_else(|| (source_rate <= max_rate).then_some(source_rate))
        .unwrap_or(max_rate)
}

pub fn auto_target_rate_for_device(source_rate: u32, device_name: Option<&str>) -> u32 {
    auto_target_rate(
        source_rate,
        output_device_capabilities(device_name).max_sample_rate,
    )
}

fn cpal_output_device_capabilities(name: Option<&str>) -> Option<AudioDeviceCapabilities> {
    let host = cpal::default_host();
    let device = match name {
        Some(requested_name) => host.output_devices().ok()?.find(|device| {
            device
                .name()
                .ok()
                .is_some_and(|name| name.trim() == requested_name)
        })?,
        None => host.default_output_device()?,
    };

    capabilities_for_cpal_device(&device)
}

fn sample_format_bit_depth(format: cpal::SampleFormat) -> u8 {
    (format.sample_size() * 8)
        .try_into()
        .unwrap_or(DEFAULT_MAX_BIT_DEPTH)
}

fn in_rate_family(source_rate: u32, base_rate: u32) -> bool {
    source_rate != 0
        && ((source_rate >= base_rate && source_rate.is_multiple_of(base_rate))
            || (source_rate < base_rate && base_rate.is_multiple_of(source_rate)))
}

fn best_rate_from(family: &[u32], max_rate: u32) -> Option<u32> {
    family.iter().rev().copied().find(|rate| *rate <= max_rate)
}

fn best_standard_rate(max_rate: u32) -> Option<u32> {
    STANDARD_TARGET_RATES
        .iter()
        .rev()
        .copied()
        .find(|rate| *rate <= max_rate)
}

#[cfg(test)]
mod tests {
    use super::{
        auto_target_rate, dop_dsd128_supported_for_backend, dop_dsd256_supported_for_backend,
        output_device_capabilities,
    };

    #[test]
    fn auto_best_keeps_source_clock_family_within_device_cap() {
        assert_eq!(auto_target_rate(44_100, 384_000), 352_800);
        assert_eq!(auto_target_rate(48_000, 384_000), 384_000);
        assert_eq!(auto_target_rate(44_100, 96_000), 88_200);
        assert_eq!(auto_target_rate(48_000, 96_000), 96_000);
    }

    #[test]
    fn auto_best_downshifts_when_source_exceeds_device_cap() {
        assert_eq!(auto_target_rate(176_400, 96_000), 88_200);
        assert_eq!(auto_target_rate(192_000, 96_000), 96_000);
    }

    #[test]
    fn auto_best_uses_48k_family_when_source_rate_is_unknown() {
        assert_eq!(auto_target_rate(0, 384_000), 384_000);
        assert_eq!(auto_target_rate(0, 192_000), 192_000);
        assert_eq!(auto_target_rate(0, 44_100), 44_100);
    }

    #[test]
    fn auto_best_falls_back_to_standard_rate_for_unmatched_clock_family() {
        assert_eq!(auto_target_rate(50_000, 384_000), 384_000);
        assert_eq!(auto_target_rate(50_000, 100_000), 96_000);
        assert_eq!(auto_target_rate(50_000, 40_000), 40_000);
    }

    #[test]
    fn dop_support_is_inferred_from_backend_and_pcm_carrier_rate() {
        assert!(dop_dsd128_supported_for_backend(
            Some("coreaudio"),
            352_800,
            24
        ));
        assert!(dop_dsd256_supported_for_backend(
            Some("wasapi"),
            705_600,
            32
        ));
        assert!(!dop_dsd256_supported_for_backend(
            Some("coreaudio"),
            384_000,
            32
        ));
        assert!(!dop_dsd128_supported_for_backend(Some("alsa"), 705_600, 32));
        assert!(!dop_dsd128_supported_for_backend(
            Some("wasapi"),
            705_600,
            16
        ));
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    #[test]
    fn hegel_h390_usb_uses_known_dop_capability_when_dsd_is_not_advertised() {
        let caps = output_device_capabilities(Some("Hegel H390 USB"));

        assert_eq!(
            caps.max_sample_rate,
            super::HEGEL_H390_USB_DOP_MAX_SAMPLE_RATE
        );
        assert_eq!(caps.max_dsd_rate, Some(256));
        assert!(caps.supports_dsd128);
        assert!(caps.supports_dsd256);
    }

    #[test]
    fn sonos_capability_limit_remains_48khz_without_dsd() {
        let target = crate::audio::sinks::sonos::SonosTarget {
            id: "RINCON_TEST".to_string(),
            name: "Sonos".to_string(),
            host: "127.0.0.1".to_string(),
            port: 1400,
            model: Some("Sonos Five".to_string()),
            coordinator: true,
            group_name: None,
        };
        let caps = output_device_capabilities(Some(
            &crate::audio::sinks::sonos::target_device_name(&target),
        ));

        assert_eq!(
            caps.max_sample_rate,
            crate::audio::sinks::sonos::SONOS_SAMPLE_RATE
        );
        assert_eq!(caps.max_dsd_rate, None);
        assert!(!caps.supports_dsd128);
        assert!(!caps.supports_dsd256);
    }
}
