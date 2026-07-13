use super::{
    AutoMetaAuditIssue, AutoMetaJobItem, AutoMetaLocalVersion, AutoMetaProgress, Library,
    collect_rows, now_secs,
};
use rusqlite::{OptionalExtension, TransactionBehavior, params};

impl Library {
    pub fn autometa_progress(&self) -> AutoMetaProgress {
        if let Ok(Some(progress)) = self.latest_autometa_job_progress() {
            return progress;
        }
        self.autometa_progress.lock().unwrap().clone()
    }

    // Reserved for the manual autometa job runner while background orchestration is paused.
    #[allow(dead_code)]
    pub fn begin_autometa_progress(&self, total: usize) -> bool {
        let mut progress = self.autometa_progress.lock().unwrap();
        if progress.running {
            return false;
        }
        *progress = AutoMetaProgress {
            running: true,
            total,
            ..AutoMetaProgress::default()
        };
        true
    }

    pub fn set_autometa_current(&self, album: &str, version: &str) {
        let mut progress = self.autometa_progress.lock().unwrap();
        progress.current_album = Some(album.to_string());
        progress.current_version = Some(version.to_string());
    }

    // Reserved for the manual autometa job runner while background orchestration is paused.
    #[allow(dead_code)]
    pub fn update_autometa_progress<F>(&self, last_result: String, update: F)
    where
        F: FnOnce(&mut AutoMetaProgress),
    {
        let mut progress = self.autometa_progress.lock().unwrap();
        progress.processed = (progress.processed + 1).min(progress.total);
        progress.last_result = Some(last_result);
        update(&mut progress);
    }

    // Reserved for the manual autometa job runner while background orchestration is paused.
    #[allow(dead_code)]
    pub fn finish_autometa_progress(&self) {
        let mut progress = self.autometa_progress.lock().unwrap();
        progress.running = false;
        progress.current_album = None;
        progress.current_version = None;
        progress.last_result = Some(if progress.errors > 0 {
            format!(
                "Completed with {} error{}",
                progress.errors,
                if progress.errors == 1 { "" } else { "s" }
            )
        } else {
            "Completed".to_string()
        });
    }

    pub fn fail_autometa_progress(&self, error: String) {
        let mut progress = self.autometa_progress.lock().unwrap();
        progress.running = false;
        progress.error = Some(error.clone());
        progress.last_result = Some(error);
        progress.current_album = None;
        progress.current_version = None;
    }

    pub fn autometa_local_versions(&self) -> Result<Vec<AutoMetaLocalVersion>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                r#"
                SELECT a.id, v.id, a.title,
                       COALESCE(v.source_label, v.title, 'Library'),
                       v.musicbrainz_match_status,
                       v.musicbrainz_release_id,
                       v.qobuz_match_status,
                       a.qobuz_match_status,
                       a.qobuz_album_id,
                       COALESCE(a.primary_version_id = v.id, 0)
                FROM album_versions v
                JOIN albums a ON a.id = v.album_id
                WHERE v.provider = 'local'
                ORDER BY lower(a.album_artist), lower(a.title),
                         COALESCE(v.sample_rate, 0) DESC,
                         COALESCE(v.bit_depth, 0) DESC,
                         v.id
                "#,
            )
            .map_err(|e| format!("autometa local versions: {e}"))?;
        collect_rows(
            stmt.query_map([], |row| {
                Ok(AutoMetaLocalVersion {
                    album_id: row.get(0)?,
                    version_id: row.get(1)?,
                    album_title: row.get(2)?,
                    version_label: row.get(3)?,
                    musicbrainz_match_status: row.get(4)?,
                    musicbrainz_release_id: row.get(5)?,
                    qobuz_match_status: row.get(6)?,
                    album_qobuz_match_status: row.get(7)?,
                    album_qobuz_album_id: row.get(8)?,
                    is_primary_version: row.get(9)?,
                })
            })
            .map_err(|e| format!("autometa local versions map: {e}"))?,
        )
    }

    pub fn set_version_musicbrainz_status(
        &self,
        version_id: i64,
        status: &str,
        release_id: Option<&str>,
        message: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        let changed = conn
            .execute(
                r#"
            UPDATE album_versions
            SET musicbrainz_match_status = ?2,
                musicbrainz_release_id = ?3,
                musicbrainz_tagged_at = CASE WHEN ?2 = 'matched' THEN ?4 ELSE musicbrainz_tagged_at END,
                autometa_message = ?5,
                updated_at = ?4
            WHERE id = ?1 AND provider = 'local'
            "#,
                params![version_id, status, release_id, now_secs(), message],
            )
            .map_err(|e| format!("set autometa musicbrainz status: {e}"))?;
        if changed != 1 {
            return Err(format!(
                "set autometa musicbrainz status changed {changed} rows for local version {version_id}"
            ));
        }
        Ok(())
    }

    pub fn set_version_qobuz_status(
        &self,
        version_id: i64,
        status: &str,
        message: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        let changed = conn
            .execute(
                r#"
            UPDATE album_versions
            SET qobuz_match_status = ?2,
                qobuz_tagged_at = CASE WHEN ?2 = 'matched' THEN ?3 ELSE qobuz_tagged_at END,
                autometa_message = ?4,
                updated_at = ?3
            WHERE id = ?1 AND provider = 'local'
            "#,
                params![version_id, status, now_secs(), message],
            )
            .map_err(|e| format!("set autometa qobuz status: {e}"))?;
        if changed != 1 {
            return Err(format!(
                "set autometa qobuz status changed {changed} rows for local version {version_id}"
            ));
        }
        Ok(())
    }

    pub fn recover_interrupted_autometa_jobs(&self) -> Result<(), String> {
        let now = now_secs();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            r#"
            UPDATE autometa_job_items
            SET status = 'interrupted',
                phase = CASE WHEN phase = 'queued' THEN 'interrupted' ELSE phase END,
                message = COALESCE(message, 'Interrupted by server restart'),
                updated_at = ?1
            WHERE status = 'processing'
            "#,
            [now],
        )
        .map_err(|e| format!("recover autometa items: {e}"))?;
        conn.execute(
            r#"
            UPDATE autometa_jobs
            SET status = 'interrupted',
                last_result = 'Interrupted by server restart',
                updated_at = ?1
            WHERE status IN ('running', 'stopping', 'paused')
            "#,
            [now],
        )
        .map_err(|e| format!("recover autometa jobs: {e}"))?;
        Ok(())
    }

    pub fn create_autometa_job(
        &self,
        mode: &str,
        link_qobuz: bool,
    ) -> Result<AutoMetaProgress, String> {
        let mode = normalized_autometa_mode(mode);
        let versions = self.autometa_job_versions(&mode, link_qobuz)?;
        let now = now_secs();
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("begin AutoMetadata job transaction: {e}"))?;
        let active_job: Option<i64> = tx
            .query_row(
                "SELECT id FROM autometa_jobs WHERE status IN ('running', 'paused', 'stopping', 'interrupted') ORDER BY id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| format!("check active AutoMetadata job: {e}"))?;
        if active_job.is_some() {
            return Err("AutoMetadata already has an active or recoverable job".to_string());
        }
        tx.execute(
            r#"
            INSERT INTO autometa_jobs (
                status, mode, link_qobuz, total, last_result,
                started_at, created_at, updated_at, finished_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, ?6, ?7)
            "#,
            params![
                if versions.is_empty() {
                    "completed"
                } else {
                    "running"
                },
                mode,
                if link_qobuz { 1 } else { 0 },
                versions.len() as i64,
                if versions.is_empty() {
                    "No albums queued"
                } else {
                    "Starting AutoMetadata"
                },
                now,
                if versions.is_empty() { Some(now) } else { None },
            ],
        )
        .map_err(|e| format!("create autometa job: {e}"))?;
        let job_id = tx.last_insert_rowid();
        for version in versions {
            tx.execute(
                r#"
                INSERT INTO autometa_job_items (
                    job_id, album_id, version_id, phase, status,
                    created_at, updated_at
                )
                VALUES (?1, ?2, ?3, 'queued', 'pending', ?4, ?4)
                "#,
                params![job_id, version.album_id, version.version_id, now],
            )
            .map_err(|e| format!("create autometa job item: {e}"))?;
        }
        tx.commit()
            .map_err(|e| format!("commit AutoMetadata job: {e}"))?;
        drop(conn);
        self.autometa_job_progress(job_id)
    }

    pub fn set_autometa_job_status(
        &self,
        job_id: i64,
        status: &str,
    ) -> Result<AutoMetaProgress, String> {
        let now = now_secs();
        let finished = matches!(status, "completed" | "failed" | "stopped");
        let conn = self.conn.lock().unwrap();
        let changed = conn
            .execute(
                r#"
                UPDATE autometa_jobs
                SET status = ?2,
                    last_result = CASE
                        WHEN ?2 = 'paused' THEN 'Paused after current item'
                        WHEN ?2 = 'stopping' THEN 'Stopping after current item'
                        WHEN ?2 = 'running' THEN 'AutoMetadata running'
                        ELSE last_result
                    END,
                    updated_at = ?3,
                    finished_at = CASE WHEN ?4 THEN COALESCE(finished_at, ?3) ELSE finished_at END
                WHERE id = ?1
                "#,
                params![job_id, status, now, finished],
            )
            .map_err(|e| format!("set autometa job status: {e}"))?;
        if changed != 1 {
            return Err(format!("AutoMetadata job {job_id} not found"));
        }
        drop(conn);
        self.autometa_job_progress(job_id)
    }

    pub fn autometa_job_status(&self, job_id: i64) -> Result<Option<String>, String> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT status FROM autometa_jobs WHERE id = ?1",
            [job_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| format!("autometa job status: {e}"))
    }

    pub fn resume_autometa_job(&self, job_id: i64) -> Result<(AutoMetaProgress, bool), String> {
        let now = now_secs();
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("begin AutoMetadata resume: {e}"))?;
        let status: Option<String> = tx
            .query_row(
                "SELECT status FROM autometa_jobs WHERE id = ?1",
                [job_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| format!("read AutoMetadata resume state: {e}"))?;
        let should_spawn = match status.as_deref() {
            Some("paused") => false,
            Some("interrupted") => true,
            Some("running") => {
                tx.commit()
                    .map_err(|e| format!("commit AutoMetadata resume check: {e}"))?;
                drop(conn);
                return self
                    .autometa_job_progress(job_id)
                    .map(|progress| (progress, false));
            }
            Some(status) => {
                return Err(format!("Cannot resume AutoMetadata job in {status} state"));
            }
            None => return Err(format!("AutoMetadata job {job_id} not found")),
        };
        tx.execute(
            r#"
            UPDATE autometa_jobs
            SET status = 'running',
                error = NULL,
                last_result = 'AutoMetadata running',
                finished_at = NULL,
                updated_at = ?2
            WHERE id = ?1 AND status IN ('paused', 'interrupted')
            "#,
            params![job_id, now],
        )
        .map_err(|e| format!("resume AutoMetadata job: {e}"))?;
        tx.commit()
            .map_err(|e| format!("commit AutoMetadata resume: {e}"))?;
        drop(conn);
        self.autometa_job_progress(job_id)
            .map(|progress| (progress, should_spawn))
    }

    pub fn latest_autometa_job_id(&self) -> Result<Option<i64>, String> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT id FROM autometa_jobs ORDER BY id DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| format!("latest autometa job id: {e}"))
    }

    pub fn latest_autometa_job_progress(&self) -> Result<Option<AutoMetaProgress>, String> {
        let Some(job_id) = self.latest_autometa_job_id()? else {
            return Ok(None);
        };
        self.autometa_job_progress(job_id).map(Some)
    }

    pub fn autometa_job_progress(&self, job_id: i64) -> Result<AutoMetaProgress, String> {
        let conn = self.conn.lock().unwrap();
        let Some(job) = conn
            .query_row(
                r#"
                SELECT id, status, mode, link_qobuz, total, last_result, error,
                       started_at, updated_at, finished_at
                FROM autometa_jobs
                WHERE id = ?1
                "#,
                [job_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)? != 0,
                        row.get::<_, i64>(4)? as usize,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, Option<i64>>(7)?,
                        row.get::<_, Option<i64>>(8)?,
                        row.get::<_, Option<i64>>(9)?,
                    ))
                },
            )
            .optional()
            .map_err(|e| format!("autometa job progress: {e}"))?
        else {
            return Ok(AutoMetaProgress::default());
        };
        let (
            job_id,
            status,
            mode,
            link_qobuz,
            total,
            last_result,
            error,
            started_at,
            updated_at,
            finished_at,
        ) = job;
        let count = |where_sql: &str| -> Result<usize, String> {
            conn.query_row(
                &format!(
                    "SELECT COUNT(*) FROM autometa_job_items WHERE job_id = ?1 AND {where_sql}"
                ),
                [job_id],
                |row| row.get::<_, i64>(0),
            )
            .map(|v| v as usize)
            .map_err(|e| format!("autometa job count: {e}"))
        };
        let processed =
            count("status IN ('matched', 'needs_review', 'error', 'skipped', 'stopped')")?;
        let musicbrainz_matched = count("musicbrainz_release_id IS NOT NULL")?;
        let qobuz_matched = count("qobuz_album_id IS NOT NULL AND status = 'matched'")?;
        let no_proper_match = count("status = 'needs_review'")?;
        let skipped = count("status = 'skipped'")?;
        let errors = count("status = 'error'")?;
        let recent_results = self.autometa_job_items_with_conn(&conn, job_id, None, 6)?;
        let current = conn
            .query_row(
                r#"
                SELECT a.title, COALESCE(v.source_label, v.title, 'Library'), i.phase
                FROM autometa_job_items i
                JOIN albums a ON a.id = i.album_id
                JOIN album_versions v ON v.id = i.version_id
                WHERE i.job_id = ?1 AND i.status = 'processing'
                ORDER BY i.updated_at DESC, i.id DESC
                LIMIT 1
                "#,
                [job_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(|e| format!("autometa current item: {e}"))?;
        let (current_album, current_version, phase) = current
            .map(|(album, version, phase)| (Some(album), Some(version), Some(phase)))
            .unwrap_or((None, None, None));
        let now = now_secs();
        let elapsed_secs =
            started_at.map(|started| finished_at.unwrap_or(now).saturating_sub(started));
        // Estimate throughput from time actually spent processing completed
        // items. Using the job's wall-clock age makes paused or recovered jobs
        // look hundreds of hours slower than they really are.
        let average_item_secs = conn
            .query_row(
                r#"
                SELECT AVG(duration_secs)
                FROM (
                    SELECT MAX(1, finished_at - started_at) AS duration_secs
                    FROM autometa_job_items
                    WHERE job_id = ?1
                      AND attempts = 1
                      AND started_at IS NOT NULL
                      AND finished_at IS NOT NULL
                      AND finished_at >= started_at
                      AND status IN ('matched', 'needs_review', 'error', 'skipped', 'stopped')
                    ORDER BY finished_at DESC
                    LIMIT 20
                )
                "#,
                [job_id],
                |row| row.get::<_, Option<f64>>(0),
            )
            .map_err(|e| format!("autometa average item duration: {e}"))?;
        let rate_per_min = average_item_secs
            .filter(|seconds| *seconds > 0.0)
            .map(|seconds| 60.0 / seconds);
        let eta_secs = match (rate_per_min, total.saturating_sub(processed)) {
            (Some(rate), remaining) if rate > 0.0 && remaining > 0 => {
                Some(((remaining as f64 / rate) * 60.0).round() as i64)
            }
            _ => None,
        };
        Ok(AutoMetaProgress {
            job_id: Some(job_id),
            status: status.clone(),
            running: status == "running" || status == "stopping",
            processed,
            total,
            exact_matched: qobuz_matched.max(musicbrainz_matched),
            musicbrainz_matched,
            qobuz_matched,
            no_proper_match,
            skipped,
            errors,
            current_album,
            current_version,
            phase,
            mode: Some(mode),
            link_qobuz,
            last_result,
            error,
            started_at,
            updated_at,
            finished_at,
            elapsed_secs,
            eta_secs,
            rate_per_min,
            remaining: total.saturating_sub(processed),
            pause_requested: status == "paused",
            stop_requested: status == "stopping",
            recent_results,
        })
    }

    pub fn autometa_job_items(
        &self,
        job_id: i64,
        status: Option<&str>,
    ) -> Result<Vec<AutoMetaJobItem>, String> {
        let conn = self.conn.lock().unwrap();
        self.autometa_job_items_with_conn(&conn, job_id, status, 200)
    }

    pub fn claim_autometa_work_item(
        &self,
        job_id: i64,
    ) -> Result<Option<AutoMetaJobWorkItem>, String> {
        let now = now_secs();
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("begin AutoMetadata item claim: {e}"))?;
        let work = tx
            .query_row(
                r#"
            SELECT i.id, a.id, v.id, a.title,
                   COALESCE(v.source_label, v.title, 'Library'),
                   COALESCE(a.primary_version_id = v.id, 0),
                   v.musicbrainz_match_status,
                   v.musicbrainz_release_id,
                   v.qobuz_match_status,
                   a.qobuz_match_status,
                   a.qobuz_album_id
            FROM autometa_job_items i
            JOIN album_versions v ON v.id = i.version_id
            JOIN albums a ON a.id = i.album_id
            WHERE i.job_id = ?1
              AND i.status IN ('pending', 'interrupted')
            ORDER BY i.id
            LIMIT 1
            "#,
                [job_id],
                |row| {
                    Ok(AutoMetaJobWorkItem {
                        item_id: row.get(0)?,
                        version: AutoMetaLocalVersion {
                            album_id: row.get(1)?,
                            version_id: row.get(2)?,
                            album_title: row.get(3)?,
                            version_label: row.get(4)?,
                            is_primary_version: row.get(5)?,
                            musicbrainz_match_status: row.get(6)?,
                            musicbrainz_release_id: row.get(7)?,
                            qobuz_match_status: row.get(8)?,
                            album_qobuz_match_status: row.get(9)?,
                            album_qobuz_album_id: row.get(10)?,
                        },
                    })
                },
            )
            .optional()
            .map_err(|e| format!("select AutoMetadata item to claim: {e}"))?;
        let Some(work) = work else {
            tx.commit()
                .map_err(|e| format!("commit empty AutoMetadata item claim: {e}"))?;
            return Ok(None);
        };
        let changed = tx
            .execute(
                r#"
            UPDATE autometa_job_items
            SET status = 'processing',
                phase = 'musicbrainz',
                attempts = attempts + 1,
                started_at = COALESCE(started_at, ?3),
                updated_at = ?3
            WHERE id = ?2 AND job_id = ?1
              AND status IN ('pending', 'interrupted')
            "#,
                params![job_id, work.item_id, now],
            )
            .map_err(|e| format!("claim AutoMetadata item: {e}"))?;
        if changed != 1 {
            return Err(format!(
                "AutoMetadata item {} was already claimed",
                work.item_id
            ));
        }
        tx.execute(
            r#"
            UPDATE autometa_jobs
            SET current_album_id = (SELECT album_id FROM autometa_job_items WHERE id = ?2),
                current_version_id = (SELECT version_id FROM autometa_job_items WHERE id = ?2),
                updated_at = ?3
            WHERE id = ?1
            "#,
            params![job_id, work.item_id, now],
        )
        .map_err(|e| format!("set autometa current item: {e}"))?;
        tx.commit()
            .map_err(|e| format!("commit AutoMetadata item claim: {e}"))?;
        Ok(Some(work))
    }

    pub fn fail_autometa_job(&self, job_id: i64, error: &str) -> Result<(), String> {
        let now = now_secs();
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("begin AutoMetadata failure transaction: {e}"))?;
        tx.execute(
            r#"
            UPDATE autometa_job_items
            SET status = 'error',
                message = ?2,
                finished_at = ?3,
                updated_at = ?3
            WHERE job_id = ?1 AND status = 'processing'
            "#,
            params![job_id, error, now],
        )
        .map_err(|e| format!("fail active AutoMetadata item: {e}"))?;
        let changed = tx
            .execute(
                r#"
                UPDATE autometa_jobs
                SET status = 'failed',
                    error = ?2,
                    last_result = ?2,
                    current_album_id = NULL,
                    current_version_id = NULL,
                    updated_at = ?3,
                    finished_at = COALESCE(finished_at, ?3)
                WHERE id = ?1
                "#,
                params![job_id, error, now],
            )
            .map_err(|e| format!("fail AutoMetadata job: {e}"))?;
        if changed != 1 {
            return Err(format!("AutoMetadata job {job_id} not found"));
        }
        tx.commit()
            .map_err(|e| format!("commit AutoMetadata failure: {e}"))
    }

    pub fn update_autometa_item_phase(
        &self,
        job_id: i64,
        item_id: i64,
        phase: &str,
    ) -> Result<(), String> {
        let now = now_secs();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE autometa_job_items SET phase = ?3, updated_at = ?4 WHERE job_id = ?1 AND id = ?2",
            params![job_id, item_id, phase, now],
        )
        .map_err(|e| format!("update autometa item phase: {e}"))?;
        conn.execute(
            "UPDATE autometa_jobs SET updated_at = ?2 WHERE id = ?1",
            params![job_id, now],
        )
        .map_err(|e| format!("touch autometa job: {e}"))?;
        Ok(())
    }

    // Autometa completion mirrors the persisted job item columns and optional external IDs.
    #[allow(clippy::too_many_arguments)]
    pub fn finish_autometa_item(
        &self,
        job_id: i64,
        item_id: i64,
        status: &str,
        phase: &str,
        message: &str,
        musicbrainz_release_id: Option<&str>,
        qobuz_album_id: Option<&str>,
    ) -> Result<(), String> {
        let now = now_secs();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            r#"
            UPDATE autometa_job_items
            SET status = ?3,
                phase = ?4,
                message = ?5,
                musicbrainz_release_id = COALESCE(?6, musicbrainz_release_id),
                qobuz_album_id = COALESCE(?7, qobuz_album_id),
                finished_at = ?8,
                updated_at = ?8
            WHERE job_id = ?1 AND id = ?2
            "#,
            params![
                job_id,
                item_id,
                status,
                phase,
                message,
                musicbrainz_release_id,
                qobuz_album_id,
                now
            ],
        )
        .map_err(|e| format!("finish autometa item: {e}"))?;
        conn.execute(
            r#"
            UPDATE autometa_jobs
            SET last_result = ?2,
                current_album_id = NULL,
                current_version_id = NULL,
                updated_at = ?3
            WHERE id = ?1
            "#,
            params![job_id, message, now],
        )
        .map_err(|e| format!("finish autometa item job: {e}"))?;
        Ok(())
    }

    pub fn complete_autometa_job_if_done(&self, job_id: i64) -> Result<bool, String> {
        let now = now_secs();
        let conn = self.conn.lock().unwrap();
        let pending: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM autometa_job_items WHERE job_id = ?1 AND status IN ('pending', 'processing', 'interrupted')",
                [job_id],
                |row| row.get(0),
            )
            .map_err(|e| format!("count autometa pending: {e}"))?;
        if pending > 0 {
            return Ok(false);
        }
        let errors: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM autometa_job_items WHERE job_id = ?1 AND status = 'error'",
                [job_id],
                |row| row.get(0),
            )
            .unwrap_or(0);
        let message = if errors > 0 {
            format!(
                "Completed with {errors} error{}",
                if errors == 1 { "" } else { "s" }
            )
        } else {
            "Completed".to_string()
        };
        conn.execute(
            r#"
            UPDATE autometa_jobs
            SET status = 'completed',
                last_result = ?2,
                current_album_id = NULL,
                current_version_id = NULL,
                updated_at = ?3,
                finished_at = COALESCE(finished_at, ?3)
            WHERE id = ?1
            "#,
            params![job_id, message, now],
        )
        .map_err(|e| format!("complete autometa job: {e}"))?;
        Ok(true)
    }

    pub fn stop_autometa_job_after_current_item(&self, job_id: i64) -> Result<(), String> {
        let now = now_secs();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            r#"
            UPDATE autometa_job_items
            SET status = 'stopped',
                phase = 'stopped',
                message = COALESCE(message, 'Stopped before processing'),
                finished_at = ?2,
                updated_at = ?2
            WHERE job_id = ?1 AND status IN ('pending', 'interrupted')
            "#,
            params![job_id, now],
        )
        .map_err(|e| format!("stop autometa items: {e}"))?;
        conn.execute(
            r#"
            UPDATE autometa_jobs
            SET status = 'stopped',
                last_result = 'Stopped after current item',
                current_album_id = NULL,
                current_version_id = NULL,
                updated_at = ?2,
                finished_at = COALESCE(finished_at, ?2)
            WHERE id = ?1
            "#,
            params![job_id, now],
        )
        .map_err(|e| format!("stop autometa job: {e}"))?;
        Ok(())
    }

    pub fn autometa_audit_issues(&self) -> Result<Vec<AutoMetaAuditIssue>, String> {
        let conn = self.conn.lock().unwrap();
        let mut issues = Vec::new();
        let mut stmt = conn
            .prepare(
                r#"
                SELECT a.id, v.id, a.title, COALESCE(v.source_label, v.title, 'Library'),
                       v.musicbrainz_release_id
                FROM album_versions v
                JOIN albums a ON a.id = v.album_id
                WHERE v.provider = 'local'
                  AND v.musicbrainz_match_status = 'matched'
                "#,
            )
            .map_err(|e| format!("autometa audit mb stmt: {e}"))?;
        for row in stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            })
            .map_err(|e| format!("autometa audit mb map: {e}"))?
        {
            let (album_id, version_id, album_title, version_label, release_id) =
                row.map_err(|e| format!("autometa audit mb row: {e}"))?;
            if !release_id
                .as_deref()
                .is_some_and(is_valid_musicbrainz_release_id)
            {
                issues.push(AutoMetaAuditIssue {
                    album_id,
                    version_id: Some(version_id),
                    album_title,
                    version_label: Some(version_label),
                    kind: "invalid_musicbrainz_match".to_string(),
                    message: "MusicBrainz is marked matched without a valid release id".to_string(),
                });
            }
        }
        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, title, qobuz_album_id
                FROM albums
                WHERE qobuz_match_status = 'matched'
                  AND (qobuz_album_id IS NULL OR trim(qobuz_album_id) = '')
                "#,
            )
            .map_err(|e| format!("autometa audit qobuz stmt: {e}"))?;
        for row in stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })
            .map_err(|e| format!("autometa audit qobuz map: {e}"))?
        {
            let (album_id, album_title, qobuz_album_id) =
                row.map_err(|e| format!("autometa audit qobuz row: {e}"))?;
            issues.push(AutoMetaAuditIssue {
                album_id,
                version_id: None,
                album_title,
                version_label: None,
                kind: "invalid_qobuz_match".to_string(),
                message: format!(
                    "Qobuz is marked matched without a usable album id ({})",
                    qobuz_album_id.unwrap_or_default()
                ),
            });
        }
        Ok(issues)
    }

    fn autometa_job_versions(
        &self,
        mode: &str,
        link_qobuz: bool,
    ) -> Result<Vec<AutoMetaLocalVersion>, String> {
        let versions = self.autometa_local_versions()?;
        Ok(versions
            .into_iter()
            .filter(|version| match mode {
                "all" => true,
                "retry_errors" => autometa_version_has_error_or_stale_match(version),
                _ => !autometa_version_done(version, link_qobuz),
            })
            .collect())
    }

    fn autometa_job_items_with_conn(
        &self,
        conn: &rusqlite::Connection,
        job_id: i64,
        status: Option<&str>,
        limit: usize,
    ) -> Result<Vec<AutoMetaJobItem>, String> {
        let sql = if status.is_some() {
            r#"
            SELECT i.id, i.job_id, i.album_id, i.version_id, a.title,
                   COALESCE(v.source_label, v.title, 'Library'),
                   i.phase, i.status, i.attempts, i.musicbrainz_release_id,
                   i.qobuz_album_id, i.message, i.started_at, i.finished_at, i.updated_at
            FROM autometa_job_items i
            JOIN albums a ON a.id = i.album_id
            JOIN album_versions v ON v.id = i.version_id
            WHERE i.job_id = ?1 AND i.status = ?2
            ORDER BY i.updated_at DESC, i.id DESC
            LIMIT ?3
            "#
        } else {
            r#"
            SELECT i.id, i.job_id, i.album_id, i.version_id, a.title,
                   COALESCE(v.source_label, v.title, 'Library'),
                   i.phase, i.status, i.attempts, i.musicbrainz_release_id,
                   i.qobuz_album_id, i.message, i.started_at, i.finished_at, i.updated_at
            FROM autometa_job_items i
            JOIN albums a ON a.id = i.album_id
            JOIN album_versions v ON v.id = i.version_id
            WHERE i.job_id = ?1
            ORDER BY i.updated_at DESC, i.id DESC
            LIMIT ?2
            "#
        };
        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| format!("autometa job items stmt: {e}"))?;
        let rows = if let Some(status) = status {
            stmt.query_map(
                params![job_id, status, limit as i64],
                autometa_job_item_from_row,
            )
            .map_err(|e| format!("autometa job items map: {e}"))?
        } else {
            stmt.query_map(params![job_id, limit as i64], autometa_job_item_from_row)
                .map_err(|e| format!("autometa job items map: {e}"))?
        };
        collect_rows(rows)
    }
}

#[derive(Debug, Clone)]
pub struct AutoMetaJobWorkItem {
    pub item_id: i64,
    pub version: AutoMetaLocalVersion,
}

pub(crate) fn is_valid_musicbrainz_release_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for (idx, byte) in bytes.iter().enumerate() {
        if matches!(idx, 8 | 13 | 18 | 23) {
            if *byte != b'-' {
                return false;
            }
        } else if !byte.is_ascii_hexdigit() {
            return false;
        }
    }
    true
}

pub(crate) fn autometa_version_done(version: &AutoMetaLocalVersion, link_qobuz: bool) -> bool {
    autometa_musicbrainz_done(version) && (!link_qobuz || autometa_qobuz_done(version))
}

pub(crate) fn autometa_musicbrainz_done(version: &AutoMetaLocalVersion) -> bool {
    version.musicbrainz_match_status.as_deref() == Some("matched")
        && version
            .musicbrainz_release_id
            .as_deref()
            .is_some_and(is_valid_musicbrainz_release_id)
}

pub(crate) fn autometa_qobuz_done(version: &AutoMetaLocalVersion) -> bool {
    version.qobuz_match_status.as_deref() == Some("matched")
        || autometa_existing_qobuz_match(version).is_some()
}

pub(crate) fn autometa_existing_qobuz_match(version: &AutoMetaLocalVersion) -> Option<&str> {
    if version.album_qobuz_match_status.as_deref() != Some("matched") {
        return None;
    }
    version
        .album_qobuz_album_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
}

fn autometa_version_has_error_or_stale_match(version: &AutoMetaLocalVersion) -> bool {
    matches!(version.musicbrainz_match_status.as_deref(), Some("error"))
        || matches!(version.qobuz_match_status.as_deref(), Some("error"))
        || (version.musicbrainz_match_status.as_deref() == Some("matched")
            && !version
                .musicbrainz_release_id
                .as_deref()
                .is_some_and(is_valid_musicbrainz_release_id))
}

fn normalized_autometa_mode(mode: &str) -> String {
    match mode {
        "all" | "retry_errors" => mode.to_string(),
        _ => "remaining".to_string(),
    }
}

fn autometa_job_item_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AutoMetaJobItem> {
    Ok(AutoMetaJobItem {
        id: row.get(0)?,
        job_id: row.get(1)?,
        album_id: row.get(2)?,
        version_id: row.get(3)?,
        album_title: row.get(4)?,
        version_label: row.get(5)?,
        phase: row.get(6)?,
        status: row.get(7)?,
        attempts: row.get(8)?,
        musicbrainz_release_id: row.get(9)?,
        qobuz_album_id: row.get(10)?,
        message: row.get(11)?,
        started_at: row.get(12)?,
        finished_at: row.get(13)?,
        updated_at: row.get(14)?,
    })
}
