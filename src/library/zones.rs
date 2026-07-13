use super::{Library, ZoneDefinition, ZoneSettings, normalize_volume, now_secs};
use rusqlite::{OptionalExtension, params};

impl Library {
    pub fn upsert_zone_definition(
        &self,
        id: &str,
        name: &str,
        kind: &str,
        device_name: Option<&str>,
        enabled: bool,
    ) -> Result<(), String> {
        let now = now_secs();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            r#"
            INSERT INTO playback_zones (id, name, kind, device_name, enabled, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
            ON CONFLICT(id) DO UPDATE SET
                name = CASE
                    WHEN playback_zones.name = 'Mac Mini Core' THEN excluded.name
                    ELSE playback_zones.name
                END,
                kind = excluded.kind,
                device_name = excluded.device_name,
                updated_at = excluded.updated_at
            "#,
            params![id, name, kind, device_name, if enabled { 1 } else { 0 }, now],
        )
        .map_err(|e| format!("upsert zone: {e}"))?;
        Ok(())
    }

    pub fn zone_definitions(&self) -> Result<Vec<ZoneDefinition>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT id, name, kind, device_name, enabled FROM playback_zones")
            .map_err(|e| format!("zone definitions query: {e}"))?;
        let rows = stmt
            .query_map([], |row| {
                Ok(ZoneDefinition {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    kind: row.get(2)?,
                    device_name: row.get(3)?,
                    enabled: row.get::<_, i64>(4)? != 0,
                })
            })
            .map_err(|e| format!("zone definitions map: {e}"))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| format!("zone definitions row: {e}"))?);
        }
        Ok(out)
    }

    pub fn zone_settings(&self, zone_id: &str) -> Result<ZoneSettings, String> {
        let conn = self.conn.lock().unwrap();
        let body: Option<String> = conn
            .query_row(
                "SELECT settings_json FROM zone_settings WHERE zone_id = ?1",
                [zone_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| format!("zone settings query: {e}"))?;
        let Some(body) = body else {
            return Ok(ZoneSettings::default());
        };
        serde_json::from_str(&body).map_err(|e| format!("parse zone settings: {e}"))
    }

    pub fn set_zone_airplay_default_volume(
        &self,
        zone_id: &str,
        volume: Option<f32>,
    ) -> Result<ZoneSettings, String> {
        let mut settings = self.zone_settings(zone_id)?;
        settings.airplay_default_volume = normalize_volume(volume);
        self.save_zone_settings(zone_id, &settings)?;
        Ok(settings)
    }

    pub fn remember_zone_airplay_volume(
        &self,
        zone_id: &str,
        volume: f32,
    ) -> Result<ZoneSettings, String> {
        let mut settings = self.zone_settings(zone_id)?;
        settings.airplay_last_volume = normalize_volume(Some(volume));
        self.save_zone_settings(zone_id, &settings)?;
        Ok(settings)
    }

    pub fn set_zone_settings(
        &self,
        zone_id: &str,
        settings: ZoneSettings,
    ) -> Result<ZoneSettings, String> {
        self.save_zone_settings(zone_id, &settings)?;
        Ok(settings)
    }

    fn save_zone_settings(&self, zone_id: &str, settings: &ZoneSettings) -> Result<(), String> {
        let body =
            serde_json::to_string(settings).map_err(|e| format!("serialize zone settings: {e}"))?;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            r#"
            INSERT INTO zone_settings (zone_id, settings_json, updated_at)
            VALUES (?1, ?2, ?3)
            ON CONFLICT(zone_id) DO UPDATE SET
                settings_json = excluded.settings_json,
                updated_at = excluded.updated_at
            "#,
            params![zone_id, body, now_secs()],
        )
        .map_err(|e| format!("save zone settings: {e}"))?;
        Ok(())
    }

    pub fn set_zone_enabled(&self, zone_id: &str, enabled: bool) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE playback_zones SET enabled = ?2, updated_at = ?3 WHERE id = ?1",
            params![zone_id, if enabled { 1 } else { 0 }, now_secs()],
        )
        .map_err(|e| format!("set zone enabled: {e}"))?;
        Ok(())
    }

    pub fn rename_zone(&self, zone_id: &str, name: &str) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE playback_zones SET name = ?2, updated_at = ?3 WHERE id = ?1",
            params![zone_id, name, now_secs()],
        )
        .map_err(|e| format!("rename zone: {e}"))?;
        Ok(())
    }
}
