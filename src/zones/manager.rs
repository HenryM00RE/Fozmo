use super::LOCAL_ZONE_ID;
use super::active_zone_policy::ActiveZonePolicy;
use super::agent_bridge::AgentZoneBridge;
use super::capabilities::local_protocol;
use super::local_device_zone_id;
use super::model::{default_zone_name, normalized_zone_id, short_zone_name};
use super::persistence::ZonePersistence;
use super::registry::{LocalZoneEntry, ZoneRegistry};
use super::snapshot::ZoneSnapshotBuilder;
use crate::audio::output::device_caps::AudioDeviceCapabilities;
use crate::audio::player::Player;
use crate::audio::{airplay, sonos, upnp};
use crate::library::ZoneDefinition;
use crate::protocol::{
    AgentBufferState, AgentCapabilities, AgentPlaybackState, BrowserStreamSignal,
    CoreToAgentCommand, SinkProtocol, SyncSignalPath, ZoneProfile,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

#[derive(Clone)]
pub struct ZoneManager {
    inner: Arc<Mutex<ZoneRegistry>>,
    /// Last server-side stream chain served per browser zone, for the
    /// signal-path UI. Keyed by zone (= agent) id.
    browser_stream_signals: Arc<Mutex<HashMap<String, BrowserStreamSignal>>>,
}

#[derive(Debug, Clone, Default)]
pub struct RemoteSnapshot {
    pub playback: Option<AgentPlaybackState>,
    pub signal_path: Option<SyncSignalPath>,
    pub buffer: Option<AgentBufferState>,
}

impl ZoneManager {
    pub fn new(local_player: Arc<Player>, preferred_active_zone_id: Option<String>) -> Self {
        let mut local_zones = HashMap::new();
        local_zones.insert(
            LOCAL_ZONE_ID.to_string(),
            LocalZoneEntry {
                name: default_zone_name(),
                device_name: None,
                player: local_player,
                enabled: true,
                online: true,
                status_message: None,
            },
        );
        let preferred_active_zone_id = preferred_active_zone_id
            .and_then(|id| normalized_zone_id(&id))
            .or_else(|| Some(LOCAL_ZONE_ID.to_string()));
        Self {
            inner: Arc::new(Mutex::new(ZoneRegistry {
                active_zone_id: LOCAL_ZONE_ID.to_string(),
                preferred_active_zone_id,
                local_zones,
                local_device_capabilities: HashMap::new(),
                agents: HashMap::new(),
                agent_connection_counter: 0,
            })),
            browser_stream_signals: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Record the server-side stream chain last served to a browser zone.
    pub fn note_browser_stream_signal(&self, zone_id: &str, signal: BrowserStreamSignal) {
        self.browser_stream_signals
            .lock()
            .unwrap()
            .insert(zone_id.to_string(), signal);
    }

    pub fn browser_stream_signal(&self, zone_id: &str) -> Option<BrowserStreamSignal> {
        self.browser_stream_signals
            .lock()
            .unwrap()
            .get(zone_id)
            .cloned()
    }

    pub fn list_zones(&self) -> Vec<ZoneProfile> {
        let guard = self.inner.lock().unwrap();
        ZoneSnapshotBuilder::build(&guard)
    }

    pub fn cache_local_device_capabilities(
        &self,
        name: &str,
        capabilities: AudioDeviceCapabilities,
    ) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        self.inner
            .lock()
            .unwrap()
            .local_device_capabilities
            .insert(name.to_string(), capabilities);
    }

    pub fn sync_local_devices(&self, devices: Vec<String>) {
        let mut guard = self.inner.lock().unwrap();
        for zone in guard.local_zones.values_mut() {
            if zone.device_name.as_deref().is_some_and(|device| {
                !airplay::is_airplay_device_name(device) && !sonos::is_sonos_device_name(device)
            }) {
                // A running player or owned CoreAudio DoP AudioUnit is stronger
                // evidence than a transient device-enumeration miss. In
                // particular, CoreAudio can omit a hogged USB DAC while the
                // output is opening or draining at a track boundary.
                zone.online = ActiveZonePolicy::local_zone_has_running_or_owned_output(zone);
                zone.status_message = None;
            }
        }
        for name in devices {
            let id = local_device_zone_id(&name);
            guard
                .local_zones
                .entry(id.clone())
                .or_insert_with(|| LocalZoneEntry {
                    name: short_zone_name(&name),
                    device_name: Some(name),
                    player: Arc::new(Player::new()),
                    enabled: false,
                    online: true,
                    status_message: None,
                });
            if let Some(zone) = guard.local_zones.get_mut(&id) {
                zone.online = true;
                zone.status_message = None;
            }
        }
        ActiveZonePolicy::restore_preferred_or_fallback(&mut guard);
    }

    pub fn coreaudio_dop_output_owned(&self) -> bool {
        self.inner.lock().unwrap().local_zones.values().any(|zone| {
            zone.player.output_transport() == crate::audio::player::OutputTransport::DopCoreAudio
        })
    }

    pub fn sync_standby_local_zone(
        &self,
        zone_id: &str,
        name: &str,
        device_name: &str,
        status_message: &str,
    ) {
        let mut guard = self.inner.lock().unwrap();
        let zone = guard
            .local_zones
            .entry(zone_id.to_string())
            .or_insert_with(|| LocalZoneEntry {
                name: name.to_string(),
                device_name: Some(device_name.to_string()),
                player: Arc::new(Player::new()),
                enabled: true,
                online: false,
                status_message: None,
            });
        let was_online = zone.online;
        zone.device_name = Some(device_name.to_string());
        zone.enabled = true;
        zone.online = true;
        if !was_online {
            zone.status_message = Some(status_message.to_string());
        }
        ActiveZonePolicy::restore_preferred_or_fallback(&mut guard);
    }

    pub fn sync_saved_local_zone(
        &self,
        zone_id: &str,
        name: &str,
        device_name: &str,
        enabled: bool,
        status_message: &str,
    ) {
        let mut guard = self.inner.lock().unwrap();
        let zone = guard
            .local_zones
            .entry(zone_id.to_string())
            .or_insert_with(|| LocalZoneEntry {
                name: name.to_string(),
                device_name: Some(device_name.to_string()),
                player: Arc::new(Player::new()),
                enabled,
                online: false,
                status_message: Some(status_message.to_string()),
            });
        zone.name = name.to_string();
        zone.device_name = Some(device_name.to_string());
        zone.enabled = enabled;
        if !zone.online {
            zone.status_message = Some(status_message.to_string());
        }
        ActiveZonePolicy::restore_preferred_or_fallback(&mut guard);
    }

    pub fn sync_airplay_receivers(&self, receivers: Vec<airplay::AirPlayReceiver>) {
        let mut guard = self.inner.lock().unwrap();
        for zone in guard.local_zones.values_mut() {
            if zone
                .device_name
                .as_deref()
                .is_some_and(airplay::is_airplay_device_name)
            {
                zone.online = false;
            }
        }
        for receiver in receivers {
            let id = airplay::receiver_zone_id(&receiver.target.id);
            let device_name = airplay::target_device_name(&receiver.target);
            guard
                .local_zones
                .entry(id.clone())
                .or_insert_with(|| LocalZoneEntry {
                    name: receiver.target.name.clone(),
                    device_name: Some(device_name.clone()),
                    player: Arc::new(Player::new()),
                    enabled: false,
                    online: receiver.online,
                    status_message: None,
                });
            if let Some(zone) = guard.local_zones.get_mut(&id) {
                zone.device_name = Some(device_name);
                zone.online = receiver.online;
                zone.status_message = None;
            }
        }
        ActiveZonePolicy::restore_preferred_or_fallback(&mut guard);
    }

    pub fn sync_sonos_speakers(&self, speakers: Vec<sonos::SonosSpeaker>) {
        let mut guard = self.inner.lock().unwrap();
        for zone in guard.local_zones.values_mut() {
            if zone
                .device_name
                .as_deref()
                .is_some_and(sonos::is_sonos_device_name)
            {
                zone.online = false;
            }
        }
        for speaker in speakers {
            let id = sonos::receiver_zone_id(&speaker.target.id);
            let device_name = sonos::target_device_name(&speaker.target);
            guard
                .local_zones
                .entry(id.clone())
                .or_insert_with(|| LocalZoneEntry {
                    name: speaker.target.name.clone(),
                    device_name: Some(device_name.clone()),
                    player: Arc::new(Player::new()),
                    enabled: false,
                    online: speaker.online,
                    status_message: None,
                });
            if let Some(zone) = guard.local_zones.get_mut(&id) {
                zone.device_name = Some(device_name);
                zone.online = speaker.online;
                zone.status_message = None;
            }
        }
        ActiveZonePolicy::restore_preferred_or_fallback(&mut guard);
    }

    pub fn sync_upnp_renderers(&self, renderers: Vec<upnp::UpnpRenderer>) {
        let mut guard = self.inner.lock().unwrap();
        for zone in guard.local_zones.values_mut() {
            if zone
                .device_name
                .as_deref()
                .is_some_and(upnp::is_upnp_device_name)
            {
                zone.online = false;
            }
        }
        for renderer in renderers {
            let id = upnp::receiver_zone_id(&renderer.target.id);
            let device_name = upnp::target_device_name(&renderer.target);
            guard
                .local_zones
                .entry(id.clone())
                .or_insert_with(|| LocalZoneEntry {
                    name: renderer.target.name.clone(),
                    device_name: Some(device_name.clone()),
                    player: Arc::new(Player::new()),
                    enabled: false,
                    online: renderer.online,
                    status_message: None,
                });
            if let Some(zone) = guard.local_zones.get_mut(&id) {
                zone.device_name = Some(device_name);
                zone.online = renderer.online;
                zone.status_message = None;
            }
        }
        ActiveZonePolicy::restore_preferred_or_fallback(&mut guard);
    }

    pub fn apply_zone_definitions(&self, definitions: Vec<ZoneDefinition>) {
        let mut guard = self.inner.lock().unwrap();
        ZonePersistence::apply_definitions(&mut guard, definitions);
    }

    pub fn rename_zone(&self, zone_id: &str, name: &str) -> Result<(), String> {
        let trimmed = name.trim();
        if trimmed.is_empty() || trimmed.len() > 80 {
            return Err("Zone name must be 1-80 characters".to_string());
        }
        let mut guard = self.inner.lock().unwrap();
        if let Some(zone) = guard.local_zones.get_mut(zone_id) {
            zone.name = trimmed.to_string();
            Ok(())
        } else if let Some(agent) = guard.agents.get_mut(zone_id) {
            agent.name = trimmed.to_string();
            Ok(())
        } else {
            Err(format!("Zone '{zone_id}' is not available"))
        }
    }

    pub fn enable_zone(&self, zone_id: &str) -> Result<(), String> {
        let mut guard = self.inner.lock().unwrap();
        let Some(zone) = guard.local_zones.get_mut(zone_id) else {
            if let Some(agent) = guard.agents.get_mut(zone_id) {
                agent.enabled = true;
                if !agent.browser {
                    guard.preferred_active_zone_id = Some(zone_id.to_string());
                    ActiveZonePolicy::activate_zone_unchecked(&mut guard, zone_id);
                }
                return Ok(());
            }
            return Err(format!("Zone '{zone_id}' is not available"));
        };
        if !zone.online {
            return Err(format!("Zone '{zone_id}' is offline"));
        }
        if let Some(reason) = ActiveZonePolicy::local_zone_airplay_unsupported_reason(zone) {
            return Err(reason.to_string());
        }
        zone.enabled = true;
        if let Some(device) = zone.device_name.clone()
            && !sonos::is_sonos_device_name(&device)
            && !upnp::is_upnp_device_name(&device)
        {
            zone.player.select_device(Some(device));
        }
        guard.preferred_active_zone_id = Some(zone_id.to_string());
        guard.active_zone_id = zone_id.to_string();
        Ok(())
    }

    pub fn disable_zone(&self, zone_id: &str) -> Result<(), String> {
        let mut guard = self.inner.lock().unwrap();
        if let Some(zone) = guard.local_zones.get_mut(zone_id) {
            zone.enabled = false;
            zone.player.stop();
            if guard.active_zone_id == zone_id {
                guard.active_zone_id = ActiveZonePolicy::first_enabled_zone_id(&guard);
            }
            if guard.preferred_active_zone_id.as_deref() == Some(zone_id) {
                guard.preferred_active_zone_id = Some(guard.active_zone_id.clone());
            }
            return Ok(());
        }
        let disabling_active = guard.active_zone_id == zone_id;
        let stop_tx = {
            let Some(agent) = guard.agents.get_mut(zone_id) else {
                return Err(format!("Zone '{zone_id}' is not available"));
            };
            let stop_on_disable = disabling_active || agent.browser;
            agent.enabled = false;
            stop_on_disable.then(|| agent.tx.clone())
        };
        if disabling_active {
            guard.active_zone_id = ActiveZonePolicy::first_enabled_zone_id(&guard);
        }
        if guard.preferred_active_zone_id.as_deref() == Some(zone_id) {
            guard.preferred_active_zone_id = Some(guard.active_zone_id.clone());
        }
        drop(guard);
        if let Some(tx) = stop_tx {
            let _ = tx.send(CoreToAgentCommand::Stop);
        }
        Ok(())
    }

    pub fn active_zone_id(&self) -> String {
        self.inner.lock().unwrap().active_zone_id.clone()
    }

    pub fn preferred_active_zone_id(&self) -> String {
        let guard = self.inner.lock().unwrap();
        guard
            .preferred_active_zone_id
            .clone()
            .unwrap_or_else(|| guard.active_zone_id.clone())
    }

    pub fn zone_name(&self, zone_id: &str) -> String {
        let guard = self.inner.lock().unwrap();
        guard
            .local_zones
            .get(zone_id)
            .map(|zone| zone.name.clone())
            .or_else(|| guard.agents.get(zone_id).map(|agent| agent.name.clone()))
            .unwrap_or_else(|| "Remote zone".to_string())
    }

    pub fn zone_bound_device_name(&self, zone_id: &str) -> Option<String> {
        let guard = self.inner.lock().unwrap();
        guard
            .local_zones
            .get(zone_id)
            .and_then(|zone| zone.device_name.clone())
            .or_else(|| {
                guard
                    .agents
                    .get(zone_id)
                    .and_then(|agent| agent.output_device.clone())
            })
    }

    pub fn known_local_output_device_name(&self, name: &str) -> bool {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return false;
        }
        let guard = self.inner.lock().unwrap();
        guard
            .local_zones
            .values()
            .any(|zone| zone.online && zone.device_name.as_deref().map(str::trim) == Some(trimmed))
    }

    pub fn zone_protocol(&self, zone_id: &str) -> Option<SinkProtocol> {
        let guard = self.inner.lock().unwrap();
        if let Some(zone) = guard.local_zones.get(zone_id) {
            Some(local_protocol(zone.device_name.as_deref()))
        } else if guard.agents.contains_key(zone_id) {
            Some(SinkProtocol::RemoteAgent)
        } else {
            None
        }
    }

    pub fn active_player(&self) -> Arc<Player> {
        let guard = self.inner.lock().unwrap();
        guard
            .local_zones
            .get(&guard.active_zone_id)
            .or_else(|| guard.local_zones.get(LOCAL_ZONE_ID))
            .expect("local core zone should exist")
            .player
            .clone()
    }

    pub fn player_for_zone(&self, zone_id: &str) -> Option<Arc<Player>> {
        self.inner
            .lock()
            .unwrap()
            .local_zones
            .get(zone_id)
            .filter(|zone| ActiveZonePolicy::local_zone_is_controllable(zone))
            .map(|zone| zone.player.clone())
    }

    pub fn select_zone(&self, zone_id: &str) -> Result<(), String> {
        let mut guard = self.inner.lock().unwrap();
        if guard.agents.get(zone_id).is_some_and(|agent| agent.browser) {
            return Err("Browser outputs cannot become the shared active zone".to_string());
        }
        if ActiveZonePolicy::zone_is_controllable(&guard, zone_id) {
            guard.preferred_active_zone_id = Some(zone_id.to_string());
            ActiveZonePolicy::activate_zone_unchecked(&mut guard, zone_id);
            Ok(())
        } else {
            Err(format!("Zone '{zone_id}' is not available"))
        }
    }

    /// Registers (or re-registers) an agent's zones and returns the
    /// connection id identifying this registration.
    pub fn register_agent(
        &self,
        agent_id: String,
        name: String,
        capabilities: AgentCapabilities,
        tx: mpsc::UnboundedSender<CoreToAgentCommand>,
    ) -> u64 {
        let mut guard = self.inner.lock().unwrap();
        AgentZoneBridge::register_agent(&mut guard, agent_id, name, capabilities, tx)
    }

    /// Unregisters the agent's zones regardless of which connection made the
    /// registration.
    #[allow(dead_code)]
    pub fn unregister_agent(&self, agent_id: &str) {
        let mut guard = self.inner.lock().unwrap();
        AgentZoneBridge::unregister_agent(&mut guard, agent_id, None);
    }

    /// Unregisters the agent's zones only when they still belong to the
    /// registration identified by `connection_id`, so a stale socket's
    /// cleanup cannot remove a newer connection's zones.
    pub fn unregister_agent_connection(&self, agent_id: &str, connection_id: u64) {
        let mut guard = self.inner.lock().unwrap();
        AgentZoneBridge::unregister_agent(&mut guard, agent_id, Some(connection_id));
    }

    pub fn send_to_zone(&self, zone_id: &str, cmd: CoreToAgentCommand) -> Result<(), String> {
        let dispatch = {
            let mut guard = self.inner.lock().unwrap();
            AgentZoneBridge::prepare_send_to_zone(&mut guard, zone_id, cmd)?
        };
        dispatch.send()
    }

    pub fn update_playback(&self, agent_id: &str, playback: AgentPlaybackState, base_url: &str) {
        let prefetch_cmd = {
            let mut guard = self.inner.lock().unwrap();
            AgentZoneBridge::update_playback(&mut guard, agent_id, playback, base_url)
        };

        if let Some(cmd) = prefetch_cmd {
            let _ = self.send_to_agent(agent_id, cmd);
        }
    }

    pub fn update_buffer(&self, agent_id: &str, buffer: AgentBufferState) {
        let mut guard = self.inner.lock().unwrap();
        AgentZoneBridge::update_buffer(&mut guard, agent_id, buffer);
    }

    pub fn update_signal_path(&self, agent_id: &str, signal_path: SyncSignalPath) {
        let mut guard = self.inner.lock().unwrap();
        AgentZoneBridge::update_signal_path(&mut guard, agent_id, signal_path);
    }

    pub fn remote_snapshot_for_zone(&self, zone_id: &str) -> Option<RemoteSnapshot> {
        let guard = self.inner.lock().unwrap();
        AgentZoneBridge::remote_snapshot_for_zone(&guard, zone_id)
    }

    /// The owning agent id when `zone_id` is a browser-private zone.
    pub fn browser_zone_agent_id(&self, zone_id: &str) -> Option<String> {
        let guard = self.inner.lock().unwrap();
        guard
            .agents
            .get(zone_id)
            .filter(|agent| agent.browser)
            .map(|agent| agent.agent_id.clone())
    }

    fn send_to_agent(&self, agent_id: &str, cmd: CoreToAgentCommand) -> Result<(), String> {
        let dispatch = {
            let guard = self.inner.lock().unwrap();
            AgentZoneBridge::prepare_send_to_agent(&guard, agent_id, cmd)?
        };
        dispatch.send()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::airplay::{AirPlayReceiver, AirPlayServiceKind, AirPlayTarget};
    use crate::audio::player::OutputMode;
    use crate::protocol::ZoneStatus;
    use crate::settings::DsdSourceRule;
    use crate::zones::current_playback_config;

    fn airplay_target(id: &str) -> AirPlayTarget {
        AirPlayTarget {
            id: id.to_string(),
            name: "Living Room".to_string(),
            service_kind: AirPlayServiceKind::Raop,
            supported: true,
            unsupported_reason: None,
        }
    }

    fn airplay_receiver(target: AirPlayTarget, online: bool) -> AirPlayReceiver {
        AirPlayReceiver { target, online }
    }

    fn wait_for_selected_device(player: &Player, expected: &str) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while std::time::Instant::now() < deadline {
            if player.selected_device_name().as_deref() == Some(expected) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(player.selected_device_name().as_deref(), Some(expected));
    }

    fn assert_selected_device_stays(player: &Player, expected: &str, _forbidden: &str) {
        for _ in 0..20 {
            let selected = player.selected_device_name();
            assert_eq!(selected.as_deref(), Some(expected));
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    fn spoofed_cleartext_airplay_target(original: &AirPlayTarget) -> AirPlayTarget {
        AirPlayTarget {
            name: "Spoofed display name".to_string(),
            service_kind: AirPlayServiceKind::Raop,
            ..original.clone()
        }
    }

    fn airplay2_target(id: &str) -> AirPlayTarget {
        AirPlayTarget {
            service_kind: AirPlayServiceKind::AirPlay2,
            ..airplay_target(id)
        }
    }

    fn sonos_target(id: &str) -> sonos::SonosTarget {
        sonos::SonosTarget {
            id: id.to_string(),
            name: "Sonos".to_string(),
            host: "127.0.0.1".to_string(),
            port: 1400,
            model: Some("Sonos Five".to_string()),
            coordinator: true,
            group_name: None,
        }
    }

    fn single_agent_capabilities(device: &str) -> AgentCapabilities {
        AgentCapabilities {
            output_devices: vec![device.to_string()],
            output_device_capabilities: vec![crate::protocol::OutputDeviceCapabilities {
                name: device.to_string(),
                backend: Some("wasapi".to_string()),
                max_sample_rate: 192_000,
                max_bit_depth: 32,
                supports_dsd128: false,
                supports_dsd256: false,
            }],
            max_sample_rate: 192_000,
            max_bit_depth: 32,
            exclusive_supported: true,
            supports_dsd128: false,
            supports_dsd256: false,
            browser: false,
        }
    }

    #[test]
    fn active_airplay_zone_remains_controllable_when_discovery_goes_offline() {
        let manager = ZoneManager::new(Arc::new(Player::new()), None);
        let target = airplay_target("aa:bb:cc:dd:ee:ff");
        let zone_id = airplay::receiver_zone_id(&target.id);

        manager.sync_airplay_receivers(vec![airplay_receiver(target.clone(), true)]);
        manager.enable_zone(&zone_id).unwrap();
        let player = manager.player_for_zone(&zone_id).unwrap();
        player.set_playback_state_for_test(crate::audio::player::PlaybackState::Paused);

        manager.sync_airplay_receivers(vec![airplay_receiver(target, false)]);

        assert_eq!(manager.active_zone_id(), zone_id);
        assert!(manager.player_for_zone(&zone_id).is_some());
        let zone = manager
            .list_zones()
            .into_iter()
            .find(|zone| zone.id == zone_id)
            .unwrap();
        assert_eq!(zone.status, ZoneStatus::Active);
    }

    #[test]
    fn offline_non_stopped_local_zone_can_be_reselected() {
        let manager = ZoneManager::new(Arc::new(Player::new()), None);
        let target = airplay_target("aa:bb:cc:dd:ee:ff");
        let zone_id = airplay::receiver_zone_id(&target.id);

        manager.sync_airplay_receivers(vec![airplay_receiver(target.clone(), true)]);
        manager.enable_zone(&zone_id).unwrap();
        let player = manager.player_for_zone(&zone_id).unwrap();
        player.set_playback_state_for_test(crate::audio::player::PlaybackState::Playing);
        manager.sync_airplay_receivers(vec![airplay_receiver(target, false)]);

        manager.select_zone(LOCAL_ZONE_ID).unwrap();
        assert_eq!(manager.active_zone_id(), LOCAL_ZONE_ID);

        manager.select_zone(&zone_id).unwrap();

        assert_eq!(manager.active_zone_id(), zone_id);
    }

    #[test]
    fn stopped_airplay_zone_falls_back_when_discovery_goes_offline() {
        let manager = ZoneManager::new(Arc::new(Player::new()), None);
        let target = airplay_target("11:22:33:44:55:66");
        let zone_id = airplay::receiver_zone_id(&target.id);

        manager.sync_airplay_receivers(vec![airplay_receiver(target.clone(), true)]);
        manager.enable_zone(&zone_id).unwrap();

        manager.sync_airplay_receivers(vec![airplay_receiver(target, false)]);

        assert_eq!(manager.active_zone_id(), LOCAL_ZONE_ID);
        assert!(manager.player_for_zone(&zone_id).is_none());
    }

    #[test]
    fn airplay_discovery_update_does_not_reselect_enabled_player() {
        let manager = ZoneManager::new(Arc::new(Player::new()), None);
        let target = airplay_target("22:33:44:55:66:77");
        let zone_id = airplay::receiver_zone_id(&target.id);
        let original_device_name = airplay::target_device_name(&target);
        let spoofed = spoofed_cleartext_airplay_target(&target);
        let spoofed_device_name = airplay::target_device_name(&spoofed);

        manager.sync_airplay_receivers(vec![airplay_receiver(target, true)]);
        manager.enable_zone(&zone_id).unwrap();
        let player = manager.player_for_zone(&zone_id).unwrap();

        wait_for_selected_device(&player, &original_device_name);

        manager.sync_airplay_receivers(vec![airplay_receiver(spoofed, true)]);

        assert_eq!(
            manager.zone_bound_device_name(&zone_id).as_deref(),
            Some(spoofed_device_name.as_str())
        );
        assert_selected_device_stays(&player, &original_device_name, &spoofed_device_name);
    }

    #[test]
    fn airplay2_registry_choice_binds_airplay2_zone() {
        let manager = ZoneManager::new(Arc::new(Player::new()), None);
        let airplay2 = airplay2_target("33:44:55:66:77:88");
        let spoofed = spoofed_cleartext_airplay_target(&airplay2);
        let zone_id = airplay::receiver_zone_id(&airplay2.id);
        let airplay2_device_name = airplay::target_device_name(&airplay2);
        let spoofed_device_name = airplay::target_device_name(&spoofed);

        manager.sync_airplay_receivers(vec![airplay_receiver(airplay2, true)]);
        manager.enable_zone(&zone_id).unwrap();
        let player = manager.player_for_zone(&zone_id).unwrap();

        assert_eq!(
            manager.zone_bound_device_name(&zone_id).as_deref(),
            Some(airplay2_device_name.as_str())
        );
        wait_for_selected_device(&player, &airplay2_device_name);
        assert_selected_device_stays(&player, &airplay2_device_name, &spoofed_device_name);
    }

    #[test]
    fn preferred_sonos_does_not_steal_active_local_zone_when_it_reappears() {
        let target = sonos_target("RINCON_TEST");
        let sonos_zone_id = sonos::receiver_zone_id(&target.id);
        let hegel_device = "Hegel H390 USB";
        let hegel_zone_id = local_device_zone_id(hegel_device);
        let manager = ZoneManager::new(Arc::new(Player::new()), Some(sonos_zone_id.clone()));

        manager.sync_local_devices(vec![hegel_device.to_string()]);
        manager.apply_zone_definitions(vec![
            ZoneDefinition {
                id: LOCAL_ZONE_ID.to_string(),
                name: "Default".to_string(),
                kind: Some("local_coreaudio".to_string()),
                device_name: None,
                enabled: false,
            },
            ZoneDefinition {
                id: hegel_zone_id.clone(),
                name: "Hegel H390 USB".to_string(),
                kind: Some("local_coreaudio".to_string()),
                device_name: Some(hegel_device.to_string()),
                enabled: true,
            },
        ]);

        assert_eq!(manager.active_zone_id(), hegel_zone_id);

        manager.sync_sonos_speakers(vec![sonos::SonosSpeaker {
            target,
            online: true,
        }]);
        manager.apply_zone_definitions(vec![ZoneDefinition {
            id: sonos_zone_id,
            name: "Sonos".to_string(),
            kind: Some("sonos_upnp".to_string()),
            device_name: None,
            enabled: true,
        }]);

        assert_eq!(manager.active_zone_id(), hegel_zone_id);
    }

    #[test]
    fn missing_local_devices_are_marked_offline_on_refresh() {
        let hegel_device = "Hegel H390 USB";
        let hegel_zone_id = local_device_zone_id(hegel_device);
        let manager = ZoneManager::new(Arc::new(Player::new()), Some(hegel_zone_id.clone()));

        manager.sync_local_devices(vec![hegel_device.to_string()]);
        manager.apply_zone_definitions(vec![ZoneDefinition {
            id: hegel_zone_id.clone(),
            name: "Hegel H390 USB".to_string(),
            kind: Some("local_coreaudio".to_string()),
            device_name: Some(hegel_device.to_string()),
            enabled: true,
        }]);

        assert_eq!(manager.active_zone_id(), hegel_zone_id);

        manager.sync_local_devices(Vec::new());

        assert_eq!(manager.active_zone_id(), LOCAL_ZONE_ID);
        let zone = manager
            .list_zones()
            .into_iter()
            .find(|zone| zone.id == hegel_zone_id)
            .unwrap();
        assert_eq!(zone.status, ZoneStatus::Offline);
    }

    #[test]
    fn discovery_miss_during_playback_does_not_strand_local_zone_at_eof() {
        let device_name = "Hegel H390 USB";
        let zone_id = local_device_zone_id(device_name);
        let manager = ZoneManager::new(Arc::new(Player::new()), None);

        manager.sync_local_devices(vec![device_name.to_string()]);
        manager.enable_zone(&zone_id).unwrap();
        let player = manager.player_for_zone(&zone_id).unwrap();
        player.set_playback_state_for_test(crate::audio::player::PlaybackState::Playing);

        // Simulate CoreAudio omitting the DAC while startup/Hog Mode races a
        // background discovery pass.
        manager.sync_local_devices(Vec::new());
        player.set_playback_state_for_test(crate::audio::player::PlaybackState::Stopped);

        assert_eq!(manager.active_zone_id(), zone_id);
        assert!(manager.player_for_zone(&zone_id).is_some());
        let zone = manager
            .list_zones()
            .into_iter()
            .find(|zone| zone.id == zone_id)
            .unwrap();
        assert_eq!(zone.status, ZoneStatus::Active);
    }

    #[test]
    fn owned_dop_output_recovers_controllability_after_discovery_race() {
        let device_name = "Hegel H390 USB";
        let zone_id = local_device_zone_id(device_name);
        let manager = ZoneManager::new(Arc::new(Player::new()), None);

        manager.sync_local_devices(vec![device_name.to_string()]);
        manager.enable_zone(&zone_id).unwrap();
        let player = manager.player_for_zone(&zone_id).unwrap();

        // Discovery wins the race first and marks the stopped zone offline;
        // the AudioUnit then finishes opening and owns the device.
        manager.sync_local_devices(Vec::new());
        player.set_coreaudio_dop_buffer_health_for_test(5_644_800, 65_536, 0, 32_768, 4096);

        assert!(manager.coreaudio_dop_output_owned());
        assert!(manager.player_for_zone(&zone_id).is_some());
        manager.select_zone(&zone_id).unwrap();
        assert_eq!(manager.active_zone_id(), zone_id);
    }

    #[test]
    fn local_zone_snapshot_uses_cached_device_capabilities() {
        let hegel_device = "Hegel H390 USB";
        let hegel_zone_id = local_device_zone_id(hegel_device);
        let manager = ZoneManager::new(Arc::new(Player::new()), Some(hegel_zone_id.clone()));

        manager.sync_local_devices(vec![hegel_device.to_string()]);
        manager.cache_local_device_capabilities(
            hegel_device,
            AudioDeviceCapabilities {
                max_sample_rate: 768_000,
                max_bit_depth: 32,
                max_dsd_rate: Some(256),
                supports_dsd128: true,
                supports_dsd256: true,
            },
        );

        let zone = manager
            .list_zones()
            .into_iter()
            .find(|zone| zone.id == hegel_zone_id)
            .unwrap();

        assert_eq!(zone.capabilities.max_sample_rate, 768_000);
        assert!(zone.capabilities.supports_dsd128);
        assert!(zone.capabilities.supports_dsd256);
    }

    #[test]
    fn standby_local_zone_is_selectable_without_usb_discovery() {
        let hegel_device = "Hegel H390 USB";
        let hegel_zone_id = local_device_zone_id(hegel_device);
        let manager = ZoneManager::new(Arc::new(Player::new()), None);

        manager.sync_standby_local_zone(
            &hegel_zone_id,
            "Hegel H390 USB",
            hegel_device,
            "Hegel standby",
        );

        let zone = manager
            .list_zones()
            .into_iter()
            .find(|zone| zone.id == hegel_zone_id)
            .unwrap();
        assert!(zone.enabled);
        assert_eq!(zone.status, ZoneStatus::Available);
        assert_eq!(zone.status_message.as_deref(), Some("Hegel standby"));

        manager.select_zone(&hegel_zone_id).unwrap();

        assert_eq!(manager.active_zone_id(), hegel_zone_id);
        assert!(manager.player_for_zone(&hegel_zone_id).is_some());
    }

    #[test]
    fn saved_local_zone_can_be_restored_offline_without_usb_discovery() {
        let hegel_device = "Hegel H390 USB";
        let hegel_zone_id = local_device_zone_id(hegel_device);
        let manager = ZoneManager::new(Arc::new(Player::new()), Some(hegel_zone_id.clone()));

        manager.sync_saved_local_zone(
            &hegel_zone_id,
            "Hegel H390 USB",
            hegel_device,
            true,
            "USB missing",
        );

        assert_eq!(manager.active_zone_id(), LOCAL_ZONE_ID);
        assert!(manager.player_for_zone(&hegel_zone_id).is_none());
        let zone = manager
            .list_zones()
            .into_iter()
            .find(|zone| zone.id == hegel_zone_id)
            .unwrap();
        assert!(zone.enabled);
        assert_eq!(zone.status, ZoneStatus::Offline);
        assert_eq!(zone.status_message.as_deref(), Some("USB missing"));
    }

    #[test]
    fn remote_agent_zones_keep_their_configured_output_device() {
        let manager = ZoneManager::new(Arc::new(Player::new()), None);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        manager.register_agent(
            "agent-1".to_string(),
            "Studio PC".to_string(),
            AgentCapabilities {
                output_devices: vec![
                    "Speakers (Brooklyn DAC+)".to_string(),
                    "ASIO: Brooklyn DAC+".to_string(),
                ],
                output_device_capabilities: vec![
                    crate::protocol::OutputDeviceCapabilities {
                        name: "Speakers (Brooklyn DAC+)".to_string(),
                        backend: Some("wasapi".to_string()),
                        max_sample_rate: 192_000,
                        max_bit_depth: 32,
                        supports_dsd128: false,
                        supports_dsd256: false,
                    },
                    crate::protocol::OutputDeviceCapabilities {
                        name: "ASIO: Brooklyn DAC+".to_string(),
                        backend: Some("asio".to_string()),
                        max_sample_rate: 384_000,
                        max_bit_depth: 32,
                        supports_dsd128: true,
                        supports_dsd256: true,
                    },
                ],
                max_sample_rate: 384_000,
                max_bit_depth: 32,
                exclusive_supported: true,
                supports_dsd128: true,
                supports_dsd256: true,
                browser: false,
            },
            tx,
        );
        manager.update_signal_path(
            "agent-1",
            SyncSignalPath {
                output_device: Some("ASIO: Brooklyn DAC+".to_string()),
                ..SyncSignalPath::default()
            },
        );

        let zones = manager.list_zones();
        let wasapi_zone = zones
            .iter()
            .find(|zone| zone.device_name.as_deref() == Some("Speakers (Brooklyn DAC+)"))
            .expect("WASAPI zone should keep its own output device");
        assert_eq!(wasapi_zone.agent_name.as_deref(), Some("Studio PC"));
        assert_eq!(wasapi_zone.backend.as_deref(), Some("wasapi"));
        assert_eq!(wasapi_zone.capabilities.max_sample_rate, 192_000);

        let asio_zone = zones
            .iter()
            .find(|zone| zone.device_name.as_deref() == Some("ASIO: Brooklyn DAC+"))
            .expect("ASIO zone should keep its own output device");
        assert_eq!(asio_zone.backend.as_deref(), Some("asio"));
        assert_eq!(asio_zone.capabilities.max_sample_rate, 384_000);
    }

    #[test]
    fn remote_agent_outputs_can_be_disabled() {
        let manager = ZoneManager::new(Arc::new(Player::new()), None);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        manager.register_agent(
            "agent-1".to_string(),
            "Henry's PC".to_string(),
            single_agent_capabilities("Speakers (Agent DAC)"),
            tx,
        );
        let zone_id = manager
            .list_zones()
            .into_iter()
            .find(|zone| zone.agent_name.as_deref() == Some("Henry's PC"))
            .expect("agent zone should be registered")
            .id;

        manager.select_zone(&zone_id).unwrap();
        manager.disable_zone(&zone_id).unwrap();

        let disabled = manager
            .list_zones()
            .into_iter()
            .find(|zone| zone.id == zone_id)
            .expect("disabled agent zone should remain listed");
        assert!(!disabled.enabled);
        assert_ne!(manager.active_zone_id(), zone_id);
        assert!(manager.select_zone(&zone_id).is_err());
        assert_eq!(
            manager
                .send_to_zone(&zone_id, CoreToAgentCommand::Stop)
                .unwrap_err(),
            "Zone is disabled"
        );
        assert!(matches!(rx.try_recv(), Ok(CoreToAgentCommand::Stop)));

        manager.enable_zone(&zone_id).unwrap();
        assert_eq!(manager.active_zone_id(), zone_id);
        assert!(
            manager
                .list_zones()
                .into_iter()
                .find(|zone| zone.id == zone_id)
                .is_some_and(|zone| zone.enabled && zone.status == ZoneStatus::Active)
        );
    }

    fn browser_agent_capabilities() -> AgentCapabilities {
        AgentCapabilities {
            output_devices: Vec::new(),
            output_device_capabilities: Vec::new(),
            max_sample_rate: 48_000,
            max_bit_depth: 24,
            exclusive_supported: false,
            supports_dsd128: false,
            supports_dsd256: false,
            browser: true,
        }
    }

    #[test]
    fn browser_agent_zone_is_private_and_never_shared_active() {
        let manager = ZoneManager::new(Arc::new(Player::new()), None);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        manager.register_agent(
            "browser-3f9c2ab1d0e4".to_string(),
            "Safari on iPhone".to_string(),
            browser_agent_capabilities(),
            tx,
        );

        let zone = manager
            .list_zones()
            .into_iter()
            .find(|zone| zone.id == "browser-3f9c2ab1d0e4")
            .expect("browser zone should be listed by the manager");
        assert!(zone.browser);
        assert!(!zone.enabled);

        // The shared active zone must never move to a browser zone.
        assert_eq!(manager.active_zone_id(), LOCAL_ZONE_ID);
        assert!(manager.select_zone("browser-3f9c2ab1d0e4").is_err());
        manager.enable_zone("browser-3f9c2ab1d0e4").unwrap();
        assert_eq!(manager.active_zone_id(), LOCAL_ZONE_ID);
        assert!(manager.select_zone("browser-3f9c2ab1d0e4").is_err());
        manager.disable_zone("browser-3f9c2ab1d0e4").unwrap();

        assert_eq!(
            manager
                .browser_zone_agent_id("browser-3f9c2ab1d0e4")
                .as_deref(),
            Some("browser-3f9c2ab1d0e4")
        );
        assert_eq!(manager.browser_zone_agent_id(LOCAL_ZONE_ID), None);
    }

    #[test]
    fn browser_agent_zone_is_skipped_by_active_zone_fallback() {
        let manager = ZoneManager::new(Arc::new(Player::new()), None);
        // Take the local core zone out of the fallback pool so the agent
        // branch of the fallback policy is what gets exercised.
        manager.apply_zone_definitions(vec![ZoneDefinition {
            id: LOCAL_ZONE_ID.to_string(),
            name: "Default".to_string(),
            kind: Some("local_coreaudio".to_string()),
            device_name: None,
            enabled: false,
        }]);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        manager.register_agent(
            "browser-3f9c2ab1d0e4".to_string(),
            "Safari on iPhone".to_string(),
            browser_agent_capabilities(),
            tx,
        );
        manager.enable_zone("browser-3f9c2ab1d0e4").unwrap();
        let (native_tx, _native_rx) = tokio::sync::mpsc::unbounded_channel();
        manager.register_agent(
            "agent-1".to_string(),
            "Studio PC".to_string(),
            single_agent_capabilities("Speakers (Agent DAC)"),
            native_tx,
        );
        let native_zone_id = manager
            .list_zones()
            .into_iter()
            .find(|zone| zone.agent_name.as_deref() == Some("Studio PC"))
            .expect("native agent zone should be registered")
            .id;
        manager.select_zone(&native_zone_id).unwrap();

        // Losing the active native zone must never fall back to the browser
        // zone, even when it is the only enabled agent left.
        manager.unregister_agent("agent-1");

        assert_eq!(manager.active_zone_id(), LOCAL_ZONE_ID);
    }

    #[test]
    fn stale_connection_cleanup_keeps_a_reconnected_agent_registered() {
        let manager = ZoneManager::new(Arc::new(Player::new()), None);
        let agent_id = "browser-3f9c2ab1d0e4";

        // First connection registers, then the agent reconnects (new socket)
        // before the first socket's server-side cleanup has run.
        let (old_tx, _old_rx) = tokio::sync::mpsc::unbounded_channel();
        let old_connection = manager.register_agent(
            agent_id.to_string(),
            "Safari on iPhone".to_string(),
            browser_agent_capabilities(),
            old_tx,
        );
        let (new_tx, _new_rx) = tokio::sync::mpsc::unbounded_channel();
        let new_connection = manager.register_agent(
            agent_id.to_string(),
            "Safari on iPhone".to_string(),
            browser_agent_capabilities(),
            new_tx,
        );
        assert_ne!(old_connection, new_connection);

        // The stale socket's cleanup must not tear down the live registration.
        manager.unregister_agent_connection(agent_id, old_connection);
        assert_eq!(
            manager.zone_protocol(agent_id),
            Some(SinkProtocol::RemoteAgent)
        );

        // The live connection's own cleanup still removes the zone.
        manager.unregister_agent_connection(agent_id, new_connection);
        assert_eq!(manager.zone_protocol(agent_id), None);
    }

    #[test]
    fn preferred_remote_agent_zone_restores_when_agent_registers() {
        let device = "Speakers (Agent DAC)";
        let zone_id = format!("agent-1-{}", local_device_zone_id(device));
        let manager = ZoneManager::new(Arc::new(Player::new()), Some(zone_id.clone()));

        assert_eq!(manager.active_zone_id(), LOCAL_ZONE_ID);
        assert_eq!(manager.preferred_active_zone_id(), zone_id);

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        manager.register_agent(
            "agent-1".to_string(),
            "Henry's PC".to_string(),
            single_agent_capabilities(device),
            tx,
        );

        assert_eq!(manager.active_zone_id(), zone_id);
    }

    #[test]
    fn preferred_remote_agent_zone_survives_disconnect_and_reconnect() {
        let device = "Speakers (Agent DAC)";
        let zone_id = format!("agent-1-{}", local_device_zone_id(device));
        let manager = ZoneManager::new(Arc::new(Player::new()), Some(zone_id.clone()));
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        manager.register_agent(
            "agent-1".to_string(),
            "Henry's PC".to_string(),
            single_agent_capabilities(device),
            tx,
        );
        assert_eq!(manager.active_zone_id(), zone_id);

        manager.unregister_agent("agent-1");

        assert_eq!(manager.active_zone_id(), LOCAL_ZONE_ID);
        assert_eq!(manager.preferred_active_zone_id(), zone_id);

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        manager.register_agent(
            "agent-1".to_string(),
            "Henry's PC".to_string(),
            single_agent_capabilities(device),
            tx,
        );

        assert_eq!(manager.active_zone_id(), zone_id);
    }

    #[test]
    fn playback_config_carries_dsd_mode_rules_and_eq() {
        let player = Player::new();
        player.set_output_mode(OutputMode::Dsd256);
        let dsd_rules = vec![DsdSourceRule {
            source_rate: 176_400,
            filter_type: "Minimum16k".to_string(),
            output_mode: "Dsd128".to_string(),
        }];
        let eq = crate::audio::eq::EqConfig {
            enabled: true,
            preamp_db: -5.6,
            ..crate::audio::eq::EqConfig::default()
        };
        player.update_eq(eq.clone());

        let config = current_playback_config(&player, dsd_rules.clone());

        assert_eq!(config.output_mode, "Dsd256");
        assert_eq!(config.dsd_rules, dsd_rules);
        assert!(config.eq.enabled);
        assert_eq!(config.eq.preamp_db, eq.preamp_db);
    }
}
