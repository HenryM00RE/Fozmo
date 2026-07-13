use serde::Serialize;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::time::Instant;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::{sleep, timeout};

const DEFAULT_PORT: u16 = 50001;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const READ_IDLE_TIMEOUT: Duration = Duration::from_millis(350);
const COMMAND_GAP: Duration = Duration::from_millis(45);

#[derive(Debug, Clone, Default, Serialize)]
pub struct HegelStatus {
    pub power: Option<bool>,
    pub input: Option<u8>,
    pub volume: Option<u8>,
    pub muted: Option<bool>,
    pub reset: Option<String>,
    pub raw: Vec<String>,
}

#[derive(Clone, Default)]
pub struct HegelStatusCache {
    cached_status: Arc<Mutex<Option<HegelStatus>>>,
    checked_at: Arc<Mutex<Option<Instant>>>,
    directly_observed_power_input: Arc<Mutex<Option<DirectPowerInputObservation>>>,
}

#[derive(Clone)]
struct DirectPowerInputObservation {
    status: HegelStatus,
    observed_at: Instant,
}

impl HegelStatusCache {
    pub(crate) fn remember(&self, mut status: HegelStatus) -> HegelStatus {
        // Capture freshness before filling partial replies from the display
        // cache. A merged power/input pair must never masquerade as one
        // directly observed together in the current Hegel response.
        let directly_observed = status
            .directly_observes_power_and_input()
            .then(|| status.clone());
        {
            let mut cached = self.cached_status.lock().unwrap();
            if let Some(previous) = cached.as_ref() {
                status.fill_missing_from(previous);
            }
            *cached = Some(status.clone());
            if let Some(status) = directly_observed {
                *self.directly_observed_power_input.lock().unwrap() =
                    Some(DirectPowerInputObservation {
                        status,
                        observed_at: Instant::now(),
                    });
            }
        }
        status
    }

    pub(crate) fn cached(&self) -> Option<HegelStatus> {
        self.cached_status.lock().unwrap().clone()
    }

    pub(crate) fn mark_poll_due(&self, interval: Duration) -> bool {
        let mut checked_at = self.checked_at.lock().unwrap();
        let now = Instant::now();
        let due = checked_at
            .map(|previous| now.duration_since(previous) >= interval)
            .unwrap_or(true);
        if due {
            *checked_at = Some(now);
        }
        due
    }

    pub(crate) fn fresh_direct_power_input(&self, max_age: Duration) -> Option<HegelStatus> {
        let now = Instant::now();
        self.directly_observed_power_input
            .lock()
            .unwrap()
            .as_ref()
            .filter(|observation| now.saturating_duration_since(observation.observed_at) <= max_age)
            .map(|observation| observation.status.clone())
    }
}

impl HegelStatus {
    fn directly_observes_power_and_input(&self) -> bool {
        let Some(power) = self.power else {
            return false;
        };
        let Some(input) = self.input else {
            return false;
        };
        let expected_power = format!("-p.{}", bool_parameter(power));
        let expected_input = format!("-i.{input}");
        self.raw.iter().any(|line| line == &expected_power)
            && self.raw.iter().any(|line| line == &expected_input)
    }

    fn fill_missing_from(&mut self, previous: &HegelStatus) {
        if self.power.is_none() {
            self.power = previous.power;
        }
        if self.input.is_none() {
            self.input = previous.input;
        }
        if self.volume.is_none() {
            self.volume = previous.volume;
        }
        if self.muted.is_none() {
            self.muted = previous.muted;
        }
        if self.reset.is_none() {
            self.reset = previous.reset.clone();
        }
    }
}

pub fn default_port(port: Option<u16>) -> u16 {
    port.unwrap_or(DEFAULT_PORT)
}

pub async fn query_status(host: &str, port: u16) -> Result<HegelStatus, String> {
    send_commands(host, port, &["-p.?", "-i.?", "-v.?", "-m.?"]).await
}

pub async fn set_power(host: &str, port: u16, on: bool) -> Result<HegelStatus, String> {
    let command = if on { "-p.1" } else { "-p.0" };
    send_commands(host, port, &[command, "-p.?", "-i.?", "-v.?", "-m.?"]).await
}

pub async fn set_input(host: &str, port: u16, input: u8) -> Result<HegelStatus, String> {
    if !(1..=20).contains(&input) {
        return Err("Input must be between 1 and 20".to_string());
    }
    let command = format!("-i.{input}");
    send_owned_commands(
        host,
        port,
        vec![
            command,
            "-p.?".to_string(),
            "-i.?".to_string(),
            "-v.?".to_string(),
            "-m.?".to_string(),
        ],
    )
    .await
}

pub async fn set_volume(host: &str, port: u16, volume: u8) -> Result<HegelStatus, String> {
    if volume > 100 {
        return Err("Volume must be between 0 and 100".to_string());
    }
    let command = format!("-v.{volume}");
    send_owned_commands(
        host,
        port,
        vec![
            command,
            "-p.?".to_string(),
            "-i.?".to_string(),
            "-v.?".to_string(),
            "-m.?".to_string(),
        ],
    )
    .await
}

pub async fn set_mute(host: &str, port: u16, muted: bool) -> Result<HegelStatus, String> {
    let command = if muted { "-m.1" } else { "-m.0" };
    send_commands(host, port, &[command, "-p.?", "-i.?", "-v.?", "-m.?"]).await
}

async fn send_owned_commands(
    host: &str,
    port: u16,
    commands: Vec<String>,
) -> Result<HegelStatus, String> {
    let borrowed = commands.iter().map(String::as_str).collect::<Vec<_>>();
    send_commands(host, port, &borrowed).await
}

async fn send_commands(host: &str, port: u16, commands: &[&str]) -> Result<HegelStatus, String> {
    let host = normalize_host(host)?;
    let mut stream = timeout(CONNECT_TIMEOUT, TcpStream::connect((host.as_str(), port)))
        .await
        .map_err(|_| format!("Timed out connecting to {host}:{port}"))?
        .map_err(|e| format!("Failed to connect to {host}:{port}: {e}"))?;

    for command in commands {
        stream
            .write_all(format!("{command}\r").as_bytes())
            .await
            .map_err(|e| format!("Failed to send Hegel command: {e}"))?;
        sleep(COMMAND_GAP).await;
    }
    let _ = stream.flush().await;

    let requested_fields = RequestedStatusFields::from_commands(commands);
    read_status_until_complete(&mut stream, requested_fields, READ_IDLE_TIMEOUT).await
}

#[derive(Clone, Copy, Debug, Default)]
struct RequestedStatusFields {
    power: bool,
    input: bool,
    volume: bool,
    muted: bool,
}

impl RequestedStatusFields {
    fn from_commands(commands: &[&str]) -> Self {
        let mut fields = Self::default();
        for command in commands {
            let Some((name, _)) = command
                .strip_prefix('-')
                .and_then(|value| value.split_once('.'))
            else {
                continue;
            };
            match name {
                "p" => fields.power = true,
                "i" => fields.input = true,
                "v" => fields.volume = true,
                "m" => fields.muted = true,
                _ => {}
            }
        }
        fields
    }

    fn all_arrived(self, status: &HegelStatus) -> bool {
        let any_requested = self.power || self.input || self.volume || self.muted;
        any_requested
            && (!self.power || status.power.is_some())
            && (!self.input || status.input.is_some())
            && (!self.volume || status.volume.is_some())
            && (!self.muted || status.muted.is_some())
    }
}

async fn read_status_until_complete(
    stream: &mut (impl tokio::io::AsyncRead + Unpin),
    requested_fields: RequestedStatusFields,
    idle_timeout: Duration,
) -> Result<HegelStatus, String> {
    let mut bytes = Vec::new();
    let mut buf = [0_u8; 1024];
    loop {
        match timeout(idle_timeout, stream.read(&mut buf)).await {
            Ok(Ok(0)) => break,
            Ok(Ok(n)) => {
                bytes.extend_from_slice(&buf[..n]);
                let status = parse_status(&bytes);
                if requested_fields.all_arrived(&status) {
                    return Ok(status);
                }
            }
            Ok(Err(e)) => return Err(format!("Failed to read Hegel response: {e}")),
            Err(_) => break,
        }
    }

    Ok(parse_status(&bytes))
}

fn normalize_host(host: &str) -> Result<String, String> {
    let host = host.trim();
    if host.is_empty() {
        return Err("Hegel host/IP is required".to_string());
    }
    Ok(host.to_string())
}

fn parse_status(bytes: &[u8]) -> HegelStatus {
    let body = String::from_utf8_lossy(bytes);
    let mut status = HegelStatus::default();
    for line in body
        .split(['\r', '\n'])
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let Some((command, parameter)) = line.strip_prefix('-').unwrap_or(line).split_once('.')
        else {
            continue;
        };
        match command {
            "p" => {
                if let Some(value) = parse_bool(parameter) {
                    status.power = Some(value);
                    status.raw.push(format!("-p.{}", bool_parameter(value)));
                }
            }
            "i" => {
                if let Ok(value) = parameter.parse::<u8>()
                    && (1..=20).contains(&value)
                {
                    status.input = Some(value);
                    status.raw.push(format!("-i.{value}"));
                }
            }
            "v" => {
                if let Ok(value) = parameter.parse::<u8>() {
                    let value = value.min(100);
                    status.volume = Some(value);
                    status.raw.push(format!("-v.{value}"));
                }
            }
            "m" => {
                if let Some(value) = parse_bool(parameter) {
                    status.muted = Some(value);
                    status.raw.push(format!("-m.{}", bool_parameter(value)));
                }
            }
            "r" => {
                if is_safe_status_parameter(parameter) {
                    status.reset = Some(parameter.to_string());
                    status.raw.push(format!("-r.{parameter}"));
                }
            }
            "e" => {}
            _ => {}
        }
    }
    status
}

fn bool_parameter(value: bool) -> &'static str {
    if value { "1" } else { "0" }
}

fn parse_bool(value: &str) -> Option<bool> {
    match value {
        "1" => Some(true),
        "0" => Some(false),
        _ => None,
    }
}

fn is_safe_status_parameter(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, b'-' | b'_' | b'.'))
}

#[cfg(test)]
mod tests {
    use super::{
        HegelStatus, HegelStatusCache, READ_IDLE_TIMEOUT, RequestedStatusFields, parse_status,
        read_status_until_complete,
    };
    use std::time::Duration;
    use tokio::io::AsyncWriteExt;
    use tokio::time::timeout;

    #[test]
    fn parses_combined_status_lines() {
        let status = parse_status(b"-p.1\r-i.9\r-v.43\r-m.0\r");
        assert_eq!(status.power, Some(true));
        assert_eq!(status.input, Some(9));
        assert_eq!(status.volume, Some(43));
        assert_eq!(status.muted, Some(false));
        assert_eq!(status.raw.len(), 4);
    }

    #[test]
    fn raw_status_omits_non_hegel_response_lines() {
        let status = parse_status(b"SSH-2.0-service\rHTTP/1.1 200 OK\r-p.nope\r-v.12\r");

        assert_eq!(status.volume, Some(12));
        assert_eq!(status.raw, vec!["-v.12"]);
    }

    #[test]
    fn status_cache_preserves_known_fields_across_partial_responses() {
        let cache = HegelStatusCache::default();
        cache.remember(HegelStatus {
            power: Some(true),
            input: Some(9),
            volume: Some(35),
            muted: Some(false),
            reset: None,
            raw: vec!["-p.1".to_string(), "-i.9".to_string()],
        });
        let directly_observed = cache
            .fresh_direct_power_input(Duration::from_secs(1))
            .expect("complete raw power/input response should be fresh");
        assert_eq!(directly_observed.power, Some(true));
        assert_eq!(directly_observed.input, Some(9));
        std::thread::sleep(Duration::from_millis(5));

        let status = cache.remember(HegelStatus {
            volume: Some(36),
            raw: vec!["-v.36".to_string()],
            ..HegelStatus::default()
        });

        assert_eq!(status.power, Some(true));
        assert_eq!(status.input, Some(9));
        assert_eq!(status.volume, Some(36));
        assert_eq!(status.muted, Some(false));
        assert_eq!(status.raw, vec!["-v.36"]);
        assert!(
            cache
                .fresh_direct_power_input(Duration::from_millis(1))
                .is_none(),
            "fields merged from an older response must not refresh direct readiness"
        );
    }

    #[test]
    fn status_cache_requires_matching_raw_power_and_input_observations() {
        let cache = HegelStatusCache::default();
        cache.remember(HegelStatus {
            power: Some(true),
            input: Some(9),
            volume: Some(35),
            muted: Some(false),
            reset: None,
            raw: vec!["-v.35".to_string()],
        });

        assert!(
            cache
                .fresh_direct_power_input(Duration::from_secs(1))
                .is_none(),
            "typed fields without their raw response lines are not a direct observation"
        );
    }

    #[tokio::test]
    async fn complete_reply_returns_without_waiting_for_idle_timeout() {
        let (mut client, mut server) = tokio::io::duplex(256);
        server
            .write_all(b"-p.1\r-i.9\r-v.43\r-m.0\r")
            .await
            .unwrap();

        let requested = RequestedStatusFields::from_commands(&["-p.?", "-i.?", "-v.?", "-m.?"]);
        let status = timeout(
            Duration::from_millis(100),
            read_status_until_complete(&mut client, requested, Duration::from_secs(5)),
        )
        .await
        .expect("complete response should not wait for the idle timeout")
        .unwrap();

        assert_eq!(status.power, Some(true));
        assert_eq!(status.input, Some(9));
        assert_eq!(status.volume, Some(43));
        assert_eq!(status.muted, Some(false));
    }

    #[tokio::test]
    async fn partial_reply_waits_for_idle_timeout_then_returns_known_fields() {
        let (mut client, mut server) = tokio::io::duplex(256);
        server.write_all(b"-p.1\r-v.43\r").await.unwrap();

        let requested = RequestedStatusFields::from_commands(&["-p.?", "-i.?", "-v.?", "-m.?"]);
        let mut response = Box::pin(read_status_until_complete(
            &mut client,
            requested,
            READ_IDLE_TIMEOUT,
        ));

        assert!(
            timeout(READ_IDLE_TIMEOUT / 2, &mut response).await.is_err(),
            "partial response returned before the idle timeout"
        );
        let status = timeout(READ_IDLE_TIMEOUT, response)
            .await
            .expect("partial response should return after the idle timeout")
            .unwrap();

        assert_eq!(status.power, Some(true));
        assert_eq!(status.input, None);
        assert_eq!(status.volume, Some(43));
        assert_eq!(status.muted, None);
    }
}
