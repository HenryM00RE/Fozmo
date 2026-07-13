use crate::audio::player::{TrackCover, TrackTags};
use crate::protocol::{
    CapabilityDetectionSource, CapabilityDetectionStatus, PlaybackConfig, UpnpPcmContainer,
    UpnpPcmContainerCapability,
};
use crate::zones::constant_time_token_matches;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use flacenc::bitsink::ByteSink;
use flacenc::component::BitRepr;
use flacenc::config;
use flacenc::error::Verify;
use flacenc::source::MemSource;
use quick_xml::Reader;
use quick_xml::events::Event;
use rand::{RngCore, rngs::OsRng};
use reqwest::{Client, Url, redirect};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::io::ErrorKind;
use std::net::IpAddr;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::net::UdpSocket;
use tokio::sync::Mutex as AsyncMutex;

use crate::protocol::SourceRef;

pub const UPNP_DEVICE_PREFIX: &str = "UPnP AV Renderer:";
pub const UPNP_FALLBACK_SAMPLE_RATE: u32 = 48_000;
pub const UPNP_FALLBACK_BIT_DEPTH: u8 = 16;

const UPNP_DISCOVERY_INTERVAL: Duration = Duration::from_secs(30);
const UPNP_ASSET_TTL: Duration = Duration::from_secs(60 * 60 * 4);
const UPNP_DESCRIPTION_TIMEOUT: Duration = Duration::from_secs(2);
const UPNP_DESCRIPTION_MAX_BYTES: usize = 256 * 1024;
const UPNP_PLAYBACK_REFRESH_TIMEOUT: Duration = Duration::from_millis(1200);
const UPNP_SOAP_ACTION_TIMEOUT: Duration = Duration::from_secs(35);
const UPNP_SOAP_STOP_TIMEOUT: Duration = Duration::from_millis(1500);
const UPNP_SOAP_SET_URI_TIMEOUT: Duration = Duration::from_secs(8);
const UPNP_SOAP_SET_NEXT_URI_TIMEOUT: Duration = Duration::from_secs(8);
const UPNP_SOAP_PLAY_TIMEOUT: Duration = Duration::from_secs(8);
const UPNP_SOAP_MAX_BYTES: usize = 512 * 1024;
const UPNP_VOLUME_REFRESH_TIMEOUT: Duration = Duration::from_millis(1200);
const UPNP_STARTUP_ACCEPT_TIMEOUT: Duration = Duration::from_secs(8);
const UPNP_KEF_STARTUP_PLAYING_TIMEOUT: Duration = Duration::from_secs(4);
const UPNP_PLAY_ERROR_ACCEPT_GRACE: Duration = Duration::from_millis(900);
const UPNP_HEGEL_DOP_STARTUP_EVIDENCE_TIMEOUT: Duration = Duration::from_secs(4);
const UPNP_HEGEL_DOP_RETRY_SETTLE: Duration = Duration::from_millis(180);
const UPNP_STARTUP_PLAYING_TIMEOUT: Duration = Duration::from_secs(20);
const UPNP_STARTUP_POLL_INTERVAL: Duration = Duration::from_millis(500);
const UPNP_SEEK_PLAYING_TIMEOUT: Duration = Duration::from_secs(3);
const UPNP_SEEK_PLAYING_POLL_INTERVAL: Duration = Duration::from_millis(250);
const UPNP_SEEK_NEXT_HANDOFF_SETTLE: Duration = Duration::from_secs(2);
pub(crate) const UPNP_KEF_NEXT_HANDOFF_DISABLED: &str =
    "KEF next-track handoff disabled to preserve seek stability";
const UPNP_POSITION_RESYNC_THRESHOLD_SECS: f64 = 1.1;
const UPNP_STARTUP_POSITION_AHEAD_GRACE_SECS: f64 = 2.0;
const UPNP_SEEK_PENDING_POSITION_TOLERANCE_SECS: f64 = 2.0;
const UPNP_COMPLETION_RATIO: f64 = 0.95;
const UPNP_COMPLETION_TAIL_SECONDS: f64 = 2.0;
const UPNP_ENDED_RESET_POSITION_SECS: f64 = 1.0;
const UPNP_PROBE_ACCEPT_TIMEOUT: Duration = Duration::from_secs(4);
const UPNP_PROBE_TOTAL_TIMEOUT: Duration = Duration::from_secs(45);
const UPNP_PROBE_DURATION_MS: u32 = 4_000;
const UPNP_PROBE_VERIFY_TIMEOUT: Duration = Duration::from_secs(2);
const UPNP_PROBE_VERIFY_POLL_INTERVAL: Duration = Duration::from_millis(150);
const UPNP_PCM_PROBE_LADDER: [PcmProbeCandidate; 8] = [
    PcmProbeCandidate {
        sample_rate: 192_000,
        bit_depth: 24,
    },
    PcmProbeCandidate {
        sample_rate: 176_400,
        bit_depth: 24,
    },
    PcmProbeCandidate {
        sample_rate: 96_000,
        bit_depth: 24,
    },
    PcmProbeCandidate {
        sample_rate: 88_200,
        bit_depth: 24,
    },
    PcmProbeCandidate {
        sample_rate: 48_000,
        bit_depth: 24,
    },
    PcmProbeCandidate {
        sample_rate: 44_100,
        bit_depth: 24,
    },
    PcmProbeCandidate {
        sample_rate: 48_000,
        bit_depth: 16,
    },
    PcmProbeCandidate {
        sample_rate: 44_100,
        bit_depth: 16,
    },
];
const UPNP_PCM_EXTENDED_PROBE_RATES: [u32; 4] = [768_000, 705_600, 384_000, 352_800];
const UPNP_DSD_PROBE_RATES: [u16; 3] = [64, 128, 256];
const DSF_BLOCK_SIZE_PER_CHANNEL: usize = 4096;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UpnpRendererTarget {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub model: Option<String>,
    pub manufacturer: Option<String>,
    pub av_transport_control_url: String,
    pub rendering_control_url: Option<String>,
    pub connection_manager_url: Option<String>,
    pub max_sample_rate: u32,
    pub max_bit_depth: u8,
    #[serde(default)]
    pub max_dsd_rate: Option<u16>,
    #[serde(default)]
    pub capability_detection_source: CapabilityDetectionSource,
    #[serde(default)]
    pub capability_detection_status: CapabilityDetectionStatus,
    #[serde(default)]
    pub capability_detection_message: Option<String>,
    #[serde(default)]
    pub protocol_info: Vec<String>,
    #[serde(default)]
    pub pcm_containers: Vec<UpnpPcmContainerCapability>,
}

#[derive(Debug, Clone)]
pub struct UpnpRenderer {
    pub target: UpnpRendererTarget,
    pub online: bool,
}

#[derive(Debug, Clone)]
pub struct UpnpAsset {
    pub id: String,
    pub source_ref: SourceRef,
    pub stream_url: String,
    pub mime_type: String,
    pub byte_len: Option<u64>,
    pub art_url: Option<String>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration_secs: Option<f64>,
    pub source_rate: u32,
    pub target_rate: u32,
    pub source_bits: u32,
    pub target_bits: u32,
    pub active_output_mode: Option<String>,
    pub qobuz_resolve_ms: Option<u64>,
    pub asset_registration_ms: Option<u64>,
    pub render_signature: Option<String>,
    pub configured_render_signature: Option<String>,
    pub render_ms: Option<u64>,
    pub prepare_ms: Option<u64>,
    pub cache_hit: Option<bool>,
    pub render_or_stream_plan: Option<String>,
    pub cache_lookup_ms: Option<u64>,
    pub cache_wait_ms: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct UpnpPlaybackSnapshot {
    pub state: String,
    pub file_name: Option<String>,
    pub current_source: Option<SourceRef>,
    pub track_title: Option<String>,
    pub track_artist: Option<String>,
    pub track_album: Option<String>,
    pub position_secs: f64,
    pub duration_secs: f64,
    pub source_rate: u32,
    pub target_rate: u32,
    pub source_bits: u32,
    pub target_bits: u32,
    pub active_output_mode: Option<String>,
    pub volume: Option<f32>,
    pub playback_speed: Option<String>,
    pub notice: Option<String>,
    pub config_applied_to_current_playback: bool,
    pub restart_pending: bool,
    pub render_status: String,
    pub active_render_signature: Option<String>,
    pub configured_render_signature: Option<String>,
    pub current_render_or_stream_plan: Option<String>,
    pub last_render_ms: Option<u64>,
    pub last_prepare_ms: Option<u64>,
    pub last_cache_hit: Option<bool>,
    pub transport_pending: String,
    pub transport_pending_position_secs: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct UpnpSeekOutcome {
    pub confirmed: bool,
    pub verification: Option<String>,
    pub needs_completed_render_fallback: bool,
}

// Source variants intentionally own their prepared playback state.
#[allow(clippy::large_enum_variant)]
pub enum UpnpSource {
    LocalFile {
        source_ref: SourceRef,
        path: PathBuf,
        tags: TrackTags,
        cover: Option<TrackCover>,
        byte_len: Option<u64>,
        source_rate: u32,
        source_bits: u32,
    },
    RemoteStream {
        id: String,
        source_ref: SourceRef,
        stream_url: String,
        mime_type: String,
        byte_len: Option<u64>,
        art_url: Option<String>,
        tags: TrackTags,
        source_rate: u32,
        source_bits: u32,
        qobuz_resolve_ms: Option<u64>,
        asset_registration_ms: Option<u64>,
    },
    GeneratedDspStream {
        id: String,
        zone_id: String,
        source_ref: SourceRef,
        mime_type: String,
        tags: TrackTags,
        source_rate: u32,
        source_bits: u32,
        target_rate: u32,
        target_bits: u32,
        active_output_mode: Option<String>,
        byte_len: Option<u64>,
        dop_lead_in_data_len: u64,
        target: UpnpRendererTarget,
        playback_config: PlaybackConfig,
    },
}

#[derive(Clone)]
struct CachedAsset {
    path: PathBuf,
    tokens: Vec<String>,
    art: Option<TrackCover>,
    mime_type: String,
    byte_len: Option<u64>,
    is_probe: bool,
    active_output_mode: Option<String>,
    target_bits: u32,
    expires_at: Instant,
}

#[derive(Clone)]
struct CachedGeneratedDspStream {
    tokens: Vec<String>,
    stream: UpnpGeneratedDspStream,
}

#[derive(Clone)]
pub struct UpnpCachedAsset {
    pub path: PathBuf,
    pub mime_type: String,
    pub is_probe: bool,
    pub active_output_mode: Option<String>,
    pub target_bits: u32,
}

#[derive(Clone)]
pub struct UpnpGeneratedDspStream {
    pub source_ref: SourceRef,
    pub zone_id: String,
    pub mime_type: String,
    pub byte_len: Option<u64>,
    pub source_rate: u32,
    pub source_bits: u32,
    pub target_rate: u32,
    pub target_bits: u32,
    pub active_output_mode: Option<String>,
    pub dop_lead_in_data_len: u64,
    pub target: UpnpRendererTarget,
    pub playback_config: PlaybackConfig,
    expires_at: Instant,
}

pub struct UpnpRemoteStreamMetadata {
    pub mime_type: String,
    pub byte_len: Option<u64>,
    pub qobuz_format_id: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpnpSoapTrace {
    pub action: String,
    pub attempt: u8,
    pub timeout_ms: u64,
    pub elapsed_ms: u64,
    pub ok: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpnpHttpTrace {
    pub play_id: u64,
    pub asset_id: String,
    pub kind: String,
    pub range: Option<String>,
    pub since_play_ms: Option<u64>,
    pub request_elapsed_ms: Option<u64>,
    pub elapsed_since_play_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpnpProxyTrace {
    pub play_id: u64,
    pub asset_id: String,
    pub track_id: u64,
    pub range: Option<String>,
    pub status: u16,
    pub since_play_ms: Option<u64>,
    pub request_elapsed_ms: Option<u64>,
    pub elapsed_since_play_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpnpSeekTrace {
    pub target_secs: f64,
    pub seek_advertised: Option<bool>,
    pub verification: Option<String>,
    pub elapsed_since_play_ms: Option<u64>,
    pub ok: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpnpNextHandoffTrace {
    pub asset_id: String,
    pub source_key: String,
    pub title: Option<String>,
    pub mime_type: String,
    pub stream_host: Option<String>,
    pub prepared_at_ms: u64,
    pub armed_at_ms: Option<u64>,
    pub renderer_requested_at_ms: Option<u64>,
    /// Signed offset from the expected end of the current track. Negative is early.
    pub renderer_request_relative_to_eof_ms: Option<i64>,
    pub promoted_at_ms: Option<u64>,
    pub promoted_without_play: bool,
    pub transition_path: Option<String>,
    pub fallback_reason: Option<String>,
    pub fresh_play_after_completion: bool,
    pub first_byte_after_next_ms: Option<u64>,
    pub ok: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpnpPlayTrace {
    pub play_id: u64,
    pub zone_id: String,
    pub renderer_name: String,
    pub renderer_model: Option<String>,
    pub asset_id: String,
    pub title: Option<String>,
    pub mime_type: String,
    pub byte_len: Option<u64>,
    pub stream_host: Option<String>,
    pub started_at_ms: u64,
    pub current_duration_ms: Option<u64>,
    pub total_elapsed_ms: Option<u64>,
    pub qobuz_resolve_ms: Option<u64>,
    pub asset_registration_ms: Option<u64>,
    pub render_ms: Option<u64>,
    pub prepare_ms: Option<u64>,
    pub cache_hit: Option<bool>,
    pub render_or_stream_plan: Option<String>,
    pub cache_lookup_ms: Option<u64>,
    pub cache_wait_ms: Option<u64>,
    pub render_signature: Option<String>,
    pub configured_render_signature: Option<String>,
    pub source_rate: u32,
    pub target_rate: u32,
    pub source_bits: u32,
    pub target_bits: u32,
    pub active_output_mode: Option<String>,
    pub render_container: Option<String>,
    pub first_renderer_request: Option<UpnpHttpTrace>,
    pub renderer_requests: Vec<UpnpHttpTrace>,
    pub first_local_body_byte_ms: Option<u64>,
    pub first_local_audio_payload_ms: Option<u64>,
    pub first_local_dop_frame_ms: Option<u64>,
    pub first_qobuz_proxy_byte: Option<UpnpProxyTrace>,
    pub qobuz_proxy_bytes: Vec<UpnpProxyTrace>,
    pub first_playing_observed_ms: Option<u64>,
    pub startup_phase: String,
    pub startup_confirmation: Option<String>,
    pub startup_elapsed_ms: Option<u64>,
    pub startup_accept_deadline_ms: u64,
    pub startup_playing_deadline_ms: u64,
    pub last_transport_state: Option<String>,
    pub last_refresh_error: Option<String>,
    pub stale_command_discards: u32,
    pub soap: Vec<UpnpSoapTrace>,
    pub seeks: Vec<UpnpSeekTrace>,
    pub active_seek_started_ms: Option<u64>,
    pub active_seek_renderer_request_count: usize,
    pub next_handoff: Option<UpnpNextHandoffTrace>,
    /// Retained when a failed handoff is followed by a fresh fallback play.
    pub previous_handoff: Option<UpnpNextHandoffTrace>,
    pub dop_control_policy: Option<String>,
    pub skipped_initial_stop: bool,
    pub hegel_mute_guard: Option<String>,
    pub used_renderer_next: bool,
    pub handoff_promoted_without_play: bool,
    pub dop_seek_strategy: Option<String>,
    pub first_byte_after_seek_or_next_ms: Option<u64>,
    pub notice: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpnpDiagnostics {
    pub zone_id: String,
    pub public_base_url: String,
    pub renderer: UpnpRendererTarget,
    pub warnings: Vec<String>,
    pub capability_probe: Option<UpnpCapabilityProbeDiagnostics>,
    pub last_play_trace: Option<UpnpPlayTrace>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpnpCapabilityProbeDiagnostics {
    pub renderer_id: String,
    pub source: CapabilityDetectionSource,
    pub status: CapabilityDetectionStatus,
    pub message: Option<String>,
    pub started_at_ms: u64,
    pub finished_at_ms: Option<u64>,
    pub final_max_sample_rate: u32,
    pub final_max_bit_depth: u8,
    pub final_max_dsd_rate: Option<u16>,
    pub final_pcm_containers: Vec<UpnpPcmContainerCapability>,
    pub basis: Option<String>,
    pub attempts: Vec<UpnpCapabilityProbeAttempt>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpnpCapabilityProbeAttempt {
    pub kind: String,
    pub candidate: String,
    pub mime_type: String,
    pub protocol_info: String,
    pub started_at_ms: u64,
    pub finished_at_ms: Option<u64>,
    pub accepted: bool,
    pub renderer_get: bool,
    pub renderer_head: bool,
    pub playing_observed: bool,
    pub terminal_state: Option<String>,
    pub evidence: Option<String>,
    pub error: Option<String>,
}

#[derive(Clone)]
struct CachedRemoteStream {
    tokens: Vec<String>,
    art: Option<TrackCover>,
    mime_type: String,
    byte_len: Option<u64>,
    qobuz_format_id: Option<u32>,
    expires_at: Instant,
}

#[derive(Debug, Clone)]
struct UpnpTraceContext {
    zone_id: String,
    play_id: u64,
    asset_id: String,
    expires_at: Instant,
}

#[derive(Debug, Clone)]
struct UpnpSession {
    play_id: u64,
    current: Option<UpnpAsset>,
    armed_next: Option<UpnpAsset>,
    state: String,
    started_at: Option<Instant>,
    paused_position: f64,
    playback_polled_at: Option<Instant>,
    playback_speed: Option<String>,
    volume: Option<f32>,
    volume_polled_at: Option<Instant>,
    notice: Option<String>,
    startup: Option<UpnpStartup>,
    reconfigure: UpnpReconfigureState,
    transport_pending: Option<String>,
    transport_pending_position_secs: Option<f64>,
}

impl Default for UpnpSession {
    fn default() -> Self {
        Self {
            play_id: 0,
            current: None,
            armed_next: None,
            state: "Stopped".to_string(),
            started_at: None,
            paused_position: 0.0,
            playback_polled_at: None,
            playback_speed: None,
            volume: None,
            volume_polled_at: None,
            notice: None,
            startup: None,
            reconfigure: UpnpReconfigureState::default(),
            transport_pending: None,
            transport_pending_position_secs: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum UpnpDopControlPolicy {
    Standard,
    HegelH390DopWav,
}

impl UpnpDopControlPolicy {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::HegelH390DopWav => "hegel_h390_dop_wav",
        }
    }

    pub(super) fn skips_initial_stop(self) -> bool {
        matches!(self, Self::HegelH390DopWav)
    }

    pub(super) fn startup_evidence_timeout(self) -> Option<Duration> {
        match self {
            Self::Standard => None,
            Self::HegelH390DopWav => Some(UPNP_HEGEL_DOP_STARTUP_EVIDENCE_TIMEOUT),
        }
    }
}

pub(super) fn dop_control_policy_for(
    target: &UpnpRendererTarget,
    asset: &UpnpAsset,
) -> UpnpDopControlPolicy {
    if target_is_hegel_h390(target) && asset_is_dop_wav(asset) {
        UpnpDopControlPolicy::HegelH390DopWav
    } else {
        UpnpDopControlPolicy::Standard
    }
}

pub(super) fn asset_is_dop_wav(asset: &UpnpAsset) -> bool {
    matches!(
        asset.active_output_mode.as_deref(),
        Some("Dsd64" | "Dsd128" | "Dsd256")
    ) && matches!(
        asset.mime_type.as_str(),
        "audio/wav" | "audio/wave" | "audio/x-wav"
    ) && asset.target_bits == 24
        && matches!(
            asset.target_rate,
            176_400 | 192_000 | 352_800 | 384_000 | 705_600 | 768_000
        )
}

pub(super) fn target_is_hegel_h390(target: &UpnpRendererTarget) -> bool {
    let combined = [
        target.name.as_str(),
        target.model.as_deref().unwrap_or_default(),
        target.manufacturer.as_deref().unwrap_or_default(),
    ]
    .join(" ")
    .to_ascii_lowercase();
    combined.contains("hegel") && combined.contains("h390")
}

#[derive(Debug, Clone)]
struct UpnpReconfigureState {
    generation: u64,
    restart_pending: bool,
    render_status: String,
    configured_render_signature: Option<String>,
    last_render_ms: Option<u64>,
    last_prepare_ms: Option<u64>,
    last_cache_hit: Option<bool>,
}

impl Default for UpnpReconfigureState {
    fn default() -> Self {
        Self {
            generation: 0,
            restart_pending: false,
            render_status: "idle".to_string(),
            configured_render_signature: None,
            last_render_ms: None,
            last_prepare_ms: None,
            last_cache_hit: None,
        }
    }
}

#[derive(Debug, Clone)]
struct UpnpStartup {
    play_id: u64,
    asset_id: String,
    started_at: Instant,
    accepted_at: Option<Instant>,
    accepted_reason: Option<String>,
    confirmed_playing_at: Option<Instant>,
    failed: bool,
    timed_out: bool,
}

#[derive(Debug, Clone, Default)]
struct UpnpTransportSnapshot {
    state: Option<String>,
    status: Option<String>,
    playback_speed: Option<String>,
    position_secs: Option<f64>,
    duration_secs: Option<f64>,
    current_uri: Option<String>,
}

#[derive(Debug, Clone)]
struct UpnpCapabilityInference {
    max_sample_rate: u32,
    max_bit_depth: u8,
    max_dsd_rate: Option<u16>,
    detection_source: CapabilityDetectionSource,
    detection_status: CapabilityDetectionStatus,
    detection_message: Option<String>,
    needs_probe: bool,
    pcm_containers: Vec<UpnpPcmContainerCapability>,
}

#[derive(Debug, Clone)]
struct UpnpCapabilityProbeResult {
    max_sample_rate: u32,
    max_bit_depth: u8,
    max_dsd_rate: Option<u16>,
    detection_source: CapabilityDetectionSource,
    detection_status: CapabilityDetectionStatus,
    detection_message: Option<String>,
    basis: Option<String>,
    pcm_containers: Vec<UpnpPcmContainerCapability>,
}

#[derive(Debug, Clone)]
struct UpnpProbeAcceptance {
    accepted: bool,
    renderer_get: bool,
    renderer_head: bool,
    playing_observed: bool,
    terminal_state: Option<String>,
    evidence: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpnpCapabilityCalibration {
    pub target: UpnpRendererTarget,
    pub message: String,
}

#[derive(Clone)]
pub struct UpnpRendererService {
    http: Client,
    public_base_url: String,
    renderers: Arc<Mutex<HashMap<String, UpnpRenderer>>>,
    assets: Arc<Mutex<HashMap<String, CachedAsset>>>,
    remote_streams: Arc<Mutex<HashMap<String, CachedRemoteStream>>>,
    generated_dsp_streams: Arc<Mutex<HashMap<String, CachedGeneratedDspStream>>>,
    stream_trace_contexts: Arc<Mutex<HashMap<String, UpnpTraceContext>>>,
    command_generations: Arc<Mutex<HashMap<String, u64>>>,
    seek_reservations: Arc<Mutex<HashMap<String, u64>>>,
    seek_settling_until: Arc<Mutex<HashMap<String, Instant>>>,
    sessions: Arc<Mutex<HashMap<String, UpnpSession>>>,
    command_locks: Arc<Mutex<HashMap<String, Arc<AsyncMutex<()>>>>>,
    traces: Arc<Mutex<HashMap<String, UpnpPlayTrace>>>,
    capability_probe_cache: Arc<Mutex<HashMap<String, UpnpCapabilityProbeResult>>>,
    capability_probe_diagnostics: Arc<Mutex<HashMap<String, UpnpCapabilityProbeDiagnostics>>>,
    capability_probe_tasks: Arc<Mutex<HashSet<String>>>,
    next_uri_unsupported_renderers: Arc<Mutex<HashSet<String>>>,
}

impl UpnpRendererService {
    pub fn new(public_base_url: String) -> Self {
        let service = Self {
            http: Client::builder()
                .redirect(redirect::Policy::none())
                .build()
                .expect("build UPnP HTTP client"),
            public_base_url,
            renderers: Arc::new(Mutex::new(HashMap::new())),
            assets: Arc::new(Mutex::new(HashMap::new())),
            remote_streams: Arc::new(Mutex::new(HashMap::new())),
            generated_dsp_streams: Arc::new(Mutex::new(HashMap::new())),
            stream_trace_contexts: Arc::new(Mutex::new(HashMap::new())),
            command_generations: Arc::new(Mutex::new(HashMap::new())),
            seek_reservations: Arc::new(Mutex::new(HashMap::new())),
            seek_settling_until: Arc::new(Mutex::new(HashMap::new())),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            command_locks: Arc::new(Mutex::new(HashMap::new())),
            traces: Arc::new(Mutex::new(HashMap::new())),
            capability_probe_cache: Arc::new(Mutex::new(HashMap::new())),
            capability_probe_diagnostics: Arc::new(Mutex::new(HashMap::new())),
            capability_probe_tasks: Arc::new(Mutex::new(HashSet::new())),
            next_uri_unsupported_renderers: Arc::new(Mutex::new(HashSet::new())),
        };
        #[cfg(feature = "upnp")]
        service.spawn_discovery();
        service
    }

    pub fn renderers(&self) -> Vec<UpnpRenderer> {
        self.renderers.lock().unwrap().values().cloned().collect()
    }

    #[cfg(test)]
    pub(crate) fn insert_test_renderer(&self, target: UpnpRendererTarget, online: bool) {
        self.renderers
            .lock()
            .unwrap()
            .insert(target.id.clone(), UpnpRenderer { target, online });
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct PcmProbeCandidate {
    sample_rate: u32,
    bit_depth: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PcmProbeFormat {
    Flac,
    Wav,
}

impl PcmProbeFormat {
    pub(super) fn container(self) -> UpnpPcmContainer {
        match self {
            Self::Flac => UpnpPcmContainer::Flac,
            Self::Wav => UpnpPcmContainer::Wav,
        }
    }
}

mod assets;
mod discovery;
mod probe;
mod session;
mod soap;
#[cfg(test)]
mod tests;
mod trace;
mod transport;

pub use discovery::{
    UpnpTargetRefreshKind, classify_upnp_target_refresh, is_upnp_device_name,
    parse_target_device_name, receiver_zone_id, target_capability_status_message,
    target_device_name, upnp_target_origin_label, upnp_target_origin_matches,
};
pub use probe::probe_path_is_streamable;
