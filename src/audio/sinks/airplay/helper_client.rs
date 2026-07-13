use fozmo_airplay_protocol::{
    Command, ControlRequest, ControlResponse, ControlResult, PROTOCOL_VERSION, ResponsePayload,
    StreamAttach, default_control_socket,
};
use std::fmt;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);
const CONTROL_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_CONTROL_LINE_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    Missing,
    Unavailable,
    Incompatible,
    Protocol,
}

#[derive(Debug)]
pub struct HelperClientError {
    pub kind: ErrorKind,
    message: String,
}

impl HelperClientError {
    fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl fmt::Display for HelperClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for HelperClientError {}

#[cfg(unix)]
pub fn request(command: Command) -> Result<ResponsePayload, HelperClientError> {
    use std::os::unix::net::UnixStream;

    let socket = default_control_socket();
    if !socket.exists() {
        return Err(HelperClientError::new(
            ErrorKind::Missing,
            format!("AirPlay helper socket is missing: {}", socket.display()),
        ));
    }
    let mut stream = UnixStream::connect(&socket).map_err(|error| {
        HelperClientError::new(
            ErrorKind::Unavailable,
            format!("failed to connect to AirPlay helper: {error}"),
        )
    })?;
    stream.set_read_timeout(Some(CONTROL_TIMEOUT)).ok();
    stream.set_write_timeout(Some(CONTROL_TIMEOUT)).ok();

    let request_id = NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed);
    let request = ControlRequest::new(request_id, command);
    serde_json::to_writer(&mut stream, &request).map_err(|error| {
        HelperClientError::new(
            ErrorKind::Protocol,
            format!("failed to encode AirPlay helper request: {error}"),
        )
    })?;
    stream.write_all(b"\n").map_err(|error| {
        HelperClientError::new(
            ErrorKind::Unavailable,
            format!("failed to write AirPlay helper request: {error}"),
        )
    })?;
    stream.flush().map_err(|error| {
        HelperClientError::new(
            ErrorKind::Unavailable,
            format!("failed to flush AirPlay helper request: {error}"),
        )
    })?;

    let mut line = String::new();
    BufReader::new(stream)
        .take(MAX_CONTROL_LINE_BYTES)
        .read_line(&mut line)
        .map_err(|error| {
            HelperClientError::new(
                ErrorKind::Unavailable,
                format!("failed to read AirPlay helper response: {error}"),
            )
        })?;
    if line.is_empty() {
        return Err(HelperClientError::new(
            ErrorKind::Unavailable,
            "AirPlay helper closed the control connection",
        ));
    }
    let response: ControlResponse = serde_json::from_str(&line).map_err(|error| {
        HelperClientError::new(
            ErrorKind::Protocol,
            format!("invalid AirPlay helper response: {error}"),
        )
    })?;
    if response.version != PROTOCOL_VERSION {
        return Err(HelperClientError::new(
            ErrorKind::Incompatible,
            format!(
                "AirPlay helper protocol {} is incompatible with server protocol {}",
                response.version, PROTOCOL_VERSION
            ),
        ));
    }
    if response.request_id != request_id {
        return Err(HelperClientError::new(
            ErrorKind::Protocol,
            "AirPlay helper response request_id did not match",
        ));
    }
    match response.result {
        ControlResult::Ok { payload } => Ok(payload),
        ControlResult::Error { code, message } => Err(HelperClientError::new(
            if code == "incompatible_version" {
                ErrorKind::Incompatible
            } else {
                ErrorKind::Protocol
            },
            format!("AirPlay helper {code}: {message}"),
        )),
    }
}

#[cfg(not(unix))]
pub fn request(_command: Command) -> Result<ResponsePayload, HelperClientError> {
    Err(HelperClientError::new(
        ErrorKind::Missing,
        "the standalone AirPlay helper requires Unix-domain sockets",
    ))
}

#[cfg(unix)]
pub fn connect_pcm(
    socket: &Path,
    stream_id: &str,
) -> Result<std::os::unix::net::UnixStream, HelperClientError> {
    use std::os::unix::net::UnixStream;

    let mut stream = UnixStream::connect(socket).map_err(|error| {
        HelperClientError::new(
            ErrorKind::Unavailable,
            format!("failed to connect to AirPlay PCM socket: {error}"),
        )
    })?;
    stream.set_write_timeout(Some(CONTROL_TIMEOUT)).ok();
    serde_json::to_writer(&mut stream, &StreamAttach::new(stream_id)).map_err(|error| {
        HelperClientError::new(
            ErrorKind::Protocol,
            format!("failed to encode AirPlay PCM attachment: {error}"),
        )
    })?;
    stream.write_all(b"\n").map_err(|error| {
        HelperClientError::new(
            ErrorKind::Unavailable,
            format!("failed to attach AirPlay PCM stream: {error}"),
        )
    })?;
    stream.flush().map_err(|error| {
        HelperClientError::new(
            ErrorKind::Unavailable,
            format!("failed to flush AirPlay PCM attachment: {error}"),
        )
    })?;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_helper_is_a_distinct_runtime_state() {
        let missing = std::env::temp_dir().join(format!(
            "fozmo-no-helper-{}-{}",
            std::process::id(),
            NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed)
        ));
        assert!(!missing.exists());
        let error = connect_control_at(&missing, Command::Hello).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Missing);
    }

    #[cfg(unix)]
    fn connect_control_at(
        socket: &Path,
        _command: Command,
    ) -> Result<ResponsePayload, HelperClientError> {
        if !socket.exists() {
            return Err(HelperClientError::new(ErrorKind::Missing, "missing"));
        }
        unreachable!()
    }

    #[cfg(not(unix))]
    fn connect_control_at(
        _socket: &Path,
        command: Command,
    ) -> Result<ResponsePayload, HelperClientError> {
        request(command)
    }
}
