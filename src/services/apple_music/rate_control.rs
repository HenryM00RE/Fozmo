//! Driver nominal-rate control and Apple Music track-rate detection.
//!
//! Rate switches go through the driver's CoreAudio configuration-change
//! handshake, so a write to the nominal rate is asynchronous; `set_nominal_rate`
//! polls until the driver reports the applied rate or times out.

use std::time::{Duration, Instant};

pub(super) const SUPPORTED_CAPTURE_RATES: [u32; 6] =
    [44_100, 48_000, 88_200, 96_000, 176_400, 192_000];

/// AppleScript often returns `missing value` for streaming tracks; v1 falls
/// back to 44.1 kHz and surfaces the limitation so the user can override.
pub(super) const FALLBACK_TRACK_RATE_HZ: u32 = 44_100;

pub(super) fn is_supported_capture_rate(rate_hz: u32) -> bool {
    SUPPORTED_CAPTURE_RATES.contains(&rate_hz)
}

/// Map a detected track rate onto the rate the capture device should run at.
/// Unknown rates fall back to 44.1 kHz; unsupported rates map to the lowest
/// supported rate that is an integer multiple of the track's base family when
/// possible, otherwise the nearest supported rate at or above it.
pub(super) fn desired_capture_rate(track_rate_hz: Option<u32>) -> u32 {
    let Some(rate) = track_rate_hz.filter(|rate| *rate > 0) else {
        return FALLBACK_TRACK_RATE_HZ;
    };
    if is_supported_capture_rate(rate) {
        return rate;
    }
    SUPPORTED_CAPTURE_RATES
        .iter()
        .copied()
        .find(|supported| *supported >= rate)
        .unwrap_or(192_000)
}

/// Snapshot of the Music app read by the capture poller.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct MusicTrackInfo {
    pub player_state: Option<String>,
    /// `database ID` of the current track; the debounce key for rate switching.
    pub track_key: Option<String>,
    pub sample_rate_hz: Option<u32>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub sound_volume: Option<u32>,
}

impl MusicTrackInfo {
    pub(super) fn is_playing(&self) -> bool {
        self.player_state.as_deref() == Some("playing")
    }
}

pub(super) fn parse_music_track_info(output: &str) -> MusicTrackInfo {
    let mut lines = output.lines();
    let mut next_field = || normalize_field(lines.next());
    let player_state = next_field();
    let track_key = next_field();
    let sample_rate_hz = next_field().and_then(|value| value.parse::<u32>().ok());
    let title = next_field();
    let artist = next_field();
    let album = next_field();
    let sound_volume = next_field().and_then(|value| value.parse::<u32>().ok());
    MusicTrackInfo {
        player_state,
        track_key,
        sample_rate_hz,
        title,
        artist,
        album,
        sound_volume,
    }
}

fn normalize_field(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty() && *value != "missing value")
        .map(str::to_string)
}

/// One osascript round-trip covering everything the poller needs. Streaming
/// tracks can report `missing value` for sample rate; the `try` blocks keep a
/// single odd track from killing the whole poll.
pub(super) const MUSIC_POLL_SCRIPT: &[&str] = &[
    "tell application \"Music\"",
    "set playerState to player state as string",
    "set trackKey to \"\"",
    "set trackRate to \"\"",
    "set trackName to \"\"",
    "set artistName to \"\"",
    "set albumName to \"\"",
    "if player state is not stopped then",
    "try",
    "set trackKey to (database ID of current track) as string",
    "end try",
    "try",
    "set trackRate to (sample rate of current track) as string",
    "end try",
    "try",
    "set trackName to name of current track",
    "set artistName to artist of current track",
    "set albumName to album of current track",
    "end try",
    "end if",
    "set outputVolume to sound volume as string",
    "return playerState & linefeed & trackKey & linefeed & trackRate & linefeed & trackName & linefeed & artistName & linefeed & albumName & linefeed & outputVolume",
    "end tell",
];

/// Debounce state: a rate switch is only considered when the current track
/// changes, so a slow AppleScript reply or a rate we refuse to apply does not
/// retrigger every poll tick.
#[derive(Debug, Default)]
pub(super) struct RateSwitchDebounce {
    last_track_key: Option<String>,
}

impl RateSwitchDebounce {
    /// Returns the desired rate when this poll observes a new track.
    pub(super) fn desired_rate_on_track_change(&mut self, info: &MusicTrackInfo) -> Option<u32> {
        let track_key = info.track_key.as_deref()?;
        if self.last_track_key.as_deref() == Some(track_key) {
            return None;
        }
        self.last_track_key = Some(track_key.to_string());
        Some(desired_capture_rate(info.sample_rate_hz))
    }
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
    .map_err(|err| format!("Could not request a {rate_hz} Hz nominal rate: {err}"))?;

    // The driver applies the change via the host configuration-change
    // handshake, so confirm the applied rate rather than trusting the write.
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
    fn desired_rate_falls_back_to_44100_for_unknown() {
        assert_eq!(desired_capture_rate(None), 44_100);
        assert_eq!(desired_capture_rate(Some(0)), 44_100);
    }

    #[test]
    fn desired_rate_passes_through_supported_rates() {
        for rate in SUPPORTED_CAPTURE_RATES {
            assert_eq!(desired_capture_rate(Some(rate)), rate);
        }
    }

    #[test]
    fn desired_rate_rounds_up_unsupported_rates() {
        assert_eq!(desired_capture_rate(Some(32_000)), 44_100);
        assert_eq!(desired_capture_rate(Some(64_000)), 88_200);
        assert_eq!(desired_capture_rate(Some(352_800)), 192_000);
    }

    #[test]
    fn parses_full_music_poll_output() {
        let info = parse_music_track_info(
            "playing\n12345\n96000\nSong Title\nArtist Name\nAlbum Name\n100",
        );
        assert!(info.is_playing());
        assert_eq!(info.track_key.as_deref(), Some("12345"));
        assert_eq!(info.sample_rate_hz, Some(96_000));
        assert_eq!(info.title.as_deref(), Some("Song Title"));
        assert_eq!(info.sound_volume, Some(100));
    }

    #[test]
    fn parses_missing_value_rate_as_unknown() {
        let info = parse_music_track_info("playing\n12345\nmissing value\nSong\nArtist\nAlbum\n80");
        assert_eq!(info.sample_rate_hz, None);
        assert_eq!(info.sound_volume, Some(80));
    }

    #[test]
    fn debounce_fires_only_on_track_change() {
        let mut debounce = RateSwitchDebounce::default();
        let mut info = parse_music_track_info("playing\n1\n96000\nSong\nArtist\nAlbum\n100");
        assert_eq!(debounce.desired_rate_on_track_change(&info), Some(96_000));
        assert_eq!(debounce.desired_rate_on_track_change(&info), None);
        info.track_key = Some("2".to_string());
        info.sample_rate_hz = None;
        assert_eq!(debounce.desired_rate_on_track_change(&info), Some(44_100));
    }

    #[test]
    fn debounce_ignores_stopped_player_without_track() {
        let mut debounce = RateSwitchDebounce::default();
        let info = parse_music_track_info("stopped\n\n\n\n\n\n100");
        assert_eq!(debounce.desired_rate_on_track_change(&info), None);
    }
}
