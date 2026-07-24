//! Small, synchronous Music.app control surface for the process-tap A/B tool.
//!
//! The API route runs these calls with `spawn_blocking`; keeping AppleScript
//! here prevents Music.app transport details from leaking into HTTP handlers.

use super::model::AppleMusicComparisonTrack;
use std::process::Command;

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

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct MusicAppSnapshot {
    pub running: bool,
    pub player_state: Option<String>,
    pub track: AppleMusicComparisonTrack,
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

pub(crate) fn pause() -> Result<(), String> {
    run_music_command("pause")
}

pub(crate) fn set_position(seconds: f64) -> Result<(), String> {
    if !seconds.is_finite() || seconds < 0.0 {
        return Err("Apple Music position must be a finite non-negative value.".to_string());
    }
    let position = format!("set player position to {seconds:.3}");
    run_osascript(["tell application \"Music\"", position.as_str(), "end tell"]).map(|_| ())
}

fn run_music_command(command: &str) -> Result<(), String> {
    run_osascript(["tell application \"Music\"", command, "end tell"]).map(|_| ())
}

fn music_app_running() -> bool {
    Command::new("pgrep")
        .args(["-x", "Music"])
        .status()
        .is_ok_and(|status| status.success())
}

fn run_osascript<'a>(lines: impl IntoIterator<Item = &'a str>) -> Result<String, String> {
    let mut command = Command::new("osascript");
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
}
