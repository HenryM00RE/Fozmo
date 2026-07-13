use super::test_support::write_wav;
use super::*;
use futures_util::StreamExt;

fn temp_dir(prefix: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "fozmo-transcode-{prefix}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn default_opus_format() -> DerivativeFormat {
    DerivativeFormat::OggOpus {
        bitrate_kbps: opus::DEFAULT_BITRATE_KBPS,
    }
}

/// An EQ config that audibly shapes the signal (one active band).
fn active_eq_config() -> EqConfig {
    let mut config = EqConfig {
        enabled: true,
        ..EqConfig::default()
    };
    config.bands[4].enabled = true;
    config.bands[4].gain_db = -6.0;
    config
}

fn assert_valid_flac(bytes: &[u8]) {
    assert!(bytes.len() >= 42, "derivative should not be empty");
    assert_eq!(&bytes[..4], b"fLaC", "stream must start with FLAC magic");
    // Metadata block header byte: last-block flag (0x80) + type 0 = STREAMINFO.
    assert_eq!(
        bytes[4] & 0x7f,
        0,
        "first metadata block must be STREAMINFO"
    );
}

/// Min/max block size are the first two u16s of STREAMINFO (offset by 8 for
/// the magic + metadata block header).
fn flac_block_sizes(bytes: &[u8]) -> (u16, u16) {
    let b = &bytes[8..];
    (
        u16::from_be_bytes([b[0], b[1]]),
        u16::from_be_bytes([b[2], b[3]]),
    )
}

/// Total samples live in the low 4 bits of STREAMINFO byte 13 plus bytes
/// 14..18 (offset by 8 for the magic + metadata block header).
fn flac_total_samples(bytes: &[u8]) -> u64 {
    let b = &bytes[8..];
    ((b[13] as u64 & 0x0f) << 32)
        | ((b[14] as u64) << 24)
        | ((b[15] as u64) << 16)
        | ((b[16] as u64) << 8)
        | (b[17] as u64)
}

fn assert_valid_ogg_opus(bytes: &[u8]) {
    assert!(bytes.len() > 100, "derivative should not be empty");
    assert_eq!(&bytes[..4], b"OggS", "stream must start with an Ogg page");
    let haystack = |needle: &[u8]| bytes.windows(needle.len()).any(|w| w == needle);
    assert!(haystack(b"OpusHead"), "missing Opus identification header");
    assert!(haystack(b"OpusTags"), "missing Opus comment header");
}

#[test]
fn encodes_wav_to_flac_with_patched_total_samples() {
    let dir = temp_dir("flac");
    let source = dir.join("tone.wav");
    write_wav(&source, 44_100, 12_000);

    let mut out = Vec::new();
    let cancel = Arc::new(AtomicBool::new(false));
    let header = flac::encode_flac(&source, &mut out, None, &cancel).unwrap();

    assert_valid_flac(&out);
    assert_eq!(
        flac_total_samples(&out),
        0,
        "progressive header is unknown-length"
    );
    assert_valid_flac(&header);
    assert_eq!(header.len(), 42, "finalized header must patch in place");
    assert_eq!(flac_total_samples(&header), 12_000);
    let _ = std::fs::remove_dir_all(dir);
}

/// The tail frame is shorter than the fixed block size, which must not leak
/// into the finalized STREAMINFO: min == max declares the fixed-blocksize
/// stream the frames actually use. A min/max mismatch makes Apple's CoreMedia
/// demuxer (iOS Safari) fail to packetize the file, so EQ'd streams spin
/// forever on iPhone/iPad while lenient desktop demuxers play them fine.
#[test]
fn finalized_flac_header_declares_a_fixed_blocksize_stream() {
    let dir = temp_dir("flac-fixed-blocksize");
    let source = dir.join("tone.wav");
    // 12_000 frames = 2 full 4096 blocks + a 3808-frame tail.
    write_wav(&source, 44_100, 12_000);

    let mut out = Vec::new();
    let cancel = Arc::new(AtomicBool::new(false));
    let header = flac::encode_flac(&source, &mut out, None, &cancel).unwrap();

    assert_eq!(
        flac_block_sizes(&header),
        (4096, 4096),
        "short tail frame must not turn the header variable-blocksize"
    );
    assert_eq!(flac_block_sizes(&out), (4096, 4096));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn encodes_flac_tails_shorter_than_the_encoder_minimum_block() {
    let dir = temp_dir("flac-short-tail");
    let cancel = Arc::new(AtomicBool::new(false));
    // Tails of 1..32 frames land below flacenc's minimum block size (32) and
    // must be zero-padded instead of failing the whole encode.
    for tail in [1_usize, 16, 20, 31] {
        let source = dir.join(format!("tone-{tail}.wav"));
        write_wav(&source, 44_100, 4_096 + tail);

        let mut out = Vec::new();
        let header = flac::encode_flac(&source, &mut out, None, &cancel)
            .unwrap_or_else(|e| panic!("tail of {tail} frames must encode: {e}"));

        assert_valid_flac(&out);
        // The padded tail may round the total up to the pad boundary, but
        // never below the real frame count.
        assert!(flac_total_samples(&header) >= (4_096 + tail) as u64);
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn eq_changes_the_encoded_flac_audio() {
    let dir = temp_dir("flac-eq");
    let source = dir.join("tone.wav");
    write_wav(&source, 44_100, 12_000);
    let cancel = Arc::new(AtomicBool::new(false));

    let mut plain = Vec::new();
    flac::encode_flac(&source, &mut plain, None, &cancel).unwrap();
    let mut eqd = Vec::new();
    flac::encode_flac(&source, &mut eqd, Some(&active_eq_config()), &cancel).unwrap();

    assert_valid_flac(&eqd);
    assert_ne!(plain, eqd, "an active EQ band must change the audio");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn eq_partitions_the_derivative_cache() {
    let dir = temp_dir("cache-eq");
    let source = dir.join("tone.wav");
    write_wav(&source, opus::OPUS_SAMPLE_RATE, 9_600);
    let service = Arc::new(LocalTranscodeService::new(dir.join("cache")));

    // FLAC keeps this self-contained; the Opus/FFmpeg path (including EQ) is
    // covered by the FFmpeg backend's own tests and the key test below.
    let flac_plain = service
        .stream_derivative(7, &source, DerivativeFormat::Flac, None)
        .unwrap();
    let DerivativeStream::Generating {
        path: flac_path,
        progress,
    } = flac_plain
    else {
        panic!("first request must start an encode job");
    };
    assert_eq!(wait_for_done(progress).await.error, None);

    // Same format with an inactive EQ config reuses the EQ-free derivative.
    let mut inactive = active_eq_config();
    inactive.enabled = false;
    let reused = service
        .stream_derivative(7, &source, DerivativeFormat::Flac, Some(inactive))
        .unwrap();
    assert!(matches!(reused, DerivativeStream::Ready(path) if path == flac_path));
    assert_eq!(service.encodes_started(), 1);

    // Active EQ gets its own cache entry.
    let with_eq = service
        .stream_derivative(7, &source, DerivativeFormat::Flac, Some(active_eq_config()))
        .unwrap();
    let DerivativeStream::Generating { progress, .. } = with_eq else {
        panic!("active EQ must start a fresh encode");
    };
    assert_eq!(wait_for_done(progress).await.error, None);
    assert_eq!(service.encodes_started(), 2);

    let cached_flac = std::fs::read(&flac_path).unwrap();
    assert_valid_flac(&cached_flac);
    assert!(
        flac_total_samples(&cached_flac) > 0,
        "cached FLAC derivative must carry the patched duration"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// Format and EQ both partition the cache key without needing an encoder,
/// so this stays deterministic whether or not FFmpeg is installed.
#[test]
fn format_and_eq_partition_derivative_keys() {
    let dir = temp_dir("keys");
    let source = dir.join("tone.wav");
    write_wav(&source, opus::OPUS_SAMPLE_RATE, 1_024);
    let metadata = std::fs::metadata(&source).unwrap();

    let eq = active_eq_config();
    let opus_off = derivative_key(7, &source, &metadata, default_opus_format(), None);
    let opus_eq = derivative_key(7, &source, &metadata, default_opus_format(), Some(&eq));
    let flac_off = derivative_key(7, &source, &metadata, DerivativeFormat::Flac, None);
    let flac_eq = derivative_key(7, &source, &metadata, DerivativeFormat::Flac, Some(&eq));

    let keys = [&opus_off, &opus_eq, &flac_off, &flac_eq];
    for (i, a) in keys.iter().enumerate() {
        for b in keys.iter().skip(i + 1) {
            assert_ne!(a, b, "format/EQ combinations must not share a cache key");
        }
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn encodes_48k_wav_to_ogg_opus() {
    let dir = temp_dir("wav48");
    let source = dir.join("tone.wav");
    write_wav(&source, opus::OPUS_SAMPLE_RATE, 24_000);

    let mut out = Vec::new();
    let cancel = Arc::new(AtomicBool::new(false));
    opus::encode_ogg_opus(&source, &mut out, opus::DEFAULT_BITRATE_KBPS, None, &cancel).unwrap();

    assert_valid_ogg_opus(&out);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn encodes_44_1k_wav_through_the_resampler_bridge() {
    let dir = temp_dir("wav441");
    let source = dir.join("tone.wav");
    write_wav(&source, 44_100, 11_025);

    let mut out = Vec::new();
    let cancel = Arc::new(AtomicBool::new(false));
    opus::encode_ogg_opus(&source, &mut out, 128, None, &cancel).unwrap();

    assert_valid_ogg_opus(&out);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn rejects_non_audio_sources() {
    let dir = temp_dir("junk");
    let source = dir.join("junk.wav");
    std::fs::write(&source, b"0123456789").unwrap();

    let mut out = Vec::new();
    let cancel = Arc::new(AtomicBool::new(false));
    let result =
        opus::encode_ogg_opus(&source, &mut out, opus::DEFAULT_BITRATE_KBPS, None, &cancel);

    assert!(result.is_err());
    let _ = std::fs::remove_dir_all(dir);
}

async fn wait_for_done(mut progress: watch::Receiver<TranscodeProgress>) -> TranscodeProgress {
    loop {
        let snapshot = progress.borrow().clone();
        if snapshot.done {
            return snapshot;
        }
        progress
            .changed()
            .await
            .expect("job sends done before drop");
    }
}

#[tokio::test]
async fn derivative_cache_is_reused_for_unchanged_sources() {
    let dir = temp_dir("cache-reuse");
    let source = dir.join("tone.wav");
    write_wav(&source, opus::OPUS_SAMPLE_RATE, 9_600);
    let service = Arc::new(LocalTranscodeService::new(dir.join("cache")));

    // FLAC keeps this cache-plumbing test independent of an external FFmpeg.
    let first = service
        .stream_derivative(7, &source, DerivativeFormat::Flac, None)
        .unwrap();
    let DerivativeStream::Generating { path, progress } = first else {
        panic!("first request must start an encode job");
    };
    let done = wait_for_done(progress).await;
    assert_eq!(done.error, None);
    assert_valid_flac(&std::fs::read(&path).unwrap());

    let second = service
        .stream_derivative(7, &source, DerivativeFormat::Flac, None)
        .unwrap();
    let DerivativeStream::Ready(cached) = second else {
        panic!("second request must hit the derivative cache");
    };
    assert_eq!(cached, path);
    assert_eq!(service.encodes_started(), 1);

    // Touching the source invalidates the cached derivative.
    write_wav(&source, opus::OPUS_SAMPLE_RATE, 4_800);
    let third = service
        .stream_derivative(7, &source, DerivativeFormat::Flac, None)
        .unwrap();
    assert!(matches!(third, DerivativeStream::Generating { .. }));
    assert_eq!(service.encodes_started(), 2);

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn progressive_stream_delivers_the_full_derivative() {
    let dir = temp_dir("progressive");
    let source = dir.join("tone.wav");
    write_wav(&source, opus::OPUS_SAMPLE_RATE, 9_600);
    let service = Arc::new(LocalTranscodeService::new(dir.join("cache")));

    // Progressive tailing is format-agnostic; FLAC avoids the FFmpeg dependency.
    let DerivativeStream::Generating { path, progress } = service
        .stream_derivative(3, &source, DerivativeFormat::Flac, None)
        .unwrap()
    else {
        panic!("first request must start an encode job");
    };
    let mut body = Vec::new();
    let mut stream = std::pin::pin!(progressive_derivative_stream(path.clone(), progress));
    while let Some(chunk) = stream.next().await {
        body.extend_from_slice(&chunk.expect("derivative stream chunk"));
    }

    let file = std::fs::read(&path).unwrap();
    assert_valid_flac(&body);
    assert_eq!(
        body.len(),
        file.len(),
        "progressive stream must deliver every byte"
    );
    // The finalized file patches the 42-byte STREAMINFO header in place with
    // the real duration, so only that header differs from the tailed body.
    assert_eq!(
        body[42..],
        file[42..],
        "progressive audio frames must match the cached file"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn missing_sources_are_reported_without_starting_jobs() {
    let dir = temp_dir("missing");
    let service = Arc::new(LocalTranscodeService::new(dir.join("cache")));

    let result = service.stream_derivative(1, &dir.join("nope.flac"), default_opus_format(), None);
    assert!(matches!(result, Err(TranscodeRequestError::SourceMissing)));
    assert_eq!(service.encodes_started(), 0);
    let _ = std::fs::remove_dir_all(dir);
}
