use clap::{Args, Parser, Subcommand};
use rand::{Rng, distributions::Alphanumeric};
use reqwest::{Client, Method, StatusCode, Url};
use serde_json::{Value, json};
use std::error::Error;
use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const DEFAULT_CORE_URL: &str = "http://127.0.0.1:3001";
const AUTO_CORE_URLS: [&str; 2] = ["http://127.0.0.1:3001", "http://127.0.0.1:3000"];

type CliResult<T> = Result<T, CliError>;

#[derive(Debug)]
struct CliError(String);

impl CliError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Error for CliError {}

#[derive(Parser, Debug)]
#[command(name = "fozmoctl")]
#[command(about = "Control a Fozmo core over HTTP")]
struct Cli {
    #[arg(long, global = true, value_name = "URL")]
    core_url: Option<String>,
    #[arg(long, global = true, value_name = "TOKEN")]
    token: Option<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    Doctor(DoctorArgs),
    Status(StatusArgs),
    Search(SearchArgs),
    TrackSearch(TrackSearchArgs),
    Play(PlayArgs),
    Volume(VolumeArgs),
    Queue {
        #[command(subcommand)]
        command: QueueCommand,
    },
    #[command(alias = "mix")]
    Playlist {
        #[command(subcommand)]
        command: PlaylistCommand,
    },
    Pause(ControlArgs),
    Resume(ControlArgs),
    Next(ControlArgs),
    Stop(ControlArgs),
    History {
        #[command(subcommand)]
        command: HistoryCommand,
    },
    #[command(alias = "zone")]
    Zones {
        #[command(subcommand)]
        command: ZonesCommand,
    },
    Qobuz {
        #[command(subcommand)]
        command: QobuzCommand,
    },
}

#[derive(Args, Debug)]
struct DoctorArgs {
    #[arg(long)]
    qobuz: bool,
}

#[derive(Args, Debug)]
struct JsonArgs {
    #[arg(long)]
    json: bool,
}

#[derive(Args, Debug, Clone, Default)]
struct ZoneTargetArgs {
    #[arg(long, value_name = "ZONE")]
    zone: Option<String>,
    #[arg(long, value_name = "ZONE_ID")]
    zone_id: Option<String>,
}

#[derive(Args, Debug)]
struct StatusArgs {
    #[command(flatten)]
    zone: ZoneTargetArgs,
    #[arg(long)]
    json: bool,
}

#[derive(Args, Debug)]
struct SearchArgs {
    query: String,
    #[arg(long)]
    json: bool,
}

#[derive(Args, Debug)]
struct TrackSearchArgs {
    query: String,
    #[arg(long)]
    ranked: bool,
    #[arg(long)]
    best: bool,
    #[arg(long, value_name = "N")]
    limit: Option<usize>,
    #[arg(long)]
    json: bool,
}

#[derive(Args, Debug)]
struct PlayArgs {
    source: Option<String>,
    #[arg(long)]
    track_id: Option<i64>,
    #[arg(long)]
    file_name: Option<String>,
    #[arg(long, value_name = "SOURCE", num_args = 1..)]
    queue: Vec<String>,
    #[command(flatten)]
    zone: ZoneTargetArgs,
    #[arg(long)]
    json: bool,
}

#[derive(Args, Debug)]
struct ControlArgs {
    #[command(flatten)]
    zone: ZoneTargetArgs,
    #[arg(long)]
    json: bool,
}

#[derive(Args, Debug)]
struct VolumeArgs {
    #[arg(value_name = "VOLUME")]
    volume: Option<String>,
    #[command(flatten)]
    zone: ZoneTargetArgs,
    /// Control output-device volume instead of the Fozmo playback gain.
    #[arg(long)]
    device: bool,
    /// Control the Hegel amplifier volume using saved Hegel settings.
    #[arg(long)]
    hegel: bool,
    /// Adjust Hegel volume by one native step.
    #[arg(long, value_parser = ["up", "down"])]
    direction: Option<String>,
    #[arg(long)]
    json: bool,
}

#[derive(Subcommand, Debug)]
enum HistoryCommand {
    Top(HistoryTopArgs),
}

#[derive(Args, Debug)]
struct HistoryTopArgs {
    #[arg(long, default_value = "week")]
    range: String,
    #[arg(long, default_value_t = 25)]
    limit: i64,
    #[arg(long)]
    profile: Option<String>,
    #[arg(long)]
    exclude_radio: bool,
    #[arg(long)]
    json: bool,
}

#[derive(Subcommand, Debug)]
enum QueueCommand {
    Get(QueueGetArgs),
    Add(QueueAddArgs),
    AddMany(QueueAddManyArgs),
}

#[derive(Args, Debug)]
struct QueueGetArgs {
    #[command(flatten)]
    zone: ZoneTargetArgs,
    #[arg(long)]
    summary: bool,
    #[arg(long, value_name = "N")]
    limit: Option<usize>,
    #[arg(long)]
    json: bool,
}

#[derive(Args, Debug)]
struct QueueAddArgs {
    source: Option<String>,
    #[arg(long)]
    track_id: Option<i64>,
    #[arg(long)]
    file_name: Option<String>,
    #[command(flatten)]
    zone: ZoneTargetArgs,
    #[arg(long)]
    json: bool,
}

#[derive(Args, Debug)]
struct QueueAddManyArgs {
    #[command(flatten)]
    zone: ZoneTargetArgs,
    #[arg(long)]
    json: bool,
    #[arg(required = true, value_name = "SOURCE")]
    sources: Vec<String>,
}

#[derive(Subcommand, Debug)]
enum PlaylistCommand {
    List(JsonArgs),
    Show(PlaylistShowArgs),
    Create(PlaylistCreateArgs),
    Add(PlaylistAddArgs),
}

#[derive(Args, Debug)]
struct PlaylistShowArgs {
    #[arg(value_name = "PLAYLIST_ID")]
    playlist_id: String,
    #[arg(long)]
    json: bool,
}

#[derive(Args, Debug)]
struct PlaylistCreateArgs {
    #[arg(long)]
    name: String,
    #[arg(long, value_name = "PLAYLIST_ID")]
    id: Option<String>,
    #[arg(long)]
    json: bool,
    #[arg(value_name = "SOURCE")]
    sources: Vec<String>,
}

#[derive(Args, Debug)]
struct PlaylistAddArgs {
    #[arg(value_name = "PLAYLIST_ID")]
    playlist_id: String,
    #[arg(long)]
    json: bool,
    #[arg(required = true, value_name = "SOURCE")]
    sources: Vec<String>,
}

#[derive(Subcommand, Debug)]
enum ZonesCommand {
    List(JsonArgs),
    Select(ZoneSelectArgs),
    Swap(ZoneSwapArgs),
    UpnpDiagnostics(ZoneUpnpDiagnosticsArgs),
}

#[derive(Args, Debug)]
struct ZoneSelectArgs {
    #[arg(long)]
    zone_id: String,
}

#[derive(Args, Debug)]
struct ZoneSwapArgs {
    #[arg(long, value_name = "ZONE")]
    from: String,
    #[arg(long, value_name = "ZONE")]
    to: String,
    #[arg(long, conflicts_with = "keep_source_playing")]
    pause_source: bool,
    #[arg(long)]
    keep_source_playing: bool,
    #[arg(long)]
    json: bool,
}

#[derive(Args, Debug)]
struct ZoneUpnpDiagnosticsArgs {
    #[command(flatten)]
    zone: ZoneTargetArgs,
    #[arg(long)]
    json: bool,
}

#[derive(Subcommand, Debug)]
enum QobuzCommand {
    Search(SearchArgs),
    Play(QobuzTrackArgs),
    Queue {
        #[command(subcommand)]
        command: QobuzQueueCommand,
    },
}

#[derive(Subcommand, Debug)]
enum QobuzQueueCommand {
    Add(QobuzTrackArgs),
}

#[derive(Args, Debug)]
struct QobuzTrackArgs {
    #[arg(long)]
    track_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SourceSpec {
    Local(i64),
    Qobuz(u64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum LocalTarget {
    TrackId(i64),
    FileName(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PlayTarget {
    Local(LocalTarget),
    Qobuz(u64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedZone {
    id: String,
    name: String,
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("fozmoctl: {error}");
        std::process::exit(1);
    }
}

async fn run() -> CliResult<()> {
    let cli = Cli::parse();
    let mut client = CoreClient::from_cli(&cli).await;
    match cli.command {
        Command::Doctor(args) => run_doctor(&mut client, args.qobuz).await,
        Command::Status(args) => {
            let zone = resolve_zone_target(&mut client, &args.zone).await?;
            let status = client.get_json(&status_path(zone.as_ref()), &[]).await?;
            print_json_or_status(&status, args.json)
        }
        Command::Search(args) => {
            let search = client
                .get_json("/api/library/search", &[("q", args.query.as_str())])
                .await?;
            print_json_or_search(&search, args.json, SearchSource::Local)
        }
        Command::TrackSearch(args) => run_track_search(&mut client, args).await,
        Command::Play(args) => {
            let zone = resolve_zone_target(&mut client, &args.zone).await?;
            play_target(
                &mut client,
                zone.as_ref(),
                top_level_play_target(&args)?,
                args.queue,
                args.json,
            )
            .await
        }
        Command::Volume(args) => run_volume(&mut client, args).await,
        Command::Queue { command } => match command {
            QueueCommand::Get(args) => {
                let zone = resolve_zone_target(&mut client, &args.zone).await?;
                let queue = client
                    .get_json(&now_playing_queue_path(zone.as_ref()), &[])
                    .await?;
                if args.summary {
                    print_json_or_queue_summary(&queue, args.json, args.limit)
                } else if args.limit.is_some() {
                    Err(CliError::new("queue get --limit requires --summary"))
                } else {
                    print_json_or_queue(&queue, args.json)
                }
            }
            QueueCommand::Add(args) => {
                let zone = resolve_zone_target(&mut client, &args.zone).await?;
                append_queue_target(
                    &mut client,
                    zone.as_ref(),
                    top_level_queue_target(&args)?,
                    args.json,
                )
                .await
            }
            QueueCommand::AddMany(args) => {
                let zone = resolve_zone_target(&mut client, &args.zone).await?;
                append_queue_sources(&mut client, zone.as_ref(), args.sources, args.json).await
            }
        },
        Command::Playlist { command } => match command {
            PlaylistCommand::List(args) => run_playlist_list(&mut client, args).await,
            PlaylistCommand::Show(args) => run_playlist_show(&mut client, args).await,
            PlaylistCommand::Create(args) => run_playlist_create(&mut client, args).await,
            PlaylistCommand::Add(args) => run_playlist_add(&mut client, args).await,
        },
        Command::Pause(args) => run_control(&mut client, &args.zone, "pause", args.json).await,
        Command::Resume(args) => run_control(&mut client, &args.zone, "resume", args.json).await,
        Command::Next(args) => run_control(&mut client, &args.zone, "next", args.json).await,
        Command::Stop(args) => run_control(&mut client, &args.zone, "stop", args.json).await,
        Command::History { command } => match command {
            HistoryCommand::Top(args) => run_history_top(&mut client, args).await,
        },
        Command::Zones { command } => match command {
            ZonesCommand::List(args) => {
                let zones = client.get_json("/api/zones", &[]).await?;
                print_json_or_zones(&zones, args.json)
            }
            ZonesCommand::Select(args) => {
                client
                    .post_unit("/api/zones/select", json!({ "zone_id": args.zone_id }))
                    .await?;
                let status = client.get_json("/api/status", &[]).await?;
                let active_zone_id = status.get("active_zone_id").and_then(Value::as_str);
                if active_zone_id != Some(args.zone_id.as_str()) {
                    let active = active_zone_id.unwrap_or("unknown");
                    let active_name = status
                        .get("active_zone_name")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown");
                    return Err(CliError::new(format!(
                        "selected zone '{}' but active zone is '{}' ({active_name}); use --zone '{}' or --zone-id '{}' on playback commands for reliable routing",
                        args.zone_id, active, args.zone_id, args.zone_id
                    )));
                }
                println!("Selected zone {}", args.zone_id);
                Ok(())
            }
            ZonesCommand::Swap(args) => run_zone_swap(&mut client, args).await,
            ZonesCommand::UpnpDiagnostics(args) => {
                let zone = resolve_zone_target(&mut client, &args.zone).await?;
                let zone_id = match zone.as_ref() {
                    Some(zone) => zone.id.clone(),
                    None => client
                        .get_json("/api/status", &[])
                        .await?
                        .get("active_zone_id")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .ok_or_else(|| {
                            CliError::new("status response is missing active_zone_id")
                        })?,
                };
                let diagnostics = client
                    .get_json(&format!("/api/diagnostics/upnp/{zone_id}"), &[])
                    .await?;
                print_json_or_upnp_diagnostics(&diagnostics, args.json)
            }
        },
        Command::Qobuz { command } => match command {
            QobuzCommand::Search(args) => {
                let search = client
                    .get_json("/api/qobuz/search", &[("q", args.query.as_str())])
                    .await?;
                print_json_or_search(&search, args.json, SearchSource::Qobuz)
            }
            QobuzCommand::Play(args) => {
                play_qobuz_track(
                    &mut client,
                    None,
                    args.track_id,
                    Vec::new(),
                    Vec::new(),
                    false,
                )
                .await
            }
            QobuzCommand::Queue { command } => match command {
                QobuzQueueCommand::Add(args) => {
                    append_queue_target(&mut client, None, PlayTarget::Qobuz(args.track_id), false)
                        .await
                }
            },
        },
    }
}

struct CoreClient {
    http: Client,
    core_url: String,
    token: Option<String>,
    token_supplied: bool,
    pairing_attempted: bool,
}

async fn discover_core_url(http: &Client) -> Option<String> {
    for core_url in AUTO_CORE_URLS {
        if core_status_probe_succeeds(http, core_url).await {
            return Some(core_url.to_string());
        }
    }
    None
}

async fn core_status_probe_succeeds(http: &Client, core_url: &str) -> bool {
    let url = format!("{}/api/status", core_url.trim_end_matches('/'));
    match http
        .get(url)
        .timeout(Duration::from_millis(250))
        .send()
        .await
    {
        Ok(response) => matches!(response.status(), StatusCode::OK | StatusCode::UNAUTHORIZED),
        Err(_) => false,
    }
}

impl CoreClient {
    async fn from_cli(cli: &Cli) -> Self {
        let explicit_core_url = cli
            .core_url
            .clone()
            .or_else(|| env_non_empty("FOZMO_CORE_URL"));
        let http = Client::new();
        let core_url = match explicit_core_url {
            Some(core_url) => core_url,
            None => discover_core_url(&http)
                .await
                .unwrap_or_else(|| DEFAULT_CORE_URL.to_string()),
        };
        let token = cli
            .token
            .clone()
            .or_else(|| env_non_empty("FOZMO_PAIRING_TOKEN"));
        let token_supplied = token.is_some();
        Self {
            http,
            core_url: core_url.trim_end_matches('/').to_string(),
            token,
            token_supplied,
            pairing_attempted: false,
        }
    }

    async fn get_json(&mut self, path: &str, query: &[(&str, &str)]) -> CliResult<Value> {
        let text = self.request_text(Method::GET, path, query, None).await?;
        serde_json::from_str(&text)
            .map_err(|e| CliError::new(format!("parse JSON from {path}: {e}")))
    }

    async fn post_unit(&mut self, path: &str, body: Value) -> CliResult<()> {
        self.request_text(Method::POST, path, &[], Some(body))
            .await?;
        Ok(())
    }

    async fn post_json(&mut self, path: &str, body: Value) -> CliResult<Value> {
        let text = self
            .request_text(Method::POST, path, &[], Some(body))
            .await?;
        serde_json::from_str(&text)
            .map_err(|e| CliError::new(format!("parse JSON from {path}: {e}")))
    }

    async fn put_json(&mut self, path: &str, body: Value) -> CliResult<Value> {
        let text = self
            .request_text(Method::PUT, path, &[], Some(body))
            .await?;
        serde_json::from_str(&text)
            .map_err(|e| CliError::new(format!("parse JSON from {path}: {e}")))
    }

    async fn post_empty(&mut self, path: &str) -> CliResult<()> {
        self.request_text(Method::POST, path, &[], None).await?;
        Ok(())
    }

    async fn request_text(
        &mut self,
        method: Method,
        path: &str,
        query: &[(&str, &str)],
        body: Option<Value>,
    ) -> CliResult<String> {
        let mut response = self.send(method.clone(), path, query, body.clone()).await?;
        if response.status() == StatusCode::UNAUTHORIZED
            && !self.token_supplied
            && !self.pairing_attempted
        {
            self.obtain_pairing_token().await?;
            response = self.send(method, path, query, body).await?;
        }
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| CliError::new(format!("read response from {path}: {e}")))?;
        if status.is_success() {
            Ok(text)
        } else {
            let detail = text.trim();
            if detail.is_empty() {
                Err(CliError::new(format!("{path} returned HTTP {status}")))
            } else {
                Err(CliError::new(format!(
                    "{path} returned HTTP {status}: {detail}"
                )))
            }
        }
    }

    async fn send(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, &str)],
        body: Option<Value>,
    ) -> CliResult<reqwest::Response> {
        let url = self.url(path, query)?;
        let mut request = self.http.request(method, url);
        if let Some(token) = self
            .token
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            request = request.header("x-fozmo-token", token);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        request
            .send()
            .await
            .map_err(|e| CliError::new(format!("request {path}: {e}")))
    }

    fn url(&self, path: &str, query: &[(&str, &str)]) -> CliResult<Url> {
        let mut url = Url::parse(&format!("{}{}", self.core_url, path))
            .map_err(|e| CliError::new(format!("invalid core URL '{}': {e}", self.core_url)))?;
        {
            let mut pairs = url.query_pairs_mut();
            for (key, value) in query {
                pairs.append_pair(key, value);
            }
        }
        Ok(url)
    }

    async fn obtain_pairing_token(&mut self) -> CliResult<()> {
        self.pairing_attempted = true;
        let url = self.url("/api/pairing/start", &[])?;
        let response = self
            .http
            .post(url)
            .send()
            .await
            .map_err(|e| CliError::new(format!("request /api/pairing/start: {e}")))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| CliError::new(format!("read pairing response: {e}")))?;
        if !status.is_success() {
            return Err(CliError::new(format!(
                "/api/pairing/start returned HTTP {status}: {}",
                text.trim()
            )));
        }
        let pairing: Value = serde_json::from_str(&text)
            .map_err(|e| CliError::new(format!("parse pairing response: {e}")))?;
        let token = pairing
            .get("token")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .ok_or_else(|| CliError::new("pairing response did not include a token"))?;
        self.token = Some(token.to_string());
        Ok(())
    }
}

async fn run_doctor(client: &mut CoreClient, require_qobuz: bool) -> CliResult<()> {
    let mut failures = 0usize;
    let mut warnings = 0usize;

    doctor_check(
        "core status",
        client.get_json("/api/status", &[]).await.map(|_| ()),
        true,
        &mut failures,
        &mut warnings,
    );
    doctor_check(
        "local library search",
        client
            .get_json("/api/library/search", &[("q", "")])
            .await
            .map(|_| ()),
        true,
        &mut failures,
        &mut warnings,
    );
    doctor_check(
        "zones",
        client.get_json("/api/zones", &[]).await.map(|_| ()),
        true,
        &mut failures,
        &mut warnings,
    );
    doctor_check(
        "queue",
        client
            .get_json("/api/now-playing-queue", &[])
            .await
            .map(|_| ()),
        true,
        &mut failures,
        &mut warnings,
    );

    match client.get_json("/api/qobuz/status", &[]).await {
        Ok(status) => {
            let logged_in = status
                .get("logged_in")
                .or_else(|| status.get("authenticated"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if logged_in {
                println!("ok   qobuz");
            } else if require_qobuz {
                failures += 1;
                println!("fail qobuz: not logged in");
            } else {
                warnings += 1;
                println!("warn qobuz: not logged in");
            }
        }
        Err(error) => {
            if require_qobuz {
                failures += 1;
                println!("fail qobuz: {error}");
            } else {
                warnings += 1;
                println!("warn qobuz: {error}");
            }
        }
    }

    if failures > 0 {
        Err(CliError::new(format!(
            "doctor found {failures} failure(s) and {warnings} warning(s)"
        )))
    } else {
        if warnings > 0 {
            println!("doctor passed with {warnings} warning(s)");
        } else {
            println!("doctor passed");
        }
        Ok(())
    }
}

fn doctor_check(
    label: &str,
    result: CliResult<()>,
    fatal: bool,
    failures: &mut usize,
    warnings: &mut usize,
) {
    match result {
        Ok(()) => println!("ok   {label}"),
        Err(error) if fatal => {
            *failures += 1;
            println!("fail {label}: {error}");
        }
        Err(error) => {
            *warnings += 1;
            println!("warn {label}: {error}");
        }
    }
}

async fn run_control(
    client: &mut CoreClient,
    target: &ZoneTargetArgs,
    command: &str,
    as_json: bool,
) -> CliResult<()> {
    let zone = resolve_zone_target(client, target).await?;
    client
        .post_empty(&control_path(zone.as_ref(), command))
        .await?;
    print_mutation_confirmation(client, zone.as_ref(), as_json).await
}

async fn run_zone_swap(client: &mut CoreClient, args: ZoneSwapArgs) -> CliResult<()> {
    let zones = client.get_json("/api/zones", &[]).await?;
    let source = resolve_zone_label_from_list(&zones, &args.from)?;
    let destination = resolve_zone_label_from_list(&zones, &args.to)?;
    if source.id == destination.id {
        return Err(CliError::new("source and destination zones must differ"));
    }
    let source_snapshot = client
        .get_json(&now_playing_queue_path(Some(&source)), &[])
        .await
        .ok();
    let expected_current = source_snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.get("current_source"))
        .and_then(source_ref_key);
    let source_action = if args.keep_source_playing {
        "keep_playing"
    } else {
        let _pause_source = args.pause_source;
        "pause"
    };
    let response = client
        .post_json(
            &transfer_path(&source),
            json!({
                "destination_zone_id": destination.id.clone(),
                "source_action": source_action,
                "expected_current": expected_current,
            }),
        )
        .await?;
    print_json_or_zone_swap(&response, &source, &destination, args.json)
}

async fn run_volume(client: &mut CoreClient, args: VolumeArgs) -> CliResult<()> {
    if args.device && args.hegel {
        return Err(CliError::new("use either --device or --hegel, not both"));
    }
    if args.direction.is_some() && !args.hegel {
        return Err(CliError::new("--direction is only supported with --hegel"));
    }
    if args.hegel {
        return run_hegel_volume(client, args).await;
    }

    if args.direction.is_some() {
        return Err(CliError::new("--direction is only supported with --hegel"));
    }
    let requested = args
        .volume
        .as_deref()
        .ok_or_else(|| CliError::new("volume is required"))?;
    let volume = parse_normalized_volume(requested)?;
    let zone = resolve_zone_target(client, &args.zone).await?;
    let command = if args.device {
        "device-volume"
    } else {
        "volume"
    };
    client
        .post_unit(
            &control_path(zone.as_ref(), command),
            json!({ "volume": volume }),
        )
        .await?;
    print_volume_confirmation(
        client,
        zone.as_ref(),
        VolumeConfirmation {
            mode: if args.device { "device" } else { "playback" },
            requested: Value::String(requested.trim().to_string()),
            applied: volume,
            max: if args.device {
                device_volume_max_for_status
            } else {
                |_| Some(1.0)
            },
        },
        args.json,
    )
    .await
}

async fn run_hegel_volume(client: &mut CoreClient, args: VolumeArgs) -> CliResult<()> {
    let target_zone = resolve_hegel_zone_target(client, &args.zone).await?;
    let settings = client.get_json("/api/hegel/settings", &[]).await?;
    let hegel = hegel_target_from_settings(&settings, target_zone.as_ref())?;
    let body = if let Some(direction) = args.direction.as_deref() {
        if args.volume.is_some() {
            return Err(CliError::new(
                "Hegel --direction cannot be combined with an explicit volume",
            ));
        }
        json!({
            "host": &hegel.host,
            "port": hegel.port,
            "direction": direction,
        })
    } else {
        let requested = args
            .volume
            .as_deref()
            .ok_or_else(|| CliError::new("Hegel volume or --direction is required"))?;
        json!({
            "host": &hegel.host,
            "port": hegel.port,
            "volume": parse_hegel_volume(requested)?,
        })
    };
    let status = client.post_json("/api/hegel/volume", body).await?;
    print_hegel_volume_confirmation(&status, &hegel, args.json)
}

async fn resolve_hegel_zone_target(
    client: &mut CoreClient,
    target: &ZoneTargetArgs,
) -> CliResult<Option<ResolvedZone>> {
    if zone_target_requested(target) {
        resolve_zone_target(client, target).await
    } else {
        Ok(None)
    }
}

#[derive(Debug)]
struct HegelVolumeTarget {
    host: String,
    port: u16,
    zone_id: String,
    max_volume: u8,
}

fn hegel_target_from_settings(
    settings: &Value,
    zone: Option<&ResolvedZone>,
) -> CliResult<HegelVolumeTarget> {
    if settings.get("enabled").and_then(Value::as_bool) != Some(true) {
        return Err(CliError::new(
            "Hegel volume requires saved and enabled Hegel settings",
        ));
    }
    let host = settings
        .get("host")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|host| !host.is_empty())
        .ok_or_else(|| CliError::new("Hegel settings are missing host"))?
        .to_string();
    let zone_id = settings
        .get("zone_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|zone_id| !zone_id.is_empty())
        .ok_or_else(|| CliError::new("Hegel settings are missing zone_id"))?
        .to_string();
    if let Some(zone) = zone
        && zone.id != zone_id
    {
        return Err(CliError::new(format!(
            "Hegel is configured for zone '{}' but '{}' resolved to '{}'",
            zone_id, zone.name, zone.id
        )));
    }
    let port = settings
        .get("port")
        .and_then(Value::as_u64)
        .filter(|port| *port > 0 && *port <= u16::MAX as u64)
        .unwrap_or(50001) as u16;
    let max_volume = settings
        .get("max_volume")
        .and_then(Value::as_u64)
        .map(|value| value.min(100) as u8)
        .unwrap_or(50);
    Ok(HegelVolumeTarget {
        host,
        port,
        zone_id,
        max_volume,
    })
}

struct VolumeConfirmation {
    mode: &'static str,
    requested: Value,
    applied: f32,
    max: fn(&Value) -> Option<f32>,
}

async fn print_volume_confirmation(
    client: &mut CoreClient,
    zone: Option<&ResolvedZone>,
    confirmation: VolumeConfirmation,
    as_json: bool,
) -> CliResult<()> {
    let status = client.get_json(&status_path(zone), &[]).await?;
    let max = (confirmation.max)(&status);
    let observed = match confirmation.mode {
        "device" => status.get("device_volume").and_then(Value::as_f64),
        _ => status.get("volume").and_then(Value::as_f64),
    }
    .filter(|value| value.is_finite())
    .map(|value| value.clamp(0.0, 1.0) as f32);
    let applied = observed.unwrap_or_else(|| {
        max.map(|max| confirmation.applied.min(max))
            .unwrap_or(confirmation.applied)
    });
    let response = json!({
        "zone_id": status
            .get("active_zone_id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| zone.map(|zone| zone.id.clone()))
            .unwrap_or_else(|| "unknown".to_string()),
        "zone_name": status
            .get("active_zone_name")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| zone.map(|zone| zone.name.clone()))
            .unwrap_or_else(|| "Unknown zone".to_string()),
        "mode": confirmation.mode,
        "requested": confirmation.requested,
        "applied": applied,
        "max": max.map(Value::from).unwrap_or(Value::Null),
        "volume": status.get("volume").cloned().unwrap_or(Value::Null),
        "device_volume": status.get("device_volume").cloned().unwrap_or(Value::Null),
        "device_volume_supported": status
            .get("device_volume_supported")
            .cloned()
            .unwrap_or(Value::Bool(false)),
        "device_volume_message": status
            .get("device_volume_message")
            .cloned()
            .unwrap_or(Value::Null),
    });
    if as_json {
        return print_json(&response);
    }
    let zone_name = response
        .get("zone_name")
        .and_then(Value::as_str)
        .unwrap_or("Unknown zone");
    let applied_percent = (applied * 100.0).round() as i64;
    let max_text = max
        .map(|value| format!(" max={}%", (value * 100.0).round() as i64))
        .unwrap_or_default();
    println!(
        "{} volume set to {}% [{}]{}",
        confirmation.mode, applied_percent, zone_name, max_text
    );
    Ok(())
}

fn device_volume_max_for_status(status: &Value) -> Option<f32> {
    status
        .get("device_volume_max")
        .and_then(Value::as_f64)
        .map(|value| {
            if value.is_finite() {
                value.clamp(0.0, 1.0) as f32
            } else {
                1.0
            }
        })
}

fn print_hegel_volume_confirmation(
    status: &Value,
    target: &HegelVolumeTarget,
    as_json: bool,
) -> CliResult<()> {
    let applied = status.get("volume").and_then(Value::as_u64);
    let response = json!({
        "zone_id": &target.zone_id,
        "mode": "hegel",
        "applied": applied.map(Value::from).unwrap_or(Value::Null),
        "max": target.max_volume,
        "power": status.get("power").cloned().unwrap_or(Value::Null),
        "input": status.get("input").cloned().unwrap_or(Value::Null),
        "muted": status.get("muted").cloned().unwrap_or(Value::Null),
    });
    if as_json {
        return print_json(&response);
    }
    match applied {
        Some(volume) => println!(
            "Hegel volume set to {} [zone {}] max={}",
            volume.min(target.max_volume as u64),
            target.zone_id,
            target.max_volume
        ),
        None => println!(
            "Hegel volume command sent [zone {}] max={}",
            target.zone_id, target.max_volume
        ),
    }
    Ok(())
}

fn parse_normalized_volume(value: &str) -> CliResult<f32> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(CliError::new("volume is required"));
    }
    let has_percent = trimmed.ends_with('%');
    let numeric = trimmed
        .trim_end_matches('%')
        .trim()
        .parse::<f32>()
        .map_err(|_| {
            CliError::new("volume must be a number, a percent like 35%, or a fraction like 0.35")
        })?;
    if !numeric.is_finite() {
        return Err(CliError::new("volume must be finite"));
    }
    if numeric < 0.0 {
        return Err(CliError::new("volume must be at least 0"));
    }
    let normalized = if has_percent || numeric > 1.0 {
        if numeric > 100.0 {
            return Err(CliError::new("volume percent must be at most 100"));
        }
        numeric / 100.0
    } else {
        numeric
    };
    Ok(normalized.clamp(0.0, 1.0))
}

fn parse_hegel_volume(value: &str) -> CliResult<u8> {
    let trimmed = value.trim().trim_end_matches('%').trim();
    if trimmed.is_empty() {
        return Err(CliError::new("Hegel volume is required"));
    }
    let volume = trimmed
        .parse::<u8>()
        .map_err(|_| CliError::new("Hegel volume must be an integer from 0 to 100"))?;
    if volume > 100 {
        return Err(CliError::new("Hegel volume must be at most 100"));
    }
    Ok(volume)
}

async fn resolve_zone_target(
    client: &mut CoreClient,
    target: &ZoneTargetArgs,
) -> CliResult<Option<ResolvedZone>> {
    if !zone_target_requested(target) {
        return Ok(None);
    }
    let zones = client.get_json("/api/zones", &[]).await?;
    resolve_zone_from_list(&zones, target).map(Some)
}

fn zone_target_requested(target: &ZoneTargetArgs) -> bool {
    target
        .zone
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
        || target
            .zone_id
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
}

fn resolve_zone_from_list(zones: &Value, target: &ZoneTargetArgs) -> CliResult<ResolvedZone> {
    let zone = target
        .zone
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let zone_id = target
        .zone_id
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty());
    if zone.is_some() && zone_id.is_some() {
        return Err(CliError::new("use either --zone or --zone-id, not both"));
    }
    let requested = zone
        .or(zone_id)
        .ok_or_else(|| CliError::new("zone name or id is required"))?;
    let zones = zones
        .as_array()
        .ok_or_else(|| CliError::new("/api/zones did not return an array"))?;

    if zone_id.is_some() {
        return zones
            .iter()
            .find(|candidate| zone_id_value(candidate).as_deref() == Some(requested))
            .map(resolved_zone_from_value)
            .transpose()?
            .ok_or_else(|| {
                CliError::new(format!(
                    "zone id '{requested}' not found; available zones: {}",
                    available_zone_list(zones)
                ))
            });
    }

    if let Some(candidate) = zones
        .iter()
        .find(|candidate| zone_id_value(candidate).as_deref() == Some(requested))
    {
        return resolved_zone_from_value(candidate);
    }

    let exact_name_matches = zones
        .iter()
        .filter(|candidate| {
            zone_name_value(candidate)
                .as_deref()
                .is_some_and(|name| name.eq_ignore_ascii_case(requested))
        })
        .collect::<Vec<_>>();
    if exact_name_matches.len() == 1 {
        return resolved_zone_from_value(exact_name_matches[0]);
    }
    if exact_name_matches.len() > 1 {
        return Err(CliError::new(format!(
            "zone name '{requested}' is ambiguous; matching zones: {}",
            zone_candidates(&exact_name_matches)
        )));
    }

    let requested_lower = requested.to_ascii_lowercase();
    let substring_matches = zones
        .iter()
        .filter(|candidate| {
            zone_name_value(candidate)
                .map(|name| name.to_ascii_lowercase().contains(&requested_lower))
                .unwrap_or(false)
        })
        .collect::<Vec<_>>();
    match substring_matches.as_slice() {
        [candidate] => resolved_zone_from_value(candidate),
        [] => Err(CliError::new(format!(
            "zone '{requested}' not found; available zones: {}",
            available_zone_list(zones)
        ))),
        matches => Err(CliError::new(format!(
            "zone '{requested}' is ambiguous; matching zones: {}",
            zone_candidates(matches)
        ))),
    }
}

fn resolve_zone_label_from_list(zones: &Value, requested: &str) -> CliResult<ResolvedZone> {
    resolve_zone_from_list(
        zones,
        &ZoneTargetArgs {
            zone: Some(requested.to_string()),
            zone_id: None,
        },
    )
}

fn resolved_zone_from_value(zone: &Value) -> CliResult<ResolvedZone> {
    let id = zone_id_value(zone).ok_or_else(|| CliError::new("matching zone is missing id"))?;
    let name = zone_name_value(zone).unwrap_or_else(|| id.clone());
    Ok(ResolvedZone { id, name })
}

fn zone_id_value(zone: &Value) -> Option<String> {
    zone.get("id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|id| !id.trim().is_empty())
}

fn zone_name_value(zone: &Value) -> Option<String> {
    zone.get("name")
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|name| !name.trim().is_empty())
}

fn available_zone_list(zones: &[Value]) -> String {
    let zones = zones.iter().collect::<Vec<_>>();
    zone_candidates(&zones)
}

fn zone_candidates(zones: &[&Value]) -> String {
    zones
        .iter()
        .filter_map(|zone| {
            let id = zone_id_value(zone)?;
            let name = zone_name_value(zone).unwrap_or_else(|| id.clone());
            Some(format!("{name} ({id})"))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn status_path(zone: Option<&ResolvedZone>) -> String {
    zone.map(|zone| zone_api_path(zone, "status"))
        .unwrap_or_else(|| "/api/status".to_string())
}

fn play_path(zone: Option<&ResolvedZone>) -> String {
    zone.map(|zone| zone_api_path(zone, "play"))
        .unwrap_or_else(|| "/api/play".to_string())
}

fn qobuz_play_path(zone: Option<&ResolvedZone>) -> String {
    zone.map(|zone| zone_api_path(zone, "qobuz/play"))
        .unwrap_or_else(|| "/api/qobuz/play".to_string())
}

fn queue_path(zone: Option<&ResolvedZone>) -> String {
    zone.map(|zone| zone_api_path(zone, "queue"))
        .unwrap_or_else(|| "/api/queue".to_string())
}

fn now_playing_queue_path(zone: Option<&ResolvedZone>) -> String {
    zone.map(|zone| zone_api_path(zone, "now-playing-queue"))
        .unwrap_or_else(|| "/api/now-playing-queue".to_string())
}

fn control_path(zone: Option<&ResolvedZone>, command: &str) -> String {
    zone.map(|zone| zone_api_path(zone, command))
        .unwrap_or_else(|| format!("/api/{command}"))
}

fn transfer_path(source: &ResolvedZone) -> String {
    zone_api_path(source, "transfer")
}

fn zone_api_path(zone: &ResolvedZone, suffix: &str) -> String {
    format!("/api/zones/{}/{}", zone.id, suffix.trim_start_matches('/'))
}

async fn run_track_search(client: &mut CoreClient, args: TrackSearchArgs) -> CliResult<()> {
    let limit = validated_optional_limit(args.limit, "track-search --limit")?;
    let local = client
        .get_json("/api/library/search", &[("q", args.query.as_str())])
        .await?;
    let mut warnings = Vec::new();
    let qobuz = match client
        .get_json("/api/qobuz/search", &[("q", args.query.as_str())])
        .await
    {
        Ok(search) => Some(search),
        Err(error) => {
            warnings.push(format!("qobuz search failed: {error}"));
            None
        }
    };
    let mut response = build_track_search_response(&args.query, &local, qobuz.as_ref(), warnings);
    let ranked = args.ranked || args.best;
    let limit = if args.best { Some(1) } else { limit };
    if ranked || limit.is_some() {
        apply_track_search_options(&mut response, &args.query, ranked, limit);
    }
    print_json_or_track_search(&response, args.json)
}

async fn run_history_top(client: &mut CoreClient, args: HistoryTopArgs) -> CliResult<()> {
    let profile_id = match args.profile.as_deref() {
        Some(profile) => Some(resolve_history_profile_id(client, profile).await?),
        None => None,
    };
    let limit = args.limit.to_string();
    let exclude_radio = if args.exclude_radio {
        Some("true")
    } else {
        None
    };
    let mut query = vec![
        ("kind", "songs"),
        ("range", args.range.as_str()),
        ("limit", limit.as_str()),
    ];
    if let Some(profile_id) = profile_id.as_deref() {
        query.push(("profile_id", profile_id));
    }
    if let Some(exclude_radio) = exclude_radio {
        query.push(("exclude_radio", exclude_radio));
    }
    let top = client.get_json("/api/history/top", &query).await?;
    print_json_or_history_top(&top, args.json)
}

async fn run_playlist_list(client: &mut CoreClient, args: JsonArgs) -> CliResult<()> {
    let playlists = client.get_json("/api/playlists", &[]).await?;
    print_json_or_playlist_list(&playlists, args.json)
}

async fn run_playlist_show(client: &mut CoreClient, args: PlaylistShowArgs) -> CliResult<()> {
    let id = clean_playlist_id_input(&args.playlist_id)?;
    let playlists = client.get_json("/api/playlists", &[]).await?;
    let playlist = playlist_by_id(&playlists, &id)?;
    print_json_or_playlist_detail(&playlist, args.json)
}

async fn run_playlist_create(client: &mut CoreClient, args: PlaylistCreateArgs) -> CliResult<()> {
    let id = playlist_id(args.id)?;
    let name = clean_playlist_name(&args.name)?;
    let now = now_millis();
    let items = playlist_items_for_sources(client, args.sources).await?;
    let saved = save_playlist(client, &id, playlist_save_body(name, now, now, items)).await?;
    if args.json {
        print_json(&saved)
    } else {
        print_playlist_save_confirmation(&saved)
    }
}

async fn run_playlist_add(client: &mut CoreClient, args: PlaylistAddArgs) -> CliResult<()> {
    let id = clean_playlist_id_input(&args.playlist_id)?;
    let playlists = client.get_json("/api/playlists", &[]).await?;
    let playlist = playlist_by_id(&playlists, &id)?;
    let name = playlist_name(&playlist)
        .ok_or_else(|| CliError::new(format!("playlist '{id}' is missing name")))?;
    let created_at = playlist_created_at(&playlist).unwrap_or_else(now_millis);
    let now = now_millis();
    let additions = playlist_items_for_sources(client, args.sources).await?;
    let items = appended_playlist_items(&playlist, additions);
    let saved = save_playlist(
        client,
        &id,
        playlist_save_body(name, created_at, now, items),
    )
    .await?;
    if args.json {
        print_json(&saved)
    } else {
        print_playlist_save_confirmation(&saved)
    }
}

async fn resolve_history_profile_id(client: &mut CoreClient, profile: &str) -> CliResult<String> {
    let profiles = client.get_json("/api/profiles", &[]).await?;
    history_profile_id_from_profiles(&profiles, profile)
}

async fn playlist_items_for_sources(
    client: &mut CoreClient,
    sources: Vec<String>,
) -> CliResult<Vec<Value>> {
    let specs = sources
        .iter()
        .map(|source| parse_source_spec(source))
        .collect::<CliResult<Vec<_>>>()?;
    let mut items = Vec::with_capacity(specs.len());
    for spec in specs {
        let request_item = queue_request_item_for_source_spec(client, spec.clone()).await?;
        let source = match spec {
            SourceSpec::Local(_) => {
                let track_id = request_item
                    .get("track_id")
                    .and_then(Value::as_i64)
                    .ok_or_else(|| CliError::new("local playlist source is missing track_id"))?;
                local_source_ref_for_target(client, &LocalTarget::TrackId(track_id)).await?
            }
            SourceSpec::Qobuz(_) => request_item,
        };
        items.push(source_ref_queue_item(&source)?);
    }
    Ok(items)
}

async fn save_playlist(client: &mut CoreClient, id: &str, body: Value) -> CliResult<Value> {
    client
        .put_json(&format!("/api/playlists/{}", path_segment(id)), body)
        .await
}

fn playlist_save_body(name: String, created_at: i64, updated_at: i64, items: Vec<Value>) -> Value {
    json!({
        "name": name,
        "createdAt": created_at,
        "updatedAt": updated_at,
        "items": items,
    })
}

fn playlist_id(id: Option<String>) -> CliResult<String> {
    match id {
        Some(id) => clean_playlist_id_input(&id),
        None => Ok(generate_playlist_id()),
    }
}

fn generate_playlist_id() -> String {
    let suffix: String = rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(8)
        .map(char::from)
        .collect::<String>()
        .to_ascii_lowercase();
    format!("playlist-{}-{suffix}", base36(now_millis() as u128))
}

fn clean_playlist_id_input(id: &str) -> CliResult<String> {
    let id = id.trim();
    if id.is_empty() {
        Err(CliError::new("playlist id is required"))
    } else {
        Ok(id.to_string())
    }
}

fn clean_playlist_name(name: &str) -> CliResult<String> {
    let name = name.trim();
    if name.is_empty() {
        Err(CliError::new("playlist name is required"))
    } else {
        Ok(name.to_string())
    }
}

fn playlist_by_id(playlists: &Value, id: &str) -> CliResult<Value> {
    let playlists = playlists
        .as_array()
        .ok_or_else(|| CliError::new("/api/playlists did not return an array"))?;
    playlists
        .iter()
        .find(|playlist| playlist.get("id").and_then(Value::as_str) == Some(id))
        .cloned()
        .ok_or_else(|| CliError::new(playlist_not_found_message(playlists, id)))
}

fn playlist_not_found_message(playlists: &[Value], id: &str) -> String {
    let available = playlists
        .iter()
        .filter_map(|playlist| {
            let playlist_id = playlist.get("id").and_then(Value::as_str)?;
            let name = playlist.get("name").and_then(Value::as_str).unwrap_or("");
            if name.is_empty() {
                Some(playlist_id.to_string())
            } else {
                Some(format!("{name} ({playlist_id})"))
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    if available.is_empty() {
        format!("playlist '{id}' not found; no playlists exist")
    } else {
        format!("playlist '{id}' not found; available playlists: {available}")
    }
}

fn playlist_name(playlist: &Value) -> Option<String> {
    playlist
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
}

fn playlist_created_at(playlist: &Value) -> Option<i64> {
    numeric_i64_field(playlist, "createdAt").or_else(|| numeric_i64_field(playlist, "created_at"))
}

fn appended_playlist_items(playlist: &Value, additions: Vec<Value>) -> Vec<Value> {
    let mut items = playlist_items(playlist);
    items.extend(additions);
    items
}

fn playlist_items(playlist: &Value) -> Vec<Value> {
    playlist
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn playlist_item_count(playlist: &Value) -> usize {
    playlist
        .get("items")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0)
}

fn numeric_i64_field(value: &Value, key: &str) -> Option<i64> {
    value.get(key).and_then(|value| {
        value
            .as_i64()
            .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
            .or_else(|| value.as_str().and_then(|value| value.parse::<i64>().ok()))
    })
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

fn base36(mut value: u128) -> String {
    if value == 0 {
        return "0".to_string();
    }
    let alphabet = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut digits = Vec::new();
    while value > 0 {
        digits.push(alphabet[(value % 36) as usize] as char);
        value /= 36;
    }
    digits.into_iter().rev().collect()
}

fn path_segment(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

async fn play_target(
    client: &mut CoreClient,
    zone: Option<&ResolvedZone>,
    target: PlayTarget,
    queue_sources: Vec<String>,
    as_json: bool,
) -> CliResult<()> {
    let prepared_queue = prepare_queue_sources(client, queue_sources).await?;
    let queue_items = prepared_queue
        .iter()
        .map(|item| item.queue_item.clone())
        .collect::<Vec<_>>();
    let queue_source_refs = prepared_queue
        .iter()
        .map(|item| item.source_ref.clone())
        .collect::<Vec<_>>();
    match target {
        PlayTarget::Local(target) => {
            let source = local_source_ref_for_target(client, &target).await?;
            client
                .post_unit(
                    &play_path(zone),
                    local_play_body(target.clone(), queue_items.clone()),
                )
                .await?;
            replace_backend_queue(client, zone, queue_items, source_ref_key(&source)).await?;
            save_now_playing_state(client, zone, Some(source), queue_source_refs).await?;
            print_mutation_confirmation(client, zone, as_json).await
        }
        PlayTarget::Qobuz(track_id) => {
            play_qobuz_track(
                client,
                zone,
                track_id,
                queue_items,
                queue_source_refs,
                as_json,
            )
            .await
        }
    }
}

async fn play_qobuz_track(
    client: &mut CoreClient,
    zone: Option<&ResolvedZone>,
    track_id: u64,
    queue_items: Vec<Value>,
    queue_source_refs: Vec<Value>,
    as_json: bool,
) -> CliResult<()> {
    let track = qobuz_track_detail(client, track_id).await?;
    let source = qobuz_source_ref(&track)?;
    let body = qobuz_play_body(&track, &queue_source_refs)?;
    client.post_unit(&qobuz_play_path(zone), body).await?;
    replace_backend_queue(client, zone, queue_items, source_ref_key(&source)).await?;
    save_now_playing_state(client, zone, Some(source), queue_source_refs).await?;
    print_mutation_confirmation(client, zone, as_json).await
}

async fn append_queue_target(
    client: &mut CoreClient,
    zone: Option<&ResolvedZone>,
    target: PlayTarget,
    as_json: bool,
) -> CliResult<()> {
    let snapshot = client.get_json(&now_playing_queue_path(zone), &[]).await?;
    let item = match target {
        PlayTarget::Local(LocalTarget::TrackId(track_id)) => {
            json!({ "track_id": validate_local_track_id(track_id)? })
        }
        PlayTarget::Local(LocalTarget::FileName(file_name)) => {
            let file_name = validate_file_name(file_name)?;
            json!({ "file_name": file_name })
        }
        PlayTarget::Qobuz(track_id) => {
            let track = qobuz_track_detail(client, track_id).await?;
            qobuz_source_ref(&track)?
        }
    };
    let body = append_queue_body(&snapshot, item);
    client.post_unit(&queue_path(zone), body).await?;
    synchronize_now_playing_state(client, zone).await?;
    print_mutation_confirmation(client, zone, as_json).await
}

async fn append_queue_sources(
    client: &mut CoreClient,
    zone: Option<&ResolvedZone>,
    sources: Vec<String>,
    as_json: bool,
) -> CliResult<()> {
    let specs = sources
        .iter()
        .map(|source| parse_source_spec(source))
        .collect::<CliResult<Vec<_>>>()?;
    let mut items = Vec::with_capacity(specs.len());
    for spec in specs {
        items.push(queue_request_item_for_source_spec(client, spec).await?);
    }
    let snapshot = client.get_json(&now_playing_queue_path(zone), &[]).await?;
    let body = append_queue_items_body(&snapshot, items);
    client.post_unit(&queue_path(zone), body).await?;
    synchronize_now_playing_state(client, zone).await?;
    print_mutation_confirmation(client, zone, as_json).await
}

async fn queue_request_item_for_source_spec(
    client: &mut CoreClient,
    spec: SourceSpec,
) -> CliResult<Value> {
    match spec {
        SourceSpec::Local(track_id) => Ok(json!({ "track_id": track_id })),
        SourceSpec::Qobuz(track_id) => {
            let track = qobuz_track_detail(client, track_id).await?;
            qobuz_source_ref(&track)
        }
    }
}

struct PreparedQueueSource {
    queue_item: Value,
    source_ref: Value,
}

async fn prepare_queue_sources(
    client: &mut CoreClient,
    sources: Vec<String>,
) -> CliResult<Vec<PreparedQueueSource>> {
    let specs = sources
        .iter()
        .map(|source| parse_source_spec(source))
        .collect::<CliResult<Vec<_>>>()?;
    let mut prepared = Vec::with_capacity(specs.len());
    for spec in specs {
        prepared.push(prepare_queue_source(client, spec).await?);
    }
    Ok(prepared)
}

async fn prepare_queue_source(
    client: &mut CoreClient,
    spec: SourceSpec,
) -> CliResult<PreparedQueueSource> {
    match spec {
        SourceSpec::Local(track_id) => {
            let source_ref =
                local_source_ref_for_target(client, &LocalTarget::TrackId(track_id)).await?;
            Ok(PreparedQueueSource {
                queue_item: source_ref.clone(),
                source_ref,
            })
        }
        SourceSpec::Qobuz(track_id) => {
            let track = qobuz_track_detail(client, track_id).await?;
            let source_ref = qobuz_source_ref(&track)?;
            Ok(PreparedQueueSource {
                queue_item: source_ref.clone(),
                source_ref,
            })
        }
    }
}

async fn replace_backend_queue(
    client: &mut CoreClient,
    zone: Option<&ResolvedZone>,
    queue: Vec<Value>,
    expected_current: Option<String>,
) -> CliResult<()> {
    client
        .post_unit(
            &queue_path(zone),
            json!({
                "queue": queue,
                "expected_current": expected_current,
            }),
        )
        .await
}

async fn synchronize_now_playing_state(
    client: &mut CoreClient,
    zone: Option<&ResolvedZone>,
) -> CliResult<()> {
    let snapshot = client.get_json(&now_playing_queue_path(zone), &[]).await?;
    let current = snapshot
        .get("current_source")
        .cloned()
        .filter(|v| !v.is_null())
        .or_else(|| saved_current_source(&snapshot));
    let current_key = current.as_ref().and_then(source_ref_key);
    let queued = snapshot
        .get("queued_sources")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|source| {
            let Some(current_key) = current_key.as_deref() else {
                return true;
            };
            source_ref_key(source).as_deref() != Some(current_key)
        })
        .collect();
    save_now_playing_state(client, zone, current, queued).await
}

fn saved_current_source(snapshot: &Value) -> Option<Value> {
    let state = snapshot.get("state")?;
    let cursor = state.get("cursor")?.as_i64()?;
    if cursor < 0 {
        return None;
    }
    let item = state.get("items")?.as_array()?.get(cursor as usize)?;
    queue_item_source_ref(item)
}

async fn save_now_playing_state(
    client: &mut CoreClient,
    zone: Option<&ResolvedZone>,
    current: Option<Value>,
    queued: Vec<Value>,
) -> CliResult<()> {
    let mut sources = Vec::with_capacity(queued.len() + usize::from(current.is_some()));
    if let Some(current) = current {
        sources.push(current);
    }
    sources.extend(queued);
    let items = sources
        .iter()
        .map(source_ref_queue_item)
        .collect::<CliResult<Vec<_>>>()?;
    let state = json!({
        "kind": queue_kind_for_items(&items),
        "cursor": if items.is_empty() { -1 } else { 0 },
        "items": items,
        "loopMode": "off",
    });
    client
        .post_unit(&now_playing_queue_path(zone), json!({ "state": state }))
        .await
}

async fn qobuz_track_detail(client: &mut CoreClient, track_id: u64) -> CliResult<Value> {
    if track_id == 0 {
        return Err(CliError::new("qobuz track id must be positive"));
    }
    client
        .get_json(&format!("/api/qobuz/tracks/{track_id}"), &[])
        .await
}

fn append_queue_body(snapshot: &Value, item: Value) -> Value {
    append_queue_items_body(snapshot, vec![item])
}

fn append_queue_items_body(snapshot: &Value, items: Vec<Value>) -> Value {
    let mut queue = snapshot
        .get("queued_sources")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    queue.extend(items);
    json!({
        "queue": queue,
        "expected_current": expected_current_from_snapshot(snapshot),
    })
}

fn expected_current_from_snapshot(snapshot: &Value) -> Option<String> {
    snapshot
        .get("current_source")
        .and_then(source_ref_key)
        .or_else(|| {
            let state = snapshot.get("state")?;
            let cursor = state.get("cursor")?.as_i64()?;
            if cursor < 0 {
                return None;
            }
            let item = state.get("items")?.as_array()?.get(cursor as usize)?;
            queue_item_source_key(item)
        })
}

fn local_play_body(target: LocalTarget, queue: Vec<Value>) -> Value {
    match target {
        LocalTarget::TrackId(track_id) => json!({ "track_id": track_id, "queue": queue }),
        LocalTarget::FileName(file_name) => json!({ "file_name": file_name, "queue": queue }),
    }
}

fn qobuz_play_body(track: &Value, queue_sources: &[Value]) -> CliResult<Value> {
    let source = qobuz_source_ref(track)?;
    Ok(json!({
        "track_id": source["track_id"],
        "title": source.get("title").cloned().unwrap_or(Value::Null),
        "artist": source.get("artist").cloned().unwrap_or(Value::Null),
        "album": source.get("album").cloned().unwrap_or(Value::Null),
        "album_id": source.get("album_id").cloned().unwrap_or(Value::Null),
        "image_url": source.get("image_url").cloned().unwrap_or(Value::Null),
        "duration_secs": source.get("duration_secs").cloned().unwrap_or(Value::Null),
        "replace_current": true,
        "radio_auto": false,
        "queue": qobuz_queue_tracks_from_sources(queue_sources),
    }))
}

fn qobuz_queue_tracks_from_sources(queue_sources: &[Value]) -> Vec<Value> {
    queue_sources
        .iter()
        .filter(|source| {
            matches!(
                source.get("kind").and_then(Value::as_str),
                Some("qobuz_track" | "qobuz")
            )
        })
        .filter_map(|source| {
            let track_id = source
                .get("track_id")
                .or_else(|| source.get("id"))
                .and_then(Value::as_u64)
                .filter(|id| *id > 0)?;
            Some(json!({
                "track_id": track_id,
                "title": source.get("title").cloned().unwrap_or(Value::Null),
                "artist": source.get("artist").cloned().unwrap_or(Value::Null),
                "album": source.get("album").cloned().unwrap_or(Value::Null),
                "album_id": source.get("album_id").cloned().unwrap_or(Value::Null),
                "image_url": source.get("image_url").cloned().unwrap_or(Value::Null),
                "duration_secs": source.get("duration_secs").cloned().unwrap_or(Value::Null),
                "format_id": Value::Null,
                "radio": source.get("radio").and_then(Value::as_bool).unwrap_or(false),
            }))
        })
        .collect()
}

async fn local_source_ref_for_target(
    client: &mut CoreClient,
    target: &LocalTarget,
) -> CliResult<Value> {
    let tracks = client.get_json("/api/library/tracks", &[]).await?;
    let tracks = tracks
        .as_array()
        .ok_or_else(|| CliError::new("/api/library/tracks did not return an array"))?;
    let track = match target {
        LocalTarget::TrackId(track_id) => tracks.iter().find(|track| {
            track
                .get("id")
                .or_else(|| track.get("track_id"))
                .and_then(Value::as_i64)
                == Some(*track_id)
        }),
        LocalTarget::FileName(file_name) => tracks.iter().find(|track| {
            track
                .get("file_name")
                .and_then(Value::as_str)
                .is_some_and(|candidate| candidate == file_name)
        }),
    }
    .ok_or_else(|| match target {
        LocalTarget::TrackId(track_id) => {
            CliError::new(format!("local track {track_id} not found"))
        }
        LocalTarget::FileName(file_name) => {
            CliError::new(format!("local file_name '{file_name}' not found"))
        }
    })?;
    local_source_ref(track)
}

fn local_source_ref(track: &Value) -> CliResult<Value> {
    let id = track
        .get("id")
        .or_else(|| track.get("track_id"))
        .and_then(Value::as_i64)
        .filter(|id| *id > 0)
        .ok_or_else(|| CliError::new("local track is missing id"))?;
    Ok(json!({
        "kind": "local_track",
        "track_id": id,
        "file_name": nullable_string(track, "file_name"),
        "title": nullable_string(track, "title"),
        "artist": nullable_string(track, "artist"),
        "album": nullable_string(track, "album"),
        "album_artist": nullable_string(track, "album_artist"),
        "album_id": track.get("album_id").cloned().unwrap_or(Value::Null),
        "art_id": track.get("art_id").cloned().unwrap_or(Value::Null),
        "duration_secs": track.get("duration_secs").cloned().unwrap_or(Value::Null),
        "radio": false,
    }))
}

fn qobuz_source_ref(track: &Value) -> CliResult<Value> {
    let id = track
        .get("id")
        .or_else(|| track.get("track_id"))
        .and_then(Value::as_u64)
        .filter(|id| *id > 0)
        .ok_or_else(|| CliError::new("Qobuz track detail is missing id"))?;
    Ok(json!({
        "kind": "qobuz_track",
        "track_id": id,
        "title": nullable_string(track, "title"),
        "artist": nullable_string(track, "artist"),
        "album": nullable_string(track, "album"),
        "album_id": track.get("album_id").cloned().unwrap_or(Value::Null),
        "image_url": track.get("image_url").cloned().unwrap_or(Value::Null),
        "duration_secs": qobuz_duration_secs(track).map(Value::from).unwrap_or(Value::Null),
        "radio": false,
    }))
}

fn source_ref_queue_item(source: &Value) -> CliResult<Value> {
    let kind = source
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| CliError::new("source ref is missing kind"))?;
    match kind {
        "local_track" | "local" => local_source_ref_queue_item(source),
        "qobuz_track" | "qobuz" => qobuz_source_ref_queue_item(source),
        _ => Err(CliError::new(format!("unsupported source kind '{kind}'"))),
    }
}

fn local_source_ref_queue_item(source: &Value) -> CliResult<Value> {
    let track_id = source
        .get("track_id")
        .and_then(Value::as_i64)
        .filter(|id| *id > 0)
        .ok_or_else(|| CliError::new("local source ref is missing track_id"))?;
    let file_name = source.get("file_name").cloned().unwrap_or(Value::Null);
    Ok(json!({
        "title": source.get("title").cloned().unwrap_or_else(|| json!(format!("Track {track_id}"))),
        "artist": source.get("artist").cloned().unwrap_or(Value::String(String::new())),
        "album": source.get("album").cloned().unwrap_or(Value::String(String::new())),
        "albumArtist": source
            .get("album_artist")
            .or_else(|| source.get("artist"))
            .cloned()
            .unwrap_or(Value::String(String::new())),
        "albumId": source.get("album_id").cloned().unwrap_or(Value::Null),
        "artId": source.get("art_id").cloned().unwrap_or(Value::Null),
        "imageUrl": Value::Null,
        "durationSecs": source.get("duration_secs").cloned().unwrap_or_else(|| json!(0.0)),
        "filename": file_name.clone(),
        "ref": {
            "track_id": track_id,
            "file_name": file_name,
        },
        "resolvedSource": source,
        "radio": source.get("radio").and_then(Value::as_bool).unwrap_or(false),
    }))
}

fn qobuz_source_ref_queue_item(source: &Value) -> CliResult<Value> {
    let track_id = source
        .get("track_id")
        .or_else(|| source.get("id"))
        .and_then(Value::as_u64)
        .filter(|id| *id > 0)
        .ok_or_else(|| CliError::new("qobuz source ref is missing track_id"))?;
    let qobuz_track = json!({
        "id": track_id,
        "track_id": track_id,
        "title": source.get("title").cloned().unwrap_or(Value::Null),
        "artist": source.get("artist").cloned().unwrap_or(Value::Null),
        "album": source.get("album").cloned().unwrap_or(Value::Null),
        "album_id": source.get("album_id").cloned().unwrap_or(Value::Null),
        "image_url": source.get("image_url").cloned().unwrap_or(Value::Null),
        "duration": source.get("duration_secs").cloned().unwrap_or(Value::Null),
        "duration_secs": source.get("duration_secs").cloned().unwrap_or(Value::Null),
        "radio": source.get("radio").and_then(Value::as_bool).unwrap_or(false),
    });
    let title = source
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("Untitled");
    let artist = source.get("artist").and_then(Value::as_str).unwrap_or("");
    let filename = if artist.is_empty() {
        title.to_string()
    } else {
        format!("{artist} - {title}")
    };
    Ok(json!({
        "title": source.get("title").cloned().unwrap_or(Value::String("Untitled".to_string())),
        "artist": source.get("artist").cloned().unwrap_or(Value::String(String::new())),
        "album": source.get("album").cloned().unwrap_or(Value::String(String::new())),
        "albumId": source.get("album_id").cloned().unwrap_or(Value::Null),
        "imageUrl": source.get("image_url").cloned().unwrap_or(Value::Null),
        "durationSecs": source.get("duration_secs").cloned().unwrap_or_else(|| json!(0.0)),
        "filename": filename,
        "qobuzTrack": qobuz_track,
        "resolvedSource": source,
        "radio": source.get("radio").and_then(Value::as_bool).unwrap_or(false),
    }))
}

fn queue_kind_for_items(items: &[Value]) -> Value {
    let mut has_local = false;
    let mut has_qobuz = false;
    for item in items {
        if item.get("qobuzTrack").is_some() {
            has_qobuz = true;
        }
        if item.get("ref").is_some() {
            has_local = true;
        }
    }
    match (has_local, has_qobuz) {
        (true, true) => json!("mixed"),
        (true, false) => json!("local"),
        (false, true) => json!("qobuz"),
        (false, false) => Value::Null,
    }
}

fn qobuz_duration_secs(track: &Value) -> Option<f64> {
    track
        .get("duration_secs")
        .and_then(Value::as_f64)
        .or_else(|| track.get("duration").and_then(Value::as_f64))
}

fn nullable_string(value: &Value, key: &str) -> Value {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(|s| Value::String(s.to_string()))
        .unwrap_or(Value::Null)
}

fn top_level_play_target(args: &PlayArgs) -> CliResult<PlayTarget> {
    target_from_parts(
        args.source.as_deref(),
        args.track_id,
        args.file_name.as_deref(),
    )
}

fn top_level_queue_target(args: &QueueAddArgs) -> CliResult<PlayTarget> {
    target_from_parts(
        args.source.as_deref(),
        args.track_id,
        args.file_name.as_deref(),
    )
}

fn target_from_parts(
    source: Option<&str>,
    track_id: Option<i64>,
    file_name: Option<&str>,
) -> CliResult<PlayTarget> {
    if let Some(source) = source {
        if track_id.is_some() || file_name.is_some() {
            return Err(CliError::new(
                "source specs cannot be combined with --track-id or --file-name",
            ));
        }
        return match parse_source_spec(source)? {
            SourceSpec::Local(id) => Ok(PlayTarget::Local(LocalTarget::TrackId(id))),
            SourceSpec::Qobuz(id) => Ok(PlayTarget::Qobuz(id)),
        };
    }
    match (track_id, file_name) {
        (Some(track_id), None) => Ok(PlayTarget::Local(LocalTarget::TrackId(
            validate_local_track_id(track_id)?,
        ))),
        (None, Some(file_name)) => Ok(PlayTarget::Local(LocalTarget::FileName(
            validate_file_name(file_name.to_string())?,
        ))),
        (Some(_), Some(_)) => Err(CliError::new(
            "use either --track-id or --file-name, not both",
        )),
        (None, None) => Err(CliError::new(
            "missing source: use --track-id, --file-name, local:<id>, or qobuz:<id>",
        )),
    }
}

fn parse_source_spec(source: &str) -> CliResult<SourceSpec> {
    let (kind, id) = source
        .split_once(':')
        .ok_or_else(|| CliError::new("source specs must look like local:<id> or qobuz:<id>"))?;
    let kind = kind.trim();
    let id = id.trim();
    if id.is_empty() {
        return Err(CliError::new("source spec id is required"));
    }
    match kind {
        "local" => Ok(SourceSpec::Local(validate_local_track_id(
            id.parse()
                .map_err(|_| CliError::new("local source id must be a positive integer"))?,
        )?)),
        "qobuz" => {
            let id = id
                .parse::<u64>()
                .map_err(|_| CliError::new("qobuz source id must be a positive integer"))?;
            if id == 0 {
                return Err(CliError::new("qobuz source id must be positive"));
            }
            Ok(SourceSpec::Qobuz(id))
        }
        _ => Err(CliError::new(
            "unknown source namespace; use local:<id> or qobuz:<id>",
        )),
    }
}

fn validate_local_track_id(track_id: i64) -> CliResult<i64> {
    if track_id <= 0 {
        Err(CliError::new("local track id must be positive"))
    } else {
        Ok(track_id)
    }
}

fn validate_file_name(file_name: String) -> CliResult<String> {
    let trimmed = file_name.trim();
    if trimmed.is_empty() {
        Err(CliError::new("file name is required"))
    } else {
        Ok(trimmed.to_string())
    }
}

fn validated_optional_limit(limit: Option<usize>, label: &str) -> CliResult<Option<usize>> {
    match limit {
        Some(0) => Err(CliError::new(format!("{label} must be greater than zero"))),
        _ => Ok(limit),
    }
}

fn source_ref_key(source: &Value) -> Option<String> {
    let kind = source.get("kind").and_then(Value::as_str)?;
    match kind {
        "local_track" | "local" => source
            .get("track_id")
            .and_then(Value::as_i64)
            .map(|id| format!("local:{id}")),
        "qobuz_track" | "qobuz" => source
            .get("track_id")
            .or_else(|| source.get("id"))
            .and_then(Value::as_u64)
            .map(|id| format!("qobuz:{id}")),
        _ => None,
    }
}

fn queue_item_source_key(item: &Value) -> Option<String> {
    item.get("resolvedSource")
        .and_then(source_ref_key)
        .or_else(|| {
            item.get("qobuzTrack")
                .and_then(|track| {
                    track
                        .get("track_id")
                        .or_else(|| track.get("id"))
                        .and_then(Value::as_u64)
                })
                .map(|track_id| format!("qobuz:{track_id}"))
        })
        .or_else(|| {
            item.get("ref")
                .and_then(|ref_value| ref_value.get("track_id"))
                .and_then(Value::as_i64)
                .map(|track_id| format!("local:{track_id}"))
        })
}

fn queue_item_source_ref(item: &Value) -> Option<Value> {
    item.get("resolvedSource")
        .cloned()
        .or_else(|| {
            let track = item.get("qobuzTrack")?;
            let track_id = track
                .get("track_id")
                .or_else(|| track.get("id"))
                .and_then(Value::as_u64)?;
            Some(json!({
                "kind": "qobuz_track",
                "track_id": track_id,
                "title": track.get("title").cloned().unwrap_or(Value::Null),
                "artist": track.get("artist").cloned().unwrap_or(Value::Null),
                "album": track.get("album").cloned().unwrap_or(Value::Null),
                "album_id": track.get("album_id").cloned().unwrap_or(Value::Null),
                "image_url": track.get("image_url").cloned().unwrap_or(Value::Null),
                "duration_secs": track
                    .get("duration_secs")
                    .or_else(|| track.get("duration"))
                    .cloned()
                    .unwrap_or(Value::Null),
                "radio": track.get("radio").and_then(Value::as_bool).unwrap_or(false),
            }))
        })
        .or_else(|| {
            let reference = item.get("ref")?;
            let track_id = reference.get("track_id").and_then(Value::as_i64)?;
            Some(json!({
                "kind": "local_track",
                "track_id": track_id,
                "file_name": reference
                    .get("file_name")
                    .cloned()
                    .or_else(|| item.get("filename").cloned())
                    .unwrap_or(Value::Null),
                "title": item.get("title").cloned().unwrap_or(Value::Null),
                "artist": item.get("artist").cloned().unwrap_or(Value::Null),
                "album": item.get("album").cloned().unwrap_or(Value::Null),
                "album_artist": item.get("albumArtist").cloned().unwrap_or(Value::Null),
                "album_id": item.get("albumId").cloned().unwrap_or(Value::Null),
                "art_id": item.get("artId").cloned().unwrap_or(Value::Null),
                "duration_secs": item.get("durationSecs").cloned().unwrap_or(Value::Null),
                "radio": item.get("radio").and_then(Value::as_bool).unwrap_or(false),
            }))
        })
}

async fn print_mutation_confirmation(
    client: &mut CoreClient,
    zone: Option<&ResolvedZone>,
    as_json: bool,
) -> CliResult<()> {
    let status = client.get_json(&status_path(zone), &[]).await?;
    let queue = client
        .get_json(&now_playing_queue_path(zone), &[])
        .await
        .unwrap_or_else(|_| json!({ "queued_sources": [] }));
    let confirmation = mutation_confirmation(&status, &queue, zone);
    if as_json {
        return print_json(&confirmation);
    }
    let state = confirmation
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or("Unknown");
    let zone_name = confirmation
        .get("zone_name")
        .and_then(Value::as_str)
        .unwrap_or("Unknown zone");
    let zone_id = confirmation
        .get("zone_id")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let title = confirmation
        .get("track_title")
        .and_then(Value::as_str)
        .unwrap_or("No track");
    let artist = confirmation
        .get("track_artist")
        .and_then(Value::as_str)
        .unwrap_or("");
    let track = if artist.trim().is_empty() {
        title.to_string()
    } else {
        format!("{artist} - {title}")
    };
    let queued_count = confirmation
        .get("queued_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    println!("{state}: {track} [{zone_name}/{zone_id}] queued={queued_count}");
    Ok(())
}

fn mutation_confirmation(status: &Value, queue: &Value, zone: Option<&ResolvedZone>) -> Value {
    let current_source_key = status
        .get("current_source")
        .and_then(source_ref_key)
        .or_else(|| queue.get("current_source").and_then(source_ref_key));
    let queued_count = queue
        .get("queued_sources")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    json!({
        "zone_id": status
            .get("active_zone_id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| zone.map(|zone| zone.id.clone()))
            .unwrap_or_else(|| "unknown".to_string()),
        "zone_name": status
            .get("active_zone_name")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| zone.map(|zone| zone.name.clone()))
            .unwrap_or_else(|| "Unknown zone".to_string()),
        "state": status.get("state").cloned().unwrap_or(Value::Null),
        "current_source_key": current_source_key.map(Value::String).unwrap_or(Value::Null),
        "track_title": status.get("track_title").cloned().unwrap_or(Value::Null),
        "track_artist": status.get("track_artist").cloned().unwrap_or(Value::Null),
        "queued_count": queued_count,
    })
}

fn print_json(value: &Value) -> CliResult<()> {
    let body = serde_json::to_string_pretty(value)
        .map_err(|e| CliError::new(format!("serialize JSON: {e}")))?;
    println!("{body}");
    Ok(())
}

fn print_json_or_status(status: &Value, as_json: bool) -> CliResult<()> {
    if as_json {
        return print_json(status);
    }
    let state = status
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or("Unknown");
    let zone = status
        .get("active_zone_name")
        .and_then(Value::as_str)
        .unwrap_or("Unknown zone");
    let title = status
        .get("track_title")
        .and_then(Value::as_str)
        .or_else(|| status.get("file_name").and_then(Value::as_str))
        .unwrap_or("No track");
    println!("{state} - {title} [{zone}]");
    Ok(())
}

#[derive(Clone, Copy)]
enum SearchSource {
    Local,
    Qobuz,
}

fn print_json_or_search(search: &Value, as_json: bool, source: SearchSource) -> CliResult<()> {
    if as_json {
        return print_json(search);
    }
    for track in tracks_array(search) {
        let id = track
            .get("id")
            .or_else(|| track.get("track_id"))
            .and_then(Value::as_i64)
            .map(|id| id.to_string())
            .or_else(|| {
                track
                    .get("id")
                    .or_else(|| track.get("track_id"))
                    .and_then(Value::as_u64)
                    .map(|id| id.to_string())
            })
            .unwrap_or_else(|| "?".to_string());
        let prefix = match source {
            SearchSource::Local => "local",
            SearchSource::Qobuz => "qobuz",
        };
        let title = track
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("Untitled");
        let artist = track.get("artist").and_then(Value::as_str).unwrap_or("");
        let album = track.get("album").and_then(Value::as_str).unwrap_or("");
        println!("{prefix}:{id}\t{artist} - {title}\t{album}");
    }
    Ok(())
}

fn tracks_array(value: &Value) -> Vec<&Value> {
    if let Some(items) = value.as_array() {
        return items.iter().collect();
    }
    value
        .get("tracks")
        .or_else(|| value.get("songs"))
        .and_then(Value::as_array)
        .map(|items| items.iter().collect())
        .unwrap_or_default()
}

const TRACK_SEARCH_LIMIT: usize = 25;
const DUPLICATE_DURATION_TOLERANCE_SECS: f64 = 3.0;

fn build_track_search_response(
    query: &str,
    local_search: &Value,
    qobuz_search: Option<&Value>,
    warnings: Vec<String>,
) -> Value {
    let mut tracks: Vec<Value> = tracks_array(local_search)
        .into_iter()
        .filter_map(local_track_search_result)
        .collect();
    let mut qobuz_tracks = Vec::new();

    if let Some(qobuz_search) = qobuz_search {
        for qobuz_track in tracks_array(qobuz_search) {
            if let Some(local_index) = tracks
                .iter()
                .position(|local_track| tracks_are_duplicates(local_track, qobuz_track))
            {
                if let Some(id) = qobuz_id_string(qobuz_track)
                    && let Some(ids) = tracks[local_index]
                        .get_mut("deduped_qobuz_ids")
                        .and_then(Value::as_array_mut)
                    && !ids.iter().any(|existing| existing.as_str() == Some(&id))
                {
                    ids.push(Value::String(id));
                }
            } else if let Some(result) = qobuz_track_search_result(qobuz_track) {
                qobuz_tracks.push(result);
            }
        }
    }

    tracks.extend(qobuz_tracks);
    tracks.truncate(TRACK_SEARCH_LIMIT);
    json!({
        "query": query,
        "tracks": tracks,
        "warnings": warnings,
    })
}

fn apply_track_search_options(
    response: &mut Value,
    query: &str,
    ranked: bool,
    limit: Option<usize>,
) {
    let Some(tracks) = response.get_mut("tracks").and_then(Value::as_array_mut) else {
        return;
    };
    if ranked {
        let context = TrackSearchRankContext::new(query);
        tracks.sort_by(|a, b| {
            track_search_rank_score(b, &context)
                .cmp(&track_search_rank_score(a, &context))
                .then_with(|| track_search_key(a).cmp(&track_search_key(b)))
        });
    }
    if let Some(limit) = limit {
        tracks.truncate(limit);
    }
}

#[derive(Debug)]
struct TrackSearchRankContext {
    query_norm: String,
    query_tokens: Vec<String>,
    wants_live: bool,
    wants_remix: bool,
    wants_demo: bool,
    wants_instrumental: bool,
    wants_edit: bool,
    wants_cover: bool,
    wants_karaoke: bool,
    wants_tribute: bool,
}

impl TrackSearchRankContext {
    fn new(query: &str) -> Self {
        let query_norm = normalized_track_text(Some(query));
        let plain = plain_search_text(Some(query));
        let query_tokens = plain
            .split_whitespace()
            .map(str::to_string)
            .collect::<Vec<_>>();
        Self {
            query_norm,
            wants_live: query_has_any(&query_tokens, &["live"]),
            wants_remix: query_has_any(&query_tokens, &["remix", "mix"]),
            wants_demo: query_has_any(&query_tokens, &["demo"]),
            wants_instrumental: query_has_any(&query_tokens, &["instrumental"]),
            wants_edit: query_has_any(&query_tokens, &["edit", "radio"]),
            wants_cover: query_has_any(&query_tokens, &["cover"]),
            wants_karaoke: query_has_any(&query_tokens, &["karaoke"]),
            wants_tribute: query_has_any(&query_tokens, &["tribute"]),
            query_tokens,
        }
    }
}

fn track_search_rank_score(track: &Value, context: &TrackSearchRankContext) -> i64 {
    let title = normalized_track_text(track.get("title").and_then(Value::as_str));
    let artist = normalized_track_text(track.get("artist").and_then(Value::as_str));
    let album = normalized_track_text(track.get("album").and_then(Value::as_str));
    let title_tokens = split_tokens(&title);
    let artist_tokens = split_tokens(&artist);
    let album_tokens = split_tokens(&album);
    let combined = [artist.as_str(), title.as_str()]
        .into_iter()
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join(" ");

    let mut score = 0i64;
    if track.get("source").and_then(Value::as_str) == Some("local") {
        score += 120;
    }
    if !context.query_norm.is_empty() && combined == context.query_norm {
        score += 700;
    }
    if !context.query_norm.is_empty() && title == context.query_norm {
        score += 500;
    }
    if contains_all_tokens(&context.query_tokens, &title_tokens) {
        score += 260;
    }
    if contains_all_tokens(&context.query_tokens, &artist_tokens) {
        score += 140;
    }
    if contains_all_tokens(&context.query_tokens, &album_tokens) {
        score += 40;
    }
    score += 20 * token_overlap(&context.query_tokens, &title_tokens) as i64;
    score += 12 * token_overlap(&context.query_tokens, &artist_tokens) as i64;
    score += 5 * token_overlap(&context.query_tokens, &album_tokens) as i64;

    let descriptor_text = track_descriptor_text(track);
    score += descriptor_score(&descriptor_text, &["live"], context.wants_live, -140, 220);
    score += descriptor_score(
        &descriptor_text,
        &["remix", "mix"],
        context.wants_remix,
        -120,
        200,
    );
    score += descriptor_score(&descriptor_text, &["demo"], context.wants_demo, -100, 80);
    score += descriptor_score(
        &descriptor_text,
        &["instrumental"],
        context.wants_instrumental,
        -90,
        80,
    );
    score += descriptor_score(
        &descriptor_text,
        &["radio edit", "edit"],
        context.wants_edit,
        -70,
        70,
    );
    score += descriptor_score(
        &descriptor_text,
        &["karaoke"],
        context.wants_karaoke,
        -250,
        120,
    );
    score += descriptor_score(
        &descriptor_text,
        &[
            "tribute",
            "made famous",
            "originally performed",
            "vitamin string quartet",
            "twinkle twinkle",
            "piano tribute",
            "lullaby",
        ],
        context.wants_tribute,
        -220,
        100,
    );
    score += descriptor_score(&descriptor_text, &["cover"], context.wants_cover, -180, 90);
    score
}

fn track_search_key(track: &Value) -> String {
    track
        .get("source_key")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string()
}

fn split_tokens(value: &str) -> Vec<String> {
    value.split_whitespace().map(str::to_string).collect()
}

fn query_has_any(tokens: &[String], needles: &[&str]) -> bool {
    needles
        .iter()
        .any(|needle| tokens.iter().any(|token| token == needle))
}

fn contains_all_tokens(haystack: &[String], needles: &[String]) -> bool {
    !needles.is_empty()
        && needles
            .iter()
            .all(|needle| haystack.iter().any(|token| token == needle))
}

fn token_overlap(left: &[String], right: &[String]) -> usize {
    right
        .iter()
        .filter(|needle| left.iter().any(|token| token == *needle))
        .count()
}

fn descriptor_score(
    text: &str,
    needles: &[&str],
    query_wants_descriptor: bool,
    penalty: i64,
    bonus: i64,
) -> i64 {
    if !text_has_any_descriptor(text, needles) {
        return 0;
    }
    if query_wants_descriptor {
        bonus
    } else {
        penalty
    }
}

fn text_has_any_descriptor(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| {
        let needle = plain_search_text(Some(needle));
        let padded_text = format!(" {text} ");
        let padded_needle = format!(" {needle} ");
        padded_text.contains(&padded_needle)
    })
}

fn track_descriptor_text(track: &Value) -> String {
    [
        track.get("title").and_then(Value::as_str),
        track.get("artist").and_then(Value::as_str),
        track.get("album").and_then(Value::as_str),
    ]
    .into_iter()
    .map(plain_search_text)
    .filter(|value| !value.is_empty())
    .collect::<Vec<_>>()
    .join(" ")
}

fn local_track_search_result(track: &Value) -> Option<Value> {
    let id = track.get("id").and_then(Value::as_i64)?;
    let artist = display_artist(track);
    let local_source_key = format!("local:{id}");
    let preferred_source = track
        .get("preferred_play_source")
        .and_then(preferred_play_source_key);
    let source_key = preferred_source
        .clone()
        .unwrap_or_else(|| local_source_key.clone());
    let source = source_key
        .split_once(':')
        .map(|(source, _)| source)
        .unwrap_or("local");
    let mut result = json!({
        "source": source,
        "source_key": source_key,
        "id": preferred_source_id(track, &source_key).unwrap_or(Value::from(id)),
        "title": optional_string(track, "title"),
        "artist": artist,
        "album": optional_string(track, "album"),
        "duration_secs": optional_f64(track, "duration_secs"),
        "sample_rate": optional_i64(track, "sample_rate"),
        "bit_depth": optional_i64(track, "bit_depth"),
        "deduped_qobuz_ids": [],
    });
    if preferred_source.is_some() && source_key != local_source_key {
        result["matched_local_source_key"] = Value::String(local_source_key);
        result["matched_local_id"] = Value::from(id);
    }
    Some(result)
}

fn preferred_play_source_key(source: &Value) -> Option<String> {
    match source.get("kind").and_then(Value::as_str)? {
        "local" => source
            .get("track_id")
            .and_then(Value::as_i64)
            .map(|id| format!("local:{id}")),
        "qobuz" => source
            .get("track_id")
            .and_then(Value::as_u64)
            .map(|id| format!("qobuz:{id}")),
        _ => None,
    }
}

fn preferred_source_id(track: &Value, source_key: &str) -> Option<Value> {
    if let Some(id) = source_key
        .strip_prefix("local:")
        .and_then(|id| id.parse::<i64>().ok())
    {
        return Some(Value::from(id));
    }
    if let Some(id) = source_key
        .strip_prefix("qobuz:")
        .and_then(|id| id.parse::<u64>().ok())
    {
        return Some(Value::from(id));
    }
    track.get("id").cloned()
}

fn qobuz_track_search_result(track: &Value) -> Option<Value> {
    let id = qobuz_id_string(track)?;
    let source_key = format!("qobuz:{id}");
    let numeric_id = id.parse::<u64>().ok();
    Some(json!({
        "source": "qobuz",
        "source_key": source_key,
        "id": numeric_id.map(Value::from).unwrap_or_else(|| Value::String(id)),
        "title": optional_string(track, "title"),
        "artist": optional_string(track, "artist"),
        "album": optional_string(track, "album"),
        "duration_secs": qobuz_duration_secs(track),
        "sample_rate": optional_f64(track, "maximum_sampling_rate")
            .or_else(|| optional_f64(track, "sample_rate")),
        "bit_depth": optional_i64(track, "maximum_bit_depth")
            .or_else(|| optional_i64(track, "bit_depth")),
        "deduped_qobuz_ids": [],
    }))
}

fn tracks_are_duplicates(local_track: &Value, qobuz_track: &Value) -> bool {
    let local_title = normalized_track_text(local_track.get("title").and_then(Value::as_str));
    let qobuz_title = normalized_track_text(qobuz_track.get("title").and_then(Value::as_str));
    if local_title.is_empty() || qobuz_title.is_empty() || local_title != qobuz_title {
        return false;
    }

    let local_artist = normalized_track_text(local_track.get("artist").and_then(Value::as_str));
    let qobuz_artist = normalized_track_text(qobuz_track.get("artist").and_then(Value::as_str));
    if local_artist.is_empty() || qobuz_artist.is_empty() || local_artist != qobuz_artist {
        return false;
    }

    let local_album = normalized_track_text(local_track.get("album").and_then(Value::as_str));
    let qobuz_album = normalized_track_text(qobuz_track.get("album").and_then(Value::as_str));
    if !local_album.is_empty() && !qobuz_album.is_empty() && local_album == qobuz_album {
        return true;
    }

    durations_close(
        local_track.get("duration_secs").and_then(Value::as_f64),
        qobuz_duration_secs(qobuz_track),
    )
}

fn durations_close(local_secs: Option<f64>, qobuz_secs: Option<f64>) -> bool {
    match (local_secs, qobuz_secs) {
        (Some(local_secs), Some(qobuz_secs)) => {
            (local_secs - qobuz_secs).abs() <= DUPLICATE_DURATION_TOLERANCE_SECS
        }
        _ => false,
    }
}

fn normalized_track_text(value: Option<&str>) -> String {
    let Some(value) = value else {
        return String::new();
    };
    let mut normalized = String::new();
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if matches!(ch, '(' | '[' | '{') {
            let closing = match ch {
                '(' => ')',
                '[' => ']',
                '{' => '}',
                _ => unreachable!(),
            };
            let mut content = String::new();
            for inner in chars.by_ref() {
                if inner == closing {
                    break;
                }
                content.push(inner);
            }
            if is_ignorable_version_suffix(&content) {
                normalized.push(' ');
            } else {
                normalized.push(' ');
                normalized.push_str(&content);
                normalized.push(' ');
            }
            continue;
        }
        if ch.is_ascii_alphanumeric() {
            normalized.push(ch.to_ascii_lowercase());
        } else {
            normalized.push(' ');
        }
    }
    normalized.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn plain_search_text(value: Option<&str>) -> String {
    let Some(value) = value else {
        return String::new();
    };
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_ignorable_version_suffix(value: &str) -> bool {
    let normalized = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>();
    let tokens: Vec<_> = normalized.split_whitespace().collect();
    tokens.iter().any(|token| {
        matches!(
            *token,
            "remaster"
                | "remastered"
                | "demo"
                | "live"
                | "mix"
                | "remix"
                | "version"
                | "alternate"
                | "edit"
        )
    })
}

fn display_artist(track: &Value) -> Value {
    track
        .get("artist")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .or_else(|| track.get("album_artist").and_then(Value::as_str))
        .map(|value| Value::String(value.to_string()))
        .unwrap_or(Value::Null)
}

fn optional_string(value: &Value, key: &str) -> Value {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(|value| Value::String(value.to_string()))
        .unwrap_or(Value::Null)
}

fn optional_i64(value: &Value, key: &str) -> Option<i64> {
    value.get(key).and_then(Value::as_i64).or_else(|| {
        value
            .get(key)
            .and_then(Value::as_u64)
            .map(|value| value as i64)
    })
}

fn optional_f64(value: &Value, key: &str) -> Option<f64> {
    value
        .get(key)
        .and_then(Value::as_f64)
        .or_else(|| {
            value
                .get(key)
                .and_then(Value::as_i64)
                .map(|value| value as f64)
        })
        .or_else(|| {
            value
                .get(key)
                .and_then(Value::as_u64)
                .map(|value| value as f64)
        })
}

fn qobuz_id_string(track: &Value) -> Option<String> {
    track
        .get("id")
        .or_else(|| track.get("track_id"))
        .and_then(|id| {
            id.as_u64()
                .map(|id| id.to_string())
                .or_else(|| id.as_i64().map(|id| id.to_string()))
                .or_else(|| id.as_str().map(str::to_string))
        })
        .filter(|id| !id.trim().is_empty())
}

fn print_json_or_track_search(search: &Value, as_json: bool) -> CliResult<()> {
    if as_json {
        return print_json(search);
    }
    for warning in search
        .get("warnings")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        eprintln!("warn {warning}");
    }
    for track in tracks_array(search) {
        let source_key = track
            .get("source_key")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let title = track
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("Untitled");
        let artist = track.get("artist").and_then(Value::as_str).unwrap_or("");
        let album = track.get("album").and_then(Value::as_str).unwrap_or("");
        println!("{source_key}\t{artist} - {title}\t{album}");
    }
    Ok(())
}

fn history_profile_id_from_profiles(profiles: &Value, profile: &str) -> CliResult<String> {
    let requested = profile.trim();
    if requested.is_empty() {
        return Err(CliError::new("profile name or id is required"));
    }
    let profiles = profiles
        .get("profiles")
        .and_then(Value::as_array)
        .ok_or_else(|| CliError::new("/api/profiles did not include a profiles array"))?;
    let name_matches = profiles
        .iter()
        .filter(|candidate| {
            candidate
                .get("name")
                .and_then(Value::as_str)
                .is_some_and(|name| name.eq_ignore_ascii_case(requested))
        })
        .collect::<Vec<_>>();
    if name_matches.len() > 1 {
        let ids = name_matches
            .iter()
            .filter_map(|profile| profile.get("id").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(CliError::new(format!(
            "profile name '{requested}' is ambiguous; matching ids: {ids}"
        )));
    }
    if let Some(profile) = name_matches.first() {
        return profile
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| CliError::new("matching profile is missing id"));
    }
    if let Some(profile) = profiles.iter().find(|candidate| {
        candidate
            .get("id")
            .and_then(Value::as_str)
            .is_some_and(|id| id == requested)
    }) {
        return profile
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| CliError::new("matching profile is missing id"));
    }
    let available = profiles
        .iter()
        .filter_map(|profile| profile.get("name").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join(", ");
    if available.is_empty() {
        Err(CliError::new(format!("profile '{requested}' not found")))
    } else {
        Err(CliError::new(format!(
            "profile '{requested}' not found; available profiles: {available}"
        )))
    }
}

fn print_json_or_history_top(top: &Value, as_json: bool) -> CliResult<()> {
    if as_json {
        return print_json(top);
    }
    let profile = top
        .get("profile")
        .and_then(|profile| profile.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("Active profile");
    let range = top.get("range").and_then(Value::as_str).unwrap_or("week");
    println!("Top songs for {profile} ({range})");
    for item in top
        .get("items")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let rank = item.get("rank").and_then(Value::as_u64).unwrap_or(0);
        let title = item
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("Untitled");
        let artist = item.get("artist").and_then(Value::as_str).unwrap_or("");
        let album = item.get("album").and_then(Value::as_str).unwrap_or("");
        let play_count = item.get("play_count").and_then(Value::as_i64).unwrap_or(0);
        let listened = format_history_minutes(
            item.get("listened_secs")
                .and_then(Value::as_f64)
                .unwrap_or(0.0),
        );
        let track = if artist.trim().is_empty() {
            title.to_string()
        } else {
            format!("{artist} - {title}")
        };
        let plays = if play_count == 1 { "play" } else { "plays" };
        if album.trim().is_empty() {
            println!("{rank}. {track} | {play_count} {plays} | {listened}");
        } else {
            println!("{rank}. {track} | {album} | {play_count} {plays} | {listened}");
        }
    }
    Ok(())
}

fn format_history_minutes(seconds: f64) -> String {
    if seconds <= 0.0 {
        return "0m".to_string();
    }
    format!("{}m", ((seconds / 60.0).round() as i64).max(1))
}

fn print_json_or_queue(queue: &Value, as_json: bool) -> CliResult<()> {
    if as_json {
        return print_json(queue);
    }
    if let Some(current) = queue.get("current_source").and_then(source_ref_key) {
        println!("current\t{current}");
    } else {
        println!("current\t-");
    }
    for source in queue
        .get("queued_sources")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let key = source_ref_key(source).unwrap_or_else(|| "unknown".to_string());
        let title = source
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("Untitled");
        let artist = source.get("artist").and_then(Value::as_str).unwrap_or("");
        println!("queued\t{key}\t{artist} - {title}");
    }
    Ok(())
}

fn print_json_or_queue_summary(
    queue: &Value,
    as_json: bool,
    limit: Option<usize>,
) -> CliResult<()> {
    let limit = validated_optional_limit(limit, "queue get --limit")?;
    let summary = queue_summary(queue, limit);
    if as_json {
        return print_json(&summary);
    }
    if let Some(current) = summary.get("current").filter(|value| !value.is_null()) {
        print_queue_summary_row(0, "current", current);
    } else {
        println!("0\tcurrent\t-\t-");
    }
    for (index, source) in summary
        .get("queued")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        print_queue_summary_row(index + 1, "queued", source);
    }
    Ok(())
}

fn print_json_or_zone_swap(
    response: &Value,
    source: &ResolvedZone,
    destination: &ResolvedZone,
    as_json: bool,
) -> CliResult<()> {
    if as_json {
        return print_json(response);
    }
    let source_key = response
        .get("source_key")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let queued_count = response
        .get("queued_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let action = response
        .get("source_action")
        .and_then(Value::as_str)
        .unwrap_or("pause");
    let action_text = if action == "keep_playing" {
        "source kept playing"
    } else {
        "source paused"
    };
    println!(
        "Swapped {source_key} from {} to {} ({queued_count} queued, {action_text})",
        source.name, destination.name
    );
    if let Some(seek_error) = response
        .get("seek_error")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    {
        println!("warn seek: {seek_error}");
    }
    if let Some(clear_error) = response
        .get("source_queue_clear_error")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    {
        println!("warn source queue: {clear_error}");
    }
    Ok(())
}

fn print_queue_summary_row(index: usize, label: &str, source: &Value) {
    let source_key = source
        .get("source_key")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let title = source
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("Untitled");
    let artist = source.get("artist").and_then(Value::as_str).unwrap_or("");
    let album = source.get("album").and_then(Value::as_str).unwrap_or("");
    let track = if artist.is_empty() {
        title.to_string()
    } else {
        format!("{artist} - {title}")
    };
    println!("{index}\t{label}\t{source_key}\t{track}\t{album}");
}

fn queue_summary(queue: &Value, limit: Option<usize>) -> Value {
    let current = queue
        .get("current_source")
        .filter(|value| !value.is_null())
        .and_then(compact_queue_source)
        .unwrap_or(Value::Null);
    let mut queued = queue
        .get("queued_sources")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(compact_queue_source)
        .collect::<Vec<_>>();
    if let Some(limit) = limit {
        queued.truncate(limit);
    }
    json!({
        "current": current,
        "queued": queued,
    })
}

fn compact_queue_source(source: &Value) -> Option<Value> {
    let source_key = source_ref_key(source)?;
    Some(json!({
        "source_key": source_key,
        "kind": source.get("kind").cloned().unwrap_or(Value::Null),
        "artist": source.get("artist").cloned().unwrap_or(Value::Null),
        "title": source.get("title").cloned().unwrap_or(Value::Null),
        "album": source.get("album").cloned().unwrap_or(Value::Null),
        "duration_secs": source.get("duration_secs").cloned().unwrap_or(Value::Null),
    }))
}

fn print_json_or_playlist_list(playlists: &Value, as_json: bool) -> CliResult<()> {
    if as_json {
        return print_json(playlists);
    }
    let playlists = playlists
        .as_array()
        .ok_or_else(|| CliError::new("/api/playlists did not return an array"))?;
    for playlist in playlists {
        let id = playlist
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let name = playlist
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("Untitled playlist");
        println!("{id}\t{name}\t{}", playlist_item_count(playlist));
    }
    Ok(())
}

fn print_json_or_playlist_detail(playlist: &Value, as_json: bool) -> CliResult<()> {
    if as_json {
        return print_json(playlist);
    }
    let id = playlist
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let name = playlist
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("Untitled playlist");
    println!(
        "playlist\t{id}\t{name}\tsongs={}",
        playlist_item_count(playlist)
    );
    println!("track\tsource\tartist - title\talbum");
    for (index, item) in playlist_items(playlist).iter().enumerate() {
        let source_key = playlist_item_source_key(item);
        let title = item
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("Untitled");
        let artist = item.get("artist").and_then(Value::as_str).unwrap_or("");
        let album = item.get("album").and_then(Value::as_str).unwrap_or("");
        let track = if artist.trim().is_empty() {
            title.to_string()
        } else {
            format!("{artist} - {title}")
        };
        println!("{}\t{source_key}\t{track}\t{album}", index + 1);
    }
    Ok(())
}

fn print_playlist_save_confirmation(playlist: &Value) -> CliResult<()> {
    let id = playlist
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| CliError::new("saved playlist response is missing id"))?;
    let name = playlist
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("Untitled playlist");
    println!(
        "Saved playlist {id} \"{name}\" songs={}",
        playlist_item_count(playlist)
    );
    Ok(())
}

fn playlist_item_source_key(item: &Value) -> String {
    queue_item_source_key(item).unwrap_or_else(|| "unknown".to_string())
}

fn print_json_or_zones(zones: &Value, as_json: bool) -> CliResult<()> {
    if as_json {
        return print_json(zones);
    }
    for zone in zones.as_array().into_iter().flatten() {
        let id = zone.get("id").and_then(Value::as_str).unwrap_or("unknown");
        let name = zone
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("Unnamed");
        let status = zone
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let enabled = zone
            .get("enabled")
            .and_then(Value::as_bool)
            .map(|value| if value { "enabled" } else { "disabled" })
            .unwrap_or("unknown");
        println!("{id}\t{name}\t{status}\t{enabled}");
    }
    Ok(())
}

fn print_json_or_upnp_diagnostics(diagnostics: &Value, as_json: bool) -> CliResult<()> {
    if as_json {
        return print_json(diagnostics);
    }
    let zone_id = diagnostics
        .get("zone_id")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let renderer = diagnostics.get("renderer").unwrap_or(&Value::Null);
    let renderer_name = renderer
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let model = renderer
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let public_base_url = diagnostics
        .get("public_base_url")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    println!("UPnP diagnostics for {zone_id}");
    println!("Renderer: {renderer_name} ({model})");
    println!("Public URL: {public_base_url}");
    if let Some(warnings) = diagnostics.get("warnings").and_then(Value::as_array) {
        for warning in warnings.iter().filter_map(Value::as_str) {
            println!("Warning: {warning}");
        }
    }
    if let Some(trace) = diagnostics
        .get("last_play_trace")
        .and_then(Value::as_object)
    {
        let asset = trace
            .get("asset_id")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let elapsed = trace
            .get("total_elapsed_ms")
            .and_then(Value::as_u64)
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        println!("Last play: asset={asset} elapsed_ms={elapsed}");
    } else {
        println!("Last play: none recorded");
    }
    Ok(())
}

fn env_non_empty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parses_local_default_commands() {
        let cli = Cli::parse_from(["fozmoctl", "search", "Nude", "--json"]);
        assert!(matches!(
            cli.command,
            Command::Search(SearchArgs {
                query,
                json: true,
            }) if query == "Nude"
        ));

        let cli = Cli::parse_from(["fozmoctl", "play", "--track-id", "123"]);
        let Command::Play(args) = cli.command else {
            panic!("expected play command");
        };
        assert_eq!(
            top_level_play_target(&args).unwrap(),
            PlayTarget::Local(LocalTarget::TrackId(123))
        );
    }

    #[test]
    fn parses_volume_commands() {
        let cli = Cli::parse_from([
            "fozmoctl", "volume", "--zone", "Hegel", "35", "--hegel", "--json",
        ]);
        assert!(matches!(
            cli.command,
            Command::Volume(VolumeArgs {
                volume: Some(volume),
                zone: ZoneTargetArgs {
                    zone: Some(zone),
                    zone_id: None,
                },
                hegel: true,
                device: false,
                direction: None,
                json: true,
            }) if volume == "35" && zone == "Hegel"
        ));

        let cli = Cli::parse_from([
            "fozmoctl",
            "volume",
            "--zone-id",
            "local:hegel",
            "--hegel",
            "--direction",
            "down",
        ]);
        assert!(matches!(
            cli.command,
            Command::Volume(VolumeArgs {
                volume: None,
                zone: ZoneTargetArgs {
                    zone: None,
                    zone_id: Some(zone_id),
                },
                hegel: true,
                direction: Some(direction),
                ..
            }) if zone_id == "local:hegel" && direction == "down"
        ));
    }

    #[test]
    fn parses_normalized_volume_inputs() {
        assert_eq!(parse_normalized_volume("0.35").unwrap(), 0.35);
        assert_eq!(parse_normalized_volume("35").unwrap(), 0.35);
        assert_eq!(parse_normalized_volume("35%").unwrap(), 0.35);
        assert!(parse_normalized_volume("101").is_err());
        assert!(parse_normalized_volume("-1").is_err());
    }

    #[test]
    fn parses_hegel_native_volume_inputs() {
        assert_eq!(parse_hegel_volume("35").unwrap(), 35);
        assert_eq!(parse_hegel_volume("35%").unwrap(), 35);
        assert!(parse_hegel_volume("35.5").is_err());
        assert!(parse_hegel_volume("101").is_err());
    }

    #[test]
    fn hegel_target_uses_saved_max_and_rejects_other_zone() {
        let settings = json!({
            "enabled": true,
            "zone_id": "zone-hegel",
            "host": "192.168.1.50",
            "port": 50001,
            "max_volume": 50
        });
        let zone = ResolvedZone {
            id: "zone-hegel".to_string(),
            name: "Hegel".to_string(),
        };
        let target = hegel_target_from_settings(&settings, Some(&zone)).unwrap();
        assert_eq!(target.max_volume, 50);
        assert_eq!(target.zone_id, "zone-hegel");

        let other_zone = ResolvedZone {
            id: "zone-kitchen".to_string(),
            name: "Kitchen".to_string(),
        };
        assert!(hegel_target_from_settings(&settings, Some(&other_zone)).is_err());
    }

    #[test]
    fn parses_track_search_command() {
        let cli = Cli::parse_from(["fozmoctl", "track-search", "Radiohead Kid A", "--json"]);
        assert!(matches!(
            cli.command,
            Command::TrackSearch(TrackSearchArgs {
                query,
                ranked: false,
                best: false,
                limit: None,
                json: true,
            }) if query == "Radiohead Kid A"
        ));

        let cli = Cli::parse_from([
            "fozmoctl",
            "track-search",
            "Radiohead Reckoner",
            "--ranked",
            "--limit",
            "5",
            "--json",
        ]);
        assert!(matches!(
            cli.command,
            Command::TrackSearch(TrackSearchArgs {
                query,
                ranked: true,
                best: false,
                limit: Some(5),
                json: true,
            }) if query == "Radiohead Reckoner"
        ));

        let cli = Cli::parse_from(["fozmoctl", "track-search", "Björk Jóga", "--best"]);
        assert!(matches!(
            cli.command,
            Command::TrackSearch(TrackSearchArgs {
                query,
                ranked: false,
                best: true,
                limit: None,
                json: false,
            }) if query == "Björk Jóga"
        ));
    }

    #[test]
    fn parses_history_top_command() {
        let cli = Cli::parse_from([
            "fozmoctl",
            "history",
            "top",
            "--profile",
            "Henry",
            "--range",
            "all",
            "--limit",
            "25",
            "--json",
        ]);
        assert!(matches!(
            cli.command,
            Command::History {
                command: HistoryCommand::Top(HistoryTopArgs {
                    range,
                    limit: 25,
                    profile: Some(profile),
                    exclude_radio: false,
                    json: true,
                })
            } if range == "all" && profile == "Henry"
        ));
    }

    #[test]
    fn parses_playlist_and_mix_commands() {
        let cli = Cli::parse_from(["fozmoctl", "playlist", "list", "--json"]);
        assert!(matches!(
            cli.command,
            Command::Playlist {
                command: PlaylistCommand::List(JsonArgs { json: true })
            }
        ));

        let cli = Cli::parse_from(["fozmoctl", "playlist", "show", "morning-mix"]);
        assert!(matches!(
            cli.command,
            Command::Playlist {
                command: PlaylistCommand::Show(PlaylistShowArgs {
                    playlist_id,
                    json: false,
                })
            } if playlist_id == "morning-mix"
        ));

        let cli = Cli::parse_from([
            "fozmoctl",
            "playlist",
            "create",
            "--name",
            "Morning Mix",
            "local:1",
            "qobuz:2",
            "--json",
        ]);
        assert!(matches!(
            cli.command,
            Command::Playlist {
                command: PlaylistCommand::Create(PlaylistCreateArgs {
                    name,
                    id: None,
                    json: true,
                    sources,
                })
            } if name == "Morning Mix" && sources == vec!["local:1", "qobuz:2"]
        ));

        let cli = Cli::parse_from([
            "fozmoctl",
            "playlist",
            "add",
            "morning-mix",
            "local:3",
            "qobuz:4",
        ]);
        assert!(matches!(
            cli.command,
            Command::Playlist {
                command: PlaylistCommand::Add(PlaylistAddArgs {
                    playlist_id,
                    json: false,
                    sources,
                })
            } if playlist_id == "morning-mix" && sources == vec!["local:3", "qobuz:4"]
        ));

        let cli = Cli::parse_from(["fozmoctl", "mix", "create", "--name", "Late Set", "local:5"]);
        assert!(matches!(
            cli.command,
            Command::Playlist {
                command: PlaylistCommand::Create(PlaylistCreateArgs {
                    name,
                    sources,
                    ..
                })
            } if name == "Late Set" && sources == vec!["local:5"]
        ));
    }

    #[test]
    fn history_profile_resolution_prefers_name_then_exact_id() {
        let profiles = json!({
            "profiles": [
                { "id": "henry-123", "name": "Henry" },
                { "id": "dad-id", "name": "Dad" }
            ],
            "active_profile_id": "dad-id"
        });

        assert_eq!(
            history_profile_id_from_profiles(&profiles, "henry").unwrap(),
            "henry-123"
        );
        assert_eq!(
            history_profile_id_from_profiles(&profiles, "henry-123").unwrap(),
            "henry-123"
        );
    }

    #[test]
    fn history_profile_resolution_reports_unknown_profile() {
        let profiles = json!({
            "profiles": [
                { "id": "henry-123", "name": "Henry" }
            ],
            "active_profile_id": "henry-123"
        });

        let error = history_profile_id_from_profiles(&profiles, "Dad")
            .expect_err("unknown profile should fail")
            .to_string();
        assert!(error.contains("profile 'Dad' not found"));
        assert!(error.contains("Henry"));
    }

    #[test]
    fn generated_playlist_ids_are_prefixed() {
        let id = generate_playlist_id();
        assert!(id.starts_with("playlist-"));
        assert!(id.len() > "playlist-".len());
    }

    #[test]
    fn playlist_append_preserves_existing_items_and_order() {
        let playlist = json!({
            "items": [
                { "title": "First", "ref": { "track_id": 1 } }
            ]
        });
        let items = appended_playlist_items(
            &playlist,
            vec![
                json!({ "title": "Second", "qobuzTrack": { "id": 2 } }),
                json!({ "title": "Third", "ref": { "track_id": 3 } }),
            ],
        );

        assert_eq!(items.len(), 3);
        assert_eq!(items[0]["title"], "First");
        assert_eq!(items[1]["title"], "Second");
        assert_eq!(items[2]["title"], "Third");
    }

    #[test]
    fn playlist_item_source_keys_render_from_queue_items() {
        let local = source_ref_queue_item(&json!({
            "kind": "local_track",
            "track_id": 12104,
            "file_name": "1 15 Step.wav",
            "title": "15 Step",
            "artist": "Radiohead",
            "album": "In Rainbows",
            "album_artist": "Radiohead",
            "duration_secs": 237.0
        }))
        .unwrap();
        let qobuz = source_ref_queue_item(&json!({
            "kind": "qobuz_track",
            "track_id": 42,
            "title": "Qobuz Track",
            "artist": "Artist",
            "album": "Album",
            "duration_secs": 180.0
        }))
        .unwrap();

        assert_eq!(playlist_item_source_key(&local), "local:12104");
        assert_eq!(playlist_item_source_key(&qobuz), "qobuz:42");
    }

    #[test]
    fn missing_playlist_id_reports_available_playlists() {
        let playlists = json!([
            { "id": "morning-mix", "name": "Morning Mix", "items": [] }
        ]);

        let error = playlist_by_id(&playlists, "late-set")
            .expect_err("missing playlist should fail")
            .to_string();
        assert!(error.contains("playlist 'late-set' not found"));
        assert!(error.contains("Morning Mix (morning-mix)"));
    }

    #[test]
    fn parses_source_specs_for_top_level_play_and_queue() {
        let cli = Cli::parse_from(["fozmoctl", "play", "local:123"]);
        let Command::Play(args) = cli.command else {
            panic!("expected play command");
        };
        assert_eq!(
            top_level_play_target(&args).unwrap(),
            PlayTarget::Local(LocalTarget::TrackId(123))
        );

        let cli = Cli::parse_from(["fozmoctl", "play", "qobuz:987"]);
        let Command::Play(args) = cli.command else {
            panic!("expected play command");
        };
        assert_eq!(
            top_level_play_target(&args).unwrap(),
            PlayTarget::Qobuz(987)
        );

        let cli = Cli::parse_from(["fozmoctl", "queue", "add", "local:456"]);
        let Command::Queue {
            command: QueueCommand::Add(args),
        } = cli.command
        else {
            panic!("expected queue add command");
        };
        assert_eq!(
            top_level_queue_target(&args).unwrap(),
            PlayTarget::Local(LocalTarget::TrackId(456))
        );

        let cli = Cli::parse_from(["fozmoctl", "queue", "add-many", "local:456", "qobuz:987"]);
        let Command::Queue {
            command: QueueCommand::AddMany(args),
        } = cli.command
        else {
            panic!("expected queue add-many command");
        };
        assert_eq!(args.sources, vec!["local:456", "qobuz:987"]);

        let cli = Cli::parse_from([
            "fozmoctl",
            "queue",
            "get",
            "--summary",
            "--limit",
            "12",
            "--json",
        ]);
        assert!(matches!(
            cli.command,
            Command::Queue {
                command: QueueCommand::Get(QueueGetArgs {
                    summary: true,
                    limit: Some(12),
                    json: true,
                    ..
                })
            }
        ));
    }

    #[test]
    fn parses_agent_zone_commands() {
        let cli = Cli::parse_from(["fozmoctl", "status", "--zone", "Hegel", "--json"]);
        assert!(matches!(
            cli.command,
            Command::Status(StatusArgs {
                zone: ZoneTargetArgs {
                    zone: Some(zone),
                    zone_id: None,
                },
                json: true,
            }) if zone == "Hegel"
        ));

        let cli = Cli::parse_from([
            "fozmoctl",
            "play",
            "--zone-id",
            "local-U_107GUN3uT4",
            "local:860",
            "--queue",
            "local:12225",
            "qobuz:643414",
            "--json",
        ]);
        let Command::Play(args) = cli.command else {
            panic!("expected play command");
        };
        assert_eq!(args.source.as_deref(), Some("local:860"));
        assert_eq!(args.zone.zone_id.as_deref(), Some("local-U_107GUN3uT4"));
        assert_eq!(args.queue, vec!["local:12225", "qobuz:643414"]);
        assert!(args.json);

        let cli = Cli::parse_from([
            "fozmoctl", "queue", "add-many", "--zone", "Hegel", "local:1", "qobuz:2",
        ]);
        let Command::Queue {
            command: QueueCommand::AddMany(args),
        } = cli.command
        else {
            panic!("expected queue add-many command");
        };
        assert_eq!(args.zone.zone.as_deref(), Some("Hegel"));
        assert_eq!(args.sources, vec!["local:1", "qobuz:2"]);

        let cli = Cli::parse_from(["fozmoctl", "stop", "--zone", "Hegel"]);
        assert!(matches!(
            cli.command,
            Command::Stop(ControlArgs {
                zone: ZoneTargetArgs {
                    zone: Some(zone),
                    zone_id: None,
                },
                json: false,
            }) if zone == "Hegel"
        ));

        let cli = Cli::parse_from([
            "fozmoctl", "zones", "swap", "--from", "Lounge", "--to", "Kitchen", "--json",
        ]);
        assert!(matches!(
            cli.command,
            Command::Zones {
                command: ZonesCommand::Swap(ZoneSwapArgs {
                    from,
                    to,
                    keep_source_playing: false,
                    json: true,
                    ..
                })
            } if from == "Lounge" && to == "Kitchen"
        ));

        let cli = Cli::parse_from([
            "fozmoctl",
            "zones",
            "swap",
            "--from",
            "Lounge",
            "--to",
            "Kitchen",
            "--keep-source-playing",
        ]);
        assert!(matches!(
            cli.command,
            Command::Zones {
                command: ZonesCommand::Swap(ZoneSwapArgs {
                    keep_source_playing: true,
                    json: false,
                    ..
                })
            }
        ));

        let cli = Cli::parse_from([
            "fozmoctl",
            "zone",
            "upnp-diagnostics",
            "--zone",
            "KEF",
            "--json",
        ]);
        assert!(matches!(
            cli.command,
            Command::Zones {
                command: ZonesCommand::UpnpDiagnostics(ZoneUpnpDiagnosticsArgs {
                    zone: ZoneTargetArgs {
                        zone: Some(zone),
                        zone_id: None,
                    },
                    json: true,
                })
            } if zone == "KEF"
        ));
    }

    #[test]
    fn resolves_zone_targets() {
        let zones = json!([
            { "id": "Hegel", "name": "Other" },
            { "id": "local-U_107GUN3uT4", "name": "Hegel H390" },
            { "id": "airplay-d8f710d43a06", "name": "H390_D43A06" },
            { "id": "sonos-1", "name": "Sonos" }
        ]);

        let exact_id = resolve_zone_from_list(
            &zones,
            &ZoneTargetArgs {
                zone: Some("Hegel".to_string()),
                zone_id: None,
            },
        )
        .unwrap();
        assert_eq!(exact_id.id, "Hegel");

        let exact_name = resolve_zone_from_list(
            &zones,
            &ZoneTargetArgs {
                zone: Some("hegel h390".to_string()),
                zone_id: None,
            },
        )
        .unwrap();
        assert_eq!(exact_name.id, "local-U_107GUN3uT4");

        let substring = resolve_zone_from_list(
            &zones,
            &ZoneTargetArgs {
                zone: Some("son".to_string()),
                zone_id: None,
            },
        )
        .unwrap();
        assert_eq!(substring.id, "sonos-1");

        let exact_zone_id = resolve_zone_from_list(
            &zones,
            &ZoneTargetArgs {
                zone: None,
                zone_id: Some("airplay-d8f710d43a06".to_string()),
            },
        )
        .unwrap();
        assert_eq!(exact_zone_id.name, "H390_D43A06");
    }

    #[test]
    fn zone_resolution_reports_ambiguous_and_missing_targets() {
        let zones = json!([
            { "id": "local-1", "name": "Hegel H390" },
            { "id": "airplay-1", "name": "H390_D43A06" }
        ]);

        let ambiguous = resolve_zone_from_list(
            &zones,
            &ZoneTargetArgs {
                zone: Some("h390".to_string()),
                zone_id: None,
            },
        )
        .expect_err("ambiguous substring should fail")
        .to_string();
        assert!(ambiguous.contains("ambiguous"));
        assert!(ambiguous.contains("local-1"));
        assert!(ambiguous.contains("airplay-1"));

        let missing = resolve_zone_from_list(
            &zones,
            &ZoneTargetArgs {
                zone: Some("Kitchen".to_string()),
                zone_id: None,
            },
        )
        .expect_err("missing zone should fail")
        .to_string();
        assert!(missing.contains("not found"));
        assert!(missing.contains("Hegel H390"));

        let both = resolve_zone_from_list(
            &zones,
            &ZoneTargetArgs {
                zone: Some("Hegel".to_string()),
                zone_id: Some("local-1".to_string()),
            },
        )
        .expect_err("supplying both zone options should fail")
        .to_string();
        assert!(both.contains("either --zone or --zone-id"));
    }

    #[test]
    fn zone_endpoint_paths_switch_from_active_to_targeted() {
        let zone = ResolvedZone {
            id: "local-U_107GUN3uT4".to_string(),
            name: "Hegel H390".to_string(),
        };

        assert_eq!(status_path(None), "/api/status");
        assert_eq!(queue_path(None), "/api/queue");
        assert_eq!(now_playing_queue_path(None), "/api/now-playing-queue");
        assert_eq!(play_path(None), "/api/play");
        assert_eq!(qobuz_play_path(None), "/api/qobuz/play");
        assert_eq!(control_path(None, "stop"), "/api/stop");

        assert_eq!(
            status_path(Some(&zone)),
            "/api/zones/local-U_107GUN3uT4/status"
        );
        assert_eq!(
            queue_path(Some(&zone)),
            "/api/zones/local-U_107GUN3uT4/queue"
        );
        assert_eq!(
            now_playing_queue_path(Some(&zone)),
            "/api/zones/local-U_107GUN3uT4/now-playing-queue"
        );
        assert_eq!(play_path(Some(&zone)), "/api/zones/local-U_107GUN3uT4/play");
        assert_eq!(
            qobuz_play_path(Some(&zone)),
            "/api/zones/local-U_107GUN3uT4/qobuz/play"
        );
        assert_eq!(
            control_path(Some(&zone), "stop"),
            "/api/zones/local-U_107GUN3uT4/stop"
        );
        assert_eq!(
            transfer_path(&zone),
            "/api/zones/local-U_107GUN3uT4/transfer"
        );
    }

    #[test]
    fn track_search_prefers_local_for_duplicate_qobuz_track() {
        let response = build_track_search_response(
            "Radiohead Kid A",
            &json!({
                "tracks": [{
                    "id": 12115,
                    "title": "Kid A",
                    "artist": null,
                    "album_artist": "Radiohead",
                    "album": "Kid A",
                    "duration_secs": 284.50666666666666,
                    "sample_rate": 44100,
                    "bit_depth": 16
                }]
            }),
            Some(&json!({
                "tracks": [{
                    "id": 999,
                    "title": "Kid A",
                    "artist": "Radiohead",
                    "album": "Kid A",
                    "duration": 285
                }]
            })),
            Vec::new(),
        );

        let tracks = response["tracks"].as_array().unwrap();
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0]["source"], "local");
        assert_eq!(tracks[0]["source_key"], "local:12115");
        assert_eq!(tracks[0]["artist"], "Radiohead");
        assert_eq!(tracks[0]["deduped_qobuz_ids"], json!(["999"]));
    }

    #[test]
    fn track_search_uses_preferred_qobuz_source_for_primary_version() {
        let mut response = build_track_search_response(
            "Björk Venus as a Boy",
            &json!({
                "tracks": [{
                    "id": 12115,
                    "title": "Venus as a Boy",
                    "artist": "Björk",
                    "album": "Debut",
                    "duration_secs": 240.0,
                    "sample_rate": 44100,
                    "bit_depth": 16,
                    "preferred_play_source": {
                        "kind": "qobuz",
                        "track_id": 9901,
                        "title": "Venus as a Boy",
                        "artist": "Björk",
                        "album": "Debut",
                        "album_id": "debut-hires",
                        "image_url": null,
                        "duration_secs": 240.0,
                        "format_id": 7
                    }
                }]
            }),
            Some(&json!({
                "tracks": [{
                    "id": 9901,
                    "title": "Venus as a Boy",
                    "artist": "Björk",
                    "album": "Debut",
                    "duration": 240
                }]
            })),
            Vec::new(),
        );

        apply_track_search_options(&mut response, "Björk Venus as a Boy", true, Some(1));
        let tracks = response["tracks"].as_array().unwrap();
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0]["source"], "qobuz");
        assert_eq!(tracks[0]["source_key"], "qobuz:9901");
        assert_eq!(tracks[0]["matched_local_source_key"], "local:12115");
        assert_eq!(tracks[0]["deduped_qobuz_ids"], json!(["9901"]));
    }

    #[test]
    fn track_search_dedupes_punctuation_and_duration_matches() {
        let response = build_track_search_response(
            "stereolab french disko",
            &json!({
                "tracks": [{
                    "id": 12,
                    "title": "French Disko",
                    "artist": "Stereolab",
                    "album": "Refried Ectoplasm",
                    "duration_secs": 215.2
                }]
            }),
            Some(&json!({
                "tracks": [{
                    "id": 34,
                    "title": "French-Disko (Remastered)",
                    "artist": "Stereolab",
                    "album": "Different Compilation",
                    "duration": 216
                }]
            })),
            Vec::new(),
        );

        let tracks = response["tracks"].as_array().unwrap();
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0]["source_key"], "local:12");
        assert_eq!(tracks[0]["deduped_qobuz_ids"], json!(["34"]));
    }

    #[test]
    fn track_search_keeps_distinct_demo_or_live_versions() {
        let response = build_track_search_response(
            "Radiohead Kid A",
            &json!({
                "tracks": [{
                    "id": 12115,
                    "title": "Kid A",
                    "artist": "Radiohead",
                    "album": "Kid A",
                    "duration_secs": 284.5
                }]
            }),
            Some(&json!({
                "tracks": [{
                    "id": 333,
                    "title": "Kid A (Live)",
                    "artist": "Radiohead",
                    "album": "Live at Bonnaroo 2006",
                    "duration": 227
                }]
            })),
            Vec::new(),
        );

        let tracks = response["tracks"].as_array().unwrap();
        assert_eq!(tracks.len(), 2);
        assert_eq!(tracks[0]["source_key"], "local:12115");
        assert_eq!(tracks[1]["source_key"], "qobuz:333");
    }

    #[test]
    fn ranked_track_search_prefers_original_local_version() {
        let mut response = build_track_search_response(
            "Radiohead Kid A",
            &json!({
                "tracks": [{
                    "id": 12115,
                    "title": "Kid A",
                    "artist": "Radiohead",
                    "album": "Kid A",
                    "duration_secs": 284.5
                }]
            }),
            Some(&json!({
                "tracks": [
                    {
                        "id": 333,
                        "title": "Kid A (Live)",
                        "artist": "Radiohead",
                        "album": "Live at Bonnaroo 2006",
                        "duration": 227
                    },
                    {
                        "id": 444,
                        "title": "Kid A (Remix)",
                        "artist": "Radiohead",
                        "album": "Kid A Remixes",
                        "duration": 330
                    },
                    {
                        "id": 555,
                        "title": "Kid A",
                        "artist": "Vitamin String Quartet",
                        "album": "Vitamin String Quartet Performs Radiohead",
                        "duration": 270
                    }
                ]
            })),
            Vec::new(),
        );

        apply_track_search_options(&mut response, "Radiohead Kid A", true, None);
        let tracks = response["tracks"].as_array().unwrap();
        assert_eq!(tracks[0]["source_key"], "local:12115");
        assert_eq!(tracks[3]["source_key"], "qobuz:555");
    }

    #[test]
    fn ranked_track_search_honors_version_descriptors_and_best_limit() {
        let response = build_track_search_response(
            "Radiohead Kid A",
            &json!({
                "tracks": [{
                    "id": 12115,
                    "title": "Kid A",
                    "artist": "Radiohead",
                    "album": "Kid A",
                    "duration_secs": 284.5
                }]
            }),
            Some(&json!({
                "tracks": [
                    {
                        "id": 333,
                        "title": "Kid A (Live)",
                        "artist": "Radiohead",
                        "album": "Live at Bonnaroo 2006",
                        "duration": 227
                    },
                    {
                        "id": 444,
                        "title": "Kid A (Remix)",
                        "artist": "Radiohead",
                        "album": "Kid A Remixes",
                        "duration": 330
                    }
                ]
            })),
            Vec::new(),
        );

        let mut live = response.clone();
        apply_track_search_options(&mut live, "Radiohead Kid A live", true, Some(1));
        let tracks = live["tracks"].as_array().unwrap();
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0]["source_key"], "qobuz:333");

        let mut remix = response;
        apply_track_search_options(&mut remix, "Radiohead Kid A remix", true, Some(1));
        let tracks = remix["tracks"].as_array().unwrap();
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0]["source_key"], "qobuz:444");
    }

    #[test]
    fn track_search_keeps_qobuz_only_discoveries() {
        let response = build_track_search_response(
            "Stereolab Metronomic Underground",
            &json!({ "tracks": [] }),
            Some(&json!({
                "tracks": [{
                    "id": 79173662u64,
                    "title": "Metronomic Underground",
                    "artist": "Stereolab",
                    "album": "Emperor Tomato Ketchup",
                    "duration": 474,
                    "maximum_sampling_rate": 44.1,
                    "maximum_bit_depth": 16
                }]
            })),
            Vec::new(),
        );

        let tracks = response["tracks"].as_array().unwrap();
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0]["source"], "qobuz");
        assert_eq!(tracks[0]["source_key"], "qobuz:79173662");
        assert_eq!(tracks[0]["id"], 79173662u64);
        assert!(
            tracks[0]["deduped_qobuz_ids"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn track_search_json_results_have_playable_fields_and_warnings() {
        let response = build_track_search_response(
            "anything",
            &json!({
                "tracks": [{
                    "id": 1,
                    "title": "Track",
                    "artist": "Artist",
                    "album": "Album"
                }]
            }),
            None,
            vec!["qobuz search failed: not logged in".to_string()],
        );

        assert_eq!(response["query"], "anything");
        assert_eq!(
            response["warnings"],
            json!(["qobuz search failed: not logged in"])
        );
        let track = &response["tracks"].as_array().unwrap()[0];
        assert_eq!(track["source"], "local");
        assert_eq!(track["source_key"], "local:1");
        assert_eq!(track["id"], 1);
    }

    #[test]
    fn parses_qobuz_queue_add() {
        let cli = Cli::parse_from([
            "fozmoctl",
            "qobuz",
            "queue",
            "add",
            "--track-id",
            "987654321",
        ]);
        assert!(matches!(
            cli.command,
            Command::Qobuz {
                command: QobuzCommand::Queue {
                    command: QobuzQueueCommand::Add(QobuzTrackArgs {
                        track_id: 987654321
                    })
                }
            }
        ));
    }

    #[test]
    fn rejects_ambiguous_local_inputs() {
        let args = PlayArgs {
            source: None,
            track_id: Some(1),
            file_name: Some("01 Track.flac".to_string()),
            queue: Vec::new(),
            zone: ZoneTargetArgs::default(),
            json: false,
        };
        assert!(top_level_play_target(&args).is_err());

        let args = PlayArgs {
            source: Some("qobuz:1".to_string()),
            track_id: Some(1),
            file_name: None,
            queue: Vec::new(),
            zone: ZoneTargetArgs::default(),
            json: false,
        };
        assert!(top_level_play_target(&args).is_err());
    }

    #[test]
    fn qobuz_source_maps_id_and_duration() {
        let source = qobuz_source_ref(&json!({
            "id": 987,
            "title": "Nude",
            "artist": "Radiohead",
            "album": "In Rainbows",
            "album_id": "abc",
            "image_url": "https://example.test/cover.jpg",
            "duration": 256
        }))
        .unwrap();

        assert_eq!(source["kind"], "qobuz_track");
        assert_eq!(source["track_id"], 987);
        assert_eq!(source["duration_secs"], 256.0);
    }

    #[test]
    fn qobuz_play_body_maps_track_detail() {
        let body = qobuz_play_body(
            &json!({
                "id": 42,
                "title": "Track",
                "artist": "Artist",
                "album": "Album",
                "duration": 180
            }),
            &[],
        )
        .unwrap();

        assert_eq!(body["track_id"], 42);
        assert_eq!(body["duration_secs"], 180.0);
        assert_eq!(body["replace_current"], true);
        assert_eq!(body["queue"].as_array().map(Vec::len), Some(0));
    }

    #[test]
    fn play_bodies_include_follow_up_queue_sources() {
        let local_queue = json!({
            "kind": "local_track",
            "track_id": 12225,
            "title": "Subterranean Homesick Alien"
        });
        let qobuz_queue = json!({
            "kind": "qobuz_track",
            "track_id": 643414u64,
            "title": "Mysteries",
            "artist": "Beth Gibbons",
            "album": "Out Of Season",
            "duration_secs": 278.0,
            "radio": false
        });

        let local_body = local_play_body(
            LocalTarget::TrackId(860),
            vec![local_queue.clone(), qobuz_queue.clone()],
        );
        assert_eq!(local_body["track_id"], 860);
        let queue = local_body["queue"].as_array().unwrap();
        assert_eq!(queue.len(), 2);
        assert_eq!(queue[0]["track_id"], 12225);
        assert_eq!(queue[1]["track_id"], 643414u64);

        let qobuz_body = qobuz_play_body(
            &json!({
                "id": 100u64,
                "title": "Current Qobuz",
                "artist": "Artist",
                "album": "Album",
                "duration": 180
            }),
            &[local_queue, qobuz_queue],
        )
        .unwrap();
        let queue = qobuz_body["queue"].as_array().unwrap();
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0]["track_id"], 643414u64);
    }

    #[test]
    fn local_source_maps_track_summary() {
        let source = local_source_ref(&json!({
            "id": 12104,
            "file_name": "1 15 Step.wav",
            "title": "15 Step",
            "artist": "Radiohead",
            "album": "In Rainbows",
            "album_artist": "Radiohead",
            "album_id": 861,
            "art_id": 706,
            "duration_secs": 237.29333333333332
        }))
        .unwrap();

        assert_eq!(source["kind"], "local_track");
        assert_eq!(source["track_id"], 12104);
        assert_eq!(source["file_name"], "1 15 Step.wav");
    }

    #[test]
    fn source_refs_become_frontend_queue_items() {
        let local = source_ref_queue_item(&json!({
            "kind": "local_track",
            "track_id": 12104,
            "file_name": "1 15 Step.wav",
            "title": "15 Step",
            "artist": "Radiohead",
            "album": "In Rainbows",
            "album_artist": "Radiohead",
            "album_id": 861,
            "art_id": 706,
            "duration_secs": 237.0,
            "radio": false
        }))
        .unwrap();
        let qobuz = source_ref_queue_item(&json!({
            "kind": "qobuz_track",
            "track_id": 42,
            "title": "Qobuz Track",
            "artist": "Artist",
            "album": "Album",
            "duration_secs": 180.0,
            "radio": false
        }))
        .unwrap();

        assert_eq!(local["ref"]["track_id"], 12104);
        assert_eq!(local["resolvedSource"]["kind"], "local_track");
        assert_eq!(qobuz["qobuzTrack"]["id"], 42);
        assert_eq!(qobuz["resolvedSource"]["kind"], "qobuz_track");
        assert_eq!(queue_kind_for_items(&[local, qobuz]), "mixed");
    }

    #[test]
    fn appends_local_queue_item_to_existing_sources() {
        let body = append_queue_body(
            &json!({
                "current_source": {
                    "kind": "local_track",
                    "track_id": 123,
                    "title": "Current"
                },
                "queued_sources": [
                    {
                        "kind": "qobuz_track",
                        "track_id": 987,
                        "title": "Queued"
                    }
                ]
            }),
            json!({ "track_id": 456 }),
        );

        assert_eq!(body["expected_current"], "local:123");
        let queue = body["queue"].as_array().unwrap();
        assert_eq!(queue.len(), 2);
        assert_eq!(queue[1]["track_id"], 456);
    }

    #[test]
    fn batch_append_preserves_current_and_appends_all_sources() {
        let body = append_queue_items_body(
            &json!({
                "current_source": {
                    "kind": "local_track",
                    "track_id": 123,
                    "title": "Current"
                },
                "queued_sources": [
                    {
                        "kind": "local_track",
                        "track_id": 456,
                        "title": "Already Queued"
                    }
                ]
            }),
            vec![
                json!({ "track_id": 789 }),
                json!({
                    "kind": "qobuz_track",
                    "track_id": 987,
                    "title": "Qobuz Queued"
                }),
            ],
        );

        assert_eq!(body["expected_current"], "local:123");
        let queue = body["queue"].as_array().unwrap();
        assert_eq!(queue.len(), 3);
        assert_eq!(queue[1]["track_id"], 789);
        assert_eq!(queue[2]["kind"], "qobuz_track");
        assert_eq!(queue[2]["track_id"], 987);
    }

    #[test]
    fn mutation_confirmation_reports_zone_current_and_queue_count() {
        let zone = ResolvedZone {
            id: "local-U_107GUN3uT4".to_string(),
            name: "Hegel H390".to_string(),
        };
        let confirmation = mutation_confirmation(
            &json!({
                "active_zone_id": "local-U_107GUN3uT4",
                "active_zone_name": "Hegel H390",
                "state": "Playing",
                "track_title": "Unravel",
                "track_artist": "Björk",
                "current_source": {
                    "kind": "local_track",
                    "track_id": 860
                }
            }),
            &json!({
                "queued_sources": [
                    { "kind": "local_track", "track_id": 12225 },
                    { "kind": "qobuz_track", "track_id": 643414u64 }
                ]
            }),
            Some(&zone),
        );

        assert_eq!(confirmation["zone_id"], "local-U_107GUN3uT4");
        assert_eq!(confirmation["zone_name"], "Hegel H390");
        assert_eq!(confirmation["current_source_key"], "local:860");
        assert_eq!(confirmation["queued_count"], 2);
    }

    #[test]
    fn append_uses_saved_cursor_when_live_current_is_missing() {
        let body = append_queue_body(
            &json!({
                "current_source": null,
                "queued_sources": [],
                "state": {
                    "cursor": 0,
                    "items": [{
                        "title": "15 Step",
                        "ref": { "track_id": 12104 },
                        "resolvedSource": {
                            "kind": "local_track",
                            "track_id": 12104,
                            "title": "15 Step"
                        }
                    }],
                    "kind": "local",
                    "loopMode": "off"
                }
            }),
            json!({ "track_id": 12106 }),
        );

        assert_eq!(body["expected_current"], "local:12104");
        assert_eq!(body["queue"].as_array().unwrap()[0]["track_id"], 12106);
    }

    #[test]
    fn queue_summary_returns_compact_current_and_limited_queued_sources() {
        let summary = queue_summary(
            &json!({
                "current_source": {
                    "kind": "local_track",
                    "track_id": 12104,
                    "title": "15 Step",
                    "artist": "Radiohead",
                    "album": "In Rainbows",
                    "duration_secs": 237.0
                },
                "queued_sources": [
                    {
                        "kind": "qobuz_track",
                        "track_id": 42,
                        "title": "Qobuz Track",
                        "artist": "Artist",
                        "album": "Album",
                        "duration_secs": 180.0
                    },
                    {
                        "kind": "local_track",
                        "track_id": 12106,
                        "title": "Bodysnatchers",
                        "artist": "Radiohead",
                        "album": "In Rainbows",
                        "duration_secs": 242.0
                    }
                ]
            }),
            Some(1),
        );

        assert_eq!(summary["current"]["source_key"], "local:12104");
        assert_eq!(summary["current"]["title"], "15 Step");
        let queued = summary["queued"].as_array().unwrap();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0]["source_key"], "qobuz:42");
        assert_eq!(queued[0]["kind"], "qobuz_track");
        assert_eq!(queued[0]["duration_secs"], 180.0);
    }

    #[test]
    fn saved_current_source_recovers_cursor_item() {
        let snapshot = json!({
            "current_source": null,
            "queued_sources": [{ "kind": "local_track", "track_id": 12106, "title": "Bodysnatchers" }],
            "state": {
                "cursor": 0,
                "items": [{
                    "title": "15 Step",
                    "artist": "Radiohead",
                    "album": "In Rainbows",
                    "ref": { "track_id": 12104, "file_name": "1 15 Step.wav" }
                }],
                "kind": "local",
                "loopMode": "off"
            }
        });

        let current = saved_current_source(&snapshot).unwrap();
        assert_eq!(current["kind"], "local_track");
        assert_eq!(current["track_id"], 12104);
        assert_eq!(current["title"], "15 Step");
    }

    #[test]
    fn file_name_is_a_library_field_not_a_relative_path_requirement() {
        let args = PlayArgs {
            source: None,
            track_id: None,
            file_name: Some("01 Track.flac".to_string()),
            queue: Vec::new(),
            zone: ZoneTargetArgs::default(),
            json: false,
        };
        assert_eq!(
            top_level_play_target(&args).unwrap(),
            PlayTarget::Local(LocalTarget::FileName("01 Track.flac".to_string()))
        );
    }
}
