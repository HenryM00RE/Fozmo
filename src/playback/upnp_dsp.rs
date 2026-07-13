use crate::app::state::AppState;
use crate::audio::dsd::delta_sigma::DsdModulator;
use crate::audio::dsd::dop::{DopIdlePattern, DopMarkerStamper};
use crate::audio::dsd::dsd_render::{DsdRate, DsdRenderer};
use crate::audio::dsd::native_dsd::NativeDsdOrder;
use crate::audio::dsp::dither::{DitherMode, DitherPreference, DitherState, quantize_signed_pcm};
use crate::audio::dsp::eq::EqProcessor;
use crate::audio::dsp::resampler::{DEFAULT_FILTER_TYPE, FilterType, SincResampler};
use crate::audio::output::device_caps::auto_target_rate;
use crate::audio::player::{TrackCover, TrackTags};
use crate::audio::upnp::{UpnpGeneratedDspStream, UpnpRendererTarget, UpnpSource};
use crate::playback::upnp::qobuz_format_id_for_upnp_target;
use crate::protocol::{
    CapabilityDetectionSource, CapabilityDetectionStatus, PlaybackConfig, SourceRef,
    UpnpPcmContainer,
};
use crate::services::qobuz::QobuzPlayRequest;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use bytes::Bytes;
use flacenc::bitsink::ByteSink;
use flacenc::component::BitRepr;
use flacenc::config;
use flacenc::error::Verify;
use flacenc::source::MemSource;
use futures_util::Stream;
use rand::{RngCore, rngs::OsRng};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::File;
use std::io::{Error as IoError, ErrorKind};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use symphonia::core::codecs::{CODEC_TYPE_NULL, DecoderOptions};
use symphonia::core::formats::{FormatOptions, SeekMode, SeekTo};
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::{Time, TimeBase};
use tracing::{debug, warn};

const DEFAULT_RENDERED_PCM_BITS: u32 = 24;
// Fixed seed so cached/generated 16-bit renders are reproducible across runs.
const UPNP_DITHER_SEED: u64 = 0x5eed_0026_16b1_7d17;
const DSF_BLOCK_SIZE_PER_CHANNEL: usize = 4096;
const UPNP_RENDER_CACHE_SCHEMA: &str = "upnp-render-v3";
const GENERATED_WAV_SEEK_PREROLL_SECS: f64 = 0.5;
const GENERATED_WAV_NEAR_START_SEEK_FALLBACK_SECS: f64 = 2.0;
const GENERATED_PCM_STREAM_CHANNEL_CAPACITY: usize = 8;
const GENERATED_DOP_STREAM_CHANNEL_CAPACITY: usize = 64;
const DOP_WAV_LEAD_IN_MS: u64 = 150;
const DOP_WAV_CHANNELS: usize = 2;
const DOP_WAV_SAMPLE_BYTES: u64 = 3;
const DOP_WAV_FRAME_BYTES: u64 = DOP_WAV_SAMPLE_BYTES * DOP_WAV_CHANNELS as u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UpnpDspStreamingPolicy {
    Auto,
    ForceCompletedRender,
}

pub(crate) async fn rendered_upnp_source_if_needed(
    state: &AppState,
    zone_id: &str,
    source_ref: &SourceRef,
    target: &UpnpRendererTarget,
    config: &PlaybackConfig,
    streaming_policy: UpnpDspStreamingPolicy,
) -> Result<UpnpDspDecision, String> {
    let filter_type = FilterType::from_name(&config.filter_type).unwrap_or(DEFAULT_FILTER_TYPE);
    let render_config = UpnpDspRenderConfig {
        filter_type,
        target_rate: config.target_rate,
        target_bit_depth: normalize_target_bit_depth(config.target_bit_depth),
        upsampling_enabled: config.upsampling_enabled,
        output_mode: output_mode_for_upnp_config(config),
        dsd_modulator: DsdModulator::from_name(&config.dsd_modulator).unwrap_or_default(),
        dsd_isi_penalty: config.dsd_isi_penalty,
        dsd_rules: config.dsd_rules.clone(),
        headroom_db: config.headroom_db,
        eq: config.eq.clone(),
        dither_mode: DitherPreference::from_name(&config.dither_mode)
            .unwrap_or(DitherPreference::Auto),
        target: target.clone(),
    };
    // Renderer compatibility must not turn ordinary passthrough playback into
    // a full-track render. KEF's completed WAV policy applies only after DSP
    // processing has made rendering necessary.
    if upnp_dsp_inactive(config, &render_config) {
        return Ok(UpnpDspDecision {
            rendered: None,
            render_signature: render_signature_for_plan(
                source_ref,
                0,
                0,
                0,
                normalize_target_bit_depth(config.target_bit_depth),
                "passthrough",
                &render_config,
            ),
            render_ms: None,
            cache_hit: None,
            render_or_stream_plan: Some("passthrough".to_string()),
            cache_lookup_ms: None,
            cache_wait_ms: None,
            active_output_mode: OutputModeForUpnp::Pcm.as_name().to_string(),
            source_rate: 0,
            source_bits: 0,
            output_rate: 0,
            output_bits: 0,
        });
    }
    let source_meta = source_render_metadata(state, zone_id, source_ref, target).await;
    let Some((source_rate, source_bits)) = source_meta else {
        return Ok(UpnpDspDecision {
            rendered: None,
            render_signature: render_signature_for_plan(
                source_ref,
                0,
                0,
                0,
                normalize_target_bit_depth(config.target_bit_depth),
                "unknown",
                &render_config,
            ),
            render_ms: None,
            cache_hit: None,
            render_or_stream_plan: Some("unknown_source_passthrough".to_string()),
            cache_lookup_ms: None,
            cache_wait_ms: None,
            active_output_mode: OutputModeForUpnp::Pcm.as_name().to_string(),
            source_rate: 0,
            source_bits: 0,
            output_rate: 0,
            output_bits: 0,
        });
    };
    let plan = render_plan_for_source(&render_config, source_ref, source_rate, source_bits);
    if !plan.render_needed {
        return Ok(UpnpDspDecision {
            rendered: None,
            render_signature: plan.signature,
            render_ms: None,
            cache_hit: None,
            render_or_stream_plan: Some("passthrough".to_string()),
            cache_lookup_ms: None,
            cache_wait_ms: None,
            active_output_mode: plan.active_output_mode.as_name().to_string(),
            source_rate,
            source_bits,
            output_rate: source_rate,
            output_bits: source_bits,
        });
    }

    let cache_probe_started = std::time::Instant::now();
    let cache_dir = rendered_cache_dir()?;
    if let Some(decision) = completed_local_render_cache_decision(
        state,
        source_ref,
        &render_config,
        &plan,
        source_rate,
        source_bits,
        &cache_dir,
    )
    .await?
    {
        debug!(
            event = "upnp_dsp_completed_cache_hit",
            zone_id = %zone_id,
            source_rate,
            source_bits,
            output_rate = decision.output_rate,
            output_bits = decision.output_bits,
            cache_lookup_ms = ?decision.cache_lookup_ms,
            "Selected completed UPnP DSP cache without decode"
        );
        return Ok(decision);
    }
    let cache_lookup_ms = Some(elapsed_ms(cache_probe_started));

    if streaming_policy == UpnpDspStreamingPolicy::Auto
        && let Some(progressive_plan) = progressive_wav_plan_for_source(
            &render_config,
            source_ref,
            &plan,
            source_rate,
            source_bits,
        )
        && let Some(mut decision) = generated_wav_decision_for_plan(
            zone_id,
            source_ref,
            target,
            config,
            &progressive_plan,
            source_rate,
            source_bits,
        )
    {
        decision.cache_lookup_ms = cache_lookup_ms;
        debug!(
            event = "upnp_dsp_generated_stream_selected",
            zone_id = %zone_id,
            source_rate,
            source_bits,
            output_rate = decision.output_rate,
            output_bits = decision.output_bits,
            "Selected generated UPnP DSP WAV stream"
        );
        return Ok(decision);
    }

    let request = source_request(
        state,
        zone_id,
        source_ref,
        target,
        Some((source_rate, source_bits)),
    )
    .await?;
    let cache_key = rendered_cache_key(&request.cache_key, &render_config, &plan);
    let render_signature = plan.signature.clone();
    let active_output_mode = plan.active_output_mode.as_name().to_string();
    let render_started = std::time::Instant::now();
    debug!(
        event = "upnp_dsp_eager_render_start",
        zone_id = %zone_id,
        source_rate,
        source_bits,
        output_rate = plan.output_rate,
        output_bits = plan.output_bits,
        container = %plan.container,
        "Starting eager UPnP DSP render"
    );
    let rendered = tokio::task::spawn_blocking(move || {
        render_upnp_source_blocking(cache_dir, request, render_config, cache_key, plan)
    })
    .await
    .map_err(|e| format!("join UPnP DSP render task: {e}"))??;
    let render_ms = render_started
        .elapsed()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64;

    debug!(
        event = "upnp_dsp_rendered",
        path = %rendered.path.display(),
        source_rate = rendered.source_rate,
        source_bits = rendered.source_bits,
        output_rate = rendered.output_rate,
        output_bits = rendered.output_bits,
        output_kind = %rendered.output_kind,
        "Rendered UPnP DSP asset"
    );

    Ok(UpnpDspDecision {
        rendered: Some(UpnpSource::LocalFile {
            source_ref: rendered.source_ref,
            path: rendered.path,
            tags: rendered.tags,
            cover: rendered.cover,
            byte_len: rendered.byte_len,
            source_rate: rendered.output_rate,
            source_bits: rendered.output_bits,
        }),
        render_signature,
        render_ms: Some(render_ms),
        cache_hit: Some(rendered.cache_hit),
        render_or_stream_plan: Some("eager_render".to_string()),
        cache_lookup_ms,
        cache_wait_ms: Some(rendered.cache_wait_ms),
        active_output_mode,
        source_rate: rendered.source_rate,
        source_bits: rendered.source_bits,
        output_rate: rendered.output_rate,
        output_bits: rendered.output_bits,
    })
}

fn upnp_dsp_inactive(config: &PlaybackConfig, render_config: &UpnpDspRenderConfig) -> bool {
    !config.upsampling_enabled
        && !render_config.output_mode.is_dsd()
        && !config.eq.enabled
        && (processing_gain(render_config) - 1.0).abs() <= f64::EPSILON
}

pub(crate) struct UpnpDspDecision {
    pub rendered: Option<UpnpSource>,
    pub render_signature: String,
    pub render_ms: Option<u64>,
    pub cache_hit: Option<bool>,
    pub render_or_stream_plan: Option<String>,
    pub cache_lookup_ms: Option<u64>,
    pub cache_wait_ms: Option<u64>,
    pub active_output_mode: String,
    pub source_rate: u32,
    pub source_bits: u32,
    pub output_rate: u32,
    pub output_bits: u32,
}

pub(crate) async fn generated_upnp_dsp_wav_stream(
    state: AppState,
    stream: UpnpGeneratedDspStream,
    byte_start: u64,
    byte_end: u64,
) -> Result<impl Stream<Item = Result<Bytes, IoError>>, String> {
    if stream.mime_type != UpnpPcmContainer::Wav.mime_type() {
        return Err("Generated UPnP DSP streaming only supports WAV PCM".to_string());
    }
    let byte_len = stream
        .byte_len
        .ok_or_else(|| "Generated UPnP WAV stream is missing byte length".to_string())?;
    let range = GeneratedByteRange::validate(byte_start, byte_end, byte_len)?;
    let filter_type =
        FilterType::from_name(&stream.playback_config.filter_type).unwrap_or(DEFAULT_FILTER_TYPE);
    let render_config = UpnpDspRenderConfig {
        filter_type,
        target_rate: stream.target_rate,
        target_bit_depth: stream.target_bits,
        upsampling_enabled: stream.playback_config.upsampling_enabled,
        output_mode: output_mode_for_upnp_config(&stream.playback_config),
        dsd_modulator: DsdModulator::from_name(&stream.playback_config.dsd_modulator)
            .unwrap_or_default(),
        dsd_isi_penalty: stream.playback_config.dsd_isi_penalty,
        dsd_rules: stream.playback_config.dsd_rules.clone(),
        headroom_db: stream.playback_config.headroom_db,
        eq: stream.playback_config.eq.clone(),
        dither_mode: DitherPreference::from_name(&stream.playback_config.dither_mode)
            .unwrap_or(DitherPreference::Auto),
        target: stream.target.clone(),
    };
    let channel_capacity = if generated_stream_is_dop_bound(&stream) {
        GENERATED_DOP_STREAM_CHANNEL_CAPACITY
    } else {
        GENERATED_PCM_STREAM_CHANNEL_CAPACITY
    };
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, IoError>>(channel_capacity);
    let (preflight_tx, preflight_rx) = mpsc::channel::<Result<(), String>>();
    tokio::spawn(async move {
        let mut preflight_tx = Some(preflight_tx);
        let request = match source_request(
            &state,
            &stream.zone_id,
            &stream.source_ref,
            &stream.target,
            Some((stream.source_rate, stream.source_bits)),
        )
        .await
        {
            Ok(request) => request,
            Err(error) => {
                if let Some(preflight_tx) = preflight_tx.take() {
                    let _ = preflight_tx.send(Err(error.clone()));
                }
                let _ = tx.send(Err(IoError::other(error))).await;
                return;
            }
        };
        tokio::task::spawn_blocking(move || {
            if let Err(error) = stream_wav_dsp_blocking(
                request,
                render_config,
                &stream,
                range,
                tx.clone(),
                &mut preflight_tx,
            ) {
                if let Some(preflight_tx) = preflight_tx.take() {
                    let _ = preflight_tx.send(Err(error.clone()));
                }
                let _ = tx.blocking_send(Err(IoError::other(error)));
            }
        });
    });
    let preflight_result = tokio::task::spawn_blocking(move || preflight_rx.recv())
        .await
        .map_err(|e| format!("join generated UPnP WAV preflight: {e}"))?;
    match preflight_result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => return Err(error),
        Err(_) => {
            return Err("Generated UPnP WAV stream preflight did not report readiness".to_string());
        }
    }
    Ok(futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    }))
}

fn generated_stream_is_dop_bound(stream: &UpnpGeneratedDspStream) -> bool {
    stream.dop_lead_in_data_len > 0
        || matches!(
            stream.active_output_mode.as_deref(),
            Some("Dsd64" | "Dsd128" | "Dsd256")
        )
}

#[derive(Clone)]
struct UpnpDspRenderConfig {
    filter_type: FilterType,
    target_rate: u32,
    target_bit_depth: u32,
    upsampling_enabled: bool,
    output_mode: OutputModeForUpnp,
    dsd_modulator: DsdModulator,
    dsd_isi_penalty: f32,
    dsd_rules: Vec<crate::settings::DsdSourceRule>,
    headroom_db: f32,
    eq: crate::audio::eq::EqConfig,
    dither_mode: DitherPreference,
    target: UpnpRendererTarget,
}

/// Dither for UPnP PCM quantization. Shaping is scoped to endpoints that are
/// actually forced to 16-bit; 24/32-bit rendering stays undithered as before.
fn upnp_pcm_dither_mode(preference: DitherPreference, output_bits: u32) -> DitherMode {
    if output_bits != 16 {
        return DitherMode::Off;
    }
    match preference {
        DitherPreference::Off => DitherMode::Off,
        DitherPreference::Auto => DitherMode::Shaped16,
        DitherPreference::Tpdf => DitherMode::Tpdf,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GeneratedByteRange {
    start: u64,
    end: u64,
}

impl GeneratedByteRange {
    fn validate(start: u64, end: u64, byte_len: u64) -> Result<Self, String> {
        if byte_len == 0 || start > end || end >= byte_len {
            return Err("Generated UPnP DSP stream byte range is not satisfiable".to_string());
        }
        Ok(Self { start, end })
    }

    fn len(self) -> u64 {
        self.end - self.start + 1
    }
}

struct SourceRequest {
    source_ref: SourceRef,
    source: Box<dyn MediaSource>,
    ext_hint: Option<String>,
    tags: TrackTags,
    cover: Option<TrackCover>,
    cache_key: String,
    source_rate: u32,
    source_bits: u32,
}

struct SourceMetadata {
    source_ref: SourceRef,
    tags: TrackTags,
    cover: Option<TrackCover>,
}

struct RenderedUpnpAsset {
    source_ref: SourceRef,
    path: PathBuf,
    tags: TrackTags,
    cover: Option<TrackCover>,
    byte_len: Option<u64>,
    source_rate: u32,
    source_bits: u32,
    output_rate: u32,
    output_bits: u32,
    output_kind: String,
    cache_hit: bool,
    cache_wait_ms: u64,
}

#[derive(Clone)]
struct UpnpRenderPlan {
    render_needed: bool,
    signature: String,
    output_rate: u32,
    output_bits: u32,
    active_output_mode: OutputModeForUpnp,
    container: String,
}

fn generated_wav_decision_for_plan(
    zone_id: &str,
    source_ref: &SourceRef,
    target: &UpnpRendererTarget,
    config: &PlaybackConfig,
    plan: &UpnpRenderPlan,
    source_rate: u32,
    source_bits: u32,
) -> Option<UpnpDspDecision> {
    if !generated_wav_streaming_enabled_for_upnp() {
        return None;
    }
    if !plan.render_needed || plan.container != UpnpPcmContainer::Wav.as_str() {
        return None;
    }
    let (tags, byte_len, dop_lead_in_data_len) = generated_wav_metadata_for_source(
        source_ref,
        plan.output_rate,
        plan.output_bits,
        plan.active_output_mode,
    )?;
    let id = generated_dsp_asset_id(source_ref, &plan.signature);
    Some(UpnpDspDecision {
        rendered: Some(UpnpSource::GeneratedDspStream {
            id,
            zone_id: zone_id.to_string(),
            source_ref: source_ref.clone(),
            mime_type: UpnpPcmContainer::Wav.mime_type().to_string(),
            tags,
            source_rate,
            source_bits,
            target_rate: plan.output_rate,
            target_bits: plan.output_bits,
            active_output_mode: Some(plan.active_output_mode.as_name().to_string()),
            byte_len,
            dop_lead_in_data_len,
            target: target.clone(),
            playback_config: config.clone(),
        }),
        render_signature: plan.signature.clone(),
        render_ms: Some(0),
        cache_hit: Some(false),
        render_or_stream_plan: Some("progressive_wav_stream".to_string()),
        cache_lookup_ms: None,
        cache_wait_ms: Some(0),
        active_output_mode: plan.active_output_mode.as_name().to_string(),
        source_rate,
        source_bits,
        output_rate: plan.output_rate,
        output_bits: plan.output_bits,
    })
}

fn generated_wav_streaming_enabled_for_upnp() -> bool {
    std::env::var("FOZMO_UPNP_PROGRESSIVE_WAV_DSP")
        .map(|value| value.trim() != "0")
        .unwrap_or(true)
}

async fn completed_local_render_cache_decision(
    state: &AppState,
    source_ref: &SourceRef,
    config: &UpnpDspRenderConfig,
    plan: &UpnpRenderPlan,
    source_rate: u32,
    source_bits: u32,
    cache_dir: &Path,
) -> Result<Option<UpnpDspDecision>, String> {
    let lookup_started = std::time::Instant::now();
    let Some((source_cache_key, tags)) = local_source_cache_key_and_tags(state, source_ref).await?
    else {
        return Ok(None);
    };
    let cache_key = rendered_cache_key(&source_cache_key, config, plan);
    let Some(path) = rendered_cache_path_for_plan(cache_dir, &cache_key, plan, source_rate) else {
        return Ok(None);
    };
    if !path.exists() {
        return Ok(None);
    }
    let rendered = rendered_file(path, plan.output_rate, plan.output_bits, true);
    let asset = rendered_asset(
        SourceMetadata {
            source_ref: source_ref.clone(),
            tags,
            cover: None,
        },
        MediaInfo {
            sample_rate: source_rate,
            bits_per_sample: source_bits,
            channels: 2,
            duration_secs: tags_for_source_ref(source_ref).duration_secs,
        },
        rendered,
        plan.container.as_str(),
        0,
    );
    Ok(Some(UpnpDspDecision {
        rendered: Some(UpnpSource::LocalFile {
            source_ref: asset.source_ref,
            path: asset.path,
            tags: asset.tags,
            cover: asset.cover,
            byte_len: asset.byte_len,
            source_rate: asset.output_rate,
            source_bits: asset.output_bits,
        }),
        render_signature: plan.signature.clone(),
        render_ms: Some(0),
        cache_hit: Some(true),
        render_or_stream_plan: Some("completed_cache".to_string()),
        cache_lookup_ms: Some(elapsed_ms(lookup_started)),
        cache_wait_ms: Some(0),
        active_output_mode: plan.active_output_mode.as_name().to_string(),
        source_rate: asset.source_rate,
        source_bits: asset.source_bits,
        output_rate: asset.output_rate,
        output_bits: asset.output_bits,
    }))
}

async fn local_source_cache_key_and_tags(
    state: &AppState,
    source_ref: &SourceRef,
) -> Result<Option<(String, TrackTags)>, String> {
    let SourceRef::LocalTrack {
        track_id,
        title,
        artist,
        album,
        duration_secs,
        ..
    } = source_ref
    else {
        return Ok(None);
    };
    let path = state
        .library()
        .track_path(*track_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "Track not found".to_string())?;
    let meta = tokio::fs::metadata(&path)
        .await
        .map_err(|e| format!("inspect local UPnP DSP source: {e}"))?;
    let cache_key = format!(
        "local:{}:{}:{}",
        path.display(),
        meta.len(),
        meta.modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs())
            .unwrap_or(0)
    );
    let tags = TrackTags {
        title: title.clone(),
        artist: artist.clone(),
        album: album.clone(),
        duration_secs: *duration_secs,
        ..TrackTags::default()
    };
    Ok(Some((cache_key, tags)))
}

fn progressive_wav_plan_for_source(
    config: &UpnpDspRenderConfig,
    source_ref: &SourceRef,
    plan: &UpnpRenderPlan,
    source_rate: u32,
    source_bits: u32,
) -> Option<UpnpRenderPlan> {
    if !generated_wav_streaming_enabled_for_upnp()
        || !plan.render_needed
        || plan.container == "dsf"
        || source_bits == 1
        || plan.output_bits == 1
        || !matches!(plan.output_bits, 16 | 24 | 32)
        || generated_wav_metadata_for_source(
            source_ref,
            plan.output_rate,
            plan.output_bits,
            plan.active_output_mode,
        )
        .is_none()
    {
        return None;
    }
    let bit_depth = plan.output_bits.min(u32::from(u8::MAX)) as u8;
    let wav_already_selected =
        plan.container == UpnpPcmContainer::Wav.as_str() || plan.container == "dop_wav";
    let target_advertises_wav = if plan.container == "dop_wav" {
        target_supports_dop_wav_carrier(&config.target, plan.output_rate, bit_depth)
    } else {
        target_supports_pcm_container(
            &config.target,
            UpnpPcmContainer::Wav,
            plan.output_rate,
            bit_depth,
        )
    };
    if !wav_already_selected && !target_advertises_wav {
        return None;
    }
    let container = UpnpPcmContainer::Wav.as_str().to_string();
    Some(UpnpRenderPlan {
        render_needed: true,
        signature: render_signature_for_plan(
            source_ref,
            source_rate,
            source_bits,
            plan.output_rate,
            plan.output_bits,
            &container,
            config,
        ),
        output_rate: plan.output_rate,
        output_bits: plan.output_bits,
        active_output_mode: plan.active_output_mode,
        container,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OutputModeForUpnp {
    Pcm,
    Dsd64,
    Dsd128,
    Dsd256,
}

impl OutputModeForUpnp {
    fn from_name(name: &str) -> Self {
        match name {
            "Dsd64" => Self::Dsd64,
            "Dsd128" => Self::Dsd128,
            "Dsd256" => Self::Dsd256,
            _ => Self::Pcm,
        }
    }

    fn is_dsd(self) -> bool {
        matches!(self, Self::Dsd64 | Self::Dsd128 | Self::Dsd256)
    }

    fn as_name(self) -> &'static str {
        match self {
            Self::Pcm => "Pcm",
            Self::Dsd64 => "Dsd64",
            Self::Dsd128 => "Dsd128",
            Self::Dsd256 => "Dsd256",
        }
    }

    fn dsd_rate(self) -> Option<DsdRate> {
        match self {
            Self::Pcm => None,
            Self::Dsd64 => Some(DsdRate::Dsd64),
            Self::Dsd128 => Some(DsdRate::Dsd128),
            Self::Dsd256 => Some(DsdRate::Dsd256),
        }
    }
}

fn output_mode_for_upnp_config(config: &PlaybackConfig) -> OutputModeForUpnp {
    let mode = OutputModeForUpnp::from_name(&config.output_mode);
    if config.upsampling_enabled || mode.is_dsd() {
        mode
    } else {
        OutputModeForUpnp::Pcm
    }
}

async fn source_request(
    state: &AppState,
    zone_id: &str,
    source_ref: &SourceRef,
    target: &UpnpRendererTarget,
    known_source_meta: Option<(u32, u32)>,
) -> Result<SourceRequest, String> {
    match source_ref {
        SourceRef::LocalTrack {
            track_id,
            title,
            artist,
            album,
            duration_secs,
            ext_hint,
            ..
        } => {
            let path = state
                .library()
                .track_path(*track_id)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| "Track not found".to_string())?;
            let meta = tokio::fs::metadata(&path)
                .await
                .map_err(|e| format!("inspect local UPnP DSP source: {e}"))?;
            let (source_rate, source_bits) = known_source_meta.unwrap_or((0, 0));
            Ok(SourceRequest {
                source_ref: source_ref.clone(),
                source: Box::new(File::open(&path).map_err(|e| e.to_string())?),
                ext_hint: ext_hint.clone().or_else(|| {
                    path.extension()
                        .and_then(|ext| ext.to_str())
                        .map(|ext| ext.to_string())
                }),
                tags: TrackTags {
                    title: title.clone(),
                    artist: artist.clone(),
                    album: album.clone(),
                    duration_secs: *duration_secs,
                    ..TrackTags::default()
                },
                cover: None,
                cache_key: format!(
                    "local:{}:{}:{}",
                    path.display(),
                    meta.len(),
                    meta.modified()
                        .ok()
                        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|duration| duration.as_secs())
                        .unwrap_or(0)
                ),
                source_rate,
                source_bits,
            })
        }
        SourceRef::QobuzTrack {
            track_id,
            title,
            artist,
            album,
            album_id,
            image_url,
            duration_secs,
            radio,
            playlist_context,
            ..
        } => {
            let hires_enabled = state
                .library()
                .zone_settings(zone_id)
                .map(|settings| settings.qobuz_hires_enabled)
                .unwrap_or(false);
            let format_id = qobuz_format_id_for_upnp_target(target, hires_enabled);
            let req = QobuzPlayRequest {
                track_id: *track_id,
                title: title.clone(),
                artist: artist.clone(),
                album: album.clone(),
                album_id: album_id.clone(),
                image_url: image_url.clone(),
                duration_secs: *duration_secs,
                format_id: Some(format_id),
                expected_current: None,
                radio_auto: *radio,
                replace_current: false,
                playlist_context: playlist_context.clone(),
                queue: Vec::new(),
            };
            let handle = state.qobuz().open_stream(&req).await?;
            let (source_rate, source_bits) = known_source_meta.unwrap_or((0, 0));
            Ok(SourceRequest {
                source_ref: source_ref.clone(),
                source: Box::new(handle.source),
                ext_hint: Some(handle.ext),
                tags: TrackTags {
                    title: title.clone(),
                    artist: artist.clone(),
                    album: album.clone(),
                    album_artist: artist.clone(),
                    duration_secs: *duration_secs,
                    ..TrackTags::default()
                },
                cover: None,
                cache_key: format!("qobuz:{track_id}:{format_id}:{}", handle.display_name),
                source_rate,
                source_bits,
            })
        }
    }
}

async fn source_render_metadata(
    state: &AppState,
    zone_id: &str,
    source_ref: &SourceRef,
    target: &UpnpRendererTarget,
) -> Option<(u32, u32)> {
    match source_ref {
        SourceRef::LocalTrack { track_id, .. } => state
            .library()
            .track_by_id(*track_id)
            .ok()
            .flatten()
            .map(|track| {
                (
                    positive_i64_as_u32(track.sample_rate).unwrap_or(0),
                    positive_i64_as_u32(track.bit_depth).unwrap_or(0),
                )
            }),
        SourceRef::QobuzTrack { track_id, .. } => {
            let hires_enabled = state
                .library()
                .zone_settings(zone_id)
                .map(|settings| settings.qobuz_hires_enabled)
                .unwrap_or(false);
            let format_id = qobuz_format_id_for_upnp_target(target, hires_enabled);
            state
                .qobuz()
                .resolved_stream_for_format(*track_id, Some(format_id))
                .await
                .ok()
                .map(|stream| (stream.sample_rate_hz, stream.bit_depth))
        }
    }
}

fn positive_i64_as_u32(value: Option<i64>) -> Option<u32> {
    value.and_then(|value| (value > 0).then_some(value as u32))
}

fn render_plan_for_source(
    config: &UpnpDspRenderConfig,
    source_ref: &SourceRef,
    source_rate: u32,
    source_bits: u32,
) -> UpnpRenderPlan {
    let dsd_policy = dsd_policy_for_source(
        config.output_mode,
        config.filter_type,
        source_rate,
        &config.dsd_rules,
    );
    if let Some((_dsd_rate, dop_frame_rate)) =
        dop_dsd_rate_for_target(dsd_policy.output_mode, &config.target, source_rate)
    {
        let container = "dop_wav".to_string();
        return UpnpRenderPlan {
            render_needed: true,
            signature: render_signature_for_plan(
                source_ref,
                source_rate,
                source_bits,
                dop_frame_rate,
                24,
                &container,
                config,
            ),
            output_rate: dop_frame_rate,
            output_bits: 24,
            active_output_mode: dsd_policy.output_mode,
            container,
        };
    }
    if let Some(dsd_rate) = dsd_rate_for_target(dsd_policy.output_mode, &config.target) {
        let output_rate = dsd_rate
            .wire_rate_for_source(source_rate)
            .unwrap_or(source_rate);
        let container = "dsf".to_string();
        return UpnpRenderPlan {
            render_needed: true,
            signature: render_signature_for_plan(
                source_ref,
                source_rate,
                source_bits,
                output_rate,
                1,
                &container,
                config,
            ),
            output_rate,
            output_bits: 1,
            active_output_mode: dsd_policy.output_mode,
            container,
        };
    }

    let target_rate = resolve_pcm_target_rate(source_rate, config);
    // KEF's REL_TIME seek is unstable for served FLAC: it resumes briefly,
    // issues several approximate byte ranges, then reports ERROR_OCCURRED.
    // A standard 16-bit WAV gives the renderer an exact time-to-byte mapping.
    let target_bits = if is_kef_upnp_target(&config.target) {
        16
    } else {
        normalize_target_bit_depth(config.target_bit_depth)
    };
    let render_container =
        pcm_render_container_for_target(target_rate, target_bits, &config.target);
    let container = render_container.as_str().to_string();
    let render_needed = (source_rate > 0 && target_rate != source_rate)
        || (pcm_bit_depth_conversion_active(config)
            && source_bits > 0
            && target_bits != source_bits)
        || config.eq.enabled
        || processing_gain(config) != 1.0
        || dsd_policy.output_mode.is_dsd()
        || is_kef_upnp_target(&config.target);

    UpnpRenderPlan {
        render_needed,
        signature: render_signature_for_plan(
            source_ref,
            source_rate,
            source_bits,
            if render_needed {
                target_rate
            } else {
                source_rate
            },
            if render_needed {
                target_bits
            } else {
                source_bits
            },
            if render_needed {
                &container
            } else {
                "passthrough"
            },
            config,
        ),
        output_rate: if render_needed {
            target_rate
        } else {
            source_rate
        },
        output_bits: if render_needed {
            target_bits
        } else {
            source_bits
        },
        active_output_mode: OutputModeForUpnp::Pcm,
        container: if render_needed {
            container
        } else {
            "passthrough".to_string()
        },
    }
}

fn generated_wav_metadata_for_source(
    source_ref: &SourceRef,
    target_rate: u32,
    target_bits: u32,
    active_output_mode: OutputModeForUpnp,
) -> Option<(TrackTags, Option<u64>, u64)> {
    let tags = tags_for_source_ref(source_ref);
    let duration = tags.duration_secs?;
    let layout =
        generated_wav_layout_for_duration(duration, target_rate, target_bits, active_output_mode)?;
    Some((
        tags,
        Some(44 + layout.data_len),
        layout.dop_lead_in_data_len,
    ))
}

fn tags_for_source_ref(source_ref: &SourceRef) -> TrackTags {
    match source_ref {
        SourceRef::LocalTrack {
            title,
            artist,
            album,
            album_artist,
            duration_secs,
            ..
        } => TrackTags {
            title: title.clone(),
            artist: artist.clone(),
            album: album.clone(),
            album_artist: album_artist.clone(),
            duration_secs: *duration_secs,
            ..TrackTags::default()
        },
        SourceRef::QobuzTrack {
            title,
            artist,
            album,
            duration_secs,
            ..
        } => TrackTags {
            title: title.clone(),
            artist: artist.clone(),
            album: album.clone(),
            album_artist: artist.clone(),
            duration_secs: *duration_secs,
            ..TrackTags::default()
        },
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GeneratedWavLayout {
    data_len: u64,
    program_data_len: u64,
    dop_lead_in_data_len: u64,
}

fn generated_wav_layout_for_duration(
    duration_secs: f64,
    sample_rate: u32,
    bit_depth: u32,
    active_output_mode: OutputModeForUpnp,
) -> Option<GeneratedWavLayout> {
    if !duration_secs.is_finite()
        || duration_secs <= 0.0
        || sample_rate == 0
        || !matches!(bit_depth, 16 | 24 | 32)
    {
        return None;
    }
    let frames = (duration_secs * sample_rate as f64).round() as u64;
    let bytes_per_frame = 2_u64.checked_mul(u64::from(bit_depth / 8))?;
    let program_data_len = frames.checked_mul(bytes_per_frame)?;
    let dop_lead_in_data_len =
        generated_dop_lead_in_data_len(active_output_mode, sample_rate, bit_depth);
    let data_len = program_data_len.checked_add(dop_lead_in_data_len)?;
    if data_len > u32::MAX as u64 {
        return None;
    }
    Some(GeneratedWavLayout {
        data_len,
        program_data_len,
        dop_lead_in_data_len,
    })
}

fn generated_dop_lead_in_data_len(
    active_output_mode: OutputModeForUpnp,
    sample_rate: u32,
    bit_depth: u32,
) -> u64 {
    if !active_output_mode.is_dsd() || bit_depth != 24 || sample_rate == 0 {
        return 0;
    }
    let frames = u64::from(sample_rate).saturating_mul(DOP_WAV_LEAD_IN_MS) / 1_000;
    frames
        .saturating_mul(DOP_WAV_FRAME_BYTES)
        .saturating_sub(frames.saturating_mul(DOP_WAV_FRAME_BYTES) % DOP_WAV_FRAME_BYTES)
}

fn generated_dsp_asset_id(source_ref: &SourceRef, signature: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"generated-upnp-dsp");
    hasher.update(source_ref.key().as_bytes());
    hasher.update(signature.as_bytes());
    URL_SAFE_NO_PAD.encode(&hasher.finalize()[..18])
}

pub(crate) fn passthrough_render_signature(
    source_ref: &SourceRef,
    source_rate: u32,
    source_bits: u32,
    target: &UpnpRendererTarget,
    config: &PlaybackConfig,
) -> String {
    let render_config = UpnpDspRenderConfig {
        filter_type: FilterType::from_name(&config.filter_type).unwrap_or(DEFAULT_FILTER_TYPE),
        target_rate: config.target_rate,
        target_bit_depth: normalize_target_bit_depth(config.target_bit_depth),
        upsampling_enabled: config.upsampling_enabled,
        output_mode: output_mode_for_upnp_config(config),
        dsd_modulator: DsdModulator::from_name(&config.dsd_modulator).unwrap_or_default(),
        dsd_isi_penalty: config.dsd_isi_penalty,
        dsd_rules: config.dsd_rules.clone(),
        headroom_db: config.headroom_db,
        eq: config.eq.clone(),
        dither_mode: DitherPreference::from_name(&config.dither_mode)
            .unwrap_or(DitherPreference::Auto),
        target: target.clone(),
    };
    render_plan_for_source(&render_config, source_ref, source_rate, source_bits).signature
}

fn render_signature_for_plan(
    source_ref: &SourceRef,
    source_rate: u32,
    source_bits: u32,
    output_rate: u32,
    output_bits: u32,
    container: &str,
    config: &UpnpDspRenderConfig,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(UPNP_RENDER_CACHE_SCHEMA.as_bytes());
    hasher.update(source_ref.key().as_bytes());
    hasher.update(source_rate.to_le_bytes());
    hasher.update(source_bits.to_le_bytes());
    hasher.update(output_rate.to_le_bytes());
    hasher.update(output_bits.to_le_bytes());
    hasher.update(container.as_bytes());
    hasher.update(config.target.id.as_bytes());
    hasher.update(config.target.max_sample_rate.to_le_bytes());
    hasher.update(config.target.max_bit_depth.to_le_bytes());
    hasher.update(config.target.max_dsd_rate.unwrap_or_default().to_le_bytes());
    if container == "passthrough" {
        return URL_SAFE_NO_PAD.encode(&hasher.finalize()[..18]);
    }
    if output_bits == 16 {
        // Dither only changes rendered bytes for 16-bit output; keeping it
        // out of 24/32-bit signatures preserves those cache entries.
        hasher.update(config.dither_mode.as_name().as_bytes());
    }
    hasher.update(config.filter_type.as_name().as_bytes());
    hasher.update(config.target_rate.to_le_bytes());
    hasher.update(config.target_bit_depth.to_le_bytes());
    hasher.update([config.upsampling_enabled as u8]);
    hasher.update(config.output_mode.as_name().as_bytes());
    hasher.update(config.dsd_modulator.as_name().as_bytes());
    hasher.update(config.dsd_isi_penalty.to_le_bytes());
    hasher.update(config.headroom_db.to_le_bytes());
    hasher.update(serde_json::to_vec(&config.eq).unwrap_or_default());
    hasher.update(serde_json::to_vec(&config.dsd_rules).unwrap_or_default());
    URL_SAFE_NO_PAD.encode(&hasher.finalize()[..18])
}

fn render_upnp_source_blocking(
    cache_dir: PathBuf,
    request: SourceRequest,
    config: UpnpDspRenderConfig,
    cache_key: String,
    plan: UpnpRenderPlan,
) -> Result<RenderedUpnpAsset, String> {
    let cache_lock = render_cache_lock(&cache_key, &plan);
    let cache_wait_started = std::time::Instant::now();
    let _cache_guard = cache_lock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let cache_wait_ms = elapsed_ms(cache_wait_started);
    let SourceRequest {
        source_ref,
        source,
        ext_hint,
        tags,
        cover,
        cache_key: _,
        source_rate,
        source_bits,
    } = request;
    let metadata = SourceMetadata {
        source_ref,
        tags,
        cover,
    };
    if let Some(path) = rendered_cache_path_for_plan(&cache_dir, &cache_key, &plan, source_rate)
        && path.exists()
    {
        let source_info = MediaInfo {
            sample_rate: source_rate,
            bits_per_sample: source_bits,
            channels: 2,
            duration_secs: metadata.tags.duration_secs,
        };
        let rendered = rendered_file(path, plan.output_rate, plan.output_bits, true);
        return Ok(rendered_asset(
            metadata,
            source_info,
            rendered,
            output_kind_for_plan(&plan),
            cache_wait_ms,
        ));
    }
    let decoded = decode_pcm(source, ext_hint.as_deref())?;
    let dsd_policy = dsd_policy_for_source(
        config.output_mode,
        config.filter_type,
        decoded.source_rate,
        &config.dsd_rules,
    );
    if let Some((dsd_rate, dop_frame_rate)) =
        dop_dsd_rate_for_target(dsd_policy.output_mode, &config.target, decoded.source_rate)
    {
        let rendered = render_dop_wav(
            &cache_dir,
            &cache_key,
            &decoded,
            &config,
            dsd_policy.filter_type,
            dsd_rate,
            dop_frame_rate,
            &plan,
        )?;
        return Ok(rendered_asset(
            metadata,
            decoded.info,
            rendered,
            "wav",
            cache_wait_ms,
        ));
    }
    if let Some(dsd_rate) = dsd_rate_for_target(dsd_policy.output_mode, &config.target) {
        match render_dsf(
            &cache_dir,
            &cache_key,
            &decoded,
            &config,
            dsd_policy.filter_type,
            dsd_rate,
        ) {
            Ok(rendered) => {
                return Ok(rendered_asset(
                    metadata,
                    decoded.info,
                    rendered,
                    "dsf",
                    cache_wait_ms,
                ));
            }
            Err(error) => {
                warn!(
                    event = "upnp_dsp_dsd_fallback",
                    error = %error,
                    "UPnP DSD render failed; falling back to PCM DSP"
                );
            }
        }
    } else if dsd_policy.output_mode.is_dsd() {
        warn!(
            event = "upnp_dsp_dsd_unavailable",
            requested_mode = dsd_policy.output_mode.as_name(),
            renderer = %config.target.name,
            max_dsd_rate = config.target.max_dsd_rate.unwrap_or_default(),
            "UPnP renderer cannot accept the app's requested DSD mode; falling back to PCM DSP"
        );
    }

    let rendered = render_pcm(&cache_dir, &cache_key, &decoded, &config, &plan)?;
    let output_kind = rendered
        .path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("pcm")
        .to_string();
    Ok(rendered_asset(
        metadata,
        decoded.info,
        rendered,
        &output_kind,
        cache_wait_ms,
    ))
}

fn rendered_asset(
    request: SourceMetadata,
    source_info: MediaInfo,
    rendered: RenderedFile,
    output_kind: &str,
    cache_wait_ms: u64,
) -> RenderedUpnpAsset {
    let mut tags = request.tags;
    tags.sample_rate = Some(rendered.sample_rate);
    tags.bits_per_sample = Some(rendered.bits_per_sample);
    tags.channels = Some(source_info.channels);
    if tags.duration_secs.is_none() {
        tags.duration_secs = source_info.duration_secs;
    }
    RenderedUpnpAsset {
        source_ref: request.source_ref,
        path: rendered.path,
        tags,
        cover: request.cover,
        byte_len: rendered.byte_len,
        source_rate: source_info.sample_rate,
        source_bits: source_info.bits_per_sample,
        output_rate: rendered.sample_rate,
        output_bits: rendered.bits_per_sample,
        output_kind: output_kind.to_string(),
        cache_hit: rendered.cache_hit,
        cache_wait_ms,
    }
}

fn render_cache_lock(cache_key: &str, plan: &UpnpRenderPlan) -> Arc<Mutex<()>> {
    static RENDER_LOCKS: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();
    let locks = RENDER_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let lock_key = format!(
        "{}:{}:{}:{}",
        cache_key, plan.container, plan.output_rate, plan.output_bits
    );
    let mut locks = locks
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    locks
        .entry(lock_key)
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

fn rendered_cache_path_for_plan(
    cache_dir: &Path,
    cache_key: &str,
    plan: &UpnpRenderPlan,
    _source_rate: u32,
) -> Option<PathBuf> {
    let ext = match plan.container.as_str() {
        "flac" => "flac",
        "wav" | "dop_wav" => "wav",
        _ => return None,
    };
    Some(cache_dir.join(format!(
        "{cache_key}-{}-{}.{ext}",
        plan.output_rate, plan.output_bits
    )))
}

fn output_kind_for_plan(plan: &UpnpRenderPlan) -> &str {
    match plan.container.as_str() {
        "dop_wav" => "wav",
        other => other,
    }
}

struct MediaInfo {
    sample_rate: u32,
    bits_per_sample: u32,
    channels: u16,
    duration_secs: Option<f64>,
}

struct DecodedPcm {
    samples: Vec<f64>,
    source_rate: u32,
    info: MediaInfo,
}

struct RenderedFile {
    path: PathBuf,
    byte_len: Option<u64>,
    sample_rate: u32,
    bits_per_sample: u32,
    cache_hit: bool,
}

fn decode_pcm(source: Box<dyn MediaSource>, ext_hint: Option<&str>) -> Result<DecodedPcm, String> {
    let mut hint = Hint::new();
    if let Some(ext) = ext_hint {
        hint.with_extension(ext);
    }
    let mss = MediaSourceStream::new(source, Default::default());
    let mut probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| format!("probe UPnP DSP source: {e}"))?;
    let track = probed
        .format
        .tracks()
        .iter()
        .find(|track| track.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| "No playable audio track found".to_string())?;
    let track_id = track.id;
    let source_rate = track.codec_params.sample_rate.unwrap_or(44_100);
    let source_bits = track.codec_params.bits_per_sample.unwrap_or(16);
    let channels = track
        .codec_params
        .channels
        .map(|channels| channels.count() as u16)
        .unwrap_or(2);
    if channels == 0 || channels > 2 {
        return Err("UPnP DSP supports mono or stereo PCM sources only".to_string());
    }
    if source_bits == 1 {
        return Err("UPnP DSP cannot process native DSD source files yet".to_string());
    }
    let duration_secs = track
        .codec_params
        .n_frames
        .map(|frames| frames as f64 / source_rate.max(1) as f64);
    let codec_params = track.codec_params.clone();
    let mut decoder = symphonia::default::get_codecs()
        .make(&codec_params, &DecoderOptions::default())
        .map_err(|e| format!("create UPnP DSP decoder: {e}"))?;
    let mut rendered = Vec::new();

    loop {
        match probed.format.next_packet() {
            Ok(packet) if packet.track_id() == track_id => {
                let decoded = decoder
                    .decode(&packet)
                    .map_err(|e| format!("decode UPnP DSP source: {e}"))?;
                let mut sample_buf = symphonia::core::audio::AudioBuffer::<f64>::new(
                    decoded.capacity() as u64,
                    *decoded.spec(),
                );
                decoded.convert(&mut sample_buf);
                let planes = sample_buf.planes();
                let left = planes.planes()[0];
                let right = if planes.planes().len() > 1 {
                    planes.planes()[1]
                } else {
                    left
                };
                let frames = left.len().min(right.len());
                rendered.reserve(frames * 2);
                for idx in 0..frames {
                    rendered.push(sanitize_sample(left[idx]));
                    rendered.push(sanitize_sample(right[idx]));
                }
            }
            Ok(_) => {}
            Err(symphonia::core::errors::Error::IoError(err))
                if err.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(e) => return Err(format!("read UPnP DSP source packet: {e}")),
        }
    }

    Ok(DecodedPcm {
        samples: rendered,
        source_rate,
        info: MediaInfo {
            sample_rate: source_rate,
            bits_per_sample: source_bits,
            channels,
            duration_secs,
        },
    })
}

fn render_pcm(
    cache_dir: &Path,
    cache_key: &str,
    decoded: &DecodedPcm,
    config: &UpnpDspRenderConfig,
    plan: &UpnpRenderPlan,
) -> Result<RenderedFile, String> {
    let target_rate = plan.output_rate;
    let target_bits = plan.output_bits;
    let render_flac = plan.container == UpnpPcmContainer::Flac.as_str();
    let path = rendered_cache_path_for_plan(cache_dir, cache_key, plan, decoded.source_rate)
        .ok_or_else(|| format!("unsupported UPnP DSP PCM container {}", plan.container))?;
    if path.exists() {
        return Ok(rendered_file(path, target_rate, target_bits, true));
    }

    let mut interleaved = resample_interleaved(
        &decoded.samples,
        decoded.source_rate,
        target_rate,
        config.filter_type,
    );
    let mut eq = EqProcessor::new(target_rate, &config.eq);
    eq.process_interleaved_stereo(&mut interleaved);
    apply_gain(&mut interleaved, processing_gain(config));
    let dither_mode = upnp_pcm_dither_mode(config.dither_mode, target_bits);
    let mut dither_state = DitherState::new(UPNP_DITHER_SEED);
    let samples: Vec<i32> = if target_bits == 16 {
        interleaved
            .iter()
            .enumerate()
            .map(|(i, sample)| {
                quantize_signed_pcm(*sample, 16, i % 2, &mut dither_state, dither_mode)
            })
            .collect()
    } else {
        interleaved
            .iter()
            .map(|sample| float_to_pcm_i32(*sample, target_bits))
            .collect()
    };
    if render_flac {
        encode_flac_file(&path, &samples, target_rate, target_bits)?;
    } else {
        encode_wav_file(&path, &samples, target_rate, target_bits)?;
    }
    Ok(rendered_file(path, target_rate, target_bits, false))
}

#[allow(clippy::too_many_arguments)]
fn render_dop_wav(
    cache_dir: &Path,
    cache_key: &str,
    decoded: &DecodedPcm,
    config: &UpnpDspRenderConfig,
    filter_type: FilterType,
    dsd_rate: DsdRate,
    dop_frame_rate: u32,
    plan: &UpnpRenderPlan,
) -> Result<RenderedFile, String> {
    let path = rendered_cache_path_for_plan(cache_dir, cache_key, plan, decoded.source_rate)
        .ok_or_else(|| format!("unsupported UPnP DSP DoP container {}", plan.container))?;
    if path.exists() {
        return Ok(rendered_file(path, dop_frame_rate, 24, true));
    }

    let mut renderer = DsdRenderer::new_with_dsd_modulator_and_isi_penalty(
        filter_type,
        decoded.source_rate,
        dsd_rate,
        config.dsd_modulator,
        config.dsd_isi_penalty as f64,
    )
    .map_err(|e| format!("create UPnP DoP renderer: {e}"))?;
    if renderer.dop_frame_rate() != dop_frame_rate {
        return Err(format!(
            "UPnP DoP frame rate mismatch: renderer={} plan={}",
            renderer.dop_frame_rate(),
            dop_frame_rate
        ));
    }

    let mut eq = EqProcessor::new(decoded.source_rate, &config.eq);
    let gain = processing_gain(config);
    let mut dop_left_justified = Vec::new();
    append_dop_idle_samples(
        &mut dop_left_justified,
        generated_dop_lead_in_data_len(
            OutputModeForUpnp::from_name(plan.active_output_mode.as_name()),
            dop_frame_rate,
            24,
        ),
    );
    let mut left = Vec::new();
    let mut right = Vec::new();

    for block in decoded.samples.chunks(8192 * 2) {
        left.clear();
        right.clear();
        for frame in block.chunks_exact(2) {
            left.push(frame[0]);
            right.push(frame[1]);
        }
        eq.process_planar_stereo(&mut left, &mut right);
        renderer.upsample(&left, &right);
        renderer.modulate_and_pack(gain, &mut dop_left_justified);
    }
    renderer.drain_resampler_eof();
    renderer.modulate_and_pack(gain, &mut dop_left_justified);
    renderer.flush_modulators_and_pack(&mut dop_left_justified);
    let mut stamper = DopMarkerStamper::new();
    stamper.restamp_interleaved_i32(&mut dop_left_justified, DOP_WAV_CHANNELS);

    // DopPacker emits 24-bit DoP frames left-justified in i32 for device APIs.
    // A 24-bit WAV stores the significant 24 bits directly.
    let dop_samples: Vec<i32> = dop_left_justified
        .into_iter()
        .map(|sample| sample >> 8)
        .collect();
    encode_wav_file(&path, &dop_samples, dop_frame_rate, 24)?;
    Ok(rendered_file(path, dop_frame_rate, 24, false))
}

fn append_dop_idle_samples(out: &mut Vec<i32>, data_len: u64) {
    let frame_count = (data_len / DOP_WAV_FRAME_BYTES) as usize;
    if frame_count == 0 {
        return;
    }
    let start = out.len();
    out.resize(start + frame_count * DOP_WAV_CHANNELS, 0);
    let mut idle = DopIdlePattern::new();
    idle.fill_interleaved_i32(&mut out[start..], DOP_WAV_CHANNELS);
}

fn stream_wav_dsp_blocking(
    request: SourceRequest,
    config: UpnpDspRenderConfig,
    stream: &UpnpGeneratedDspStream,
    range: GeneratedByteRange,
    tx: tokio::sync::mpsc::Sender<Result<Bytes, IoError>>,
    preflight_tx: &mut Option<mpsc::Sender<Result<(), String>>>,
) -> Result<(), String> {
    let byte_len = stream
        .byte_len
        .ok_or_else(|| "Generated UPnP WAV stream is missing byte length".to_string())?;
    let data_len = byte_len
        .checked_sub(44)
        .ok_or_else(|| "Generated UPnP WAV stream length is too small".to_string())?;
    if data_len > u32::MAX as u64 {
        return Err("Generated UPnP WAV stream is too large".to_string());
    }
    let mut decoded = decode_pcm_streaming_header(request.source, request.ext_hint.as_deref())?;
    if decoded.source_bits == 1 {
        return Err("UPnP DSP cannot stream native DSD source files yet".to_string());
    }
    let dsd_policy = dsd_policy_for_source(
        config.output_mode,
        config.filter_type,
        decoded.source_rate,
        &config.dsd_rules,
    );
    let dop_target =
        dop_dsd_rate_for_target(dsd_policy.output_mode, &config.target, decoded.source_rate);
    if let Some((dsd_rate, dop_frame_rate)) = dop_target
        && dop_frame_rate == stream.target_rate
        && stream.target_bits == 24
    {
        return stream_wav_dop_blocking(
            decoded,
            config,
            dsd_policy.filter_type,
            dsd_rate,
            dop_frame_rate,
            data_len,
            stream.dop_lead_in_data_len.min(data_len),
            range,
            tx,
            preflight_tx,
        );
    }
    if generated_stream_is_dop_bound(stream) {
        let error = format!(
            "Generated UPnP DoP stream no longer matches source probe: source_rate={} source_bits={} target_rate={} target_bits={} active_output_mode={:?}",
            decoded.source_rate,
            decoded.source_bits,
            stream.target_rate,
            stream.target_bits,
            stream.active_output_mode
        );
        warn!(
            event = "dop_mode_mismatch_error",
            source_rate = decoded.source_rate,
            source_bits = decoded.source_bits,
            target_rate = stream.target_rate,
            target_bits = stream.target_bits,
            active_output_mode = ?stream.active_output_mode,
            dop_target = ?dop_target,
            "Refusing PCM fallback inside generated DoP WAV stream"
        );
        return Err(error);
    }
    let seek_plan =
        generated_wav_seek_plan(range, stream.target_rate, stream.target_bits, data_len, 0)?;
    let mut written = 0_u64;
    if let Some(seek_plan) = seek_plan {
        let seek_started = std::time::Instant::now();
        let seek_to = SeekTo::Time {
            time: Time::new(
                seek_plan.seek_seconds.floor() as u64,
                seek_plan.seek_seconds.fract(),
            ),
            track_id: Some(decoded.track_id),
        };
        match decoded.probed.format.seek(SeekMode::Accurate, seek_to) {
            Ok(seeked_to) => {
                decoded.decoder.reset();
                written = seek_cursor_data_bytes(
                    &decoded,
                    &seeked_to,
                    stream.target_rate,
                    stream.target_bits,
                    data_len,
                    0,
                )
                .unwrap_or(seek_plan.cursor_data_bytes);
                debug!(
                    event = "upnp_dsp_generated_seek",
                    requested_range_start = range.start,
                    requested_range_end = range.end,
                    range_to_output_frame = seek_plan.target_frame,
                    actual_ts = seeked_to.actual_ts,
                    required_ts = seeked_to.required_ts,
                    source_seek_ms = elapsed_ms(seek_started),
                    preroll_ms = (seek_plan.preroll_seconds * 1000.0).round() as u64,
                    discarded_output_frames = seek_plan
                        .target_frame
                        .saturating_sub(seek_plan.cursor_frame),
                    bytes_rendered_before_requested_range = written,
                    "Seeked generated UPnP DSP WAV producer near requested byte range"
                );
            }
            Err(error) => {
                if seek_plan.seek_seconds > GENERATED_WAV_NEAR_START_SEEK_FALLBACK_SECS {
                    return Err(format!(
                        "Generated UPnP DSP WAV source seek failed at {:.3}s: {error}",
                        seek_plan.seek_seconds
                    ));
                }
                warn!(
                    event = "upnp_dsp_generated_seek_failed",
                    requested_range_start = range.start,
                    requested_range_end = range.end,
                    error = %error,
                    "Generated UPnP DSP WAV producer will fall back to rendering from start"
                );
            }
        }
    }
    send_generated_stream_preflight_ready(preflight_tx)?;
    let header = wav_header_bytes(stream.target_rate, stream.target_bits, data_len as u32)?;
    let mut emitted = 0_u64;
    emit_generated_wav_bytes(&header, 0, range, &mut emitted, &tx)
        .map_err(|_| "UPnP renderer disconnected before WAV header".to_string())?;
    if emitted >= range.len() {
        return Ok(());
    }
    let mut resampler = (decoded.source_rate != stream.target_rate)
        .then(|| SincResampler::new(config.filter_type, decoded.source_rate, stream.target_rate));
    let mut eq = EqProcessor::new(stream.target_rate, &config.eq);
    let gain = processing_gain(&config);
    let dither_mode = upnp_pcm_dither_mode(config.dither_mode, stream.target_bits);
    let mut dither_state = DitherState::new(UPNP_DITHER_SEED);
    let mut left = Vec::new();
    let mut right = Vec::new();
    let mut interleaved = Vec::new();
    let mut output = Vec::new();
    let mut probed = decoded.probed;
    let track_id = decoded.track_id;
    let mut decoder = decoded.decoder;

    loop {
        match probed.format.next_packet() {
            Ok(packet) if packet.track_id() == track_id => {
                let decoded = decoder
                    .decode(&packet)
                    .map_err(|e| format!("decode generated UPnP DSP source: {e}"))?;
                let mut sample_buf = symphonia::core::audio::AudioBuffer::<f64>::new(
                    decoded.capacity() as u64,
                    *decoded.spec(),
                );
                decoded.convert(&mut sample_buf);
                let planes = sample_buf.planes();
                let plane_l = planes.planes()[0];
                let plane_r = if planes.planes().len() > 1 {
                    planes.planes()[1]
                } else {
                    plane_l
                };
                let frames = plane_l.len().min(plane_r.len());
                left.clear();
                right.clear();
                left.reserve(frames);
                right.reserve(frames);
                for idx in 0..frames {
                    left.push(sanitize_sample(plane_l[idx]));
                    right.push(sanitize_sample(plane_r[idx]));
                }
                interleaved.clear();
                if let Some(resampler) = resampler.as_mut() {
                    resampler.input(&left, &right);
                    output.clear();
                    resampler.process(&mut output);
                    interleaved.extend_from_slice(&output);
                } else {
                    interleaved.reserve(frames * 2);
                    for idx in 0..frames {
                        interleaved.push(left[idx]);
                        interleaved.push(right[idx]);
                    }
                }
                stream_wav_pcm_chunk(
                    &mut interleaved,
                    stream.target_rate,
                    stream.target_bits,
                    &mut eq,
                    gain,
                    &mut dither_state,
                    dither_mode,
                    data_len,
                    &mut written,
                    range,
                    &mut emitted,
                    &tx,
                )?;
            }
            Ok(_) => {}
            Err(symphonia::core::errors::Error::IoError(err))
                if err.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(e) => return Err(format!("read generated UPnP DSP source packet: {e}")),
        }
        if written >= data_len {
            break;
        }
    }
    if let Some(resampler) = resampler.as_mut() {
        output.clear();
        resampler.drain_eof(&mut output);
        stream_wav_pcm_chunk(
            &mut output,
            stream.target_rate,
            stream.target_bits,
            &mut eq,
            gain,
            &mut dither_state,
            dither_mode,
            data_len,
            &mut written,
            range,
            &mut emitted,
            &tx,
        )?;
    }
    stream_wav_zero_padding(data_len, &mut written, range, &mut emitted, &tx)?;
    if emitted < range.len() {
        return Err(format!(
            "Generated UPnP WAV stream ended before requested range: emitted {emitted} of {} bytes",
            range.len()
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn stream_wav_dop_blocking(
    mut decoded: StreamingDecodeHeader,
    config: UpnpDspRenderConfig,
    filter_type: FilterType,
    dsd_rate: DsdRate,
    dop_frame_rate: u32,
    data_len: u64,
    dop_lead_in_data_len: u64,
    range: GeneratedByteRange,
    tx: tokio::sync::mpsc::Sender<Result<Bytes, IoError>>,
    preflight_tx: &mut Option<mpsc::Sender<Result<(), String>>>,
) -> Result<(), String> {
    let seek_plan =
        generated_wav_seek_plan(range, dop_frame_rate, 24, data_len, dop_lead_in_data_len)?;
    let mut written = 0_u64;
    if let Some(seek_plan) = seek_plan {
        let seek_started = std::time::Instant::now();
        let seek_to = SeekTo::Time {
            time: Time::new(
                seek_plan.seek_seconds.floor() as u64,
                seek_plan.seek_seconds.fract(),
            ),
            track_id: Some(decoded.track_id),
        };
        match decoded.probed.format.seek(SeekMode::Accurate, seek_to) {
            Ok(seeked_to) => {
                decoded.decoder.reset();
                written = seek_cursor_data_bytes(
                    &decoded,
                    &seeked_to,
                    dop_frame_rate,
                    24,
                    data_len,
                    dop_lead_in_data_len,
                )
                .unwrap_or(seek_plan.cursor_data_bytes);
                debug!(
                    event = "upnp_dsp_generated_dop_seek",
                    requested_range_start = range.start,
                    requested_range_end = range.end,
                    range_to_output_frame = seek_plan.target_frame,
                    actual_ts = seeked_to.actual_ts,
                    required_ts = seeked_to.required_ts,
                    dop_lead_in_data_len,
                    source_seek_ms = elapsed_ms(seek_started),
                    preroll_ms = (seek_plan.preroll_seconds * 1000.0).round() as u64,
                    discarded_output_frames = seek_plan
                        .target_frame
                        .saturating_sub(seek_plan.cursor_frame),
                    bytes_rendered_before_requested_range = written,
                    "Seeked generated UPnP DoP WAV producer near requested byte range"
                );
            }
            Err(error) => {
                if seek_plan.seek_seconds > GENERATED_WAV_NEAR_START_SEEK_FALLBACK_SECS {
                    return Err(format!(
                        "Generated UPnP DoP WAV source seek failed at {:.3}s: {error}",
                        seek_plan.seek_seconds
                    ));
                }
                warn!(
                    event = "upnp_dsp_generated_dop_seek_failed",
                    requested_range_start = range.start,
                    requested_range_end = range.end,
                    error = %error,
                    "Generated UPnP DoP WAV producer will fall back to rendering from start"
                );
            }
        }
    }

    let mut renderer = DsdRenderer::new_with_dsd_modulator_and_isi_penalty(
        filter_type,
        decoded.source_rate,
        dsd_rate,
        config.dsd_modulator,
        config.dsd_isi_penalty as f64,
    )
    .map_err(|e| format!("create generated UPnP DoP renderer: {e}"))?;
    if renderer.dop_frame_rate() != dop_frame_rate {
        return Err(format!(
            "Generated UPnP DoP frame rate mismatch: renderer={} stream={}",
            renderer.dop_frame_rate(),
            dop_frame_rate
        ));
    }

    let mut eq = EqProcessor::new(decoded.source_rate, &config.eq);
    let gain = processing_gain(&config);
    let mut left = Vec::new();
    let mut right = Vec::new();
    let mut dop_left_justified = Vec::new();
    let header = wav_header_bytes(dop_frame_rate, 24, data_len as u32)?;
    let mut emitted = 0_u64;
    send_generated_stream_preflight_ready(preflight_tx)?;
    emit_generated_wav_bytes(&header, 0, range, &mut emitted, &tx)
        .map_err(|_| "UPnP renderer disconnected before WAV header".to_string())?;
    if emitted >= range.len() {
        return Ok(());
    }
    stream_wav_dop_idle_span(dop_lead_in_data_len, &mut written, range, &mut emitted, &tx)?;
    if emitted >= range.len() {
        return Ok(());
    }

    loop {
        match decoded.probed.format.next_packet() {
            Ok(packet) if packet.track_id() == decoded.track_id => {
                let decoded_packet = decoded
                    .decoder
                    .decode(&packet)
                    .map_err(|e| format!("decode generated UPnP DoP source: {e}"))?;
                let mut sample_buf = symphonia::core::audio::AudioBuffer::<f64>::new(
                    decoded_packet.capacity() as u64,
                    *decoded_packet.spec(),
                );
                decoded_packet.convert(&mut sample_buf);
                let planes = sample_buf.planes();
                let plane_l = planes.planes()[0];
                let plane_r = if planes.planes().len() > 1 {
                    planes.planes()[1]
                } else {
                    plane_l
                };
                let frames = plane_l.len().min(plane_r.len());
                left.clear();
                right.clear();
                left.reserve(frames);
                right.reserve(frames);
                for idx in 0..frames {
                    left.push(sanitize_sample(plane_l[idx]));
                    right.push(sanitize_sample(plane_r[idx]));
                }
                eq.process_planar_stereo(&mut left, &mut right);
                renderer.upsample(&left, &right);
                dop_left_justified.clear();
                renderer.modulate_and_pack(gain, &mut dop_left_justified);
                stream_wav_dop_chunk(
                    &dop_left_justified,
                    data_len,
                    &mut written,
                    range,
                    &mut emitted,
                    &tx,
                )?;
            }
            Ok(_) => {}
            Err(symphonia::core::errors::Error::IoError(err))
                if err.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(e) => return Err(format!("read generated UPnP DoP source packet: {e}")),
        }
        if written >= data_len {
            break;
        }
    }

    renderer.drain_resampler_eof();
    dop_left_justified.clear();
    renderer.modulate_and_pack(gain, &mut dop_left_justified);
    stream_wav_dop_chunk(
        &dop_left_justified,
        data_len,
        &mut written,
        range,
        &mut emitted,
        &tx,
    )?;
    dop_left_justified.clear();
    renderer.flush_modulators_and_pack(&mut dop_left_justified);
    stream_wav_dop_chunk(
        &dop_left_justified,
        data_len,
        &mut written,
        range,
        &mut emitted,
        &tx,
    )?;
    stream_wav_dop_idle_padding(data_len, &mut written, range, &mut emitted, &tx)?;
    if emitted < range.len() {
        return Err(format!(
            "Generated UPnP DoP stream ended before requested range: emitted {emitted} of {} bytes",
            range.len()
        ));
    }
    Ok(())
}

fn stream_wav_dop_chunk(
    samples: &[i32],
    data_len: u64,
    written: &mut u64,
    range: GeneratedByteRange,
    emitted: &mut u64,
    tx: &tokio::sync::mpsc::Sender<Result<Bytes, IoError>>,
) -> Result<(), String> {
    if samples.is_empty() || *written >= data_len {
        return Ok(());
    }
    let bytes = dop_samples_to_wav_bytes(samples, data_len - *written, *written);
    if bytes.is_empty() {
        return Ok(());
    }
    let absolute_start = 44 + *written;
    *written += bytes.len() as u64;
    emit_generated_wav_bytes(&bytes, absolute_start, range, emitted, tx)
        .map_err(|_| "UPnP renderer disconnected during generated DoP stream".to_string())
}

fn dop_samples_to_wav_bytes(
    samples: &[i32],
    remaining_bytes: u64,
    start_data_byte: u64,
) -> Vec<u8> {
    let max_samples = (remaining_bytes / DOP_WAV_SAMPLE_BYTES) as usize;
    let sample_count = samples.len().min(max_samples) - (samples.len().min(max_samples) % 2);
    if sample_count == 0 {
        return Vec::new();
    }
    let mut restamped = samples[..sample_count].to_vec();
    let mut stamper = DopMarkerStamper::with_next_marker_phase_b(dop_marker_phase_b_for_data_byte(
        start_data_byte,
    ));
    stamper.restamp_interleaved_i32(&mut restamped, DOP_WAV_CHANNELS);
    let mut bytes = Vec::with_capacity(sample_count * 3);
    for sample in restamped {
        let pcm_sample = sample >> 8;
        bytes.extend_from_slice(&pcm_sample.to_le_bytes()[..3]);
    }
    bytes
}

fn stream_wav_dop_idle_padding(
    data_len: u64,
    written: &mut u64,
    range: GeneratedByteRange,
    emitted: &mut u64,
    tx: &tokio::sync::mpsc::Sender<Result<Bytes, IoError>>,
) -> Result<(), String> {
    stream_wav_dop_idle_span(data_len, written, range, emitted, tx)
}

fn stream_wav_dop_idle_span(
    end_data_byte: u64,
    written: &mut u64,
    range: GeneratedByteRange,
    emitted: &mut u64,
    tx: &tokio::sync::mpsc::Sender<Result<Bytes, IoError>>,
) -> Result<(), String> {
    while *written < end_data_byte {
        let remaining = end_data_byte - *written;
        let mut chunk_len = remaining.min(64 * 1024);
        chunk_len -= chunk_len % DOP_WAV_FRAME_BYTES;
        if chunk_len == 0 {
            return Err("Generated UPnP DoP WAV padding ended on a partial frame".to_string());
        }
        let frame_count = (chunk_len / DOP_WAV_FRAME_BYTES) as usize;
        let mut samples = vec![0_i32; frame_count * DOP_WAV_CHANNELS];
        let mut idle =
            DopIdlePattern::with_next_marker_phase_b(dop_marker_phase_b_for_data_byte(*written));
        idle.fill_interleaved_i32(&mut samples, DOP_WAV_CHANNELS);
        let bytes = dop_samples_to_wav_bytes(&samples, chunk_len, *written);
        if bytes.is_empty() {
            return Err("Generated UPnP DoP WAV padding produced no bytes".to_string());
        }
        let absolute_start = 44 + *written;
        *written += bytes.len() as u64;
        emit_generated_wav_bytes(&bytes, absolute_start, range, emitted, tx).map_err(|_| {
            "UPnP renderer disconnected during generated DoP idle padding".to_string()
        })?;
    }
    Ok(())
}

fn dop_marker_phase_b_for_data_byte(data_byte: u64) -> bool {
    ((data_byte / DOP_WAV_FRAME_BYTES) % 2) == 1
}

fn send_generated_stream_preflight_ready(
    preflight_tx: &mut Option<mpsc::Sender<Result<(), String>>>,
) -> Result<(), String> {
    if let Some(preflight_tx) = preflight_tx.take() {
        preflight_tx
            .send(Ok(()))
            .map_err(|_| "UPnP renderer disconnected before WAV preflight completed".to_string())?;
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct GeneratedWavSeekPlan {
    target_frame: u64,
    cursor_frame: u64,
    cursor_data_bytes: u64,
    seek_seconds: f64,
    preroll_seconds: f64,
}

fn generated_wav_seek_plan(
    range: GeneratedByteRange,
    target_rate: u32,
    target_bits: u32,
    data_len: u64,
    dop_lead_in_data_len: u64,
) -> Result<Option<GeneratedWavSeekPlan>, String> {
    if range.start <= 44 || target_rate == 0 {
        return Ok(None);
    }
    let bytes_per_frame = generated_wav_bytes_per_frame(target_bits)?;
    let requested_data_byte = range.start.saturating_sub(44).min(data_len);
    if requested_data_byte <= dop_lead_in_data_len {
        return Ok(None);
    }
    let requested_program_data_byte = requested_data_byte - dop_lead_in_data_len;
    let aligned_program_data_byte =
        (requested_program_data_byte / bytes_per_frame) * bytes_per_frame;
    if aligned_program_data_byte == 0 {
        return Ok(None);
    }
    let target_frame = aligned_program_data_byte / bytes_per_frame;
    let target_seconds = target_frame as f64 / target_rate as f64;
    let preroll_seconds = GENERATED_WAV_SEEK_PREROLL_SECS.min(target_seconds);
    let seek_seconds = (target_seconds - preroll_seconds).max(0.0);
    if seek_seconds <= 0.0 {
        return Ok(None);
    }
    let cursor_frame = (seek_seconds * target_rate as f64).floor() as u64;
    let cursor_data_bytes = cursor_frame
        .checked_mul(bytes_per_frame)
        .and_then(|bytes| bytes.checked_add(dop_lead_in_data_len))
        .ok_or_else(|| "generated UPnP WAV seek cursor overflow".to_string())?
        .min(data_len);
    Ok(Some(GeneratedWavSeekPlan {
        target_frame,
        cursor_frame,
        cursor_data_bytes,
        seek_seconds,
        preroll_seconds,
    }))
}

fn seek_cursor_data_bytes(
    decoded: &StreamingDecodeHeader,
    seeked_to: &symphonia::core::formats::SeekedTo,
    target_rate: u32,
    target_bits: u32,
    data_len: u64,
    dop_lead_in_data_len: u64,
) -> Option<u64> {
    if seeked_to.track_id != decoded.track_id {
        return None;
    }
    let time_base = decoded.time_base?;
    let actual = time_base.calc_time(seeked_to.actual_ts);
    let actual_seconds = actual.seconds as f64 + actual.frac;
    let bytes_per_frame = generated_wav_bytes_per_frame(target_bits).ok()?;
    let cursor_frame = (actual_seconds.max(0.0) * target_rate as f64).floor() as u64;
    cursor_frame
        .checked_mul(bytes_per_frame)
        .and_then(|bytes| bytes.checked_add(dop_lead_in_data_len))
        .map(|bytes| bytes.min(data_len))
}

fn generated_wav_bytes_per_frame(bit_depth: u32) -> Result<u64, String> {
    let bytes_per_sample = match bit_depth {
        16 => 2_u64,
        24 => 3_u64,
        32 => 4_u64,
        _ => {
            return Err(format!(
                "unsupported generated UPnP WAV bit depth {bit_depth}"
            ));
        }
    };
    Ok(bytes_per_sample * 2)
}

struct StreamingDecodeHeader {
    probed: symphonia::core::probe::ProbeResult,
    decoder: Box<dyn symphonia::core::codecs::Decoder>,
    track_id: u32,
    time_base: Option<TimeBase>,
    source_rate: u32,
    source_bits: u32,
}

fn decode_pcm_streaming_header(
    source: Box<dyn MediaSource>,
    ext_hint: Option<&str>,
) -> Result<StreamingDecodeHeader, String> {
    let mut hint = Hint::new();
    if let Some(ext) = ext_hint {
        hint.with_extension(ext);
    }
    let mss = MediaSourceStream::new(source, Default::default());
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| format!("probe generated UPnP DSP source: {e}"))?;
    let track = probed
        .format
        .tracks()
        .iter()
        .find(|track| track.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| "No playable audio track found".to_string())?;
    let track_id = track.id;
    let time_base = track.codec_params.time_base;
    let source_rate = track.codec_params.sample_rate.unwrap_or(44_100);
    let source_bits = track.codec_params.bits_per_sample.unwrap_or(16);
    let channels = track
        .codec_params
        .channels
        .map(|channels| channels.count() as u16)
        .unwrap_or(2);
    if channels == 0 || channels > 2 {
        return Err("UPnP DSP supports mono or stereo PCM sources only".to_string());
    }
    let codec_params = track.codec_params.clone();
    let decoder = symphonia::default::get_codecs()
        .make(&codec_params, &DecoderOptions::default())
        .map_err(|e| format!("create generated UPnP DSP decoder: {e}"))?;
    Ok(StreamingDecodeHeader {
        probed,
        decoder,
        track_id,
        time_base,
        source_rate,
        source_bits,
    })
}

#[allow(clippy::too_many_arguments)]
fn stream_wav_pcm_chunk(
    samples: &mut [f64],
    _sample_rate: u32,
    bit_depth: u32,
    eq: &mut EqProcessor,
    gain: f64,
    dither_state: &mut DitherState,
    dither_mode: DitherMode,
    data_len: u64,
    written: &mut u64,
    range: GeneratedByteRange,
    emitted: &mut u64,
    tx: &tokio::sync::mpsc::Sender<Result<Bytes, IoError>>,
) -> Result<(), String> {
    if samples.is_empty() || *written >= data_len {
        return Ok(());
    }
    eq.process_interleaved_stereo(samples);
    apply_gain(samples, gain);
    let bytes = pcm_samples_to_wav_bytes(
        samples,
        bit_depth,
        data_len - *written,
        dither_state,
        dither_mode,
    )?;
    if bytes.is_empty() {
        return Ok(());
    }
    let absolute_start = 44 + *written;
    *written += bytes.len() as u64;
    emit_generated_wav_bytes(&bytes, absolute_start, range, emitted, tx)
        .map_err(|_| "UPnP renderer disconnected during generated WAV stream".to_string())
}

fn pcm_samples_to_wav_bytes(
    samples: &[f64],
    bit_depth: u32,
    remaining_bytes: u64,
    dither_state: &mut DitherState,
    dither_mode: DitherMode,
) -> Result<Vec<u8>, String> {
    let bytes_per_sample = match bit_depth {
        16 => 2,
        24 => 3,
        32 => 4,
        _ => {
            return Err(format!(
                "unsupported generated UPnP WAV bit depth {bit_depth}"
            ));
        }
    };
    let max_samples = (remaining_bytes as usize) / bytes_per_sample;
    let sample_count = samples.len().min(max_samples);
    let mut bytes = Vec::with_capacity(sample_count * bytes_per_sample);
    for (i, sample) in samples.iter().take(sample_count).enumerate() {
        let sample = if bit_depth == 16 {
            quantize_signed_pcm(*sample, 16, i % 2, dither_state, dither_mode)
        } else {
            float_to_pcm_i32(*sample, bit_depth)
        };
        match bit_depth {
            16 => bytes.extend_from_slice(&(sample as i16).to_le_bytes()),
            24 => bytes.extend_from_slice(&sample.to_le_bytes()[..3]),
            32 => bytes.extend_from_slice(&sample.to_le_bytes()),
            _ => unreachable!(),
        }
    }
    Ok(bytes)
}

fn stream_wav_zero_padding(
    data_len: u64,
    written: &mut u64,
    range: GeneratedByteRange,
    emitted: &mut u64,
    tx: &tokio::sync::mpsc::Sender<Result<Bytes, IoError>>,
) -> Result<(), String> {
    while *written < data_len {
        let chunk_len = (data_len - *written).min(64 * 1024) as usize;
        let absolute_start = 44 + *written;
        *written += chunk_len as u64;
        let padding = vec![0; chunk_len];
        emit_generated_wav_bytes(&padding, absolute_start, range, emitted, tx)
            .map_err(|_| "UPnP renderer disconnected during generated WAV padding".to_string())?;
    }
    Ok(())
}

fn emit_generated_wav_bytes(
    bytes: &[u8],
    absolute_start: u64,
    range: GeneratedByteRange,
    emitted: &mut u64,
    tx: &tokio::sync::mpsc::Sender<Result<Bytes, IoError>>,
) -> Result<(), ()> {
    let Some(slice) = generated_range_slice(bytes, absolute_start, range) else {
        return Ok(());
    };
    *emitted += slice.len() as u64;
    tx.blocking_send(Ok(Bytes::copy_from_slice(slice)))
        .map_err(|_| ())
}

fn generated_range_slice(
    bytes: &[u8],
    absolute_start: u64,
    range: GeneratedByteRange,
) -> Option<&[u8]> {
    if bytes.is_empty() {
        return None;
    }
    let chunk_end = absolute_start.checked_add(bytes.len() as u64 - 1)?;
    if chunk_end < range.start || absolute_start > range.end {
        return None;
    }
    let slice_start = range.start.saturating_sub(absolute_start) as usize;
    let slice_end = range.end.min(chunk_end) - absolute_start + 1;
    let slice_end = slice_end as usize;
    (slice_start < slice_end).then_some(&bytes[slice_start..slice_end])
}

fn wav_header_bytes(sample_rate: u32, bit_depth: u32, data_len: u32) -> Result<Vec<u8>, String> {
    let bits_per_sample = match bit_depth {
        16 | 24 | 32 => bit_depth as u16,
        _ => {
            return Err(format!(
                "unsupported generated UPnP WAV bit depth: {bit_depth}"
            ));
        }
    };
    let channels = 2_u16;
    let bytes_per_sample = bits_per_sample / 8;
    let block_align = channels
        .checked_mul(bytes_per_sample)
        .ok_or_else(|| "generated UPnP WAV block align overflow".to_string())?;
    let byte_rate = sample_rate
        .checked_mul(u32::from(block_align))
        .ok_or_else(|| "generated UPnP WAV byte rate overflow".to_string())?;
    let riff_size = 36_u32
        .checked_add(data_len)
        .ok_or_else(|| "generated UPnP WAV RIFF size overflow".to_string())?;
    let mut bytes = Vec::with_capacity(44);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&riff_size.to_le_bytes());
    bytes.extend_from_slice(b"WAVE");
    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    bytes.extend_from_slice(&byte_rate.to_le_bytes());
    bytes.extend_from_slice(&block_align.to_le_bytes());
    bytes.extend_from_slice(&bits_per_sample.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    Ok(bytes)
}

fn render_dsf(
    cache_dir: &Path,
    cache_key: &str,
    decoded: &DecodedPcm,
    config: &UpnpDspRenderConfig,
    filter_type: FilterType,
    dsd_rate: DsdRate,
) -> Result<RenderedFile, String> {
    let wire_rate = dsd_rate
        .wire_rate_for_source(decoded.source_rate)
        .ok_or_else(|| format!("source rate {} cannot render to DSD", decoded.source_rate))?;
    let dsd_rate_name = dsd_rate.oversample();
    let path = cache_dir.join(format!("{cache_key}-dsd{dsd_rate_name}.dsf"));
    if path.exists() {
        return Ok(rendered_file(path, wire_rate, 1, true));
    }

    let mut renderer = DsdRenderer::new_with_dsd_modulator_and_isi_penalty(
        filter_type,
        decoded.source_rate,
        dsd_rate,
        config.dsd_modulator,
        config.dsd_isi_penalty as f64,
    )
    .map_err(|e| format!("create UPnP DSD renderer: {e}"))?;
    renderer.set_native_order(NativeDsdOrder::MsbFirst);
    let mut eq = EqProcessor::new(decoded.source_rate, &config.eq);
    let gain = processing_gain(config);
    let mut out_l = Vec::new();
    let mut out_r = Vec::new();
    let mut left = Vec::new();
    let mut right = Vec::new();

    for block in decoded.samples.chunks(8192 * 2) {
        left.clear();
        right.clear();
        for frame in block.chunks_exact(2) {
            left.push(frame[0]);
            right.push(frame[1]);
        }
        eq.process_planar_stereo(&mut left, &mut right);
        renderer.upsample(&left, &right);
        renderer.modulate_and_pack_native(gain, &mut out_l, &mut out_r);
    }
    renderer.drain_resampler_eof();
    renderer.modulate_and_pack_native(gain, &mut out_l, &mut out_r);
    renderer.flush_modulators_and_pack_native(&mut out_l, &mut out_r);
    renderer.flush_native_with_idle(&mut out_l, &mut out_r);

    write_dsf_file(&path, wire_rate, &out_l, &out_r)?;
    Ok(rendered_file(path, wire_rate, 1, false))
}

fn resample_interleaved(
    samples: &[f64],
    source_rate: u32,
    target_rate: u32,
    filter_type: FilterType,
) -> Vec<f64> {
    if source_rate == target_rate {
        return samples.to_vec();
    }
    let mut resampler = SincResampler::new(filter_type, source_rate, target_rate);
    let mut rendered = Vec::new();
    let mut output = Vec::new();
    let mut left = Vec::new();
    let mut right = Vec::new();
    for block in samples.chunks(8192 * 2) {
        left.clear();
        right.clear();
        for frame in block.chunks_exact(2) {
            left.push(frame[0]);
            right.push(frame[1]);
        }
        resampler.input(&left, &right);
        output.clear();
        resampler.process(&mut output);
        rendered.extend_from_slice(&output);
    }
    output.clear();
    resampler.drain_eof(&mut output);
    rendered.extend_from_slice(&output);
    rendered
}

fn resolve_pcm_target_rate(source_rate: u32, config: &UpnpDspRenderConfig) -> u32 {
    if !config.upsampling_enabled {
        return source_rate;
    }
    let requested = config.target_rate;
    let caps_unknown = upnp_caps_unknown(&config.target);
    if caps_unknown {
        return source_rate;
    }
    let cap = config.target.max_sample_rate.max(source_rate);
    let target = if requested == 0 {
        auto_target_rate(source_rate, config.target.max_sample_rate)
    } else {
        requested.clamp(1, cap.max(1))
    };
    if is_kef_upnp_target(&config.target)
        && source_rate <= 96_000
        && config.target.protocol_info.iter().any(|value| {
            protocol_info_content_format(value)
                .is_some_and(|mime| mime_matches_pcm_container(mime, UpnpPcmContainer::Flac))
        })
    {
        // The in-process FLAC encoder and KEF's reliable UPnP path meet at
        // 96 kHz. Keep the source clock family when applying that ceiling.
        return auto_target_rate(source_rate, target.min(96_000));
    }
    target
}

fn dsd_rate_for_target(mode: OutputModeForUpnp, target: &UpnpRendererTarget) -> Option<DsdRate> {
    let dsd_rate = mode.dsd_rate()?;
    let required = dsd_rate.oversample() as u16;
    (target.max_dsd_rate.unwrap_or_default() >= required).then_some(dsd_rate)
}

fn dop_dsd_rate_for_target(
    mode: OutputModeForUpnp,
    target: &UpnpRendererTarget,
    source_rate: u32,
) -> Option<(DsdRate, u32)> {
    let dsd_rate = mode.dsd_rate()?;
    let wire_rate = dsd_rate.wire_rate_for_source(source_rate)?;
    let dop_frame_rate = DsdRate::dop_frame_rate(wire_rate);
    let supports_wav_dop = target_supports_dop_wav_carrier(target, dop_frame_rate, 24);
    supports_wav_dop.then_some((dsd_rate, dop_frame_rate))
}

struct DsdPolicy {
    output_mode: OutputModeForUpnp,
    filter_type: FilterType,
}

fn dsd_policy_for_source(
    requested_mode: OutputModeForUpnp,
    configured_filter: FilterType,
    source_rate: u32,
    rules: &[crate::settings::DsdSourceRule],
) -> DsdPolicy {
    if requested_mode.is_dsd()
        && let Some(rule) = rules.iter().find(|rule| rule.source_rate == source_rate)
        && {
            let mode = OutputModeForUpnp::from_name(&rule.output_mode);
            mode.is_dsd()
        }
    {
        let mode = OutputModeForUpnp::from_name(&rule.output_mode);
        let filter_type = FilterType::from_name(&rule.filter_type).unwrap_or(configured_filter);
        return DsdPolicy {
            output_mode: mode,
            filter_type,
        };
    }
    DsdPolicy {
        output_mode: requested_mode,
        filter_type: configured_filter,
    }
}

fn normalize_target_bit_depth(bits: u32) -> u32 {
    match bits {
        16 | 24 | 32 => bits,
        _ => DEFAULT_RENDERED_PCM_BITS,
    }
}

fn pcm_render_container_for_target(
    sample_rate: u32,
    bit_depth: u32,
    target: &UpnpRendererTarget,
) -> UpnpPcmContainer {
    let bit_depth_u8 = bit_depth.min(u32::from(u8::MAX)) as u8;
    if bit_depth > 24 {
        return UpnpPcmContainer::Wav;
    }
    if is_kef_upnp_target(target) && bit_depth == 16 {
        return UpnpPcmContainer::Wav;
    }
    // LSX-class KEF renderers advertise open-ended FLAC and WAV protocolInfo,
    // but reject 24-bit rendered WAV with AVTransport ERROR_OCCURRED. Their
    // generic FLAC entry is the compatible high-resolution PCM container even
    // though it does not spell out rate/bit-depth parameters.
    if is_kef_upnp_target(target)
        && target.max_sample_rate >= sample_rate
        && target.max_bit_depth >= bit_depth_u8
        && target.protocol_info.iter().any(|value| {
            protocol_info_content_format(value)
                .is_some_and(|mime| mime_matches_pcm_container(mime, UpnpPcmContainer::Flac))
        })
    {
        return UpnpPcmContainer::Flac;
    }
    if target_supports_pcm_container(target, UpnpPcmContainer::Flac, sample_rate, bit_depth_u8) {
        return UpnpPcmContainer::Flac;
    }
    if target_supports_pcm_container(target, UpnpPcmContainer::Wav, sample_rate, bit_depth_u8) {
        return UpnpPcmContainer::Wav;
    }
    UpnpPcmContainer::Wav
}

fn target_supports_pcm_container(
    target: &UpnpRendererTarget,
    container: UpnpPcmContainer,
    sample_rate: u32,
    bit_depth: u8,
) -> bool {
    target.pcm_containers.iter().any(|capability| {
        capability.container == container
            && capability.max_sample_rate >= sample_rate
            && capability.max_bit_depth >= bit_depth
    }) || target
        .protocol_info
        .iter()
        .any(|value| protocol_info_supports_pcm_container(value, container, sample_rate, bit_depth))
}

fn target_supports_dop_wav_carrier(
    target: &UpnpRendererTarget,
    sample_rate: u32,
    bit_depth: u8,
) -> bool {
    // KEF's UPnP service advertises high-rate PCM WAV but rejects DoP-marked
    // WAV streams with AVTransport ERROR_OCCURRED. Treat the advertised WAV
    // support as PCM-only so an explicit DSD preference safely falls back to
    // the renderer's PCM path.
    if is_kef_upnp_target(target) {
        return false;
    }
    if target_supports_pcm_container(target, UpnpPcmContainer::Wav, sample_rate, bit_depth) {
        return true;
    }
    if target.max_sample_rate < sample_rate || target.max_bit_depth < bit_depth {
        return false;
    }
    if target.protocol_info.iter().any(|value| {
        protocol_info_content_format(value)
            .is_some_and(|mime| mime_matches_pcm_container(mime, UpnpPcmContainer::Wav))
    }) {
        return true;
    }
    target.pcm_containers.is_empty()
        && !target.protocol_info.iter().any(|value| {
            protocol_info_content_format(value)
                .is_some_and(|mime| mime_matches_pcm_container(mime, UpnpPcmContainer::Flac))
        })
}

fn is_kef_upnp_target(target: &UpnpRendererTarget) -> bool {
    target
        .manufacturer
        .as_deref()
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("KEF"))
        || target.name.trim().to_ascii_lowercase().starts_with("kef ")
}

fn protocol_info_content_format(value: &str) -> Option<&str> {
    value
        .split(':')
        .nth(2)
        .map(str::trim)
        .filter(|mime| !mime.is_empty())
}

fn protocol_info_supports_pcm_container(
    value: &str,
    container: UpnpPcmContainer,
    sample_rate: u32,
    bit_depth: u8,
) -> bool {
    let mut parts = value.splitn(4, ':');
    let _protocol = parts.next();
    let _network = parts.next();
    let Some(content_format) = parts.next().map(|value| value.trim().to_ascii_lowercase()) else {
        return false;
    };
    let additional_info = parts.next().unwrap_or("").trim().to_ascii_lowercase();
    if !mime_matches_pcm_container(&content_format, container) {
        return false;
    }
    let combined = format!("{content_format};{additional_info}");
    let has_rate = combined
        .split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '.')
        .filter(|token| !token.is_empty())
        .filter_map(pcm_rate_token)
        .any(|rate| rate >= sample_rate);
    let advertised_bits = pcm_bit_depth_from_protocol_text(&combined);
    has_rate && advertised_bits >= bit_depth
}

fn mime_matches_pcm_container(mime: &str, container: UpnpPcmContainer) -> bool {
    match container {
        UpnpPcmContainer::Flac => mime.contains("flac"),
        UpnpPcmContainer::Wav => mime.contains("wav") || mime.contains("wave"),
    }
}

fn pcm_rate_token(token: &str) -> Option<u32> {
    let token = token.trim().to_ascii_lowercase();
    if let Some(khz) = token.strip_suffix("khz") {
        return parse_khz_rate_token(khz);
    }
    if let Some(hz) = token.strip_suffix("hz") {
        return parse_hz_rate_token(hz);
    }
    parse_hz_rate_token(&token).or_else(|| parse_khz_rate_token(&token))
}

fn parse_hz_rate_token(value: &str) -> Option<u32> {
    let parsed = value.parse::<u32>().ok()?;
    match parsed {
        44_100 | 48_000 | 88_200 | 96_000 | 176_400 | 192_000 | 352_800 | 384_000 | 705_600
        | 768_000 => Some(parsed),
        _ => None,
    }
}

fn parse_khz_rate_token(value: &str) -> Option<u32> {
    match value {
        "44" | "44.1" => Some(44_100),
        "48" => Some(48_000),
        "88" | "88.2" => Some(88_200),
        "96" => Some(96_000),
        "176" | "176.4" => Some(176_400),
        "192" => Some(192_000),
        "352" | "352.8" => Some(352_800),
        "384" => Some(384_000),
        "705" | "705.6" => Some(705_600),
        "768" => Some(768_000),
        _ => None,
    }
}

fn pcm_bit_depth_from_protocol_text(value: &str) -> u8 {
    let lower = value.to_ascii_lowercase();
    if lower.contains("bitspersample=32")
        || lower.contains("bitdepth=32")
        || lower.contains("32bit")
        || lower.contains("s32")
    {
        32
    } else if lower.contains("bitspersample=24")
        || lower.contains("bitdepth=24")
        || lower.contains("24bit")
        || lower.contains("s24")
        || lower.contains("l24")
        || lower.contains("_24")
    {
        24
    } else if lower.contains("bitspersample=16")
        || lower.contains("bitdepth=16")
        || lower.contains("16bit")
        || lower.contains("s16")
        || lower.contains("l16")
    {
        16
    } else {
        0
    }
}

fn upnp_caps_unknown(target: &UpnpRendererTarget) -> bool {
    matches!(
        target.capability_detection_source,
        CapabilityDetectionSource::Fallback | CapabilityDetectionSource::Probing
    ) || matches!(
        target.capability_detection_status,
        CapabilityDetectionStatus::Unknown
            | CapabilityDetectionStatus::Probing
            | CapabilityDetectionStatus::Deferred
            | CapabilityDetectionStatus::Failed
    )
}

fn output_gain(config: &UpnpDspRenderConfig) -> f64 {
    let headroom = if config.headroom_db.is_finite() {
        config.headroom_db.clamp(-24.0, 0.0)
    } else {
        0.0
    };
    10.0f64.powf(headroom as f64 / 20.0)
}

fn processing_gain(config: &UpnpDspRenderConfig) -> f64 {
    if config.upsampling_enabled || config.eq.enabled || config.output_mode.is_dsd() {
        output_gain(config)
    } else {
        1.0
    }
}

fn pcm_bit_depth_conversion_active(config: &UpnpDspRenderConfig) -> bool {
    config.upsampling_enabled || config.eq.enabled || config.output_mode.is_dsd()
}

fn encode_flac_file(
    path: &Path,
    samples: &[i32],
    sample_rate: u32,
    bit_depth: u32,
) -> Result<(), String> {
    if path.exists() {
        return Ok(());
    }
    let source = MemSource::from_samples(samples, 2, bit_depth as usize, sample_rate as usize);
    let config = config::Encoder::default()
        .into_verified()
        .map_err(|e| format!("verify UPnP DSP FLAC config: {e:?}"))?;
    let stream = flacenc::encode_with_fixed_block_size(&config, source, 4096)
        .map_err(|e| format!("encode UPnP DSP FLAC: {e}"))?;
    let mut sink = ByteSink::new();
    stream
        .write(&mut sink)
        .map_err(|e| format!("write UPnP DSP FLAC bitstream: {e}"))?;
    write_cache_file_once(path, sink.into_inner())
}

fn encode_wav_file(
    path: &Path,
    samples: &[i32],
    sample_rate: u32,
    bit_depth: u32,
) -> Result<(), String> {
    if path.exists() {
        return Ok(());
    }
    let bits_per_sample = match bit_depth {
        16 | 24 | 32 => bit_depth as u16,
        _ => return Err(format!("unsupported UPnP DSP PCM bit depth: {bit_depth}")),
    };
    let channels = 2_u16;
    let bytes_per_sample = bits_per_sample / 8;
    let block_align = channels
        .checked_mul(bytes_per_sample)
        .ok_or_else(|| "UPnP DSP WAV block align overflow".to_string())?;
    let byte_rate = sample_rate
        .checked_mul(u32::from(block_align))
        .ok_or_else(|| "UPnP DSP WAV byte rate overflow".to_string())?;
    let data_len = (samples.len() as u64)
        .checked_mul(u64::from(bytes_per_sample))
        .ok_or_else(|| "UPnP DSP WAV data length overflow".to_string())?;
    if data_len > u32::MAX as u64 {
        return Err("UPnP DSP WAV output is too large".to_string());
    }
    let riff_size = 36_u64 + data_len;
    if riff_size > u32::MAX as u64 {
        return Err("UPnP DSP WAV output is too large".to_string());
    }

    let mut bytes = Vec::with_capacity(44 + data_len as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(riff_size as u32).to_le_bytes());
    bytes.extend_from_slice(b"WAVE");
    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    bytes.extend_from_slice(&byte_rate.to_le_bytes());
    bytes.extend_from_slice(&block_align.to_le_bytes());
    bytes.extend_from_slice(&bits_per_sample.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&(data_len as u32).to_le_bytes());
    for sample in samples {
        match bits_per_sample {
            16 => bytes.extend_from_slice(&(*sample as i16).to_le_bytes()),
            24 => {
                let raw = sample.to_le_bytes();
                bytes.extend_from_slice(&raw[..3]);
            }
            32 => bytes.extend_from_slice(&sample.to_le_bytes()),
            _ => unreachable!(),
        }
    }
    write_cache_file_once(path, bytes)
}

fn write_dsf_file(path: &Path, sample_rate: u32, left: &[u8], right: &[u8]) -> Result<(), String> {
    if path.exists() {
        return Ok(());
    }
    let channels = 2_u32;
    let bytes_per_channel = left.len().max(right.len());
    let sample_count = (bytes_per_channel as u64) * 8;
    let blocks = bytes_per_channel
        .div_ceil(DSF_BLOCK_SIZE_PER_CHANNEL)
        .max(1);
    let padded_len = blocks * DSF_BLOCK_SIZE_PER_CHANNEL;
    let payload_len = padded_len * channels as usize;
    let data_chunk_size = 12_u64 + payload_len as u64;
    let file_size = 28_u64 + 52 + data_chunk_size;
    let mut bytes = Vec::with_capacity(file_size as usize);

    bytes.extend_from_slice(b"DSD ");
    bytes.extend_from_slice(&28_u64.to_le_bytes());
    bytes.extend_from_slice(&file_size.to_le_bytes());
    bytes.extend_from_slice(&0_u64.to_le_bytes());

    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&52_u64.to_le_bytes());
    bytes.extend_from_slice(&1_u32.to_le_bytes());
    bytes.extend_from_slice(&0_u32.to_le_bytes());
    bytes.extend_from_slice(&2_u32.to_le_bytes());
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    bytes.extend_from_slice(&1_u32.to_le_bytes());
    bytes.extend_from_slice(&sample_count.to_le_bytes());
    bytes.extend_from_slice(&(DSF_BLOCK_SIZE_PER_CHANNEL as u32).to_le_bytes());
    bytes.extend_from_slice(&0_u32.to_le_bytes());

    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_chunk_size.to_le_bytes());
    for block_idx in 0..blocks {
        append_dsf_block(&mut bytes, left, block_idx);
        append_dsf_block(&mut bytes, right, block_idx);
    }
    write_cache_file_once(path, bytes)
}

fn append_dsf_block(bytes: &mut Vec<u8>, channel: &[u8], block_idx: usize) {
    let start = block_idx * DSF_BLOCK_SIZE_PER_CHANNEL;
    let end = (start + DSF_BLOCK_SIZE_PER_CHANNEL).min(channel.len());
    if start < channel.len() {
        bytes.extend_from_slice(&channel[start..end]);
    }
    bytes.resize(
        bytes.len() + DSF_BLOCK_SIZE_PER_CHANNEL - end.saturating_sub(start),
        0x69,
    );
}

fn write_cache_file_once(path: &Path, bytes: Vec<u8>) -> Result<(), String> {
    let Some(parent) = path.parent() else {
        return Err("UPnP DSP cache path is missing a parent directory".to_string());
    };
    std::fs::create_dir_all(parent).map_err(|e| format!("create UPnP DSP cache: {e}"))?;
    let mut token = [0_u8; 12];
    OsRng.fill_bytes(&mut token);
    let temp_path = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("upnp-dsp"),
        URL_SAFE_NO_PAD.encode(token)
    ));
    std::fs::write(&temp_path, bytes).map_err(|e| format!("write UPnP DSP cache: {e}"))?;
    match std::fs::hard_link(&temp_path, path) {
        Ok(()) => {
            let _ = std::fs::remove_file(&temp_path);
            Ok(())
        }
        Err(e) if e.kind() == ErrorKind::AlreadyExists => {
            let _ = std::fs::remove_file(&temp_path);
            Ok(())
        }
        Err(e) => {
            let _ = std::fs::remove_file(&temp_path);
            if path.exists() {
                Ok(())
            } else {
                Err(format!("publish UPnP DSP cache: {e}"))
            }
        }
    }
}

fn rendered_file(
    path: PathBuf,
    sample_rate: u32,
    bits_per_sample: u32,
    cache_hit: bool,
) -> RenderedFile {
    let byte_len = std::fs::metadata(&path).ok().map(|meta| meta.len());
    RenderedFile {
        path,
        byte_len,
        sample_rate,
        bits_per_sample,
        cache_hit,
    }
}

fn rendered_cache_dir() -> Result<PathBuf, String> {
    let dir = std::env::temp_dir().join("fozmo-upnp-dsp");
    std::fs::create_dir_all(&dir).map_err(|e| format!("create UPnP DSP cache dir: {e}"))?;
    Ok(dir)
}

fn elapsed_ms(started: std::time::Instant) -> u64 {
    started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
}

fn rendered_cache_key(
    source_key: &str,
    config: &UpnpDspRenderConfig,
    plan: &UpnpRenderPlan,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(UPNP_RENDER_CACHE_SCHEMA.as_bytes());
    hasher.update(source_key.as_bytes());
    hasher.update(plan.output_rate.to_le_bytes());
    hasher.update(plan.output_bits.to_le_bytes());
    hasher.update(plan.container.as_bytes());
    if plan.output_bits == 16 {
        hasher.update(config.dither_mode.as_name().as_bytes());
    }
    hasher.update(config.filter_type.as_name().as_bytes());
    hasher.update(config.target_rate.to_le_bytes());
    hasher.update(config.target_bit_depth.to_le_bytes());
    hasher.update([config.upsampling_enabled as u8]);
    hasher.update(config.output_mode.as_name().as_bytes());
    hasher.update(config.dsd_modulator.as_name().as_bytes());
    hasher.update(config.dsd_isi_penalty.to_le_bytes());
    hasher.update(config.headroom_db.to_le_bytes());
    hasher.update(serde_json::to_vec(&config.eq).unwrap_or_default());
    hasher.update(serde_json::to_vec(&config.dsd_rules).unwrap_or_default());
    hasher.update(config.target.id.as_bytes());
    hasher.update(config.target.max_sample_rate.to_le_bytes());
    hasher.update(config.target.max_bit_depth.to_le_bytes());
    hasher.update(config.target.max_dsd_rate.unwrap_or_default().to_le_bytes());
    URL_SAFE_NO_PAD.encode(&hasher.finalize()[..18])
}

#[inline]
fn sanitize_sample(sample: f64) -> f64 {
    if sample.is_finite() { sample } else { 0.0 }
}

fn apply_gain(samples: &mut [f64], gain: f64) {
    if (gain - 1.0).abs() <= f64::EPSILON {
        return;
    }
    for sample in samples {
        *sample *= gain;
    }
}

#[inline]
fn float_to_pcm_i32(sample: f64, bit_depth: u32) -> i32 {
    let peak = match bit_depth {
        16 => i16::MAX as f64,
        24 => 8_388_607.0,
        32 => i32::MAX as f64,
        _ => 8_388_607.0,
    };
    let scaled = sanitize_sample(sample).clamp(-1.0, 1.0) * peak;
    scaled.round() as i32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playback::test_support::app_state;

    fn target(sample_rate: u32, dsd: Option<u16>) -> UpnpRendererTarget {
        UpnpRendererTarget {
            id: "renderer".to_string(),
            name: "Renderer".to_string(),
            host: "127.0.0.1".to_string(),
            port: 1400,
            model: None,
            manufacturer: None,
            av_transport_control_url: "/AVTransport".to_string(),
            rendering_control_url: None,
            connection_manager_url: None,
            max_sample_rate: sample_rate,
            max_bit_depth: 24,
            max_dsd_rate: dsd,
            capability_detection_source: CapabilityDetectionSource::Probed,
            capability_detection_status: CapabilityDetectionStatus::Complete,
            capability_detection_message: None,
            protocol_info: Vec::new(),
            pcm_containers: Vec::new(),
        }
    }

    fn config(target: UpnpRendererTarget) -> UpnpDspRenderConfig {
        UpnpDspRenderConfig {
            filter_type: FilterType::Minimum16k,
            target_rate: 0,
            target_bit_depth: DEFAULT_RENDERED_PCM_BITS,
            upsampling_enabled: true,
            output_mode: OutputModeForUpnp::Pcm,
            dsd_modulator: DsdModulator::default(),
            dsd_isi_penalty: 0.0,
            dsd_rules: Vec::new(),
            headroom_db: 0.0,
            eq: Default::default(),
            dither_mode: DitherPreference::Auto,
            target,
        }
    }

    #[test]
    fn upnp_dither_targets_16_bit_output_only() {
        assert_eq!(
            upnp_pcm_dither_mode(DitherPreference::Auto, 16),
            DitherMode::Shaped16
        );
        assert_eq!(
            upnp_pcm_dither_mode(DitherPreference::Tpdf, 16),
            DitherMode::Tpdf
        );
        assert_eq!(
            upnp_pcm_dither_mode(DitherPreference::Off, 16),
            DitherMode::Off
        );
        for bits in [24, 32] {
            assert_eq!(
                upnp_pcm_dither_mode(DitherPreference::Auto, bits),
                DitherMode::Off
            );
            assert_eq!(
                upnp_pcm_dither_mode(DitherPreference::Tpdf, bits),
                DitherMode::Off
            );
        }
    }

    #[test]
    fn generated_wav_16_bit_keeps_sub_lsb_signal_alive() {
        // A quarter-LSB DC offset truncates to silence without dither.
        let samples = vec![1.0 / 32768.0 / 4.0; 4096];
        let mut dither_state = DitherState::new(UPNP_DITHER_SEED);
        let bytes = pcm_samples_to_wav_bytes(
            &samples,
            16,
            u64::from(u32::MAX),
            &mut dither_state,
            upnp_pcm_dither_mode(DitherPreference::Auto, 16),
        )
        .expect("16-bit WAV bytes");

        assert_eq!(bytes.len(), samples.len() * 2);
        let nonzero = bytes
            .chunks_exact(2)
            .filter(|pair| i16::from_le_bytes([pair[0], pair[1]]) != 0)
            .count();
        assert!(
            nonzero > 200,
            "shaped 16-bit stream collapsed to silence: {nonzero}"
        );
    }

    #[test]
    fn generated_wav_24_bit_quantization_is_unchanged() {
        let samples: Vec<f64> = (0..64).map(|n| (n as f64 / 64.0) - 0.5).collect();
        let mut dither_state = DitherState::new(UPNP_DITHER_SEED);
        let bytes = pcm_samples_to_wav_bytes(
            &samples,
            24,
            u64::from(u32::MAX),
            &mut dither_state,
            upnp_pcm_dither_mode(DitherPreference::Auto, 24),
        )
        .expect("24-bit WAV bytes");

        for (i, sample) in samples.iter().enumerate() {
            let expected = float_to_pcm_i32(*sample, 24).to_le_bytes();
            assert_eq!(&bytes[i * 3..i * 3 + 3], &expected[..3]);
        }
    }

    #[test]
    fn render_signature_distinguishes_dither_for_16_bit_only() {
        let source = source_ref();
        let base = config(target(48_000, None));
        let mut tpdf = base.clone();
        tpdf.dither_mode = DitherPreference::Tpdf;

        let auto_16 = render_signature_for_plan(&source, 96_000, 24, 48_000, 16, "wav", &base);
        let tpdf_16 = render_signature_for_plan(&source, 96_000, 24, 48_000, 16, "wav", &tpdf);
        assert_ne!(auto_16, tpdf_16);

        let auto_24 = render_signature_for_plan(&source, 96_000, 24, 96_000, 24, "flac", &base);
        let tpdf_24 = render_signature_for_plan(&source, 96_000, 24, 96_000, 24, "flac", &tpdf);
        assert_eq!(auto_24, tpdf_24);
    }

    #[test]
    fn pcm_auto_target_uses_renderer_cap() {
        let cfg = config(target(192_000, None));
        assert_eq!(resolve_pcm_target_rate(44_100, &cfg), 176_400);
        assert_eq!(resolve_pcm_target_rate(48_000, &cfg), 192_000);
    }

    #[test]
    fn pcm_auto_target_keeps_rate_family_under_asymmetric_cap() {
        let cfg = config(target(192_000, None));
        assert_eq!(resolve_pcm_target_rate(88_200, &cfg), 176_400);
        assert_eq!(resolve_pcm_target_rate(96_000, &cfg), 192_000);
    }

    #[test]
    fn unknown_caps_do_not_downsample_existing_hires_source() {
        let mut cfg = config(target(48_000, None));
        cfg.target.capability_detection_source = CapabilityDetectionSource::Fallback;
        cfg.target.capability_detection_status = CapabilityDetectionStatus::Unknown;
        assert_eq!(resolve_pcm_target_rate(192_000, &cfg), 192_000);
        assert_eq!(resolve_pcm_target_rate(44_100, &cfg), 44_100);
    }

    #[test]
    fn dsd_requires_matching_renderer_capability() {
        assert_eq!(
            dsd_rate_for_target(OutputModeForUpnp::Dsd128, &target(192_000, Some(64))),
            None
        );
        assert_eq!(
            dsd_rate_for_target(OutputModeForUpnp::Dsd128, &target(192_000, Some(128))),
            Some(DsdRate::Dsd128)
        );
    }

    #[test]
    fn dsd64_only_renderer_accepts_generated_dsd64() {
        assert_eq!(
            dsd_rate_for_target(OutputModeForUpnp::Dsd64, &target(192_000, Some(64))),
            Some(DsdRate::Dsd64)
        );
        assert_eq!(
            dsd_rate_for_target(OutputModeForUpnp::Dsd64, &target(192_000, Some(128))),
            Some(DsdRate::Dsd64)
        );
        assert_eq!(
            dsd_rate_for_target(OutputModeForUpnp::Dsd64, &target(192_000, None)),
            None
        );
    }

    #[test]
    fn dsd64_uses_dop_wav_when_renderer_accepts_pcm_carrier() {
        let mut cfg = config(target(192_000, Some(64)));
        cfg.target.pcm_containers = vec![crate::protocol::UpnpPcmContainerCapability {
            container: UpnpPcmContainer::Wav,
            max_sample_rate: 192_000,
            max_bit_depth: 24,
        }];
        cfg.output_mode = OutputModeForUpnp::Dsd64;

        let plan = render_plan_for_source(&cfg, &source_ref(), 96_000, 24);

        assert!(plan.render_needed);
        assert_eq!(plan.container, "dop_wav");
        assert_eq!(plan.output_rate, 192_000);
        assert_eq!(plan.output_bits, 24);
        assert_eq!(plan.active_output_mode, OutputModeForUpnp::Dsd64);
    }

    #[test]
    fn dsd64_uses_dop_wav_with_calibrated_pcm_carrier_without_container_detail() {
        let mut cfg = config(target(192_000, Some(64)));
        cfg.output_mode = OutputModeForUpnp::Dsd64;

        let plan = render_plan_for_source(&cfg, &source_ref(), 96_000, 24);

        assert!(plan.render_needed);
        assert_eq!(plan.container, "dop_wav");
        assert_eq!(plan.output_rate, 192_000);
        assert_eq!(plan.output_bits, 24);
    }

    #[test]
    fn dsd64_uses_dop_wav_even_when_pcm_upsampling_toggle_is_off() {
        let mut cfg = config(target(192_000, Some(64)));
        cfg.upsampling_enabled = false;
        cfg.output_mode = OutputModeForUpnp::Dsd64;

        let plan = render_plan_for_source(&cfg, &source_ref(), 44_100, 16);

        assert!(plan.render_needed);
        assert_eq!(plan.container, "dop_wav");
        assert_eq!(plan.output_rate, 176_400);
        assert_eq!(plan.output_bits, 24);
        assert_eq!(plan.active_output_mode, OutputModeForUpnp::Dsd64);
    }

    #[test]
    fn dsd64_uses_dop_wav_with_open_ended_wav_protocol_info() {
        let mut cfg = config(target(192_000, Some(64)));
        cfg.target.protocol_info = vec!["http-get:*:audio/wav:*".to_string()];
        cfg.output_mode = OutputModeForUpnp::Dsd64;

        let plan = render_plan_for_source(&cfg, &source_ref(), 96_000, 24);

        assert!(plan.render_needed);
        assert_eq!(plan.container, "dop_wav");
        assert_eq!(plan.output_rate, 192_000);
        assert_eq!(plan.output_bits, 24);
    }

    #[test]
    fn kef_dsd_preference_falls_back_to_pcm_instead_of_dop_wav() {
        let mut kef = target(192_000, None);
        kef.name = "KEF LSX".to_string();
        kef.manufacturer = Some("KEF".to_string());
        kef.model = Some("SP3994".to_string());
        kef.protocol_info = vec!["http-get:*:audio/flac:*".to_string()];
        kef.pcm_containers = vec![crate::protocol::UpnpPcmContainerCapability {
            container: UpnpPcmContainer::Wav,
            max_sample_rate: 192_000,
            max_bit_depth: 24,
        }];
        let mut cfg = config(kef);
        cfg.output_mode = OutputModeForUpnp::Dsd64;

        let plan = render_plan_for_source(&cfg, &source_ref(), 44_100, 16);

        assert!(plan.render_needed);
        assert_eq!(plan.container, "wav");
        assert_eq!(plan.output_rate, 88_200);
        assert_eq!(plan.output_bits, 16);
        assert_eq!(plan.active_output_mode, OutputModeForUpnp::Pcm);
    }

    #[test]
    fn kef_pcm_uses_seekable_16_bit_wav_even_without_upsampling() {
        let mut kef = target(192_000, None);
        kef.name = "KEF LSX".to_string();
        kef.manufacturer = Some("KEF".to_string());
        kef.protocol_info = vec!["http-get:*:audio/wav:*".to_string()];
        let mut cfg = config(kef);
        cfg.upsampling_enabled = false;
        cfg.output_mode = OutputModeForUpnp::Pcm;

        let plan = render_plan_for_source(&cfg, &source_ref(), 44_100, 24);

        assert!(plan.render_needed);
        assert_eq!(plan.container, "wav");
        assert_eq!(plan.output_rate, 44_100);
        assert_eq!(plan.output_bits, 16);
    }

    #[tokio::test]
    async fn inactive_kef_playback_uses_passthrough_without_eager_rendering() {
        let state = app_state("upnp-kef-inactive-passthrough");
        let mut kef = target(192_000, None);
        kef.name = "KEF LSX".to_string();
        kef.manufacturer = Some("KEF".to_string());
        kef.protocol_info = vec!["http-get:*:audio/wav:*".to_string()];

        let mut playback = playback_config();
        playback.upsampling_enabled = false;
        playback.target_rate = 0;
        playback.target_bit_depth = 24;
        playback.output_mode = "Pcm".to_string();
        playback.headroom_db = 0.0;
        playback.eq.enabled = false;

        let decision = rendered_upnp_source_if_needed(
            &state,
            "kef-zone",
            &source_ref(),
            &kef,
            &playback,
            UpnpDspStreamingPolicy::ForceCompletedRender,
        )
        .await
        .expect("inactive KEF playback should not open or decode the source");

        assert!(decision.rendered.is_none());
        assert_eq!(
            decision.render_or_stream_plan.as_deref(),
            Some("passthrough")
        );
        assert_eq!(decision.render_ms, None);
        assert_eq!(decision.cache_hit, None);
    }

    #[test]
    fn dsd64_flac_only_renderer_uses_native_dsf_when_available() {
        let mut cfg = config(target(192_000, Some(64)));
        cfg.target.protocol_info = vec!["http-get:*:audio/flac:*".to_string()];
        cfg.output_mode = OutputModeForUpnp::Dsd64;

        let plan = render_plan_for_source(&cfg, &source_ref(), 96_000, 24);

        assert!(plan.render_needed);
        assert_eq!(plan.container, "dsf");
        assert_eq!(plan.output_rate, 3_072_000);
        assert_eq!(plan.output_bits, 1);
    }

    #[test]
    fn dsd64_without_pcm_carrier_uses_native_dsf_when_available() {
        let mut cfg = config(target(96_000, Some(64)));
        cfg.output_mode = OutputModeForUpnp::Dsd64;

        let plan = render_plan_for_source(&cfg, &source_ref(), 96_000, 24);

        assert!(plan.render_needed);
        assert_eq!(plan.container, "dsf");
        assert_eq!(plan.output_rate, 3_072_000);
        assert_eq!(plan.output_bits, 1);
    }

    #[test]
    fn pcm_render_container_uses_target_container_capabilities() {
        let mut flac_target = target(192_000, None);
        flac_target.pcm_containers = vec![crate::protocol::UpnpPcmContainerCapability {
            container: UpnpPcmContainer::Flac,
            max_sample_rate: 192_000,
            max_bit_depth: 24,
        }];
        assert_eq!(
            pcm_render_container_for_target(192_000, 24, &flac_target),
            UpnpPcmContainer::Flac
        );

        let mut wav_target = target(192_000, None);
        wav_target.pcm_containers = vec![crate::protocol::UpnpPcmContainerCapability {
            container: UpnpPcmContainer::Wav,
            max_sample_rate: 192_000,
            max_bit_depth: 24,
        }];
        assert_eq!(
            pcm_render_container_for_target(192_000, 24, &wav_target),
            UpnpPcmContainer::Wav
        );

        let unknown_target = target(192_000, None);
        assert_eq!(
            pcm_render_container_for_target(96_000, 24, &unknown_target),
            UpnpPcmContainer::Wav
        );
        assert_eq!(
            pcm_render_container_for_target(192_000, 24, &unknown_target),
            UpnpPcmContainer::Wav
        );
        assert_eq!(
            pcm_render_container_for_target(96_000, 32, &unknown_target),
            UpnpPcmContainer::Wav
        );
    }

    #[test]
    fn pcm_sample_conversion_uses_selected_bit_depth() {
        assert_eq!(float_to_pcm_i32(1.0, 16), 32_767);
        assert_eq!(float_to_pcm_i32(1.0, 24), 8_388_607);
        assert_eq!(float_to_pcm_i32(1.0, 32), i32::MAX);
    }

    #[test]
    fn generated_byte_range_rejects_invalid_windows() {
        assert_eq!(
            GeneratedByteRange::validate(5, 9, 10).unwrap(),
            GeneratedByteRange { start: 5, end: 9 }
        );
        assert!(GeneratedByteRange::validate(5, 4, 10).is_err());
        assert!(GeneratedByteRange::validate(10, 10, 10).is_err());
        assert!(GeneratedByteRange::validate(0, 0, 0).is_err());
    }

    #[test]
    fn generated_range_slice_trims_absolute_chunks() {
        let bytes: Vec<u8> = (0..10).collect();
        let range = GeneratedByteRange { start: 3, end: 8 };

        assert_eq!(generated_range_slice(&bytes, 0, range), Some(&bytes[3..9]));
        assert_eq!(generated_range_slice(&bytes, 5, range), Some(&bytes[0..4]));
        assert_eq!(generated_range_slice(&bytes, 10, range), None);
    }

    #[test]
    fn dop_wav_bytes_are_restamped_for_absolute_range_phase() {
        let samples = [0_i32; 4];

        let from_start = dop_samples_to_wav_bytes(&samples, 12, 0);
        assert_eq!(dop_wav_markers(&from_start), vec![0x05, 0xFA]);

        let from_second_frame = dop_samples_to_wav_bytes(&samples, 12, DOP_WAV_FRAME_BYTES);
        assert_eq!(dop_wav_markers(&from_second_frame), vec![0xFA, 0x05]);
    }

    #[test]
    fn generated_dop_idle_padding_uses_markered_silence_not_zero_pcm() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(2);
        let range = GeneratedByteRange { start: 44, end: 55 };
        let mut written = 0;
        let mut emitted = 0;

        stream_wav_dop_idle_padding(12, &mut written, range, &mut emitted, &tx)
            .expect("DoP idle padding");

        let bytes = rx.try_recv().expect("padding bytes").expect("ok bytes");
        assert_eq!(written, 12);
        assert_eq!(emitted, 12);
        assert_ne!(&bytes[..], &[0_u8; 12]);
        assert_eq!(dop_wav_markers(&bytes), vec![0x05, 0xFA]);
    }

    fn dop_wav_markers(bytes: &[u8]) -> Vec<u8> {
        bytes
            .chunks_exact(6)
            .map(|frame| {
                assert_eq!(frame[2], frame[5], "left/right markers must match");
                frame[2]
            })
            .collect()
    }

    #[test]
    fn generated_wav_seek_plan_maps_deep_range_to_output_time_with_preroll() {
        let bytes_per_frame = generated_wav_bytes_per_frame(24).unwrap();
        let target_frame = 192_000_u64 * 30;
        let range = GeneratedByteRange {
            start: 44 + target_frame * bytes_per_frame,
            end: 44 + target_frame * bytes_per_frame + 1023,
        };

        let plan = generated_wav_seek_plan(range, 192_000, 24, u32::MAX as u64, 0)
            .unwrap()
            .expect("deep range should seek");

        assert_eq!(plan.target_frame, target_frame);
        assert_eq!(plan.preroll_seconds, GENERATED_WAV_SEEK_PREROLL_SECS);
        assert!(plan.seek_seconds >= 29.49 && plan.seek_seconds <= 29.51);
        assert_eq!(plan.cursor_frame, 192_000_u64 * 29 + 96_000);
        assert!(plan.cursor_data_bytes < range.start - 44);
    }

    #[test]
    fn generated_wav_seek_plan_does_not_seek_for_header_or_start() {
        assert!(
            generated_wav_seek_plan(
                GeneratedByteRange { start: 0, end: 43 },
                192_000,
                24,
                4096,
                0
            )
            .unwrap()
            .is_none()
        );
        assert!(
            generated_wav_seek_plan(
                GeneratedByteRange {
                    start: 44,
                    end: 1024
                },
                192_000,
                24,
                4096,
                0
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn generated_dop_wav_layout_includes_lead_in_without_changing_tags() {
        let source = qobuz_source_ref();
        let (tags, byte_len, lead_in) =
            generated_wav_metadata_for_source(&source, 192_000, 24, OutputModeForUpnp::Dsd64)
                .expect("DoP metadata");

        assert_eq!(tags.duration_secs, Some(180.0));
        assert_eq!(
            lead_in,
            192_000 * DOP_WAV_FRAME_BYTES * DOP_WAV_LEAD_IN_MS / 1_000
        );
        assert_eq!(
            byte_len,
            Some(44 + (180.0_f64 * 192_000.0).round() as u64 * 6 + lead_in)
        );
    }

    #[test]
    fn generated_dop_wav_seek_plan_subtracts_lead_in_for_source_time() {
        let lead_in = generated_dop_lead_in_data_len(OutputModeForUpnp::Dsd64, 192_000, 24);
        let target_frame = 192_000_u64 * 30;
        let range = GeneratedByteRange {
            start: 44 + lead_in + target_frame * DOP_WAV_FRAME_BYTES,
            end: 44 + lead_in + target_frame * DOP_WAV_FRAME_BYTES + 1023,
        };

        let plan = generated_wav_seek_plan(range, 192_000, 24, u32::MAX as u64, lead_in)
            .unwrap()
            .expect("post lead-in range should seek");

        assert_eq!(plan.target_frame, target_frame);
        assert!(plan.seek_seconds >= 29.49 && plan.seek_seconds <= 29.51);
        assert_eq!(
            plan.cursor_data_bytes,
            lead_in + (192_000_u64 * 29 + 96_000) * 6
        );
        assert!(
            generated_wav_seek_plan(
                GeneratedByteRange {
                    start: 44,
                    end: 44 + lead_in - 1
                },
                192_000,
                24,
                u32::MAX as u64,
                lead_in
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn bit_depth_change_is_passthrough_when_upsampling_disabled() {
        let mut cfg = config(target(192_000, None));
        cfg.upsampling_enabled = false;
        cfg.target_bit_depth = 24;

        let plan = render_plan_for_source(&cfg, &source_ref(), 44_100, 16);

        assert!(!plan.render_needed);
    }

    #[test]
    fn bit_depth_change_forces_pcm_render_when_upsampling_enabled() {
        let mut cfg = config(target(192_000, None));
        cfg.upsampling_enabled = true;
        cfg.target_rate = 44_100;
        cfg.target_bit_depth = 24;

        let plan = render_plan_for_source(&cfg, &source_ref(), 44_100, 16);

        assert!(plan.render_needed);
    }

    #[test]
    fn disabled_upsampling_ignores_headroom_for_passthrough_upnp() {
        let mut cfg = config(target(192_000, None));
        cfg.upsampling_enabled = false;
        cfg.target_rate = 0;
        cfg.target_bit_depth = 24;
        cfg.headroom_db = -4.0;

        let plan = render_plan_for_source(&cfg, &source_ref(), 192_000, 24);

        assert!(!plan.render_needed);
    }

    #[test]
    fn filter_only_equal_rate_pcm_is_passthrough() {
        let mut cfg = config(target(192_000, None));
        cfg.target_rate = 44_100;
        cfg.target_bit_depth = 16;
        cfg.upsampling_enabled = true;

        let plan = render_plan_for_source(&cfg, &source_ref(), 44_100, 16);
        cfg.filter_type = FilterType::Minimum16k;
        let filter_changed = render_plan_for_source(&cfg, &source_ref(), 44_100, 16);

        assert!(!plan.render_needed);
        assert!(!filter_changed.render_needed);
        assert_eq!(plan.signature, filter_changed.signature);
    }

    #[test]
    fn dsd_rule_changes_render_signature() {
        let mut cfg = config(target(192_000, Some(256)));
        cfg.output_mode = OutputModeForUpnp::Dsd128;
        let base = render_plan_for_source(&cfg, &source_ref(), 44_100, 24).signature;
        cfg.dsd_rules = vec![crate::settings::DsdSourceRule {
            source_rate: 44_100,
            filter_type: "Minimum16k".to_string(),
            output_mode: "Dsd256".to_string(),
        }];

        let changed = render_plan_for_source(&cfg, &source_ref(), 44_100, 24).signature;

        assert_ne!(base, changed);
    }

    #[test]
    fn dsd_rules_are_part_of_render_cache_key() {
        let mut cfg = config(target(192_000, Some(256)));
        let base_plan = render_plan_for_source(&cfg, &source_ref(), 44_100, 24);
        let base = rendered_cache_key("source", &cfg, &base_plan);
        cfg.dsd_rules = vec![crate::settings::DsdSourceRule {
            source_rate: 44_100,
            filter_type: "Minimum16k".to_string(),
            output_mode: "Dsd256".to_string(),
        }];

        let changed_plan = render_plan_for_source(&cfg, &source_ref(), 44_100, 24);
        let changed = rendered_cache_key("source", &cfg, &changed_plan);

        assert_ne!(base, changed);
    }

    #[test]
    fn render_cache_key_changes_with_pcm_container() {
        let mut flac_cfg = config(target(192_000, None));
        flac_cfg.target.pcm_containers = vec![crate::protocol::UpnpPcmContainerCapability {
            container: UpnpPcmContainer::Flac,
            max_sample_rate: 192_000,
            max_bit_depth: 24,
        }];
        flac_cfg.target_rate = 192_000;
        let flac_plan = render_plan_for_source(&flac_cfg, &source_ref(), 48_000, 24);

        let mut wav_cfg = config(target(192_000, None));
        wav_cfg.target.pcm_containers = vec![crate::protocol::UpnpPcmContainerCapability {
            container: UpnpPcmContainer::Wav,
            max_sample_rate: 192_000,
            max_bit_depth: 24,
        }];
        wav_cfg.target_rate = 192_000;
        let wav_plan = render_plan_for_source(&wav_cfg, &source_ref(), 48_000, 24);

        assert_eq!(flac_plan.container, "flac");
        assert_eq!(wav_plan.container, "wav");
        assert_ne!(
            rendered_cache_key("source", &flac_cfg, &flac_plan),
            rendered_cache_key("source", &wav_cfg, &wav_plan)
        );
    }

    #[test]
    fn render_cache_lock_reuses_matching_output_key() {
        let mut cfg = config(target(192_000, None));
        cfg.target_rate = 192_000;
        let plan = render_plan_for_source(&cfg, &source_ref(), 48_000, 24);
        let same = render_plan_for_source(&cfg, &source_ref(), 48_000, 24);
        let different = UpnpRenderPlan {
            output_rate: plan.output_rate.saturating_mul(2),
            ..plan.clone()
        };

        assert!(Arc::ptr_eq(
            &render_cache_lock("source-key", &plan),
            &render_cache_lock("source-key", &same)
        ));
        assert!(!Arc::ptr_eq(
            &render_cache_lock("source-key", &plan),
            &render_cache_lock("source-key", &different)
        ));
    }

    #[test]
    fn rendered_pcm_cache_path_uses_selected_container_extension() {
        let cache_dir = Path::new("/tmp/fozmo-upnp-dsp-test");
        let flac = rendered_cache_path_for_plan(
            cache_dir,
            "cache-key",
            &UpnpRenderPlan {
                render_needed: true,
                signature: "sig".to_string(),
                output_rate: 192_000,
                output_bits: 24,
                active_output_mode: OutputModeForUpnp::Pcm,
                container: "flac".to_string(),
            },
            44_100,
        )
        .expect("FLAC cache path");
        let wav = rendered_cache_path_for_plan(
            cache_dir,
            "cache-key",
            &UpnpRenderPlan {
                render_needed: true,
                signature: "sig".to_string(),
                output_rate: 192_000,
                output_bits: 24,
                active_output_mode: OutputModeForUpnp::Pcm,
                container: "wav".to_string(),
            },
            44_100,
        )
        .expect("WAV cache path");

        assert!(flac.ends_with("cache-key-192000-24.flac"));
        assert!(wav.ends_with("cache-key-192000-24.wav"));
    }

    #[test]
    fn render_needed_wav_plan_uses_progressive_generated_stream() {
        let mut cfg = config(target(192_000, None));
        cfg.target.pcm_containers = vec![crate::protocol::UpnpPcmContainerCapability {
            container: UpnpPcmContainer::Wav,
            max_sample_rate: 192_000,
            max_bit_depth: 24,
        }];
        cfg.target_rate = 192_000;
        cfg.target_bit_depth = 24;
        let source = source_ref();
        let plan = render_plan_for_source(&cfg, &source, 96_000, 24);

        assert!(plan.render_needed);
        assert_eq!(plan.container, UpnpPcmContainer::Wav.as_str());

        let progressive_plan = progressive_wav_plan_for_source(&cfg, &source, &plan, 96_000, 24)
            .expect("progressive WAV plan");
        let decision = generated_wav_decision_for_plan(
            "zone",
            &source,
            &cfg.target,
            &playback_config(),
            &progressive_plan,
            96_000,
            24,
        )
        .expect("generated stream decision");

        assert_eq!(
            decision.render_or_stream_plan.as_deref(),
            Some("progressive_wav_stream")
        );
        assert_eq!(decision.render_ms, Some(0));
        assert_eq!(decision.output_rate, 192_000);
        assert_eq!(decision.output_bits, 24);
        assert!(matches!(
            decision.rendered,
            Some(UpnpSource::GeneratedDspStream { .. })
        ));
    }

    #[test]
    fn qobuz_wav_fallback_uses_progressive_generated_stream() {
        let mut cfg = config(target(192_000, None));
        cfg.target_rate = 192_000;
        cfg.target_bit_depth = 24;
        let source = qobuz_source_ref();
        let plan = render_plan_for_source(&cfg, &source, 96_000, 24);

        assert!(plan.render_needed);
        assert_eq!(plan.container, UpnpPcmContainer::Wav.as_str());

        let progressive_plan = progressive_wav_plan_for_source(&cfg, &source, &plan, 96_000, 24)
            .expect("progressive WAV plan");
        assert_eq!(progressive_plan.container, UpnpPcmContainer::Wav.as_str());

        let decision = generated_wav_decision_for_plan(
            "zone",
            &source,
            &cfg.target,
            &playback_config(),
            &progressive_plan,
            96_000,
            24,
        )
        .expect("generated stream decision");

        assert_eq!(
            decision.render_or_stream_plan.as_deref(),
            Some("progressive_wav_stream")
        );
        assert!(matches!(
            decision.rendered,
            Some(UpnpSource::GeneratedDspStream { .. })
        ));
    }

    #[test]
    fn qobuz_dop_wav_uses_progressive_generated_stream() {
        let mut cfg = config(target(192_000, Some(64)));
        cfg.output_mode = OutputModeForUpnp::Dsd64;
        let source = qobuz_source_ref();
        let plan = render_plan_for_source(&cfg, &source, 96_000, 24);

        assert!(plan.render_needed);
        assert_eq!(plan.container, "dop_wav");

        let progressive_plan = progressive_wav_plan_for_source(&cfg, &source, &plan, 96_000, 24)
            .expect("progressive DoP WAV plan");
        assert_eq!(progressive_plan.container, UpnpPcmContainer::Wav.as_str());
        assert_eq!(progressive_plan.output_rate, 192_000);
        assert_eq!(progressive_plan.output_bits, 24);
        assert_eq!(
            progressive_plan.active_output_mode,
            OutputModeForUpnp::Dsd64
        );

        let mut playback_config = playback_config();
        playback_config.output_mode = "Dsd64".to_string();
        let decision = generated_wav_decision_for_plan(
            "zone",
            &source,
            &cfg.target,
            &playback_config,
            &progressive_plan,
            96_000,
            24,
        )
        .expect("generated stream decision");

        assert_eq!(
            decision.render_or_stream_plan.as_deref(),
            Some("progressive_wav_stream")
        );
        assert_eq!(decision.render_ms, Some(0));
        assert_eq!(decision.active_output_mode, "Dsd64");
        assert_eq!(decision.output_rate, 192_000);
        assert_eq!(decision.output_bits, 24);
        assert!(matches!(
            decision.rendered,
            Some(UpnpSource::GeneratedDspStream { .. })
        ));
    }

    #[test]
    fn progressive_generated_stream_requires_known_duration() {
        let mut cfg = config(target(192_000, None));
        cfg.target.pcm_containers = vec![crate::protocol::UpnpPcmContainerCapability {
            container: UpnpPcmContainer::Wav,
            max_sample_rate: 192_000,
            max_bit_depth: 24,
        }];
        cfg.target_rate = 192_000;
        cfg.target_bit_depth = 24;
        let mut source = source_ref();
        if let SourceRef::LocalTrack { duration_secs, .. } = &mut source {
            *duration_secs = None;
        }
        let plan = render_plan_for_source(&cfg, &source, 96_000, 24);

        assert!(plan.render_needed);
        assert!(progressive_wav_plan_for_source(&cfg, &source, &plan, 96_000, 24).is_none());
    }

    fn playback_config() -> PlaybackConfig {
        PlaybackConfig {
            filter_type: "Minimum16k".to_string(),
            target_rate: 192_000,
            target_bit_depth: 24,
            upsampling_enabled: true,
            exclusive: false,
            dither_mode: "Auto".to_string(),
            output_mode: "Pcm".to_string(),
            dsd_modulator: "EcDepth2".to_string(),
            dsd_isi_penalty: 0.0,
            dsd_rules: Vec::new(),
            headroom_db: 0.0,
            dsp_buffer_ms: 0,
            volume: 1.0,
            eq: Default::default(),
            output_device: None,
        }
    }

    fn source_ref() -> SourceRef {
        SourceRef::LocalTrack {
            track_id: 1,
            file_name: None,
            title: Some("Track".to_string()),
            artist: Some("Artist".to_string()),
            album: Some("Album".to_string()),
            album_artist: None,
            album_id: None,
            art_id: None,
            duration_secs: Some(180.0),
            ext_hint: None,
            radio: false,
            radio_context: None,
            playlist_context: None,
        }
    }

    fn qobuz_source_ref() -> SourceRef {
        SourceRef::QobuzTrack {
            track_id: 1,
            title: Some("Qobuz Track".to_string()),
            artist: Some("Artist".to_string()),
            album: Some("Album".to_string()),
            album_id: Some("album".to_string()),
            image_url: None,
            duration_secs: Some(180.0),
            radio: false,
            radio_context: None,
            playlist_context: None,
        }
    }
}
