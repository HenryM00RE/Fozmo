use crate::playback::error::PlaybackError;
use crate::playback::sequencer::PlaybackRequestSequence;
use crate::protocol::SourceRef;
use crate::services::qobuz::QobuzPlayRequest;

#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant)]
pub(crate) enum PlaybackIntent {
    Play {
        profile_id: String,
        source: SourceRef,
        queue: Vec<SourceRef>,
        radio_auto: bool,
        guard: PlaybackGuard,
        qobuz_request: Option<Box<QobuzPlayRequest>>,
    },
    Pause,
    Resume,
    Stop,
    Next,
    Seek {
        seconds: f64,
    },
    SetLoopMode {
        mode: LoopMode,
    },
    SetVolume {
        volume: f32,
    },
    SetDeviceVolume {
        volume: f32,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum PlaybackOutcome {
    Completed,
    QobuzRadioAdvanced,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct PlaybackGuard {
    expected_sequence: Option<PlaybackRequestSequence>,
}

impl PlaybackGuard {
    pub(crate) fn none() -> Self {
        Self::default()
    }

    pub(crate) fn from_expected_sequence(
        expected_sequence: Option<PlaybackRequestSequence>,
    ) -> Self {
        Self { expected_sequence }
    }

    pub(crate) fn is_current(&self, state: &crate::app::state::AppState) -> bool {
        self.expected_sequence
            .as_ref()
            .is_none_or(|expected| state.playback_sequencer().is_current(expected))
    }

    pub(crate) fn expected_sequence(&self) -> Option<&PlaybackRequestSequence> {
        self.expected_sequence.as_ref()
    }
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
