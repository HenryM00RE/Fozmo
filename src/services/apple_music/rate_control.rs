//! Fozmo Capture nominal-rate control.

#[cfg(target_os = "macos")]
use std::time::{Duration, Instant};

const SUPPORTED_CAPTURE_RATES: [u32; 6] = [44_100, 48_000, 88_200, 96_000, 176_400, 192_000];

pub(super) fn is_supported_capture_rate(rate_hz: u32) -> bool {
    SUPPORTED_CAPTURE_RATES.contains(&rate_hz)
}

#[cfg(target_os = "macos")]
pub(super) fn set_nominal_rate(
    device_id: coreaudio_sys::AudioDeviceID,
    rate_hz: u32,
) -> Result<(), String> {
    use super::coreaudio;

    if !is_supported_capture_rate(rate_hz) {
        return Err(format!(
            "{rate_hz} Hz is not a supported Fozmo Capture rate."
        ));
    }
    let current = coreaudio::read_f64(
        device_id,
        coreaudio_sys::kAudioDevicePropertyNominalSampleRate,
    );
    if current.is_some_and(|rate| (rate - f64::from(rate_hz)).abs() < 0.5) {
        return Ok(());
    }
    coreaudio::write_scalar(
        device_id,
        coreaudio_sys::kAudioDevicePropertyNominalSampleRate,
        f64::from(rate_hz),
    )
    .map_err(|error| format!("Could not request a {rate_hz} Hz nominal rate: {error}"))?;

    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let applied = coreaudio::read_f64(
            device_id,
            coreaudio_sys::kAudioDevicePropertyNominalSampleRate,
        );
        if applied.is_some_and(|rate| (rate - f64::from(rate_hz)).abs() < 0.5) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "Fozmo Capture did not confirm the {rate_hz} Hz nominal rate within 3 s (current: {})",
                applied
                    .map(|rate| format!("{rate:.0} Hz"))
                    .unwrap_or_else(|| "unknown".to_string())
            ));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_rates_match_the_capture_driver() {
        for rate in SUPPORTED_CAPTURE_RATES {
            assert!(is_supported_capture_rate(rate));
        }
        assert!(!is_supported_capture_rate(32_000));
        assert!(!is_supported_capture_rate(352_800));
    }
}
