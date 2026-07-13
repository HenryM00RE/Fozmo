use super::{Library, NowPlayingQueueSnapshot, ZoneQueueEntry, now_secs};
use crate::protocol::SourceRef;
use rusqlite::{OptionalExtension, params};
use serde_json::Value;

impl Library {
    pub fn set_zone_queue(&self, zone_id: &str, queue: &[SourceRef]) -> Result<(), String> {
        let now = now_secs();
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction()
            .map_err(|e| format!("zone queue tx: {e}"))?;
        tx.execute("DELETE FROM zone_queue_items WHERE zone_id = ?1", [zone_id])
            .map_err(|e| format!("clear zone queue: {e}"))?;
        for (idx, source) in queue.iter().enumerate() {
            let source_json = serde_json::to_string(source)
                .map_err(|e| format!("serialize queue source: {e}"))?;
            tx.execute(
                "INSERT INTO zone_queue_items (zone_id, position, source_json, created_at) VALUES (?1, ?2, ?3, ?4)",
                params![zone_id, idx as i64, source_json, now],
            )
            .map_err(|e| format!("insert zone queue: {e}"))?;
        }
        tx.commit().map_err(|e| format!("commit zone queue: {e}"))?;
        Ok(())
    }

    pub fn zone_queue(&self, zone_id: &str) -> Result<Vec<ZoneQueueEntry>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT source_json FROM zone_queue_items WHERE zone_id = ?1 ORDER BY position",
            )
            .map_err(|e| format!("zone queue query: {e}"))?;
        let rows = stmt
            .query_map([zone_id], |row| row.get::<_, String>(0))
            .map_err(|e| format!("zone queue map: {e}"))?;
        let mut out = Vec::new();
        for row in rows {
            let body = row.map_err(|e| format!("zone queue row: {e}"))?;
            let source = serde_json::from_str::<SourceRef>(&body)
                .map_err(|e| format!("parse zone queue source: {e}"))?;
            out.push(ZoneQueueEntry { source });
        }
        Ok(out)
    }

    pub fn set_now_playing_queue(&self, zone_id: &str, state: &Value) -> Result<(), String> {
        let body = serde_json::to_string(state)
            .map_err(|e| format!("serialize now playing queue: {e}"))?;
        let now = now_secs();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            r#"
            INSERT INTO now_playing_queues (zone_id, state_json, updated_at)
            VALUES (?1, ?2, ?3)
            ON CONFLICT(zone_id) DO UPDATE SET
                state_json = excluded.state_json,
                updated_at = excluded.updated_at
            "#,
            params![zone_id, body, now],
        )
        .map_err(|e| format!("set now playing queue: {e}"))?;
        Ok(())
    }

    pub fn now_playing_queue(
        &self,
        zone_id: &str,
    ) -> Result<Option<NowPlayingQueueSnapshot>, String> {
        let conn = self.conn.lock().unwrap();
        let row = conn
            .query_row(
                "SELECT state_json, updated_at FROM now_playing_queues WHERE zone_id = ?1",
                [zone_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()
            .map_err(|e| format!("now playing queue query: {e}"))?;
        let Some((body, updated_at)) = row else {
            return Ok(None);
        };
        let state = serde_json::from_str::<Value>(&body)
            .map_err(|e| format!("parse now playing queue: {e}"))?;
        Ok(Some(NowPlayingQueueSnapshot { state, updated_at }))
    }
}
