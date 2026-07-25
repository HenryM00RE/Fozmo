//! Driver nominal-rate control and Apple Music source-format detection.
//!
//! Rate switches go through the driver's CoreAudio configuration-change
//! handshake, so a write to the nominal rate is asynchronous; `set_nominal_rate`
//! polls until the driver reports the applied rate or times out.
//!
//! Streaming tracks commonly hide their rate from AppleScript. On macOS the
//! poller therefore queries a short, tightly-filtered Unified Log window for
//! the format emitted by Apple's lossless decoder. This is an independent
//! implementation of the observable log-format technique; no MusicKit setup is
//! required and failure to read the private log degrades to AppleScript/current
//! capture rate instead of silently forcing 44.1 kHz.

use regex::Regex;
use serde_json::Value;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

pub(super) const SUPPORTED_CAPTURE_RATES: [u32; 6] =
    [44_100, 48_000, 88_200, 96_000, 176_400, 192_000];
pub(super) const UNIFIED_LOG_PROBE_ATTEMPTS: u8 = 5;
const UNIFIED_LOG_LOOKBACK_SECS: u64 = 8;
const UNIFIED_LOG_QUERY_TIMEOUT: Duration = Duration::from_secs(3);

pub(super) fn is_supported_capture_rate(rate_hz: u32) -> bool {
    SUPPORTED_CAPTURE_RATES.contains(&rate_hz)
}

/// Map a detected track rate onto the rate the capture device should run at.
/// Unsupported rates map to the nearest supported rate at or above them, or
/// the device maximum. Unknown rates are deliberately not accepted here:
/// callers must keep the current device rate rather than inventing 44.1 kHz.
pub(super) fn desired_capture_rate(rate: u32) -> u32 {
    if is_supported_capture_rate(rate) {
        return rate;
    }
    SUPPORTED_CAPTURE_RATES
        .iter()
        .copied()
        .find(|supported| *supported >= rate)
        .unwrap_or(192_000)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub(super) enum FormatDetectionSource {
    AppleDecoderLog = 1,
    MusicAudioCapabilitiesLog = 2,
    MusicAppleScript = 3,
    ManualOverride = 4,
}

impl FormatDetectionSource {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::AppleDecoderLog => "apple_decoder_log",
            Self::MusicAudioCapabilitiesLog => "music_audio_capabilities_log",
            Self::MusicAppleScript => "music_applescript",
            Self::ManualOverride => "manual_override",
        }
    }

    pub(super) fn from_code(code: u32) -> Option<Self> {
        match code {
            1 => Some(Self::AppleDecoderLog),
            2 => Some(Self::MusicAudioCapabilitiesLog),
            3 => Some(Self::MusicAppleScript),
            4 => Some(Self::ManualOverride),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SourceFormatDetection {
    pub sample_rate_hz: u32,
    pub source_bit_depth: Option<u32>,
    pub source: FormatDetectionSource,
    /// Stable for the same stored log event across overlapping queries.
    pub log_marker: Option<String>,
}

impl SourceFormatDetection {
    pub(super) fn from_applescript(sample_rate_hz: u32) -> Self {
        Self {
            sample_rate_hz,
            source_bit_depth: None,
            source: FormatDetectionSource::MusicAppleScript,
            log_marker: None,
        }
    }

    pub(super) fn from_manual_override(sample_rate_hz: u32) -> Self {
        Self {
            sample_rate_hz,
            source_bit_depth: None,
            source: FormatDetectionSource::ManualOverride,
            log_marker: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct UnifiedLogRecord {
    marker: String,
    message: String,
}

/// Parse `log show --style ndjson` and return the newest recognized source
/// format. The final `{"count":...,"finished":1}` record is ignored.
fn parse_unified_log_output(output: &str) -> Option<SourceFormatDetection> {
    let records = output
        .lines()
        .filter_map(|line| {
            let value = serde_json::from_str::<Value>(line).ok()?;
            let message = ["eventMessage", "composedMessage", "message"]
                .into_iter()
                .find_map(|field| value.get(field).and_then(Value::as_str))?
                .to_string();
            let marker = value
                .get("timestamp")
                .map(json_scalar)
                .filter(|value| !value.is_empty())
                .map(|timestamp| format!("{timestamp}\u{1f}{message}"))
                .unwrap_or_else(|| line.to_string());
            Some(UnifiedLogRecord { marker, message })
        })
        .collect::<Vec<_>>();

    records.iter().rev().find_map(parse_log_record)
}

fn json_scalar(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Number(value) => value.to_string(),
        _ => String::new(),
    }
}

fn parse_log_record(record: &UnifiedLogRecord) -> Option<SourceFormatDetection> {
    parse_apple_decoder_message(&record.message)
        .or_else(|| parse_music_audio_capabilities_message(&record.message))
        .map(|mut detection| {
            detection.log_marker = Some(record.marker.clone());
            detection
        })
}

fn parse_apple_decoder_message(message: &str) -> Option<SourceFormatDetection> {
    static RATE_PATTERN: OnceLock<Regex> = OnceLock::new();
    static BIT_DEPTH_PATTERN: OnceLock<Regex> = OnceLock::new();
    let rate_pattern = RATE_PATTERN.get_or_init(|| {
        Regex::new(
            r"(?is)ACAppleLosslessDecoder(?:\.cpp)?.*?Input format:.*?\b\d+\s*ch,\s*([0-9][0-9,]*(?:\.[0-9]+)?)\s*Hz",
        )
        .expect("Apple decoder log regex")
    });
    let bit_depth_pattern = BIT_DEPTH_PATTERN.get_or_init(|| {
        Regex::new(r"(?i)\bfrom\s+(\d{1,2})\s*-\s*bit\s+source")
            .expect("Apple decoder bit-depth regex")
    });
    let captures = rate_pattern.captures(message)?;
    let sample_rate_hz = parse_rate(captures.get(1)?.as_str(), false)?;
    let source_bit_depth = bit_depth_pattern
        .captures(message)
        .and_then(|captures| captures.get(1))
        .and_then(|value| parse_bit_depth(value.as_str()));
    Some(SourceFormatDetection {
        sample_rate_hz,
        source_bit_depth,
        source: FormatDetectionSource::AppleDecoderLog,
        log_marker: None,
    })
}

/// Newer Music releases also publish a private `audioCapabilities:` message.
/// Its ASBD sample rate is expressed in kHz. Treat bit depth as optional so a
/// wording change cannot prevent a still-exact rate from being used.
fn parse_music_audio_capabilities_message(message: &str) -> Option<SourceFormatDetection> {
    if !message.to_ascii_lowercase().contains("audiocapabilities:") {
        return None;
    }
    static RATE_PATTERN: OnceLock<Regex> = OnceLock::new();
    static BIT_DEPTH_PATTERN: OnceLock<Regex> = OnceLock::new();
    let rate_pattern = RATE_PATTERN.get_or_init(|| {
        Regex::new(r"(?i)\basbdSampleRate\s*=\s*([0-9][0-9,]*(?:\.[0-9]+)?)\s*(kHz|Hz)?")
            .expect("Music audio-capabilities rate regex")
    });
    let bit_depth_pattern = BIT_DEPTH_PATTERN.get_or_init(|| {
        Regex::new(r"(?i)\bsdBitDepth\s*=\s*(\d{1,2})\s*bit")
            .expect("Music audio-capabilities bit-depth regex")
    });
    let rate = rate_pattern.captures(message)?;
    let unit_is_khz = rate
        .get(2)
        .map(|unit| unit.as_str().eq_ignore_ascii_case("kHz"))
        // Apple's private field has historically been kHz when no explicit
        // unit is retained in a reformatted log line.
        .unwrap_or(true);
    let sample_rate_hz = parse_rate(rate.get(1)?.as_str(), unit_is_khz)?;
    let source_bit_depth = bit_depth_pattern
        .captures(message)
        .and_then(|captures| captures.get(1))
        .and_then(|value| parse_bit_depth(value.as_str()));
    Some(SourceFormatDetection {
        sample_rate_hz,
        source_bit_depth,
        source: FormatDetectionSource::MusicAudioCapabilitiesLog,
        log_marker: None,
    })
}

fn parse_rate(value: &str, unit_is_khz: bool) -> Option<u32> {
    let normalized = value.replace(',', "");
    let mut rate = normalized.parse::<f64>().ok()?;
    if unit_is_khz {
        rate *= 1_000.0;
    }
    if !rate.is_finite() || !(8_000.0..=768_000.0).contains(&rate) {
        return None;
    }
    let rounded = rate.round();
    ((rate - rounded).abs() <= 0.5).then_some(rounded as u32)
}

fn parse_bit_depth(value: &str) -> Option<u32> {
    value
        .parse::<u32>()
        .ok()
        .filter(|bits| (1..=64).contains(bits))
}

#[cfg(target_os = "macos")]
pub(super) fn query_recent_source_format() -> Result<Option<SourceFormatDetection>, String> {
    use std::process::{Command, Stdio};
    use std::time::{SystemTime, UNIX_EPOCH};

    const PREDICATE: &str = r#"process == "Music" AND ((subsystem == "com.apple.coreaudio" AND eventMessage CONTAINS[c] "ACAppleLosslessDecoder" AND eventMessage CONTAINS[c] "Input format:") OR (subsystem == "com.apple.Music" AND eventMessage CONTAINS[c] "audioCapabilities:"))"#;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|err| format!("system clock is before the Unix epoch: {err}"))?
        .as_secs();
    let start = format!("@{}", now.saturating_sub(UNIFIED_LOG_LOOKBACK_SECS));
    let mut command = Command::new("/usr/bin/log");
    command
        .args([
            "show",
            "--start",
            &start,
            "--style",
            "ndjson",
            "--color",
            "none",
            "--no-pager",
            "--no-backtrace",
            "--no-signpost",
            "--no-loss",
            "--info",
            "--debug",
            "--predicate",
            PREDICATE,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let output = command_output_with_timeout(command, UNIFIED_LOG_QUERY_TIMEOUT)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = truncate_error(stderr.trim(), 512);
        return Err(if detail.is_empty() {
            format!("`log show` exited with {}", output.status)
        } else {
            format!("`log show` exited with {}: {detail}", output.status)
        });
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_unified_log_output(&stdout))
}

#[cfg(target_os = "macos")]
fn command_output_with_timeout(
    mut command: std::process::Command,
    timeout: Duration,
) -> Result<std::process::Output, String> {
    let mut child = command
        .spawn()
        .map_err(|err| format!("could not start `/usr/bin/log show`: {err}"))?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                return child
                    .wait_with_output()
                    .map_err(|err| format!("could not collect `/usr/bin/log show`: {err}"));
            }
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(25));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "`/usr/bin/log show` did not finish within {} s",
                    timeout.as_secs()
                ));
            }
            Err(err) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("could not wait for `/usr/bin/log show`: {err}"));
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn truncate_error(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let truncated = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{truncated}…")
    } else {
        truncated
    }
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

    /// Database IDs are preferred. Metadata keeps streamed tracks debounced on
    /// Music versions that omit the database ID.
    pub(super) fn debounce_key(&self) -> Option<String> {
        self.track_key.clone().or_else(|| {
            let fields = [
                self.title.as_deref(),
                self.artist.as_deref(),
                self.album.as_deref(),
            ];
            fields
                .iter()
                .any(|field| field.is_some_and(|value| !value.is_empty()))
                .then(|| {
                    fields
                        .into_iter()
                        .map(Option::unwrap_or_default)
                        .collect::<Vec<_>>()
                        .join("\u{1f}")
                })
        })
    }
}

pub(super) fn parse_music_track_info(output: &str) -> MusicTrackInfo {
    let mut lines = output.lines();
    let mut next_field = || normalize_field(lines.next());
    let player_state = next_field();
    let track_key = next_field();
    let sample_rate_hz = next_field().and_then(|value| parse_rate(&value, false));
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

/// Debounce state: format probing is only started when the current track
/// changes, so an unavailable private log cannot retrigger work every tick.
#[derive(Debug, Default)]
pub(super) struct RateSwitchDebounce {
    last_track_key: Option<String>,
}

impl RateSwitchDebounce {
    pub(super) fn track_changed(&mut self, info: &MusicTrackInfo) -> bool {
        let Some(track_key) = info.debounce_key() else {
            return false;
        };
        if self.last_track_key.as_deref() == Some(track_key.as_str()) {
            return false;
        }
        self.last_track_key = Some(track_key);
        true
    }

    pub(super) fn reset(&mut self) {
        self.last_track_key = None;
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
    fn desired_rate_passes_through_supported_rates() {
        for rate in SUPPORTED_CAPTURE_RATES {
            assert_eq!(desired_capture_rate(rate), rate);
        }
    }

    #[test]
    fn desired_rate_rounds_up_unsupported_rates() {
        assert_eq!(desired_capture_rate(32_000), 44_100);
        assert_eq!(desired_capture_rate(64_000), 88_200);
        assert_eq!(desired_capture_rate(352_800), 192_000);
    }

    #[test]
    fn parses_apple_lossless_decoder_source_format() {
        let detection = parse_apple_decoder_message(
            "ACAppleLosslessDecoder.cpp:123 Input format:\n  2 ch, 96000 Hz from 24-bit source",
        )
        .expect("decoder format");
        assert_eq!(detection.sample_rate_hz, 96_000);
        assert_eq!(detection.source_bit_depth, Some(24));
        assert_eq!(detection.source, FormatDetectionSource::AppleDecoderLog);
    }

    #[test]
    fn decoder_parser_accepts_formatted_rate_and_spacing() {
        let detection = parse_apple_decoder_message(
            "ACAppleLosslessDecoder Input format: 2 ch, 176,400.0 Hz from 16 - bit source",
        )
        .expect("decoder format");
        assert_eq!(detection.sample_rate_hz, 176_400);
        assert_eq!(detection.source_bit_depth, Some(16));
    }

    #[test]
    fn parses_music_audio_capabilities_fallback() {
        let detection = parse_music_audio_capabilities_message(
            "audioCapabilities: asbdSampleRate = 88.2 kHz, sdBitDepth = 24 bit",
        )
        .expect("Music audio capabilities");
        assert_eq!(detection.sample_rate_hz, 88_200);
        assert_eq!(detection.source_bit_depth, Some(24));
        assert_eq!(
            detection.source,
            FormatDetectionSource::MusicAudioCapabilitiesLog
        );
    }

    #[test]
    fn parses_ndjson_and_uses_newest_recognized_event() {
        let old = serde_json::json!({
            "timestamp": "2026-07-25 10:00:00.000000+1200",
            "eventMessage": "ACAppleLosslessDecoder.cpp Input format: 2 ch, 44100 Hz from 16-bit source"
        })
        .to_string();
        let unrelated = serde_json::json!({
            "timestamp": "2026-07-25 10:00:01.000000+1200",
            "eventMessage": "not a decoder message"
        })
        .to_string();
        let new = serde_json::json!({
            "timestamp": "2026-07-25 10:00:02.000000+1200",
            "eventMessage": "ACAppleLosslessDecoder.cpp Input format: 2 ch, 192000 Hz from 24-bit source"
        })
        .to_string();
        let output = format!("{old}\n{unrelated}\n{new}\n{{\"count\":3,\"finished\":1}}\n");

        let detection = parse_unified_log_output(&output).expect("newest format");
        assert_eq!(detection.sample_rate_hz, 192_000);
        assert_eq!(detection.source_bit_depth, Some(24));
        assert!(
            detection
                .log_marker
                .as_deref()
                .is_some_and(|marker| marker.contains("10:00:02"))
        );
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
        assert!(debounce.track_changed(&info));
        assert!(!debounce.track_changed(&info));
        info.track_key = Some("2".to_string());
        info.sample_rate_hz = None;
        assert!(debounce.track_changed(&info));
    }

    #[test]
    fn debounce_uses_metadata_when_database_id_is_missing() {
        let mut debounce = RateSwitchDebounce::default();
        let first = parse_music_track_info(
            "playing\nmissing value\nmissing value\nSong\nArtist\nAlbum\n100",
        );
        assert!(debounce.track_changed(&first));
        assert!(!debounce.track_changed(&first));
        let second = parse_music_track_info(
            "playing\nmissing value\nmissing value\nNext Song\nArtist\nAlbum\n100",
        );
        assert!(debounce.track_changed(&second));
    }

    #[test]
    fn debounce_ignores_stopped_player_without_identity_and_can_reset() {
        let mut debounce = RateSwitchDebounce::default();
        let info = parse_music_track_info("stopped\n\n\n\n\n\n100");
        assert!(!debounce.track_changed(&info));
        let playing = parse_music_track_info("playing\n1\n96000\nSong\nArtist\nAlbum\n100");
        assert!(debounce.track_changed(&playing));
        debounce.reset();
        assert!(debounce.track_changed(&playing));
    }
}
