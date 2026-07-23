mod local;
mod remote_agent;
mod sonos;
mod upnp;

use self::local::LocalPlaybackSink;
use self::remote_agent::RemoteAgentSink;
use self::sonos::SonosSink;
use self::upnp::UpnpSink;
use crate::app::state::AppState;
use crate::playback::error::PlaybackError;
use crate::playback::intent::{LoopMode, PlaybackOutcome};
use crate::playback::request::PlaybackRequest;
use crate::protocol::SinkProtocol;
use tracing::debug;

pub(crate) struct SinkResolver<'a> {
    state: &'a AppState,
}

pub(crate) enum ResolvedSink<'a> {
    Local(LocalPlaybackSink<'a>),
    RemoteAgent(RemoteAgentSink<'a>),
    Sonos(SonosSink<'a>),
    Upnp(UpnpSink<'a>),
}

impl<'a> SinkResolver<'a> {
    pub(crate) fn new(state: &'a AppState) -> Self {
        Self { state }
    }

    pub(crate) fn resolve(&self, zone_id: &str) -> Result<ResolvedSink<'a>, PlaybackError> {
        let sink = match self.state.zones().zone_protocol(zone_id) {
            Some(SinkProtocol::RemoteAgent) => {
                ResolvedSink::RemoteAgent(RemoteAgentSink::new(self.state))
            }
            Some(SinkProtocol::SonosUpnp) if cfg!(feature = "sonos") => {
                ResolvedSink::Sonos(SonosSink::new(self.state))
            }
            Some(SinkProtocol::SonosUpnp) => {
                return Err(PlaybackError::bad_request(
                    "Sonos support was not compiled into this build",
                ));
            }
            Some(SinkProtocol::UpnpAvRenderer) if cfg!(feature = "upnp") => {
                ResolvedSink::Upnp(UpnpSink::new(self.state))
            }
            Some(SinkProtocol::UpnpAvRenderer) => {
                return Err(PlaybackError::bad_request(
                    "UPnP support was not compiled into this build",
                ));
            }
            Some(_) => ResolvedSink::Local(LocalPlaybackSink::new(self.state)),
            None => return Err(PlaybackError::ZoneNotAvailable),
        };
        debug!(
            event = "zone_route",
            zone_id,
            sink = sink.as_str(),
            "Resolved zone sink"
        );
        Ok(sink)
    }
}

impl ResolvedSink<'_> {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Local(_) => "local",
            Self::RemoteAgent(_) => "remote_agent",
            Self::Sonos(_) => "sonos",
            Self::Upnp(_) => "upnp",
        }
    }

    pub(crate) async fn play(
        &self,
        zone_id: &str,
        request: PlaybackRequest,
    ) -> Result<PlaybackOutcome, PlaybackError> {
        match self {
            Self::Local(sink) => sink.play(zone_id, request).await,
            Self::RemoteAgent(sink) => sink.play(zone_id, request),
            Self::Sonos(sink) => sink.play(zone_id, request).await,
            Self::Upnp(sink) => sink.play(zone_id, request).await,
        }
    }

    pub(crate) async fn next(
        &self,
        zone_id: &str,
        profile_id: String,
    ) -> Result<PlaybackOutcome, PlaybackError> {
        match self {
            Self::Local(sink) => sink.next(zone_id, profile_id).await,
            Self::RemoteAgent(sink) => sink.next(zone_id),
            Self::Sonos(sink) => sink.next(zone_id).await,
            Self::Upnp(sink) => sink.next(zone_id, profile_id).await,
        }
    }

    pub(crate) async fn pause(&self, zone_id: &str) -> Result<(), PlaybackError> {
        match self {
            Self::Local(sink) => sink.pause(zone_id),
            Self::RemoteAgent(sink) => sink.pause(zone_id),
            Self::Sonos(sink) => sink.pause(zone_id).await,
            Self::Upnp(sink) => sink.pause(zone_id).await,
        }
    }

    pub(crate) async fn resume(&self, zone_id: &str) -> Result<(), PlaybackError> {
        match self {
            Self::Local(sink) => sink.resume(zone_id).await,
            Self::RemoteAgent(sink) => sink.resume(zone_id),
            Self::Sonos(sink) => sink.resume(zone_id).await,
            Self::Upnp(sink) => sink.resume(zone_id).await,
        }
    }

    pub(crate) async fn stop(&self, zone_id: &str) -> Result<(), PlaybackError> {
        match self {
            Self::Local(sink) => sink.stop(zone_id),
            Self::RemoteAgent(sink) => sink.stop(zone_id),
            Self::Sonos(sink) => sink.stop(zone_id).await,
            Self::Upnp(sink) => sink.stop(zone_id).await,
        }
    }

    pub(crate) async fn seek(&self, zone_id: &str, seconds: f64) -> Result<(), PlaybackError> {
        match self {
            Self::Local(sink) => sink.seek(zone_id, seconds),
            Self::RemoteAgent(sink) => sink.seek(zone_id, seconds),
            Self::Sonos(sink) => sink.seek(zone_id, seconds).await,
            Self::Upnp(sink) => sink.seek(zone_id, seconds).await,
        }
    }

    pub(crate) fn set_loop_mode(
        &self,
        zone_id: &str,
        mode: &LoopMode,
    ) -> Result<(), PlaybackError> {
        match self {
            Self::Local(sink) => sink.set_loop_mode(zone_id, mode),
            Self::RemoteAgent(sink) => sink.set_loop_mode(zone_id, mode),
            Self::Sonos(sink) => sink.set_loop_mode(zone_id, mode),
            Self::Upnp(sink) => sink.set_loop_mode(zone_id, mode),
        }
    }

    pub(crate) async fn set_volume(&self, zone_id: &str, volume: f32) -> Result<(), PlaybackError> {
        match self {
            Self::Local(sink) => sink.set_volume(zone_id, volume),
            Self::RemoteAgent(sink) => sink.set_volume(zone_id),
            Self::Sonos(sink) => sink.set_volume(zone_id, volume).await,
            Self::Upnp(sink) => sink.set_volume(zone_id, volume).await,
        }
    }

    pub(crate) async fn set_device_volume(
        &self,
        zone_id: &str,
        volume: f32,
    ) -> Result<(), PlaybackError> {
        match self {
            Self::Local(sink) => sink.set_device_volume(zone_id, volume).await,
            Self::RemoteAgent(sink) => sink.set_device_volume(),
            Self::Sonos(sink) => sink.set_device_volume(zone_id, volume).await,
            Self::Upnp(sink) => sink.set_device_volume(zone_id, volume).await,
        }
    }
}
