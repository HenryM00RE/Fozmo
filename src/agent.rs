use crate::app::identity;
#[cfg(all(target_os = "windows", feature = "asio"))]
use crate::audio::asio_output;
use crate::audio::device_caps::{self, DEFAULT_MAX_SAMPLE_RATE};
use crate::audio::dither::DitherPreference;
use crate::audio::dsd::delta_sigma::DsdModulator;
use crate::audio::player::{LivePlaybackConfig, OutputMode, Player, PlayerSnapshot, TrackTags};
use crate::audio::resampler::{DEFAULT_FILTER_TYPE, FilterType};
use crate::cpu::ProcessCpuMonitor;
use crate::protocol::{
    AgentBufferState, AgentCapabilities, AgentPlaybackState, AgentToCoreMessage,
    CoreToAgentCommand, OutputDeviceCapabilities, PlaybackConfig, SourceRef, SyncSignalPath,
    system_audio_backend,
};
use futures_util::{SinkExt, StreamExt};
use md5::Digest;
use rand::RngCore;
use reqwest::header::{CONTENT_LENGTH, CONTENT_RANGE, HeaderMap, HeaderValue, RANGE};
use reqwest::{Client, StatusCode, Url};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use symphonia::core::io::MediaSource;
use tokio::sync::mpsc;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

struct AgentRuntimeState {
    queue: VecDeque<SourceRef>,
    prefetched: HashMap<String, AgentStreamHandle>,
    current_source: Option<SourceRef>,
    current_started_at: Option<Instant>,
    stream_base_url: Option<String>,
    generation: u64,
    loading_generation: Option<u64>,
    prefetching: bool,
    was_active: bool,
    skip_requested: bool,
    repeat_one: bool,
}

impl AgentRuntimeState {
    fn new() -> Self {
        Self {
            queue: VecDeque::new(),
            prefetched: HashMap::new(),
            current_source: None,
            current_started_at: None,
            stream_base_url: None,
            generation: 0,
            loading_generation: None,
            prefetching: false,
            was_active: false,
            skip_requested: false,
            repeat_one: false,
        }
    }
}

const AGENT_PENDING_START_GRACE: Duration = Duration::from_secs(20);
const AGENT_STREAM_PREFETCH_BYTES: u64 = 2 * 1024 * 1024;
// Each range block is fully buffered (by reqwest here and, for Qobuz, by the
// core's proxy) before any byte becomes readable, so a cold buffer fill or a
// seek stalls the decoder for the entire block transfer. Keep blocks small;
// the next-block prefetch pipeline maintains throughput between blocks.
const AGENT_STREAM_RANGE_BYTES: u64 = 8 * 1024 * 1024;
const AGENT_MAX_PREFETCHED_STREAMS: usize = 2;

struct AgentStreamHandle {
    source: AgentStreamSource,
    ext_hint: Option<String>,
    display_name: String,
    fallback_tags: TrackTags,
}

// Agent playback should prioritize the current read position. A whole-file
// progressive cache can backfill skipped bytes after a seek and starve playback.
struct AgentStreamSource {
    client: Client,
    url: Url,
    headers: HeaderMap,
    runtime: tokio::runtime::Handle,
    position: u64,
    byte_len: Option<u64>,
    buffer_start: u64,
    buffer: Vec<u8>,
    range_bytes: u64,
    next_block: Option<AgentRangeBlock>,
    prefetch: Option<AgentRangePrefetch>,
}

impl Read for AgentStreamSource {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.byte_len.is_some_and(|len| self.position >= len) {
            return Ok(0);
        }
        self.collect_prefetch();
        if !self.buffer_contains(self.position) {
            self.load_buffer_at(self.position)?;
            if self.buffer.is_empty() {
                return Ok(0);
            }
        }
        self.start_prefetch_next();

        let offset = self.position.saturating_sub(self.buffer_start) as usize;
        let available = self.buffer.len().saturating_sub(offset);
        if available == 0 {
            return Ok(0);
        }

        let read_len = available.min(buf.len());
        buf[..read_len].copy_from_slice(&self.buffer[offset..offset + read_len]);
        self.position = self.position.saturating_add(read_len as u64);
        self.collect_prefetch();
        self.start_prefetch_next();
        Ok(read_len)
    }
}

impl Seek for AgentStreamSource {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        let target = match pos {
            SeekFrom::Start(pos) => pos as i128,
            SeekFrom::Current(delta) => self.position as i128 + delta as i128,
            SeekFrom::End(delta) => {
                let Some(len) = self.byte_len else {
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        "cannot seek from end without a known stream length",
                    ));
                };
                len as i128 + delta as i128
            }
        };
        if target < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek before start of stream",
            ));
        }

        let mut target = target as u64;
        if let Some(len) = self.byte_len {
            target = target.min(len);
        }
        self.position = target;
        if !self.buffer_contains(target) && !self.next_block_contains(target) {
            let keep_prefetch = self
                .prefetch
                .as_ref()
                .is_some_and(|prefetch| prefetch.could_contain(target));
            if !keep_prefetch {
                self.prefetch = None;
            }
            self.next_block = None;
        }
        Ok(self.position)
    }
}

impl MediaSource for AgentStreamSource {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        self.byte_len
    }
}

impl AgentStreamSource {
    async fn open(token: &str, url: Url) -> Result<Self, String> {
        let client = Client::new();
        let headers = agent_auth_headers(token)?;
        let initial = fetch_agent_range(
            client.clone(),
            url.clone(),
            headers.clone(),
            0,
            AGENT_STREAM_PREFETCH_BYTES,
            None,
        )
        .await?;

        let mut source = Self {
            client,
            url,
            headers,
            runtime: tokio::runtime::Handle::current(),
            position: 0,
            byte_len: initial.byte_len,
            buffer_start: initial.start,
            buffer: initial.bytes,
            range_bytes: AGENT_STREAM_RANGE_BYTES,
            next_block: None,
            prefetch: None,
        };
        source.start_prefetch_next();
        Ok(source)
    }

    fn buffer_contains(&self, position: u64) -> bool {
        let buffer_end = self.buffer_start.saturating_add(self.buffer.len() as u64);
        position >= self.buffer_start && position < buffer_end
    }

    fn next_block_contains(&self, position: u64) -> bool {
        self.next_block
            .as_ref()
            .is_some_and(|block| block.contains(position))
    }

    fn buffer_end(&self) -> u64 {
        self.buffer_start.saturating_add(self.buffer.len() as u64)
    }

    fn load_buffer_at(&mut self, start: u64) -> io::Result<()> {
        self.collect_prefetch();
        if self.install_next_block_if_contains(start) {
            self.start_prefetch_next();
            return Ok(());
        }
        if self.wait_for_prefetch_if_contains(start)? {
            self.start_prefetch_next();
            return Ok(());
        }

        let block = self
            .runtime
            .block_on(fetch_agent_range(
                self.client.clone(),
                self.url.clone(),
                self.headers.clone(),
                start,
                self.range_bytes,
                self.byte_len,
            ))
            .map_err(io::Error::other)?;
        self.install_block(block);
        self.start_prefetch_next();
        Ok(())
    }

    fn install_block(&mut self, block: AgentRangeBlock) {
        self.byte_len = block.byte_len.or(self.byte_len);
        self.buffer_start = block.start;
        self.buffer = block.bytes;
    }

    fn install_next_block_if_contains(&mut self, position: u64) -> bool {
        let Some(block) = self.next_block.take() else {
            return false;
        };
        if block.contains(position) || block.is_empty_at(position, self.byte_len) {
            self.install_block(block);
            true
        } else {
            self.next_block = Some(block);
            false
        }
    }

    fn wait_for_prefetch_if_contains(&mut self, position: u64) -> io::Result<bool> {
        let Some(prefetch) = self.prefetch.take() else {
            return Ok(false);
        };
        if !prefetch.could_contain(position) {
            self.prefetch = Some(prefetch);
            return Ok(false);
        }

        match prefetch.wait() {
            Ok(block) if block.contains(position) || block.is_empty_at(position, self.byte_len) => {
                self.install_block(block);
                Ok(true)
            }
            Ok(block) => {
                self.next_block = Some(block);
                Ok(false)
            }
            Err(err) => {
                eprintln!("agent: stream lookahead failed: {err}; retrying inline");
                Ok(false)
            }
        }
    }

    fn collect_prefetch(&mut self) {
        let Some(prefetch) = self.prefetch.as_ref() else {
            return;
        };
        let Some(result) = prefetch.try_take() else {
            return;
        };
        self.prefetch = None;
        match result {
            Ok(block) => {
                self.byte_len = block.byte_len.or(self.byte_len);
                if !block.bytes.is_empty() {
                    self.next_block = Some(block);
                }
            }
            Err(err) => eprintln!("agent: stream lookahead failed: {err}"),
        }
    }

    fn start_prefetch_next(&mut self) {
        self.collect_prefetch();
        if self.prefetch.is_some() || self.next_block.is_some() || self.buffer.is_empty() {
            return;
        }

        let start = self.buffer_end();
        if self.byte_len.is_some_and(|len| start >= len) {
            return;
        }

        let result = Arc::new((Mutex::new(None), Condvar::new()));
        let task_result = Arc::clone(&result);
        let client = self.client.clone();
        let url = self.url.clone();
        let headers = self.headers.clone();
        let range_bytes = self.range_bytes;
        let known_len = self.byte_len;
        let task = self.runtime.spawn(async move {
            let fetched =
                fetch_agent_range(client, url, headers, start, range_bytes, known_len).await;
            let (lock, cvar) = &*task_result;
            *lock.lock().unwrap() = Some(fetched);
            cvar.notify_all();
        });
        self.prefetch = Some(AgentRangePrefetch {
            start,
            range_bytes,
            result,
            task,
        });
    }
}

struct AgentRangeBlock {
    start: u64,
    bytes: Vec<u8>,
    byte_len: Option<u64>,
}

type AgentRangePrefetchResult = Arc<(Mutex<Option<Result<AgentRangeBlock, String>>>, Condvar)>;

impl AgentRangeBlock {
    fn contains(&self, position: u64) -> bool {
        let end = self.start.saturating_add(self.bytes.len() as u64);
        position >= self.start && position < end
    }

    fn is_empty_at(&self, position: u64, known_len: Option<u64>) -> bool {
        self.bytes.is_empty()
            && position == self.start
            && known_len
                .or(self.byte_len)
                .is_some_and(|len| position >= len)
    }
}

struct AgentRangePrefetch {
    start: u64,
    range_bytes: u64,
    result: AgentRangePrefetchResult,
    task: tokio::task::JoinHandle<()>,
}

impl AgentRangePrefetch {
    fn could_contain(&self, position: u64) -> bool {
        position >= self.start && position < self.start.saturating_add(self.range_bytes)
    }

    fn try_take(&self) -> Option<Result<AgentRangeBlock, String>> {
        self.result.0.lock().unwrap().take()
    }

    fn wait(self) -> Result<AgentRangeBlock, String> {
        let (lock, cvar) = &*self.result;
        let mut result = lock.lock().unwrap();
        while result.is_none() {
            result = cvar.wait(result).unwrap();
        }
        result.take().unwrap()
    }
}

impl Drop for AgentRangePrefetch {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn fetch_agent_range(
    client: Client,
    url: Url,
    headers: HeaderMap,
    start: u64,
    range_bytes: u64,
    known_len: Option<u64>,
) -> Result<AgentRangeBlock, String> {
    if known_len.is_some_and(|len| start >= len) {
        return Ok(AgentRangeBlock {
            start,
            bytes: Vec::new(),
            byte_len: known_len,
        });
    }

    let mut end = capped_range_end(start, None, range_bytes);
    if let Some(len) = known_len
        && len > 0
    {
        end = end.min(len - 1);
    }

    let request = client
        .get(url.clone())
        .headers(headers)
        .header(RANGE, format_range_header_bytes(start, Some(end)));

    let response = request
        .send()
        .await
        .map_err(|e| format!("agent range request failed: {e}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("agent range request returned HTTP {status}"));
    }

    let response_headers = response.headers().clone();
    let response_len = header_content_length(&response_headers);
    if status == StatusCode::OK && (start != 0 || response_len.is_some_and(|len| len > range_bytes))
    {
        return Err("agent stream server ignored byte range request".to_string());
    }
    let byte_len = content_range_total(&response_headers).or_else(|| {
        if status == StatusCode::OK {
            response_len
        } else {
            None
        }
    });
    let bytes = response
        .bytes()
        .await
        .map_err(|e| format!("read agent stream range: {e}"))?
        .to_vec();

    Ok(AgentRangeBlock {
        start,
        bytes,
        byte_len: byte_len.or(known_len),
    })
}

fn agent_auth_headers(token: &str) -> Result<HeaderMap, String> {
    let mut headers = HeaderMap::new();
    let token = token.trim();
    if !token.is_empty() {
        let value = HeaderValue::from_str(token)
            .map_err(|e| format!("invalid pairing token header: {e}"))?;
        headers.insert(identity::AUTH_HEADER, value);
    }
    Ok(headers)
}

fn capped_range_end(start: u64, requested_end: Option<u64>, range_bytes: u64) -> u64 {
    let max_end = start.saturating_add(range_bytes.saturating_sub(1));
    requested_end.map_or(max_end, |end| end.min(max_end))
}

fn format_range_header_bytes(start: u64, end: Option<u64>) -> String {
    format!(
        "bytes={start}-{}",
        end.map(|end| end.to_string()).unwrap_or_default()
    )
}

fn header_content_length(headers: &HeaderMap) -> Option<u64> {
    headers
        .get(CONTENT_LENGTH)?
        .to_str()
        .ok()?
        .parse::<u64>()
        .ok()
}

fn content_range_total(headers: &HeaderMap) -> Option<u64> {
    let value = headers.get(CONTENT_RANGE)?.to_str().ok()?;
    let (_, total) = value.rsplit_once('/')?;
    if total == "*" {
        None
    } else {
        total.parse::<u64>().ok()
    }
}

fn resolve_core_url() -> String {
    explicit_core_url().unwrap_or_else(|| {
        eprintln!(
            "agent: no core URL supplied; using http://127.0.0.1:3000. \
Pass --core-url or FOZMO_CORE_URL to connect to a LAN core."
        );
        "http://127.0.0.1:3000".to_string()
    })
}

fn explicit_core_url() -> Option<String> {
    std::env::var(identity::env_key("CORE_URL"))
        .ok()
        .filter(|url| !url.trim().is_empty())
        .or_else(|| {
            std::env::args().find_map(|arg| {
                arg.strip_prefix("--core-url=")
                    .map(str::trim)
                    .filter(|url| !url.is_empty())
                    .map(str::to_string)
            })
        })
}

fn agent_platform_label() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "Mac"
    }
    #[cfg(target_os = "windows")]
    {
        "Windows PC"
    }
    #[cfg(target_os = "linux")]
    {
        "Linux"
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        "Device"
    }
}

pub async fn run_agent() -> Result<(), Box<dyn std::error::Error>> {
    let core_url = resolve_core_url();
    let explicit_agent_token = std::env::var(identity::env_key("AGENT_TOKEN"))
        .ok()
        .or_else(|| {
            std::env::args().find_map(|arg| {
                arg.strip_prefix("--agent-token=")
                    .map(str::trim)
                    .filter(|token| !token.is_empty())
                    .map(str::to_string)
            })
        });
    let legacy_pairing_token = std::env::var(identity::env_key("PAIRING_TOKEN"))
        .ok()
        .or_else(|| {
            std::env::args().find_map(|arg| {
                arg.strip_prefix("--token=")
                    .map(str::trim)
                    .filter(|token| !token.is_empty())
                    .map(str::to_string)
            })
        });
    let mut token = explicit_agent_token
        .or_else(|| {
            if legacy_pairing_token.is_some() {
                eprintln!(
                    "agent: FOZMO_PAIRING_TOKEN/--token is deprecated; use FOZMO_AGENT_TOKEN/--agent-token."
                );
            }
            legacy_pairing_token
        })
        .unwrap_or_default();
    let agent_name = std::env::var(identity::env_key("AGENT_NAME"))
        .ok()
        .or_else(|| {
            std::env::args().find_map(|arg| arg.strip_prefix("--agent-name=").map(str::to_string))
        })
        .unwrap_or_else(hostname_fallback);
    let http = Client::new();
    let agent_id = stable_agent_id(&agent_name);
    let ws_url = agent_ws_url(&core_url)?;

    println!(
        "Starting {} {} Agent: {agent_name}",
        identity::APP_DISPLAY_NAME,
        agent_platform_label()
    );
    println!("Connecting to Core at {ws_url}");

    let player = Arc::new(Player::new());
    let runtime = Arc::new(Mutex::new(AgentRuntimeState::new()));
    let cpu_monitor = Arc::new(Mutex::new(ProcessCpuMonitor::new()));
    let mut ws_request = ws_url.clone().into_client_request()?;
    if !token.trim().is_empty() {
        ws_request
            .headers_mut()
            .insert(identity::AUTH_HEADER, token.trim().parse()?);
    }
    let (ws, _) = connect_async(ws_request).await?;
    let (mut ws_write, mut ws_read) = ws.split();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<AgentToCoreMessage>();

    let write_task = tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            if let Ok(body) = serde_json::to_string(&msg)
                && ws_write.send(Message::Text(body.into())).await.is_err()
            {
                break;
            }
        }
    });

    let output_device_capabilities = output_device_capabilities();
    log_agent_output_device_summary(&output_device_capabilities);
    let output_devices = output_device_capabilities
        .iter()
        .map(|caps| caps.name.clone())
        .collect::<Vec<_>>();
    let max_sample_rate = output_device_capabilities
        .iter()
        .map(|caps| caps.max_sample_rate)
        .max()
        .unwrap_or(DEFAULT_MAX_SAMPLE_RATE);
    let supports_dsd128 = output_device_capabilities
        .iter()
        .any(|caps| caps.supports_dsd128);
    let supports_dsd256 = output_device_capabilities
        .iter()
        .any(|caps| caps.supports_dsd256);

    let _ = out_tx.send(AgentToCoreMessage::Register {
        agent_id: agent_id.clone(),
        name: agent_name.clone(),
        capabilities: AgentCapabilities {
            output_devices,
            output_device_capabilities,
            max_sample_rate,
            max_bit_depth: 32,
            exclusive_supported: cfg!(target_os = "windows"),
            supports_dsd128,
            supports_dsd256,
            browser: false,
        },
    });

    let status_player = Arc::clone(&player);
    let status_runtime = Arc::clone(&runtime);
    let status_cpu_monitor = Arc::clone(&cpu_monitor);
    let status_tx = out_tx.clone();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_millis(100));
        loop {
            ticker.tick().await;
            let snapshot = status_player.snapshot_no_cover();
            let signal = signal_path_snapshot(&snapshot, &status_cpu_monitor);
            let (current_source, buffer) = {
                let rt = status_runtime.lock().unwrap();
                (
                    rt.current_source.clone(),
                    AgentBufferState {
                        buffered_next: rt.prefetched.keys().next().cloned(),
                        prefetching: rt.prefetching,
                        buffered_bytes: rt
                            .prefetched
                            .values()
                            .filter_map(|handle| handle.source.byte_len)
                            .sum(),
                    },
                )
            };
            let playback = playback_snapshot(&snapshot, current_source);
            let _ = status_tx.send(AgentToCoreMessage::PlaybackState(playback));
            let _ = status_tx.send(AgentToCoreMessage::SyncSignalPath(signal));
            let _ = status_tx.send(AgentToCoreMessage::BufferState(buffer));
        }
    });

    let mut tick = tokio::time::interval(Duration::from_millis(250));
    loop {
        tokio::select! {
            msg = ws_read.next() => {
                let Some(msg) = msg else { break; };
                let msg = msg?;
                if let Message::Text(body) = msg {
                    match serde_json::from_str::<CoreToAgentCommand>(&body) {
                        Ok(cmd) => handle_command(
                            cmd,
                            &player,
                            &http,
                            &mut token,
                            &runtime,
                            &core_url,
                        ).await,
                        Err(e) => eprintln!("agent: invalid command: {e}"),
                    }
                }
            }
            _ = tick.tick() => {
                maybe_advance_gapless(&player, &http, token.clone(), &runtime, &core_url).await;
            }
        }
    }

    write_task.abort();
    Ok(())
}

async fn handle_command(
    cmd: CoreToAgentCommand,
    player: &Arc<Player>,
    http: &Client,
    token: &mut String,
    runtime: &Arc<Mutex<AgentRuntimeState>>,
    core_url: &str,
) {
    match cmd {
        CoreToAgentCommand::PlaySource {
            source_ref,
            queue,
            playback_config,
            stream_base_url,
        } => {
            let token = token.clone();
            let stream_base_url = reachable_stream_base_url(&stream_base_url, core_url);
            apply_playback_config(player, playback_config);
            let play_epoch = player.reserve_playback_change();
            let generation = {
                let mut rt = runtime.lock().unwrap();
                rt.generation = rt.generation.wrapping_add(1);
                let generation = rt.generation;
                rt.queue = queue.into();
                rt.current_source = Some(source_ref.clone());
                rt.current_started_at = Some(Instant::now());
                rt.stream_base_url = Some(stream_base_url.clone());
                rt.loading_generation = Some(generation);
                rt.was_active = false;
                rt.skip_requested = false;
                retain_relevant_prefetches_with_preferred(&mut rt, Some(&source_ref.key()));
                generation
            };
            let player = Arc::clone(player);
            let http = http.clone();
            let runtime = Arc::clone(runtime);
            tokio::spawn(async move {
                if let Err(e) = play_source(
                    &player,
                    &http,
                    &token,
                    &runtime,
                    &stream_base_url,
                    source_ref,
                    generation,
                    play_epoch,
                )
                .await
                {
                    clear_loading_generation(&runtime, generation);
                    eprintln!("agent: play source failed: {e}");
                }
            });
        }
        CoreToAgentCommand::PreFetch {
            source_ref,
            stream_base_url,
        } => {
            let token = token.clone();
            let stream_base_url = reachable_stream_base_url(&stream_base_url, core_url);
            let http = http.clone();
            let runtime = Arc::clone(runtime);
            tokio::spawn(async move {
                if let Err(e) =
                    prefetch_source(&http, &token, &runtime, &stream_base_url, source_ref).await
                {
                    eprintln!("agent: prefetch failed: {e}");
                }
            });
        }
        CoreToAgentCommand::Pause => player.pause(),
        CoreToAgentCommand::Resume => player.resume(),
        CoreToAgentCommand::Stop => {
            player.stop();
            let mut rt = runtime.lock().unwrap();
            rt.generation = rt.generation.wrapping_add(1);
            rt.queue.clear();
            rt.prefetched.clear();
            rt.current_source = None;
            rt.current_started_at = None;
            rt.stream_base_url = None;
            rt.loading_generation = None;
            rt.prefetching = false;
            rt.was_active = false;
            rt.skip_requested = false;
        }
        CoreToAgentCommand::Next => {
            runtime.lock().unwrap().skip_requested = true;
            player.next();
        }
        CoreToAgentCommand::Seek { seconds } => player.seek(seconds),
        CoreToAgentCommand::SetQueue { queue } => {
            runtime.lock().unwrap().queue = queue.into();
        }
        CoreToAgentCommand::SetLoopMode { repeat_one } => {
            player.set_repeat_one(repeat_one);
            runtime.lock().unwrap().repeat_one = repeat_one;
        }
        CoreToAgentCommand::SetPlaybackConfig { playback_config } => {
            apply_playback_config(player, playback_config);
        }
        CoreToAgentCommand::AuthorizeStreams {
            token: stream_token,
        } => {
            *token = stream_token;
            println!("Agent media streaming authorized by core.");
        }
        CoreToAgentCommand::Heartbeat => {}
    }
}

struct GaplessPlay {
    source: SourceRef,
    generation: u64,
    base_url: String,
}

async fn maybe_advance_gapless(
    player: &Arc<Player>,
    http: &Client,
    token: String,
    runtime: &Arc<Mutex<AgentRuntimeState>>,
    fallback_base_url: &str,
) {
    let player_state = player.playback_state();
    let player_file_name = player.current_file_name();
    let next_play = {
        let mut rt = runtime.lock().unwrap();
        if rt.loading_generation.is_some() {
            return;
        }
        let has_current = rt.current_source.is_some();
        let current_matches_player = agent_source_matches_player_file(
            rt.current_source.as_ref(),
            player_file_name.as_deref(),
        );
        if !player_state.is_stopped() && !rt.skip_requested {
            if current_matches_player {
                rt.was_active = true;
            }
            None
        } else if !has_current {
            rt.skip_requested = false;
            None
        } else {
            let timed_out_pending_start = rt
                .current_started_at
                .as_ref()
                .is_some_and(|started| started.elapsed() >= AGENT_PENDING_START_GRACE);
            let should_select_source =
                rt.skip_requested || rt.was_active || timed_out_pending_start;
            if !should_select_source {
                return;
            }

            let source = if rt.repeat_one && !rt.skip_requested {
                rt.current_source.clone()
            } else {
                rt.queue.pop_front()
            };
            rt.was_active = false;
            rt.skip_requested = false;

            if let Some(source) = source {
                rt.generation = rt.generation.wrapping_add(1);
                let generation = rt.generation;
                rt.current_source = Some(source.clone());
                rt.current_started_at = Some(Instant::now());
                rt.loading_generation = Some(generation);
                retain_relevant_prefetches(&mut rt);
                let base_url = rt
                    .stream_base_url
                    .clone()
                    .unwrap_or_else(|| fallback_base_url.trim_end_matches('/').to_string());
                Some(GaplessPlay {
                    source,
                    generation,
                    base_url,
                })
            } else {
                rt.current_source = None;
                rt.current_started_at = None;
                rt.loading_generation = None;
                retain_relevant_prefetches(&mut rt);
                None
            }
        }
    };

    if let Some(GaplessPlay {
        source,
        generation,
        base_url,
    }) = next_play
    {
        let play_epoch = player.reserve_playback_change();
        let player = Arc::clone(player);
        let http = http.clone();
        let runtime = Arc::clone(runtime);
        let token = token.clone();
        tokio::spawn(async move {
            if let Err(e) = play_source(
                &player, &http, &token, &runtime, &base_url, source, generation, play_epoch,
            )
            .await
            {
                clear_loading_generation(&runtime, generation);
                eprintln!("agent: gapless advance failed: {e}");
            }
        });
    }
}

fn clear_loading_generation(runtime: &Arc<Mutex<AgentRuntimeState>>, generation: u64) {
    let mut rt = runtime.lock().unwrap();
    if rt.generation == generation {
        rt.loading_generation = None;
    }
}

// Agent playback hands off source, auth, runtime, and epoch data from the remote-control loop.
#[allow(clippy::too_many_arguments)]
async fn play_source(
    player: &Player,
    _http: &Client,
    token: &str,
    runtime: &Arc<Mutex<AgentRuntimeState>>,
    base_url: &str,
    source: SourceRef,
    generation: u64,
    play_epoch: u64,
) -> Result<(), String> {
    let key = source.key();
    let prefetched = {
        let mut rt = runtime.lock().unwrap();
        retain_relevant_prefetches_with_preferred(&mut rt, Some(&key));
        rt.prefetched.remove(&key)
    };
    let handle = match prefetched {
        Some(handle) => handle,
        None => open_stream_source(token, base_url, &source).await?,
    };
    let still_current = {
        let rt = runtime.lock().unwrap();
        rt.generation == generation
            && rt
                .current_source
                .as_ref()
                .is_some_and(|current| current.key() == key)
    };
    if !still_current {
        return Ok(());
    }
    if !player.play_stream_if_epoch(
        play_epoch,
        Box::new(handle.source),
        handle.ext_hint,
        handle.display_name,
        None,
        Some(handle.fallback_tags),
        Vec::new(),
    ) {
        let mut rt = runtime.lock().unwrap();
        if rt.generation == generation {
            rt.loading_generation = None;
        }
        return Ok(());
    }
    let mut rt = runtime.lock().unwrap();
    if rt.generation == generation {
        rt.current_started_at = Some(Instant::now());
        rt.loading_generation = None;
        rt.was_active = false;
    }
    Ok(())
}

async fn prefetch_source(
    _http: &Client,
    token: &str,
    runtime: &Arc<Mutex<AgentRuntimeState>>,
    base_url: &str,
    source: SourceRef,
) -> Result<(), String> {
    let key = source.key();
    {
        let mut rt = runtime.lock().unwrap();
        retain_relevant_prefetches(&mut rt);
        if !prefetch_key_is_relevant(&rt, &key) || rt.prefetched.contains_key(&key) {
            return Ok(());
        }
        rt.prefetching = true;
    }
    let result = open_stream_source(token, base_url, &source).await;
    let mut rt = runtime.lock().unwrap();
    rt.prefetching = false;
    match result {
        Ok(handle) => {
            retain_relevant_prefetches(&mut rt);
            if prefetch_key_is_relevant(&rt, &key) {
                insert_prefetched(&mut rt, key, handle);
            }
            Ok(())
        }
        Err(e) => Err(e),
    }
}

fn prefetch_key_is_relevant(rt: &AgentRuntimeState, key: &str) -> bool {
    rt.current_source
        .as_ref()
        .is_some_and(|source| source.key() == key)
        || rt.queue.iter().any(|source| source.key() == key)
}

fn agent_source_matches_player_file(
    source: Option<&SourceRef>,
    player_file_name: Option<&str>,
) -> bool {
    source.is_some_and(|source| {
        player_file_name.is_some_and(|file_name| source_display_name(source) == file_name)
    })
}

fn retain_relevant_prefetches(rt: &mut AgentRuntimeState) {
    retain_relevant_prefetches_with_preferred(rt, None);
}

fn retain_relevant_prefetches_with_preferred(
    rt: &mut AgentRuntimeState,
    preferred_key: Option<&str>,
) {
    let relevant_keys = relevant_prefetch_keys(rt);
    rt.prefetched
        .retain(|key, _| relevant_keys.contains(key.as_str()));
    trim_prefetched_to_limit(rt, preferred_key);
}

fn relevant_prefetch_keys(rt: &AgentRuntimeState) -> HashSet<String> {
    let mut keys = HashSet::new();
    if let Some(source) = &rt.current_source {
        keys.insert(source.key());
    }
    keys.extend(rt.queue.iter().map(SourceRef::key));
    keys
}

fn insert_prefetched(rt: &mut AgentRuntimeState, key: String, handle: AgentStreamHandle) {
    rt.prefetched.insert(key.clone(), handle);
    trim_prefetched_to_limit(rt, Some(&key));
}

fn trim_prefetched_to_limit(rt: &mut AgentRuntimeState, preferred_key: Option<&str>) {
    while rt.prefetched.len() > AGENT_MAX_PREFETCHED_STREAMS {
        let victim = rt
            .prefetched
            .keys()
            .find(|key| preferred_key != Some(key.as_str()))
            .cloned()
            .or_else(|| rt.prefetched.keys().next().cloned());
        let Some(victim) = victim else {
            break;
        };
        rt.prefetched.remove(&victim);
    }
}

async fn open_stream_source(
    token: &str,
    base_url: &str,
    source: &SourceRef,
) -> Result<AgentStreamHandle, String> {
    let url = source_stream_url(base_url, source);
    let url = Url::parse(&url).map_err(|e| format!("parse stream URL: {e}"))?;
    let source_handle = AgentStreamSource::open(token, url).await?;
    Ok(AgentStreamHandle {
        source: source_handle,
        ext_hint: source_ext_hint(source),
        display_name: source_display_name(source),
        fallback_tags: source_fallback_tags(source),
    })
}

fn source_stream_url(base_url: &str, source: &SourceRef) -> String {
    let base = base_url.trim_end_matches('/');
    match source {
        SourceRef::LocalTrack { track_id, .. } => format!("{base}/api/stream/local/{track_id}"),
        SourceRef::QobuzTrack { track_id, .. } => format!("{base}/api/stream/qobuz/{track_id}"),
    }
}

fn reachable_stream_base_url(advertised_base_url: &str, core_url: &str) -> String {
    let advertised = advertised_base_url.trim();
    if advertised.is_empty() {
        return core_url.trim_end_matches('/').to_string();
    }
    if url_uses_loopback(advertised) && !url_uses_loopback(core_url) {
        core_url.trim_end_matches('/').to_string()
    } else {
        advertised.trim_end_matches('/').to_string()
    }
}

fn url_uses_loopback(url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    let Some(host) = parsed.host_str() else {
        return false;
    };
    host.eq_ignore_ascii_case("localhost") || host == "::1" || host.starts_with("127.")
}

fn source_ext_hint(source: &SourceRef) -> Option<String> {
    match source {
        SourceRef::LocalTrack { ext_hint, .. } => ext_hint.clone(),
        SourceRef::QobuzTrack { .. } => Some("flac".to_string()),
    }
}

fn source_display_name(source: &SourceRef) -> String {
    fn from_parts(title: Option<&str>, artist: Option<&str>, fallback: String) -> String {
        match (title, artist) {
            (Some(title), Some(artist)) if !title.is_empty() && !artist.is_empty() => {
                format!("{artist} - {title}")
            }
            (Some(title), _) if !title.is_empty() => title.to_string(),
            _ => fallback,
        }
    }

    match source {
        SourceRef::LocalTrack {
            title,
            artist,
            track_id,
            ..
        } => from_parts(
            title.as_deref(),
            artist.as_deref(),
            format!("local-{track_id}"),
        ),
        SourceRef::QobuzTrack {
            title,
            artist,
            track_id,
            ..
        } => from_parts(
            title.as_deref(),
            artist.as_deref(),
            format!("qobuz-{track_id}"),
        ),
    }
}

fn source_fallback_tags(source: &SourceRef) -> TrackTags {
    let mut tags = TrackTags::default();
    match source {
        SourceRef::LocalTrack {
            title,
            artist,
            album,
            ..
        }
        | SourceRef::QobuzTrack {
            title,
            artist,
            album,
            ..
        } => {
            tags.title = title.clone();
            tags.artist = artist.clone();
            tags.album = album.clone();
            tags.album_artist = artist.clone();
        }
    }
    tags
}

fn apply_playback_config(player: &Player, cfg: PlaybackConfig) {
    let filter = FilterType::from_name(&cfg.filter_type).unwrap_or(DEFAULT_FILTER_TYPE);
    let output_mode = OutputMode::from_name(&cfg.output_mode).unwrap_or(OutputMode::Pcm);
    let dsd_modulator = DsdModulator::from_name(&cfg.dsd_modulator).unwrap_or_default();
    player.apply_playback_config(LivePlaybackConfig {
        filter_type: filter,
        target_rate: cfg.target_rate,
        upsampling_enabled: cfg.upsampling_enabled,
        exclusive: cfg.exclusive,
        dsp_buffer_ms: cfg.dsp_buffer_ms,
        output_mode,
        dsd_modulator,
        dsd_isi_penalty: cfg.dsd_isi_penalty,
        dsd_rules: cfg.dsd_rules,
        eq: Some(cfg.eq),
    });
    let dither = DitherPreference::from_name(&cfg.dither_mode).unwrap_or(DitherPreference::Auto);
    player.set_dither_mode(dither.as_id());
    player.set_headroom_db(cfg.headroom_db);
    player.set_volume(cfg.volume);
    if let Some(device) = cfg.output_device {
        player.select_device(Some(device));
    }
}

fn playback_snapshot(
    snapshot: &PlayerSnapshot,
    current_source: Option<SourceRef>,
) -> AgentPlaybackState {
    let signal = &snapshot.signal_path;
    let metrics = &snapshot.metrics;
    let source_rate = signal.source_rate;
    let target_rate = signal.target_rate;
    AgentPlaybackState {
        state: snapshot.state.as_name().to_string(),
        current_source,
        file_name: snapshot.file_name.clone(),
        track_title: snapshot.track_tags.title.clone(),
        track_artist: snapshot.track_tags.artist.clone(),
        track_album: snapshot.track_tags.album.clone(),
        position_secs: if target_rate > 0 {
            metrics.position_samples as f64 / target_rate as f64
        } else {
            0.0
        },
        duration_secs: if source_rate > 0 {
            metrics.duration_samples as f64 / source_rate as f64
        } else {
            0.0
        },
        source_rate,
        target_rate,
        source_bits: signal.source_bits,
        target_bits: signal.target_bits,
        volume: snapshot.config.volume,
    }
}

fn signal_path_snapshot(
    snapshot: &PlayerSnapshot,
    cpu_monitor: &Arc<Mutex<ProcessCpuMonitor>>,
) -> SyncSignalPath {
    let signal = &snapshot.signal_path;
    let config = &snapshot.config;
    let metrics = &snapshot.metrics;
    let diagnostics = &snapshot.diagnostics;
    let dsd_buffer_health = metrics.dsd_buffer_health.as_ref();
    SyncSignalPath {
        source_format: signal.source_format.clone(),
        source_rate: signal.source_rate,
        source_bit_depth: signal.source_bits,
        dsp_filter: config
            .filter_type
            .map(|f| f.as_name().to_string())
            .unwrap_or_else(|| "Unknown".to_string()),
        dsp_target_rate: signal.target_rate,
        src_path_kind: signal.src_path_kind.map(|kind| kind.as_name().to_string()),
        src_capped_fallback: signal.src_capped_fallback,
        src_phase_profile_preserved: signal.src_phase_profile_preserved,
        src_ratio_num: signal.src_ratio_num,
        src_ratio_den: signal.src_ratio_den,
        output_device: signal.output_device.clone(),
        output_rate: signal.target_rate,
        output_bit_depth: signal.target_bits,
        output_mode: Some(signal.output_mode.as_name().to_string()),
        active_output_mode: Some(signal.active_output_mode.as_name().to_string()),
        output_transport: Some(signal.output_transport.as_name().to_string()),
        dsd_stability_resets: signal.dsd_stability_resets,
        dsd_modulator: Some(config.dsd_modulator.as_name().to_string()),
        exclusive: config.exclusive,
        cpu_percent: cpu_monitor.lock().unwrap().sample_percent(),
        resample_time_ns: metrics.resample_time_ns,
        dsd_upsample_time_ns: metrics.dsd_upsample_time_ns,
        dsd_modulate_time_ns: metrics.dsd_modulate_time_ns,
        dsd_output_pending_samples: metrics.dsd_output_pending_samples,
        dsd_buffer_health: metrics.dsd_buffer_health.clone(),
        dop_ring_capacity_ms: dsd_buffer_health
            .map(|health| health.ring_capacity_ms)
            .unwrap_or_default(),
        dop_ring_fill_ms: dsd_buffer_health
            .map(|health| health.ring_fill_ms)
            .unwrap_or_default(),
        dop_ring_low_watermark_ms: dsd_buffer_health
            .map(|health| health.ring_low_watermark_ms)
            .unwrap_or_default(),
        dop_callback_frames: dsd_buffer_health
            .map(|health| health.callback_frames)
            .unwrap_or_default(),
        dop_callback_ms: dsd_buffer_health
            .map(|health| health.callback_ms)
            .unwrap_or_default(),
        dop_requested_hardware_buffer_frames: dsd_buffer_health
            .map(|health| health.requested_hardware_buffer_frames)
            .unwrap_or_default(),
        dop_requested_hardware_buffer_ms: dsd_buffer_health
            .map(|health| health.requested_hardware_buffer_ms)
            .unwrap_or_default(),
        dop_hardware_buffer_min_frames: dsd_buffer_health
            .map(|health| health.hardware_buffer_min_frames)
            .unwrap_or_default(),
        dop_hardware_buffer_max_frames: dsd_buffer_health
            .map(|health| health.hardware_buffer_max_frames)
            .unwrap_or_default(),
        dop_hardware_buffer_frames: dsd_buffer_health
            .map(|health| health.hardware_buffer_frames)
            .unwrap_or_default(),
        dop_hardware_buffer_ms: dsd_buffer_health
            .map(|health| health.hardware_buffer_ms)
            .unwrap_or_default(),
        dop_lock_miss_events: dsd_buffer_health
            .map(|health| health.lock_miss_events)
            .unwrap_or_default(),
        dop_callback_deadline_miss_events: dsd_buffer_health
            .map(|health| health.callback_deadline_miss_events)
            .unwrap_or_default(),
        dop_soft_callback_gap_125_events: dsd_buffer_health
            .map(|health| health.soft_callback_gap_125_events)
            .unwrap_or_default(),
        dop_soft_callback_gap_150_events: dsd_buffer_health
            .map(|health| health.soft_callback_gap_150_events)
            .unwrap_or_default(),
        dop_soft_callback_gap_175_events: dsd_buffer_health
            .map(|health| health.soft_callback_gap_175_events)
            .unwrap_or_default(),
        dop_last_soft_callback_gap_ms: dsd_buffer_health
            .map(|health| health.last_soft_callback_gap_ms)
            .unwrap_or_default(),
        dop_last_soft_callback_gap_at_ms: dsd_buffer_health
            .map(|health| health.last_soft_callback_gap_at_ms)
            .unwrap_or_default(),
        dop_ring_below_250ms_events: dsd_buffer_health
            .map(|health| health.ring_below_250ms_events)
            .unwrap_or_default(),
        dop_ring_below_100ms_events: dsd_buffer_health
            .map(|health| health.ring_below_100ms_events)
            .unwrap_or_default(),
        dop_ring_below_50ms_events: dsd_buffer_health
            .map(|health| health.ring_below_50ms_events)
            .unwrap_or_default(),
        dop_ring_below_callback_events: dsd_buffer_health
            .map(|health| health.ring_below_callback_events)
            .unwrap_or_default(),
        dop_last_ring_pressure_at_ms: dsd_buffer_health
            .map(|health| health.last_ring_pressure_at_ms)
            .unwrap_or_default(),
        dop_marker_error_events: dsd_buffer_health
            .map(|health| health.marker_error_events)
            .unwrap_or_default(),
        dop_program_idle_splice_events: dsd_buffer_health
            .map(|health| health.program_idle_splice_events)
            .unwrap_or_default(),
        dop_program_to_idle_events: dsd_buffer_health
            .map(|health| health.program_to_idle_events)
            .unwrap_or_default(),
        dop_idle_to_program_events: dsd_buffer_health
            .map(|health| health.idle_to_program_events)
            .unwrap_or_default(),
        dop_mixed_output_events: dsd_buffer_health
            .map(|health| health.mixed_output_events)
            .unwrap_or_default(),
        dop_last_output_transition_id: dsd_buffer_health
            .map(|health| health.last_output_transition_id)
            .unwrap_or_default(),
        dop_last_output_transition_at_ms: dsd_buffer_health
            .map(|health| health.last_output_transition_at_ms)
            .unwrap_or_default(),
        dop_repeated_payload_events: dsd_buffer_health
            .map(|health| health.repeated_payload_events)
            .unwrap_or_default(),
        dop_callback_index: dsd_buffer_health
            .map(|health| health.callback_index)
            .unwrap_or_default(),
        dop_last_callback_at_ms: dsd_buffer_health
            .map(|health| health.last_callback_at_ms)
            .unwrap_or_default(),
        dop_last_callback_gap_ms: dsd_buffer_health
            .map(|health| health.last_callback_gap_ms)
            .unwrap_or_default(),
        dop_last_callback_frames: dsd_buffer_health
            .map(|health| health.last_callback_frames)
            .unwrap_or_default(),
        dop_last_output_kind_id: dsd_buffer_health
            .map(|health| health.last_output_kind_id)
            .unwrap_or_default(),
        dop_last_ring_fill_samples: dsd_buffer_health
            .map(|health| health.last_ring_fill_samples)
            .unwrap_or_default(),
        dop_last_program_read_samples: dsd_buffer_health
            .map(|health| health.last_program_read_samples)
            .unwrap_or_default(),
        dop_ring_read_cursor_samples: dsd_buffer_health
            .map(|health| health.ring_read_cursor_samples)
            .unwrap_or_default(),
        dop_last_payload_fingerprint: dsd_buffer_health
            .map(|health| health.last_payload_fingerprint)
            .unwrap_or_default(),
        dop_last_payload_fingerprint_at_ms: dsd_buffer_health
            .map(|health| health.last_payload_fingerprint_at_ms)
            .unwrap_or_default(),
        dop_marker_scan_count: dsd_buffer_health
            .map(|health| health.marker_scan_count)
            .unwrap_or_default(),
        dop_every_callback_scan_enabled: dsd_buffer_health
            .map(|health| health.every_callback_scan_enabled)
            .unwrap_or_default(),
        dop_last_underrun_at_ms: dsd_buffer_health
            .map(|health| health.last_underrun_at_ms)
            .unwrap_or_default(),
        dsd_overbudget_blocks: metrics.dsd_overbudget_blocks,
        dsd_last_load: metrics.dsd_last_load,
        dsd_recent_load_p95: metrics.dsd_recent_load_p95,
        dsd_recent_load_p99: metrics.dsd_recent_load_p99,
        block_duration_ns: metrics.block_duration_ns,
        output_ring_fill_now_ms: diagnostics.output_ring_fill_now_ms,
        output_ring_fill_min_ms: diagnostics.output_ring_fill_min_ms,
        startup_ring_low_watermark_ms: diagnostics.startup_ring_low_watermark_ms,
        startup_ready_ms: diagnostics.startup_ready_ms,
        startup_first_render_block_ms: diagnostics.startup_first_render_block_ms,
        startup_producer_over_budget_count: diagnostics.startup_producer_over_budget_count,
        startup_callback_gaps_ms: diagnostics.startup_callback_gaps_ms.clone(),
        underrun_count: diagnostics.underrun_count,
        producer_over_budget_count: diagnostics.producer_over_budget_count,
        max_render_block_ms: diagnostics.max_render_block_ms,
        max_audio_callback_gap_ms: diagnostics.max_audio_callback_gap_ms,
        dsp_graph_rebuild_count: diagnostics.dsp_graph_rebuild_count,
        sample_rate_change_count: diagnostics.sample_rate_change_count,
        dop_alignment_reset_count: diagnostics.dop_alignment_reset_count,
        coreaudio_dop_open_count: diagnostics.coreaudio_dop_open_count,
        coreaudio_dop_start_count: diagnostics.coreaudio_dop_start_count,
        coreaudio_dop_stop_count: diagnostics.coreaudio_dop_stop_count,
        coreaudio_dop_drop_count: diagnostics.coreaudio_dop_drop_count,
        coreaudio_dop_quiesce_count: diagnostics.coreaudio_dop_quiesce_count,
        coreaudio_dop_last_lifecycle_event_id: diagnostics.coreaudio_dop_last_lifecycle_event_id,
        coreaudio_dop_last_lifecycle_at_ms: diagnostics.coreaudio_dop_last_lifecycle_at_ms,
        reopen_reason_count: diagnostics.reopen_reason_count,
        last_reopen_reason_id: diagnostics.last_reopen_reason_id,
        last_reopen_reason_at_ms: diagnostics.last_reopen_reason_at_ms,
        flush_reason_count: diagnostics.flush_reason_count,
        last_flush_reason_id: diagnostics.last_flush_reason_id,
        last_flush_reason_at_ms: diagnostics.last_flush_reason_at_ms,
        modulator_reset_count: diagnostics.modulator_reset_count,
        decoder_starved_count: diagnostics.decoder_starved_count,
        source_read_time_ms: diagnostics.source_read_time_ms,
        max_source_read_ms: diagnostics.max_source_read_ms,
        source_read_stall_count: diagnostics.source_read_stall_count,
        source_read_stall_last_at_ms: diagnostics.source_read_stall_last_at_ms,
        decoder_decode_time_ms: diagnostics.decoder_decode_time_ms,
        max_decoder_decode_ms: diagnostics.max_decoder_decode_ms,
        decoder_decode_stall_count: diagnostics.decoder_decode_stall_count,
        decoder_decode_stall_last_at_ms: diagnostics.decoder_decode_stall_last_at_ms,
        lock_wait_max_ms: diagnostics.lock_wait_max_ms,
        signal_peak: metrics.signal_peak,
        signal_peak_max: metrics.signal_peak_max,
        signal_clipping: metrics.signal_clipping,
        signal_clip_events: metrics.signal_clip_events,
        signal_clip_samples: metrics.signal_clip_samples,
        dsd_limiter_peak_ratio: metrics.dsd_limiter_peak_ratio,
        dsd_limiter_peak_ratio_max: metrics.dsd_limiter_peak_ratio_max,
        dsd_limiter_active: metrics.dsd_limiter_active,
        dsd_limiter_events: metrics.dsd_limiter_events,
        dsd_limiter_samples: metrics.dsd_limiter_samples,
        underrun_events: metrics.underrun_events,
        underrun_samples: metrics.underrun_samples,
    }
}

fn cpal_output_device_names() -> Vec<String> {
    use cpal::traits::{DeviceTrait, HostTrait};
    cpal::default_host()
        .output_devices()
        .map(|devices| devices.filter_map(|d| d.name().ok()).collect())
        .unwrap_or_default()
}

fn output_device_capabilities() -> Vec<OutputDeviceCapabilities> {
    let mut devices = Vec::new();
    for name in cpal_output_device_names() {
        if devices
            .iter()
            .any(|caps: &OutputDeviceCapabilities| caps.name == name)
        {
            continue;
        }
        let caps = device_caps::output_device_capabilities(Some(&name));
        devices.push(OutputDeviceCapabilities {
            name,
            backend: Some(system_audio_backend().to_string()),
            max_sample_rate: caps.max_sample_rate,
            max_bit_depth: caps.max_bit_depth,
            supports_dsd128: caps.supports_dsd128,
            supports_dsd256: caps.supports_dsd256,
        });
    }

    #[cfg(all(target_os = "windows", feature = "asio"))]
    for driver in asio_output::list_devices() {
        let name = format!("ASIO: {driver}");
        if devices.iter().any(|caps| caps.name == name) {
            continue;
        }
        let caps = device_caps::output_device_capabilities(Some(&name));
        devices.push(OutputDeviceCapabilities {
            name,
            backend: Some("asio".to_string()),
            max_sample_rate: caps.max_sample_rate,
            max_bit_depth: caps.max_bit_depth,
            supports_dsd128: caps.supports_dsd128,
            supports_dsd256: caps.supports_dsd256,
        });
    }

    devices
}

fn log_agent_output_device_summary(devices: &[OutputDeviceCapabilities]) {
    #[cfg(all(target_os = "windows", feature = "asio"))]
    let asio_count = devices
        .iter()
        .filter(|caps| caps.backend.as_deref() == Some("asio"))
        .count();
    #[cfg(all(target_os = "windows", feature = "asio"))]
    {
        println!(
            "AudioWorker: ASIO support enabled; discovered {asio_count} ASIO output device(s)."
        );
    }
    #[cfg(all(target_os = "windows", not(feature = "asio")))]
    {
        println!(
            "AudioWorker: ASIO support is not compiled into this binary; rebuild with `cargo build --release --features asio` to enumerate ASIO drivers."
        );
    }
    println!(
        "AudioWorker: Advertised {} output device(s) to core.",
        devices.len()
    );
}

fn agent_ws_url(core_url: &str) -> Result<String, String> {
    let mut base = core_url.trim_end_matches('/').to_string();
    if base.starts_with("https://") {
        base = base.replacen("https://", "wss://", 1);
    } else if base.starts_with("http://") {
        base = base.replacen("http://", "ws://", 1);
    } else {
        base = format!("ws://{base}");
    }
    Ok(format!("{base}/api/agent/ws"))
}

fn stable_agent_id(name: &str) -> String {
    if let Some(agent_id) = std::env::var(identity::env_key("AGENT_ID"))
        .ok()
        .and_then(|id| normalize_agent_id(&id))
    {
        return agent_id;
    }

    if let Some(path) = agent_id_file_path() {
        if let Ok(existing) = fs::read_to_string(&path)
            && let Some(agent_id) = normalize_agent_id(&existing)
        {
            return agent_id;
        }

        let agent_id = generate_agent_id();
        if write_agent_id_file(&path, &agent_id).is_ok() {
            return agent_id;
        }
        eprintln!(
            "agent: could not persist agent id at {}; using hostname fallback",
            path.display()
        );
    }

    let seed = format!("{name}:{}", hostname_fallback());
    format!("agent-{:x}", md5::Md5::digest(seed.as_bytes()))
}

fn normalize_agent_id(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with("agent-") {
        Some(trimmed.to_string())
    } else {
        Some(format!("agent-{trimmed}"))
    }
}

fn generate_agent_id() -> String {
    let mut bytes = [0_u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    let mut body = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        body.push_str(&format!("{byte:02x}"));
    }
    format!("agent-{body}")
}

fn agent_id_file_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(identity::env_key("AGENT_ID_FILE")) {
        let path = PathBuf::from(path);
        if !path.as_os_str().is_empty() {
            return Some(path);
        }
    }
    agent_config_dir().map(|dir| dir.join("agent-id"))
}

fn agent_config_dir() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("LOCALAPPDATA")
            .or_else(|| std::env::var_os("APPDATA"))
            .map(PathBuf::from)
            .map(|dir| dir.join(identity::DATA_DIR_NAME))
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME").map(PathBuf::from).map(|home| {
            home.join("Library")
                .join("Application Support")
                .join(identity::DATA_DIR_NAME)
        })
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .map(|home| home.join(".config"))
            })
            .map(|dir| dir.join(identity::APP_SLUG))
    }
}

fn write_agent_id_file(path: &Path, agent_id: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, format!("{agent_id}\n"))
}

fn hostname_fallback() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "Windows PC".to_string())
}

#[cfg(test)]
mod tests {
    use super::{agent_source_matches_player_file, normalize_agent_id, reachable_stream_base_url};
    use crate::protocol::SourceRef;

    #[test]
    fn loopback_stream_base_uses_non_loopback_core_url() {
        let rewritten =
            reachable_stream_base_url("http://localhost:9090", "https://music.example.com");

        assert_eq!(rewritten, "https://music.example.com");
    }

    #[test]
    fn loopback_stream_base_does_not_preserve_advertised_port_or_path() {
        let rewritten = reachable_stream_base_url(
            "http://127.0.0.1:9090/audio",
            "https://music.example.com/proxy",
        );

        assert_eq!(rewritten, "https://music.example.com/proxy");
    }

    #[test]
    fn non_loopback_stream_base_is_left_alone() {
        let base = reachable_stream_base_url("http://10.0.0.50:9090", "http://10.0.0.12:3000");

        assert_eq!(base, "http://10.0.0.50:9090");
    }

    #[test]
    fn agent_ids_are_normalized_once() {
        assert_eq!(
            normalize_agent_id("0123456789abcdef").as_deref(),
            Some("agent-0123456789abcdef")
        );
        assert_eq!(
            normalize_agent_id(" agent-existing ").as_deref(),
            Some("agent-existing")
        );
        assert!(normalize_agent_id("   ").is_none());
    }

    #[test]
    fn agent_source_active_check_requires_requested_display_name() {
        let fuses = SourceRef::QobuzTrack {
            track_id: 1,
            title: Some("Fuses".to_string()),
            artist: Some("Stereolab".to_string()),
            album: None,
            album_id: None,
            image_url: None,
            duration_secs: None,
            radio: false,
            radio_context: None,
            playlist_context: None,
        };

        assert!(agent_source_matches_player_file(
            Some(&fuses),
            Some("Stereolab - Fuses")
        ));
        assert!(!agent_source_matches_player_file(
            Some(&fuses),
            Some("Stereolab - People Do It All The Time")
        ));
        assert!(!agent_source_matches_player_file(Some(&fuses), None));
    }
}
