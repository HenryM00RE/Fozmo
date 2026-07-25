#![cfg(target_os = "macos")]

use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use std::env;
use std::time::{Duration, Instant};

const ENABLE_ENV: &str = "FOZMO_APPLE_MUSIC_REAL";
const CAPTURE_CONFIRM_ENV: &str = "FOZMO_APPLE_MUSIC_CONFIRM_CAPTURE";

#[derive(Debug)]
struct RealConfig {
    base_url: String,
    storefront: String,
    song_ids: [String; 2],
    local_track_id: i64,
    qobuz_track_id: u64,
    timeout: Duration,
}

impl RealConfig {
    fn from_env() -> Result<Option<Self>, String> {
        if env::var(ENABLE_ENV).ok().as_deref() != Some("1") {
            eprintln!(
                "SKIP Apple Music real-Mac tests: set {ENABLE_ENV}=1 only for an explicitly provisioned test run"
            );
            return Ok(None);
        }
        if env::var(CAPTURE_CONFIRM_ENV).ok().as_deref() != Some("1") {
            return Err(format!(
                "{CAPTURE_CONFIRM_ENV}=1 is required to confirm helper-process audio capture"
            ));
        }

        let song_ids = required("FOZMO_APPLE_MUSIC_SONG_IDS")?
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>();
        if song_ids.len() < 2 {
            return Err(
                "FOZMO_APPLE_MUSIC_SONG_IDS must contain two comma-separated catalog Song IDs"
                    .to_string(),
            );
        }
        for song_id in song_ids.iter().take(2) {
            if !song_id
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || ".-_".contains(character))
            {
                return Err(format!("unsafe Apple Music Song ID: {song_id}"));
            }
        }

        let local_track_id = parse_required("FOZMO_APPLE_MUSIC_LOCAL_TRACK_ID")?;
        let qobuz_track_id = parse_required("FOZMO_APPLE_MUSIC_QOBUZ_TRACK_ID")?;
        let timeout_secs = env::var("FOZMO_APPLE_MUSIC_TIMEOUT_SECS")
            .ok()
            .map(|value| {
                value
                    .parse::<u64>()
                    .map_err(|error| format!("invalid FOZMO_APPLE_MUSIC_TIMEOUT_SECS: {error}"))
            })
            .transpose()?
            .unwrap_or(30);

        Ok(Some(Self {
            base_url: env::var("FOZMO_APPLE_MUSIC_BASE_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:3000".to_string())
                .trim_end_matches('/')
                .to_string(),
            storefront: env::var("FOZMO_APPLE_MUSIC_STOREFRONT")
                .unwrap_or_else(|_| "nz".to_string()),
            song_ids: [song_ids[0].clone(), song_ids[1].clone()],
            local_track_id,
            qobuz_track_id,
            timeout: Duration::from_secs(timeout_secs),
        }))
    }
}

#[derive(Clone)]
struct Api {
    base_url: String,
    client: Client,
    timeout: Duration,
}

impl Api {
    fn new(config: &RealConfig) -> Result<Self, String> {
        let client = Client::builder()
            .timeout(config.timeout)
            .build()
            .map_err(|error| format!("build HTTP client: {error}"))?;
        Ok(Self {
            base_url: config.base_url.clone(),
            client,
            timeout: config.timeout,
        })
    }

    async fn get(&self, path: &str) -> Result<Value, String> {
        let response = self
            .client
            .get(format!("{}{}", self.base_url, path))
            .send()
            .await
            .map_err(|error| format!("GET {path}: {error}"))?;
        decode_response("GET", path, response).await
    }

    async fn post(&self, path: &str, body: Value) -> Result<Value, String> {
        let response = self
            .client
            .post(format!("{}{}", self.base_url, path))
            .json(&body)
            .send()
            .await
            .map_err(|error| format!("POST {path}: {error}"))?;
        decode_response("POST", path, response).await
    }

    async fn start(&self, source: Value, queue: Vec<Value>) -> Result<(), String> {
        self.post(
            "/api/apple-music/play",
            json!({
                "source": source,
                "queue": queue,
                "confirm_system_audio_capture": true
            }),
        )
        .await?;
        Ok(())
    }

    async fn stop(&self) -> Result<(), String> {
        self.post("/api/stop", json!({})).await?;
        self.wait("playback to stop", |status, apple| {
            !matches!(
                status.get("state").and_then(Value::as_str),
                Some("Playing" | "Paused" | "Starting")
            ) && apple.pointer("/process_tap/state").and_then(Value::as_str) != Some("running")
        })
        .await?;
        Ok(())
    }

    async fn wait<F>(&self, label: &str, predicate: F) -> Result<(Value, Value), String>
    where
        F: Fn(&Value, &Value) -> bool,
    {
        let deadline = Instant::now() + self.timeout;
        loop {
            let status = self.get("/api/status").await?;
            let apple = self.get("/api/apple-music/status").await?;
            if predicate(&status, &apple) {
                return Ok((status, apple));
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "timed out waiting for {label}; status={} apple={}",
                    compact(&status),
                    compact(&apple)
                ));
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    async fn wait_source(&self, kind: &str, provider_id: &str) -> Result<(Value, Value), String> {
        self.wait(&format!("{kind} source {provider_id}"), |status, _apple| {
            let source = status.get("current_source").unwrap_or(&Value::Null);
            source.get("kind").and_then(Value::as_str) == Some(kind)
                && match kind {
                    "apple_music_track" => {
                        source.get("song_id").and_then(Value::as_str) == Some(provider_id)
                    }
                    "local_track" => {
                        source.get("track_id").and_then(Value::as_i64) == provider_id.parse().ok()
                    }
                    "qobuz_track" => {
                        source.get("track_id").and_then(Value::as_u64) == provider_id.parse().ok()
                    }
                    _ => false,
                }
        })
        .await
    }
}

#[derive(Debug, PartialEq, Eq)]
struct TapIdentity {
    helper_pid: u64,
    target_pid: u64,
    target_process_kind: String,
    tap_object_id: u64,
    player_epoch: u64,
}

impl TapIdentity {
    fn from_status(apple: &Value) -> Result<Self, String> {
        Ok(Self {
            helper_pid: required_u64(apple, "/helper_pid")?,
            target_pid: required_u64(apple, "/process_tap/target_pid")?,
            target_process_kind: required_string(apple, "/process_tap/target_process_kind")?,
            tap_object_id: required_u64(apple, "/process_tap/tap_object_id")?,
            player_epoch: required_u64(apple, "/playback_session/player_epoch")?,
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provisioned_helper_runs_the_real_backend_matrix() -> Result<(), String> {
    let Some(config) = RealConfig::from_env()? else {
        return Ok(());
    };
    let api = Api::new(&config)?;

    println!("Apple Music real-Mac gate: helper entitlement and subscriber capability");
    api.post("/api/apple-music/launch", json!({})).await?;
    let readiness = api.get("/api/apple-music/status").await?;
    require(
        readiness
            .get("helper_musickit_entitled")
            .and_then(Value::as_bool)
            == Some(true),
        "helper is not signed with the MusicKit-enabled App ID's development profile",
    )?;
    require(
        readiness.get("authorization").and_then(Value::as_str) == Some("authorized"),
        "authorize Apple Music in Settings before running the real suite",
    )?;
    require(
        readiness
            .get("can_play_catalog_content")
            .and_then(Value::as_bool)
            == Some(true),
        "the signed-in account cannot play Apple Music catalog content",
    )?;

    let first_catalog = api
        .get(&format!(
            "/api/apple-music/catalog/songs/{}?storefront={}",
            config.song_ids[0], config.storefront
        ))
        .await?;
    api.get(&format!(
        "/api/apple-music/catalog/songs/{}?storefront={}",
        config.song_ids[1], config.storefront
    ))
    .await?;
    let first_duration = first_catalog
        .get("duration_secs")
        .and_then(Value::as_f64)
        .filter(|duration| *duration >= 12.0)
        .ok_or_else(|| {
            "first Apple test song needs a duration of at least 12 seconds".to_string()
        })?;

    let first = apple_source(&config.song_ids[0], &config.storefront);
    let second = apple_source(&config.song_ids[1], &config.storefront);
    let local = local_source(config.local_track_id);
    let qobuz = qobuz_source(config.qobuz_track_id);

    println!("Apple Music real-Mac gate: one song, DSP, pause/resume, seek, adjacent transition");
    api.stop().await?;
    api.start(first.clone(), vec![second.clone()]).await?;
    let (_status, apple) = api
        .wait("Apple DSP handoff", |status, apple| {
            source_matches(status, "apple_music_track", &config.song_ids[0])
                && apple.pointer("/process_tap/state").and_then(Value::as_str) == Some("running")
                && apple
                    .pointer("/process_tap/dsp_handoff_active")
                    .and_then(Value::as_bool)
                    == Some(true)
        })
        .await?;
    let adjacent_identity = TapIdentity::from_status(&apple)?;
    require(
        adjacent_identity.helper_pid != adjacent_identity.target_pid,
        "the process tap incorrectly targets the helper instead of MusicKit's renderer",
    )?;
    require(
        adjacent_identity.target_process_kind == "musickit_renderer",
        "the process tap does not identify its target as MusicKit's renderer",
    )?;

    api.post("/api/pause", json!({})).await?;
    api.wait("generic pause", |status, _apple| {
        status.get("state").and_then(Value::as_str) == Some("Paused")
    })
    .await?;
    api.post("/api/resume", json!({})).await?;
    api.wait("generic resume", |status, _apple| {
        status.get("state").and_then(Value::as_str) == Some("Playing")
    })
    .await?;

    let seek_target = (first_duration * 0.5).clamp(5.0, first_duration - 6.0);
    api.post("/api/seek", json!({ "seconds": seek_target }))
        .await?;
    api.wait("seeked Apple timeline", |status, _apple| {
        source_matches(status, "apple_music_track", &config.song_ids[0])
            && status
                .get("position_secs")
                .and_then(Value::as_f64)
                .is_some_and(|position| position >= seek_target - 3.0)
    })
    .await?;

    seek_to_tail(&api, first_duration).await?;
    let (_status, apple) = api
        .wait_source("apple_music_track", &config.song_ids[1])
        .await?;
    require(
        TapIdentity::from_status(&apple)? == adjacent_identity,
        "helper PID, tap object, or Player epoch changed inside an adjacent Apple run",
    )?;
    require(
        apple
            .pointer("/playback_session/current_segment_index")
            .and_then(Value::as_u64)
            == Some(1),
        "adjacent Apple transition did not advance the segment index",
    )?;
    require(
        no_ring_overruns(&apple),
        "process tap reported a ring overrun",
    )?;

    println!("Apple Music real-Mac gate: manual Next stays inside the prepared Apple run");
    api.stop().await?;
    api.start(first.clone(), vec![second.clone()]).await?;
    let (_status, apple) = api
        .wait_source("apple_music_track", &config.song_ids[0])
        .await?;
    let manual_next_identity = TapIdentity::from_status(&apple)?;
    api.post("/api/next", json!({})).await?;
    let (_status, apple) = api
        .wait_source("apple_music_track", &config.song_ids[1])
        .await?;
    require(
        TapIdentity::from_status(&apple)? == manual_next_identity,
        "manual Next rebuilt the helper tap inside an Apple run",
    )?;

    println!("Apple Music real-Mac gate: Local -> Apple");
    api.stop().await?;
    api.start(local.clone(), vec![first.clone()]).await?;
    api.wait_source("local_track", &config.local_track_id.to_string())
        .await?;
    api.post("/api/next", json!({})).await?;
    api.wait_source("apple_music_track", &config.song_ids[0])
        .await?;

    println!("Apple Music real-Mac gate: Qobuz -> Apple");
    api.stop().await?;
    api.start(qobuz.clone(), vec![first.clone()]).await?;
    api.wait_source("qobuz_track", &config.qobuz_track_id.to_string())
        .await?;
    api.post("/api/next", json!({})).await?;
    api.wait_source("apple_music_track", &config.song_ids[0])
        .await?;

    println!("Apple Music real-Mac gate: Apple -> Local");
    api.stop().await?;
    api.start(first.clone(), vec![local]).await?;
    api.wait_source("apple_music_track", &config.song_ids[0])
        .await?;
    seek_to_tail(&api, first_duration).await?;
    api.wait_source("local_track", &config.local_track_id.to_string())
        .await?;

    println!("Apple Music real-Mac gate: Apple -> Qobuz");
    api.stop().await?;
    api.start(first.clone(), vec![qobuz]).await?;
    api.wait_source("apple_music_track", &config.song_ids[0])
        .await?;
    seek_to_tail(&api, first_duration).await?;
    api.wait_source("qobuz_track", &config.qobuz_track_id.to_string())
        .await?;

    println!("Apple Music real-Mac gate: helper termination owns only its Player epoch");
    api.stop().await?;
    api.start(first, vec![second]).await?;
    api.wait_source("apple_music_track", &config.song_ids[0])
        .await?;
    api.post("/api/apple-music/shutdown", json!({})).await?;
    api.wait("helper-failure cleanup", |status, apple| {
        !matches!(
            status.get("state").and_then(Value::as_str),
            Some("Playing" | "Paused" | "Starting")
        ) && apple.pointer("/process_tap/state").and_then(Value::as_str) != Some("running")
    })
    .await?;
    let queue = api.get("/api/zones/local-core/now-playing-queue").await?;
    require(
        queue
            .get("queued_sources")
            .and_then(Value::as_array)
            .is_some_and(|sources| {
                sources.iter().any(|source| {
                    source.get("song_id").and_then(Value::as_str)
                        == Some(config.song_ids[1].as_str())
                })
            }),
        "helper termination did not retain the remaining Fozmo queue",
    )?;

    println!(
        "PASS automated real-Mac matrix. Authorization revocation and process-tap permission revocation remain intentional manual OS actions."
    );
    Ok(())
}

async fn seek_to_tail(api: &Api, duration: f64) -> Result<(), String> {
    api.post("/api/seek", json!({ "seconds": (duration - 4.0).max(0.0) }))
        .await?;
    Ok(())
}

fn apple_source(song_id: &str, storefront: &str) -> Value {
    json!({
        "kind": "apple_music_track",
        "song_id": song_id,
        "storefront": storefront
    })
}

fn local_source(track_id: i64) -> Value {
    json!({
        "kind": "local_track",
        "track_id": track_id,
        "title": null,
        "artist": null
    })
}

fn qobuz_source(track_id: u64) -> Value {
    json!({
        "kind": "qobuz_track",
        "track_id": track_id,
        "title": null,
        "artist": null,
        "album": null,
        "image_url": null
    })
}

fn source_matches(status: &Value, kind: &str, provider_id: &str) -> bool {
    let source = status.get("current_source").unwrap_or(&Value::Null);
    source.get("kind").and_then(Value::as_str) == Some(kind)
        && match kind {
            "apple_music_track" => {
                source.get("song_id").and_then(Value::as_str) == Some(provider_id)
            }
            "local_track" => {
                source.get("track_id").and_then(Value::as_i64) == provider_id.parse().ok()
            }
            "qobuz_track" => {
                source.get("track_id").and_then(Value::as_u64) == provider_id.parse().ok()
            }
            _ => false,
        }
}

fn no_ring_overruns(apple: &Value) -> bool {
    apple
        .pointer("/process_tap/metrics/ring_overruns")
        .and_then(Value::as_u64)
        == Some(0)
}

fn required(name: &str) -> Result<String, String> {
    env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("{name} is required"))
}

fn parse_required<T>(name: &str) -> Result<T, String>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    required(name)?
        .parse()
        .map_err(|error| format!("invalid {name}: {error}"))
}

fn required_u64(value: &Value, pointer: &str) -> Result<u64, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("missing {pointer} in {}", compact(value)))
}

fn required_string(value: &Value, pointer: &str) -> Result<String, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("missing string at {pointer}"))
}

fn require(condition: bool, message: &str) -> Result<(), String> {
    condition.then_some(()).ok_or_else(|| message.to_string())
}

fn compact(value: &Value) -> String {
    let text = value.to_string();
    if text.chars().count() > 1_200 {
        format!("{}…", text.chars().take(1_200).collect::<String>())
    } else {
        text
    }
}

async fn decode_response(
    method: &str,
    path: &str,
    response: reqwest::Response,
) -> Result<Value, String> {
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|error| format!("{method} {path} read response: {error}"))?;
    if !status.is_success() {
        return Err(format!(
            "{method} {path} returned {}: {}",
            status,
            text.trim()
        ));
    }
    if status == StatusCode::NO_CONTENT || text.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(&text)
        .map_err(|error| format!("{method} {path} returned invalid JSON: {error}: {text}"))
}
