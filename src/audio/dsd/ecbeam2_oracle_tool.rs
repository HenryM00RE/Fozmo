//! Frozen-corpus driver for the EcBeam2 exact N8/N12/N16 quality oracle.
//!
//! This module is invoked only by the `ecbeam2_exact_oracle` quality binary;
//! it is not part of renderer selection or playback policy.

use std::collections::{BTreeMap, BTreeSet};
use std::f64::consts::PI;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::delta_sigma::{
    EcBeam2ExactOracleReport, EcBeam2ExperimentConfig, prepare_ecbeam2_oracle_seed,
    run_ecbeam2_exact_oracle_from_seed,
};
use super::dsd_coeffs::CRFB_OSR64_OBG165;
use super::dsd_render::{DsdRate, DsdRenderer, dsd_source_window_to_modulator_samples};
use crate::audio::dsd::delta_sigma::DsdModulator;
use crate::audio::dsp::resampler::{FilterType, SincResampler};

const CORPUS_SCHEMA: &str = "ecbeam2-corpus-v1";
const ORACLE_SCHEMA: &str = "ecbeam2-exact-oracle-v2";
const BUDGET_SCHEMA: &str = "ecbeam2-frozen-budgets-v1";
const PROFILE: &str = "harness24to32-v1";
const SOURCE_CHUNK_FRAMES: usize = 1024;
const V1_PLANT_ID: &str = "ecbeam2-crfb-osr64-obg165-v1";
const V1_COEFFICIENT_TABLE: &str = "CRFB_OSR64_OBG165";
const V1_COEFFICIENT_ENCODING: &str =
    "a-row-major,b-row-major,c,d1,state-limit,input-peak,osr-u32,obg;little-endian";
const V1_COEFFICIENTS_SHA256: &str =
    "e5ddedd2c3885c0c92050c4f25243e803467d169d916565961e8687cfc83d554";
const V1_STATE_LIMIT_SHA256: &str =
    "247105152940185696a9745a57454825ff78c79ddb996e432c7d54933b2338e5";

#[derive(Debug, Clone, Deserialize)]
struct CorpusManifest {
    schema_version: String,
    corpus_id: String,
    source_rates: Vec<u32>,
    wire_rates: Vec<u32>,
    filters: Vec<String>,
    seeds: Vec<u64>,
    fixtures: Vec<FixtureSpec>,
    difficult_windows: Vec<WindowSpec>,
}

#[derive(Debug, Clone, Deserialize)]
struct FixtureSpec {
    id: String,
    kind: String,
    generator: Option<String>,
    generator_spec_sha256: Option<String>,
    path: Option<String>,
    start_sec: Option<f64>,
    end_sec: Option<f64>,
    sha256: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
struct WindowSpec {
    case_id: String,
    fixture_id: String,
    category: String,
    source_rate: u32,
    start_sample: usize,
    length_samples: usize,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct OracleRequest {
    schema_version: String,
    corpus_id: String,
    corpus_manifest_sha256: String,
    profile: String,
    profile_bindings: BTreeMap<String, String>,
    input_hash_encoding: String,
    plant: V1PlantBinding,
    beam: BeamBinding,
    exact_horizons: Vec<usize>,
    objective: String,
    feasibility: String,
    candidate_id: String,
    objective_configs: BTreeMap<String, OracleObjectiveConfig>,
    objective_scale_bindings: BTreeMap<String, ObjectiveScaleBinding>,
    start_mode: String,
    cases: Vec<OracleCase>,
    request_digest: String,
    request_sha256: String,
    required_result_fields: Vec<String>,
    #[serde(default)]
    constraint_budgets: Option<FrozenBudgetBinding>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct OracleObjectiveConfig {
    state_terminal_weight: f64,
    state_deadzone: f64,
    state_deadzone_weight: f64,
    quantizer_regularizer: f64,
    ultrasonic_budget: Option<f64>,
    signed_error_budget: Option<f64>,
}

impl OracleObjectiveConfig {
    fn engine_config(self) -> EcBeam2ExperimentConfig {
        EcBeam2ExperimentConfig {
            state_terminal_weight: self.state_terminal_weight,
            state_deadzone: self.state_deadzone,
            state_deadzone_weight: self.state_deadzone_weight,
            quantizer_regularizer: self.quantizer_regularizer,
            ultrasonic_budget: self.ultrasonic_budget,
            signed_error_budget: self.signed_error_budget,
            ..EcBeam2ExperimentConfig::default()
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ObjectiveScaleBinding {
    scale_probe_digest: String,
    wire_rate: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct OracleCase {
    case_id: String,
    source_case_id: String,
    fixture_id: String,
    category: String,
    filter: String,
    channel: OracleChannel,
    source_rate: u32,
    wire_rate: u32,
    generator_spec: String,
    generator_spec_sha256: String,
    seed: u64,
    start_sample: usize,
    length_samples: usize,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
enum OracleChannel {
    Left,
    Right,
}

type StereoModulatorInput = (Vec<f64>, Vec<f64>);
type OracleInputCache = BTreeMap<(String, String, u32), StereoModulatorInput>;

impl OracleChannel {
    const ALL: [Self; 2] = [Self::Left, Self::Right];

    fn as_str(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
        }
    }

    fn select(self, planes: &StereoModulatorInput) -> &[f64] {
        match self {
            Self::Left => &planes.0,
            Self::Right => &planes.1,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct BeamBinding {
    m: usize,
    n: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct V1PlantBinding {
    plant_id: String,
    coefficient_table: String,
    coefficient_encoding: String,
    coefficients_sha256: String,
    state_limit_sha256: String,
    osr: u32,
    obg: f64,
    input_peak: f64,
    headroom_db: f64,
    isi_penalty: f64,
    dither_scale: f64,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FrozenBudgetBinding {
    pub schema_version: String,
    pub document_sha256: String,
    pub calibration_digest: String,
    pub by_wire_rate: BTreeMap<String, FrozenWireBudget>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FrozenWireBudget {
    pub ultrasonic_ema_max: f64,
    pub signed_error_ema_abs_max: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExactOracleResults {
    schema_version: String,
    request_digest: String,
    request_sha256: String,
    request_file_sha256: String,
    corpus_id: String,
    corpus_manifest_sha256: String,
    profile: String,
    profile_bindings: BTreeMap<String, String>,
    input_hash_encoding: String,
    plant: V1PlantBinding,
    objective: String,
    candidate_id: String,
    objective_configs: BTreeMap<String, OracleObjectiveConfig>,
    objective_scale_bindings: BTreeMap<String, ObjectiveScaleBinding>,
    start_mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    constraint_budgets: Option<FrozenBudgetBinding>,
    results: Vec<ExactOracleRow>,
    comparisons: Vec<ExactOracleComparisonSummary>,
}

impl ExactOracleResults {
    pub fn result_count(&self) -> usize {
        self.results.len()
    }
}

#[derive(Debug, Clone, Serialize)]
struct ExactOracleRow {
    case_id: String,
    source_case_id: String,
    fixture_id: String,
    filter: String,
    channel: OracleChannel,
    source_rate: u32,
    wire_rate: u32,
    seed: u64,
    ultrasonic_budget: Option<f64>,
    signed_error_budget: Option<f64>,
    horizon: usize,
    first_bit: i8,
    m4n8_first_bit: i8,
    sequence_bits: Vec<i8>,
    objective: f64,
    reconstruction_objective: f64,
    starting_state_potential: f64,
    terminal_state_potential: f64,
    state_terminal_delta: f64,
    state_terminal_cost: f64,
    state_barrier_raw: f64,
    state_barrier_cost: f64,
    quantizer_error_energy: f64,
    quantizer_regularizer_cost: f64,
    total_objective: f64,
    starting_tail_energy: f64,
    causal_reconstruction_energy: f64,
    remaining_tail_energy: f64,
    tail_adjusted_energy: f64,
    causal_ultrasonic_energy: f64,
    maximum_state_overflow: f64,
    maximum_budget_violation: f64,
    constraint_escapes: u64,
    state_repairs: u64,
    complete_sequences: usize,
    state_feasible: bool,
    budgets_feasible: bool,
    reconstructed_outputs: Vec<f64>,
    /// Source-PCM-domain difficult-window boundary from the frozen manifest.
    source_window_start_sample: usize,
    /// Wire-rate post-limiter `u` samples preceding the exact window.
    prefix_sample_count: usize,
    prefix_constraint_escapes: u64,
    prefix_state_repairs: u64,
    prefix_all_nonfinite_resets: u64,
    prefix_invalid_input_substitutions: u64,
    prefix_output_length_events: u64,
    prefix_sha256: String,
    window_sha256: String,
}

#[derive(Debug, Clone, Serialize)]
struct ExactOracleComparisonSummary {
    case_id: String,
    source_case_id: String,
    filter: String,
    channel: OracleChannel,
    source_rate: u32,
    wire_rate: u32,
    prefix_constraint_escapes: u64,
    prefix_state_repairs: u64,
    prefix_all_nonfinite_resets: u64,
    prefix_invalid_input_substitutions: u64,
    prefix_output_length_events: u64,
    m4n8_first_bit: i8,
    exact_n8_first_bit: i8,
    m4n8_vs_exact_n8_disagrees: bool,
    exact_n12_first_bit: i8,
    exact_n16_first_bit: i8,
    n8_vs_n12_first_bit_disagrees: bool,
    n8_vs_n16_first_bit_disagrees: bool,
    n12_minus_n8_objective_per_sample: f64,
    n16_minus_n8_objective_per_sample: f64,
}

/// Execute a frozen exact-oracle request and write schema-compatible JSON.
pub fn run_frozen_exact_oracle(
    corpus_path: &Path,
    request_path: &Path,
    budget_path: Option<&Path>,
    output_path: &Path,
) -> Result<ExactOracleResults, String> {
    let corpus_bytes = fs::read(corpus_path)
        .map_err(|err| format!("failed to read corpus {}: {err}", corpus_path.display()))?;
    let request_bytes = fs::read(request_path)
        .map_err(|err| format!("failed to read request {}: {err}", request_path.display()))?;
    let corpus: CorpusManifest = serde_json::from_slice(&corpus_bytes)
        .map_err(|err| format!("failed to parse corpus {}: {err}", corpus_path.display()))?;
    let request_value: Value = serde_json::from_slice(&request_bytes)
        .map_err(|err| format!("failed to parse request {}: {err}", request_path.display()))?;
    let request: OracleRequest = serde_json::from_value(request_value.clone())
        .map_err(|err| format!("failed to decode oracle request: {err}"))?;
    let corpus_sha256 = sha256_hex(&corpus_bytes);
    let canonical_request_bytes = canonical_pretty_json(&request_value)?;
    if request_bytes != canonical_request_bytes {
        return Err("oracle request must use canonical sorted JSON formatting".to_string());
    }
    let request_file_sha256 = sha256_hex(&canonical_request_bytes);
    validate_request(&corpus, &request, &request_value, &corpus_sha256)?;
    validate_manifest_assets(
        &corpus,
        corpus_path.parent().unwrap_or_else(|| Path::new(".")),
    )?;

    let frozen_budgets =
        load_and_validate_budgets(budget_path, request.constraint_budgets.as_ref())?;
    let manifest_dir = corpus_path.parent().unwrap_or_else(|| Path::new("."));
    let fixture_by_id = corpus
        .fixtures
        .iter()
        .map(|fixture| (fixture.id.as_str(), fixture))
        .collect::<BTreeMap<_, _>>();
    let mut input_cache = OracleInputCache::new();
    let mut rows = Vec::new();

    for case in &request.cases {
        let key = (
            case.fixture_id.clone(),
            case.filter.clone(),
            case.source_rate,
        );
        if !input_cache.contains_key(&key) {
            let fixture = fixture_by_id
                .get(case.fixture_id.as_str())
                .ok_or_else(|| format!("unknown fixture {}", case.fixture_id))?;
            let frames = required_fixture_frames(&corpus, &case.fixture_id, case.source_rate);
            let (left, right) =
                materialize_fixture(fixture, manifest_dir, case.source_rate, frames)?;
            let filter = parse_primary_filter(&case.filter)?;
            let inputs = materialize_modulator_input(&left, &right, filter, case.source_rate)?;
            input_cache.insert(key.clone(), inputs);
        }
        let input = case.channel.select(&input_cache[&key]);
        let mapped = dsd_source_window_to_modulator_samples(
            parse_primary_filter(&case.filter)?,
            case.source_rate,
            case.wire_rate,
            case.start_sample,
            case.length_samples,
        )
        .ok_or_else(|| format!("failed to map source window for {}", case.case_id))?;
        let prefix_len = mapped.start;
        let prefix_numerator = case
            .start_sample
            .checked_mul(case.wire_rate as usize)
            .ok_or_else(|| format!("prefix length overflow for {}", case.case_id))?;
        let source_rate = case.source_rate as usize;
        if prefix_numerator % source_rate != 0 || prefix_len != prefix_numerator / source_rate {
            return Err(format!(
                "{} does not use exact zero-delay source/wire prefix alignment",
                case.case_id
            ));
        }
        let end = prefix_len
            .checked_add(16)
            .ok_or_else(|| format!("window length overflow for {}", case.case_id))?;
        if end > input.len() {
            return Err(format!(
                "{} maps to u samples {prefix_len}..{end}, but fixture produced {}",
                case.case_id,
                input.len()
            ));
        }
        let prefix = &input[..prefix_len];
        let prefix_sha256 = sha256_f64_le(prefix);
        let objective_config = *request
            .objective_configs
            .get(&case.wire_rate.to_string())
            .ok_or_else(|| format!("missing objective config for {}", case.wire_rate))?;
        let config = objective_config.engine_config();
        let oracle_seed = prepare_ecbeam2_oracle_seed(case.wire_rate, case.seed, prefix, config)
            .map_err(|err| format!("exact oracle prefix {}: {err}", case.case_id))?;
        for &horizon in &request.exact_horizons {
            let window = &input[prefix_len..prefix_len + horizon];
            let comparison = run_ecbeam2_exact_oracle_from_seed(&oracle_seed, window)
                .map_err(|err| format!("exact oracle {} N{horizon}: {err}", case.case_id))?;
            let exact = comparison.exact;
            validate_objective_accounting(&exact)
                .map_err(|err| format!("exact oracle {} N{horizon}: {err}", case.case_id))?;
            rows.push(ExactOracleRow {
                case_id: case.case_id.clone(),
                source_case_id: case.source_case_id.clone(),
                fixture_id: case.fixture_id.clone(),
                filter: case.filter.clone(),
                channel: case.channel,
                source_rate: case.source_rate,
                wire_rate: case.wire_rate,
                seed: case.seed,
                ultrasonic_budget: objective_config.ultrasonic_budget,
                signed_error_budget: objective_config.signed_error_budget,
                horizon,
                first_bit: bit_sign(exact.chosen_first_bit),
                m4n8_first_bit: bit_sign(comparison.m4n8_first_bit),
                sequence_bits: unpack_sequence(exact.chosen_sequence, horizon),
                objective: exact.sequence_objective,
                reconstruction_objective: exact.reconstruction_objective,
                starting_state_potential: exact.starting_state_potential,
                terminal_state_potential: exact.terminal_state_potential,
                state_terminal_delta: exact.state_terminal_delta,
                state_terminal_cost: exact.state_terminal_cost,
                state_barrier_raw: exact.state_barrier_raw,
                state_barrier_cost: exact.state_barrier_cost,
                quantizer_error_energy: exact.quantizer_error_energy,
                quantizer_regularizer_cost: exact.quantizer_regularizer_cost,
                total_objective: exact.total_objective,
                starting_tail_energy: exact.starting_tail_energy,
                causal_reconstruction_energy: exact.causal_reconstruction_energy,
                remaining_tail_energy: exact.remaining_tail_energy,
                tail_adjusted_energy: exact.tail_adjusted_energy,
                causal_ultrasonic_energy: exact.causal_ultrasonic_energy,
                maximum_state_overflow: exact.maximum_state_overflow,
                maximum_budget_violation: exact.maximum_budget_violation,
                constraint_escapes: exact.constraint_escapes,
                state_repairs: exact.state_repairs,
                complete_sequences: exact.complete_sequences,
                state_feasible: exact.state_feasible,
                budgets_feasible: exact.budgets_feasible,
                reconstructed_outputs: exact.reconstructed_output,
                source_window_start_sample: case.start_sample,
                prefix_sample_count: prefix_len,
                prefix_constraint_escapes: comparison.prefix_constraint_escapes,
                prefix_state_repairs: comparison.prefix_state_repairs,
                prefix_all_nonfinite_resets: comparison.prefix_all_nonfinite_resets,
                prefix_invalid_input_substitutions: comparison.prefix_invalid_input_substitutions,
                prefix_output_length_events: comparison.prefix_output_length_events,
                prefix_sha256: prefix_sha256.clone(),
                window_sha256: sha256_f64_le(window),
            });
        }
    }

    let comparisons = comparison_summaries(&request.cases, &rows)?;
    let results = ExactOracleResults {
        schema_version: ORACLE_SCHEMA.to_string(),
        request_digest: request.request_digest,
        request_sha256: request.request_sha256,
        request_file_sha256,
        corpus_id: request.corpus_id,
        corpus_manifest_sha256: corpus_sha256,
        profile: request.profile,
        profile_bindings: request.profile_bindings,
        input_hash_encoding: request.input_hash_encoding,
        plant: request.plant,
        objective: request.objective,
        candidate_id: request.candidate_id,
        objective_configs: request.objective_configs,
        objective_scale_bindings: request.objective_scale_bindings,
        start_mode: request.start_mode,
        constraint_budgets: frozen_budgets,
        results: rows,
        comparisons,
    };
    let bytes =
        canonical_pretty_json(&serde_json::to_value(&results).map_err(|err| err.to_string())?)?;
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| format!("failed to create {}: {err}", parent.display()))?;
    }
    fs::write(output_path, bytes)
        .map_err(|err| format!("failed to write {}: {err}", output_path.display()))?;
    Ok(results)
}

fn validate_request(
    corpus: &CorpusManifest,
    request: &OracleRequest,
    request_value: &Value,
    corpus_sha256: &str,
) -> Result<(), String> {
    if corpus.schema_version != CORPUS_SCHEMA || request.schema_version != ORACLE_SCHEMA {
        return Err("unexpected EcBeam2 corpus/oracle schema".to_string());
    }
    if request.corpus_id != corpus.corpus_id
        || request.corpus_manifest_sha256 != corpus_sha256
        || request.profile != PROFILE
        || request.objective != "tail_adjusted_energy_increment"
        || request.feasibility != "ecbeam2-v1"
        || request.candidate_id.trim().is_empty()
        || request.start_mode != "active-prefix"
        || request.input_hash_encoding != "f64-le"
        || request.plant != v1_plant_binding()
        || request.beam != (BeamBinding { m: 4, n: 8 })
        || request.exact_horizons != [8, 12, 16]
        || request.required_result_fields != required_result_fields()
    {
        return Err("oracle request is not bound to the frozen v1 experiment".to_string());
    }
    let expected_profile_bindings = corpus
        .wire_rates
        .iter()
        .map(|wire_rate| (wire_rate.to_string(), PROFILE.to_string()))
        .collect::<BTreeMap<_, _>>();
    if request.profile_bindings != expected_profile_bindings {
        return Err("oracle request has incomplete or extra profile bindings".to_string());
    }
    let expected_wires = corpus
        .wire_rates
        .iter()
        .map(u32::to_string)
        .collect::<BTreeSet<_>>();
    if request
        .objective_configs
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>()
        != expected_wires
        || request
            .objective_scale_bindings
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>()
            != expected_wires
    {
        return Err("oracle request has incomplete candidate objective bindings".to_string());
    }
    for wire_rate in &corpus.wire_rates {
        let key = wire_rate.to_string();
        let objective = request.objective_configs[&key];
        objective
            .engine_config()
            .validated()
            .map_err(|err| format!("invalid objective config for {wire_rate}: {err}"))?;
        let scale = &request.objective_scale_bindings[&key];
        if scale.wire_rate != *wire_rate || !is_sha256_hex(&scale.scale_probe_digest) {
            return Err(format!("invalid objective scale binding for {wire_rate}"));
        }
    }
    if let Some(binding) = &request.constraint_budgets {
        let actual_wires = binding
            .by_wire_rate
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>();
        if binding.schema_version != BUDGET_SCHEMA
            || binding.calibration_digest.is_empty()
            || actual_wires != expected_wires
        {
            return Err("oracle request has incomplete frozen constraint budgets".to_string());
        }
        for wire_rate in &corpus.wire_rates {
            let key = wire_rate.to_string();
            let objective = request.objective_configs[&key];
            let frozen = binding.by_wire_rate[&key];
            if objective.ultrasonic_budget != Some(frozen.ultrasonic_ema_max)
                || objective.signed_error_budget != Some(frozen.signed_error_ema_abs_max)
            {
                return Err(format!(
                    "candidate objective budgets do not match frozen binding for {wire_rate}"
                ));
            }
        }
    } else if request
        .objective_configs
        .values()
        .any(|config| config.ultrasonic_budget.is_some() || config.signed_error_budget.is_some())
    {
        return Err("candidate objective enables budgets without a frozen binding".to_string());
    }
    let mut digest_value = request_value.clone();
    let object = digest_value
        .as_object_mut()
        .ok_or_else(|| "oracle request must be an object".to_string())?;
    object.remove("request_digest");
    object.remove("request_sha256");
    let digest = sha256_hex(compact_json(&digest_value)?.as_bytes());
    if request.request_digest != digest || request.request_sha256 != digest {
        return Err("oracle request digest mismatch".to_string());
    }

    let windows = corpus
        .difficult_windows
        .iter()
        .map(|window| (window.case_id.as_str(), window))
        .collect::<BTreeMap<_, _>>();
    let fixtures = corpus
        .fixtures
        .iter()
        .map(|fixture| (fixture.id.as_str(), fixture))
        .collect::<BTreeMap<_, _>>();
    let mut actual = BTreeSet::new();
    for case in &request.cases {
        let window = windows
            .get(case.source_case_id.as_str())
            .ok_or_else(|| format!("unknown source case {}", case.source_case_id))?;
        if case.fixture_id != window.fixture_id
            || case.category != window.category
            || case.source_rate != window.source_rate
            || case.start_sample != window.start_sample
            || case.length_samples != window.length_samples
        {
            return Err(format!(
                "expanded case {} changed its source window",
                case.case_id
            ));
        }
        if case.case_id
            != format!(
                "{}--{}--{}",
                case.source_case_id,
                case.filter,
                case.channel.as_str()
            )
            || !corpus.filters.contains(&case.filter)
            || !corpus.seeds.contains(&case.seed)
            || DsdRate::Dsd64.wire_rate_for_source(case.source_rate) != Some(case.wire_rate)
        {
            return Err(format!(
                "expanded case {} has invalid frozen axes",
                case.case_id
            ));
        }
        let fixture = fixtures.get(case.fixture_id.as_str()).ok_or_else(|| {
            format!(
                "expanded case {} references an unknown fixture",
                case.case_id
            )
        })?;
        let fixture_seed = fixture_generator_seed(fixture)?.ok_or_else(|| {
            format!(
                "exact-oracle case {} must reference a generated seeded fixture",
                case.case_id
            )
        })?;
        let fixture_spec = fixture.generator.as_deref().unwrap_or_default();
        let fixture_spec_sha256 = fixture.generator_spec_sha256.as_deref().unwrap_or_default();
        if case.seed != fixture_seed
            || case.generator_spec != fixture_spec
            || case.generator_spec_sha256 != fixture_spec_sha256
            || case.generator_spec_sha256 != sha256_hex(case.generator_spec.as_bytes())
        {
            return Err(format!(
                "expanded case {} generator/seed binding for seed {} does not match fixture seed {}",
                case.case_id, case.seed, fixture_seed
            ));
        }
        if !actual.insert((
            case.source_case_id.clone(),
            case.filter.clone(),
            case.channel,
        )) {
            return Err(format!("duplicate expanded case {}", case.case_id));
        }
    }
    let mut expected = BTreeSet::new();
    for window in &corpus.difficult_windows {
        for filter in &corpus.filters {
            for channel in OracleChannel::ALL {
                expected.insert((window.case_id.clone(), filter.clone(), channel));
            }
        }
    }
    if actual != expected {
        return Err(
            "oracle request does not cover every difficult-window/filter/channel cell".to_string(),
        );
    }
    Ok(())
}

fn v1_plant_binding() -> V1PlantBinding {
    let coefficients_sha256 = coefficient_table_sha256();
    let state_limit_sha256 = sha256_f64_le(&CRFB_OSR64_OBG165.state_limit);
    debug_assert_eq!(coefficients_sha256, V1_COEFFICIENTS_SHA256);
    debug_assert_eq!(state_limit_sha256, V1_STATE_LIMIT_SHA256);
    V1PlantBinding {
        plant_id: V1_PLANT_ID.to_string(),
        coefficient_table: V1_COEFFICIENT_TABLE.to_string(),
        coefficient_encoding: V1_COEFFICIENT_ENCODING.to_string(),
        coefficients_sha256,
        state_limit_sha256,
        osr: CRFB_OSR64_OBG165.osr,
        obg: CRFB_OSR64_OBG165.obg,
        input_peak: CRFB_OSR64_OBG165.input_peak,
        headroom_db: -2.0,
        isi_penalty: 0.0,
        dither_scale: 0.0,
    }
}

fn coefficient_table_sha256() -> String {
    let mut digest = Sha256::new();
    for row in &CRFB_OSR64_OBG165.a {
        for value in row {
            digest.update(value.to_le_bytes());
        }
    }
    for row in &CRFB_OSR64_OBG165.b {
        for value in row {
            digest.update(value.to_le_bytes());
        }
    }
    for value in &CRFB_OSR64_OBG165.c {
        digest.update(value.to_le_bytes());
    }
    digest.update(CRFB_OSR64_OBG165.d1.to_le_bytes());
    for value in &CRFB_OSR64_OBG165.state_limit {
        digest.update(value.to_le_bytes());
    }
    digest.update(CRFB_OSR64_OBG165.input_peak.to_le_bytes());
    digest.update(CRFB_OSR64_OBG165.osr.to_le_bytes());
    digest.update(CRFB_OSR64_OBG165.obg.to_le_bytes());
    format!("{:x}", digest.finalize())
}

fn required_result_fields() -> Vec<String> {
    [
        "case_id",
        "source_case_id",
        "fixture_id",
        "filter",
        "channel",
        "source_rate",
        "wire_rate",
        "seed",
        "ultrasonic_budget",
        "signed_error_budget",
        "horizon",
        "first_bit",
        "m4n8_first_bit",
        "sequence_bits",
        "objective",
        "reconstruction_objective",
        "starting_state_potential",
        "terminal_state_potential",
        "state_terminal_delta",
        "state_terminal_cost",
        "state_barrier_raw",
        "state_barrier_cost",
        "quantizer_error_energy",
        "quantizer_regularizer_cost",
        "total_objective",
        "starting_tail_energy",
        "causal_reconstruction_energy",
        "remaining_tail_energy",
        "tail_adjusted_energy",
        "causal_ultrasonic_energy",
        "maximum_state_overflow",
        "maximum_budget_violation",
        "constraint_escapes",
        "state_repairs",
        "complete_sequences",
        "state_feasible",
        "budgets_feasible",
        "reconstructed_outputs",
        "source_window_start_sample",
        "prefix_sample_count",
        "prefix_constraint_escapes",
        "prefix_state_repairs",
        "prefix_all_nonfinite_resets",
        "prefix_invalid_input_substitutions",
        "prefix_output_length_events",
        "prefix_sha256",
        "window_sha256",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

fn validate_manifest_assets(corpus: &CorpusManifest, manifest_dir: &Path) -> Result<(), String> {
    let source_rates = corpus.source_rates.iter().copied().collect::<BTreeSet<_>>();
    let wire_rates = corpus.wire_rates.iter().copied().collect::<BTreeSet<_>>();
    let filters = corpus.filters.iter().cloned().collect::<BTreeSet<_>>();
    if source_rates != BTreeSet::from([44_100, 48_000])
        || source_rates.len() != corpus.source_rates.len()
        || wire_rates != BTreeSet::from([2_822_400, 3_072_000])
        || wire_rates.len() != corpus.wire_rates.len()
        || filters != BTreeSet::from(["MinimumPhase".to_string(), "SplitPhase".to_string()])
        || filters.len() != corpus.filters.len()
        || corpus.source_rates.len() != corpus.wire_rates.len()
        || corpus.seeds.is_empty()
    {
        return Err(
            "corpus axes must uniquely freeze both DSD64 families and primary filters".to_string(),
        );
    }
    for (source_rate, wire_rate) in corpus.source_rates.iter().zip(&corpus.wire_rates) {
        if DsdRate::Dsd64.wire_rate_for_source(*source_rate) != Some(*wire_rate) {
            return Err("corpus source/wire axes are not paired in declared order".to_string());
        }
    }
    let mut generated_seeds = BTreeSet::new();
    let mut fixture_ids = BTreeSet::new();
    for fixture in &corpus.fixtures {
        if fixture.id.trim().is_empty() || !fixture_ids.insert(fixture.id.clone()) {
            return Err(format!("invalid or duplicate fixture id {}", fixture.id));
        }
        match fixture.kind.as_str() {
            "generated" => {
                let spec = fixture
                    .generator
                    .as_deref()
                    .ok_or_else(|| format!("generated fixture {} lacks a spec", fixture.id))?;
                if fixture.generator_spec_sha256.as_deref()
                    != Some(sha256_hex(spec.as_bytes()).as_str())
                {
                    return Err(format!("generated fixture {} hash mismatch", fixture.id));
                }
                validate_generator_spec(spec)?;
                let seed = generator_seed(spec)?;
                if !generated_seeds.insert(seed) {
                    return Err(format!(
                        "generated fixture {} reuses generator seed {seed}",
                        fixture.id
                    ));
                }
            }
            "committed-wav-window" => {
                let path = manifest_dir.join(fixture.path.as_deref().unwrap_or_default());
                let bytes = fs::read(&path)
                    .map_err(|err| format!("failed to read {}: {err}", path.display()))?;
                if fixture.sha256.as_deref() != Some(sha256_hex(&bytes).as_str()) {
                    return Err(format!("committed fixture {} hash mismatch", fixture.id));
                }
                let start = fixture.start_sec.unwrap_or(f64::NAN);
                let end = fixture.end_sec.unwrap_or(f64::NAN);
                if !start.is_finite() || !end.is_finite() || start < 0.0 || end <= start {
                    return Err(format!(
                        "committed fixture {} has invalid bounds",
                        fixture.id
                    ));
                }
            }
            other => return Err(format!("unsupported fixture kind {other}")),
        }
    }
    let declared_seeds = corpus.seeds.iter().copied().collect::<BTreeSet<_>>();
    if declared_seeds.len() != corpus.seeds.len() || declared_seeds != generated_seeds {
        return Err(format!(
            "corpus seeds {:?} do not exactly match generated fixture seeds {:?}",
            corpus.seeds, generated_seeds
        ));
    }
    let mut case_ids = BTreeSet::new();
    for window in &corpus.difficult_windows {
        if window.case_id.trim().is_empty()
            || !case_ids.insert(window.case_id.clone())
            || !fixture_ids.contains(&window.fixture_id)
            || !source_rates.contains(&window.source_rate)
            || window.length_samples == 0
            || window
                .start_sample
                .checked_add(window.length_samples)
                .is_none()
        {
            return Err(format!("invalid difficult window {}", window.case_id));
        }
    }
    Ok(())
}

fn load_and_validate_budgets(
    path: Option<&Path>,
    request_binding: Option<&FrozenBudgetBinding>,
) -> Result<Option<FrozenBudgetBinding>, String> {
    match (path, request_binding) {
        (None, None) => Ok(None),
        (Some(_), None) => {
            Err("budget document supplied but request is not budget-bound".to_string())
        }
        (None, Some(_)) => Err("budget-bound oracle request requires --budgets".to_string()),
        (Some(path), Some(expected)) => {
            let bytes = fs::read(path)
                .map_err(|err| format!("failed to read budgets {}: {err}", path.display()))?;
            if sha256_hex(&bytes) != expected.document_sha256 {
                return Err("frozen budget document SHA-256 mismatch".to_string());
            }
            let document: Value = serde_json::from_slice(&bytes)
                .map_err(|err| format!("failed to parse frozen budget document: {err}"))?;
            if document.get("schema_version").and_then(Value::as_str)
                != Some(expected.schema_version.as_str())
                || document.get("calibration_digest").and_then(Value::as_str)
                    != Some(expected.calibration_digest.as_str())
            {
                return Err("frozen budget document provenance mismatch".to_string());
            }
            for (wire, budget) in &expected.by_wire_rate {
                if wire.parse::<u32>().is_err()
                    || !budget.ultrasonic_ema_max.is_finite()
                    || budget.ultrasonic_ema_max <= 0.0
                    || !budget.signed_error_ema_abs_max.is_finite()
                    || budget.signed_error_ema_abs_max <= 0.0
                {
                    return Err(format!("invalid frozen budgets for {wire}"));
                }
                let document_budget = document
                    .get("by_wire_rate")
                    .and_then(|value| value.get(wire))
                    .ok_or_else(|| format!("frozen budget document lacks wire rate {wire}"))?;
                if document_budget
                    .get("ultrasonic_ema_max")
                    .and_then(Value::as_f64)
                    != Some(budget.ultrasonic_ema_max)
                    || document_budget
                        .get("signed_error_ema_abs_max")
                        .and_then(Value::as_f64)
                        != Some(budget.signed_error_ema_abs_max)
                {
                    return Err(format!("frozen budget limits changed for wire rate {wire}"));
                }
            }
            Ok(Some(expected.clone()))
        }
    }
}

fn parse_primary_filter(name: &str) -> Result<FilterType, String> {
    match name {
        "MinimumPhase" => Ok(FilterType::Minimum16k),
        "SplitPhase" => Ok(FilterType::Split128k),
        _ => Err(format!("unsupported primary EcBeam2 filter {name}")),
    }
}

fn materialize_modulator_input(
    left: &[f64],
    right: &[f64],
    filter: FilterType,
    source_rate: u32,
) -> Result<(Vec<f64>, Vec<f64>), String> {
    let mut renderer = DsdRenderer::new_with_dsd_modulator(
        filter,
        source_rate,
        DsdRate::Dsd64,
        DsdModulator::EcBeam2,
    )
    .map_err(str::to_string)?;
    let frames = left.len().min(right.len());
    let mut output_left = Vec::new();
    let mut output_right = Vec::new();
    for start in (0..frames).step_by(SOURCE_CHUNK_FRAMES) {
        let end = (start + SOURCE_CHUNK_FRAMES).min(frames);
        renderer.upsample(&left[start..end], &right[start..end]);
        let (block_left, block_right) = renderer
            .ecbeam2_oracle_modulator_input_block(1.0)
            .map_err(str::to_string)?;
        output_left.extend(block_left);
        output_right.extend(block_right);
    }
    renderer.drain_resampler_eof();
    let (block_left, block_right) = renderer
        .ecbeam2_oracle_modulator_input_block(1.0)
        .map_err(str::to_string)?;
    output_left.extend(block_left);
    output_right.extend(block_right);
    if output_left.len() != output_right.len() {
        return Err("EcBeam2 oracle produced unequal stereo input lengths".to_string());
    }
    let wire_rate = DsdRate::Dsd64
        .wire_rate_for_source(source_rate)
        .ok_or_else(|| format!("unsupported EcBeam2 oracle source rate {source_rate}"))?;
    if wire_rate % source_rate != 0 {
        return Err("EcBeam2 oracle source-to-wire ratio is not integral".to_string());
    }
    let expected = frames
        .checked_mul((wire_rate / source_rate) as usize)
        .ok_or_else(|| "EcBeam2 oracle nominal input length overflow".to_string())?;
    if output_left.len() != expected {
        return Err(format!(
            "EcBeam2 oracle produced {} samples per channel, expected {expected}",
            output_left.len()
        ));
    }
    Ok((output_left, output_right))
}

fn required_fixture_frames(corpus: &CorpusManifest, fixture_id: &str, source_rate: u32) -> usize {
    let minimum = (source_rate as f64 * 0.50).round() as usize;
    corpus
        .difficult_windows
        .iter()
        .filter(|window| window.fixture_id == fixture_id)
        .filter_map(|window| {
            let end = window.start_sample.checked_add(window.length_samples)?;
            let seconds = end as f64 / window.source_rate as f64;
            Some((seconds * source_rate as f64).ceil() as usize)
        })
        .max()
        .unwrap_or(minimum)
        .max(minimum)
}

fn materialize_fixture(
    fixture: &FixtureSpec,
    manifest_dir: &Path,
    source_rate: u32,
    frames: usize,
) -> Result<(Vec<f64>, Vec<f64>), String> {
    match fixture.kind.as_str() {
        "generated" => generated_fixture(
            fixture.generator.as_deref().unwrap_or_default(),
            frames,
            source_rate,
        ),
        "committed-wav-window" => {
            let path = manifest_dir.join(fixture.path.as_deref().unwrap_or_default());
            let (left, right) = load_pcm16_wav_excerpt(
                &path,
                fixture.start_sec.unwrap_or_default(),
                fixture.end_sec.unwrap_or_default(),
            )?;
            if source_rate == 44_100 {
                Ok((left, right))
            } else {
                Ok((
                    resample_mono(&left, 44_100, source_rate),
                    resample_mono(&right, 44_100, source_rate),
                ))
            }
        }
        other => Err(format!("unsupported fixture kind {other}")),
    }
}

fn validate_generator_spec(spec: &str) -> Result<(), String> {
    let parts = spec.split('|').collect::<Vec<_>>();
    let seed = |part: &str| {
        part.strip_prefix("seed=")
            .is_some_and(|value| !value.is_empty())
    };
    let valid = match parts.as_slice() {
        [
            "program_multitone" | "pink_noise" | "fades_overload" | "spur_windows",
            seed_part,
            "v1",
        ] => seed(seed_part),
        ["low_level_tones", "-120,-100,-80", seed_part, "v1"] => seed(seed_part),
        ["tiny_dc", "levels=1e-6,1e-5", seed_part, "v1"] => seed(seed_part),
        ["high_frequency", "18000,19000", seed_part, "v1"] => seed(seed_part),
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(format!("unsupported EcBeam2 generator spec {spec}"))
    }
}

fn generator_seed(spec: &str) -> Result<u64, String> {
    let value = spec
        .split('|')
        .find_map(|part| part.strip_prefix("seed="))
        .ok_or_else(|| format!("generator lacks seed: {spec}"))?;
    value
        .strip_prefix("0x")
        .map(|hex| u64::from_str_radix(hex, 16))
        .unwrap_or_else(|| value.parse::<u64>())
        .map_err(|_| format!("invalid generator seed {value}"))
}

fn fixture_generator_seed(fixture: &FixtureSpec) -> Result<Option<u64>, String> {
    fixture.generator.as_deref().map(generator_seed).transpose()
}

fn generated_fixture(
    spec: &str,
    frames: usize,
    sample_rate: u32,
) -> Result<(Vec<f64>, Vec<f64>), String> {
    validate_generator_spec(spec)?;
    let seed = generator_seed(spec)?;
    let kind = spec.split('|').next().unwrap_or_default();
    let phase = (seed & 0xffff) as f64 / 65_536.0 * 2.0 * PI;
    let stereo_phase = ((seed >> 16) & 0xffff) as f64 / 65_536.0 * 2.0 * PI;
    let sample = |index: usize, channel_phase: f64| {
        let t = index as f64 / sample_rate as f64;
        match kind {
            "program_multitone" => {
                let tones = [137.0, 499.0, 997.0, 2_711.0, 7_321.0, 13_733.0, 17_101.0];
                tones
                    .iter()
                    .enumerate()
                    .map(|(voice, freq)| {
                        let weight = 1.0 / (voice + 2) as f64;
                        weight
                            * (2.0 * PI * freq * t + phase + channel_phase + voice as f64 * 0.37)
                                .sin()
                    })
                    .sum::<f64>()
                    * 0.42
                    / 1.718
            }
            "pink_noise" => 0.0,
            "low_level_tones" => {
                let levels = [-120.0, -100.0, -80.0];
                let segment = (frames / levels.len()).max(1);
                let band = (index / segment).min(levels.len() - 1);
                let amp = 10.0f64.powf(levels[band] / 20.0);
                let freq = [997.0, 3_997.0, 12_001.0][band];
                amp * (2.0 * PI * freq * t + phase + channel_phase).sin()
            }
            "tiny_dc" => {
                let level = if index < frames / 2 { 1.0e-6 } else { 1.0e-5 };
                let sign = if channel_phase == 0.0 { 1.0 } else { -1.0 };
                sign * level
            }
            "high_frequency" => {
                0.18 * (2.0 * PI * 18_000.0 * t + phase).sin()
                    + 0.18 * (2.0 * PI * 19_000.0 * t + channel_phase).sin()
            }
            "fades_overload" => {
                let p = index as f64 / frames.max(1) as f64;
                let fade = if p < 0.25 {
                    smoothstep(p / 0.25)
                } else if p < 0.50 {
                    1.0 - smoothstep((p - 0.25) / 0.25)
                } else {
                    0.35
                };
                let program = 0.62 * (2.0 * PI * 997.0 * t + phase).sin()
                    + 0.25 * (2.0 * PI * 17_101.0 * t + channel_phase).sin();
                let overload = if (0.70..0.705).contains(&p) {
                    if index.is_multiple_of(2) { 0.98 } else { -0.98 }
                } else {
                    0.0
                };
                (fade * program + overload).clamp(-0.98, 0.98)
            }
            "spur_windows" => {
                let p = index as f64 / frames.max(1) as f64;
                let dc = if p < 0.33 { 1.0e-5 } else { -1.0e-5 };
                dc + 0.002 * (2.0 * PI * 997.0 * t + phase).sin()
                    + 0.004 * (2.0 * PI * 18_997.0 * t + channel_phase).sin()
            }
            _ => 0.0,
        }
    };
    if kind == "pink_noise" {
        return Ok((
            pink_noise(frames, seed, 0.28),
            pink_noise(frames, seed ^ 0x9e37_79b9_7f4a_7c15, 0.28),
        ));
    }
    Ok((
        (0..frames).map(|index| sample(index, 0.0)).collect(),
        (0..frames)
            .map(|index| sample(index, stereo_phase.max(1.0e-12)))
            .collect(),
    ))
}

fn pink_noise(frames: usize, seed: u64, amplitude: f64) -> Vec<f64> {
    let mut b0 = 0.0;
    let mut b1 = 0.0;
    let mut b2 = 0.0;
    let mut max_abs = 0.0f64;
    let mut output = Vec::with_capacity(frames);
    for index in 0..frames {
        let white = deterministic_noise(index, seed);
        b0 = 0.99765 * b0 + white * 0.0990460;
        b1 = 0.96300 * b1 + white * 0.2965164;
        b2 = 0.57000 * b2 + white * 1.0526913;
        let sample = b0 + b1 + b2 + white * 0.1848;
        max_abs = max_abs.max(sample.abs());
        output.push(sample);
    }
    let scale = amplitude / max_abs.max(1.0e-18);
    output.iter_mut().for_each(|sample| *sample *= scale);
    output
}

fn deterministic_noise(index: usize, seed: u64) -> f64 {
    let mut value = index as u64 ^ seed;
    value ^= value >> 12;
    value ^= value << 25;
    value ^= value >> 27;
    let mantissa = (value.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1_u64 << 53) as f64;
    2.0 * mantissa - 1.0
}

fn smoothstep(value: f64) -> f64 {
    let value = value.clamp(0.0, 1.0);
    value * value * (3.0 - 2.0 * value)
}

fn resample_mono(input: &[f64], source_rate: u32, target_rate: u32) -> Vec<f64> {
    let mut resampler = SincResampler::new(FilterType::SincExtreme32k, source_rate, target_rate);
    resampler.input(input, input);
    let mut interleaved = Vec::new();
    resampler.process(&mut interleaved);
    resampler.drain_eof(&mut interleaved);
    interleaved.chunks_exact(2).map(|frame| frame[0]).collect()
}

fn load_pcm16_wav_excerpt(
    path: &Path,
    start_sec: f64,
    end_sec: f64,
) -> Result<(Vec<f64>, Vec<f64>), String> {
    let data = fs::read(path).map_err(|err| format!("failed to read WAV: {err}"))?;
    if data.len() < 44 || &data[..4] != b"RIFF" || &data[8..12] != b"WAVE" {
        return Err(format!("{} is not RIFF/WAVE", path.display()));
    }
    let mut offset = 12;
    let mut format = None;
    let mut pcm = None;
    while offset + 8 <= data.len() {
        let id = &data[offset..offset + 4];
        let len = u32::from_le_bytes(data[offset + 4..offset + 8].try_into().unwrap()) as usize;
        offset += 8;
        if offset + len > data.len() {
            return Err("truncated WAV chunk".to_string());
        }
        if id == b"fmt " && len >= 16 {
            format = Some((
                u16::from_le_bytes(data[offset..offset + 2].try_into().unwrap()),
                u16::from_le_bytes(data[offset + 2..offset + 4].try_into().unwrap()),
                u32::from_le_bytes(data[offset + 4..offset + 8].try_into().unwrap()),
                u16::from_le_bytes(data[offset + 14..offset + 16].try_into().unwrap()),
            ));
        } else if id == b"data" {
            pcm = Some((offset, len));
        }
        offset += len + len % 2;
    }
    if format != Some((1, 2, 44_100, 16)) {
        return Err("WAV must be PCM16 stereo 44.1 kHz".to_string());
    }
    let (pcm_offset, pcm_len) = pcm.ok_or_else(|| "WAV lacks data chunk".to_string())?;
    let start = (start_sec * 44_100.0).round() as usize * 4;
    let end = (end_sec * 44_100.0).round() as usize * 4;
    if start >= end || end > pcm_len {
        return Err("WAV excerpt lies outside data chunk".to_string());
    }
    let mut left = Vec::with_capacity((end - start) / 4);
    let mut right = Vec::with_capacity((end - start) / 4);
    for frame in data[pcm_offset + start..pcm_offset + end].chunks_exact(4) {
        left.push(i16::from_le_bytes([frame[0], frame[1]]) as f64 / 32768.0);
        right.push(i16::from_le_bytes([frame[2], frame[3]]) as f64 / 32768.0);
    }
    Ok((left, right))
}

fn comparison_summaries(
    cases: &[OracleCase],
    rows: &[ExactOracleRow],
) -> Result<Vec<ExactOracleComparisonSummary>, String> {
    let indexed = rows
        .iter()
        .map(|row| ((row.case_id.as_str(), row.horizon), row))
        .collect::<BTreeMap<_, _>>();
    cases
        .iter()
        .map(|case| {
            let n8 = indexed
                .get(&(case.case_id.as_str(), 8))
                .ok_or_else(|| format!("missing N8 result for {}", case.case_id))?;
            let n12 = indexed
                .get(&(case.case_id.as_str(), 12))
                .ok_or_else(|| format!("missing N12 result for {}", case.case_id))?;
            let n16 = indexed
                .get(&(case.case_id.as_str(), 16))
                .ok_or_else(|| format!("missing N16 result for {}", case.case_id))?;
            let prefix_health = |row: &ExactOracleRow| {
                (
                    row.prefix_constraint_escapes,
                    row.prefix_state_repairs,
                    row.prefix_all_nonfinite_resets,
                    row.prefix_invalid_input_substitutions,
                    row.prefix_output_length_events,
                )
            };
            let n8_prefix_health = prefix_health(n8);
            if prefix_health(n12) != n8_prefix_health || prefix_health(n16) != n8_prefix_health {
                return Err(format!(
                    "exact-oracle prefix health changed by horizon for {}",
                    case.case_id
                ));
            }
            if n8_prefix_health != (0, 0, 0, 0, 0) {
                return Err(format!(
                    "exact-oracle prefix is ineligible for {}: constraint_escapes={}, \
                     state_repairs={}, all_nonfinite_resets={}, invalid_input_substitutions={}, \
                     output_length_events={}",
                    case.case_id,
                    n8_prefix_health.0,
                    n8_prefix_health.1,
                    n8_prefix_health.2,
                    n8_prefix_health.3,
                    n8_prefix_health.4,
                ));
            }
            Ok(ExactOracleComparisonSummary {
                case_id: case.case_id.clone(),
                source_case_id: case.source_case_id.clone(),
                filter: case.filter.clone(),
                channel: case.channel,
                source_rate: case.source_rate,
                wire_rate: case.wire_rate,
                prefix_constraint_escapes: n8.prefix_constraint_escapes,
                prefix_state_repairs: n8.prefix_state_repairs,
                prefix_all_nonfinite_resets: n8.prefix_all_nonfinite_resets,
                prefix_invalid_input_substitutions: n8.prefix_invalid_input_substitutions,
                prefix_output_length_events: n8.prefix_output_length_events,
                m4n8_first_bit: n8.m4n8_first_bit,
                exact_n8_first_bit: n8.first_bit,
                m4n8_vs_exact_n8_disagrees: n8.m4n8_first_bit != n8.first_bit,
                exact_n12_first_bit: n12.first_bit,
                exact_n16_first_bit: n16.first_bit,
                n8_vs_n12_first_bit_disagrees: n8.first_bit != n12.first_bit,
                n8_vs_n16_first_bit_disagrees: n8.first_bit != n16.first_bit,
                n12_minus_n8_objective_per_sample: n12.objective / 12.0 - n8.objective / 8.0,
                n16_minus_n8_objective_per_sample: n16.objective / 16.0 - n8.objective / 8.0,
            })
        })
        .collect()
}

fn validate_objective_accounting(report: &EcBeam2ExactOracleReport) -> Result<(), String> {
    let component_sum = report.reconstruction_objective
        + report.state_terminal_cost
        + report.state_barrier_cost
        + report.quantizer_regularizer_cost;
    let state_delta = report.terminal_state_potential - report.starting_state_potential;
    let close = |left: f64, right: f64| {
        let scale = left.abs().max(right.abs()).max(1.0);
        (left - right).abs() <= 256.0 * f64::EPSILON * scale
    };
    if !close(report.total_objective, component_sum)
        || !close(report.sequence_objective, report.total_objective)
    {
        return Err("objective components do not sum to the total path metric".to_string());
    }
    if !close(report.state_terminal_delta, state_delta) {
        return Err("state-terminal increments do not telescope".to_string());
    }
    Ok(())
}

fn bit_sign(bit: u8) -> i8 {
    if bit == 0 { -1 } else { 1 }
}

fn unpack_sequence(history: u16, horizon: usize) -> Vec<i8> {
    (0..horizon)
        .map(|index| bit_sign(((history >> (horizon - index - 1)) & 1) as u8))
        .collect()
}

fn sha256_f64_le(samples: &[f64]) -> String {
    let mut digest = Sha256::new();
    for sample in samples {
        digest.update(sample.to_le_bytes());
    }
    format!("{:x}", digest.finalize())
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn compact_json(value: &Value) -> Result<String, String> {
    serde_json::to_string(value).map_err(|err| err.to_string())
}

fn canonical_pretty_json(value: &Value) -> Result<Vec<u8>, String> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|err| err.to_string())?;
    bytes.push(b'\n');
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packed_sequence_uses_first_decision_as_most_significant_bit() {
        assert_eq!(unpack_sequence(0b1010, 4), vec![1, -1, 1, -1]);
    }

    #[test]
    fn generated_fixture_is_deterministic_and_finite() {
        let first = generated_fixture("pink_noise|seed=0xc002|v1", 1024, 48_000).unwrap();
        let second = generated_fixture("pink_noise|seed=0xc002|v1", 1024, 48_000).unwrap();
        assert_eq!(first, second);
        assert!(
            first
                .0
                .iter()
                .chain(&first.1)
                .all(|sample| sample.is_finite())
        );
        assert!(first.0.iter().any(|sample| *sample != 0.0));
    }

    #[test]
    fn oracle_materialization_preserves_distinct_stereo_planes() {
        let (left, right) =
            generated_fixture("program_multitone|seed=0x12345678|v1", 1024, 44_100).unwrap();
        let (normalized_left, normalized_right) =
            materialize_modulator_input(&left, &right, FilterType::Minimum16k, 44_100).unwrap();
        assert!(!normalized_left.is_empty());
        assert_eq!(normalized_left.len(), normalized_right.len());
        assert_ne!(
            sha256_f64_le(&normalized_left),
            sha256_f64_le(&normalized_right)
        );
    }

    #[test]
    fn generator_specs_are_exactly_versioned() {
        for spec in [
            "program_multitone|seed=0xc001|v1",
            "low_level_tones|-120,-100,-80|seed=0x5101|v1",
            "tiny_dc|levels=1e-6,1e-5|seed=0x5102|v1",
            "high_frequency|18000,19000|seed=0x5103|v1",
        ] {
            validate_generator_spec(spec).unwrap();
        }
        for spec in [
            "program_multitone|extra|seed=1|v1",
            "low_level_tones|-100|seed=1|v1",
            "tiny_dc|levels=1e-5|seed=1|v1",
            "high_frequency|19000,18000|seed=1|v1",
        ] {
            assert!(validate_generator_spec(spec).is_err(), "accepted {spec}");
        }
    }

    #[test]
    fn oracle_case_seed_is_derived_from_its_fixture() {
        let fixture = FixtureSpec {
            id: "fixture".to_string(),
            kind: "generated".to_string(),
            generator: Some("program_multitone|seed=0xc001|v1".to_string()),
            generator_spec_sha256: None,
            path: None,
            start_sec: None,
            end_sec: None,
            sha256: None,
        };
        assert_eq!(fixture_generator_seed(&fixture).unwrap(), Some(0xc001));
        let committed = FixtureSpec {
            generator: None,
            kind: "committed-wav-window".to_string(),
            ..fixture
        };
        assert_eq!(fixture_generator_seed(&committed).unwrap(), None);
    }

    #[test]
    fn legacy_mono_oracle_case_fails_closed_without_channel() {
        let case = serde_json::json!({
            "case_id": "case--MinimumPhase",
            "source_case_id": "case",
            "fixture_id": "fixture",
            "category": "program",
            "filter": "MinimumPhase",
            "source_rate": 44_100,
            "wire_rate": 2_822_400,
            "generator_spec": "program_multitone|seed=1|v1",
            "generator_spec_sha256": sha256_hex(b"program_multitone|seed=1|v1"),
            "seed": 1,
            "start_sample": 0,
            "length_samples": 16
        });
        assert!(serde_json::from_value::<OracleCase>(case).is_err());
    }

    #[test]
    fn candidate_objective_config_rejects_altered_or_unknown_fields() {
        let valid = serde_json::json!({
            "state_terminal_weight": 0.1,
            "state_deadzone": 0.8,
            "state_deadzone_weight": 0.03,
            "quantizer_regularizer": 0.01,
            "ultrasonic_budget": null,
            "signed_error_budget": null
        });
        let decoded: OracleObjectiveConfig = serde_json::from_value(valid.clone()).unwrap();
        decoded.engine_config().validated().unwrap();

        let mut unknown = valid.clone();
        unknown["terminal_viability"] = serde_json::json!(true);
        assert!(serde_json::from_value::<OracleObjectiveConfig>(unknown).is_err());

        let mut invalid = valid;
        invalid["state_terminal_weight"] = serde_json::json!(-0.1);
        let decoded: OracleObjectiveConfig = serde_json::from_value(invalid).unwrap();
        assert!(decoded.engine_config().validated().is_err());
    }

    #[test]
    fn hashes_are_domain_and_order_sensitive() {
        assert_eq!(sha256_f64_le(&[0.0, 1.0]), sha256_f64_le(&[0.0, 1.0]));
        assert_ne!(sha256_f64_le(&[0.0, 1.0]), sha256_f64_le(&[1.0, 0.0]));
        assert_ne!(sha256_f64_le(&[0.0]), sha256_f64_le(&[-0.0]));
    }

    #[test]
    fn v1_plant_binding_pins_the_full_crfb_table_and_limits() {
        assert_eq!(coefficient_table_sha256(), V1_COEFFICIENTS_SHA256);
        assert_eq!(
            sha256_f64_le(&CRFB_OSR64_OBG165.state_limit),
            V1_STATE_LIMIT_SHA256
        );
        let plant = v1_plant_binding();
        assert_eq!(plant.osr, 64);
        assert_eq!(plant.obg, 1.65);
        assert_eq!(plant.input_peak, 0.23256);
        assert_eq!(plant.headroom_db, -2.0);
        assert_eq!(plant.isi_penalty, 0.0);
        assert_eq!(plant.dither_scale, 0.0);
    }

    #[test]
    fn frozen_budget_binding_checks_document_hash_and_limits() {
        let path = std::env::temp_dir().join(format!(
            "ecbeam2-oracle-budgets-{}-{}.json",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let bytes = br#"{"schema_version":"ecbeam2-frozen-budgets-v1","calibration_digest":"cal-1","by_wire_rate":{"2822400":{"ultrasonic_ema_max":0.25,"signed_error_ema_abs_max":0.01}}}"#;
        fs::write(&path, bytes).unwrap();
        let binding = FrozenBudgetBinding {
            schema_version: BUDGET_SCHEMA.to_string(),
            document_sha256: sha256_hex(bytes),
            calibration_digest: "cal-1".to_string(),
            by_wire_rate: BTreeMap::from([(
                "2822400".to_string(),
                FrozenWireBudget {
                    ultrasonic_ema_max: 0.25,
                    signed_error_ema_abs_max: 0.01,
                },
            )]),
        };
        assert_eq!(
            load_and_validate_budgets(Some(&path), Some(&binding)).unwrap(),
            Some(binding.clone())
        );
        let mut changed = binding;
        changed
            .by_wire_rate
            .get_mut("2822400")
            .unwrap()
            .ultrasonic_ema_max = 0.3;
        assert!(load_and_validate_budgets(Some(&path), Some(&changed)).is_err());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn mapped_prefix_uses_the_resamplers_zero_delay_wire_alignment() {
        for (filter, source_rate, wire_rate) in [
            (FilterType::Minimum16k, 44_100, 2_822_400),
            (FilterType::Split128k, 48_000, 3_072_000),
        ] {
            let source_start = 4096usize;
            let mapped = dsd_source_window_to_modulator_samples(
                filter,
                source_rate,
                wire_rate,
                source_start,
                16,
            )
            .unwrap();
            assert_eq!(mapped.start, source_start * 64);
            assert_eq!(mapped.end - mapped.start, 16 * 64);
        }
    }
}
