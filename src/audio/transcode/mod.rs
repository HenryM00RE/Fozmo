//! Bounded derivative cache for browser-playback transcodes of local tracks.
//!
//! Derivatives live under `library/transcode-cache` and are keyed by track id,
//! source path, source mtime/size, output format (container + bitrate), and
//! the EQ configuration baked into the audio, so replaying an unchanged track
//! never re-encodes. This is a temporary playback cache in the same spirit as
//! the Qobuz playback cache — not an export or download path.

mod decode;
mod ffmpeg_opus;
pub mod flac;
pub mod opus;

use crate::audio::eq::EqConfig;
use bytes::Bytes;
use futures_util::Stream;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::sync::watch;

/// Cache format version; bump to invalidate previously cached derivatives.
/// v3 moved Ogg Opus derivatives (with or without EQ) onto the FFmpeg/libopus
/// backend (issue #81), so previously cached Rust-Opus derivatives must be
/// discarded. v4 fixed finalized FLAC headers that declared a
/// variable-blocksize stream (unplayable in iOS Safari), so FLAC derivatives
/// cached with the inconsistent header must be re-encoded.
const CACHE_FORMAT_VERSION: u32 = 4;
/// Default derivative cache budget. At 160 kbps this is roughly 25 hours of
/// audio, plenty for repeated album playback without growing unbounded.
const DEFAULT_MAX_CACHE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// At most this many tracks encode concurrently; further requests queue.
const MAX_CONCURRENT_ENCODES: usize = 2;
const READ_CHUNK_SIZE: usize = 64 * 1024;

#[derive(Clone, Debug, Default)]
pub struct TranscodeProgress {
    /// Bumped whenever more derivative bytes are on disk.
    pub revision: u64,
    pub done: bool,
    pub error: Option<String>,
}

/// How a derivative can be served right now.
pub enum DerivativeStream {
    /// Fully cached; serve like a regular file (ranges work).
    Ready(PathBuf),
    /// Encoding in progress; tail the growing file guided by `progress`.
    Generating {
        path: PathBuf,
        progress: watch::Receiver<TranscodeProgress>,
    },
}

#[derive(Debug)]
pub enum TranscodeRequestError {
    SourceMissing,
    Failed(String),
}

/// Delivery format of a playback derivative.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DerivativeFormat {
    OggOpus { bitrate_kbps: u32 },
    Flac,
}

impl DerivativeFormat {
    fn extension(self) -> &'static str {
        match self {
            DerivativeFormat::OggOpus { .. } => "ogg",
            DerivativeFormat::Flac => "flac",
        }
    }

    pub fn content_type(self) -> &'static str {
        match self {
            DerivativeFormat::OggOpus { .. } => "audio/ogg",
            DerivativeFormat::Flac => "audio/flac",
        }
    }

    fn cache_label(self) -> String {
        match self {
            DerivativeFormat::OggOpus { bitrate_kbps } => format!("ogg-opus|{bitrate_kbps}"),
            DerivativeFormat::Flac => "flac|24".to_string(),
        }
    }
}

/// Cache-key discriminator for the EQ baked into a derivative. `None` (or a
/// disabled/inactive config) hashes to a stable "off" value so toggling EQ
/// away and back reuses earlier derivatives.
fn eq_cache_label(eq: Option<&EqConfig>) -> String {
    let Some(config) = eq else {
        return "eq-off".to_string();
    };
    let json = serde_json::to_string(config).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(json.as_bytes());
    let digest = hasher.finalize();
    let mut label = String::with_capacity(16);
    for byte in &digest[..8] {
        label.push_str(&format!("{byte:02x}"));
    }
    label
}

/// An EQ config only shapes the audio when enabled with at least one active
/// band; anything else is treated as "no EQ" for both encoding and caching.
pub fn active_eq(eq: Option<EqConfig>) -> Option<EqConfig> {
    eq.filter(|config| config.enabled && config.bands.iter().any(|band| band.enabled))
}

/// Serves Ogg Opus derivatives of local library tracks for browser playback.
pub struct LocalTranscodeService {
    cache_dir: PathBuf,
    jobs: Mutex<HashMap<String, watch::Receiver<TranscodeProgress>>>,
    encode_slots: Arc<tokio::sync::Semaphore>,
    max_cache_bytes: u64,
    encodes_started: AtomicU64,
}

impl LocalTranscodeService {
    pub fn new(cache_dir: PathBuf) -> Self {
        Self {
            cache_dir,
            jobs: Mutex::new(HashMap::new()),
            encode_slots: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_ENCODES)),
            max_cache_bytes: DEFAULT_MAX_CACHE_BYTES,
            encodes_started: AtomicU64::new(0),
        }
    }

    /// Number of encode jobs ever started; used by tests to assert cache hits.
    pub fn encodes_started(&self) -> u64 {
        self.encodes_started.load(Ordering::Relaxed)
    }

    /// Playback derivative for `source` in `format` with `eq` baked in,
    /// starting an encode job if the cache has no derivative for the source's
    /// current mtime/size, format, and EQ configuration.
    pub fn stream_derivative(
        self: &Arc<Self>,
        track_id: i64,
        source: &Path,
        format: DerivativeFormat,
        eq: Option<EqConfig>,
    ) -> Result<DerivativeStream, TranscodeRequestError> {
        let metadata = match std::fs::metadata(source) {
            Ok(metadata) => metadata,
            Err(_) => return Err(TranscodeRequestError::SourceMissing),
        };
        let eq = active_eq(eq);
        let key = derivative_key(track_id, source, &metadata, format, eq.as_ref());
        let data_path = self.cache_dir.join(format!("{key}.{}", format.extension()));
        let marker_path = self.cache_dir.join(format!("{key}.ok"));

        let mut jobs = self.jobs.lock().expect("transcode jobs lock");
        if let Some(progress) = jobs.get(&key) {
            return Ok(DerivativeStream::Generating {
                path: data_path,
                progress: progress.clone(),
            });
        }
        if marker_path.is_file() && data_path.is_file() {
            // Rewrite the marker so pruning treats this entry as recently used.
            let _ = std::fs::write(&marker_path, b"ok");
            return Ok(DerivativeStream::Ready(data_path));
        }

        std::fs::create_dir_all(&self.cache_dir)
            .map_err(|e| TranscodeRequestError::Failed(format!("create transcode cache: {e}")))?;
        let (tx, rx) = watch::channel(TranscodeProgress::default());
        jobs.insert(key.clone(), rx.clone());
        drop(jobs);
        self.encodes_started.fetch_add(1, Ordering::Relaxed);
        self.spawn_encode_job(
            key,
            source.to_path_buf(),
            data_path.clone(),
            marker_path,
            format,
            eq,
            tx,
        );
        Ok(DerivativeStream::Generating {
            path: data_path,
            progress: rx,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_encode_job(
        self: &Arc<Self>,
        key: String,
        source: PathBuf,
        data_path: PathBuf,
        marker_path: PathBuf,
        format: DerivativeFormat,
        eq: Option<EqConfig>,
        tx: watch::Sender<TranscodeProgress>,
    ) {
        let service = Arc::clone(self);
        tokio::spawn(async move {
            let _permit = service
                .encode_slots
                .clone()
                .acquire_owned()
                .await
                .expect("encode semaphore closed");
            let blocking_tx = tx.clone();
            let blocking_data_path = data_path.clone();
            // The browser dropping its connection does not cancel the job: the
            // encode is bounded by track length and the finished derivative is
            // exactly what a reconnect/seek/replay will ask for next. The flag
            // only fires on shutdown-style aborts of this task.
            let cancel = Arc::new(AtomicBool::new(false));
            let result = tokio::task::spawn_blocking(move || {
                let file = std::fs::File::create(&blocking_data_path)
                    .map_err(|e| format!("create derivative file: {e}"))?;
                let mut writer = ProgressFileWriter {
                    file,
                    tx: blocking_tx,
                };
                match format {
                    // Issue #81: Opus derivatives decode/resample/encode through
                    // FFmpeg/libopus from the native file, removing Fozmo's
                    // resampler from the local path. Zone EQ is baked in as
                    // FFmpeg biquad filters (see `ffmpeg_opus`).
                    DerivativeFormat::OggOpus { bitrate_kbps } => ffmpeg_opus::encode_ogg_opus(
                        &source,
                        &mut writer,
                        bitrate_kbps,
                        eq.as_ref(),
                        &cancel,
                    ),
                    DerivativeFormat::Flac => {
                        let header = flac::encode_flac(&source, &mut writer, eq.as_ref(), &cancel)?;
                        drop(writer);
                        // Patch the provisional header so fully cached
                        // derivatives report an accurate duration.
                        patch_file_prefix(&blocking_data_path, &header)
                    }
                }
            })
            .await
            .unwrap_or_else(|e| Err(format!("transcode task panicked: {e}")));

            let error = match &result {
                Ok(()) => {
                    if let Err(e) = std::fs::write(&marker_path, b"ok") {
                        Some(format!("write derivative marker: {e}"))
                    } else {
                        None
                    }
                }
                Err(e) => Some(e.clone()),
            };
            if error.is_some() {
                let _ = std::fs::remove_file(&data_path);
                let _ = std::fs::remove_file(&marker_path);
            }
            if let Some(error) = &error {
                tracing::warn!(
                    event = "local_opus_transcode_failed",
                    error = %crate::diagnostics::logging::sanitize_error(error),
                    "Local Opus derivative encode failed"
                );
            }
            service
                .jobs
                .lock()
                .expect("transcode jobs lock")
                .remove(&key);
            tx.send_modify(|progress| {
                progress.revision += 1;
                progress.done = true;
                progress.error = error;
            });
            service.prune_cache();
        });
    }

    /// Keep completed derivatives within the cache budget, oldest-marker
    /// first. In-flight jobs have no marker yet and are never pruned.
    fn prune_cache(&self) {
        let Ok(entries) = std::fs::read_dir(&self.cache_dir) else {
            return;
        };
        let mut completed: Vec<(PathBuf, PathBuf, std::time::SystemTime, u64)> = Vec::new();
        for entry in entries.flatten() {
            let marker = entry.path();
            if marker.extension().and_then(|ext| ext.to_str()) != Some("ok") {
                continue;
            }
            let Some(data) = ["ogg", "flac"]
                .iter()
                .map(|ext| marker.with_extension(ext))
                .find(|path| path.is_file())
            else {
                continue;
            };
            let (Ok(marker_meta), Ok(data_meta)) = (entry.metadata(), std::fs::metadata(&data))
            else {
                continue;
            };
            let used_at = marker_meta
                .modified()
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            completed.push((marker, data, used_at, data_meta.len()));
        }
        let mut total: u64 = completed.iter().map(|(_, _, _, len)| len).sum();
        if total <= self.max_cache_bytes {
            return;
        }
        completed.sort_by_key(|(_, _, used_at, _)| *used_at);
        for (marker, data, _, len) in completed {
            if total <= self.max_cache_bytes {
                break;
            }
            // Remove the marker first so a partially deleted entry is treated
            // as absent rather than complete.
            if std::fs::remove_file(&marker).is_ok() {
                let _ = std::fs::remove_file(&data);
                total = total.saturating_sub(len);
            }
        }
    }
}

struct ProgressFileWriter {
    file: std::fs::File,
    tx: watch::Sender<TranscodeProgress>,
}

impl std::io::Write for ProgressFileWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let written = std::io::Write::write(&mut self.file, buf)?;
        if written > 0 {
            self.tx.send_modify(|progress| progress.revision += 1);
        }
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        std::io::Write::flush(&mut self.file)
    }
}

/// Overwrite the first `prefix.len()` bytes of `path` in place.
fn patch_file_prefix(path: &Path, prefix: &[u8]) -> Result<(), String> {
    use std::io::{Seek, SeekFrom, Write};
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|e| format!("open derivative for header patch: {e}"))?;
    file.seek(SeekFrom::Start(0))
        .map_err(|e| format!("seek derivative for header patch: {e}"))?;
    file.write_all(prefix)
        .map_err(|e| format!("patch derivative header: {e}"))
}

fn derivative_key(
    track_id: i64,
    source: &Path,
    metadata: &std::fs::Metadata,
    format: DerivativeFormat,
    eq: Option<&EqConfig>,
) -> String {
    let mtime_ns = metadata
        .modified()
        .ok()
        .and_then(|mtime| mtime.duration_since(std::time::SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut hasher = Sha256::new();
    hasher.update(format!(
        "v{CACHE_FORMAT_VERSION}|{track_id}|{}|{mtime_ns}|{}|{}|{}",
        source.to_string_lossy(),
        metadata.len(),
        format.cache_label(),
        eq_cache_label(eq),
    ));
    let digest = hasher.finalize();
    let mut key = String::with_capacity(32);
    for byte in &digest[..16] {
        key.push_str(&format!("{byte:02x}"));
    }
    key
}

/// Body stream that tails a derivative file while (and after) it is encoded.
/// Yields chunks as the encoder appends them and ends when the job reports
/// completion; an encode failure surfaces as a stream error, which aborts the
/// HTTP response body so the browser does not treat a truncated derivative as
/// a complete track.
pub fn progressive_derivative_stream(
    path: PathBuf,
    progress: watch::Receiver<TranscodeProgress>,
) -> impl Stream<Item = Result<Bytes, std::io::Error>> {
    struct TailState {
        path: PathBuf,
        file: Option<tokio::fs::File>,
        offset: u64,
        progress: watch::Receiver<TranscodeProgress>,
    }

    futures_util::stream::unfold(
        TailState {
            path,
            file: None,
            offset: 0,
            progress,
        },
        |mut state| async move {
            loop {
                let snapshot = state.progress.borrow().clone();
                if state.file.is_none() {
                    match tokio::fs::File::open(&state.path).await {
                        Ok(mut file) => {
                            if state.offset > 0
                                && let Err(e) =
                                    file.seek(std::io::SeekFrom::Start(state.offset)).await
                            {
                                return Some((Err(e), state));
                            }
                            state.file = Some(file);
                        }
                        Err(e) => {
                            if let Some(error) = snapshot.error {
                                return Some((Err(std::io::Error::other(error)), state));
                            }
                            if snapshot.done {
                                return Some((Err(e), state));
                            }
                            if state.progress.changed().await.is_err() {
                                return Some((
                                    Err(std::io::Error::other("transcode job ended unexpectedly")),
                                    state,
                                ));
                            }
                            continue;
                        }
                    }
                }
                let file = state.file.as_mut().expect("derivative file open");
                let mut chunk = vec![0_u8; READ_CHUNK_SIZE];
                match file.read(&mut chunk).await {
                    Ok(0) => {
                        if let Some(error) = snapshot.error {
                            return Some((Err(std::io::Error::other(error)), state));
                        }
                        if snapshot.done {
                            return None;
                        }
                        // Wait for the encoder to append more bytes or reach a
                        // terminal state. The job always sends `done` before
                        // its sender drops, so an error here means the job was
                        // aborted mid-encode: surface it as a truncation.
                        if state.progress.changed().await.is_err() {
                            return Some((
                                Err(std::io::Error::other("transcode job ended unexpectedly")),
                                state,
                            ));
                        }
                    }
                    Ok(n) => {
                        chunk.truncate(n);
                        state.offset += n as u64;
                        return Some((Ok(Bytes::from(chunk)), state));
                    }
                    Err(e) => return Some((Err(e), state)),
                }
            }
        },
    )
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::f64::consts::TAU;
    use std::path::Path;

    /// Minimal 16-bit stereo PCM WAV with a quiet sine tone.
    pub(crate) fn write_wav(path: &Path, sample_rate: u32, frames: usize) {
        let mut data = Vec::with_capacity(frames * 4);
        for i in 0..frames {
            let sample = ((TAU * 440.0 * i as f64 / sample_rate as f64).sin() * 8192.0) as i16;
            data.extend_from_slice(&sample.to_le_bytes());
            data.extend_from_slice(&sample.to_le_bytes());
        }
        let mut wav = Vec::with_capacity(44 + data.len());
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16_u32.to_le_bytes());
        wav.extend_from_slice(&1_u16.to_le_bytes()); // PCM
        wav.extend_from_slice(&2_u16.to_le_bytes()); // stereo
        wav.extend_from_slice(&sample_rate.to_le_bytes());
        wav.extend_from_slice(&(sample_rate * 4).to_le_bytes());
        wav.extend_from_slice(&4_u16.to_le_bytes());
        wav.extend_from_slice(&16_u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(data.len() as u32).to_le_bytes());
        wav.extend_from_slice(&data);
        std::fs::write(path, wav).unwrap();
    }
}

#[cfg(test)]
mod tests;
