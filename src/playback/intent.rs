use crate::playback::error::PlaybackError;
use crate::playback::request::PlaybackRequest;

#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant)]
pub(crate) enum PlaybackIntent {
    Play { request: PlaybackRequest },
    Pause,
    Resume,
    Stop,
    Next,
    Seek { seconds: f64 },
    SetLoopMode { mode: LoopMode },
    SetVolume { volume: f32 },
    SetDeviceVolume { volume: f32 },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum PlaybackOutcome {
    Completed,
    QobuzRadioAdvanced,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum LoopMode {
    Off,
    One,
    Loop,
}

impl LoopMode {
    pub(crate) fn parse(mode: &str) -> Result<Self, PlaybackError> {
        match mode.trim().to_ascii_lowercase().as_str() {
            "off" => Ok(Self::Off),
            "one" => Ok(Self::One),
            "loop" => Ok(Self::Loop),
            _ => Err(PlaybackError::bad_request("Invalid loop mode")),
        }
    }

    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::One => "one",
            Self::Loop => "loop",
        }
    }

    pub(crate) fn repeat_one(&self) -> bool {
        matches!(self, Self::One | Self::Loop)
    }
}

#[cfg(test)]
mod tests {
    use super::LoopMode;

    #[test]
    fn loop_mode_enables_repeat_one() {
        assert!(LoopMode::Loop.repeat_one());
        assert!(LoopMode::One.repeat_one());
        assert!(!LoopMode::Off.repeat_one());
    }
}
