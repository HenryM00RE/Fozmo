//! Synchronous Music.app controls for native Apple Music product playback.

use super::model::MusicAppTrack;
use std::ffi::{CStr, CString, c_char};
#[cfg(test)]
use std::process::Command;
use std::thread;
use std::time::Duration;

const MUSIC_COMMAND_ATTEMPTS: usize = 3;
const MUSIC_COMMAND_RETRY_DELAY: Duration = Duration::from_millis(150);
const MUSIC_APPLE_EVENT_TIMEOUT_SECS: f64 = 2.0;

const MUSIC_STATUS_SCRIPT: &[&str] = &[
    "tell application \"Music\"",
    "set playbackState to player state as string",
    "set trackKey to \"\"",
    "set trackName to \"\"",
    "set artistName to \"\"",
    "set albumName to \"\"",
    "set trackDuration to \"\"",
    "set trackPosition to \"\"",
    "if player state is not stopped then",
    "try",
    "set trackKey to (database ID of current track) as string",
    "end try",
    "try",
    "set trackName to name of current track",
    "set artistName to artist of current track",
    "set albumName to album of current track",
    "set trackDuration to duration of current track as string",
    "set trackPosition to player position as string",
    "end try",
    "end if",
    "return playbackState & linefeed & trackKey & linefeed & trackName & linefeed & artistName & linefeed & albumName & linefeed & trackDuration & linefeed & trackPosition",
    "end tell",
];

const MUSIC_PLAY_CURRENT_ONCE_FROM_START_SCRIPT: &[&str] = &[
    "tell application \"Music\"",
    "pause",
    "play current track once true",
    "set player position to 0",
    "end tell",
];

const MUSIC_PLAY_CURRENT_IN_CONTEXT_FROM_START_SCRIPT: &[&str] = &[
    "tell application \"Music\"",
    "pause",
    "play current track once false",
    "set player position to 0",
    "end tell",
];

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct MusicAppSnapshot {
    pub running: bool,
    pub player_state: Option<String>,
    pub track: MusicAppTrack,
}

unsafe extern "C" {
    fn fozmo_music_app_pid() -> i32;
    fn fozmo_music_activate_catalog_track(
        storefront: *const c_char,
        album_id: *const c_char,
        song_id: *const c_char,
        error_buffer: *mut c_char,
        error_capacity: usize,
    ) -> i32;
    fn fozmo_music_execute_script(
        source: *const c_char,
        timeout_seconds: f64,
        output_buffer: *mut c_char,
        output_capacity: usize,
        error_buffer: *mut c_char,
        error_capacity: usize,
    ) -> i32;
    fn fozmo_music_wait_for_player_notification(timeout_ms: u32) -> i32;
}

pub(super) fn pid() -> Option<u32> {
    u32::try_from(unsafe { fozmo_music_app_pid() })
        .ok()
        .filter(|pid| *pid > 0)
}

impl MusicAppSnapshot {
    pub(crate) fn has_current_track(&self) -> bool {
        self.track.track_key.is_some() || self.track.title.is_some()
    }
}

pub(crate) fn status() -> Result<MusicAppSnapshot, String> {
    if !music_app_running() {
        return Ok(MusicAppSnapshot::default());
    }
    let output = run_apple_script(MUSIC_STATUS_SCRIPT.iter().copied())?;
    Ok(parse_status(&output))
}

pub(crate) fn play() -> Result<(), String> {
    run_music_command("play")
}

/// Restart the selected native Music.app track at zero and constrain playback
/// to that one track. Fozmo, rather than Music.app's album queue, owns the next
/// provider boundary.
pub(crate) fn play_current_once() -> Result<(), String> {
    run_music_script_with_retry(MUSIC_PLAY_CURRENT_ONCE_FROM_START_SCRIPT).map(|_| ())
}

/// Restart the selected track at zero while retaining Music.app's album
/// context. When Fozmo's next queued source is the following track from the
/// same catalog album, Music.app can decode the boundary continuously into the
/// existing capture stream instead of requiring another catalog activation.
pub(crate) fn play_current_in_context() -> Result<(), String> {
    run_music_script_with_retry(MUSIC_PLAY_CURRENT_IN_CONTEXT_FROM_START_SCRIPT).map(|_| ())
}

pub(crate) fn pause() -> Result<(), String> {
    run_music_command("pause")
}

pub(crate) fn prepare_bit_perfect() -> Result<(), String> {
    run_music_script_with_retry(&[
        "tell application \"Music\"",
        "set sound volume to 100",
        "try",
        "set EQ enabled to false",
        "end try",
        "end tell",
    ])
    .map(|_| ())
}

/// Navigate Music.app to the catalog album and activate the exact row by its
/// stable accessibility identifier. The native bridge briefly foregrounds
/// Music, delivers a HID-level double-click at that row, restores the pointer
/// and previous foreground app, then returns.
pub(crate) fn activate_catalog_track(
    storefront: &str,
    album_id: &str,
    song_id: &str,
) -> Result<(), String> {
    validate_catalog_component("storefront", storefront)?;
    validate_catalog_component("album ID", album_id)?;
    validate_catalog_component("song ID", song_id)?;
    let storefront = CString::new(storefront)
        .map_err(|_| "Apple Music storefront contains an invalid NUL byte.".to_string())?;
    let album_id = CString::new(album_id)
        .map_err(|_| "Apple Music album ID contains an invalid NUL byte.".to_string())?;
    let song_id = CString::new(song_id)
        .map_err(|_| "Apple Music song ID contains an invalid NUL byte.".to_string())?;
    let mut error = vec![0_i8; 1_024];
    let result = unsafe {
        fozmo_music_activate_catalog_track(
            storefront.as_ptr(),
            album_id.as_ptr(),
            song_id.as_ptr(),
            error.as_mut_ptr(),
            error.len(),
        )
    };
    if result == 1 {
        return Ok(());
    }
    let message = unsafe { CStr::from_ptr(error.as_ptr()) }
        .to_string_lossy()
        .trim()
        .to_string();
    Err(if message.is_empty() {
        "Music.app could not activate the requested Apple Music catalog track.".to_string()
    } else {
        message
    })
}

pub(crate) fn set_position(seconds: f64) -> Result<(), String> {
    if !seconds.is_finite() || seconds < 0.0 {
        return Err("Apple Music position must be a finite non-negative value.".to_string());
    }
    let position = format!("set player position to {seconds:.3}");
    run_music_script_with_retry(&["tell application \"Music\"", position.as_str(), "end tell"])
        .map(|_| ())
}

pub(crate) fn wait_for_player_notification(timeout: Duration) -> bool {
    let timeout_ms = u32::try_from(timeout.as_millis())
        .unwrap_or(u32::MAX)
        .max(1);
    unsafe { fozmo_music_wait_for_player_notification(timeout_ms) == 1 }
}

fn run_music_command(command: &str) -> Result<(), String> {
    run_music_script_with_retry(&["tell application \"Music\"", command, "end tell"]).map(|_| ())
}

fn validate_catalog_component(label: &str, value: &str) -> Result<(), String> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(format!("Apple Music {label} is invalid."));
    }
    Ok(())
}

/// Answer from the kernel's process table. Playback start and the transport
/// monitor both query `status` on latency-sensitive paths, so this must not
/// fork a helper process, and it must not consult `NSWorkspace`, whose cached
/// running-application list goes stale in a process that never pumps a run loop.
fn music_app_running() -> bool {
    pid().is_some()
}

fn run_apple_script<'a>(lines: impl IntoIterator<Item = &'a str>) -> Result<String, String> {
    let source = lines.into_iter().collect::<Vec<_>>().join("\n");
    let source = CString::new(source)
        .map_err(|_| "The Music app command contains an invalid NUL byte.".to_string())?;
    let mut output = vec![0_i8; 8_192];
    let mut error = vec![0_i8; 2_048];
    let result = unsafe {
        fozmo_music_execute_script(
            source.as_ptr(),
            MUSIC_APPLE_EVENT_TIMEOUT_SECS,
            output.as_mut_ptr(),
            output.len(),
            error.as_mut_ptr(),
            error.len(),
        )
    };
    if result == 1 {
        Ok(unsafe { CStr::from_ptr(output.as_ptr()) }
            .to_string_lossy()
            .trim()
            .to_string())
    } else {
        let error = unsafe { CStr::from_ptr(error.as_ptr()) }
            .to_string_lossy()
            .trim()
            .to_string();
        Err(if error.is_empty() {
            "The Music app command failed.".to_string()
        } else {
            readable_apple_script_error(&error)
        })
    }
}

fn run_music_script_with_retry(lines: &[&str]) -> Result<String, String> {
    let mut last_error = None;
    for attempt in 0..MUSIC_COMMAND_ATTEMPTS {
        match run_apple_script(lines.iter().copied()) {
            Ok(output) => return Ok(output),
            Err(error) => last_error = Some(error),
        }
        if attempt + 1 < MUSIC_COMMAND_ATTEMPTS {
            thread::sleep(MUSIC_COMMAND_RETRY_DELAY);
        }
    }
    Err(last_error.unwrap_or_else(|| "The Music app command failed.".to_string()))
}

fn readable_apple_script_error(stderr: &str) -> String {
    let message = stderr.trim();
    let mut parts = message.splitn(3, ':');
    let first = parts.next().unwrap_or_default().trim();
    let second = parts.next().unwrap_or_default().trim();
    let message = if !first.is_empty()
        && !second.is_empty()
        && first.bytes().all(|byte| byte.is_ascii_digit())
        && second.bytes().all(|byte| byte.is_ascii_digit())
    {
        parts.next().unwrap_or(message).trim()
    } else {
        message
    };
    let message = message
        .strip_prefix("execution error:")
        .unwrap_or(message)
        .trim();
    if let Some(detail) = message.strip_prefix("Music got an error:") {
        format!("Music.app reported: {}", detail.trim())
    } else {
        message.to_string()
    }
}

fn parse_status(output: &str) -> MusicAppSnapshot {
    let mut lines = output.lines();
    let player_state = normalize(lines.next());
    let track_key = normalize(lines.next());
    let title = normalize(lines.next());
    let artist = normalize(lines.next());
    let album = normalize(lines.next());
    let duration_secs = parse_number(lines.next());
    let position_secs = parse_number(lines.next());
    MusicAppSnapshot {
        running: true,
        player_state,
        track: MusicAppTrack {
            track_key,
            title,
            artist,
            album,
            duration_secs,
            position_secs,
        },
    }
}

fn normalize(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty() && *value != "missing value")
        .map(str::to_string)
}

fn parse_number(value: Option<&str>) -> Option<f64> {
    normalize(value)
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value >= 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Playback aborts with "Music.app quit" the moment this disagrees with
    /// reality, so check it against the process table rather than against a
    /// fixed expectation: this holds whether or not Music.app is running here.
    #[test]
    fn music_app_liveness_agrees_with_the_process_table() {
        let running_per_pgrep = Command::new("/usr/bin/pgrep")
            .args(["-x", "Music"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success());

        assert_eq!(music_app_running(), running_per_pgrep);
        assert_eq!(pid().is_some(), running_per_pgrep);
    }

    #[test]
    fn parses_music_status_with_timeline() {
        let snapshot =
            parse_status("playing\n42\nMelodie Is A Wound\nDaniel Avery\nTremor\n312.5\n81.25");

        assert!(snapshot.running);
        assert!(snapshot.has_current_track());
        assert_eq!(snapshot.player_state.as_deref(), Some("playing"));
        assert_eq!(snapshot.track.track_key.as_deref(), Some("42"));
        assert_eq!(snapshot.track.title.as_deref(), Some("Melodie Is A Wound"));
        assert_eq!(snapshot.track.position_secs, Some(81.25));
        assert_eq!(snapshot.track.duration_secs, Some(312.5));
    }

    #[test]
    fn stopped_music_has_no_current_track() {
        let snapshot = parse_status("stopped\n\n\n\n\n\n");

        assert_eq!(snapshot.player_state.as_deref(), Some("stopped"));
        assert!(!snapshot.has_current_track());
        assert_eq!(snapshot.track.position_secs, None);
    }

    #[test]
    fn catalog_components_reject_url_and_accessibility_injection() {
        assert!(validate_catalog_component("song ID", "635770203").is_ok());
        assert!(validate_catalog_component("storefront", "nz").is_ok());
        assert!(validate_catalog_component("album ID", "../../bad").is_err());
        assert!(validate_catalog_component("song ID", "1?i=2").is_err());
        assert!(validate_catalog_component("storefront", "").is_err());
    }

    #[test]
    fn single_track_restart_resets_timeline_after_starting_transport() {
        assert_eq!(
            MUSIC_PLAY_CURRENT_ONCE_FROM_START_SCRIPT,
            &[
                "tell application \"Music\"",
                "pause",
                "play current track once true",
                "set player position to 0",
                "end tell",
            ]
        );
    }

    #[test]
    fn contextual_restart_preserves_music_app_album_advance() {
        assert_eq!(
            MUSIC_PLAY_CURRENT_IN_CONTEXT_FROM_START_SCRIPT,
            &[
                "tell application \"Music\"",
                "pause",
                "play current track once false",
                "set player position to 0",
                "end tell",
            ]
        );
    }

    #[test]
    fn apple_script_errors_hide_source_offsets_and_name_music_app() {
        assert_eq!(
            readable_apple_script_error(
                "54:58: execution error: Music got an error: The operation timed out. (-1712)"
            ),
            "Music.app reported: The operation timed out. (-1712)"
        );
        assert_eq!(
            readable_apple_script_error("execution error: Music is not running. (-600)"),
            "Music is not running. (-600)"
        );
    }
}
