use crate::audio::dsp::resampler::{FilterType, SincResampler};
use crate::audio::engine::player::{TrackCover, TrackTags};
use crate::zones::constant_time_token_matches;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use flacenc::bitsink::ByteSink;
use flacenc::component::{BitRepr, MetadataBlockData, Stream};
use flacenc::config;
use flacenc::error::Verify;
use flacenc::source::MemSource;
use quick_xml::Reader;
use quick_xml::events::{BytesRef, Event};
use rand::{RngCore, rngs::OsRng};
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::File;
use std::io::ErrorKind;
use std::net::{IpAddr, Ipv4Addr, SocketAddrV4};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use symphonia::core::codecs::{CODEC_TYPE_NULL, DecoderOptions};
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use tokio::net::UdpSocket;

pub const SONOS_DEVICE_PREFIX: &str = "Sonos UPnP:";
pub const SONOS_SAMPLE_RATE: u32 = 48_000;
pub const SONOS_BIT_DEPTH: u8 = 24;
const SONOS_CONTROL_PORT: u16 = 1400;
const SONOS_DISCOVERY_INTERVAL: Duration = Duration::from_secs(30);
const SONOS_ASSET_TTL: Duration = Duration::from_secs(60 * 60 * 4);
const SONOS_PLAYBACK_REFRESH_TIMEOUT: Duration = Duration::from_millis(1200);
const SONOS_VOLUME_REFRESH_TIMEOUT: Duration = Duration::from_millis(1200);
const SONOS_POSITION_RESYNC_THRESHOLD_SECS: f64 = 1.1;
const SONOS_COMPLETION_RATIO: f64 = 0.95;
const SONOS_COMPLETION_TAIL_SECONDS: f64 = 2.0;
const SONOS_ENDED_RESET_POSITION_SECS: f64 = 1.0;
const SONOS_DESCRIPTION_TIMEOUT: Duration = Duration::from_secs(2);
const SONOS_DESCRIPTION_MAX_BYTES: usize = 256 * 1024;
// Sonos recommends roughly 1% FLAC seek resolution. Generated derivatives need
// this metadata because large REL_TIME seeks otherwise require fragile scans.
const SONOS_FLAC_SEEK_POINTS: usize = 100;
// Keep pre-fix derivatives from being reused after the encoded format changes.
const SONOS_TRANSCODE_CACHE_VERSION: &[u8] = b"seektable-v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SonosTarget {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub model: Option<String>,
    #[serde(default)]
    pub coordinator: bool,
    #[serde(default)]
    pub group_name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SonosSpeaker {
    pub target: SonosTarget,
    pub online: bool,
}

#[derive(Debug, Clone)]
pub struct SonosAsset {
    pub id: String,
    pub stream_url: String,
    pub mime_type: String,
    pub art_url: Option<String>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration_secs: Option<f64>,
    pub source_rate: u32,
    pub target_rate: u32,
    pub source_bits: u32,
    pub target_bits: u32,
}

#[derive(Debug, Clone)]
pub struct SonosPlaybackSnapshot {
    pub state: String,
    pub file_name: Option<String>,
    pub track_title: Option<String>,
    pub track_artist: Option<String>,
    pub track_album: Option<String>,
    pub position_secs: f64,
    pub duration_secs: f64,
    pub source_rate: u32,
    pub target_rate: u32,
    pub source_bits: u32,
    pub target_bits: u32,
    pub volume: Option<f32>,
    pub notice: Option<String>,
}

#[derive(Clone)]
pub enum SonosSource {
    LocalFile {
        path: PathBuf,
        tags: TrackTags,
        cover: Option<TrackCover>,
    },
    RemoteStream {
        id: String,
        stream_url: String,
        mime_type: String,
        art_url: Option<String>,
        tags: TrackTags,
        source_rate: u32,
        source_bits: u32,
    },
}

#[derive(Clone)]
struct CachedAsset {
    path: PathBuf,
    tokens: Vec<String>,
    art: Option<TrackCover>,
    expires_at: Instant,
}

#[derive(Clone)]
struct CachedRemoteStream {
    tokens: Vec<String>,
    scope: RemoteStreamScope,
    art: Option<TrackCover>,
    expires_at: Instant,
}

#[derive(Clone, PartialEq, Eq)]
enum RemoteStreamScope {
    Qobuz { track_id: u64, format_id: u32 },
}

#[derive(Debug, Clone)]
struct SonosSession {
    current: Option<SonosAsset>,
    queue: VecDeque<SonosAsset>,
    state: String,
    started_at: Option<Instant>,
    paused_position: f64,
    playback_polled_at: Option<Instant>,
    volume: Option<f32>,
    volume_polled_at: Option<Instant>,
    notice: Option<String>,
}

impl Default for SonosSession {
    fn default() -> Self {
        Self {
            current: None,
            queue: VecDeque::new(),
            state: "Stopped".to_string(),
            started_at: None,
            paused_position: 0.0,
            playback_polled_at: None,
            volume: None,
            volume_polled_at: None,
            notice: None,
        }
    }
}

#[derive(Debug, Clone, Default)]
struct SonosTransportSnapshot {
    state: Option<String>,
    current_uri: Option<String>,
    metadata: Option<SonosTrackMetadata>,
    position_secs: Option<f64>,
    duration_secs: Option<f64>,
}

#[derive(Debug, Clone, Default)]
struct SonosTrackMetadata {
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
    art_url: Option<String>,
    resource_uri: Option<String>,
}

impl SonosSession {
    fn replace_next(&mut self, asset: SonosAsset) {
        self.queue.clear();
        self.queue.push_back(asset);
    }

    fn append_next_if_tail_matches(&mut self, expected_tail_id: &str, asset: SonosAsset) -> bool {
        if self
            .queue
            .back()
            .is_some_and(|tail| tail.id == expected_tail_id)
        {
            self.queue.push_back(asset);
            return true;
        }
        false
    }

    fn set_volume(&mut self, volume: f32) {
        self.volume = Some(volume);
        self.volume_polled_at = Some(Instant::now());
    }
}

#[derive(Clone)]
pub struct SonosService {
    http: Client,
    public_base_url: String,
    cache_dir: PathBuf,
    speakers: Arc<Mutex<HashMap<String, SonosSpeaker>>>,
    assets: Arc<Mutex<HashMap<String, CachedAsset>>>,
    remote_streams: Arc<Mutex<HashMap<String, CachedRemoteStream>>>,
    sessions: Arc<Mutex<HashMap<String, SonosSession>>>,
    background_preparations: Arc<Mutex<HashSet<String>>>,
}

impl SonosService {
    pub fn new(cache_dir: PathBuf, public_base_url: String) -> Result<Self, std::io::Error> {
        std::fs::create_dir_all(&cache_dir)?;
        let service = Self {
            http: Client::new(),
            public_base_url,
            cache_dir,
            speakers: Arc::new(Mutex::new(HashMap::new())),
            assets: Arc::new(Mutex::new(HashMap::new())),
            remote_streams: Arc::new(Mutex::new(HashMap::new())),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            background_preparations: Arc::new(Mutex::new(HashSet::new())),
        };
        #[cfg(feature = "sonos")]
        service.spawn_discovery();
        Ok(service)
    }

    pub fn speakers(&self) -> Vec<SonosSpeaker> {
        self.speakers.lock().unwrap().values().cloned().collect()
    }

    pub async fn prepare_source(
        &self,
        source: SonosSource,
        filter_type: FilterType,
    ) -> Result<SonosAsset, String> {
        if let SonosSource::RemoteStream {
            id,
            stream_url,
            mime_type,
            art_url,
            tags,
            source_rate,
            source_bits,
        } = source
        {
            return Ok(SonosAsset {
                id,
                stream_url,
                mime_type: if mime_type.trim().is_empty() {
                    "audio/flac".to_string()
                } else {
                    mime_type
                },
                art_url,
                title: tags.title,
                artist: tags.artist,
                album: tags.album,
                duration_secs: tags.duration_secs,
                source_rate,
                target_rate: source_rate,
                source_bits,
                target_bits: source_bits,
            });
        }

        self.evict_expired_assets();
        let legacy_prepared = tokio::task::spawn_blocking({
            let cache_dir = self.cache_dir.clone();
            let source = source.clone();
            move || legacy_cached_transcode(&cache_dir, &source)
        })
        .await
        .map_err(|e| format!("inspect legacy Sonos asset task failed: {e}"))??;
        let prepared = if let Some(prepared) = legacy_prepared {
            self.spawn_background_prepare(source, filter_type);
            prepared
        } else {
            tokio::task::spawn_blocking({
                let cache_dir = self.cache_dir.clone();
                move || prepare_source_blocking(cache_dir, source, filter_type)
            })
            .await
            .map_err(|e| format!("prepare Sonos asset task failed: {e}"))??
        };

        let mut token = [0_u8; 18];
        OsRng.fill_bytes(&mut token);
        let token = URL_SAFE_NO_PAD.encode(token);
        let stream_url = format!(
            "{}/sonos/stream/{}?token={}",
            self.public_base_url.trim_end_matches('/'),
            prepared.id,
            token
        );
        let art_url = prepared.cover.as_ref().map(|_| {
            format!(
                "{}/sonos/art/{}?token={}",
                self.public_base_url.trim_end_matches('/'),
                prepared.id,
                token
            )
        });
        let expires_at = Instant::now() + SONOS_ASSET_TTL;
        let mut assets = self.assets.lock().unwrap();
        let cached = assets
            .entry(prepared.id.clone())
            .or_insert_with(|| CachedAsset {
                path: prepared.path.clone(),
                tokens: Vec::new(),
                art: None,
                expires_at,
            });
        cached.path = prepared.path.clone();
        cached.art = prepared.cover.clone();
        cached.expires_at = expires_at;
        cached.tokens.push(token.clone());

        Ok(SonosAsset {
            id: prepared.id,
            stream_url,
            mime_type: "audio/flac".to_string(),
            art_url,
            title: prepared.tags.title,
            artist: prepared.tags.artist,
            album: prepared.tags.album,
            duration_secs: prepared.tags.duration_secs,
            source_rate: prepared.source_rate,
            target_rate: prepared.target_rate,
            source_bits: prepared.source_bits,
            target_bits: prepared.target_bits,
        })
    }

    fn spawn_background_prepare(&self, source: SonosSource, filter_type: FilterType) {
        let SonosSource::LocalFile { path, .. } = &source else {
            return;
        };
        let preparation_key = path.to_string_lossy().to_string();
        if !self
            .background_preparations
            .lock()
            .unwrap()
            .insert(preparation_key.clone())
        {
            return;
        }
        let cache_dir = self.cache_dir.clone();
        let background_preparations = Arc::clone(&self.background_preparations);
        tokio::spawn(async move {
            let result = tokio::task::spawn_blocking(move || {
                prepare_source_blocking(cache_dir, source, filter_type)
            })
            .await;
            match result {
                Err(error) => eprintln!("sonos: background derivative task failed: {error}"),
                Ok(Err(error)) => {
                    eprintln!("sonos: background derivative preparation failed: {error}");
                }
                Ok(Ok(_)) => {}
            }
            background_preparations
                .lock()
                .unwrap()
                .remove(&preparation_key);
        });
    }

    pub fn register_qobuz_remote_stream(
        &self,
        track_id: u64,
        format_id: u32,
        art: Option<TrackCover>,
    ) -> (String, String) {
        let asset_id = format!("qobuz-{track_id}-{format_id}");
        self.evict_expired_remote_streams();
        let mut token = [0_u8; 18];
        OsRng.fill_bytes(&mut token);
        let token = URL_SAFE_NO_PAD.encode(token);
        let expires_at = Instant::now() + SONOS_ASSET_TTL;
        let mut remote_streams = self.remote_streams.lock().unwrap();
        let cached = remote_streams
            .entry(asset_id.clone())
            .or_insert_with(|| CachedRemoteStream {
                tokens: Vec::new(),
                scope: RemoteStreamScope::Qobuz {
                    track_id,
                    format_id,
                },
                art: None,
                expires_at,
            });
        cached.scope = RemoteStreamScope::Qobuz {
            track_id,
            format_id,
        };
        cached.expires_at = expires_at;
        if art.is_some() {
            cached.art = art;
        }
        cached.tokens.push(token.clone());
        (asset_id, token)
    }

    pub fn qobuz_remote_stream_token_valid(
        &self,
        asset_id: &str,
        token: &str,
        requested_track_id: u64,
        requested_format_id: u32,
    ) -> bool {
        self.remote_streams
            .lock()
            .unwrap()
            .get(asset_id)
            .is_some_and(|asset| {
                constant_time_token_matches(&asset.tokens, token)
                    && asset.expires_at > Instant::now()
                    && asset.scope
                        == RemoteStreamScope::Qobuz {
                            track_id: requested_track_id,
                            format_id: requested_format_id,
                        }
            })
    }

    pub fn asset_path_for_request(&self, asset_id: &str, token: &str) -> Option<PathBuf> {
        self.assets
            .lock()
            .unwrap()
            .get(asset_id)
            .filter(|asset| {
                constant_time_token_matches(&asset.tokens, token)
                    && asset.expires_at > Instant::now()
            })
            .map(|asset| asset.path.clone())
    }

    pub fn art_for_request(&self, asset_id: &str, token: &str) -> Option<TrackCover> {
        if let Some(cover) = self
            .assets
            .lock()
            .unwrap()
            .get(asset_id)
            .filter(|asset| {
                constant_time_token_matches(&asset.tokens, token)
                    && asset.expires_at > Instant::now()
            })
            .and_then(|asset| asset.art.clone())
        {
            return Some(cover);
        }

        self.remote_streams
            .lock()
            .unwrap()
            .get(asset_id)
            .filter(|asset| {
                constant_time_token_matches(&asset.tokens, token)
                    && asset.expires_at > Instant::now()
            })
            .and_then(|asset| asset.art.clone())
    }

    pub async fn play(
        &self,
        zone_id: &str,
        target: &SonosTarget,
        asset: SonosAsset,
    ) -> Result<(), String> {
        self.set_av_transport_uri(target, &asset, false).await?;
        self.play_transport(target).await?;
        let mut sessions = self.sessions.lock().unwrap();
        let session = sessions.entry(zone_id.to_string()).or_default();
        session.current = Some(asset);
        session.queue.clear();
        session.state = "Playing".to_string();
        session.started_at = Some(Instant::now());
        session.paused_position = 0.0;
        session.notice = None;
        Ok(())
    }

    pub async fn set_next(
        &self,
        zone_id: &str,
        target: &SonosTarget,
        asset: SonosAsset,
    ) -> Result<(), String> {
        self.set_av_transport_uri(target, &asset, true).await?;
        self.sessions
            .lock()
            .unwrap()
            .entry(zone_id.to_string())
            .or_default()
            .replace_next(asset);
        Ok(())
    }

    pub fn append_next_if_tail_matches(
        &self,
        zone_id: &str,
        expected_tail_id: &str,
        asset: SonosAsset,
    ) -> bool {
        self.sessions
            .lock()
            .unwrap()
            .get_mut(zone_id)
            .is_some_and(|session| session.append_next_if_tail_matches(expected_tail_id, asset))
    }

    pub fn queued_next_count(&self, zone_id: &str) -> usize {
        self.sessions
            .lock()
            .unwrap()
            .get(zone_id)
            .map(|session| session.queue.len())
            .unwrap_or(0)
    }

    pub async fn pause(&self, zone_id: &str, target: &SonosTarget) -> Result<(), String> {
        self.soap_action(
            target,
            "/MediaRenderer/AVTransport/Control",
            "urn:schemas-upnp-org:service:AVTransport:1",
            "Pause",
            "<InstanceID>0</InstanceID>",
        )
        .await?;
        if let Some(session) = self.sessions.lock().unwrap().get_mut(zone_id) {
            session.paused_position = session_position(session);
            session.started_at = None;
            session.state = "Paused".to_string();
        }
        Ok(())
    }

    pub async fn resume(&self, zone_id: &str, target: &SonosTarget) -> Result<(), String> {
        self.play_transport(target).await?;
        if let Some(session) = self.sessions.lock().unwrap().get_mut(zone_id) {
            session.started_at = Some(Instant::now());
            session.state = "Playing".to_string();
        }
        Ok(())
    }

    pub async fn stop(&self, zone_id: &str, target: &SonosTarget) -> Result<(), String> {
        self.soap_action(
            target,
            "/MediaRenderer/AVTransport/Control",
            "urn:schemas-upnp-org:service:AVTransport:1",
            "Stop",
            "<InstanceID>0</InstanceID>",
        )
        .await?;
        self.sessions
            .lock()
            .unwrap()
            .insert(zone_id.to_string(), SonosSession::default());
        Ok(())
    }

    pub async fn next(&self, zone_id: &str, target: &SonosTarget) -> Result<(), String> {
        self.soap_action(
            target,
            "/MediaRenderer/AVTransport/Control",
            "urn:schemas-upnp-org:service:AVTransport:1",
            "Next",
            "<InstanceID>0</InstanceID>",
        )
        .await?;
        let next_to_arm = {
            let mut sessions = self.sessions.lock().unwrap();
            let Some(session) = sessions.get_mut(zone_id) else {
                return Ok(());
            };
            if let Some(next) = session.queue.pop_front() {
                session.current = Some(next);
                session.started_at = Some(Instant::now());
                session.paused_position = 0.0;
                session.state = "Playing".to_string();
            }
            session.queue.front().cloned()
        };
        if let Some(next) = next_to_arm {
            self.set_av_transport_uri(target, &next, true).await?;
        }
        Ok(())
    }

    pub async fn refresh_playback_if_stale(
        &self,
        zone_id: &str,
        target: &SonosTarget,
        max_age: Duration,
    ) -> Result<(), String> {
        let should_poll = {
            let sessions = self.sessions.lock().unwrap();
            let Some(session) = sessions.get(zone_id) else {
                return Ok(());
            };
            session
                .playback_polled_at
                .is_none_or(|polled_at| polled_at.elapsed() >= max_age)
        };
        if !should_poll {
            return Ok(());
        }

        let result = tokio::time::timeout(
            SONOS_PLAYBACK_REFRESH_TIMEOUT,
            self.get_transport_snapshot(target),
        )
        .await
        .map_err(|_| "Sonos playback refresh timed out".to_string())?;

        let next_to_arm = {
            let mut sessions = self.sessions.lock().unwrap();
            let Some(session) = sessions.get_mut(zone_id) else {
                return result.map(|_| ());
            };
            session.playback_polled_at = Some(Instant::now());
            let transport = result?;
            let promoted = reconcile_session_with_transport(session, transport, Instant::now());
            promoted.then(|| session.queue.front().cloned()).flatten()
        };
        if let Some(next) = next_to_arm {
            self.set_av_transport_uri(target, &next, true).await?;
        }
        Ok(())
    }

    pub async fn seek(
        &self,
        zone_id: &str,
        target: &SonosTarget,
        seconds: f64,
    ) -> Result<(), String> {
        let target_time = format_hhmmss(seconds.max(0.0));
        let body = format!(
            "<InstanceID>0</InstanceID><Unit>REL_TIME</Unit><Target>{}</Target>",
            xml_escape(&target_time)
        );
        self.soap_action(
            target,
            "/MediaRenderer/AVTransport/Control",
            "urn:schemas-upnp-org:service:AVTransport:1",
            "Seek",
            &body,
        )
        .await?;
        if let Some(session) = self.sessions.lock().unwrap().get_mut(zone_id) {
            session.paused_position = seconds.max(0.0);
            session.started_at = (session.state == "Playing").then_some(Instant::now());
        }
        Ok(())
    }

    pub async fn set_volume(
        &self,
        zone_id: &str,
        target: &SonosTarget,
        volume: f32,
    ) -> Result<(), String> {
        let percent = (volume.clamp(0.0, 1.0) * 100.0).round() as u8;
        let body = format!(
            "<InstanceID>0</InstanceID><Channel>Master</Channel><DesiredVolume>{percent}</DesiredVolume>"
        );
        self.soap_action(
            target,
            "/MediaRenderer/RenderingControl/Control",
            "urn:schemas-upnp-org:service:RenderingControl:1",
            "SetVolume",
            &body,
        )
        .await?;
        self.sessions
            .lock()
            .unwrap()
            .entry(zone_id.to_string())
            .or_default()
            .set_volume(volume.clamp(0.0, 1.0));
        Ok(())
    }

    pub async fn refresh_volume_if_stale(
        &self,
        zone_id: &str,
        target: &SonosTarget,
        max_age: Duration,
    ) -> Result<(), String> {
        let should_poll = {
            let sessions = self.sessions.lock().unwrap();
            sessions
                .get(zone_id)
                .and_then(|session| session.volume_polled_at)
                .is_none_or(|polled_at| polled_at.elapsed() >= max_age)
        };
        if !should_poll {
            return Ok(());
        }

        let result =
            match tokio::time::timeout(SONOS_VOLUME_REFRESH_TIMEOUT, self.get_volume(target)).await
            {
                Ok(result) => result,
                Err(_) => Err("Sonos GetVolume timed out".to_string()),
            };
        let mut sessions = self.sessions.lock().unwrap();
        let session = sessions.entry(zone_id.to_string()).or_default();
        session.volume_polled_at = Some(Instant::now());
        match result {
            Ok(volume) => {
                session.volume = Some(volume);
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    pub fn snapshot(&self, zone_id: &str) -> Option<SonosPlaybackSnapshot> {
        let sessions = self.sessions.lock().unwrap();
        let session = sessions.get(zone_id)?;
        let current = session.current.clone();
        Some(SonosPlaybackSnapshot {
            state: session.state.clone(),
            file_name: current.as_ref().and_then(|asset| {
                asset
                    .title
                    .clone()
                    .or_else(|| Some(format!("sonos:{}", asset.id)))
            }),
            track_title: current.as_ref().and_then(|asset| asset.title.clone()),
            track_artist: current.as_ref().and_then(|asset| asset.artist.clone()),
            track_album: current.as_ref().and_then(|asset| asset.album.clone()),
            position_secs: session_position(session),
            duration_secs: current
                .as_ref()
                .and_then(|asset| asset.duration_secs)
                .unwrap_or(0.0),
            source_rate: current.as_ref().map(|asset| asset.source_rate).unwrap_or(0),
            target_rate: current.as_ref().map(|asset| asset.target_rate).unwrap_or(0),
            source_bits: current.as_ref().map(|asset| asset.source_bits).unwrap_or(0),
            target_bits: current.as_ref().map(|asset| asset.target_bits).unwrap_or(0),
            volume: session.volume,
            notice: session.notice.clone(),
        })
    }

    pub fn mark_notice(&self, zone_id: &str, notice: String) {
        self.sessions
            .lock()
            .unwrap()
            .entry(zone_id.to_string())
            .or_default()
            .notice = Some(notice);
    }

    fn spawn_discovery(&self) {
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        let speakers = Arc::clone(&self.speakers);
        let http = self.http.clone();
        tokio::spawn(async move {
            loop {
                if let Err(e) = discover_once(&http, Arc::clone(&speakers)).await {
                    eprintln!("sonos: discovery failed: {e}");
                }
                tokio::time::sleep(SONOS_DISCOVERY_INTERVAL).await;
            }
        });
    }

    async fn set_av_transport_uri(
        &self,
        target: &SonosTarget,
        asset: &SonosAsset,
        next: bool,
    ) -> Result<(), String> {
        let metadata = didl_metadata(asset);
        let action = if next {
            "SetNextAVTransportURI"
        } else {
            "SetAVTransportURI"
        };
        let uri_tag = if next { "NextURI" } else { "CurrentURI" };
        let meta_tag = if next {
            "NextURIMetaData"
        } else {
            "CurrentURIMetaData"
        };
        let body = format!(
            "<InstanceID>0</InstanceID><{uri_tag}>{}</{uri_tag}><{meta_tag}>{}</{meta_tag}>",
            xml_escape(&asset.stream_url),
            xml_escape(&metadata)
        );
        self.soap_action(
            target,
            "/MediaRenderer/AVTransport/Control",
            "urn:schemas-upnp-org:service:AVTransport:1",
            action,
            &body,
        )
        .await
        .map(|_| ())
    }

    async fn play_transport(&self, target: &SonosTarget) -> Result<(), String> {
        self.soap_action(
            target,
            "/MediaRenderer/AVTransport/Control",
            "urn:schemas-upnp-org:service:AVTransport:1",
            "Play",
            "<InstanceID>0</InstanceID><Speed>1</Speed>",
        )
        .await
        .map(|_| ())
    }

    async fn get_volume(&self, target: &SonosTarget) -> Result<f32, String> {
        let body = self
            .soap_action(
                target,
                "/MediaRenderer/RenderingControl/Control",
                "urn:schemas-upnp-org:service:RenderingControl:1",
                "GetVolume",
                "<InstanceID>0</InstanceID><Channel>Master</Channel>",
            )
            .await?;
        parse_volume_response(&body)
            .ok_or_else(|| "Sonos GetVolume response did not include CurrentVolume".to_string())
    }

    async fn get_transport_snapshot(
        &self,
        target: &SonosTarget,
    ) -> Result<SonosTransportSnapshot, String> {
        let transport_body = self
            .soap_action(
                target,
                "/MediaRenderer/AVTransport/Control",
                "urn:schemas-upnp-org:service:AVTransport:1",
                "GetTransportInfo",
                "<InstanceID>0</InstanceID>",
            )
            .await?;
        let position_body = self
            .soap_action(
                target,
                "/MediaRenderer/AVTransport/Control",
                "urn:schemas-upnp-org:service:AVTransport:1",
                "GetPositionInfo",
                "<InstanceID>0</InstanceID>",
            )
            .await?;

        Ok(SonosTransportSnapshot {
            state: tag_text(&transport_body, "CurrentTransportState")
                .as_deref()
                .map(sonos_state_label),
            current_uri: tag_text(&position_body, "TrackURI")
                .or_else(|| tag_text(&position_body, "CurrentURI"))
                .filter(|uri| !uri.trim().is_empty()),
            metadata: tag_text(&position_body, "TrackMetaData")
                .and_then(|metadata| parse_track_metadata(&metadata)),
            position_secs: tag_text(&position_body, "RelTime")
                .and_then(|time| parse_sonos_time(&time)),
            duration_secs: tag_text(&position_body, "TrackDuration")
                .and_then(|time| parse_sonos_time(&time)),
        })
    }

    async fn soap_action(
        &self,
        target: &SonosTarget,
        path: &str,
        service: &str,
        action: &str,
        inner: &str,
    ) -> Result<String, String> {
        let envelope = soap_envelope(service, action, inner);
        let url = format!("http://{}:{}{}", target.host, target.port, path);
        let response = self
            .http
            .post(url)
            .header("Content-Type", "text/xml; charset=\"utf-8\"")
            .header("SOAPACTION", format!("\"{service}#{action}\""))
            .body(envelope)
            .send()
            .await
            .map_err(|e| format!("Sonos SOAP {action} request failed: {e}"))?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(parse_soap_error(&body)
                .unwrap_or_else(|| format!("Sonos SOAP {action} failed with {status}")));
        }
        Ok(body)
    }

    fn evict_expired_assets(&self) {
        let now = Instant::now();
        let mut expired_paths = Vec::new();
        {
            let mut assets = self.assets.lock().unwrap();
            assets.retain(|_, asset| {
                let keep = asset.expires_at > now;
                if !keep && asset.path.starts_with(&self.cache_dir) {
                    expired_paths.push(asset.path.clone());
                }
                keep
            });
        }
        for path in expired_paths {
            let _ = std::fs::remove_file(path);
        }
    }

    fn evict_expired_remote_streams(&self) {
        let now = Instant::now();
        self.remote_streams
            .lock()
            .unwrap()
            .retain(|_, asset| asset.expires_at > now);
    }
}

pub fn is_sonos_device_name(name: &str) -> bool {
    name.trim_start().starts_with(SONOS_DEVICE_PREFIX)
}

pub fn target_device_name(target: &SonosTarget) -> String {
    let body = serde_json::to_vec(target).unwrap_or_default();
    format!("{SONOS_DEVICE_PREFIX}{}", URL_SAFE_NO_PAD.encode(body))
}

pub fn parse_target_device_name(name: &str) -> Option<SonosTarget> {
    let encoded = name.trim().strip_prefix(SONOS_DEVICE_PREFIX)?;
    let bytes = URL_SAFE_NO_PAD.decode(encoded.as_bytes()).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub fn receiver_zone_id(target_id: &str) -> String {
    format!("sonos-{target_id}")
}

pub fn sonos_target_rate_for_source(source_rate: u32) -> u32 {
    if source_rate == 0 || source_rate <= SONOS_SAMPLE_RATE {
        return source_rate.clamp(1, SONOS_SAMPLE_RATE);
    }
    if in_rate_family(source_rate, 44_100) {
        44_100
    } else {
        48_000
    }
}

pub fn parse_ssdp_response(response: &str) -> Option<String> {
    for line in response.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        if key.trim().eq_ignore_ascii_case("location") {
            let location = value.trim();
            if !location.is_empty() {
                return Some(location.to_string());
            }
        }
    }
    None
}

pub fn soap_envelope(service: &str, action: &str, inner: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?><s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/"><s:Body><u:{action} xmlns:u="{service}">{inner}</u:{action}></s:Body></s:Envelope>"#
    )
}

pub fn parse_soap_error(body: &str) -> Option<String> {
    let code = tag_text(body, "errorCode");
    let description = tag_text(body, "errorDescription");
    match (code, description) {
        (Some(code), Some(description)) => Some(format!("Sonos SOAP error {code}: {description}")),
        (Some(code), None) => Some(format!("Sonos SOAP error {code}")),
        _ => None,
    }
}

fn parse_volume_response(body: &str) -> Option<f32> {
    let percent = tag_text(body, "CurrentVolume")?.parse::<f32>().ok()?;
    Some((percent / 100.0).clamp(0.0, 1.0))
}

struct PreparedAsset {
    id: String,
    path: PathBuf,
    tags: TrackTags,
    cover: Option<TrackCover>,
    source_rate: u32,
    target_rate: u32,
    source_bits: u32,
    target_bits: u32,
}

fn prepare_source_blocking(
    cache_dir: PathBuf,
    source: SonosSource,
    filter_type: FilterType,
) -> Result<PreparedAsset, String> {
    match source {
        SonosSource::RemoteStream { .. } => {
            Err("remote Sonos streams do not require blocking preparation".to_string())
        }
        SonosSource::LocalFile { path, tags, cover } => {
            if is_dsd_path(&path) {
                return Err("Sonos does not support DSD sources".to_string());
            }
            if is_compliant_local_flac(&path)? {
                let info = inspect_media(
                    Box::new(File::open(&path).map_err(|e| e.to_string())?),
                    path.extension().and_then(|e| e.to_str()),
                )?;
                let id = asset_id_for_path(&path);
                return Ok(PreparedAsset {
                    id,
                    path,
                    tags: merge_info_into_tags(tags, &info),
                    cover,
                    source_rate: info.sample_rate,
                    target_rate: info.sample_rate,
                    source_bits: info.bits_per_sample,
                    target_bits: info.bits_per_sample,
                });
            }
            transcode_source(
                cache_dir,
                Box::new(File::open(&path).map_err(|e| e.to_string())?),
                path.extension().and_then(|e| e.to_str()),
                tags,
                cover,
                &path,
                inspect_media(
                    Box::new(File::open(&path).map_err(|e| e.to_string())?),
                    path.extension().and_then(|e| e.to_str()),
                )?,
                filter_type,
            )
        }
    }
}

fn legacy_cached_transcode(
    cache_dir: &Path,
    source: &SonosSource,
) -> Result<Option<PreparedAsset>, String> {
    let SonosSource::LocalFile { path, tags, cover } = source else {
        return Ok(None);
    };
    if is_dsd_path(path) || is_compliant_local_flac(path)? {
        return Ok(None);
    }
    let info = inspect_media(
        Box::new(File::open(path).map_err(|e| e.to_string())?),
        path.extension().and_then(|extension| extension.to_str()),
    )?;
    let target_rate = sonos_target_rate_for_source(info.sample_rate);
    let current_id = transcoded_asset_id(path, info.sample_rate, target_rate);
    if cache_dir.join(format!("{current_id}.flac")).exists() {
        return Ok(None);
    }
    let legacy_id = legacy_transcoded_asset_id(path, info.sample_rate, target_rate);
    let legacy_path = cache_dir.join(format!("{legacy_id}.flac"));
    if !legacy_path.exists() {
        return Ok(None);
    }
    Ok(Some(PreparedAsset {
        id: legacy_id,
        path: legacy_path,
        tags: merge_info_into_tags(tags.clone(), &info),
        cover: cover.clone(),
        source_rate: info.sample_rate,
        target_rate,
        source_bits: info.bits_per_sample,
        target_bits: 24,
    }))
}

// Sonos transcoding needs source data, metadata, cover, path, media info, and DSP policy together.
#[allow(clippy::too_many_arguments)]
fn transcode_source(
    cache_dir: PathBuf,
    source: Box<dyn MediaSource>,
    ext_hint: Option<&str>,
    fallback_tags: TrackTags,
    cover: Option<TrackCover>,
    source_path: &Path,
    source_info: MediaInfo,
    filter_type: FilterType,
) -> Result<PreparedAsset, String> {
    if source_info.channels == 0 || source_info.channels > 2 {
        return Err("Sonos supports mono or stereo sources only".to_string());
    }
    if source_info.bits_per_sample == 1 {
        return Err("Sonos does not support DSD sources".to_string());
    }
    let target_rate = sonos_target_rate_for_source(source_info.sample_rate);
    let id = transcoded_asset_id(source_path, source_info.sample_rate, target_rate);
    let path = cache_dir.join(format!("{id}.flac"));
    if path.exists() {
        return Ok(PreparedAsset {
            id,
            path,
            tags: merge_info_into_tags(fallback_tags, &source_info),
            cover,
            source_rate: source_info.sample_rate,
            target_rate,
            source_bits: source_info.bits_per_sample,
            target_bits: 24,
        });
    }

    let decoded = decode_to_interleaved(source, ext_hint, filter_type)?;
    encode_flac_file(&path, &decoded.samples, decoded.target_rate)?;
    Ok(PreparedAsset {
        id,
        path,
        tags: merge_info_into_tags(fallback_tags, &decoded.info),
        cover,
        source_rate: decoded.source_rate,
        target_rate: decoded.target_rate,
        source_bits: decoded.source_bits,
        target_bits: 24,
    })
}

struct MediaInfo {
    sample_rate: u32,
    bits_per_sample: u32,
    channels: u16,
    duration_secs: Option<f64>,
}

impl MediaInfo {
    fn is_sonos_compliant(&self) -> bool {
        self.channels > 0
            && self.channels <= 2
            && self.sample_rate <= SONOS_SAMPLE_RATE
            && self.bits_per_sample <= SONOS_BIT_DEPTH as u32
    }
}

struct DecodedPcm {
    samples: Vec<i32>,
    source_rate: u32,
    target_rate: u32,
    source_bits: u32,
    info: MediaInfo,
}

fn inspect_media(
    source: Box<dyn MediaSource>,
    ext_hint: Option<&str>,
) -> Result<MediaInfo, String> {
    let mut hint = Hint::new();
    if let Some(ext) = ext_hint {
        hint.with_extension(ext);
    }
    let mss = MediaSourceStream::new(source, Default::default());
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| format!("probe Sonos source: {e}"))?;
    let track = probed
        .format
        .tracks()
        .iter()
        .find(|track| track.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| "No playable audio track found".to_string())?;
    let channels = track
        .codec_params
        .channels
        .map(|channels| channels.count() as u16)
        .unwrap_or(2);
    let sample_rate = track.codec_params.sample_rate.unwrap_or(44_100);
    let bits = track.codec_params.bits_per_sample.unwrap_or(16);
    let duration_secs = track
        .codec_params
        .n_frames
        .map(|frames| frames as f64 / sample_rate.max(1) as f64);
    Ok(MediaInfo {
        sample_rate,
        bits_per_sample: bits,
        channels,
        duration_secs,
    })
}

fn decode_to_interleaved(
    source: Box<dyn MediaSource>,
    ext_hint: Option<&str>,
    filter_type: FilterType,
) -> Result<DecodedPcm, String> {
    let mut hint = Hint::new();
    if let Some(ext) = ext_hint {
        hint.with_extension(ext);
    }
    let mss = MediaSourceStream::new(source, Default::default());
    let mut probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| format!("probe Sonos source: {e}"))?;
    let track = probed
        .format
        .tracks()
        .iter()
        .find(|track| track.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| "No playable audio track found".to_string())?;
    let track_id = track.id;
    let source_rate = track.codec_params.sample_rate.unwrap_or(44_100);
    let source_bits = track.codec_params.bits_per_sample.unwrap_or(16);
    let channels = track
        .codec_params
        .channels
        .map(|channels| channels.count() as u16)
        .unwrap_or(2);
    if channels == 0 || channels > 2 {
        return Err("Sonos supports mono or stereo sources only".to_string());
    }
    if source_bits == 1 {
        return Err("Sonos does not support DSD sources".to_string());
    }
    let duration_secs = track
        .codec_params
        .n_frames
        .map(|frames| frames as f64 / source_rate.max(1) as f64);
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| format!("create Sonos decoder: {e}"))?;
    let target_rate = sonos_target_rate_for_source(source_rate);
    let mut resampler = (source_rate != target_rate)
        .then(|| SincResampler::new(filter_type, source_rate, target_rate));
    let mut rendered = Vec::new();
    let mut output = Vec::new();

    loop {
        match probed.format.next_packet() {
            Ok(packet) if packet.track_id() == track_id => {
                let decoded = decoder
                    .decode(&packet)
                    .map_err(|e| format!("decode Sonos source: {e}"))?;
                let mut sample_buf = symphonia::core::audio::AudioBuffer::<f64>::new(
                    decoded.capacity() as u64,
                    *decoded.spec(),
                );
                decoded.convert(&mut sample_buf);
                let planes = sample_buf.planes();
                let left = planes.planes()[0];
                let right = if planes.planes().len() > 1 {
                    planes.planes()[1]
                } else {
                    left
                };
                output.clear();
                let frames = if let Some(resampler) = resampler.as_mut() {
                    resampler.input(left, right);
                    resampler.process(&mut output)
                } else {
                    let frames = left.len().min(right.len());
                    output.reserve(frames * 2);
                    for idx in 0..frames {
                        output.push(left[idx]);
                        output.push(right[idx]);
                    }
                    frames
                };
                rendered.reserve(frames * 2);
                for sample in &output {
                    rendered.push(float_to_i24(*sample));
                }
            }
            Ok(_) => {}
            Err(symphonia::core::errors::Error::IoError(err))
                if err.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(e) => return Err(format!("read Sonos source packet: {e}")),
        }
    }

    Ok(DecodedPcm {
        samples: rendered,
        source_rate,
        target_rate,
        source_bits,
        info: MediaInfo {
            sample_rate: source_rate,
            bits_per_sample: source_bits,
            channels,
            duration_secs,
        },
    })
}

fn encode_flac_file(path: &Path, samples: &[i32], sample_rate: u32) -> Result<(), String> {
    if path.exists() {
        return Ok(());
    }
    let source = MemSource::from_samples(samples, 2, 24, sample_rate as usize);
    let config = config::Encoder::default()
        .into_verified()
        .map_err(|e| format!("verify FLAC encoder config: {e:?}"))?;
    let mut stream = flacenc::encode_with_fixed_block_size(&config, source, 4096)
        .map_err(|e| format!("encode FLAC for Sonos: {e}"))?;
    add_sonos_flac_seek_table(&mut stream)?;
    let mut sink = ByteSink::new();
    stream
        .write(&mut sink)
        .map_err(|e| format!("write FLAC bitstream: {e}"))?;
    write_cache_file_once(path, sink.into_inner())
}

fn add_sonos_flac_seek_table(stream: &mut Stream) -> Result<(), String> {
    let frame_count = stream.frame_count();
    if frame_count == 0 {
        return Ok(());
    }

    let mut sample_starts = Vec::with_capacity(frame_count);
    let mut byte_offsets = Vec::with_capacity(frame_count);
    let mut sample_start = 0_u64;
    let mut byte_offset = 0_u64;
    for index in 0..frame_count {
        let frame = stream
            .frame(index)
            .ok_or_else(|| "Sonos FLAC frame disappeared while building seek table".to_string())?;
        let frame_bits = frame.count_bits();
        if !frame_bits.is_multiple_of(8) {
            return Err("Sonos FLAC frame is not byte-aligned".to_string());
        }
        sample_starts.push(sample_start);
        byte_offsets.push(byte_offset);
        sample_start = sample_start.saturating_add(frame.block_size() as u64);
        byte_offset = byte_offset.saturating_add((frame_bits / 8) as u64);
    }

    let point_count = frame_count.min(SONOS_FLAC_SEEK_POINTS);
    let mut seek_table = Vec::with_capacity(point_count * 18);
    let mut previous_index = None;
    for point in 0..point_count {
        let index = point.saturating_mul(frame_count) / point_count;
        if previous_index == Some(index) {
            continue;
        }
        previous_index = Some(index);
        let frame = stream
            .frame(index)
            .ok_or_else(|| "Sonos FLAC seek point references a missing frame".to_string())?;
        let frame_samples = u16::try_from(frame.block_size())
            .map_err(|_| "Sonos FLAC frame is too large for a seek point".to_string())?;
        seek_table.extend_from_slice(&sample_starts[index].to_be_bytes());
        seek_table.extend_from_slice(&byte_offsets[index].to_be_bytes());
        seek_table.extend_from_slice(&frame_samples.to_be_bytes());
    }

    let metadata = MetadataBlockData::new_unknown(3, &seek_table)
        .map_err(|e| format!("build Sonos FLAC seek table: {e:?}"))?;
    stream.add_metadata_block(metadata);
    Ok(())
}

fn write_cache_file_once(path: &Path, bytes: Vec<u8>) -> Result<(), String> {
    let Some(parent) = path.parent() else {
        return Err("Sonos FLAC cache path is missing a parent directory".to_string());
    };
    let mut token = [0_u8; 12];
    OsRng.fill_bytes(&mut token);
    let temp_path = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("sonos-cache"),
        URL_SAFE_NO_PAD.encode(token)
    ));
    std::fs::write(&temp_path, bytes).map_err(|e| format!("write Sonos FLAC cache: {e}"))?;
    match std::fs::hard_link(&temp_path, path) {
        Ok(()) => {
            let _ = std::fs::remove_file(&temp_path);
            Ok(())
        }
        Err(e) if e.kind() == ErrorKind::AlreadyExists => {
            let _ = std::fs::remove_file(&temp_path);
            Ok(())
        }
        Err(e) => {
            let _ = std::fs::remove_file(&temp_path);
            if path.exists() {
                Ok(())
            } else {
                Err(format!("publish Sonos FLAC cache: {e}"))
            }
        }
    }
}

fn is_compliant_local_flac(path: &Path) -> Result<bool, String> {
    if !path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("flac"))
    {
        return Ok(false);
    }
    let info = inspect_media(
        Box::new(File::open(path).map_err(|e| e.to_string())?),
        Some("flac"),
    )?;
    Ok(info.is_sonos_compliant())
}

fn merge_info_into_tags(mut tags: TrackTags, info: &MediaInfo) -> TrackTags {
    if tags.sample_rate.is_none() {
        tags.sample_rate = Some(info.sample_rate);
    }
    if tags.bits_per_sample.is_none() {
        tags.bits_per_sample = Some(info.bits_per_sample);
    }
    if tags.channels.is_none() {
        tags.channels = Some(info.channels);
    }
    if tags.duration_secs.is_none() {
        tags.duration_secs = info.duration_secs;
    }
    tags
}

fn didl_metadata(asset: &SonosAsset) -> String {
    let title = asset.title.as_deref().unwrap_or("Track");
    let creator = asset.artist.as_deref().unwrap_or("");
    let album = asset.album.as_deref().unwrap_or("");
    let art = asset.art_url.as_deref().unwrap_or("");
    let duration_attr = asset
        .duration_secs
        .filter(|duration| duration.is_finite() && *duration > 0.0)
        .map(|duration| format!(r#" duration="{}""#, format_hhmmss(duration)))
        .unwrap_or_default();
    format!(
        r#"<DIDL-Lite xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:upnp="urn:schemas-upnp-org:metadata-1-0/upnp/" xmlns:r="urn:schemas-rinconnetworks-com:metadata-1-0/" xmlns="urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/"><item id="{}" parentID="0" restricted="true"><dc:title>{}</dc:title><dc:creator>{}</dc:creator><upnp:album>{}</upnp:album><upnp:albumArtURI>{}</upnp:albumArtURI><upnp:class>object.item.audioItem.musicTrack</upnp:class><res protocolInfo="http-get:*:{}:*"{}>{}</res></item></DIDL-Lite>"#,
        xml_escape(&asset.id),
        xml_escape(title),
        xml_escape(creator),
        xml_escape(album),
        xml_escape(art),
        xml_escape(&asset.mime_type),
        duration_attr,
        xml_escape(&asset.stream_url),
    )
}

async fn discover_once(
    http: &Client,
    speakers: Arc<Mutex<HashMap<String, SonosSpeaker>>>,
) -> Result<(), String> {
    let socket = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0))
        .await
        .map_err(|e| format!("bind SSDP socket: {e}"))?;
    for search_target in [
        "urn:schemas-upnp-org:device:ZonePlayer:1",
        "upnp:rootdevice",
        "ssdp:all",
    ] {
        let message = format!(
            "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 1\r\nST: {search_target}\r\n\r\n"
        );
        socket
            .send_to(
                message.as_bytes(),
                SocketAddrV4::new(Ipv4Addr::new(239, 255, 255, 250), 1900),
            )
            .await
            .map_err(|e| format!("send Sonos SSDP search for {search_target}: {e}"))?;
    }

    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    let mut buf = vec![0_u8; 4096];
    let mut seen_locations = HashSet::new();
    while tokio::time::Instant::now() < deadline {
        let timeout = deadline.saturating_duration_since(tokio::time::Instant::now());
        let received = tokio::time::timeout(timeout, socket.recv_from(&mut buf)).await;
        let (len, addr) = match received {
            Err(_) => break,
            Ok(Err(_)) => continue,
            Ok(Ok(received)) => received,
        };
        let body = String::from_utf8_lossy(&buf[..len]);
        let Some(location) = parse_ssdp_response(&body) else {
            continue;
        };
        if !seen_locations.insert(location.clone()) {
            continue;
        }
        if let Ok(target) = resolve_sonos_target(http, &location, addr.ip()).await {
            speakers.lock().unwrap().insert(
                target.id.clone(),
                SonosSpeaker {
                    target,
                    online: true,
                },
            );
        }
    }
    Ok(())
}

async fn resolve_sonos_target(
    http: &Client,
    location: &str,
    responder_ip: IpAddr,
) -> Result<SonosTarget, String> {
    let url = validate_sonos_location(location, responder_ip)?;
    let body = fetch_sonos_description(http, url.clone()).await?;
    let host = responder_ip.to_string();
    let port = url.port().unwrap_or(SONOS_CONTROL_PORT);
    let udn = tag_text(&body, "UDN").unwrap_or_else(|| format!("uuid:{host}"));
    let id = udn.trim_start_matches("uuid:").to_string();
    let name = tag_text(&body, "friendlyName").unwrap_or_else(|| format!("Sonos {host}"));
    let model = tag_text(&body, "modelName");
    let manufacturer = tag_text(&body, "manufacturer").unwrap_or_default();
    if !manufacturer.to_ascii_lowercase().contains("sonos") {
        return Err("SSDP device is not a Sonos speaker".to_string());
    }
    Ok(SonosTarget {
        id,
        name,
        host,
        port,
        model,
        coordinator: true,
        group_name: None,
    })
}

fn validate_sonos_location(location: &str, responder_ip: IpAddr) -> Result<Url, String> {
    let url = Url::parse(location).map_err(|e| format!("invalid Sonos location URL: {e}"))?;
    if url.scheme() != "http" {
        return Err("Sonos location URL must use http".to_string());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("Sonos location URL must not contain credentials".to_string());
    }
    let host = url
        .host_str()
        .ok_or_else(|| "Sonos location URL is missing a host".to_string())?;
    let host_ip: IpAddr = host
        .parse()
        .map_err(|_| "Sonos location host must be an IP address".to_string())?;
    if host_ip != responder_ip {
        return Err("Sonos location host must match the SSDP responder".to_string());
    }
    let port = url
        .port()
        .ok_or_else(|| format!("Sonos location port must be {SONOS_CONTROL_PORT}"))?;
    if port != SONOS_CONTROL_PORT {
        return Err(format!("Sonos location port must be {SONOS_CONTROL_PORT}"));
    }
    Ok(url)
}

async fn fetch_sonos_description(http: &Client, url: Url) -> Result<String, String> {
    let mut response = tokio::time::timeout(SONOS_DESCRIPTION_TIMEOUT, http.get(url).send())
        .await
        .map_err(|_| "fetch Sonos device description timed out".to_string())?
        .map_err(|e| format!("fetch Sonos device description: {e}"))?;

    if let Some(length) = response.content_length()
        && length > SONOS_DESCRIPTION_MAX_BYTES as u64
    {
        return Err("Sonos device description is too large".to_string());
    }

    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| format!("read Sonos device description: {e}"))?
    {
        if body.len().saturating_add(chunk.len()) > SONOS_DESCRIPTION_MAX_BYTES {
            return Err("Sonos device description is too large".to_string());
        }
        body.extend_from_slice(&chunk);
    }
    String::from_utf8(body).map_err(|e| format!("decode Sonos device description: {e}"))
}

fn tag_text(body: &str, tag: &str) -> Option<String> {
    if let Some(value) = tag_text_xml(body, tag) {
        return Some(value);
    }
    let start = format!("<{tag}>");
    let end = format!("</{tag}>");
    let start_idx = body.find(&start)? + start.len();
    let end_idx = body[start_idx..].find(&end)? + start_idx;
    Some(xml_unescape(&body[start_idx..end_idx]))
}

fn tag_text_xml(body: &str, tag: &str) -> Option<String> {
    let mut reader = Reader::from_str(body);
    let mut inside = false;
    let mut content = String::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(start)) if local_xml_name(start.name().as_ref()) == tag.as_bytes() => {
                inside = true;
                content.clear();
            }
            Ok(Event::Text(text)) if inside => {
                content.push_str(&text.xml10_content().ok()?);
            }
            Ok(Event::CData(text)) if inside => {
                content.push_str(&text.decode().ok()?);
            }
            Ok(Event::GeneralRef(reference)) if inside => {
                content.push_str(&xml_general_ref_text(&reference)?);
            }
            Ok(Event::End(end)) if local_xml_name(end.name().as_ref()) == tag.as_bytes() => {
                return Some(content.trim().to_string());
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    None
}

fn xml_general_ref_text(reference: &BytesRef<'_>) -> Option<String> {
    if let Some(character) = reference.resolve_char_ref().ok()? {
        return Some(character.to_string());
    }
    let value = match reference.decode().ok()?.as_ref() {
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "quot" => "\"",
        "apos" => "'",
        other => return Some(format!("&{other};")),
    };
    Some(value.to_string())
}

fn local_xml_name(name: &[u8]) -> &[u8] {
    name.rsplit(|byte| *byte == b':').next().unwrap_or(name)
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn xml_unescape(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn sonos_state_label(state: &str) -> String {
    match state.trim() {
        "PLAYING" | "TRANSITIONING" => "Playing",
        "PAUSED_PLAYBACK" | "PAUSED_RECORDING" => "Paused",
        "STOPPED" | "NO_MEDIA_PRESENT" => "Stopped",
        other if other.eq_ignore_ascii_case("playing") => "Playing",
        other if other.eq_ignore_ascii_case("paused") => "Paused",
        other if other.eq_ignore_ascii_case("stopped") => "Stopped",
        other => other,
    }
    .to_string()
}

fn parse_sonos_time(value: &str) -> Option<f64> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("NOT_IMPLEMENTED") {
        return None;
    }
    let mut parts = trimmed.split(':');
    let hours = parts.next()?.parse::<f64>().ok()?;
    let minutes = parts.next()?.parse::<f64>().ok()?;
    let seconds = parts.next()?.parse::<f64>().ok()?;
    if parts.next().is_some() || !hours.is_finite() || !minutes.is_finite() || !seconds.is_finite()
    {
        return None;
    }
    Some((hours * 3600.0) + (minutes * 60.0) + seconds)
}

fn parse_track_metadata(metadata: &str) -> Option<SonosTrackMetadata> {
    let metadata = metadata.trim();
    if metadata.is_empty() || metadata.eq_ignore_ascii_case("NOT_IMPLEMENTED") {
        return None;
    }
    let unescaped;
    let metadata = if metadata.contains("&lt;") {
        unescaped = xml_unescape(metadata);
        unescaped.as_str()
    } else {
        metadata
    };
    let parsed = SonosTrackMetadata {
        title: tag_text(metadata, "title"),
        artist: tag_text(metadata, "creator")
            .or_else(|| tag_text(metadata, "artist"))
            .or_else(|| tag_text(metadata, "albumArtist")),
        album: tag_text(metadata, "album"),
        art_url: tag_text(metadata, "albumArtURI"),
        resource_uri: tag_text(metadata, "res"),
    };
    (parsed.title.is_some()
        || parsed.artist.is_some()
        || parsed.album.is_some()
        || parsed.resource_uri.is_some())
    .then_some(parsed)
}

fn format_hhmmss(seconds: f64) -> String {
    let total = seconds.max(0.0).round() as u64;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    format!("{h:02}:{m:02}:{s:02}")
}

fn session_position(session: &SonosSession) -> f64 {
    session_position_at(session, Instant::now())
}

fn session_position_at(session: &SonosSession, now: Instant) -> f64 {
    session.paused_position
        + session
            .started_at
            .and_then(|started| now.checked_duration_since(started))
            .map(|elapsed| elapsed.as_secs_f64())
            .unwrap_or(0.0)
}

fn reconcile_session_with_transport(
    session: &mut SonosSession,
    transport: SonosTransportSnapshot,
    now: Instant,
) -> bool {
    let mut promoted = false;
    if let Some(state) = transport.state {
        session.state = state;
    }

    if let Some(uri) = transport.current_uri.as_deref() {
        promoted |= promote_session_to_uri(session, uri, now);
    }
    if let Some(metadata) = transport.metadata.as_ref() {
        promoted |= promote_session_to_metadata(session, metadata, now);
        apply_track_metadata(session, metadata);
    }

    if let (Some(current), Some(duration)) = (session.current.as_mut(), transport.duration_secs)
        && duration > 0.0
    {
        current.duration_secs = Some(duration);
    }

    let observed_position = transport.position_secs.map(|position| position.max(0.0));
    if let Some(position) = observed_position {
        let projected_position = session_position_at(session, now);
        if session.state != "Playing"
            && sonos_reset_position_after_completion(session, position, projected_position)
        {
            session.state = "Stopped".to_string();
            session.paused_position = session
                .current
                .as_ref()
                .and_then(|current| current.duration_secs)
                .unwrap_or(projected_position);
        } else {
            session.paused_position = if session.state == "Playing"
                && (position - projected_position).abs() < SONOS_POSITION_RESYNC_THRESHOLD_SECS
            {
                projected_position
            } else {
                position
            };
        }
    }
    if session.state == "Playing" {
        if observed_position.is_some() {
            session.started_at = Some(now);
        }
    } else {
        session.started_at = None;
    }
    promoted
}

fn sonos_reset_position_after_completion(
    session: &SonosSession,
    observed_position: f64,
    projected_position: f64,
) -> bool {
    if observed_position > SONOS_ENDED_RESET_POSITION_SECS {
        return false;
    }
    let Some(duration) = session
        .current
        .as_ref()
        .and_then(|current| current.duration_secs)
    else {
        return false;
    };
    sonos_position_is_complete(projected_position, duration)
        || sonos_position_is_complete(session.paused_position, duration)
}

fn sonos_position_is_complete(position: f64, duration: f64) -> bool {
    duration.is_finite()
        && position.is_finite()
        && duration > 0.0
        && (position >= duration * SONOS_COMPLETION_RATIO
            || duration - position <= SONOS_COMPLETION_TAIL_SECONDS)
}

fn promote_session_to_uri(session: &mut SonosSession, uri: &str, now: Instant) -> bool {
    if session
        .current
        .as_ref()
        .is_some_and(|current| asset_matches_uri(current, uri))
    {
        return false;
    }
    let Some(index) = session
        .queue
        .iter()
        .position(|asset| asset_matches_uri(asset, uri))
    else {
        return false;
    };
    let mut remaining = session.queue.split_off(index);
    if let Some(next) = remaining.pop_front() {
        session.current = Some(next);
        session.queue = remaining;
        session.paused_position = 0.0;
        session.started_at = Some(now);
        return true;
    }
    false
}

fn promote_session_to_metadata(
    session: &mut SonosSession,
    metadata: &SonosTrackMetadata,
    now: Instant,
) -> bool {
    if session
        .current
        .as_ref()
        .is_some_and(|current| asset_matches_metadata(current, metadata))
    {
        return false;
    }
    let Some(index) = session
        .queue
        .iter()
        .position(|asset| asset_matches_metadata(asset, metadata))
    else {
        return false;
    };
    let mut remaining = session.queue.split_off(index);
    if let Some(next) = remaining.pop_front() {
        session.current = Some(next);
        session.queue = remaining;
        session.paused_position = 0.0;
        session.started_at = Some(now);
        return true;
    }
    false
}

fn apply_track_metadata(session: &mut SonosSession, metadata: &SonosTrackMetadata) {
    let current = session.current.get_or_insert_with(|| SonosAsset {
        id: "sonos-current".to_string(),
        stream_url: metadata.resource_uri.clone().unwrap_or_default(),
        mime_type: "audio/flac".to_string(),
        art_url: None,
        title: None,
        artist: None,
        album: None,
        duration_secs: None,
        source_rate: 0,
        target_rate: 0,
        source_bits: 0,
        target_bits: 0,
    });
    if let Some(uri) = metadata
        .resource_uri
        .as_ref()
        .filter(|uri| !uri.trim().is_empty())
    {
        current.stream_url = uri.clone();
    }
    if let Some(title) = metadata
        .title
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        current.title = Some(title.clone());
    }
    if let Some(artist) = metadata
        .artist
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        current.artist = Some(artist.clone());
    }
    if let Some(album) = metadata
        .album
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        current.album = Some(album.clone());
    }
    if let Some(art_url) = metadata
        .art_url
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        current.art_url = Some(art_url.clone());
    }
}

fn asset_matches_uri(asset: &SonosAsset, uri: &str) -> bool {
    let uri = uri.trim();
    if uri.is_empty() {
        return false;
    }
    if uri == asset.stream_url {
        return true;
    }
    let uri_without_query = uri.split_once('?').map(|(base, _)| base).unwrap_or(uri);
    let stream_without_query = asset
        .stream_url
        .split_once('?')
        .map(|(base, _)| base)
        .unwrap_or(asset.stream_url.as_str());
    uri_without_query == stream_without_query
        || uri_without_query.ends_with(&format!("/sonos/stream/{}", asset.id))
}

fn asset_matches_metadata(asset: &SonosAsset, metadata: &SonosTrackMetadata) -> bool {
    if metadata
        .resource_uri
        .as_deref()
        .is_some_and(|uri| asset_matches_uri(asset, uri))
    {
        return true;
    }

    let Some(metadata_title) = metadata
        .title
        .as_deref()
        .and_then(normalized_metadata_value)
    else {
        return false;
    };
    let Some(asset_title) = asset.title.as_deref().and_then(normalized_metadata_value) else {
        return false;
    };
    if asset_title != metadata_title {
        return false;
    }

    metadata
        .artist
        .as_deref()
        .and_then(normalized_metadata_value)
        .is_none_or(|artist| {
            asset
                .artist
                .as_deref()
                .and_then(normalized_metadata_value)
                .is_none_or(|asset_artist| asset_artist == artist)
        })
        && metadata
            .album
            .as_deref()
            .and_then(normalized_metadata_value)
            .is_none_or(|album| {
                asset
                    .album
                    .as_deref()
                    .and_then(normalized_metadata_value)
                    .is_none_or(|asset_album| asset_album == album)
            })
}

fn normalized_metadata_value(value: &str) -> Option<String> {
    let normalized = value.trim().to_ascii_lowercase();
    (!normalized.is_empty()).then_some(normalized)
}

fn float_to_i24(sample: f64) -> i32 {
    let clamped = sample.clamp(-1.0, 1.0);
    let scaled = if clamped >= 0.0 {
        clamped * 8_388_607.0
    } else {
        clamped * 8_388_608.0
    };
    scaled.round() as i32
}

fn asset_id_for_path(path: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(path.to_string_lossy().as_bytes());
    URL_SAFE_NO_PAD.encode(&hasher.finalize()[..18])
}

fn transcoded_asset_id(path: &Path, source_rate: u32, target_rate: u32) -> String {
    let mut hasher = Sha256::new();
    hasher.update(SONOS_TRANSCODE_CACHE_VERSION);
    update_transcoded_asset_hash(&mut hasher, path, source_rate, target_rate);
    URL_SAFE_NO_PAD.encode(&hasher.finalize()[..18])
}

fn legacy_transcoded_asset_id(path: &Path, source_rate: u32, target_rate: u32) -> String {
    let mut hasher = Sha256::new();
    update_transcoded_asset_hash(&mut hasher, path, source_rate, target_rate);
    URL_SAFE_NO_PAD.encode(&hasher.finalize()[..18])
}

fn update_transcoded_asset_hash(
    hasher: &mut Sha256,
    path: &Path,
    source_rate: u32,
    target_rate: u32,
) {
    hasher.update(path.to_string_lossy().as_bytes());
    hasher.update(source_rate.to_le_bytes());
    hasher.update(target_rate.to_le_bytes());
}

fn is_dsd_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| matches!(ext.to_ascii_lowercase().as_str(), "dsf" | "dff"))
}

fn in_rate_family(source_rate: u32, base_rate: u32) -> bool {
    source_rate != 0
        && ((source_rate >= base_rate && source_rate.is_multiple_of(base_rate))
            || (source_rate < base_rate && base_rate.is_multiple_of(source_rate)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> SonosTarget {
        SonosTarget {
            id: "abc".to_string(),
            name: "Living Room".to_string(),
            host: "192.168.1.5".to_string(),
            port: 1400,
            model: Some("Sonos One".to_string()),
            coordinator: true,
            group_name: None,
        }
    }

    #[test]
    fn sonos_target_round_trips_through_device_name() {
        let encoded = target_device_name(&target());
        assert!(is_sonos_device_name(&encoded));
        assert_eq!(parse_target_device_name(&encoded), Some(target()));
    }

    #[test]
    fn ssdp_location_header_is_parsed_case_insensitively() {
        let response = "HTTP/1.1 200 OK\r\nLOCATION: http://192.168.1.5:1400/xml/device_description.xml\r\nST: urn:schemas-upnp-org:device:ZonePlayer:1\r\n\r\n";
        assert_eq!(
            parse_ssdp_response(response).as_deref(),
            Some("http://192.168.1.5:1400/xml/device_description.xml")
        );
    }

    #[test]
    fn sonos_location_must_match_ssdp_responder() {
        let responder = IpAddr::from([192, 168, 1, 5]);

        assert!(
            validate_sonos_location(
                "http://192.168.1.5:1400/xml/device_description.xml",
                responder
            )
            .is_ok()
        );
        assert!(
            validate_sonos_location(
                "http://127.0.0.1:1400/xml/device_description.xml",
                responder
            )
            .is_err()
        );
        assert!(
            validate_sonos_location("http://169.254.169.254:1400/latest/meta-data/", responder)
                .is_err()
        );
    }

    #[test]
    fn sonos_location_rejects_unexpected_scheme_host_and_port() {
        let responder = IpAddr::from([192, 168, 1, 5]);

        assert!(
            validate_sonos_location(
                "https://192.168.1.5:1400/xml/device_description.xml",
                responder
            )
            .is_err()
        );
        assert!(
            validate_sonos_location(
                "http://sonos.local:1400/xml/device_description.xml",
                responder
            )
            .is_err()
        );
        assert!(
            validate_sonos_location(
                "http://192.168.1.5:8080/xml/device_description.xml",
                responder
            )
            .is_err()
        );
    }

    #[test]
    fn soap_envelope_contains_action_and_body() {
        let envelope = soap_envelope(
            "urn:schemas-upnp-org:service:AVTransport:1",
            "Play",
            "<InstanceID>0</InstanceID><Speed>1</Speed>",
        );
        assert!(envelope.contains("<u:Play"));
        assert!(envelope.contains("<Speed>1</Speed>"));
    }

    #[test]
    fn soap_fault_is_summarized() {
        let body = "<s:Envelope><s:Body><s:Fault><detail><UPnPError><errorCode>701</errorCode><errorDescription>Transition not available</errorDescription></UPnPError></detail></s:Fault></s:Body></s:Envelope>";
        assert_eq!(
            parse_soap_error(body).as_deref(),
            Some("Sonos SOAP error 701: Transition not available")
        );
    }

    #[test]
    fn volume_response_is_parsed_as_normalized_value() {
        let body = "<s:Envelope><s:Body><u:GetVolumeResponse><CurrentVolume>37</CurrentVolume></u:GetVolumeResponse></s:Body></s:Envelope>";
        assert_eq!(parse_volume_response(body), Some(0.37));
    }

    #[test]
    fn xml_tag_text_unescapes_entities() {
        let body =
            "<s:Envelope><s:Body><dc:title>M &amp; M &lt;Mix&gt;</dc:title></s:Body></s:Envelope>";
        assert_eq!(tag_text(body, "title").as_deref(), Some("M & M <Mix>"));
    }

    #[test]
    fn remote_stream_registration_preserves_active_tokens() {
        let cache_dir = unique_test_dir("sonos-remote-tokens");
        let service = SonosService::new(cache_dir.clone(), "http://core.test".to_string()).unwrap();

        let (asset_id, first) = service.register_qobuz_remote_stream(111, 6, None);
        let (second_asset_id, second) = service.register_qobuz_remote_stream(111, 6, None);

        assert_eq!(asset_id, "qobuz-111-6");
        assert_eq!(second_asset_id, asset_id);
        assert!(service.qobuz_remote_stream_token_valid(&asset_id, &first, 111, 6));
        assert!(service.qobuz_remote_stream_token_valid(&asset_id, &second, 111, 6));
        let _ = std::fs::remove_dir_all(cache_dir);
    }

    #[test]
    fn remote_stream_tokens_are_bound_to_qobuz_track_and_format() {
        let cache_dir = unique_test_dir("sonos-remote-scope");
        let service = SonosService::new(cache_dir.clone(), "http://core.test".to_string()).unwrap();
        let cover = TrackCover {
            mime: "image/png".to_string(),
            data: vec![1, 2, 3],
        };

        let (asset_id, token) = service.register_qobuz_remote_stream(111, 6, Some(cover.clone()));

        assert!(service.qobuz_remote_stream_token_valid(&asset_id, &token, 111, 6));
        assert!(!service.qobuz_remote_stream_token_valid(&asset_id, &token, 222, 6));
        assert!(!service.qobuz_remote_stream_token_valid(&asset_id, &token, 111, 7));
        assert!(!service.qobuz_remote_stream_token_valid(&asset_id, "", 111, 6));
        assert!(!service.qobuz_remote_stream_token_valid(&asset_id, "wrong-token", 111, 6));
        assert_eq!(
            service
                .art_for_request(&asset_id, &token)
                .map(|art| art.data),
            Some(cover.data)
        );

        service
            .remote_streams
            .lock()
            .unwrap()
            .get_mut(&asset_id)
            .unwrap()
            .expires_at = Instant::now() - Duration::from_secs(1);
        assert!(!service.qobuz_remote_stream_token_valid(&asset_id, &token, 111, 6));
        assert!(service.art_for_request(&asset_id, &token).is_none());
        let _ = std::fs::remove_dir_all(cache_dir);
    }

    #[test]
    fn cache_publish_does_not_overwrite_existing_file() {
        let cache_dir = unique_test_dir("sonos-cache-publish");
        std::fs::create_dir_all(&cache_dir).unwrap();
        let path = cache_dir.join("asset.flac");
        std::fs::write(&path, b"active stream").unwrap();

        write_cache_file_once(&path, b"new transcode".to_vec()).unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), b"active stream");
        let _ = std::fs::remove_dir_all(cache_dir);
    }

    #[test]
    fn sonos_time_is_parsed_to_seconds() {
        assert_eq!(parse_sonos_time("01:02:03"), Some(3723.0));
        assert_eq!(parse_sonos_time("00:00:03.5"), Some(3.5));
        assert_eq!(parse_sonos_time("NOT_IMPLEMENTED"), None);
    }

    #[test]
    fn track_metadata_is_parsed_from_escaped_didl() {
        let asset = test_asset("second", "Second");
        let metadata = xml_escape(&didl_metadata(&asset));
        let parsed = parse_track_metadata(&metadata).unwrap();

        assert_eq!(parsed.title.as_deref(), Some("Second"));
        assert_eq!(parsed.artist.as_deref(), Some("Artist"));
        assert_eq!(parsed.album.as_deref(), Some("Album"));
        assert_eq!(
            parsed.resource_uri.as_deref(),
            Some(asset.stream_url.as_str())
        );
    }

    #[test]
    fn didl_metadata_includes_resource_duration_when_known() {
        let asset = SonosAsset {
            duration_secs: Some(3723.0),
            ..test_asset("duration", "Duration")
        };
        let metadata = didl_metadata(&asset);

        assert!(metadata.contains(r#"duration="01:02:03""#));
    }

    #[test]
    fn transport_refresh_promotes_prefetched_next_track_by_uri() {
        let now = Instant::now();
        let first = test_asset("first", "First");
        let second = test_asset("second", "Second");
        let second_uri = second.stream_url.clone();
        let mut session = SonosSession {
            current: Some(first),
            queue: VecDeque::from([second]),
            state: "Playing".to_string(),
            started_at: Some(now),
            paused_position: 0.0,
            playback_polled_at: Some(now),
            volume: None,
            volume_polled_at: None,
            notice: None,
        };

        reconcile_session_with_transport(
            &mut session,
            SonosTransportSnapshot {
                state: Some("Playing".to_string()),
                current_uri: Some(second_uri),
                metadata: None,
                position_secs: Some(1.25),
                duration_secs: Some(180.0),
            },
            now,
        );

        assert_eq!(
            session
                .current
                .as_ref()
                .and_then(|asset| asset.title.as_deref()),
            Some("Second")
        );
        assert!(session.queue.is_empty());
        assert_eq!(session.paused_position, 1.25);
        assert!(session_position(&session) < 1.35);
    }

    #[test]
    fn transport_refresh_keeps_follow_up_ready_after_promotion() {
        let now = Instant::now();
        let first = test_asset("first", "First");
        let second = test_asset("second", "Second");
        let third = test_asset("third", "Third");
        let second_uri = second.stream_url.clone();
        let mut session = SonosSession {
            current: Some(first),
            queue: VecDeque::from([second, third]),
            state: "Playing".to_string(),
            started_at: Some(now),
            paused_position: 0.0,
            playback_polled_at: Some(now),
            volume: None,
            volume_polled_at: None,
            notice: None,
        };

        let promoted = reconcile_session_with_transport(
            &mut session,
            SonosTransportSnapshot {
                state: Some("Playing".to_string()),
                current_uri: Some(second_uri),
                metadata: None,
                position_secs: Some(0.5),
                duration_secs: Some(180.0),
            },
            now,
        );

        assert!(promoted);
        assert_eq!(
            session
                .current
                .as_ref()
                .and_then(|asset| asset.title.as_deref()),
            Some("Second")
        );
        assert_eq!(session.queue.len(), 1);
        assert_eq!(
            session
                .queue
                .front()
                .and_then(|asset| asset.title.as_deref()),
            Some("Third")
        );
    }

    #[test]
    fn transport_refresh_does_not_promote_without_current_uri() {
        let now = Instant::now();
        let first = test_asset("first", "First");
        let second = test_asset("second", "Second");
        let mut session = SonosSession {
            current: Some(SonosAsset {
                duration_secs: Some(30.0),
                ..first
            }),
            queue: VecDeque::from([second]),
            state: "Playing".to_string(),
            started_at: now.checked_sub(Duration::from_secs(32)),
            paused_position: 0.0,
            playback_polled_at: Some(now),
            volume: None,
            volume_polled_at: None,
            notice: None,
        };

        reconcile_session_with_transport(
            &mut session,
            SonosTransportSnapshot {
                state: Some("Playing".to_string()),
                current_uri: None,
                metadata: None,
                position_secs: Some(2.0),
                duration_secs: Some(180.0),
            },
            now,
        );

        assert_eq!(
            session
                .current
                .as_ref()
                .and_then(|asset| asset.title.as_deref()),
            Some("First")
        );
        assert_eq!(session.queue.len(), 1);
        assert_eq!(session.paused_position, 2.0);
        assert!(session_position(&session) < 2.1);
    }

    #[test]
    fn transport_refresh_keeps_smooth_position_for_coarse_sonos_time() {
        let now = Instant::now();
        let first = test_asset("first", "First");
        let started_at = now.checked_sub(Duration::from_millis(760)).unwrap();
        let mut session = SonosSession {
            current: Some(first),
            queue: VecDeque::new(),
            state: "Playing".to_string(),
            started_at: Some(started_at),
            paused_position: 12.0,
            playback_polled_at: Some(started_at),
            volume: None,
            volume_polled_at: None,
            notice: None,
        };

        reconcile_session_with_transport(
            &mut session,
            SonosTransportSnapshot {
                state: Some("Playing".to_string()),
                current_uri: None,
                metadata: None,
                position_secs: Some(12.0),
                duration_secs: Some(180.0),
            },
            now,
        );

        assert!((session.paused_position - 12.76).abs() < 0.01);
        assert!((session_position_at(&session, now) - 12.76).abs() < 0.01);
    }

    #[test]
    fn transport_refresh_preserves_completed_position_when_sonos_resets_to_start() {
        let now = Instant::now();
        let first = SonosAsset {
            duration_secs: Some(180.0),
            ..test_asset("first", "First")
        };
        let mut session = SonosSession {
            current: Some(first),
            queue: VecDeque::new(),
            state: "Playing".to_string(),
            started_at: now.checked_sub(Duration::from_secs(181)),
            paused_position: 0.0,
            playback_polled_at: Some(now),
            volume: None,
            volume_polled_at: None,
            notice: None,
        };

        reconcile_session_with_transport(
            &mut session,
            SonosTransportSnapshot {
                state: Some("Paused".to_string()),
                current_uri: None,
                metadata: None,
                position_secs: Some(0.0),
                duration_secs: Some(180.0),
            },
            now,
        );

        assert_eq!(session.state, "Stopped");
        assert_eq!(session.paused_position, 180.0);
        assert!(session.started_at.is_none());
    }

    #[test]
    fn transport_refresh_keeps_early_pause_at_start() {
        let now = Instant::now();
        let first = SonosAsset {
            duration_secs: Some(180.0),
            ..test_asset("first", "First")
        };
        let mut session = SonosSession {
            current: Some(first),
            queue: VecDeque::new(),
            state: "Playing".to_string(),
            started_at: now.checked_sub(Duration::from_secs(3)),
            paused_position: 0.0,
            playback_polled_at: Some(now),
            volume: None,
            volume_polled_at: None,
            notice: None,
        };

        reconcile_session_with_transport(
            &mut session,
            SonosTransportSnapshot {
                state: Some("Paused".to_string()),
                current_uri: None,
                metadata: None,
                position_secs: Some(0.0),
                duration_secs: Some(180.0),
            },
            now,
        );

        assert_eq!(session.state, "Paused");
        assert_eq!(session.paused_position, 0.0);
        assert!(session.started_at.is_none());
    }

    #[test]
    fn transport_refresh_promotes_prefetched_next_track_by_metadata() {
        let now = Instant::now();
        let first = test_asset("first", "First");
        let second = test_asset("second", "Second");
        let mut session = SonosSession {
            current: Some(first),
            queue: VecDeque::from([second]),
            state: "Playing".to_string(),
            started_at: Some(now),
            paused_position: 0.0,
            playback_polled_at: Some(now),
            volume: None,
            volume_polled_at: None,
            notice: None,
        };

        reconcile_session_with_transport(
            &mut session,
            SonosTransportSnapshot {
                state: Some("Playing".to_string()),
                current_uri: Some("x-rincon-queue:RINCON_TEST#0".to_string()),
                metadata: Some(SonosTrackMetadata {
                    title: Some("Second".to_string()),
                    artist: Some("Artist".to_string()),
                    album: Some("Album".to_string()),
                    art_url: None,
                    resource_uri: None,
                }),
                position_secs: Some(3.0),
                duration_secs: Some(181.0),
            },
            now,
        );

        let current = session.current.as_ref().unwrap();
        assert_eq!(current.title.as_deref(), Some("Second"));
        assert_eq!(current.duration_secs, Some(181.0));
        assert!(session.queue.is_empty());
    }

    #[test]
    fn transport_refresh_applies_sonos_metadata_when_queue_match_fails() {
        let now = Instant::now();
        let first = test_asset("first", "First");
        let mut session = SonosSession {
            current: Some(first),
            queue: VecDeque::new(),
            state: "Playing".to_string(),
            started_at: Some(now),
            paused_position: 0.0,
            playback_polled_at: Some(now),
            volume: None,
            volume_polled_at: None,
            notice: None,
        };

        reconcile_session_with_transport(
            &mut session,
            SonosTransportSnapshot {
                state: Some("Playing".to_string()),
                current_uri: Some("x-rincon-queue:RINCON_TEST#0".to_string()),
                metadata: Some(SonosTrackMetadata {
                    title: Some("Speaker Truth".to_string()),
                    artist: Some("Actual Artist".to_string()),
                    album: Some("Actual Album".to_string()),
                    art_url: None,
                    resource_uri: None,
                }),
                position_secs: Some(4.0),
                duration_secs: Some(90.0),
            },
            now,
        );

        let current = session.current.as_ref().unwrap();
        assert_eq!(current.title.as_deref(), Some("Speaker Truth"));
        assert_eq!(current.artist.as_deref(), Some("Actual Artist"));
        assert_eq!(current.album.as_deref(), Some("Actual Album"));
        assert_eq!(current.duration_secs, Some(90.0));
    }

    #[test]
    fn sonos_rate_policy_keeps_clock_family_under_limit() {
        assert_eq!(sonos_target_rate_for_source(44_100), 44_100);
        assert_eq!(sonos_target_rate_for_source(48_000), 48_000);
        assert_eq!(sonos_target_rate_for_source(88_200), 44_100);
        assert_eq!(sonos_target_rate_for_source(176_400), 44_100);
        assert_eq!(sonos_target_rate_for_source(96_000), 48_000);
        assert_eq!(sonos_target_rate_for_source(192_000), 48_000);
    }

    #[test]
    fn sonos_source_prep_uses_selected_long_filter_for_downsampling() {
        let target_rate = sonos_target_rate_for_source(96_000);
        let resampler = SincResampler::new(FilterType::SplitPhase128kE3, 96_000, target_rate);

        assert_eq!(target_rate, 48_000);
        assert_eq!(resampler.source_rate(), 96_000);
        assert_eq!(resampler.target_rate(), 48_000);
        assert_eq!(resampler.filter_type(), FilterType::SplitPhase128kE3);
        assert!(resampler.is_high_latency());
    }

    #[test]
    fn generated_sonos_flac_has_evenly_spaced_seek_table() {
        let cache_dir = unique_test_dir("sonos-flac-seektable");
        std::fs::create_dir_all(&cache_dir).unwrap();
        let path = cache_dir.join("hi-res-derivative.flac");
        let frame_count = 101;
        let samples = vec![0_i32; 4096 * 2 * frame_count];

        encode_flac_file(&path, &samples, 48_000).unwrap();

        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[..4], b"fLaC");
        let stream_info = flac_metadata_block(&bytes, 0).expect("STREAMINFO metadata block");
        assert_eq!(
            u16::from_be_bytes(stream_info[0..2].try_into().unwrap()),
            4096
        );
        assert_eq!(
            u16::from_be_bytes(stream_info[2..4].try_into().unwrap()),
            4096
        );
        assert!(u24(&stream_info[4..7]) > 0);
        assert!(u24(&stream_info[7..10]) > 0);
        let seek_table = flac_metadata_block(&bytes, 3).expect("SEEKTABLE metadata block");
        assert_eq!(seek_table.len(), SONOS_FLAC_SEEK_POINTS * 18);

        let points = seek_table
            .chunks_exact(18)
            .map(|point| {
                let sample = u64::from_be_bytes(point[0..8].try_into().unwrap());
                let offset = u64::from_be_bytes(point[8..16].try_into().unwrap());
                let samples = u16::from_be_bytes(point[16..18].try_into().unwrap());
                (sample, offset, samples)
            })
            .collect::<Vec<_>>();
        assert_eq!(points.first(), Some(&(0, 0, 4096)));
        assert!(
            points.windows(2).all(|pair| {
                pair[0].0 < pair[1].0 && pair[0].1 < pair[1].1 && pair[1].2 == 4096
            })
        );

        let _ = std::fs::remove_dir_all(cache_dir);
    }

    #[test]
    fn wav_sources_use_the_seekable_sonos_derivative_path() {
        assert!(!is_compliant_local_flac(Path::new("local-track.wav")).unwrap());
        assert!(!is_compliant_local_flac(Path::new("hi-res-track.WAV")).unwrap());
    }

    #[test]
    fn legacy_derivative_is_returned_while_seekable_upgrade_is_missing() {
        let cache_dir = unique_test_dir("sonos-legacy-migration");
        std::fs::create_dir_all(&cache_dir).unwrap();
        let source_path = cache_dir.join("source-96k.flac");
        encode_flac_file(&source_path, &vec![0_i32; 4096 * 2], 96_000).unwrap();
        let legacy_id = legacy_transcoded_asset_id(&source_path, 96_000, 48_000);
        let current_id = transcoded_asset_id(&source_path, 96_000, 48_000);
        assert_ne!(legacy_id, current_id);
        std::fs::write(cache_dir.join(format!("{legacy_id}.flac")), b"legacy").unwrap();
        let source = SonosSource::LocalFile {
            path: source_path,
            tags: TrackTags::default(),
            cover: None,
        };

        let prepared = legacy_cached_transcode(&cache_dir, &source)
            .unwrap()
            .expect("legacy derivative");

        assert_eq!(prepared.id, legacy_id);
        assert_eq!(prepared.target_rate, 48_000);
        let _ = std::fs::remove_dir_all(cache_dir);
    }

    fn u24(bytes: &[u8]) -> usize {
        (usize::from(bytes[0]) << 16) | (usize::from(bytes[1]) << 8) | usize::from(bytes[2])
    }

    fn flac_metadata_block(bytes: &[u8], wanted_type: u8) -> Option<&[u8]> {
        let mut cursor = 4_usize;
        while cursor.checked_add(4)? <= bytes.len() {
            let header = bytes[cursor];
            let is_last = header & 0x80 != 0;
            let block_type = header & 0x7f;
            let len = (usize::from(bytes[cursor + 1]) << 16)
                | (usize::from(bytes[cursor + 2]) << 8)
                | usize::from(bytes[cursor + 3]);
            let start = cursor.checked_add(4)?;
            let end = start.checked_add(len)?;
            let block = bytes.get(start..end)?;
            if block_type == wanted_type {
                return Some(block);
            }
            if is_last {
                return None;
            }
            cursor = end;
        }
        None
    }

    fn test_asset(id: &str, title: &str) -> SonosAsset {
        SonosAsset {
            id: id.to_string(),
            stream_url: format!("http://core.test/sonos/stream/{id}?token=abc"),
            mime_type: "audio/flac".to_string(),
            art_url: None,
            title: Some(title.to_string()),
            artist: Some("Artist".to_string()),
            album: Some("Album".to_string()),
            duration_secs: Some(180.0),
            source_rate: 44_100,
            target_rate: 44_100,
            source_bits: 16,
            target_bits: 16,
        }
    }

    fn unique_test_dir(prefix: &str) -> PathBuf {
        let mut token = [0_u8; 12];
        OsRng.fill_bytes(&mut token);
        std::env::temp_dir().join(format!("{prefix}-{}", URL_SAFE_NO_PAD.encode(token)))
    }
}
