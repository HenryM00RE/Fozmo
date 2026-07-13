use crate::backend::BackendSession;
use crate::discovery::Discovery;
use fozmo_airplay_protocol::{
    Command, ControlRequest, ControlResponse, PROTOCOL_VERSION, ResponsePayload, StreamAttach,
    pcm_socket_for,
};
use std::collections::HashMap;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const MAX_CONTROL_LINE_BYTES: u64 = 16 * 1024 * 1024;
static NEXT_STREAM_ID: AtomicU64 = AtomicU64::new(1);

type Sessions = Arc<Mutex<HashMap<String, BackendSession>>>;

pub fn serve(control_socket: PathBuf, exit_on_stdin_eof: bool) -> Result<(), String> {
    let pcm_socket = pcm_socket_for(&control_socket);
    let control_listener = secure_listener(&control_socket)?;
    let pcm_listener = secure_listener(&pcm_socket)?;
    control_listener
        .set_nonblocking(true)
        .map_err(|error| format!("failed to configure control socket: {error}"))?;
    pcm_listener
        .set_nonblocking(true)
        .map_err(|error| format!("failed to configure PCM socket: {error}"))?;

    let stop = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&stop))
        .map_err(|error| format!("failed to register SIGTERM handler: {error}"))?;
    signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&stop))
        .map_err(|error| format!("failed to register SIGINT handler: {error}"))?;
    if exit_on_stdin_eof {
        spawn_parent_eof_monitor(Arc::clone(&stop));
    }

    let discovery = Discovery::start();
    let sessions: Sessions = Arc::new(Mutex::new(HashMap::new()));
    let pcm_sessions = Arc::clone(&sessions);
    let pcm_stop = Arc::clone(&stop);
    let pcm_thread = thread::Builder::new()
        .name("AirPlayHelperPcmListener".into())
        .spawn(move || pcm_accept_loop(pcm_listener, pcm_sessions, pcm_stop))
        .map_err(|error| format!("failed to start PCM socket listener: {error}"))?;

    eprintln!(
        "fozmo-airplay-helper {} listening on {}",
        env!("CARGO_PKG_VERSION"),
        control_socket.display()
    );
    while !stop.load(Ordering::Relaxed) {
        match accept_blocking(&control_listener) {
            Ok((stream, _)) => {
                let discovery = discovery.clone();
                let sessions = Arc::clone(&sessions);
                let pcm_socket = pcm_socket.clone();
                thread::spawn(move || {
                    if let Err(error) = handle_control(stream, discovery, sessions, &pcm_socket) {
                        eprintln!("airplay helper control error: {error}");
                    }
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(25));
            }
            Err(error) => {
                stop.store(true, Ordering::Relaxed);
                return Err(format!("AirPlay control socket failed: {error}"));
            }
        }
    }

    sessions.lock().unwrap().clear();
    let _ = pcm_thread.join();
    remove_socket(&control_socket);
    remove_socket(&pcm_socket);
    Ok(())
}

fn handle_control(
    stream: UnixStream,
    discovery: Discovery,
    sessions: Sessions,
    pcm_socket: &Path,
) -> Result<(), String> {
    let mut line = String::new();
    let mut reader = BufReader::new(&stream);
    reader
        .by_ref()
        .take(MAX_CONTROL_LINE_BYTES)
        .read_line(&mut line)
        .map_err(|error| format!("failed to read request: {error}"))?;
    let request: ControlRequest =
        serde_json::from_str(&line).map_err(|error| format!("invalid control JSON: {error}"))?;
    let response = if request.version != PROTOCOL_VERSION {
        ControlResponse::error(
            request.request_id,
            "incompatible_version",
            format!(
                "helper protocol {PROTOCOL_VERSION} cannot serve protocol {}",
                request.version
            ),
        )
    } else {
        match execute(request.command, discovery, sessions, pcm_socket) {
            Ok(payload) => ControlResponse::ok(request.request_id, payload),
            Err((code, message)) => ControlResponse::error(request.request_id, code, message),
        }
    };
    let mut stream = reader.into_inner();
    serde_json::to_writer(&mut stream, &response)
        .map_err(|error| format!("failed to encode response: {error}"))?;
    stream
        .write_all(b"\n")
        .map_err(|error| format!("failed to write response: {error}"))?;
    stream
        .flush()
        .map_err(|error| format!("failed to flush response: {error}"))
}

fn execute(
    command: Command,
    discovery: Discovery,
    sessions: Sessions,
    pcm_socket: &Path,
) -> Result<ResponsePayload, (String, String)> {
    match command {
        Command::Hello => Ok(ResponsePayload::Hello {
            protocol_version: PROTOCOL_VERSION,
            helper_version: env!("CARGO_PKG_VERSION").to_string(),
            capabilities: vec![
                "raop".into(),
                "airplay2".into(),
                "alac".into(),
                "pcm_s16le_44100_stereo".into(),
            ],
        }),
        Command::ListReceivers => Ok(ResponsePayload::Receivers {
            receivers: discovery.receivers(),
        }),
        Command::Open {
            receiver_id,
            metadata,
            initial_volume,
        } => {
            let target = discovery.online_target(&receiver_id).ok_or_else(|| {
                (
                    "unknown_receiver".into(),
                    "receiver ID is not currently advertised by this helper".into(),
                )
            })?;
            // A helper owns one physical output stream. Close any previous
            // stream before performing the potentially slow network setup.
            sessions.lock().unwrap().clear();
            let session = BackendSession::open(target, metadata, initial_volume)
                .map_err(|error| ("open_failed".into(), error.to_string()))?;
            let stream_id = format!(
                "{}-{}",
                std::process::id(),
                NEXT_STREAM_ID.fetch_add(1, Ordering::Relaxed)
            );
            sessions.lock().unwrap().insert(stream_id.clone(), session);
            Ok(ResponsePayload::Opened {
                stream_id,
                pcm_socket: pcm_socket.to_path_buf(),
            })
        }
        Command::Pause { stream_id } => with_session(&sessions, &stream_id, |session| {
            session.pause();
            ResponsePayload::Ack
        }),
        Command::Resume { stream_id } => with_session(&sessions, &stream_id, |session| {
            session.resume();
            ResponsePayload::Ack
        }),
        Command::Flush { stream_id } => with_session(&sessions, &stream_id, |session| {
            session.flush();
            ResponsePayload::Ack
        }),
        Command::SetVolume { stream_id, volume } => {
            if !volume.is_finite() {
                return Err(("invalid_volume".into(), "volume must be finite".into()));
            }
            with_session(&sessions, &stream_id, |session| {
                let volume = volume.clamp(0.0, 1.0);
                session.set_volume(volume);
                ResponsePayload::Volume { volume }
            })
        }
        Command::SetMetadata {
            stream_id,
            metadata,
        } => with_session(&sessions, &stream_id, |session| {
            session.set_metadata(metadata);
            ResponsePayload::Ack
        }),
        Command::Close { stream_id } => {
            let removed = sessions.lock().unwrap().remove(&stream_id);
            if removed.is_some() {
                Ok(ResponsePayload::Ack)
            } else {
                Err(("unknown_stream".into(), "stream is not active".into()))
            }
        }
    }
}

fn with_session(
    sessions: &Sessions,
    stream_id: &str,
    operation: impl FnOnce(&BackendSession) -> ResponsePayload,
) -> Result<ResponsePayload, (String, String)> {
    let guard = sessions.lock().unwrap();
    let session = guard
        .get(stream_id)
        .ok_or_else(|| ("unknown_stream".into(), "stream is not active".into()))?;
    if session.reset_requested() {
        return Err((
            "stream_ended".into(),
            "the AirPlay transport ended and must be reopened".into(),
        ));
    }
    Ok(operation(session))
}

fn pcm_accept_loop(listener: UnixListener, sessions: Sessions, stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::Relaxed) {
        match accept_blocking(&listener) {
            Ok((stream, _)) => {
                let sessions = Arc::clone(&sessions);
                thread::spawn(move || {
                    if let Err(error) = handle_pcm(stream, sessions) {
                        eprintln!("airplay helper PCM error: {error}");
                    }
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(25));
            }
            Err(error) => {
                eprintln!("airplay helper PCM listener failed: {error}");
                return;
            }
        }
    }
}

fn handle_pcm(mut stream: UnixStream, sessions: Sessions) -> Result<(), String> {
    let header = read_json_line_without_overread(&mut stream, 8 * 1024)?;
    let attach: StreamAttach = serde_json::from_slice(&header)
        .map_err(|error| format!("invalid PCM attachment: {error}"))?;
    attach.validate().map_err(str::to_string)?;

    let (mut producer, alive) = {
        let mut guard = sessions.lock().unwrap();
        let session = guard
            .get_mut(&attach.stream_id)
            .ok_or_else(|| "PCM attachment references an unknown stream".to_string())?;
        let producer = session
            .producer
            .take()
            .ok_or_else(|| "PCM stream is already attached".to_string())?;
        (producer, Arc::clone(&session.alive))
    };

    let mut input = [0u8; 32 * 1024];
    let mut odd_byte = None;
    while alive.load(Ordering::Relaxed) {
        let read = stream
            .read(&mut input)
            .map_err(|error| format!("failed to read PCM: {error}"))?;
        if read == 0 {
            break;
        }
        feed_pcm_bytes(&input[..read], &mut odd_byte, &mut producer, &alive);
    }
    sessions.lock().unwrap().remove(&attach.stream_id);
    Ok(())
}

fn feed_pcm_bytes(
    bytes: &[u8],
    odd_byte: &mut Option<u8>,
    producer: &mut crate::compat::AudioProducer,
    alive: &AtomicBool,
) {
    let mut index = 0;
    if let Some(low) = odd_byte.take() {
        if let Some(high) = bytes.first() {
            push_sample(
                producer,
                i16::from_le_bytes([low, *high]) as f64 / 32768.0,
                alive,
            );
            index = 1;
        } else {
            *odd_byte = Some(low);
            return;
        }
    }
    while index + 1 < bytes.len() {
        let sample = i16::from_le_bytes([bytes[index], bytes[index + 1]]);
        push_sample(producer, sample as f64 / 32768.0, alive);
        index += 2;
    }
    if index < bytes.len() {
        *odd_byte = Some(bytes[index]);
    }
}

fn push_sample(producer: &mut crate::compat::AudioProducer, mut sample: f64, alive: &AtomicBool) {
    while alive.load(Ordering::Relaxed) {
        match producer.push(sample) {
            Ok(()) => return,
            Err(returned) => {
                sample = returned;
                thread::sleep(Duration::from_millis(1));
            }
        }
    }
}

fn read_json_line_without_overread(
    reader: &mut impl Read,
    max_bytes: usize,
) -> Result<Vec<u8>, String> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    while line.len() < max_bytes {
        let read = reader
            .read(&mut byte)
            .map_err(|error| format!("failed to read attachment header: {error}"))?;
        if read == 0 {
            return Err("connection closed before attachment header".into());
        }
        if byte[0] == b'\n' {
            return Ok(line);
        }
        line.push(byte[0]);
    }
    Err("attachment header exceeds maximum size".into())
}

fn secure_listener(path: &Path) -> Result<UnixListener, String> {
    let directory = path
        .parent()
        .ok_or_else(|| "socket path must have a parent directory".to_string())?;
    if let Ok(metadata) = fs::symlink_metadata(directory)
        && metadata.file_type().is_symlink()
    {
        return Err(format!(
            "refusing symlink AirPlay runtime directory: {}",
            directory.display()
        ));
    }
    fs::create_dir_all(directory)
        .map_err(|error| format!("failed to create runtime directory: {error}"))?;
    fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("failed to secure runtime directory: {error}"))?;
    remove_stale_socket(path)?;
    let listener = UnixListener::bind(path)
        .map_err(|error| format!("failed to bind {}: {error}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("failed to secure {}: {error}", path.display()))?;
    Ok(listener)
}

fn accept_blocking(
    listener: &UnixListener,
) -> io::Result<(UnixStream, std::os::unix::net::SocketAddr)> {
    let (stream, address) = listener.accept()?;
    // BSD-derived systems, including macOS, may propagate O_NONBLOCK from a
    // listener to accepted sockets. Only the accept loops are polled; request
    // and PCM workers use blocking I/O and must not mistake EAGAIN for EOF or
    // a broken client when they run before the peer has written.
    stream.set_nonblocking(false)?;
    Ok((stream, address))
}

fn remove_stale_socket(path: &Path) -> Result<(), String> {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return Ok(());
    };
    if !metadata.file_type().is_socket() {
        return Err(format!(
            "refusing to replace non-socket path: {}",
            path.display()
        ));
    }
    fs::remove_file(path)
        .map_err(|error| format!("failed to remove stale socket {}: {error}", path.display()))
}

fn remove_socket(path: &Path) {
    if fs::symlink_metadata(path)
        .map(|metadata| metadata.file_type().is_socket())
        .unwrap_or(false)
    {
        let _ = fs::remove_file(path);
    }
}

fn spawn_parent_eof_monitor(stop: Arc<AtomicBool>) {
    thread::Builder::new()
        .name("AirPlayHelperParentMonitor".into())
        .spawn(move || {
            let mut stdin = std::io::stdin().lock();
            let mut byte = [0u8; 1];
            loop {
                match stdin.read(&mut byte) {
                    Ok(0) | Err(_) => {
                        stop.store(true, Ordering::Relaxed);
                        return;
                    }
                    Ok(_) => {}
                }
            }
        })
        .expect("failed to start parent EOF monitor");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn header_reader_does_not_consume_pcm_after_newline() {
        let mut input = std::io::Cursor::new(b"{\"version\":1}\n\x34\x12".to_vec());
        assert_eq!(
            read_json_line_without_overread(&mut input, 100).unwrap(),
            b"{\"version\":1}"
        );
        let mut pcm = [0u8; 2];
        input.read_exact(&mut pcm).unwrap();
        assert_eq!(pcm, [0x34, 0x12]);
    }

    #[test]
    fn runtime_directory_and_socket_are_owner_only() {
        let root = std::env::temp_dir().join(format!(
            "fozmo-helper-permissions-{}-{}",
            std::process::id(),
            NEXT_STREAM_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let socket = root.join("control.sock");
        let listener = secure_listener(&socket).unwrap();
        assert_eq!(
            fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&socket).unwrap().permissions().mode() & 0o777,
            0o600
        );
        drop(listener);
        remove_socket(&socket);
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn accepted_stream_is_blocking_even_when_listener_is_not() {
        let root = std::env::temp_dir().join(format!(
            "fozmo-helper-accepted-stream-{}-{}",
            std::process::id(),
            NEXT_STREAM_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let socket = root.join("control.sock");
        let listener = secure_listener(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut client = UnixStream::connect(&socket).unwrap();
        let (mut accepted, _) = accept_blocking(&listener).unwrap();

        let reader = thread::spawn(move || {
            let mut byte = [0u8; 1];
            accepted.read_exact(&mut byte).unwrap();
            byte[0]
        });
        thread::sleep(Duration::from_millis(20));
        client.write_all(b"x").unwrap();
        assert_eq!(reader.join().unwrap(), b'x');

        drop(client);
        drop(listener);
        remove_socket(&socket);
        fs::remove_dir(root).unwrap();
    }
}
