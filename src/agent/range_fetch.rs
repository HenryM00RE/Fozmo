use crate::app::identity;
use reqwest::header::{CONTENT_LENGTH, CONTENT_RANGE, HeaderMap, HeaderValue, RANGE};
use reqwest::{Client, StatusCode, Url};
use std::sync::{Arc, Condvar, Mutex};

pub(super) struct AgentRangeBlock {
    pub(super) start: u64,
    pub(super) bytes: Vec<u8>,
    pub(super) byte_len: Option<u64>,
}

pub(super) type AgentRangePrefetchResult =
    Arc<(Mutex<Option<Result<AgentRangeBlock, String>>>, Condvar)>;

impl AgentRangeBlock {
    pub(super) fn contains(&self, position: u64) -> bool {
        let end = self.start.saturating_add(self.bytes.len() as u64);
        position >= self.start && position < end
    }

    pub(super) fn is_empty_at(&self, position: u64, known_len: Option<u64>) -> bool {
        self.bytes.is_empty()
            && position == self.start
            && known_len
                .or(self.byte_len)
                .is_some_and(|len| position >= len)
    }
}

pub(super) struct AgentRangePrefetch {
    pub(super) start: u64,
    pub(super) range_bytes: u64,
    pub(super) result: AgentRangePrefetchResult,
    pub(super) task: tokio::task::JoinHandle<()>,
}

impl AgentRangePrefetch {
    pub(super) fn could_contain(&self, position: u64) -> bool {
        position >= self.start && position < self.start.saturating_add(self.range_bytes)
    }

    pub(super) fn try_take(&self) -> Option<Result<AgentRangeBlock, String>> {
        self.result.0.lock().unwrap().take()
    }

    pub(super) fn wait(self) -> Result<AgentRangeBlock, String> {
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

pub(super) async fn fetch_agent_range(
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

pub(super) fn agent_auth_headers(token: &str) -> Result<HeaderMap, String> {
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
