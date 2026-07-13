//! FFmpeg/libopus backend for local Ogg Opus playback derivatives (issue #81).
//!
//! The in-process [`super::opus`] encoder bridges non-48 kHz sources to 48 kHz
//! with Fozmo's own sinc resampler before libopus. On iOS that path produced
//! audibly broken local playback for non-48 kHz files, so Opus derivatives now
//! hand the native source file to FFmpeg and let it decode, downmix, and
//! resample to 48 kHz before libopus:
//!
//! ```text
//! native local file -> FFmpeg decode -> [EQ biquads] -> SRC to 48 kHz -> libopus -> Ogg Opus
//! ```
//!
//! FFmpeg's stdout is streamed straight into the derivative file so the
//! existing progressive tailing keeps working.
//!
//! Zone EQ is baked in by translating Fozmo's parametric bands into FFmpeg's
//! RBJ-cookbook biquad filters (`equalizer`, `bass`, `treble`, …). Fozmo's SVF
//! is deliberately calibrated to the same RBJ semantics (see
//! [`crate::audio::dsp::eq`]), so a band's `freq/gain/Q` maps one-to-one. The
//! EQ runs at the source rate, before the 48 kHz resample, matching where the
//! in-process path applied it.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use super::opus::{MAX_BITRATE_KBPS, MIN_BITRATE_KBPS};
use crate::audio::eq::{BandType, EqBand, EqConfig};

/// Copy stdout in these increments while FFmpeg produces the derivative.
const STDOUT_CHUNK_BYTES: usize = 64 * 1024;
/// Only the tail of FFmpeg's stderr is worth logging on failure.
const STDERR_TAIL_BYTES: usize = 600;
/// Protocols FFmpeg may use while opening library tracks. Network protocols
/// are deliberately excluded so a crafted playlist cannot turn a local
/// transcode request into an outbound request.
const INPUT_PROTOCOL_WHITELIST: &str = "file,pipe";

/// FFmpeg capabilities relevant to the Opus derivative pipeline.
#[derive(Clone, Copy, Debug)]
struct Capabilities {
    /// Whether the build has the soxr resampler (`--enable-libsoxr`). When
    /// absent we fall back to FFmpeg's default `aresample` engine.
    soxr: bool,
}

/// The FFmpeg binary to run: `FOZMO_FFMPEG_PATH` when set, else `ffmpeg` on
/// `PATH`.
fn ffmpeg_binary() -> String {
    std::env::var(crate::app::identity::env_key("FFMPEG_PATH"))
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "ffmpeg".to_string())
}

/// Process-wide capability probe for the default binary, resolved once on
/// first Opus derivative request. Failure (missing FFmpeg or libopus) is
/// cached too so every subsequent request reports the same clear error rather
/// than repeatedly shelling out.
fn capabilities() -> &'static Result<Capabilities, String> {
    static CAPS: OnceLock<Result<Capabilities, String>> = OnceLock::new();
    CAPS.get_or_init(|| probe_capabilities(&ffmpeg_binary()))
}

/// Encode `source` to Ogg Opus via FFmpeg/libopus, streaming container bytes
/// into `out` as they are produced. When `eq` is an active config, its bands
/// are baked in as FFmpeg biquad filters. `cancel` is polled between chunks so
/// an abandoned job can stop the child early. On any failure — FFmpeg missing,
/// libopus missing, or a non-zero exit — this returns an error rather than
/// silently falling back, so callers surface a clear transcode failure instead
/// of shipping broken audio.
pub fn encode_ogg_opus(
    source: &Path,
    out: &mut dyn Write,
    bitrate_kbps: u32,
    eq: Option<&EqConfig>,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let caps = match capabilities() {
        Ok(caps) => *caps,
        Err(error) => return Err(error.clone()),
    };
    let eq_filters = eq.map(eq_filter_chain).unwrap_or_default();
    run_encode(
        &ffmpeg_binary(),
        source,
        out,
        bitrate_kbps,
        caps.soxr,
        &eq_filters,
        cancel,
    )
}

/// FFmpeg audio-filter fragments for `eq`, in Fozmo's preamp-then-bands order.
/// Empty when the config is disabled or has no active bands. Each fragment is a
/// single filter; the caller joins them (with the resampler) into one `-af`.
fn eq_filter_chain(eq: &EqConfig) -> Vec<String> {
    if !eq.enabled {
        return Vec::new();
    }
    let mut filters = Vec::new();
    if eq.preamp_db.abs() > f32::EPSILON {
        filters.push(format!("volume={:.6}dB", eq.preamp_db));
    }
    for band in eq.bands.iter().filter(|band| band.enabled) {
        filters.push(band_filter(band));
    }
    filters
}

/// Translate one parametric band to its RBJ-cookbook FFmpeg biquad. Fozmo's
/// SVF uses the same `freq/gain/Q` semantics, so the parameters pass straight
/// through; `t=q` selects the Q-factor width form the SVF also uses.
fn band_filter(band: &EqBand) -> String {
    let f = band.freq_hz;
    let q = band.q.max(0.01);
    let g = band.gain_db;
    match band.band_type {
        BandType::Peaking => format!("equalizer=f={f:.6}:t=q:w={q:.6}:g={g:.6}"),
        BandType::LowShelf => format!("bass=f={f:.6}:t=q:w={q:.6}:g={g:.6}"),
        BandType::HighShelf => format!("treble=f={f:.6}:t=q:w={q:.6}:g={g:.6}"),
        BandType::LowPass => format!("lowpass=f={f:.6}:t=q:w={q:.6}"),
        BandType::HighPass => format!("highpass=f={f:.6}:t=q:w={q:.6}"),
        BandType::Notch => format!("bandreject=f={f:.6}:t=q:w={q:.6}"),
        BandType::AllPass => format!("allpass=f={f:.6}:t=q:w={q:.6}"),
    }
}

/// Final resample stage of the filter chain: soxr when the build supports it,
/// otherwise FFmpeg's default engine. Placed last so any EQ runs at the source
/// rate, before the 48 kHz conversion.
fn resample_filter(use_soxr: bool) -> String {
    if use_soxr {
        "aresample=48000:resampler=soxr:precision=28".to_string()
    } else {
        "aresample=48000".to_string()
    }
}

/// Post-input FFmpeg arguments for the Opus encode. Kept separate from the
/// input path so they can be asserted in tests without a real file.
fn build_output_args(bitrate_kbps: u32, use_soxr: bool, eq_filters: &[String]) -> Vec<String> {
    let bitrate_kbps = bitrate_kbps.clamp(MIN_BITRATE_KBPS, MAX_BITRATE_KBPS);
    let mut filters: Vec<String> = eq_filters.to_vec();
    filters.push(resample_filter(use_soxr));
    let mut args: Vec<String> = [
        "-map", "0:a:0", "-vn", "-sn", "-dn", "-ac", "2", "-ar", "48000",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    args.push("-af".into());
    args.push(filters.join(","));
    args.push("-c:a".into());
    args.push("libopus".into());
    args.push("-b:a".into());
    args.push(format!("{bitrate_kbps}k"));
    args.extend(
        [
            "-vbr",
            "on",
            "-compression_level",
            "10",
            "-application",
            "audio",
            "-frame_duration",
            "20",
            "-f",
            "ogg",
            "pipe:1",
        ]
        .into_iter()
        .map(String::from),
    );
    args
}

/// Input options must precede `-i` to constrain probing and nested resources.
fn build_input_args(source: &Path) -> Vec<OsString> {
    vec![
        "-protocol_whitelist".into(),
        INPUT_PROTOCOL_WHITELIST.into(),
        "-i".into(),
        source.as_os_str().to_os_string(),
    ]
}

#[allow(clippy::too_many_arguments)]
fn run_encode(
    bin: &str,
    source: &Path,
    out: &mut dyn Write,
    bitrate_kbps: u32,
    use_soxr: bool,
    eq_filters: &[String],
    cancel: &AtomicBool,
) -> Result<(), String> {
    let mut child = Command::new(bin)
        .arg("-hide_banner")
        .arg("-nostdin")
        .arg("-y")
        .args(build_input_args(source))
        .args(build_output_args(bitrate_kbps, use_soxr, eq_filters))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn ffmpeg ({bin}): {e}"))?;

    let mut stdout = child.stdout.take().expect("ffmpeg stdout piped");
    let mut stderr = child.stderr.take().expect("ffmpeg stderr piped");
    // Drain stderr on a separate thread so a chatty FFmpeg cannot deadlock by
    // filling the stderr pipe while we are blocked reading stdout.
    let stderr_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf);
        buf
    });

    let mut chunk = vec![0_u8; STDOUT_CHUNK_BYTES];
    let copy_result = loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            break Err("transcode cancelled".to_string());
        }
        match stdout.read(&mut chunk) {
            Ok(0) => break Ok(()),
            Ok(n) => {
                if let Err(e) = out.write_all(&chunk[..n]) {
                    let _ = child.kill();
                    break Err(format!("write opus derivative: {e}"));
                }
            }
            Err(e) => {
                let _ = child.kill();
                break Err(format!("read ffmpeg stdout: {e}"));
            }
        }
    };

    let status = child.wait().map_err(|e| format!("wait for ffmpeg: {e}"))?;
    let stderr_bytes = stderr_handle.join().unwrap_or_default();
    copy_result?;
    if !status.success() {
        return Err(format!(
            "ffmpeg exited unsuccessfully ({status}): {}",
            stderr_tail(&stderr_bytes)
        ));
    }
    Ok(())
}

/// Sanitized tail of FFmpeg's stderr for failure logs.
fn stderr_tail(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let trimmed = text.trim();
    let tail = if trimmed.len() > STDERR_TAIL_BYTES {
        &trimmed[trimmed.len() - STDERR_TAIL_BYTES..]
    } else {
        trimmed
    };
    crate::diagnostics::logging::sanitize_error(&tail.replace(['\n', '\r'], " "))
}

/// Probe `bin` for the libopus encoder and soxr resampler. Errors when FFmpeg
/// cannot be run or the libopus encoder is missing; a missing soxr build just
/// disables the high-precision resampler.
fn probe_capabilities(bin: &str) -> Result<Capabilities, String> {
    let encoders = run_info(bin, &["-hide_banner", "-loglevel", "error", "-encoders"])?;
    if !encoders.contains("libopus") {
        return Err("ffmpeg is available but was built without the libopus encoder".to_string());
    }
    let soxr = run_info(bin, &["-hide_banner", "-buildconf"])
        .map(|conf| conf.contains("libsoxr"))
        .unwrap_or(false);
    Ok(Capabilities { soxr })
}

/// Run an FFmpeg info command (`-encoders`, `-buildconf`, …) and return its
/// combined stdout+stderr text. Different FFmpeg builds send these tables to
/// either stream, so both are captured.
fn run_info(bin: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("ffmpeg is not available ({bin}): {e}"))?;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::transcode::test_support::write_wav;
    use std::sync::atomic::AtomicBool;

    fn temp_dir(prefix: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "fozmo-ffmpeg-opus-{prefix}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn ffmpeg_with_libopus() -> bool {
        probe_capabilities(&ffmpeg_binary()).is_ok()
    }

    fn assert_valid_ogg_opus(bytes: &[u8]) {
        assert!(bytes.len() > 100, "derivative should not be empty");
        assert_eq!(&bytes[..4], b"OggS", "stream must start with an Ogg page");
        let haystack = |needle: &[u8]| bytes.windows(needle.len()).any(|w| w == needle);
        assert!(haystack(b"OpusHead"), "missing Opus identification header");
    }

    /// The `-af` value from a set of output args, for filter-chain assertions.
    fn filter_chain(args: &[String]) -> String {
        let idx = args.iter().position(|a| a == "-af").expect("-af present");
        args[idx + 1].clone()
    }

    fn one_band_config(band_type: BandType, freq: f32, gain: f32, q: f32) -> EqConfig {
        let mut config = EqConfig {
            enabled: true,
            ..EqConfig::default()
        };
        config.bands[3] = EqBand {
            enabled: true,
            band_type,
            freq_hz: freq,
            gain_db: gain,
            q,
        };
        config
    }

    #[test]
    fn output_args_carry_bitrate_and_libopus() {
        let args = build_output_args(256, false, &[]);
        assert!(args.iter().any(|a| a == "libopus"));
        assert!(args.windows(2).any(|w| w[0] == "-b:a" && w[1] == "256k"));
        assert!(args.iter().any(|a| a == "pipe:1"));
    }

    #[test]
    fn input_args_restrict_protocols_before_input() {
        let source = Path::new("/music/evil.mp3");
        let args = build_input_args(source);
        assert_eq!(args[0], "-protocol_whitelist");
        assert_eq!(args[1], INPUT_PROTOCOL_WHITELIST);
        assert_eq!(args[2], "-i");
        assert_eq!(args[3], source.as_os_str());
    }

    #[cfg(unix)]
    #[test]
    fn run_encode_passes_protocol_policy_before_input() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("argv");
        let fake = dir.join("ffmpeg");
        let argv_file = dir.join("argv.txt");
        let source = dir.join("evil.mp3");
        std::fs::write(&source, b"#EXTM3U\nhttp://127.0.0.1/internal\n").unwrap();
        std::fs::write(
            &fake,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n",
                argv_file.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mut out = Vec::new();
        let cancel = AtomicBool::new(false);
        run_encode(
            fake.to_str().unwrap(),
            &source,
            &mut out,
            128,
            false,
            &[],
            &cancel,
        )
        .expect("fake ffmpeg should exit successfully");

        let args = std::fs::read_to_string(&argv_file).unwrap();
        let args: Vec<&str> = args.lines().collect();
        let policy_at = args
            .iter()
            .position(|arg| *arg == "-protocol_whitelist")
            .expect("protocol policy must be present");
        let input_at = args
            .iter()
            .position(|arg| *arg == "-i")
            .expect("input argument must be present");
        assert_eq!(args[policy_at + 1], INPUT_PROTOCOL_WHITELIST);
        assert!(policy_at < input_at, "policy must precede input: {args:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn output_args_add_soxr_filter_only_when_available() {
        let with = build_output_args(128, true, &[]);
        assert!(
            filter_chain(&with).contains("resampler=soxr"),
            "soxr build should request the soxr resampler"
        );
        let without = build_output_args(128, false, &[]);
        assert!(
            !filter_chain(&without).contains("soxr"),
            "no-soxr build must not reference soxr"
        );
    }

    #[test]
    fn output_args_clamp_bitrate() {
        let args = build_output_args(9_000, false, &[]);
        assert!(
            args.windows(2)
                .any(|w| w[0] == "-b:a" && w[1] == format!("{MAX_BITRATE_KBPS}k")),
            "out-of-range bitrate must clamp to the allowed maximum"
        );
    }

    #[test]
    fn eq_filters_precede_the_resampler_at_source_rate() {
        let config = one_band_config(BandType::Peaking, 1_000.0, 6.0, 1.0);
        let args = build_output_args(256, true, &eq_filter_chain(&config));
        let chain = filter_chain(&args);
        let eq_at = chain.find("equalizer").expect("peaking -> equalizer");
        let resample_at = chain.find("aresample").expect("chain resamples to 48k");
        assert!(
            eq_at < resample_at,
            "EQ must run before the 48 kHz resample: {chain}"
        );
    }

    #[test]
    fn each_band_type_maps_to_its_rbj_biquad() {
        let cases = [
            (
                BandType::Peaking,
                "equalizer=f=1000.000000:t=q:w=1.500000:g=3.000000",
            ),
            (
                BandType::LowShelf,
                "bass=f=1000.000000:t=q:w=1.500000:g=3.000000",
            ),
            (
                BandType::HighShelf,
                "treble=f=1000.000000:t=q:w=1.500000:g=3.000000",
            ),
            (BandType::LowPass, "lowpass=f=1000.000000:t=q:w=1.500000"),
            (BandType::HighPass, "highpass=f=1000.000000:t=q:w=1.500000"),
            (BandType::Notch, "bandreject=f=1000.000000:t=q:w=1.500000"),
            (BandType::AllPass, "allpass=f=1000.000000:t=q:w=1.500000"),
        ];
        for (band_type, expected) in cases {
            let band = EqBand {
                enabled: true,
                band_type,
                freq_hz: 1_000.0,
                gain_db: 3.0,
                q: 1.5,
            };
            assert_eq!(band_filter(&band), expected, "mapping for {band_type:?}");
        }
    }

    #[test]
    fn preamp_and_only_enabled_bands_appear() {
        let mut config = one_band_config(BandType::Peaking, 1_000.0, 6.0, 1.0);
        config.preamp_db = -3.0;
        config.bands[0].enabled = false; // a disabled band must be skipped
        let filters = eq_filter_chain(&config);
        assert_eq!(filters[0], "volume=-3.000000dB", "preamp leads the chain");
        assert_eq!(
            filters.iter().filter(|f| !f.starts_with("volume")).count(),
            1,
            "only the single enabled band should be emitted"
        );
    }

    #[test]
    fn disabled_config_yields_no_eq_filters() {
        let mut config = one_band_config(BandType::Peaking, 1_000.0, 6.0, 1.0);
        config.enabled = false;
        assert!(eq_filter_chain(&config).is_empty());
    }

    #[test]
    fn missing_ffmpeg_binary_is_a_clear_error() {
        let err = probe_capabilities("fozmo-nonexistent-ffmpeg-binary")
            .expect_err("a missing binary must not probe as available");
        assert!(
            err.contains("not available"),
            "error should name the unavailable binary: {err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn ffmpeg_without_libopus_is_rejected() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("no-libopus");
        let fake = dir.join("ffmpeg");
        // A stand-in that lists encoders without libopus, mimicking a build
        // compiled without `--enable-libopus`.
        std::fs::write(
            &fake,
            "#!/bin/sh\necho ' A..... aac  AAC (Advanced Audio Coding)'\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

        let err = probe_capabilities(fake.to_str().unwrap())
            .expect_err("a build without libopus must be rejected");
        assert!(
            err.contains("libopus"),
            "error should mention libopus: {err}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn encodes_44_1k_wav_to_ogg_opus_when_ffmpeg_present() {
        if !ffmpeg_with_libopus() {
            eprintln!("skipping: ffmpeg/libopus not available on this host");
            return;
        }
        let dir = temp_dir("wav441");
        let source = dir.join("tone.wav");
        write_wav(&source, 44_100, 22_050);

        let mut out = Vec::new();
        let cancel = AtomicBool::new(false);
        encode_ogg_opus(&source, &mut out, 128, None, &cancel).unwrap();

        assert_valid_ogg_opus(&out);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn active_eq_changes_the_encoded_opus_when_ffmpeg_present() {
        if !ffmpeg_with_libopus() {
            eprintln!("skipping: ffmpeg/libopus not available on this host");
            return;
        }
        let dir = temp_dir("wav-eq");
        let source = dir.join("tone.wav");
        write_wav(&source, 48_000, 24_000);
        let cancel = AtomicBool::new(false);

        let mut plain = Vec::new();
        encode_ogg_opus(&source, &mut plain, 128, None, &cancel).unwrap();
        // A heavy low-shelf cut on a 440 Hz-ish tone must change the bytes.
        let eq = one_band_config(BandType::LowShelf, 500.0, -18.0, 0.7);
        let mut shaped = Vec::new();
        encode_ogg_opus(&source, &mut shaped, 128, Some(&eq), &cancel).unwrap();

        assert_valid_ogg_opus(&plain);
        assert_valid_ogg_opus(&shaped);
        assert_ne!(plain, shaped, "an active EQ band must change the audio");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn non_audio_source_fails_when_ffmpeg_present() {
        if !ffmpeg_with_libopus() {
            eprintln!("skipping: ffmpeg/libopus not available on this host");
            return;
        }
        let dir = temp_dir("junk");
        let source = dir.join("junk.wav");
        std::fs::write(&source, b"0123456789").unwrap();

        let mut out = Vec::new();
        let cancel = AtomicBool::new(false);
        let result = encode_ogg_opus(&source, &mut out, 128, None, &cancel);

        assert!(
            result.is_err(),
            "garbage input must surface an ffmpeg error"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn network_playlist_is_rejected_by_protocol_policy_when_ffmpeg_present() {
        if !ffmpeg_with_libopus() {
            eprintln!("skipping: ffmpeg/libopus not available on this host");
            return;
        }
        let dir = temp_dir("network-playlist");
        // Use the native extension here so this remains a protocol-policy test
        // across FFmpeg versions with stricter HLS extension detection.
        let source = dir.join("evil.m3u8");
        std::fs::write(
            &source,
            b"#EXTM3U\n#EXT-X-TARGETDURATION:10\n#EXTINF:10,\nhttp://127.0.0.1:9/internal.ts\n#EXT-X-ENDLIST\n",
        )
        .unwrap();

        let mut out = Vec::new();
        let cancel = AtomicBool::new(false);
        let error = run_encode(
            &ffmpeg_binary(),
            &source,
            &mut out,
            128,
            false,
            &[],
            &cancel,
        )
        .expect_err("a playlist requiring HTTP must be rejected");

        assert!(
            error.contains("not on whitelist") || error.contains("Protocol 'http' not allowed"),
            "FFmpeg must reject the nested HTTP resource via protocol policy: {error}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
