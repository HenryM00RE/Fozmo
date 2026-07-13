use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
pub struct PlaybackConfigApplicator {
    applied_zone_id: Arc<Mutex<Option<String>>>,
}

impl PlaybackConfigApplicator {
    pub(crate) fn remember_applied(&self, zone_id: impl Into<String>) {
        *self.applied_zone_id.lock().unwrap() = Some(zone_id.into());
    }

    pub(crate) fn mark_if_changed(&self, zone_id: &str) -> bool {
        let mut applied = self.applied_zone_id.lock().unwrap();
        if applied.as_deref() == Some(zone_id) {
            return false;
        }
        *applied = Some(zone_id.to_string());
        true
    }

    #[cfg(test)]
    pub(crate) fn applied_zone_id(&self) -> Option<String> {
        self.applied_zone_id.lock().unwrap().clone()
    }
}
