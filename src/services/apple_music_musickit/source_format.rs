//! Native source-format detection for MusicKit's out-of-process renderer.
//!
//! Core Audio's process-tap format describes the PCM mix delivered by the tap.
//! It is not proof of the catalog asset's decoded sample rate. For lossless
//! playback, the renderer's tightly PID-scoped Unified Log event is the only
//! observable source-rate signal currently available to this integration.
//! Missing, stale, or unreadable events deliberately produce no detection:
//! callers must not guess a source rate, and the strict playback path fails
//! before DSP handoff.

use regex::Regex;
use serde_json::Value;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

const UNIFIED_LOG_LOOKBACK_SECS: u64 = 8;
const UNIFIED_LOG_QUERY_TIMEOUT: Duration = Duration::from_secs(3);

pub(super) const NATIVE_APPLE_MUSIC_RATES: [u32; 6] =
    [44_100, 48_000, 88_200, 96_000, 176_400, 192_000];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MusicKitSourceFormat {
    pub(crate) sample_rate_hz: u32,
    pub(crate) source_bit_depth_bits: Option<u32>,
    /// Unified Log wall-clock timestamp used to prove this record belongs
    /// after the caller's per-entry playback boundary.
    pub(crate) logged_at: SystemTime,
    /// Stable identity for one stored log record across overlapping probes.
    pub(crate) log_marker: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MusicKitDecoderDetection {
    Lossless(MusicKitSourceFormat),
    Lossy {
        codec: String,
        sample_rate_hz: u32,
        logged_at: SystemTime,
        log_marker: String,
    },
}

impl MusicKitDecoderDetection {
    fn logged_at(&self) -> SystemTime {
        match self {
            Self::Lossless(format) => format.logged_at,
            Self::Lossy { logged_at, .. } => *logged_at,
        }
    }

    fn log_marker(&self) -> &str {
        match self {
            Self::Lossless(format) => &format.log_marker,
            Self::Lossy { log_marker, .. } => log_marker,
        }
    }
}

#[derive(Debug, Default)]
pub(super) struct SourceFormatProbeState {
    renderer_pid: Option<u32>,
    last_log_marker: Option<String>,
}

impl SourceFormatProbeState {
    pub(super) fn accept_new(
        &mut self,
        renderer_pid: u32,
        boundary: SystemTime,
        detection: Option<MusicKitDecoderDetection>,
    ) -> Option<MusicKitDecoderDetection> {
        if self.renderer_pid != Some(renderer_pid) {
            self.renderer_pid = Some(renderer_pid);
            self.last_log_marker = None;
        }
        let detection = detection?;
        if detection.logged_at() <= boundary {
            return None;
        }
        if self.last_log_marker.as_deref() == Some(detection.log_marker()) {
            return None;
        }
        self.last_log_marker = Some(detection.log_marker().to_string());
        Some(detection)
    }
}

pub(super) fn is_native_apple_music_rate(rate_hz: u32) -> bool {
    NATIVE_APPLE_MUSIC_RATES.contains(&rate_hz)
}

#[cfg(target_os = "macos")]
pub(super) fn query_recent_renderer_source_format(
    renderer_pid: u32,
    boundary: SystemTime,
    query_timeout: Duration,
) -> Result<Option<MusicKitDecoderDetection>, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("system clock is before the Unix epoch: {error}"))?
        .as_secs();
    let boundary_secs = boundary
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let start_secs = now
        .saturating_sub(UNIFIED_LOG_LOOKBACK_SECS)
        .max(boundary_secs.saturating_sub(1));
    let start = format!("@{start_secs}");
    let predicate = renderer_log_predicate(renderer_pid);
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
            &predicate,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let output =
        command_output_with_timeout(command, query_timeout.min(UNIFIED_LOG_QUERY_TIMEOUT))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = truncate_error(stderr.trim(), 512);
        return Err(if detail.is_empty() {
            format!("`log show` exited with {}", output.status)
        } else {
            format!("`log show` exited with {}: {detail}", output.status)
        });
    }
    Ok(parse_renderer_log_output(
        &String::from_utf8_lossy(&output.stdout),
        boundary,
    ))
}

#[cfg(not(target_os = "macos"))]
pub(super) fn query_recent_renderer_source_format(
    _renderer_pid: u32,
    _boundary: SystemTime,
    _query_timeout: Duration,
) -> Result<Option<MusicKitDecoderDetection>, String> {
    Ok(None)
}

fn renderer_log_predicate(renderer_pid: u32) -> String {
    format!(
        r#"processIdentifier == {renderer_pid} AND subsystem == "com.apple.coreaudio" AND eventMessage CONTAINS[c] "Input format:" AND (eventMessage CONTAINS[c] "ACAppleLosslessDecoder" OR eventMessage CONTAINS[c] "ACMP4AACBaseDecoder")"#
    )
}

fn parse_renderer_log_output(
    output: &str,
    boundary: SystemTime,
) -> Option<MusicKitDecoderDetection> {
    let detections = output
        .lines()
        .filter_map(|line| {
            let value = serde_json::from_str::<Value>(line).ok()?;
            let timestamp = value.get("timestamp")?.as_str()?;
            let logged_at = parse_log_timestamp(timestamp)?;
            if logged_at <= boundary {
                return None;
            }
            let message = ["eventMessage", "composedMessage", "message"]
                .into_iter()
                .find_map(|field| value.get(field).and_then(Value::as_str))?;
            let marker = format!("{timestamp}\u{1f}{message}");
            parse_apple_lossless_decoder_message(message, logged_at, marker.clone())
                .map(MusicKitDecoderDetection::Lossless)
                .or_else(|| parse_aac_decoder_message(message, logged_at, marker))
        })
        .collect::<Vec<_>>();

    // Music.app may create secondary/transitional AAC decoders around the
    // authoritative ALAC decoder for one selected track. Prefer the newest
    // fresh ALAC event anywhere in the snapshot; only report AAC when the
    // snapshot contains no ALAC at all. The caller keeps polling until its
    // deadline before treating that AAC observation as final.
    detections
        .iter()
        .filter_map(|detection| match detection {
            MusicKitDecoderDetection::Lossless(format) => Some(format),
            MusicKitDecoderDetection::Lossy { .. } => None,
        })
        .max_by_key(|format| format.logged_at)
        .cloned()
        .map(MusicKitDecoderDetection::Lossless)
        .or_else(|| {
            detections
                .into_iter()
                .max_by_key(MusicKitDecoderDetection::logged_at)
        })
}

fn parse_apple_lossless_decoder_message(
    message: &str,
    logged_at: SystemTime,
    log_marker: String,
) -> Option<MusicKitSourceFormat> {
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
    let sample_rate_hz = parse_rate(captures.get(1)?.as_str())?;
    if !is_native_apple_music_rate(sample_rate_hz) {
        return None;
    }
    let source_bit_depth_bits = bit_depth_pattern
        .captures(message)
        .and_then(|captures| captures.get(1))
        .and_then(|value| value.as_str().parse::<u32>().ok())
        .filter(|bits| (1..=64).contains(bits));
    Some(MusicKitSourceFormat {
        sample_rate_hz,
        source_bit_depth_bits,
        logged_at,
        log_marker,
    })
}

fn parse_aac_decoder_message(
    message: &str,
    logged_at: SystemTime,
    log_marker: String,
) -> Option<MusicKitDecoderDetection> {
    static AAC_PATTERN: OnceLock<Regex> = OnceLock::new();
    let pattern = AAC_PATTERN.get_or_init(|| {
        Regex::new(
            r"(?is)ACMP4AACBaseDecoder(?:\.cpp)?.*?Input format:.*?\b\d+\s*ch,\s*([0-9][0-9,]*(?:\.[0-9]+)?)\s*Hz,\s*([a-z0-9]+)",
        )
        .expect("Apple AAC decoder log regex")
    });
    let captures = pattern.captures(message)?;
    let sample_rate_hz = parse_rate(captures.get(1)?.as_str())?;
    let codec = captures.get(2)?.as_str().to_ascii_uppercase();
    Some(MusicKitDecoderDetection::Lossy {
        codec,
        sample_rate_hz,
        logged_at,
        log_marker,
    })
}

fn parse_log_timestamp(value: &str) -> Option<SystemTime> {
    let mut normalized = value.trim().to_string();
    if normalized.as_bytes().get(10) == Some(&b' ') {
        normalized.replace_range(10..11, "T");
    }
    if let Some(offset_start) = normalized
        .char_indices()
        .rev()
        .find_map(|(index, character)| {
            (index > 18 && matches!(character, '+' | '-')).then_some(index)
        })
    {
        let offset = &normalized[offset_start..];
        if offset.len() == 5 && offset[1..].bytes().all(|byte| byte.is_ascii_digit()) {
            normalized.insert(offset_start + 3, ':');
        }
    }
    let unix_nanos = OffsetDateTime::parse(&normalized, &Rfc3339)
        .ok()?
        .unix_timestamp_nanos();
    let unix_nanos = u128::try_from(unix_nanos).ok()?;
    let seconds = u64::try_from(unix_nanos / 1_000_000_000).ok()?;
    let nanos = u32::try_from(unix_nanos % 1_000_000_000).ok()?;
    UNIX_EPOCH.checked_add(Duration::new(seconds, nanos))
}

fn parse_rate(value: &str) -> Option<u32> {
    let rate = value.replace(',', "").parse::<f64>().ok()?;
    if !rate.is_finite() || !(8_000.0..=768_000.0).contains(&rate) {
        return None;
    }
    let rounded = rate.round();
    ((rate - rounded).abs() <= 0.5).then_some(rounded as u32)
}

fn command_output_with_timeout(
    mut command: Command,
    timeout: Duration,
) -> Result<std::process::Output, String> {
    let mut child = command
        .spawn()
        .map_err(|error| format!("could not start `/usr/bin/log show`: {error}"))?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                return child
                    .wait_with_output()
                    .map_err(|error| format!("could not collect `/usr/bin/log show`: {error}"));
            }
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(25));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "`/usr/bin/log show` did not finish within {} ms",
                    timeout.as_millis()
                ));
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("could not wait for `/usr/bin/log show`: {error}"));
            }
        }
    }
}

fn truncate_error(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let truncated = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{truncated}…")
    } else {
        truncated
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(timestamp: &str, message: &str) -> String {
        serde_json::json!({
            "timestamp": timestamp,
            "eventMessage": message,
        })
        .to_string()
    }

    fn timestamp(value: &str) -> SystemTime {
        parse_log_timestamp(value).expect("valid Unified Log timestamp")
    }

    #[test]
    fn renderer_predicate_is_pid_scoped_and_decoder_only() {
        let predicate = renderer_log_predicate(1234);
        assert!(predicate.contains("processIdentifier == 1234"));
        assert!(predicate.contains("ACAppleLosslessDecoder"));
        assert!(predicate.contains("ACMP4AACBaseDecoder"));
        assert!(!predicate.contains("audioCapabilities:"));
    }

    #[test]
    fn newest_exact_lossless_rate_wins() {
        let old = record(
            "2026-07-25 10:00:00.000000+1200",
            "ACAppleLosslessDecoder.cpp Input format: 2 ch, 44,100 Hz from 16-bit source",
        );
        let new = record(
            "2026-07-25 10:00:01.000000+1200",
            "ACAppleLosslessDecoder.cpp Input format:\n 2 ch, 192000 Hz from 24 - bit source",
        );
        let boundary = timestamp("2026-07-25 09:59:59.999999+1200");
        let detection =
            parse_renderer_log_output(&format!("{old}\n{new}\n"), boundary).expect("source format");
        let MusicKitDecoderDetection::Lossless(detection) = detection else {
            panic!("expected Apple Lossless detection");
        };
        assert_eq!(detection.sample_rate_hz, 192_000);
        assert_eq!(detection.source_bit_depth_bits, Some(24));
        assert!(detection.log_marker.contains("10:00:01"));
    }

    #[test]
    fn actual_aac_decoder_record_is_reported_as_lossy() {
        let aac = record(
            "2026-07-25 15:32:54.744462+1200",
            "ACMP4AACBaseDecoder.cpp:310 (0x79a27aa00) Input format: 2 ch, 44100 Hz, aac (0x00000000) 0 bits/channel, 0 bytes/packet, 1024 frames/packet, 0 bytes/frame",
        );
        let detection =
            parse_renderer_log_output(&aac, timestamp("2026-07-25 15:32:54.000000+1200"))
                .expect("AAC decoder detection");
        assert_eq!(
            detection,
            MusicKitDecoderDetection::Lossy {
                codec: "AAC".to_string(),
                sample_rate_hz: 44_100,
                logged_at: timestamp("2026-07-25 15:32:54.744462+1200"),
                log_marker: format!(
                    "{}\u{1f}{}",
                    "2026-07-25 15:32:54.744462+1200",
                    "ACMP4AACBaseDecoder.cpp:310 (0x79a27aa00) Input format: 2 ch, 44100 Hz, aac (0x00000000) 0 bits/channel, 0 bytes/packet, 1024 frames/packet, 0 bytes/frame"
                ),
            }
        );
    }

    #[test]
    fn fresh_lossless_decoder_wins_over_transitional_aac_decoders() {
        let initial_aac = record(
            "2026-07-25 19:46:17.145083+1200",
            "ACMP4AACBaseDecoder.cpp:310 (0x945e35c00) Input format: 2 ch, 48000 Hz, aac (0x00000000) 0 bits/channel, 0 bytes/packet, 1024 frames/packet, 0 bytes/frame",
        );
        let lossless = record(
            "2026-07-25 19:46:18.078480+1200",
            "ACAppleLosslessDecoder.cpp:680 (0x928566d00) Input format: 2 ch, 96000 Hz, alac (0x00000003) from 24-bit source, 4096 frames/packet",
        );
        let later_aac = record(
            "2026-07-25 19:46:18.905879+1200",
            "ACMP4AACBaseDecoder.cpp:310 (0x943a5ea00) Input format: 2 ch, 48000 Hz, aac (0x00000000) 0 bits/channel, 0 bytes/packet, 1024 frames/packet, 0 bytes/frame",
        );
        let detection = parse_renderer_log_output(
            &format!("{initial_aac}\n{lossless}\n{later_aac}\n"),
            timestamp("2026-07-25 19:46:17.000000+1200"),
        )
        .expect("lossless decoder detection");
        let MusicKitDecoderDetection::Lossless(format) = detection else {
            panic!("expected Apple Lossless to win over transitional AAC");
        };
        assert_eq!(format.sample_rate_hz, 96_000);
        assert_eq!(format.source_bit_depth_bits, Some(24));
    }

    #[test]
    fn unsupported_or_unrelated_rates_are_not_guessed() {
        let unsupported = record(
            "2026-07-25 10:00:00.000000+1200",
            "ACAppleLosslessDecoder.cpp Input format: 2 ch, 64000 Hz from 24-bit source",
        );
        let capabilities = record(
            "2026-07-25 10:00:01.000000+1200",
            "audioCapabilities: asbdSampleRate = 96 kHz, sdBitDepth = 24 bit",
        );
        assert!(
            parse_renderer_log_output(
                &format!("{unsupported}\n{capabilities}\n"),
                timestamp("2026-07-25 09:59:59.999999+1200"),
            )
            .is_none()
        );
    }

    #[test]
    fn records_at_or_before_the_entry_boundary_are_never_reused() {
        let previous = record(
            "2026-07-25 10:00:00.500000+1200",
            "ACAppleLosslessDecoder.cpp Input format: 2 ch, 44,100 Hz from 16-bit source",
        );
        let boundary = timestamp("2026-07-25 10:00:01.000000+1200");

        assert!(parse_renderer_log_output(&previous, boundary).is_none());

        let current = record(
            "2026-07-25 10:00:01.250000+1200",
            "ACAppleLosslessDecoder.cpp Input format: 2 ch, 192000 Hz from 24-bit source",
        );
        let detection = parse_renderer_log_output(&format!("{previous}\n{current}\n"), boundary)
            .expect("fresh source format");
        let MusicKitDecoderDetection::Lossless(detection) = detection else {
            panic!("expected Apple Lossless detection");
        };
        assert_eq!(detection.sample_rate_hz, 192_000);
    }

    #[test]
    fn overlapping_queries_emit_each_log_record_once() {
        let mut probe = SourceFormatProbeState::default();
        let first_boundary = timestamp("2026-07-25 10:00:00.000000+1200");
        let detection = MusicKitSourceFormat {
            sample_rate_hz: 96_000,
            source_bit_depth_bits: Some(24),
            logged_at: timestamp("2026-07-25 10:00:00.500000+1200"),
            log_marker: "one".to_string(),
        };
        assert_eq!(
            probe.accept_new(
                42,
                first_boundary,
                Some(MusicKitDecoderDetection::Lossless(detection.clone()))
            ),
            Some(MusicKitDecoderDetection::Lossless(detection.clone()))
        );
        assert_eq!(
            probe.accept_new(
                42,
                first_boundary,
                Some(MusicKitDecoderDetection::Lossless(detection.clone()))
            ),
            None
        );
        assert_eq!(
            probe.accept_new(
                43,
                first_boundary,
                Some(MusicKitDecoderDetection::Lossless(detection.clone()))
            ),
            Some(MusicKitDecoderDetection::Lossless(detection))
        );
    }

    #[test]
    fn a_missed_probe_cannot_leak_the_previous_rate_into_a_reused_pid() {
        let mut probe = SourceFormatProbeState::default();
        let previous = MusicKitSourceFormat {
            sample_rate_hz: 44_100,
            source_bit_depth_bits: Some(16),
            logged_at: timestamp("2026-07-25 10:00:00.500000+1200"),
            log_marker: "previous".to_string(),
        };
        let next_boundary = timestamp("2026-07-25 10:00:01.000000+1200");

        assert_eq!(probe.accept_new(42, UNIX_EPOCH, None), None);
        assert_eq!(
            probe.accept_new(
                42,
                next_boundary,
                Some(MusicKitDecoderDetection::Lossless(previous))
            ),
            None
        );

        let current = MusicKitSourceFormat {
            sample_rate_hz: 192_000,
            source_bit_depth_bits: Some(24),
            logged_at: timestamp("2026-07-25 10:00:01.250000+1200"),
            log_marker: "current".to_string(),
        };
        assert_eq!(
            probe.accept_new(
                42,
                next_boundary,
                Some(MusicKitDecoderDetection::Lossless(current.clone()))
            ),
            Some(MusicKitDecoderDetection::Lossless(current))
        );
    }
}
