use super::range_fetch::{
    AgentRangeBlock, AgentRangePrefetch, agent_auth_headers, fetch_agent_range,
};
use reqwest::header::HeaderMap;
use reqwest::{Client, Url};
use std::io::{self, Read, Seek, SeekFrom};
use std::sync::{Arc, Condvar, Mutex};
use symphonia::core::io::MediaSource;

const AGENT_STREAM_PREFETCH_BYTES: u64 = 2 * 1024 * 1024;
// Each range block is fully buffered (by reqwest here and, for Qobuz, by the
// core's proxy) before any byte becomes readable, so a cold buffer fill or a
// seek stalls the decoder for the entire block transfer. Keep blocks small;
// the next-block prefetch pipeline maintains throughput between blocks.
const AGENT_STREAM_RANGE_BYTES: u64 = 8 * 1024 * 1024;

// Agent playback should prioritize the current read position. A whole-file
// progressive cache can backfill skipped bytes after a seek and starve playback.
pub(super) struct AgentStreamSource {
    client: Client,
    url: Url,
    headers: HeaderMap,
    runtime: tokio::runtime::Handle,
    position: u64,
    pub(super) byte_len: Option<u64>,
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
    pub(super) async fn open(token: &str, url: Url) -> Result<Self, String> {
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
