use super::active_zone_policy::ActiveZonePolicy;
use super::local_device_zone_id;
use super::manager::RemoteSnapshot;
use super::model::short_zone_name;
use super::registry::{AgentEntry, ZoneState};
use crate::protocol::{
    AgentBufferState, AgentCapabilities, AgentPlaybackState, CoreToAgentCommand, SyncSignalPath,
};
use tokio::sync::mpsc;

pub(super) struct AgentZoneBridge;

pub(super) struct AgentCommandDispatch {
    tx: mpsc::UnboundedSender<CoreToAgentCommand>,
    cmd: CoreToAgentCommand,
}

impl AgentCommandDispatch {
    pub(super) fn send(self) -> Result<(), String> {
        self.tx
            .send(self.cmd)
            .map_err(|_| "Remote agent is disconnected".to_string())
    }
}

impl AgentZoneBridge {
    pub(super) fn register_agent(
        state: &mut ZoneState,
        agent_id: String,
        name: String,
        capabilities: AgentCapabilities,
        tx: mpsc::UnboundedSender<CoreToAgentCommand>,
    ) -> u64 {
        state.agent_connection_counter += 1;
        let connection_id = state.agent_connection_counter;
        let output_devices = capabilities.output_devices.clone();
        let browser = capabilities.browser;
        let enabled = !browser;
        if output_devices.is_empty() {
            state.agents.insert(
                agent_id.clone(),
                AgentEntry {
                    agent_id,
                    connection_id,
                    agent_name: name.clone(),
                    name,
                    output_device: None,
                    capabilities,
                    tx,
                    browser,
                    enabled,
                    playback: None,
                    buffer: None,
                    signal_path: None,
                    queued_sources: Vec::new(),
                    prefetched_source: None,
                },
            );
            ActiveZonePolicy::restore_preferred_or_fallback(state);
            return connection_id;
        }
        for device in output_devices {
            let zone_id = format!("{}-{}", agent_id, local_device_zone_id(&device));
            state.agents.insert(
                zone_id,
                AgentEntry {
                    agent_id: agent_id.clone(),
                    connection_id,
                    agent_name: name.clone(),
                    name: format!("{name} - {}", short_zone_name(&device)),
                    output_device: Some(device),
                    capabilities: capabilities.clone(),
                    tx: tx.clone(),
                    browser,
                    enabled,
                    playback: None,
                    buffer: None,
                    signal_path: None,
                    queued_sources: Vec::new(),
                    prefetched_source: None,
                },
            );
        }
        ActiveZonePolicy::restore_preferred_or_fallback(state);
        connection_id
    }

    /// Remove the agent's zones. `connection_id` scopes the removal to the
    /// registration made by one WebSocket connection: an agent that reconnects
    /// re-registers under the same `agent_id` before the dead socket's cleanup
    /// runs, and that cleanup must not tear down the live registration.
    pub(super) fn unregister_agent(
        state: &mut ZoneState,
        agent_id: &str,
        connection_id: Option<u64>,
    ) {
        state.agents.retain(|_, agent| {
            agent.agent_id != agent_id
                || connection_id.is_some_and(|connection| agent.connection_id != connection)
        });
        if !ActiveZonePolicy::active_zone_is_controllable(state) {
            state.active_zone_id = ActiveZonePolicy::first_enabled_zone_id(state);
        }
    }

    pub(super) fn prepare_send_to_zone(
        state: &mut ZoneState,
        zone_id: &str,
        mut cmd: CoreToAgentCommand,
    ) -> Result<AgentCommandDispatch, String> {
        let Some(agent) = state.agents.get_mut(zone_id) else {
            return Err("Zone is not a remote agent".to_string());
        };
        if !agent.enabled {
            return Err("Zone is disabled".to_string());
        }
        attach_agent_output_device(&mut cmd, agent.output_device.clone());
        match &cmd {
            CoreToAgentCommand::PlaySource { queue, .. } => {
                agent.queued_sources = queue.clone();
                agent.prefetched_source = None;
            }
            CoreToAgentCommand::SetQueue { queue } => {
                let keep_prefetch = agent
                    .prefetched_source
                    .as_ref()
                    .is_some_and(|key| queue.first().is_some_and(|source| source.key() == *key));
                agent.queued_sources = queue.clone();
                if !keep_prefetch {
                    agent.prefetched_source = None;
                }
            }
            CoreToAgentCommand::SetLoopMode { .. } => {
                // The native agent may need to move the prefetched stream into or
                // out of the audio engine's gapless queue when repeat-one changes.
                agent.prefetched_source = None;
            }
            _ => {}
        }
        Ok(AgentCommandDispatch {
            tx: agent.tx.clone(),
            cmd,
        })
    }

    pub(super) fn prepare_send_to_agent(
        state: &ZoneState,
        agent_id: &str,
        cmd: CoreToAgentCommand,
    ) -> Result<AgentCommandDispatch, String> {
        let Some(agent) = state
            .agents
            .values()
            .find(|agent| agent.agent_id == agent_id)
        else {
            return Err("Remote agent is disconnected".to_string());
        };
        Ok(AgentCommandDispatch {
            tx: agent.tx.clone(),
            cmd,
        })
    }

    pub(super) fn update_playback(
        state: &mut ZoneState,
        agent_id: &str,
        playback: AgentPlaybackState,
        base_url: &str,
    ) -> Option<CoreToAgentCommand> {
        let zone_ids: Vec<String> = state
            .agents
            .iter()
            .filter(|&(_zone_id, agent)| agent.agent_id == agent_id)
            .map(|(zone_id, _agent)| zone_id.clone())
            .collect();
        let mut prefetch = None;
        for zone_id in zone_ids {
            let Some(agent) = state.agents.get_mut(&zone_id) else {
                continue;
            };
            if let Some(current_source) = playback.current_source.as_ref() {
                let current_key = current_source.key();
                if let Some(index) = agent
                    .queued_sources
                    .iter()
                    .position(|source| source.key() == current_key)
                {
                    agent.queued_sources.drain(..=index);
                }
                if agent.prefetched_source.as_deref() == Some(current_key.as_str()) {
                    agent.prefetched_source = None;
                }
            }
            let remaining = playback.duration_secs - playback.position_secs;
            agent.playback = Some(playback.clone());
            if remaining > 0.0 && remaining <= 15.0 && prefetch.is_none() {
                prefetch = agent.queued_sources.first().and_then(|source| {
                    let key = source.key();
                    if agent.prefetched_source.as_deref() == Some(key.as_str()) {
                        None
                    } else {
                        agent.prefetched_source = Some(key);
                        Some(CoreToAgentCommand::PreFetch {
                            source_ref: source.clone(),
                            stream_base_url: base_url.to_string(),
                        })
                    }
                });
            }
        }
        prefetch
    }

    pub(super) fn update_buffer(state: &mut ZoneState, agent_id: &str, buffer: AgentBufferState) {
        for agent in state
            .agents
            .values_mut()
            .filter(|agent| agent.agent_id == agent_id)
        {
            agent.buffer = Some(buffer.clone());
        }
    }

    pub(super) fn update_signal_path(
        state: &mut ZoneState,
        agent_id: &str,
        signal_path: SyncSignalPath,
    ) {
        for agent in state
            .agents
            .values_mut()
            .filter(|agent| agent.agent_id == agent_id)
        {
            agent.signal_path = Some(signal_path.clone());
        }
    }

    pub(super) fn remote_snapshot_for_zone(
        state: &ZoneState,
        zone_id: &str,
    ) -> Option<RemoteSnapshot> {
        let agent = state.agents.get(zone_id)?;
        Some(RemoteSnapshot {
            playback: agent.playback.clone(),
            signal_path: agent.signal_path.clone(),
            buffer: agent.buffer.clone(),
        })
    }
}

fn attach_agent_output_device(cmd: &mut CoreToAgentCommand, output_device: Option<String>) {
    let Some(output_device) = output_device else {
        return;
    };
    match cmd {
        CoreToAgentCommand::PlaySource {
            playback_config, ..
        }
        | CoreToAgentCommand::SetPlaybackConfig { playback_config } => {
            playback_config.output_device = Some(output_device);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::eq::EqConfig;
    use crate::audio::player::Player;
    use crate::protocol::{AgentCapabilities, PlaybackConfig, SourceRef};
    use crate::zones::ZoneManager;
    use std::sync::Arc;

    #[test]
    fn attach_agent_output_device_updates_only_playback_config_commands() {
        let mut play = CoreToAgentCommand::PlaySource {
            source_ref: qobuz_source(),
            queue: Vec::new(),
            playback_config: playback_config(),
            stream_base_url: "http://core.test".to_string(),
        };
        attach_agent_output_device(&mut play, Some("ASIO: Brooklyn DAC+".to_string()));
        match play {
            CoreToAgentCommand::PlaySource {
                playback_config, ..
            } => {
                assert_eq!(
                    playback_config.output_device.as_deref(),
                    Some("ASIO: Brooklyn DAC+")
                );
            }
            _ => panic!("expected play source command"),
        }

        let mut config = CoreToAgentCommand::SetPlaybackConfig {
            playback_config: playback_config(),
        };
        attach_agent_output_device(&mut config, Some("Speakers (Agent DAC)".to_string()));
        match config {
            CoreToAgentCommand::SetPlaybackConfig { playback_config } => {
                assert_eq!(
                    playback_config.output_device.as_deref(),
                    Some("Speakers (Agent DAC)")
                );
            }
            _ => panic!("expected playback config command"),
        }

        let mut pause = CoreToAgentCommand::Pause;
        attach_agent_output_device(&mut pause, Some("Ignored DAC".to_string()));
        assert!(matches!(pause, CoreToAgentCommand::Pause));
    }

    #[test]
    fn reported_source_advance_consumes_agent_queue_before_prefetch() {
        let manager = ZoneManager::new(Arc::new(Player::new()), None);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        manager.register_agent(
            "agent-queue-sync".to_string(),
            "Queue Agent".to_string(),
            AgentCapabilities {
                output_devices: Vec::new(),
                output_device_capabilities: Vec::new(),
                max_sample_rate: 48_000,
                max_bit_depth: 24,
                exclusive_supported: false,
                supports_dsd128: false,
                supports_dsd256: false,
                browser: false,
            },
            tx,
        );
        let first = qobuz_source_with_id(1);
        let second = qobuz_source_with_id(2);
        let third = qobuz_source_with_id(3);
        manager
            .send_to_zone(
                "agent-queue-sync",
                CoreToAgentCommand::PlaySource {
                    source_ref: first,
                    queue: vec![second.clone(), third.clone()],
                    playback_config: playback_config(),
                    stream_base_url: "http://core.test".to_string(),
                },
            )
            .unwrap();
        assert!(matches!(
            rx.try_recv(),
            Ok(CoreToAgentCommand::PlaySource { .. })
        ));

        manager.update_playback(
            "agent-queue-sync",
            AgentPlaybackState {
                state: "Playing".to_string(),
                current_source: Some(second),
                position_secs: 171.0,
                duration_secs: 180.0,
                ..AgentPlaybackState::default()
            },
            "http://core.test",
        );

        assert!(matches!(
            rx.try_recv(),
            Ok(CoreToAgentCommand::PreFetch { source_ref, .. }) if source_ref.key() == third.key()
        ));
    }

    #[test]
    fn remote_queue_replacement_invalidates_stale_prefetch() {
        let manager = ZoneManager::new(Arc::new(Player::new()), None);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        manager.register_agent(
            "agent-queue-replace".to_string(),
            "Queue Agent".to_string(),
            AgentCapabilities {
                output_devices: Vec::new(),
                output_device_capabilities: Vec::new(),
                max_sample_rate: 48_000,
                max_bit_depth: 24,
                exclusive_supported: false,
                supports_dsd128: false,
                supports_dsd256: false,
                browser: false,
            },
            tx,
        );
        let first = qobuz_source_with_id(1);
        let stale_next = qobuz_source_with_id(2);
        let replacement = qobuz_source_with_id(3);
        manager
            .send_to_zone(
                "agent-queue-replace",
                CoreToAgentCommand::PlaySource {
                    source_ref: first.clone(),
                    queue: vec![stale_next],
                    playback_config: playback_config(),
                    stream_base_url: "http://core.test".to_string(),
                },
            )
            .unwrap();
        let _ = rx.try_recv();
        manager.update_playback(
            "agent-queue-replace",
            AgentPlaybackState {
                state: "Playing".to_string(),
                current_source: Some(first.clone()),
                position_secs: 171.0,
                duration_secs: 180.0,
                ..AgentPlaybackState::default()
            },
            "http://core.test",
        );
        assert!(matches!(
            rx.try_recv(),
            Ok(CoreToAgentCommand::PreFetch { .. })
        ));

        manager
            .send_to_zone(
                "agent-queue-replace",
                CoreToAgentCommand::SetQueue {
                    queue: vec![replacement.clone()],
                },
            )
            .unwrap();
        assert!(matches!(
            rx.try_recv(),
            Ok(CoreToAgentCommand::SetQueue { .. })
        ));
        manager.update_playback(
            "agent-queue-replace",
            AgentPlaybackState {
                state: "Playing".to_string(),
                current_source: Some(first),
                position_secs: 172.0,
                duration_secs: 180.0,
                ..AgentPlaybackState::default()
            },
            "http://core.test",
        );

        assert!(matches!(
            rx.try_recv(),
            Ok(CoreToAgentCommand::PreFetch { source_ref, .. })
                if source_ref.key() == replacement.key()
        ));
    }

    fn playback_config() -> PlaybackConfig {
        PlaybackConfig {
            filter_type: "SincBest".to_string(),
            target_rate: 192_000,
            target_bit_depth: 24,
            upsampling_enabled: true,
            exclusive: true,
            dither_mode: "Auto".to_string(),
            output_mode: "Pcm".to_string(),
            dsd_modulator: "Standard".to_string(),
            dsd_isi_penalty: 0.0,
            dsd_rules: Vec::new(),
            headroom_db: 0.0,
            dsp_buffer_ms: 0,
            volume: 1.0,
            eq: EqConfig::default(),
            output_device: None,
        }
    }

    fn qobuz_source() -> SourceRef {
        qobuz_source_with_id(42)
    }

    fn qobuz_source_with_id(track_id: u64) -> SourceRef {
        SourceRef::QobuzTrack {
            track_id,
            title: Some("Track".to_string()),
            artist: Some("Artist".to_string()),
            album: Some("Album".to_string()),
            album_id: Some("album".to_string()),
            image_url: None,
            duration_secs: Some(180.0),
            radio: false,
            radio_context: None,
            playlist_context: None,
        }
    }
}
