use super::capabilities::{
    agent_platform_label, log_agent_output_device_summary, output_device_capabilities,
};
use super::identity::{agent_ws_url, hostname_fallback, resolve_core_url, stable_agent_id};
use super::prefetch::{
    AgentStreamHandle, agent_source_matches_player_file, arm_buffered_front_for_gapless,
    play_source, prefetch_source, reachable_stream_base_url, retain_relevant_prefetches,
    retain_relevant_prefetches_with_preferred, source_display_name,
    synchronize_gapless_engine_advance,
};
use super::runtime::{AGENT_PENDING_START_GRACE, AgentRuntimeState};
use crate::app::identity;
use crate::audio::device_caps::DEFAULT_MAX_SAMPLE_RATE;
use crate::audio::dither::DitherPreference;
use crate::audio::dsd::delta_sigma::DsdModulator;
use crate::audio::player::{LivePlaybackConfig, OutputMode, Player, PlayerSnapshot};
use crate::audio::resampler::{DEFAULT_FILTER_TYPE, FilterType};
use crate::cpu::ProcessCpuMonitor;
use crate::protocol::{
    AgentBufferState, AgentCapabilities, AgentPlaybackState, AgentToCoreMessage,
    CoreToAgentCommand, PlaybackConfig, SourceRef, SyncSignalPath,
};
use futures_util::{SinkExt, StreamExt};
use reqwest::Client;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

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
                let mut rt = status_runtime.lock().unwrap();
                synchronize_gapless_engine_advance(
                    &mut rt,
                    snapshot.file_name.as_deref(),
                    status_player.stream_queue_len(),
                );
                let engine_buffered = rt
                    .engine_prefetched
                    .as_ref()
                    .map(|prefetched| (prefetched.source.key(), prefetched.buffered_bytes));
                (
                    rt.current_source.clone(),
                    AgentBufferState {
                        buffered_next: engine_buffered
                            .as_ref()
                            .map(|(key, _)| key.clone())
                            .or_else(|| rt.prefetched.keys().next().cloned()),
                        prefetching: rt.prefetching_key.is_some(),
                        buffered_bytes: engine_buffered.map(|(_, bytes)| bytes).unwrap_or_default()
                            + rt.prefetched
                                .values()
                                .map(AgentStreamHandle::byte_len)
                                .sum::<u64>(),
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
                rt.engine_prefetched = None;
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
            let player = Arc::clone(player);
            let runtime = Arc::clone(runtime);
            tokio::spawn(async move {
                if let Err(e) = prefetch_source(
                    &player,
                    &http,
                    &token,
                    &runtime,
                    &stream_base_url,
                    source_ref,
                )
                .await
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
            rt.engine_prefetched = None;
            rt.current_source = None;
            rt.current_started_at = None;
            rt.stream_base_url = None;
            rt.loading_generation = None;
            rt.prefetching_key = None;
            rt.was_active = false;
            rt.skip_requested = false;
        }
        CoreToAgentCommand::Next => {
            runtime.lock().unwrap().skip_requested = true;
            player.next();
        }
        CoreToAgentCommand::Seek { seconds } => player.seek(seconds),
        CoreToAgentCommand::SetQueue { queue } => {
            let mut rt = runtime.lock().unwrap();
            rt.queue = queue.into();
            retain_relevant_prefetches(&mut rt);
            let keep_engine_queue = rt.engine_prefetched.as_ref().is_some_and(|prefetched| {
                rt.queue
                    .front()
                    .is_some_and(|source| source.key() == prefetched.source.key())
            });
            if !keep_engine_queue {
                rt.engine_prefetched = None;
                player.set_stream_queue_if_epoch(
                    Vec::new(),
                    rt.current_source.as_ref().map(source_display_name),
                    Some(player.playback_epoch()),
                );
                arm_buffered_front_for_gapless(player, &mut rt);
            }
        }
        CoreToAgentCommand::SetLoopMode { repeat_one } => {
            player.set_repeat_one(repeat_one);
            let mut rt = runtime.lock().unwrap();
            rt.repeat_one = repeat_one;
            if repeat_one && rt.engine_prefetched.take().is_some() {
                player.set_stream_queue_if_epoch(
                    Vec::new(),
                    rt.current_source.as_ref().map(source_display_name),
                    Some(player.playback_epoch()),
                );
            } else if !repeat_one {
                arm_buffered_front_for_gapless(player, &mut rt);
            }
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
                rt.engine_prefetched = None;
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
        eq_enabled: Some(snapshot.eq_config.enabled),
        eq_active_bands: Some(
            snapshot
                .eq_config
                .bands
                .iter()
                .filter(|band| band.enabled)
                .count() as u32,
        ),
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
