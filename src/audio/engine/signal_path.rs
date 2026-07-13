use crate::audio::dsp::resampler::FilterType;
use crate::audio::output::device_caps;
use crate::audio::sinks::airplay;
use crate::settings::DsdSourceRule;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OutputMode {
    Pcm,
    Dsd64,
    Dsd128,
    Dsd256,
}

#[cfg(any(test, all(target_os = "windows", feature = "asio")))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct NativeDsdAttempt {
    pub(super) mode: OutputMode,
    pub(super) wire_rate: u32,
    pub(super) force_44k_family: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputTransport {
    None,
    PcmShared,
    PcmWasapiExclusive,
    PcmAsio,
    DopWasapi,
    DopCoreAudio,
    NativeDsdAsio,
    PcmAirPlayRaop,
    PcmAirPlay2,
    PcmCoreAudio,
}

impl OutputTransport {
    pub fn as_id(self) -> u32 {
        match self {
            Self::None => 0,
            Self::PcmShared => 1,
            Self::PcmWasapiExclusive => 2,
            Self::PcmAsio => 3,
            Self::DopWasapi => 4,
            Self::DopCoreAudio => 5,
            Self::NativeDsdAsio => 6,
            Self::PcmAirPlayRaop => 7,
            Self::PcmAirPlay2 => 8,
            Self::PcmCoreAudio => 9,
        }
    }

    pub fn from_id(id: u32) -> Self {
        match id {
            1 => Self::PcmShared,
            2 => Self::PcmWasapiExclusive,
            3 => Self::PcmAsio,
            4 => Self::DopWasapi,
            5 => Self::DopCoreAudio,
            6 => Self::NativeDsdAsio,
            7 => Self::PcmAirPlayRaop,
            8 => Self::PcmAirPlay2,
            9 => Self::PcmCoreAudio,
            _ => Self::None,
        }
    }

    pub fn as_name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::PcmShared => "pcm_shared",
            Self::PcmWasapiExclusive => "pcm_wasapi_exclusive",
            Self::PcmAsio => "pcm_asio",
            Self::DopWasapi => "dop_wasapi",
            Self::DopCoreAudio => "dop_coreaudio",
            Self::NativeDsdAsio => "native_dsd_asio",
            Self::PcmAirPlayRaop => "pcm_airplay_raop",
            Self::PcmAirPlay2 => "pcm_airplay2",
            Self::PcmCoreAudio => "pcm_coreaudio",
        }
    }
}

impl OutputMode {
    pub fn as_id(self) -> u32 {
        match self {
            OutputMode::Pcm => 0,
            OutputMode::Dsd128 => 1,
            OutputMode::Dsd256 => 2,
            // Appended after the original modes so persisted DSD128/DSD256 ids stay stable.
            OutputMode::Dsd64 => 3,
        }
    }

    pub fn from_id(id: u32) -> Self {
        match id {
            1 => OutputMode::Dsd128,
            2 => OutputMode::Dsd256,
            3 => OutputMode::Dsd64,
            _ => OutputMode::Pcm,
        }
    }

    pub fn as_name(self) -> &'static str {
        match self {
            OutputMode::Pcm => "Pcm",
            OutputMode::Dsd64 => "Dsd64",
            OutputMode::Dsd128 => "Dsd128",
            OutputMode::Dsd256 => "Dsd256",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "Pcm" => Some(OutputMode::Pcm),
            "Dsd64" => Some(OutputMode::Dsd64),
            "Dsd128" => Some(OutputMode::Dsd128),
            "Dsd256" => Some(OutputMode::Dsd256),
            _ => None,
        }
    }

    pub fn is_dsd(self) -> bool {
        matches!(
            self,
            OutputMode::Dsd64 | OutputMode::Dsd128 | OutputMode::Dsd256
        )
    }

    /// DSD wire rate (Hz) used by the modulator for this output mode + source.
    /// Returns `None` for PCM mode or for source rates outside the 44.1/48 kHz
    /// families. See `DsdRate::wire_rate_for_source` for the full rule.
    pub fn dsd_wire_rate(self, source_rate: u32) -> Option<u32> {
        use crate::audio::dsd::dsd_render::DsdRate;
        match self {
            OutputMode::Pcm => None,
            OutputMode::Dsd64 => DsdRate::Dsd64.wire_rate_for_source(source_rate),
            OutputMode::Dsd128 => DsdRate::Dsd128.wire_rate_for_source(source_rate),
            OutputMode::Dsd256 => DsdRate::Dsd256.wire_rate_for_source(source_rate),
        }
    }

    /// Native ASIO candidates in preference order for the requested DSD mode.
    /// DSD256 is treated as 44.1-family-compatible for 48/96/192 kHz sources:
    /// many DACs top out at 11.2896 MHz and reject the 12.288 MHz carrier.
    /// DSD128 is only attempted when the active DSD rule requested DSD128.
    #[cfg(any(test, all(target_os = "windows", feature = "asio")))]
    pub(super) fn native_dsd_attempts(self, source_rate: u32) -> Vec<NativeDsdAttempt> {
        if self == Self::Dsd256 && source_uses_44k_family_dsd256_compat(source_rate) {
            return vec![NativeDsdAttempt {
                mode: Self::Dsd256,
                wire_rate: crate::audio::dsd::dsd_render::DsdRate::Dsd256.wire_rate_44k_family(),
                force_44k_family: true,
            }];
        }
        let Some(wire_rate) = self.dsd_wire_rate(source_rate) else {
            return Vec::new();
        };
        vec![NativeDsdAttempt {
            mode: self,
            wire_rate,
            force_44k_family: false,
        }]
    }
}

pub(super) fn resolve_pcm_dsp_target(
    source_rate: u32,
    mut target_rate: u32,
    upsampling_enabled: bool,
    device_name: Option<&str>,
) -> (u32, bool) {
    let airplay_output = device_name.is_some_and(airplay::is_airplay_device_name);
    if airplay_output {
        target_rate = airplay::AIRPLAY_SAMPLE_RATE;
    } else if !upsampling_enabled {
        target_rate = source_rate;
    } else if target_rate == 0 {
        target_rate = device_caps::auto_target_rate_for_device(source_rate, device_name);
    }

    let format_conversion_required = airplay_output;
    let should_resample =
        (upsampling_enabled || format_conversion_required) && source_rate != target_rate;
    (target_rate, should_resample)
}

pub(super) fn effective_dsd_target_rate(
    requested_mode: OutputMode,
    active_mode: Option<OutputMode>,
    active_wire_rate: Option<u32>,
    source_rate: u32,
    pcm_target_rate: u32,
) -> u32 {
    if let Some(wire_rate) = active_wire_rate {
        return wire_rate;
    }
    let mode = active_mode
        .filter(|mode| mode.is_dsd())
        .unwrap_or(requested_mode);
    mode.dsd_wire_rate(source_rate).unwrap_or(pcm_target_rate)
}

pub(super) fn source_uses_44k_family_dsd256_compat(source_rate: u32) -> bool {
    source_rate != 0
        && source_rate.is_multiple_of(48_000)
        && !source_rate.is_multiple_of(44_100)
        && matches!(source_rate, 48_000 | 96_000 | 192_000)
}

#[derive(Clone, Copy)]
pub(super) struct DsdPlaybackPolicy {
    pub(super) mode: OutputMode,
    pub(super) filter_type: FilterType,
}

pub(super) fn dsd_policy_for_source(
    requested_mode: OutputMode,
    configured_filter: FilterType,
    source_rate: u32,
    rules: &[DsdSourceRule],
) -> DsdPlaybackPolicy {
    if let Some(rule) = rules.iter().find(|rule| rule.source_rate == source_rate) {
        let mode = OutputMode::from_name(&rule.output_mode).unwrap_or(requested_mode);
        let filter_type = FilterType::from_name(&rule.filter_type).unwrap_or(configured_filter);
        if mode.is_dsd() {
            return DsdPlaybackPolicy { mode, filter_type };
        }
    }
    DsdPlaybackPolicy {
        mode: requested_mode,
        filter_type: configured_filter,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::sinks::airplay::{AirPlayServiceKind, AirPlayTarget};

    fn airplay_device_name() -> String {
        airplay::target_device_name(&AirPlayTarget {
            id: "abc".to_string(),
            name: "Kitchen".to_string(),
            service_kind: AirPlayServiceKind::Raop,
            supported: true,
            unsupported_reason: None,
        })
    }

    #[test]
    fn output_mode_round_trips_via_id() {
        for mode in [
            OutputMode::Pcm,
            OutputMode::Dsd64,
            OutputMode::Dsd128,
            OutputMode::Dsd256,
        ] {
            assert_eq!(OutputMode::from_id(mode.as_id()), mode);
            assert_eq!(OutputMode::from_name(mode.as_name()), Some(mode));
        }
    }

    #[test]
    fn unknown_id_falls_back_to_pcm() {
        assert_eq!(OutputMode::from_id(99), OutputMode::Pcm);
    }

    #[test]
    fn is_dsd_classifies_correctly() {
        assert!(!OutputMode::Pcm.is_dsd());
        assert!(OutputMode::Dsd64.is_dsd());
        assert!(OutputMode::Dsd128.is_dsd());
        assert!(OutputMode::Dsd256.is_dsd());
    }

    #[test]
    fn dsd_wire_rate_is_family_locked_not_source_scaled() {
        assert_eq!(OutputMode::Dsd64.dsd_wire_rate(44_100), Some(2_822_400));
        assert_eq!(OutputMode::Dsd64.dsd_wire_rate(88_200), Some(2_822_400));
        assert_eq!(OutputMode::Dsd64.dsd_wire_rate(176_400), Some(2_822_400));
        assert_eq!(OutputMode::Dsd64.dsd_wire_rate(48_000), Some(3_072_000));
        assert_eq!(OutputMode::Dsd64.dsd_wire_rate(96_000), Some(3_072_000));
        assert_eq!(OutputMode::Dsd64.dsd_wire_rate(192_000), Some(3_072_000));
        assert_eq!(OutputMode::Dsd64.dsd_wire_rate(22_050), None);
        assert_eq!(OutputMode::Dsd128.dsd_wire_rate(44_100), Some(5_644_800));
        assert_eq!(OutputMode::Dsd128.dsd_wire_rate(88_200), Some(5_644_800));
        assert_eq!(OutputMode::Dsd128.dsd_wire_rate(176_400), Some(5_644_800));
        assert_eq!(OutputMode::Dsd128.dsd_wire_rate(48_000), Some(6_144_000));
        assert_eq!(OutputMode::Dsd128.dsd_wire_rate(96_000), Some(6_144_000));
        assert_eq!(OutputMode::Dsd128.dsd_wire_rate(192_000), Some(6_144_000));
        assert_eq!(OutputMode::Dsd256.dsd_wire_rate(44_100), Some(11_289_600));
        assert_eq!(OutputMode::Dsd256.dsd_wire_rate(48_000), Some(12_288_000));
        assert_eq!(OutputMode::Pcm.dsd_wire_rate(44_100), None);
        assert_eq!(OutputMode::Dsd128.dsd_wire_rate(22_050), None);
        assert_eq!(OutputMode::Dsd128.dsd_wire_rate(384_000), Some(6_144_000));
        assert_eq!(OutputMode::Dsd128.dsd_wire_rate(6_144_000), None);
    }

    #[test]
    fn native_dsd256_attempts_44k_compat_without_12mhz_probe() {
        assert_eq!(
            OutputMode::Dsd256.native_dsd_attempts(48_000),
            vec![NativeDsdAttempt {
                mode: OutputMode::Dsd256,
                wire_rate: 11_289_600,
                force_44k_family: true,
            }]
        );
        assert_eq!(
            OutputMode::Dsd256.native_dsd_attempts(192_000),
            vec![NativeDsdAttempt {
                mode: OutputMode::Dsd256,
                wire_rate: 11_289_600,
                force_44k_family: true,
            }]
        );
        assert_eq!(
            OutputMode::Dsd256.native_dsd_attempts(44_100),
            vec![NativeDsdAttempt {
                mode: OutputMode::Dsd256,
                wire_rate: 11_289_600,
                force_44k_family: false,
            }]
        );
    }

    #[test]
    fn native_dsd128_does_not_downgrade_or_retry_another_dsd_mode() {
        assert_eq!(
            OutputMode::Dsd128.native_dsd_attempts(96_000),
            vec![NativeDsdAttempt {
                mode: OutputMode::Dsd128,
                wire_rate: 6_144_000,
                force_44k_family: false,
            }]
        );
        assert!(OutputMode::Pcm.native_dsd_attempts(48_000).is_empty());
    }

    #[test]
    fn effective_dsd_target_rate_uses_live_fallback_mode() {
        assert_eq!(
            effective_dsd_target_rate(
                OutputMode::Dsd256,
                Some(OutputMode::Dsd128),
                None,
                48_000,
                384_000,
            ),
            6_144_000
        );
        assert_eq!(
            effective_dsd_target_rate(
                OutputMode::Dsd256,
                Some(OutputMode::Dsd128),
                None,
                96_000,
                384_000,
            ),
            6_144_000
        );
        assert_eq!(
            effective_dsd_target_rate(OutputMode::Dsd256, None, None, 48_000, 384_000),
            12_288_000
        );
        assert_eq!(
            effective_dsd_target_rate(
                OutputMode::Dsd256,
                Some(OutputMode::Dsd256),
                Some(11_289_600),
                48_000,
                384_000,
            ),
            11_289_600
        );
    }

    #[test]
    fn transport_status_names_round_trip() {
        for transport in [
            OutputTransport::None,
            OutputTransport::PcmShared,
            OutputTransport::PcmWasapiExclusive,
            OutputTransport::PcmAsio,
            OutputTransport::DopWasapi,
            OutputTransport::DopCoreAudio,
            OutputTransport::NativeDsdAsio,
            OutputTransport::PcmAirPlayRaop,
            OutputTransport::PcmAirPlay2,
            OutputTransport::PcmCoreAudio,
        ] {
            assert_eq!(OutputTransport::from_id(transport.as_id()), transport);
            assert!(!transport.as_name().is_empty());
        }
    }

    #[test]
    fn airplay_output_resamples_to_transport_rate_even_when_upsampling_is_disabled() {
        let device_name = airplay_device_name();
        let (target_rate, should_resample) =
            resolve_pcm_dsp_target(96_000, 0, false, Some(&device_name));

        assert_eq!(target_rate, airplay::AIRPLAY_SAMPLE_RATE);
        assert!(should_resample);
    }

    #[test]
    fn disabled_upsampling_keeps_source_rate_and_skips_resampler_for_pcm() {
        let (target_rate, should_resample) = resolve_pcm_dsp_target(96_000, 384_000, false, None);

        assert_eq!(target_rate, 96_000);
        assert!(!should_resample);
    }

    #[test]
    fn explicit_pcm_target_only_resamples_when_rate_changes() {
        assert_eq!(
            resolve_pcm_dsp_target(96_000, 96_000, true, None),
            (96_000, false)
        );
        assert_eq!(
            resolve_pcm_dsp_target(96_000, 192_000, true, None),
            (192_000, true)
        );
    }

    #[test]
    fn airplay_transport_rate_is_forced_even_without_rate_change() {
        let device_name = airplay_device_name();
        let (target_rate, should_resample) =
            resolve_pcm_dsp_target(airplay::AIRPLAY_SAMPLE_RATE, 0, false, Some(&device_name));

        assert_eq!(target_rate, airplay::AIRPLAY_SAMPLE_RATE);
        assert!(!should_resample);
    }
}
