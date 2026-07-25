//! Small, synchronous Music.app control surface for the process-tap A/B tool.
//!
//! The API route runs these calls with `spawn_blocking`; keeping AppleScript
//! here prevents Music.app transport details from leaking into HTTP handlers.

use super::model::AppleMusicComparisonTrack;
use std::ffi::{CStr, CString, c_char};
use std::process::{Command, Stdio};

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

const MUSIC_PAUSE_AND_STATUS_SCRIPT: &[&str] = &[
    "tell application \"Music\"",
    "pause",
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
    "set player position to 0",
    "play current track once true",
    "end tell",
];

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct MusicAppSnapshot {
    pub running: bool,
    pub player_state: Option<String>,
    pub track: AppleMusicComparisonTrack,
}

unsafe extern "C" {
    fn fozmo_music_activate_catalog_track(
        storefront: *const c_char,
        album_id: *const c_char,
        song_id: *const c_char,
        error_buffer: *mut c_char,
        error_capacity: usize,
    ) -> i32;
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
    let output = run_osascript(MUSIC_STATUS_SCRIPT.iter().copied())?;
    Ok(parse_status(&output))
}

pub(crate) fn play() -> Result<(), String> {
    run_music_command("play")
}

/// Restart the selected native Music.app track at zero and constrain playback
/// to that one track. Fozmo, rather than Music.app's album queue, owns the next
/// provider boundary.
pub(crate) fn play_current_once() -> Result<(), String> {
    run_osascript(MUSIC_PLAY_CURRENT_ONCE_FROM_START_SCRIPT.iter().copied()).map(|_| ())
}

pub(crate) fn pause() -> Result<(), String> {
    run_music_command("pause")
}

pub(crate) fn prepare_bit_perfect() -> Result<(), String> {
    run_osascript([
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

pub(crate) fn pause_and_status() -> Result<MusicAppSnapshot, String> {
    if !music_app_running() {
        return Ok(MusicAppSnapshot::default());
    }
    let output = run_osascript(MUSIC_PAUSE_AND_STATUS_SCRIPT.iter().copied())?;
    Ok(parse_status(&output))
}

pub(crate) fn set_position(seconds: f64) -> Result<(), String> {
    if !seconds.is_finite() || seconds < 0.0 {
        return Err("Apple Music position must be a finite non-negative value.".to_string());
    }
    let position = format!("set player position to {seconds:.3}");
    run_osascript(["tell application \"Music\"", position.as_str(), "end tell"]).map(|_| ())
}

pub(crate) fn set_position_and_play(seconds: f64) -> Result<(), String> {
    if !seconds.is_finite() || seconds < 0.0 {
        return Err("Apple Music position must be a finite non-negative value.".to_string());
    }
    let position = format!("set player position to {seconds:.3}");
    run_osascript([
        "tell application \"Music\"",
        position.as_str(),
        "play",
        "end tell",
    ])
    .map(|_| ())
}

fn run_music_command(command: &str) -> Result<(), String> {
    run_osascript(["tell application \"Music\"", command, "end tell"]).map(|_| ())
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

fn music_app_running() -> bool {
    Command::new("/usr/bin/pgrep")
        .args(["-x", "Music"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn run_osascript<'a>(lines: impl IntoIterator<Item = &'a str>) -> Result<String, String> {
    let mut command = Command::new("/usr/bin/osascript");
    for line in lines {
        command.arg("-e").arg(line);
    }
    let output = command
        .output()
        .map_err(|error| format!("Failed to talk to the Music app: {error}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(if stderr.is_empty() {
            "The Music app command failed.".to_string()
        } else {
            stderr
        })
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
        track: AppleMusicComparisonTrack {
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
    fn single_track_restart_resets_timeline_before_playing() {
        assert_eq!(
            MUSIC_PLAY_CURRENT_ONCE_FROM_START_SCRIPT,
            &[
                "tell application \"Music\"",
                "set player position to 0",
                "play current track once true",
                "end tell",
            ]
        );
    }
}
