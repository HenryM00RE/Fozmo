use crate::cpu::ProcessCpuMonitor;
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const MAX_POP_DIAGNOSTIC_ENTRIES: usize = 96;
const POP_DIAGNOSTIC_SCHEMA_VERSION: u32 = 2;
const DEVICE_ACTIVITY_NEAR_POP_MS: u64 = 1_000;

const STATUS_COUNTER_PATHS: &[(&str, &[&str])] = &[
    ("underrun_events", &["underrun_events"]),
    ("underrun_samples", &["underrun_samples"]),
    (
        "dop_callback_deadline_miss_events",
        &["dop_callback_deadline_miss_events"],
    ),
    (
        "dop_soft_callback_gap_125_events",
        &["dop_soft_callback_gap_125_events"],
    ),
    (
        "dop_soft_callback_gap_150_events",
        &["dop_soft_callback_gap_150_events"],
    ),
    (
        "dop_soft_callback_gap_175_events",
        &["dop_soft_callback_gap_175_events"],
    ),
    (
        "dop_ring_below_250ms_events",
        &["dop_ring_below_250ms_events"],
    ),
    (
        "dop_ring_below_100ms_events",
        &["dop_ring_below_100ms_events"],
    ),
    (
        "dop_ring_below_50ms_events",
        &["dop_ring_below_50ms_events"],
    ),
    (
        "dop_ring_below_callback_events",
        &["dop_ring_below_callback_events"],
    ),
    ("dop_marker_error_events", &["dop_marker_error_events"]),
    (
        "dop_program_idle_splice_events",
        &["dop_program_idle_splice_events"],
    ),
    (
        "dop_program_to_idle_events",
        &["dop_program_to_idle_events"],
    ),
    (
        "dop_idle_to_program_events",
        &["dop_idle_to_program_events"],
    ),
    ("dop_mixed_output_events", &["dop_mixed_output_events"]),
    (
        "dop_last_output_transition_id",
        &["dop_last_output_transition_id"],
    ),
    (
        "dop_last_output_transition_at_ms",
        &["dop_last_output_transition_at_ms"],
    ),
    (
        "dop_repeated_payload_events",
        &["dop_repeated_payload_events"],
    ),
    ("dop_callback_index", &["dop_callback_index"]),
    ("dop_last_callback_at_ms", &["dop_last_callback_at_ms"]),
    ("dop_last_callback_frames", &["dop_last_callback_frames"]),
    ("dop_last_output_kind_id", &["dop_last_output_kind_id"]),
    (
        "dop_last_ring_fill_samples",
        &["dop_last_ring_fill_samples"],
    ),
    (
        "dop_last_program_read_samples",
        &["dop_last_program_read_samples"],
    ),
    (
        "dop_ring_read_cursor_samples",
        &["dop_ring_read_cursor_samples"],
    ),
    (
        "dop_last_payload_fingerprint",
        &["dop_last_payload_fingerprint"],
    ),
    (
        "dop_last_payload_fingerprint_at_ms",
        &["dop_last_payload_fingerprint_at_ms"],
    ),
    ("dop_marker_scan_count", &["dop_marker_scan_count"]),
    ("dop_lock_miss_events", &["dop_lock_miss_events"]),
    ("source_read_stall_count", &["source_read_stall_count"]),
    (
        "decoder_decode_stall_count",
        &["decoder_decode_stall_count"],
    ),
    ("decoder_starved_count", &["decoder_starved_count"]),
    ("sample_rate_change_count", &["sample_rate_change_count"]),
    ("dsp_graph_rebuild_count", &["dsp_graph_rebuild_count"]),
    ("coreaudio_dop_open_count", &["coreaudio_dop_open_count"]),
    ("coreaudio_dop_start_count", &["coreaudio_dop_start_count"]),
    ("coreaudio_dop_stop_count", &["coreaudio_dop_stop_count"]),
    ("coreaudio_dop_drop_count", &["coreaudio_dop_drop_count"]),
    (
        "coreaudio_dop_quiesce_count",
        &["coreaudio_dop_quiesce_count"],
    ),
    (
        "coreaudio_dop_last_lifecycle_event_id",
        &["coreaudio_dop_last_lifecycle_event_id"],
    ),
    (
        "coreaudio_dop_last_lifecycle_at_ms",
        &["coreaudio_dop_last_lifecycle_at_ms"],
    ),
    ("reopen_reason_count", &["reopen_reason_count"]),
    ("last_reopen_reason_id", &["last_reopen_reason_id"]),
    ("last_reopen_reason_at_ms", &["last_reopen_reason_at_ms"]),
    ("flush_reason_count", &["flush_reason_count"]),
    ("last_flush_reason_id", &["last_flush_reason_id"]),
    ("last_flush_reason_at_ms", &["last_flush_reason_at_ms"]),
    ("dsd_overbudget_blocks", &["dsd_overbudget_blocks"]),
    ("dsd_stability_resets", &["dsd_stability_resets"]),
    ("dsd_limiter_events", &["dsd_limiter_events"]),
    ("signal_clip_events", &["signal_clip_events"]),
    ("signal_clip_samples", &["signal_clip_samples"]),
    ("output_notice_id", &["output_notice_id"]),
];

#[derive(Clone)]
pub struct DiagnosticsService {
    cpu_monitor: Arc<Mutex<ProcessCpuMonitor>>,
    pop_log: Arc<Mutex<PopDiagnosticLog>>,
    activity: Arc<DiagnosticActivityStats>,
}

#[derive(Clone, Serialize)]
pub struct PopDiagnosticEntry {
    pub id: u64,
    pub marked_at_ms: u64,
    pub snapshot: Value,
    pub diagnostics: PopDiagnosticContext,
}

#[derive(Clone, Serialize)]
pub struct PopDiagnosticsExport {
    pub exported_at_ms: u64,
    pub activity: DiagnosticActivitySnapshot,
    pub summary: PopDiagnosticsSummary,
    pub entries: Vec<PopDiagnosticEntry>,
}

#[derive(Default)]
struct PopDiagnosticLog {
    next_id: u64,
    entries: VecDeque<PopDiagnosticEntry>,
}

#[derive(Clone, Copy)]
pub(crate) enum DiagnosticActivity {
    ApiZonesRefresh,
    ApiDevicesList,
    LocalAudioDeviceScan,
    LocalAudioDeviceCapabilityProbe,
}

#[derive(Default)]
struct DiagnosticActivityStats {
    api_zones_refresh: ActivityCounters,
    api_devices_list: ActivityCounters,
    local_audio_device_scan: ActivityCounters,
    local_audio_device_capability_probe: ActivityCounters,
}

#[derive(Default)]
struct ActivityCounters {
    started: AtomicU64,
    finished: AtomicU64,
    in_flight: AtomicU64,
    last_started_at_ms: AtomicU64,
    last_finished_at_ms: AtomicU64,
    last_duration_ns: AtomicU64,
    max_duration_ns: AtomicU64,
}

#[derive(Clone, Default, Serialize)]
pub struct DiagnosticActivitySnapshot {
    pub api_zones_refresh: DiagnosticActivityCountersSnapshot,
    pub api_devices_list: DiagnosticActivityCountersSnapshot,
    pub local_audio_device_scan: DiagnosticActivityCountersSnapshot,
    pub local_audio_device_capability_probe: DiagnosticActivityCountersSnapshot,
}

#[derive(Clone, Default, Serialize)]
pub struct DiagnosticActivityCountersSnapshot {
    pub started: u64,
    pub finished: u64,
    pub in_flight: u64,
    pub last_started_at_ms: u64,
    pub last_finished_at_ms: u64,
    pub last_duration_ms: f64,
    pub max_duration_ms: f64,
}

#[derive(Clone, Default, Serialize)]
pub struct DiagnosticActivityDeltaSnapshot {
    pub api_zones_refresh: DiagnosticActivityCountersDelta,
    pub api_devices_list: DiagnosticActivityCountersDelta,
    pub local_audio_device_scan: DiagnosticActivityCountersDelta,
    pub local_audio_device_capability_probe: DiagnosticActivityCountersDelta,
}

#[derive(Clone, Default, Serialize)]
pub struct DiagnosticActivityCountersDelta {
    pub started: u64,
    pub finished: u64,
}

#[derive(Clone, Default, Serialize)]
pub struct DiagnosticActivityRecencySnapshot {
    pub api_zones_refresh_ms_since_last_finish: Option<u64>,
    pub api_devices_list_ms_since_last_finish: Option<u64>,
    pub local_audio_device_scan_ms_since_last_finish: Option<u64>,
    pub local_audio_device_capability_probe_ms_since_last_finish: Option<u64>,
}

#[derive(Clone, Default, Serialize)]
pub struct PopDiagnosticContext {
    pub schema_version: u32,
    pub previous_mark_delta_ms: Option<u64>,
    pub previous_position_delta_secs: Option<f64>,
    pub status_counters: BTreeMap<String, u64>,
    pub status_counter_delta_since_previous_pop: BTreeMap<String, u64>,
    pub activity: DiagnosticActivitySnapshot,
    pub activity_delta_since_previous_pop: DiagnosticActivityDeltaSnapshot,
    pub activity_recency: DiagnosticActivityRecencySnapshot,
    pub suspicion_flags: Vec<String>,
}

#[derive(Clone, Default, Serialize)]
pub struct PopDiagnosticsSummary {
    pub entry_count: usize,
    pub suspicion_flag_counts: BTreeMap<String, u64>,
    pub activity_delta_across_export: DiagnosticActivityDeltaSnapshot,
}

pub(crate) struct DiagnosticActivityGuard {
    activity: Arc<DiagnosticActivityStats>,
    kind: DiagnosticActivity,
    started: Instant,
    active: bool,
}

impl DiagnosticsService {
    pub(crate) fn new() -> Self {
        Self {
            cpu_monitor: Arc::new(Mutex::new(ProcessCpuMonitor::new())),
            pop_log: Arc::new(Mutex::new(PopDiagnosticLog::default())),
            activity: Arc::new(DiagnosticActivityStats::default()),
        }
    }

    pub(crate) fn sample_cpu_percent(&self) -> f32 {
        self.cpu_monitor.lock().unwrap().sample_percent()
    }

    pub(crate) fn begin_activity(&self, kind: DiagnosticActivity) -> DiagnosticActivityGuard {
        let counters = self.activity.counters(kind);
        counters.started.fetch_add(1, Ordering::Relaxed);
        counters.in_flight.fetch_add(1, Ordering::Relaxed);
        counters
            .last_started_at_ms
            .store(unix_epoch_millis(), Ordering::Relaxed);
        DiagnosticActivityGuard {
            activity: Arc::clone(&self.activity),
            kind,
            started: Instant::now(),
            active: true,
        }
    }

    pub(crate) fn activity_snapshot(&self) -> DiagnosticActivitySnapshot {
        self.activity.snapshot()
    }

    pub(crate) fn record_pop_snapshot<T: Serialize>(&self, snapshot: &T) -> PopDiagnosticEntry {
        let marked_at_ms = unix_epoch_millis();
        let snapshot = serde_json::to_value(snapshot).unwrap_or(Value::Null);
        let activity = self.activity_snapshot();
        let mut log = self.pop_log.lock().unwrap();
        log.next_id = log.next_id.saturating_add(1);
        let diagnostics =
            build_pop_diagnostic_context(marked_at_ms, &snapshot, activity, log.entries.back());
        let entry = PopDiagnosticEntry {
            id: log.next_id,
            marked_at_ms,
            snapshot,
            diagnostics,
        };
        log.entries.push_back(entry.clone());
        while log.entries.len() > MAX_POP_DIAGNOSTIC_ENTRIES {
            log.entries.pop_front();
        }
        entry
    }

    pub(crate) fn export_pop_log(&self) -> PopDiagnosticsExport {
        let log = self.pop_log.lock().unwrap();
        let activity = self.activity_snapshot();
        let entries: Vec<_> = log.entries.iter().cloned().collect();
        let summary = build_pop_diagnostics_summary(&entries);
        PopDiagnosticsExport {
            exported_at_ms: unix_epoch_millis(),
            activity,
            summary,
            entries,
        }
    }
}

impl Default for DiagnosticsService {
    fn default() -> Self {
        Self::new()
    }
}

fn unix_epoch_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

impl DiagnosticActivityStats {
    fn counters(&self, kind: DiagnosticActivity) -> &ActivityCounters {
        match kind {
            DiagnosticActivity::ApiZonesRefresh => &self.api_zones_refresh,
            DiagnosticActivity::ApiDevicesList => &self.api_devices_list,
            DiagnosticActivity::LocalAudioDeviceScan => &self.local_audio_device_scan,
            DiagnosticActivity::LocalAudioDeviceCapabilityProbe => {
                &self.local_audio_device_capability_probe
            }
        }
    }

    fn snapshot(&self) -> DiagnosticActivitySnapshot {
        DiagnosticActivitySnapshot {
            api_zones_refresh: self.api_zones_refresh.snapshot(),
            api_devices_list: self.api_devices_list.snapshot(),
            local_audio_device_scan: self.local_audio_device_scan.snapshot(),
            local_audio_device_capability_probe: self
                .local_audio_device_capability_probe
                .snapshot(),
        }
    }
}

impl ActivityCounters {
    fn snapshot(&self) -> DiagnosticActivityCountersSnapshot {
        DiagnosticActivityCountersSnapshot {
            started: self.started.load(Ordering::Relaxed),
            finished: self.finished.load(Ordering::Relaxed),
            in_flight: self.in_flight.load(Ordering::Relaxed),
            last_started_at_ms: self.last_started_at_ms.load(Ordering::Relaxed),
            last_finished_at_ms: self.last_finished_at_ms.load(Ordering::Relaxed),
            last_duration_ms: nanos_to_millis(self.last_duration_ns.load(Ordering::Relaxed)),
            max_duration_ms: nanos_to_millis(self.max_duration_ns.load(Ordering::Relaxed)),
        }
    }
}

impl Drop for DiagnosticActivityGuard {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let elapsed_ns = self.started.elapsed().as_nanos().min(u64::MAX as u128) as u64;
        let counters = self.activity.counters(self.kind);
        counters.finished.fetch_add(1, Ordering::Relaxed);
        counters.in_flight.fetch_sub(1, Ordering::Relaxed);
        counters
            .last_finished_at_ms
            .store(unix_epoch_millis(), Ordering::Relaxed);
        counters
            .last_duration_ns
            .store(elapsed_ns, Ordering::Relaxed);
        update_atomic_max(&counters.max_duration_ns, elapsed_ns);
        self.active = false;
    }
}

fn build_pop_diagnostic_context(
    marked_at_ms: u64,
    snapshot: &Value,
    activity: DiagnosticActivitySnapshot,
    previous: Option<&PopDiagnosticEntry>,
) -> PopDiagnosticContext {
    let status_counters = extract_status_counters(snapshot);
    let previous_status_counters = previous
        .map(|entry| entry.diagnostics.status_counters.clone())
        .unwrap_or_default();
    let status_counter_delta_since_previous_pop =
        counter_map_delta(&status_counters, &previous_status_counters);
    let previous_activity = previous
        .map(|entry| &entry.diagnostics.activity)
        .cloned()
        .unwrap_or_default();
    let activity_delta_since_previous_pop = activity_delta(&activity, &previous_activity);
    let activity_recency = activity_recency(marked_at_ms, &activity);
    let previous_mark_delta_ms =
        previous.map(|entry| marked_at_ms.saturating_sub(entry.marked_at_ms));
    let previous_position_delta_secs = previous.and_then(|entry| {
        Some(snapshot_position_secs(snapshot)? - snapshot_position_secs(&entry.snapshot)?)
    });
    let suspicion_flags = suspicion_flags(
        &status_counter_delta_since_previous_pop,
        &activity_delta_since_previous_pop,
        &activity_recency,
    );

    PopDiagnosticContext {
        schema_version: POP_DIAGNOSTIC_SCHEMA_VERSION,
        previous_mark_delta_ms,
        previous_position_delta_secs,
        status_counters,
        status_counter_delta_since_previous_pop,
        activity,
        activity_delta_since_previous_pop,
        activity_recency,
        suspicion_flags,
    }
}

fn build_pop_diagnostics_summary(entries: &[PopDiagnosticEntry]) -> PopDiagnosticsSummary {
    let mut suspicion_flag_counts = BTreeMap::new();
    for entry in entries {
        for flag in &entry.diagnostics.suspicion_flags {
            *suspicion_flag_counts.entry(flag.clone()).or_insert(0) += 1;
        }
    }

    let activity_delta_across_export = match (entries.first(), entries.last()) {
        (Some(first), Some(last)) if first.id != last.id => {
            activity_delta(&last.diagnostics.activity, &first.diagnostics.activity)
        }
        (Some(only), Some(_)) => activity_delta(
            &only.diagnostics.activity,
            &DiagnosticActivitySnapshot::default(),
        ),
        _ => DiagnosticActivityDeltaSnapshot::default(),
    };

    PopDiagnosticsSummary {
        entry_count: entries.len(),
        suspicion_flag_counts,
        activity_delta_across_export,
    }
}

fn suspicion_flags(
    status_delta: &BTreeMap<String, u64>,
    activity_delta: &DiagnosticActivityDeltaSnapshot,
    activity_recency: &DiagnosticActivityRecencySnapshot,
) -> Vec<String> {
    let mut flags = Vec::new();
    push_if_delta(
        &mut flags,
        status_delta,
        "underrun_events",
        "underrun_counter_advanced",
    );
    push_if_delta(
        &mut flags,
        status_delta,
        "dop_callback_deadline_miss_events",
        "dop_deadline_miss_counter_advanced",
    );
    push_if_delta(
        &mut flags,
        status_delta,
        "dop_soft_callback_gap_125_events",
        "dop_soft_callback_gap_counter_advanced",
    );
    push_if_delta(
        &mut flags,
        status_delta,
        "dop_soft_callback_gap_150_events",
        "dop_soft_callback_gap_counter_advanced",
    );
    push_if_delta(
        &mut flags,
        status_delta,
        "dop_soft_callback_gap_175_events",
        "dop_soft_callback_gap_counter_advanced",
    );
    push_if_delta(
        &mut flags,
        status_delta,
        "dop_ring_below_250ms_events",
        "dop_ring_pressure_counter_advanced",
    );
    push_if_delta(
        &mut flags,
        status_delta,
        "dop_ring_below_100ms_events",
        "dop_ring_pressure_counter_advanced",
    );
    push_if_delta(
        &mut flags,
        status_delta,
        "dop_ring_below_50ms_events",
        "dop_ring_pressure_counter_advanced",
    );
    push_if_delta(
        &mut flags,
        status_delta,
        "dop_ring_below_callback_events",
        "dop_ring_below_callback_counter_advanced",
    );
    push_if_delta(
        &mut flags,
        status_delta,
        "dop_marker_error_events",
        "dop_marker_error_counter_advanced",
    );
    push_if_delta(
        &mut flags,
        status_delta,
        "dop_repeated_payload_events",
        "dop_repeated_payload_counter_advanced",
    );
    push_if_delta(
        &mut flags,
        status_delta,
        "dop_program_to_idle_events",
        "dop_output_transition_counter_advanced",
    );
    push_if_delta(
        &mut flags,
        status_delta,
        "dop_idle_to_program_events",
        "dop_output_transition_counter_advanced",
    );
    push_if_delta(
        &mut flags,
        status_delta,
        "dop_mixed_output_events",
        "dop_output_transition_counter_advanced",
    );
    push_if_delta(
        &mut flags,
        status_delta,
        "sample_rate_change_count",
        "sample_rate_change_counter_advanced",
    );
    push_if_delta(
        &mut flags,
        status_delta,
        "dsp_graph_rebuild_count",
        "dsp_graph_rebuild_counter_advanced",
    );
    push_if_delta(
        &mut flags,
        status_delta,
        "coreaudio_dop_open_count",
        "coreaudio_dop_lifecycle_counter_advanced",
    );
    push_if_delta(
        &mut flags,
        status_delta,
        "coreaudio_dop_start_count",
        "coreaudio_dop_lifecycle_counter_advanced",
    );
    push_if_delta(
        &mut flags,
        status_delta,
        "coreaudio_dop_stop_count",
        "coreaudio_dop_lifecycle_counter_advanced",
    );
    push_if_delta(
        &mut flags,
        status_delta,
        "coreaudio_dop_drop_count",
        "coreaudio_dop_lifecycle_counter_advanced",
    );
    push_if_delta(
        &mut flags,
        status_delta,
        "coreaudio_dop_quiesce_count",
        "coreaudio_dop_lifecycle_counter_advanced",
    );
    push_if_delta(
        &mut flags,
        status_delta,
        "reopen_reason_count",
        "output_reopen_reason_counter_advanced",
    );
    push_if_delta(
        &mut flags,
        status_delta,
        "flush_reason_count",
        "flush_reason_counter_advanced",
    );
    push_if_delta(
        &mut flags,
        status_delta,
        "source_read_stall_count",
        "source_read_stall_counter_advanced",
    );
    push_if_delta(
        &mut flags,
        status_delta,
        "decoder_decode_stall_count",
        "decoder_decode_stall_counter_advanced",
    );
    push_if_delta(
        &mut flags,
        status_delta,
        "dsd_overbudget_blocks",
        "dsd_overbudget_counter_advanced",
    );
    push_if_delta(
        &mut flags,
        status_delta,
        "dsd_stability_resets",
        "dsd_stability_reset_counter_advanced",
    );
    push_if_delta(
        &mut flags,
        status_delta,
        "dsd_limiter_events",
        "dsd_limiter_counter_advanced",
    );
    push_if_delta(
        &mut flags,
        status_delta,
        "signal_clip_events",
        "signal_clip_counter_advanced",
    );

    if activity_delta.local_audio_device_scan.finished > 0 {
        flags.push("local_audio_device_scan_between_pops".to_string());
    }
    if activity_delta.local_audio_device_capability_probe.finished > 0 {
        flags.push("local_audio_device_capability_probe_between_pops".to_string());
    }
    if activity_delta.api_zones_refresh.finished > 0 {
        flags.push("api_zones_refresh_between_pops".to_string());
    }
    if activity_delta.api_devices_list.finished > 0 {
        flags.push("api_devices_list_between_pops".to_string());
    }
    if recency_is_near(activity_recency.local_audio_device_scan_ms_since_last_finish) {
        flags.push("local_audio_device_scan_near_pop".to_string());
    }
    if recency_is_near(activity_recency.local_audio_device_capability_probe_ms_since_last_finish) {
        flags.push("local_audio_device_capability_probe_near_pop".to_string());
    }
    if recency_is_near(activity_recency.api_zones_refresh_ms_since_last_finish) {
        flags.push("api_zones_refresh_near_pop".to_string());
    }
    if recency_is_near(activity_recency.api_devices_list_ms_since_last_finish) {
        flags.push("api_devices_list_near_pop".to_string());
    }

    flags.sort();
    flags.dedup();
    flags
}

fn push_if_delta(
    flags: &mut Vec<String>,
    status_delta: &BTreeMap<String, u64>,
    key: &str,
    flag: &str,
) {
    if status_delta.get(key).copied().unwrap_or(0) > 0 {
        flags.push(flag.to_string());
    }
}

fn extract_status_counters(snapshot: &Value) -> BTreeMap<String, u64> {
    STATUS_COUNTER_PATHS
        .iter()
        .map(|(name, path)| ((*name).to_string(), value_at_path(snapshot, path)))
        .collect()
}

fn value_at_path(snapshot: &Value, path: &[&str]) -> u64 {
    let mut value = snapshot;
    for part in path {
        let Some(next) = value.get(*part) else {
            return 0;
        };
        value = next;
    }
    value.as_u64().unwrap_or(0)
}

fn counter_map_delta(
    current: &BTreeMap<String, u64>,
    previous: &BTreeMap<String, u64>,
) -> BTreeMap<String, u64> {
    current
        .iter()
        .map(|(key, value)| {
            (
                key.clone(),
                value.saturating_sub(previous.get(key).copied().unwrap_or(0)),
            )
        })
        .collect()
}

fn activity_delta(
    current: &DiagnosticActivitySnapshot,
    previous: &DiagnosticActivitySnapshot,
) -> DiagnosticActivityDeltaSnapshot {
    DiagnosticActivityDeltaSnapshot {
        api_zones_refresh: activity_counter_delta(
            &current.api_zones_refresh,
            &previous.api_zones_refresh,
        ),
        api_devices_list: activity_counter_delta(
            &current.api_devices_list,
            &previous.api_devices_list,
        ),
        local_audio_device_scan: activity_counter_delta(
            &current.local_audio_device_scan,
            &previous.local_audio_device_scan,
        ),
        local_audio_device_capability_probe: activity_counter_delta(
            &current.local_audio_device_capability_probe,
            &previous.local_audio_device_capability_probe,
        ),
    }
}

fn activity_counter_delta(
    current: &DiagnosticActivityCountersSnapshot,
    previous: &DiagnosticActivityCountersSnapshot,
) -> DiagnosticActivityCountersDelta {
    DiagnosticActivityCountersDelta {
        started: current.started.saturating_sub(previous.started),
        finished: current.finished.saturating_sub(previous.finished),
    }
}

fn activity_recency(
    marked_at_ms: u64,
    activity: &DiagnosticActivitySnapshot,
) -> DiagnosticActivityRecencySnapshot {
    DiagnosticActivityRecencySnapshot {
        api_zones_refresh_ms_since_last_finish: ms_since_finish(
            marked_at_ms,
            activity.api_zones_refresh.last_finished_at_ms,
        ),
        api_devices_list_ms_since_last_finish: ms_since_finish(
            marked_at_ms,
            activity.api_devices_list.last_finished_at_ms,
        ),
        local_audio_device_scan_ms_since_last_finish: ms_since_finish(
            marked_at_ms,
            activity.local_audio_device_scan.last_finished_at_ms,
        ),
        local_audio_device_capability_probe_ms_since_last_finish: ms_since_finish(
            marked_at_ms,
            activity
                .local_audio_device_capability_probe
                .last_finished_at_ms,
        ),
    }
}

fn ms_since_finish(marked_at_ms: u64, last_finished_at_ms: u64) -> Option<u64> {
    (last_finished_at_ms > 0).then(|| marked_at_ms.saturating_sub(last_finished_at_ms))
}

fn recency_is_near(value: Option<u64>) -> bool {
    value.is_some_and(|ms| ms <= DEVICE_ACTIVITY_NEAR_POP_MS)
}

fn snapshot_position_secs(snapshot: &Value) -> Option<f64> {
    snapshot.get("position_secs")?.as_f64()
}

fn update_atomic_max(target: &AtomicU64, value: u64) {
    let mut current = target.load(Ordering::Relaxed);
    while value > current {
        match target.compare_exchange_weak(current, value, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break,
            Err(next) => current = next,
        }
    }
}

fn nanos_to_millis(value: u64) -> f64 {
    value as f64 / 1_000_000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn pop_snapshot_records_status_and_activity_deltas() {
        let diagnostics = DiagnosticsService::new();
        diagnostics.record_pop_snapshot(&json!({
            "position_secs": 1.0,
            "underrun_events": 0,
            "dop_callback_deadline_miss_events": 0,
            "sample_rate_change_count": 0
        }));
        {
            let _guard = diagnostics.begin_activity(DiagnosticActivity::LocalAudioDeviceScan);
        }
        let entry = diagnostics.record_pop_snapshot(&json!({
            "position_secs": 3.5,
            "underrun_events": 1,
            "dop_callback_deadline_miss_events": 0,
            "sample_rate_change_count": 0
        }));

        assert_eq!(
            entry
                .diagnostics
                .status_counter_delta_since_previous_pop
                .get("underrun_events"),
            Some(&1)
        );
        assert_eq!(entry.diagnostics.previous_position_delta_secs, Some(2.5));
        assert_eq!(
            entry
                .diagnostics
                .activity_delta_since_previous_pop
                .local_audio_device_scan
                .finished,
            1
        );
        assert!(
            entry
                .diagnostics
                .suspicion_flags
                .contains(&"underrun_counter_advanced".to_string())
        );
        assert!(
            entry
                .diagnostics
                .suspicion_flags
                .contains(&"local_audio_device_scan_between_pops".to_string())
        );
    }

    #[test]
    fn export_summary_counts_flags() {
        let diagnostics = DiagnosticsService::new();
        diagnostics.record_pop_snapshot(&json!({ "position_secs": 1.0, "underrun_events": 0 }));
        diagnostics.record_pop_snapshot(&json!({ "position_secs": 2.0, "underrun_events": 1 }));

        let export = diagnostics.export_pop_log();

        assert_eq!(export.summary.entry_count, 2);
        assert_eq!(
            export
                .summary
                .suspicion_flag_counts
                .get("underrun_counter_advanced"),
            Some(&1)
        );
    }
}
