use super::{AIRPLAY_BIT_DEPTH, AIRPLAY_SAMPLE_RATE, AirPlayTarget};
use crate::compat::{ArtworkData, AtomicPlayerState, AudioConsumer, DitherPreference, DitherState};
use crate::pcm;
use aes::cipher::{BlockEncryptMut, KeyIvInit, block_padding::NoPadding};
use alac_encoder::{AlacEncoder, FormatDescription};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use cbc::Encryptor;
use rand::{RngCore, rngs::OsRng};
use rsa::{Oaep, RsaPublicKey, pkcs1::DecodeRsaPublicKey};
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};
use x25519_dalek::{PublicKey as X25519PublicKey, StaticSecret};

const FRAME_SIZE: usize = 352;
const CHANNELS: usize = 2;
const SSRC: u32 = 0x5541_5053;
const INITIAL_SEQ: u16 = 7;
const INITIAL_TIMESTAMP: u32 = 0;
const AIRPLAY_VOLUME_MIN_DB: f32 = -30.0;
const AIRPLAY_VOLUME_POLL_INTERVAL: Duration = Duration::from_secs(2);
const AIRPLAY_USER_AGENT: &str = "AirPlay/366.0";
const RTSP_SOCKET_POLL_TIMEOUT: Duration = Duration::from_millis(250);
const RTSP_DEFAULT_RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);
const RTSP_SETUP_RESPONSE_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_RTSP_RESPONSE_BODY_BYTES: usize = 1024 * 1024;

type Aes128CbcEnc = Encryptor<aes::Aes128>;
#[cfg(test)]
type TestRtspRequest = (String, HashMap<String, String>, Vec<u8>);

// Public half of the well-known AirPort Express RAOP RSA key. RAOP receivers use
// this only to unwrap a random AES session key for the RTP audio packets.
const AIRPORT_PUBLIC_KEY: &str = r#"-----BEGIN RSA PUBLIC KEY-----
MIIBCgKCAQEA59dE8qLieItsH1WgjrcFRKj6eUWqi+bGLOX1HL3U3GhC/j0Qg90u
3sG/1CUtwC5vOYvfDmFI6oSFXi5ELabWJmT2dKHzBJKa3k9ok+8t9ucRqMd6DZHJ
2YCCLlDRKSKv6kDqnw4UwPdpOMXziC/AMj3Z/lUVX1G7WSHCAWKf1zNS1eLvqr+b
oEjXuBOitnZ/bDzPHrTOZz0Dew0uowxf/+sG+NCK3eQJVxqcaJ/vEHKIVd2M+5qL
71yJQ+87X6oV3eaYvt3zWZYD6z5vYTcrtij2VZ9Zmni/UAaHqn9JdsBWLUEpVviY
nhimNVvYFZeCXg/IdTQ+x4IRdiXNv5hEewIDAQAB
-----END RSA PUBLIC KEY-----"#;

#[derive(Clone, Copy)]
struct RtpEncryption {
    aes_key: [u8; 16],
    aes_iv: [u8; 16],
}

#[derive(Clone, Default)]
pub struct AirPlayMetadata {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub artwork: Option<ArtworkData>,
}

enum StreamCommand {
    SetVolume(f32),
    SetMetadata(AirPlayMetadata),
    Stop,
}

pub struct AirPlayStream {
    tx: mpsc::Sender<StreamCommand>,
    done: Arc<AtomicBool>,
    ended: Arc<AtomicBool>,
}

impl AirPlayStream {
    pub fn set_volume(&self, volume: f32) {
        let volume = super::device_volume_to_transport_volume(volume);
        let _ = self.tx.send(StreamCommand::SetVolume(volume));
    }

    pub fn set_metadata(&self, metadata: AirPlayMetadata) {
        let _ = self.tx.send(StreamCommand::SetMetadata(metadata));
    }

    pub fn reset_requested(&self) -> bool {
        self.ended.load(Ordering::Relaxed)
    }
}

impl Drop for AirPlayStream {
    fn drop(&mut self) {
        let _ = self.tx.send(StreamCommand::Stop);
        self.done.store(true, Ordering::Relaxed);
    }
}

pub fn open(
    target: AirPlayTarget,
    cons: AudioConsumer,
    state: Arc<AtomicPlayerState>,
    metadata: AirPlayMetadata,
    volume_state: Arc<AtomicU32>,
    initial_volume: Option<f32>,
) -> Result<AirPlayStream, Box<dyn std::error::Error>> {
    if let Some(reason) = target.unsupported_reason() {
        return Err(reason.into());
    }

    let mut session = RtspSession::connect(&target)?;
    let local_addr = session.local_addr()?;
    session.request("OPTIONS", "*", &[], None)?;
    if target.requires_mfi_auth_setup() {
        session.auth_setup()?;
    }
    let (sdp, rtp_encryption) = if target.uses_rsa_encryption() {
        let mut encryption = RtpEncryption {
            aes_key: [0u8; 16],
            aes_iv: [0u8; 16],
        };
        OsRng.fill_bytes(&mut encryption.aes_key);
        OsRng.fill_bytes(&mut encryption.aes_iv);
        let encrypted_key = encrypt_airplay_key(&encryption.aes_key)?;
        (
            build_sdp(
                local_addr,
                &target.host,
                &session.stream_token,
                Some((&encrypted_key, &encryption.aes_iv)),
            ),
            Some(encryption),
        )
    } else {
        (
            build_sdp(local_addr, &target.host, &session.stream_token, None),
            None,
        )
    };
    session.request(
        "ANNOUNCE",
        &session.url.clone(),
        &[("Content-Type", "application/sdp")],
        Some(sdp.into_bytes()),
    )?;
    let rtp_sockets = RtpSockets::bind(local_addr)?;
    let transport = rtp_sockets.transport_header()?;
    let setup = session.request(
        "SETUP",
        &session.url.clone(),
        &[("Transport", transport.as_str())],
        None,
    )?;
    let data_port = parse_server_port(&setup.headers)
        .ok_or("AirPlay receiver did not return an RTP server_port")?;
    session.request(
        "RECORD",
        &session.url.clone(),
        &[
            ("Range", "npt=0-"),
            (
                "RTP-Info",
                &format!("seq={INITIAL_SEQ};rtptime={INITIAL_TIMESTAMP}"),
            ),
        ],
        None,
    )?;
    if let Some(initial_volume) = initial_volume {
        let _ = session.set_volume(super::device_volume_to_transport_volume(initial_volume));
    }
    let volume_readback_enabled = match session.get_volume() {
        Ok(Some(volume)) => {
            volume_state.store(
                super::transport_volume_to_device_volume(volume).to_bits(),
                Ordering::Relaxed,
            );
            true
        }
        Ok(None) | Err(_) => false,
    };
    let _ = session.set_metadata(&metadata);

    let (tx, rx) = mpsc::channel();
    let done = Arc::new(AtomicBool::new(false));
    let thread_done = Arc::clone(&done);
    let ended = Arc::new(AtomicBool::new(false));
    let thread_ended = Arc::clone(&ended);
    let host = target.host.clone();
    thread::Builder::new()
        .name(format!("AirPlayStream-{}", target.name))
        .spawn(move || {
            if let Err(e) = run_stream(
                session,
                host,
                data_port,
                rtp_sockets,
                cons,
                state,
                volume_state,
                rx,
                rtp_encryption,
                thread_done,
                volume_readback_enabled,
            ) {
                eprintln!("airplay: stream ended with error: {e}");
            }
            thread_ended.store(true, Ordering::Relaxed);
        })?;

    Ok(AirPlayStream { tx, done, ended })
}

// Legacy AirPlay streaming owns RTSP, RTP, audio, volume, and shutdown state in one worker.
#[allow(clippy::too_many_arguments)]
fn run_stream(
    mut session: RtspSession,
    host: String,
    data_port: u16,
    rtp_sockets: RtpSockets,
    mut cons: AudioConsumer,
    state: Arc<AtomicPlayerState>,
    volume_state: Arc<AtomicU32>,
    rx: mpsc::Receiver<StreamCommand>,
    rtp_encryption: Option<RtpEncryption>,
    done: Arc<AtomicBool>,
    mut volume_readback_enabled: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let udp = rtp_sockets.audio;
    udp.connect((host.as_str(), data_port))?;
    let input_format = FormatDescription::pcm::<i16>(AIRPLAY_SAMPLE_RATE as f64, CHANNELS as u32);
    let output_format = FormatDescription::alac(
        AIRPLAY_SAMPLE_RATE as f64,
        FRAME_SIZE as u32,
        CHANNELS as u32,
    );
    let mut encoder = AlacEncoder::new(&output_format);
    let mut pcm = vec![0u8; FRAME_SIZE * CHANNELS * 2];
    let mut pcm_i16 = Vec::with_capacity(FRAME_SIZE * CHANNELS);
    let mut samples = vec![0.0f64; FRAME_SIZE * CHANNELS];
    let mut encoded = vec![0u8; output_format.max_packet_size()];
    let mut packet = Vec::with_capacity(12 + output_format.max_packet_size());
    let mut dither_state = DitherState::new(target_dither_seed(&host, data_port));
    let mut seq = INITIAL_SEQ;
    let mut timestamp = INITIAL_TIMESTAMP;
    let packet_duration = Duration::from_secs_f64(FRAME_SIZE as f64 / AIRPLAY_SAMPLE_RATE as f64);
    let mut next_packet_at = Instant::now();
    let mut next_volume_poll = Instant::now() + AIRPLAY_VOLUME_POLL_INTERVAL;

    state.exclusive.store(false, Ordering::Relaxed);
    state
        .target_rate
        .store(AIRPLAY_SAMPLE_RATE, Ordering::Relaxed);
    state
        .target_bits
        .store(AIRPLAY_BIT_DEPTH as u32, Ordering::Relaxed);

    while !done.load(Ordering::Relaxed) {
        while let Ok(cmd) = rx.try_recv() {
            match cmd {
                StreamCommand::SetVolume(volume) => {
                    if session.set_volume(volume).is_ok() {
                        volume_state.store(
                            super::transport_volume_to_device_volume(volume).to_bits(),
                            Ordering::Relaxed,
                        );
                    }
                }
                StreamCommand::SetMetadata(metadata) => {
                    let _ = session.set_metadata(&metadata);
                }
                StreamCommand::Stop => {
                    let _ = session.teardown();
                    return Ok(());
                }
            }
        }

        if volume_readback_enabled && Instant::now() >= next_volume_poll {
            match session.get_volume() {
                Ok(Some(volume)) => {
                    volume_state.store(
                        super::transport_volume_to_device_volume(volume).to_bits(),
                        Ordering::Relaxed,
                    );
                }
                Ok(None) | Err(_) => {
                    volume_readback_enabled = false;
                }
            }
            next_volume_poll = Instant::now() + AIRPLAY_VOLUME_POLL_INTERVAL;
        }

        if state.flush_buffer.swap(false, Ordering::Relaxed) {
            cons.clear();
            let _ = session.flush(seq);
        }

        if state.state.load(Ordering::Relaxed) != 1 {
            next_packet_at = Instant::now();
            thread::sleep(Duration::from_millis(20));
            continue;
        }

        let read = cons.pop_slice(&mut samples);
        if read < samples.len() {
            let missing = (samples.len() - read) as u64;
            let previous = state.underrun_events.fetch_add(1, Ordering::Relaxed);
            state.underrun_samples.fetch_add(missing, Ordering::Relaxed);
            if previous == 0 || (previous + 1).is_power_of_two() {
                eprintln!(
                    "airplay: underrun #{}, missing {} samples",
                    previous + 1,
                    missing
                );
            }
            for sample in &mut samples[read..] {
                *sample = 0.0;
            }
        }

        let volume = f32::from_bits(state.volume.load(Ordering::Relaxed)) as f64;
        let dither = DitherPreference::from_id(state.dither_mode.load(Ordering::Relaxed))
            .unwrap_or(DitherPreference::Auto);
        let (max_l, max_r) = pcm::quantize_interleaved_i16(
            &samples,
            volume,
            dither,
            &mut dither_state,
            &mut pcm_i16,
        );
        for (i, sample) in pcm_i16.iter().enumerate() {
            pcm[i * 2..i * 2 + 2].copy_from_slice(&sample.to_le_bytes());
        }
        state
            .meter_l
            .store((max_l as f32).to_bits(), Ordering::Relaxed);
        state
            .meter_r
            .store((max_r as f32).to_bits(), Ordering::Relaxed);

        let encoded_len = encoder.encode(&input_format, &pcm, &mut encoded);
        packet.clear();
        write_rtp_header(&mut packet, seq, timestamp);
        if let Some(encryption) = &rtp_encryption {
            packet.extend_from_slice(&encrypted_payload(
                &encoded[..encoded_len],
                &encryption.aes_key,
                &encryption.aes_iv,
            ));
        } else {
            packet.extend_from_slice(&encoded[..encoded_len]);
        }
        udp.send(&packet)?;
        seq = seq.wrapping_add(1);
        timestamp = timestamp.wrapping_add(FRAME_SIZE as u32);
        state
            .position_samples
            .fetch_add(FRAME_SIZE as u64, Ordering::Relaxed);

        next_packet_at += packet_duration;
        let now = Instant::now();
        if next_packet_at > now {
            thread::sleep(next_packet_at - now);
        } else if now.duration_since(next_packet_at) > Duration::from_millis(100) {
            next_packet_at = now;
        }
    }

    let _ = session.teardown();
    Ok(())
}

fn target_dither_seed(host: &str, port: u16) -> u64 {
    let mut seed = 0xcbf2_9ce4_8422_2325u64;
    for byte in host.as_bytes().iter().copied().chain(port.to_be_bytes()) {
        seed ^= byte as u64;
        seed = seed.wrapping_mul(0x100_0000_01b3);
    }
    seed
}

fn write_rtp_header(out: &mut Vec<u8>, seq: u16, timestamp: u32) {
    out.push(0x80);
    out.push(0x60);
    out.extend_from_slice(&seq.to_be_bytes());
    out.extend_from_slice(&timestamp.to_be_bytes());
    out.extend_from_slice(&SSRC.to_be_bytes());
}

fn encrypted_payload(payload: &[u8], key: &[u8; 16], iv: &[u8; 16]) -> Vec<u8> {
    let encrypted_len = (payload.len() / 16) * 16;
    let mut out = payload.to_vec();
    if encrypted_len > 0 {
        let mut encrypted = out[..encrypted_len].to_vec();
        let cipher = Aes128CbcEnc::new(key.into(), iv.into());
        let _ = cipher.encrypt_padded_mut::<NoPadding>(&mut encrypted, encrypted_len);
        out[..encrypted_len].copy_from_slice(&encrypted);
    }
    out
}

fn build_sdp(
    local_addr: IpAddr,
    target_addr: &str,
    stream_token: &str,
    encryption: Option<(&[u8], &[u8; 16])>,
) -> String {
    let (addr_family, addr) = match local_addr {
        IpAddr::V4(addr) => ("IP4", addr.to_string()),
        IpAddr::V6(addr) => ("IP6", addr.to_string()),
    };
    let (target_family, target_addr) = sdp_addr_parts(target_addr);
    let mut sdp = format!(
        concat!(
            "v=0\r\n",
            "o=iTunes {stream_token} 0 IN {addr_family} {addr}\r\n",
            "s={session_name}\r\n",
            "c=IN {target_family} {target_addr}\r\n",
            "t=0 0\r\n",
            "m=audio 0 RTP/AVP 96\r\n",
            "a=rtpmap:96 AppleLossless\r\n",
            "a=fmtp:96 {frame_size} 0 16 40 10 14 2 255 0 0 {sample_rate}\r\n",
        ),
        stream_token = stream_token,
        addr_family = addr_family,
        addr = addr,
        session_name = "Fozmo AirPlay Helper",
        target_family = target_family,
        target_addr = target_addr,
        frame_size = FRAME_SIZE,
        sample_rate = AIRPLAY_SAMPLE_RATE,
    );
    if let Some((encrypted_key, aes_iv)) = encryption {
        sdp.push_str(&format!(
            "a=rsaaeskey:{}\r\na=aesiv:{}\r\n",
            STANDARD.encode(encrypted_key),
            STANDARD.encode(aes_iv)
        ));
    }
    sdp
}

fn sdp_addr_parts(addr: &str) -> (&'static str, String) {
    let addr = addr
        .trim_matches(|ch| ch == '[' || ch == ']')
        .split_once('%')
        .map(|(addr, _)| addr)
        .unwrap_or(addr)
        .to_string();
    if addr.contains(':') {
        ("IP6", addr)
    } else {
        ("IP4", addr)
    }
}

fn encrypt_airplay_key(aes_key: &[u8; 16]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let public_key = RsaPublicKey::from_pkcs1_pem(AIRPORT_PUBLIC_KEY)?;
    let encrypted = public_key.encrypt(&mut OsRng, Oaep::new::<sha1::Sha1>(), aes_key)?;
    Ok(encrypted)
}

pub fn airplay_volume_db(volume: f32) -> f32 {
    let volume = volume.clamp(0.0, 1.0);
    if volume <= 0.0 {
        -144.0
    } else {
        AIRPLAY_VOLUME_MIN_DB + (volume * -AIRPLAY_VOLUME_MIN_DB)
    }
}

fn airplay_volume_from_db(db: f32) -> Option<f32> {
    if !db.is_finite() {
        return None;
    }
    if db <= AIRPLAY_VOLUME_MIN_DB {
        Some(0.0)
    } else if db >= 0.0 {
        Some(1.0)
    } else {
        Some(((db - AIRPLAY_VOLUME_MIN_DB) / -AIRPLAY_VOLUME_MIN_DB).clamp(0.0, 1.0))
    }
}

fn parse_airplay_volume_parameter(body: &[u8]) -> Option<f32> {
    let text = std::str::from_utf8(body).ok()?;
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        if let Some((key, value)) = line.split_once(':') {
            if key.trim().eq_ignore_ascii_case("volume") {
                return value
                    .trim()
                    .parse::<f32>()
                    .ok()
                    .and_then(airplay_volume_from_db);
            }
        } else if let Ok(db) = line.parse::<f32>() {
            return airplay_volume_from_db(db);
        }
    }
    None
}

fn dmap_atom(tag: &[u8; 4], payload: &[u8], out: &mut Vec<u8>) {
    out.extend_from_slice(tag);
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
}

fn dmap_string(tag: &[u8; 4], value: &Option<String>, out: &mut Vec<u8>) {
    if let Some(value) = value.as_deref().filter(|value| !value.is_empty()) {
        dmap_atom(tag, value.as_bytes(), out);
    }
}

fn metadata_dmap(metadata: &AirPlayMetadata) -> Vec<u8> {
    let mut inner = Vec::new();
    dmap_string(b"minm", &metadata.title, &mut inner);
    dmap_string(b"asar", &metadata.artist, &mut inner);
    dmap_string(b"asal", &metadata.album, &mut inner);
    let mut out = Vec::new();
    dmap_atom(b"mlit", &inner, &mut out);
    out
}

#[derive(Debug)]
struct RtspResponse {
    status: u16,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

struct RtpSockets {
    audio: UdpSocket,
    control: UdpSocket,
    timing: UdpSocket,
}

impl RtpSockets {
    fn bind(local_addr: IpAddr) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self {
            audio: bind_udp(local_addr)?,
            control: bind_udp(local_addr)?,
            timing: bind_udp(local_addr)?,
        })
    }

    fn transport_header(&self) -> Result<String, Box<dyn std::error::Error>> {
        Ok(format!(
            "RTP/AVP/UDP;unicast;mode=record;client_port={};control_port={};timing_port={}",
            self.audio.local_addr()?.port(),
            self.control.local_addr()?.port(),
            self.timing.local_addr()?.port(),
        ))
    }
}

fn bind_udp(local_addr: IpAddr) -> Result<UdpSocket, Box<dyn std::error::Error>> {
    Ok(UdpSocket::bind(SocketAddr::new(local_addr, 0))?)
}

struct RtspSession {
    stream: TcpStream,
    cseq: u32,
    session_id: Option<String>,
    url: String,
    stream_token: String,
    client_instance: String,
    active_remote: String,
}

impl RtspSession {
    fn connect(target: &AirPlayTarget) -> Result<Self, Box<dyn std::error::Error>> {
        let addr = (target.host.as_str(), target.port)
            .to_socket_addrs()?
            .next()
            .ok_or("AirPlay receiver address did not resolve")?;
        let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(5))?;
        stream.set_read_timeout(Some(RTSP_SOCKET_POLL_TIMEOUT))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        let stream_token = OsRng.next_u32().to_string();
        let client_instance = format!("{:016X}", OsRng.next_u64());
        let active_remote = OsRng.next_u32().to_string();
        let url_host = rtsp_host(&target.host);
        Ok(Self {
            stream,
            cseq: 1,
            session_id: None,
            url: format!("rtsp://{}:{}/{}", url_host, target.port, stream_token),
            stream_token,
            client_instance,
            active_remote,
        })
    }

    fn local_addr(&self) -> Result<IpAddr, Box<dyn std::error::Error>> {
        Ok(self.stream.local_addr()?.ip())
    }

    fn auth_setup(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let secret = StaticSecret::random_from_rng(OsRng);
        let public = X25519PublicKey::from(&secret);
        let mut body = Vec::with_capacity(33);
        body.push(0x01);
        body.extend_from_slice(public.as_bytes());
        self.request(
            "POST",
            "/auth-setup",
            &[("Content-Type", "application/octet-stream")],
            Some(body),
        )?;
        Ok(())
    }

    fn request(
        &mut self,
        method: &str,
        uri: &str,
        headers: &[(&str, &str)],
        body: Option<Vec<u8>>,
    ) -> Result<RtspResponse, Box<dyn std::error::Error>> {
        let body = body.unwrap_or_default();
        let mut req = format!("{method} {uri} RTSP/1.0\r\nCSeq: {}\r\n", self.cseq);
        self.cseq += 1;
        if let Some(session_id) = &self.session_id {
            req.push_str(&format!("Session: {session_id}\r\n"));
        }
        req.push_str(&format!("User-Agent: {AIRPLAY_USER_AGENT}\r\n"));
        req.push_str(&format!("Client-Instance: {}\r\n", self.client_instance));
        req.push_str(&format!("DACP-ID: {}\r\n", self.client_instance));
        req.push_str(&format!("Active-Remote: {}\r\n", self.active_remote));
        for (key, value) in headers {
            req.push_str(key);
            req.push_str(": ");
            req.push_str(value);
            req.push_str("\r\n");
        }
        if !body.is_empty() {
            req.push_str(&format!("Content-Length: {}\r\n", body.len()));
        }
        req.push_str("\r\n");
        self.stream
            .write_all(req.as_bytes())
            .map_err(|e| format!("AirPlay RTSP {method} request write failed: {e}"))?;
        if !body.is_empty() {
            self.stream
                .write_all(&body)
                .map_err(|e| format!("AirPlay RTSP {method} body write failed: {e}"))?;
        }
        self.stream
            .flush()
            .map_err(|e| format!("AirPlay RTSP {method} flush failed: {e}"))?;

        let response = read_rtsp_response(&mut self.stream, rtsp_response_timeout(method))
            .map_err(|e| format!("AirPlay RTSP {method} response read failed: {e}"))?;
        if !(200..300).contains(&response.status) {
            return Err(format!("AirPlay RTSP {method} failed with {}", response.status).into());
        }
        if let Some(session) = response.headers.get("session") {
            let id = session.split(';').next().unwrap_or(session).trim();
            if !id.is_empty() {
                self.session_id = Some(id.to_string());
            }
        }
        Ok(response)
    }

    fn set_volume(&mut self, volume: f32) -> Result<(), Box<dyn std::error::Error>> {
        let body = format!("volume: {:.6}\r\n", airplay_volume_db(volume));
        self.request(
            "SET_PARAMETER",
            &self.url.clone(),
            &[("Content-Type", "text/parameters")],
            Some(body.into_bytes()),
        )?;
        Ok(())
    }

    fn get_volume(&mut self) -> Result<Option<f32>, Box<dyn std::error::Error>> {
        let response = self.request(
            "GET_PARAMETER",
            &self.url.clone(),
            &[("Content-Type", "text/parameters")],
            Some(b"volume\r\n".to_vec()),
        )?;
        Ok(parse_airplay_volume_parameter(&response.body))
    }

    fn set_metadata(
        &mut self,
        metadata: &AirPlayMetadata,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let dmap = metadata_dmap(metadata);
        if !dmap.is_empty() {
            self.request(
                "SET_PARAMETER",
                &self.url.clone(),
                &[("Content-Type", "application/x-dmap-tagged")],
                Some(dmap),
            )?;
        }
        if let Some(artwork) = &metadata.artwork {
            self.request(
                "SET_PARAMETER",
                &self.url.clone(),
                &[("Content-Type", artwork.mime.as_str())],
                Some(artwork.data.clone()),
            )?;
        }
        Ok(())
    }

    fn flush(&mut self, seq: u16) -> Result<(), Box<dyn std::error::Error>> {
        self.request(
            "FLUSH",
            &self.url.clone(),
            &[("RTP-Info", &format!("seq={seq}"))],
            None,
        )?;
        Ok(())
    }

    fn teardown(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.request("TEARDOWN", &self.url.clone(), &[], None)?;
        Ok(())
    }
}

fn rtsp_host(host: &str) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_string()
    }
}

fn rtsp_response_timeout(method: &str) -> Duration {
    if method.eq_ignore_ascii_case("SETUP") {
        RTSP_SETUP_RESPONSE_TIMEOUT
    } else {
        RTSP_DEFAULT_RESPONSE_TIMEOUT
    }
}

fn read_rtsp_response(
    stream: &mut TcpStream,
    timeout: Duration,
) -> Result<RtspResponse, Box<dyn std::error::Error>> {
    let deadline = Instant::now() + timeout;
    let mut head = Vec::new();
    let mut buf = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        read_exact_until(stream, &mut buf, deadline)?;
        head.push(buf[0]);
        if head.len() > 64 * 1024 {
            return Err("AirPlay RTSP response header is too large".into());
        }
    }
    let head_text = String::from_utf8_lossy(&head);
    let mut lines = head_text.split("\r\n");
    let status_line = lines.next().ok_or("AirPlay RTSP response is empty")?;
    let status = status_line
        .split_whitespace()
        .nth(1)
        .ok_or("AirPlay RTSP response has no status")?
        .parse::<u16>()?;
    let mut headers = HashMap::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if let Some((key, value)) = line.split_once(':') {
            headers.insert(key.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }
    let content_len = match headers.get("content-length") {
        Some(value) => value
            .parse::<usize>()
            .map_err(|_| format!("AirPlay RTSP response has invalid Content-Length: {value}"))?,
        None => 0,
    };
    if content_len > MAX_RTSP_RESPONSE_BODY_BYTES {
        return Err(format!("AirPlay RTSP response body is too large: {content_len} bytes").into());
    }
    let mut body = Vec::new();
    body.try_reserve_exact(content_len)
        .map_err(|_| "AirPlay RTSP response body allocation failed")?;
    body.resize(content_len, 0);
    if content_len > 0 {
        read_exact_until(stream, &mut body, deadline)?;
    }
    Ok(RtspResponse {
        status,
        headers,
        body,
    })
}

fn read_exact_until(
    stream: &mut TcpStream,
    mut buf: &mut [u8],
    deadline: Instant,
) -> Result<(), Box<dyn std::error::Error>> {
    while !buf.is_empty() {
        match stream.read(buf) {
            Ok(0) => return Err("AirPlay RTSP connection closed".into()),
            Ok(n) => {
                let (_, rest) = buf.split_at_mut(n);
                buf = rest;
            }
            Err(e) if matches!(e.kind(), io::ErrorKind::Interrupted) => {}
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                if Instant::now() >= deadline {
                    return Err(format!("timed out waiting for AirPlay RTSP response: {e}").into());
                }
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

fn parse_server_port(headers: &HashMap<String, String>) -> Option<u16> {
    let transport = headers.get("transport")?;
    for part in transport.split(';') {
        if let Some(value) = part.trim().strip_prefix("server_port=") {
            return value
                .split_once('-')
                .map(|(first, _)| first)
                .unwrap_or(value)
                .parse()
                .ok();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use ringbuf::HeapRb;
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::Ordering;
    use std::time::Instant;

    fn target() -> AirPlayTarget {
        AirPlayTarget {
            id: "abc".to_string(),
            name: "Kitchen".to_string(),
            host: "127.0.0.1".to_string(),
            port: 5000,
            model: None,
            service_name: "abc@Kitchen._raop._tcp.local.".to_string(),
            password_protected: false,
            requires_encryption: true,
            encryption_types: vec![1],
            service_kind: fozmo_airplay_protocol::ServiceKind::Raop,
            device_id: Some("ab:c0:00:00:00:00".to_string()),
            features: None,
            source_version: None,
            grouped: false,
            group_id: None,
            group_public_name: None,
            parent_group_id: None,
            tight_sync_id: None,
        }
    }

    fn read_response_from_server(
        response: &'static [u8],
    ) -> Result<RtspResponse, Box<dyn std::error::Error>> {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.write_all(response).unwrap();
        });

        let mut stream = TcpStream::connect(addr).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_millis(20)))
            .unwrap();
        let result = read_rtsp_response(&mut stream, Duration::from_secs(1));
        server.join().unwrap();
        result
    }

    #[test]
    fn volume_maps_to_airplay_db_range() {
        assert_eq!(airplay_volume_db(0.0), -144.0);
        assert_eq!(airplay_volume_db(1.0), 0.0);
        assert!((airplay_volume_db(0.5) - -15.0).abs() < 0.001);
    }

    #[test]
    fn airplay_volume_db_readback_maps_to_transport_value() {
        assert_eq!(
            parse_airplay_volume_parameter(b"volume: -144.0\r\n"),
            Some(0.0)
        );
        assert_eq!(
            parse_airplay_volume_parameter(b"volume: -30.0\r\n"),
            Some(0.0)
        );
        assert_eq!(
            parse_airplay_volume_parameter(b"volume: 0.0\r\n"),
            Some(1.0)
        );
        assert!(
            (parse_airplay_volume_parameter(b"volume: -15.0\r\n").unwrap() - 0.5).abs() < 0.001
        );
    }

    #[test]
    fn sdp_contains_raop_audio_parameters() {
        let encrypted_key = [1u8; 256];
        let iv = [2u8; 16];
        let sdp = build_sdp(
            IpAddr::V4(std::net::Ipv4Addr::new(192, 168, 1, 44)),
            "192.168.1.87",
            "123456789",
            Some((&encrypted_key, &iv)),
        );
        assert!(sdp.contains("o=iTunes 123456789 0 IN IP4 192.168.1.44"));
        assert!(sdp.contains("c=IN IP4 192.168.1.87"));
        assert!(sdp.contains("m=audio 0 RTP/AVP 96"));
        assert!(sdp.contains("a=rtpmap:96 AppleLossless"));
        assert!(sdp.contains("a=fmtp:96 352 0 16 40 10 14 2 255 0 0 44100"));
        assert!(sdp.contains("a=rsaaeskey:"));
        assert!(sdp.contains("a=aesiv:"));
    }

    #[test]
    fn sdp_omits_encryption_when_receiver_allows_cleartext() {
        let sdp = build_sdp(
            IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            "192.168.1.87",
            "123456789",
            None,
        );
        assert!(sdp.contains("a=rtpmap:96 AppleLossless"));
        assert!(!sdp.contains("a=rsaaeskey:"));
        assert!(!sdp.contains("a=aesiv:"));
    }

    #[test]
    fn ipv6_rtsp_hosts_are_bracketed() {
        assert_eq!(rtsp_host("fe80::1234"), "[fe80::1234]");
        assert_eq!(rtsp_host("[fe80::1234]"), "[fe80::1234]");
        assert_eq!(rtsp_host("192.168.1.87"), "192.168.1.87");
    }

    #[test]
    fn rtp_header_uses_alac_payload_type() {
        let mut packet = Vec::new();
        write_rtp_header(&mut packet, 12, 352);
        assert_eq!(&packet[0..2], &[0x80, 0x60]);
        assert_eq!(u16::from_be_bytes([packet[2], packet[3]]), 12);
        assert_eq!(
            u32::from_be_bytes([packet[4], packet[5], packet[6], packet[7]]),
            352
        );
    }

    #[test]
    fn metadata_dmap_wraps_track_fields() {
        let metadata = AirPlayMetadata {
            title: Some("Track".to_string()),
            artist: Some("Artist".to_string()),
            album: Some("Album".to_string()),
            artwork: None,
        };
        let body = metadata_dmap(&metadata);
        assert!(body.windows(4).any(|tag| tag == b"mlit"));
        assert!(body.windows(4).any(|tag| tag == b"minm"));
        assert!(body.windows(4).any(|tag| tag == b"asar"));
        assert!(body.windows(4).any(|tag| tag == b"asal"));
    }

    #[test]
    fn server_port_is_parsed_from_transport_header() {
        let mut headers = HashMap::new();
        headers.insert(
            "transport".to_string(),
            "RTP/AVP/UDP;unicast;mode=record;server_port=6000".to_string(),
        );
        assert_eq!(parse_server_port(&headers), Some(6000));

        headers.insert(
            "transport".to_string(),
            "RTP/AVP/UDP;unicast;mode=record;server_port=6000-6001".to_string(),
        );
        assert_eq!(parse_server_port(&headers), Some(6000));
    }

    #[test]
    fn rtsp_response_reader_waits_through_socket_would_block() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            thread::sleep(Duration::from_millis(80));
            stream
                .write_all(b"RTSP/1.0 200 OK\r\nCSeq: 3\r\nContent-Length: 5\r\n\r\nhello")
                .unwrap();
        });

        let mut stream = TcpStream::connect(addr).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_millis(20)))
            .unwrap();
        let response = read_rtsp_response(&mut stream, Duration::from_secs(1)).unwrap();

        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"hello");
        server.join().unwrap();
    }

    #[test]
    fn rtsp_response_rejects_oversized_content_length_before_reading_body() {
        let response = b"RTSP/1.0 200 OK\r\nCSeq: 3\r\nContent-Length: 1048577\r\n\r\n";

        let err = read_response_from_server(response).unwrap_err().to_string();

        assert!(err.contains("too large"), "{err}");
    }

    #[test]
    fn rtsp_response_rejects_usize_max_content_length() {
        let response =
            b"RTSP/1.0 200 OK\r\nCSeq: 3\r\nContent-Length: 18446744073709551615\r\n\r\n";

        let err = read_response_from_server(response).unwrap_err().to_string();

        assert!(
            err.contains("too large") || err.contains("invalid Content-Length"),
            "{err}"
        );
    }

    #[test]
    fn rtsp_response_rejects_invalid_content_length() {
        let response = b"RTSP/1.0 200 OK\r\nCSeq: 3\r\nContent-Length: nope\r\n\r\n";

        let err = read_response_from_server(response).unwrap_err().to_string();

        assert!(err.contains("invalid Content-Length"), "{err}");
    }

    #[test]
    fn fake_raop_receiver_gets_rtsp_handshake_and_rtp_audio() {
        let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
        udp.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let udp_port = udp.local_addr().unwrap().port();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let rtsp_port = listener.local_addr().unwrap().port();
        let (methods_tx, methods_rx) = mpsc::channel::<String>();

        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut session_sent = false;
            while let Ok((method, headers, _body)) = read_test_rtsp_request(&mut stream) {
                methods_tx.send(method.clone()).unwrap();
                let cseq = headers
                    .get("cseq")
                    .cloned()
                    .unwrap_or_else(|| "1".to_string());
                let mut response = format!("RTSP/1.0 200 OK\r\nCSeq: {cseq}\r\n");
                if method == "SETUP" {
                    let transport = headers.get("transport").cloned().unwrap_or_default();
                    assert!(transport.contains("client_port="), "{transport}");
                    assert!(transport.contains("control_port="), "{transport}");
                    assert!(transport.contains("timing_port="), "{transport}");
                    assert!(!transport.contains("client_port=0"), "{transport}");
                    assert!(!transport.contains("control_port=0"), "{transport}");
                    assert!(!transport.contains("timing_port=0"), "{transport}");
                    response.push_str(&format!(
                        "Transport: RTP/AVP/UDP;unicast;mode=record;server_port={udp_port};control_port=0;timing_port=0\r\n"
                    ));
                    response.push_str("Session: TESTSESSION\r\n");
                    session_sent = true;
                } else if method == "GET_PARAMETER" {
                    let body = b"volume: -15.000000\r\n";
                    response.push_str("Content-Type: text/parameters\r\n");
                    response.push_str(&format!("Content-Length: {}\r\n", body.len()));
                    if session_sent {
                        response.push_str("Session: TESTSESSION\r\n");
                    }
                    response.push_str("\r\n");
                    stream.write_all(response.as_bytes()).unwrap();
                    stream.write_all(body).unwrap();
                    continue;
                } else if session_sent {
                    response.push_str("Session: TESTSESSION\r\n");
                }
                response.push_str("\r\n");
                stream.write_all(response.as_bytes()).unwrap();
                if method == "TEARDOWN" {
                    break;
                }
            }
        });

        let rb = HeapRb::<f64>::new(FRAME_SIZE * CHANNELS * 8);
        let (mut prod, cons) = rb.split();
        let samples = vec![0.0; FRAME_SIZE * CHANNELS * 4];
        assert_eq!(prod.push_slice(&samples), samples.len());
        let state = Arc::new(AtomicPlayerState::new());
        state.state.store(1, Ordering::Relaxed);
        let mut target = target();
        target.port = rtsp_port;
        target.requires_encryption = false;
        target.encryption_types = vec![0, 4];
        let volume_state = Arc::new(AtomicU32::new(f32::NAN.to_bits()));
        let stream = open(
            target,
            cons,
            Arc::clone(&state),
            AirPlayMetadata::default(),
            Arc::clone(&volume_state),
            Some(1.0),
        )
        .unwrap();
        assert!(
            (f32::from_bits(volume_state.load(Ordering::Relaxed)) - 0.5f32.sqrt()).abs() < 0.001,
            "receiver volume readback should update curved AirPlay device volume state"
        );

        let mut packet = [0u8; 2048];
        let (len, _) = udp.recv_from(&mut packet).unwrap();
        assert!(len > 12);
        assert_eq!(&packet[0..2], &[0x80, 0x60]);

        drop(stream);
        server.join().unwrap();

        let deadline = Instant::now() + Duration::from_secs(1);
        let mut methods = Vec::new();
        while Instant::now() < deadline {
            match methods_rx.recv_timeout(Duration::from_millis(20)) {
                Ok(method) => methods.push(method),
                Err(_) => break,
            }
        }
        for expected in [
            "OPTIONS",
            "ANNOUNCE",
            "SETUP",
            "RECORD",
            "SET_PARAMETER",
            "GET_PARAMETER",
        ] {
            assert!(
                methods.iter().any(|method| method == expected),
                "missing {expected} in {methods:?}"
            );
        }
        assert!(
            methods
                .windows(2)
                .any(|methods| methods == ["OPTIONS", "ANNOUNCE"]),
            "cleartext-capable receivers should skip auth setup: {methods:?}"
        );
        assert!(methods.iter().any(|method| method == "TEARDOWN"));
    }

    fn read_test_rtsp_request(
        stream: &mut TcpStream,
    ) -> Result<TestRtspRequest, Box<dyn std::error::Error>> {
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte)?;
            head.push(byte[0]);
        }
        let head_text = String::from_utf8_lossy(&head);
        let mut lines = head_text.split("\r\n");
        let method = lines
            .next()
            .and_then(|line| line.split_whitespace().next())
            .unwrap_or("")
            .to_string();
        let mut headers = HashMap::new();
        for line in lines {
            if line.is_empty() {
                continue;
            }
            if let Some((key, value)) = line.split_once(':') {
                headers.insert(key.trim().to_ascii_lowercase(), value.trim().to_string());
            }
        }
        let content_len = headers
            .get("content-length")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(0);
        let mut body = vec![0u8; content_len];
        if content_len > 0 {
            stream.read_exact(&mut body)?;
        }
        Ok((method, headers, body))
    }
}
