use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::Duration,
};

use rusqlite::{Connection, OpenFlags, OptionalExtension, Row, Transaction, params, types::Type};
use serde::{Deserialize, Serialize};

use crate::{
    ApplySession, ApplyState, ClassificationBasis, Error, FileFingerprint, FsIdentity, LocalRule,
    Plan, artifact::normalize_relative_path,
};

const SCHEMA_VERSION: i64 = 1;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MonitorRecord {
    pub id: String,
    pub source: String,
    pub source_identity: FsIdentity,
    pub folder_set_path: String,
    pub folder_set_sha256: String,
    pub interval_seconds: u64,
    pub enabled: bool,
    pub last_checked_unix_ms: Option<i64>,
    pub created_unix_ms: i64,
    pub updated_unix_ms: i64,
    pub deleted_unix_ms: Option<i64>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Planning,
    Planned,
    Applying,
    Completed,
    Noop,
    Failed,
    NeedsResume,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MonitoringRun {
    pub id: String,
    pub monitor_id: String,
    pub state: RunState,
    pub started_unix_ms: i64,
    pub finished_unix_ms: Option<i64>,
    pub plan_path: Option<String>,
    pub plan_sha256: Option<String>,
    pub apply_session_path: Option<String>,
    pub apply_session_id: Option<String>,
    pub total_files: u64,
    pub rule_matches: u64,
    pub name_matches: u64,
    pub content_matches: u64,
    pub fallback_matches: u64,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessedFileRecord {
    pub monitor_id: String,
    pub file_identity: FsIdentity,
    pub relative_path: String,
    pub content_sha256: String,
    pub size_bytes: u64,
    pub processing_signature: String,
    pub run_id: String,
    pub classification_basis: ClassificationBasis,
    pub rule_id: Option<String>,
    pub destination_id: String,
    pub processed_unix_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StagedFileRecord {
    pub file_id: String,
    pub file_identity: FsIdentity,
    pub relative_path: String,
    pub content_sha256: String,
    pub size_bytes: u64,
    pub processing_signature: String,
    pub classification_basis: ClassificationBasis,
    pub rule_id: Option<String>,
    pub destination_id: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ReconcileSummary {
    pub completed: usize,
    pub needs_resume: usize,
    pub failed: usize,
    pub needs_attention: usize,
}

pub struct StateStore {
    connection: Connection,
    path: Option<PathBuf>,
}

impl StateStore {
    pub fn open(path: &Path) -> Result<Self, Error> {
        validate_database_target(path)?;
        let parent = path
            .parent()
            .filter(|value| !value.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        if !parent.exists() {
            fs::create_dir_all(parent).map_err(|source| Error::FileSystem {
                action: "create state directory",
                path: parent.display().to_string(),
                source,
            })?;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).map_err(|source| {
                Error::FileSystem {
                    action: "set permissions on",
                    path: parent.display().to_string(),
                    source,
                }
            })?;
        }
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        )?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|source| {
            Error::FileSystem {
                action: "set permissions on",
                path: path.display().to_string(),
                source,
            }
        })?;
        Self::initialize(connection, Some(path.to_path_buf()))
    }

    pub fn open_in_memory() -> Result<Self, Error> {
        Self::initialize(Connection::open_in_memory()?, None)
    }

    fn initialize(mut connection: Connection, path: Option<PathBuf>) -> Result<Self, Error> {
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        migrate(&mut connection)?;
        Ok(Self { connection, path })
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn schema_version(&self) -> Result<i64, Error> {
        Ok(self.connection.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )?)
    }

    pub fn insert_monitor(&mut self, monitor: &MonitorRecord) -> Result<(), Error> {
        validate_monitor(monitor)?;
        if monitor.deleted_unix_ms.is_some() {
            return Err(Error::InvalidState(
                "a new monitor must not already be deleted".into(),
            ));
        }
        let transaction = self.connection.transaction()?;
        reject_overlapping_monitor(&transaction, &monitor.source)?;
        transaction.execute(
            "INSERT INTO monitors (
                id, source, source_device, source_inode, folder_set_path,
                folder_set_sha256, interval_seconds, enabled, last_checked_unix_ms,
                created_unix_ms, updated_unix_ms, deleted_unix_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, NULL)",
            params![
                monitor.id,
                monitor.source,
                monitor.source_identity.device.to_string(),
                monitor.source_identity.inode.to_string(),
                monitor.folder_set_path,
                monitor.folder_set_sha256,
                to_i64(monitor.interval_seconds, "monitor interval")?,
                monitor.enabled,
                monitor.last_checked_unix_ms,
                monitor.created_unix_ms,
                monitor.updated_unix_ms,
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn monitor(&self, id: &str) -> Result<Option<MonitorRecord>, Error> {
        Ok(self
            .connection
            .query_row(
                "SELECT id, source, source_device, source_inode, folder_set_path,
                        folder_set_sha256, interval_seconds, enabled, last_checked_unix_ms,
                        created_unix_ms, updated_unix_ms, deleted_unix_ms
                 FROM monitors WHERE id = ?1",
                [id],
                monitor_from_row,
            )
            .optional()?)
    }

    pub fn active_monitors(&self) -> Result<Vec<MonitorRecord>, Error> {
        let mut statement = self.connection.prepare(
            "SELECT id, source, source_device, source_inode, folder_set_path,
                    folder_set_sha256, interval_seconds, enabled, last_checked_unix_ms,
                    created_unix_ms, updated_unix_ms, deleted_unix_ms
             FROM monitors WHERE deleted_unix_ms IS NULL ORDER BY source, id",
        )?;
        let rows = statement.query_map([], monitor_from_row)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn set_monitor_enabled(
        &mut self,
        id: &str,
        enabled: bool,
        updated_unix_ms: i64,
    ) -> Result<(), Error> {
        require_changed(
            self.connection.execute(
                "UPDATE monitors SET enabled = ?2, updated_unix_ms = ?3
                 WHERE id = ?1 AND deleted_unix_ms IS NULL",
                params![id, enabled, updated_unix_ms],
            )?,
            "active monitor",
            id,
        )
    }

    pub fn update_monitor_check(&mut self, id: &str, checked_unix_ms: i64) -> Result<(), Error> {
        require_changed(
            self.connection.execute(
                "UPDATE monitors
                 SET last_checked_unix_ms = ?2, updated_unix_ms = ?2
                 WHERE id = ?1 AND deleted_unix_ms IS NULL",
                params![id, checked_unix_ms],
            )?,
            "active monitor",
            id,
        )
    }

    pub fn remove_monitor(&mut self, id: &str, deleted_unix_ms: i64) -> Result<(), Error> {
        let transaction = self.connection.transaction()?;
        let changed = transaction.execute(
            "UPDATE monitors
             SET enabled = 0, deleted_unix_ms = ?2, updated_unix_ms = ?2
             WHERE id = ?1 AND deleted_unix_ms IS NULL",
            params![id, deleted_unix_ms],
        )?;
        require_changed(changed, "active monitor", id)?;
        transaction.execute(
            "UPDATE rules
             SET enabled = 0, deleted_unix_ms = ?2, updated_unix_ms = ?2
             WHERE monitor_id = ?1 AND deleted_unix_ms IS NULL",
            params![id, deleted_unix_ms],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn insert_rule(&mut self, rule: &LocalRule, unix_ms: i64) -> Result<(), Error> {
        self.connection.execute(
            "INSERT INTO rules (
                id, monitor_id, name_glob, destination_id, priority, enabled,
                created_unix_ms, updated_unix_ms, deleted_unix_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, NULL)",
            params![
                rule.id,
                rule.monitor_id,
                rule.name_glob,
                rule.destination_id,
                rule.priority,
                rule.enabled,
                unix_ms,
            ],
        )?;
        Ok(())
    }

    pub fn active_rules(&self, monitor_id: &str) -> Result<Vec<LocalRule>, Error> {
        let mut statement = self.connection.prepare(
            "SELECT id, monitor_id, name_glob, destination_id, priority, enabled
             FROM rules
             WHERE monitor_id = ?1 AND deleted_unix_ms IS NULL
             ORDER BY priority DESC, id ASC",
        )?;
        let rows = statement.query_map([monitor_id], |row| {
            Ok(LocalRule {
                id: row.get(0)?,
                monitor_id: row.get(1)?,
                name_glob: row.get(2)?,
                destination_id: row.get(3)?,
                priority: row.get(4)?,
                enabled: row.get(5)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn rule(&self, id: &str) -> Result<Option<LocalRule>, Error> {
        Ok(self
            .connection
            .query_row(
                "SELECT id, monitor_id, name_glob, destination_id, priority, enabled
                 FROM rules WHERE id = ?1 AND deleted_unix_ms IS NULL",
                [id],
                |row| {
                    Ok(LocalRule {
                        id: row.get(0)?,
                        monitor_id: row.get(1)?,
                        name_glob: row.get(2)?,
                        destination_id: row.get(3)?,
                        priority: row.get(4)?,
                        enabled: row.get(5)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn set_rule_enabled(
        &mut self,
        id: &str,
        enabled: bool,
        updated_unix_ms: i64,
    ) -> Result<(), Error> {
        require_changed(
            self.connection.execute(
                "UPDATE rules SET enabled = ?2, updated_unix_ms = ?3
                 WHERE id = ?1 AND deleted_unix_ms IS NULL",
                params![id, enabled, updated_unix_ms],
            )?,
            "active rule",
            id,
        )
    }

    pub fn remove_rule(&mut self, id: &str, deleted_unix_ms: i64) -> Result<(), Error> {
        require_changed(
            self.connection.execute(
                "UPDATE rules
                 SET enabled = 0, deleted_unix_ms = ?2, updated_unix_ms = ?2
                 WHERE id = ?1 AND deleted_unix_ms IS NULL",
                params![id, deleted_unix_ms],
            )?,
            "active rule",
            id,
        )
    }

    pub fn start_run(
        &mut self,
        id: &str,
        monitor_id: &str,
        started_unix_ms: i64,
    ) -> Result<MonitoringRun, Error> {
        validate_identifier("run ID", id)?;
        let run = MonitoringRun {
            id: id.into(),
            monitor_id: monitor_id.into(),
            state: RunState::Planning,
            started_unix_ms,
            finished_unix_ms: None,
            plan_path: None,
            plan_sha256: None,
            apply_session_path: None,
            apply_session_id: None,
            total_files: 0,
            rule_matches: 0,
            name_matches: 0,
            content_matches: 0,
            fallback_matches: 0,
            error: None,
        };
        self.connection.execute(
            "INSERT INTO monitor_runs (
                id, monitor_id, state, started_unix_ms, finished_unix_ms,
                plan_path, plan_sha256, apply_session_path, apply_session_id,
                total_files, rule_matches, name_matches, content_matches,
                fallback_matches, error
             ) VALUES (?1, ?2, 'planning', ?3, NULL, NULL, NULL, NULL, NULL,
                       0, 0, 0, 0, 0, NULL)",
            params![id, monitor_id, started_unix_ms],
        )?;
        Ok(run)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record_plan(
        &mut self,
        run_id: &str,
        plan_path: &str,
        plan_sha256: &str,
        total_files: u64,
        rule_matches: u64,
        name_matches: u64,
        content_matches: u64,
        fallback_matches: u64,
    ) -> Result<(), Error> {
        self.record_plan_inner(
            run_id,
            plan_path,
            plan_sha256,
            total_files,
            rule_matches,
            name_matches,
            content_matches,
            fallback_matches,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record_plan_with_files(
        &mut self,
        run_id: &str,
        plan_path: &str,
        plan_sha256: &str,
        total_files: u64,
        rule_matches: u64,
        name_matches: u64,
        content_matches: u64,
        fallback_matches: u64,
        files: &[StagedFileRecord],
    ) -> Result<(), Error> {
        self.record_plan_inner(
            run_id,
            plan_path,
            plan_sha256,
            total_files,
            rule_matches,
            name_matches,
            content_matches,
            fallback_matches,
            Some(files),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn record_plan_inner(
        &mut self,
        run_id: &str,
        plan_path: &str,
        plan_sha256: &str,
        total_files: u64,
        rule_matches: u64,
        name_matches: u64,
        content_matches: u64,
        fallback_matches: u64,
        files: Option<&[StagedFileRecord]>,
    ) -> Result<(), Error> {
        validate_absolute_path("plan path", plan_path)?;
        validate_digest("plan digest", plan_sha256)?;
        let classified = rule_matches
            .checked_add(name_matches)
            .and_then(|count| count.checked_add(content_matches))
            .and_then(|count| count.checked_add(fallback_matches))
            .ok_or_else(|| Error::InvalidState("monitoring plan counts overflow".into()))?;
        if files.is_some_and(|files| classified != files.len() as u64) || classified > total_files {
            return Err(Error::InvalidState(
                "staged monitoring files must match classification counts".into(),
            ));
        }
        let transaction = self.connection.transaction()?;
        require_changed(
            transaction.execute(
                "UPDATE monitor_runs
                 SET state = 'planned', plan_path = ?2, plan_sha256 = ?3,
                     total_files = ?4, rule_matches = ?5, name_matches = ?6,
                     content_matches = ?7, fallback_matches = ?8, error = NULL
                 WHERE id = ?1 AND state = 'planning'",
                params![
                    run_id,
                    plan_path,
                    plan_sha256,
                    to_i64(total_files, "total files")?,
                    to_i64(rule_matches, "rule matches")?,
                    to_i64(name_matches, "name matches")?,
                    to_i64(content_matches, "content matches")?,
                    to_i64(fallback_matches, "fallback matches")?,
                ],
            )?,
            "planning run",
            run_id,
        )?;
        for file in files.unwrap_or_default() {
            validate_staged(file)?;
            transaction.execute(
                "INSERT INTO run_files (
                    run_id, file_id, file_device, file_inode, relative_path,
                    content_sha256, size_bytes, processing_signature,
                    classification_basis, rule_id, destination_id
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    run_id,
                    file.file_id,
                    file.file_identity.device.to_string(),
                    file.file_identity.inode.to_string(),
                    file.relative_path,
                    file.content_sha256,
                    to_i64(file.size_bytes, "staged file size")?,
                    file.processing_signature,
                    file.classification_basis.as_str(),
                    file.rule_id,
                    file.destination_id,
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn mark_run_applying(
        &mut self,
        run_id: &str,
        apply_session_path: &str,
    ) -> Result<(), Error> {
        validate_absolute_path("apply-session path", apply_session_path)?;
        require_changed(
            self.connection.execute(
                "UPDATE monitor_runs
                 SET state = 'applying', apply_session_path = ?2, error = NULL
                 WHERE id = ?1 AND state = 'planned'",
                params![run_id, apply_session_path],
            )?,
            "planned run",
            run_id,
        )
    }

    pub fn finish_run(
        &mut self,
        run_id: &str,
        state: RunState,
        finished_unix_ms: i64,
        error: Option<&str>,
    ) -> Result<(), Error> {
        if matches!(
            state,
            RunState::Planning | RunState::Planned | RunState::Applying | RunState::Completed
        ) {
            return Err(Error::InvalidState(
                "finish_run accepts only noop, failed, or needs_resume".into(),
            ));
        }
        require_changed(
            self.connection.execute(
                "UPDATE monitor_runs
                 SET state = ?2, finished_unix_ms = ?3, error = ?4
                 WHERE id = ?1 AND state != 'completed'",
                params![run_id, state.as_str(), finished_unix_ms, error],
            )?,
            "unfinished run",
            run_id,
        )
    }

    fn complete_run(
        &mut self,
        run_id: &str,
        apply_session_id: &str,
        finished_unix_ms: i64,
        processed: &[ProcessedFileRecord],
    ) -> Result<(), Error> {
        validate_identifier("apply-session ID", apply_session_id)?;
        let transaction = self.connection.transaction()?;
        let (monitor_id, apply_path): (String, Option<String>) = transaction
            .query_row(
                "SELECT monitor_id, apply_session_path FROM monitor_runs
                 WHERE id = ?1 AND state IN ('applying', 'needs_resume')
                   AND plan_path IS NOT NULL
                   AND plan_sha256 IS NOT NULL",
                [run_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| {
                Error::InvalidState(format!(
                    "run {run_id:?} is not an applying run with a durable plan"
                ))
            })?;
        if apply_path.is_none() {
            return Err(Error::InvalidState(
                "an applying run must reference an apply-session path".into(),
            ));
        }
        for record in processed {
            validate_processed(record, run_id, &monitor_id)?;
            transaction.execute(
                "INSERT INTO processed_files (
                    monitor_id, file_device, file_inode, relative_path,
                    content_sha256, size_bytes, processing_signature, run_id,
                    classification_basis, rule_id, destination_id, processed_unix_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
                 ON CONFLICT(monitor_id, file_device, file_inode) DO UPDATE SET
                    relative_path = excluded.relative_path,
                    content_sha256 = excluded.content_sha256,
                    size_bytes = excluded.size_bytes,
                    processing_signature = excluded.processing_signature,
                    run_id = excluded.run_id,
                    classification_basis = excluded.classification_basis,
                    rule_id = excluded.rule_id,
                    destination_id = excluded.destination_id,
                    processed_unix_ms = excluded.processed_unix_ms",
                params![
                    record.monitor_id,
                    record.file_identity.device.to_string(),
                    record.file_identity.inode.to_string(),
                    record.relative_path,
                    record.content_sha256,
                    to_i64(record.size_bytes, "processed file size")?,
                    record.processing_signature,
                    record.run_id,
                    record.classification_basis.as_str(),
                    record.rule_id,
                    record.destination_id,
                    record.processed_unix_ms,
                ],
            )?;
        }
        transaction.execute(
            "UPDATE monitor_runs
             SET state = 'completed', finished_unix_ms = ?2,
                 apply_session_id = ?3, error = NULL
             WHERE id = ?1",
            params![run_id, finished_unix_ms, apply_session_id],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn staged_files(&self, run_id: &str) -> Result<Vec<StagedFileRecord>, Error> {
        let mut statement = self.connection.prepare(
            "SELECT file_id, file_device, file_inode, relative_path,
                    content_sha256, size_bytes, processing_signature,
                    classification_basis, rule_id, destination_id
             FROM run_files WHERE run_id = ?1 ORDER BY file_id",
        )?;
        let rows = statement.query_map([run_id], staged_from_row)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    fn complete_staged_run(
        &mut self,
        run_id: &str,
        apply_session_id: &str,
        finished_unix_ms: i64,
    ) -> Result<(), Error> {
        let run = self
            .run(run_id)?
            .ok_or_else(|| Error::InvalidState(format!("unknown monitoring run {run_id:?}")))?;
        let processed = self
            .staged_files(run_id)?
            .into_iter()
            .map(|file| ProcessedFileRecord {
                monitor_id: run.monitor_id.clone(),
                file_identity: file.file_identity,
                relative_path: file.relative_path,
                content_sha256: file.content_sha256,
                size_bytes: file.size_bytes,
                processing_signature: file.processing_signature,
                run_id: run_id.into(),
                classification_basis: file.classification_basis,
                rule_id: file.rule_id,
                destination_id: file.destination_id,
                processed_unix_ms: finished_unix_ms,
            })
            .collect::<Vec<_>>();
        self.complete_run(run_id, apply_session_id, finished_unix_ms, &processed)
    }

    pub(crate) fn complete_from_completed_apply(
        &mut self,
        run_id: &str,
        plan: &Plan,
        apply: &ApplySession,
        finished_unix_ms: i64,
    ) -> Result<(), Error> {
        let run = self
            .run(run_id)?
            .ok_or_else(|| Error::InvalidState(format!("unknown monitoring run {run_id:?}")))?;
        let expected_plan_sha = required_run_value(run.plan_sha256.as_deref(), "plan digest")?;
        if plan.sha256()? != expected_plan_sha
            || apply.state != ApplyState::Completed
            || apply.source != plan.source
            || apply.plan_sha256 != expected_plan_sha
        {
            return Err(Error::InvalidState(
                "only a completed apply journal for the recorded monitoring Plan may complete a run"
                    .into(),
            ));
        }
        validate_staged_plan(&self.staged_files(run_id)?, plan)?;
        validate_apply_plan(apply, plan)?;
        self.complete_staged_run(run_id, &apply.id, finished_unix_ms)
    }

    pub fn reconcile_applying_runs(
        &mut self,
        monitor_id: Option<&str>,
        finished_unix_ms: i64,
    ) -> Result<ReconcileSummary, Error> {
        let runs = self.interrupted_runs(monitor_id)?;
        let mut summary = ReconcileSummary::default();
        for run in runs {
            let result = self.reconcile_run(&run, finished_unix_ms);
            match result {
                Ok(RunState::Completed) => summary.completed += 1,
                Ok(RunState::NeedsResume) => summary.needs_resume += 1,
                Ok(RunState::Failed) => summary.failed += 1,
                Ok(_) => unreachable!("reconciliation returns terminal index states"),
                Err(error) => {
                    self.finish_run(
                        &run.id,
                        RunState::Failed,
                        finished_unix_ms,
                        Some(&error.to_string()),
                    )?;
                    summary.needs_attention += 1;
                }
            }
        }
        Ok(summary)
    }

    fn interrupted_runs(&self, monitor_id: Option<&str>) -> Result<Vec<MonitoringRun>, Error> {
        let mut statement = self.connection.prepare(
            "SELECT id, monitor_id, state, started_unix_ms, finished_unix_ms,
                    plan_path, plan_sha256, apply_session_path, apply_session_id,
                    total_files, rule_matches, name_matches, content_matches,
                    fallback_matches, error
             FROM monitor_runs
             WHERE state IN ('applying', 'needs_resume')
               AND (?1 IS NULL OR monitor_id = ?1)
             ORDER BY started_unix_ms, id",
        )?;
        let rows = statement.query_map([monitor_id], run_from_row)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    fn reconcile_run(
        &mut self,
        run: &MonitoringRun,
        finished_unix_ms: i64,
    ) -> Result<RunState, Error> {
        let plan_path = required_run_value(run.plan_path.as_deref(), "plan path")?;
        let expected_plan_sha = required_run_value(run.plan_sha256.as_deref(), "plan digest")?;
        let apply_path = required_run_value(run.apply_session_path.as_deref(), "apply path")?;
        let plan = Plan::load(Path::new(plan_path))?;
        if plan.sha256()? != expected_plan_sha {
            return Err(Error::InvalidState(format!(
                "monitoring run {:?} references a plan with a different digest",
                run.id
            )));
        }
        let staged = self.staged_files(&run.id)?;
        validate_staged_plan(&staged, &plan)?;
        let apply = ApplySession::load(Path::new(apply_path))?;
        if apply.source != plan.source || apply.plan_sha256 != expected_plan_sha {
            return Err(Error::InvalidState(format!(
                "monitoring run {:?} references an unrelated apply session",
                run.id
            )));
        }
        validate_apply_plan(&apply, &plan)?;
        match apply.state {
            ApplyState::Completed => {
                self.complete_staged_run(&run.id, &apply.id, finished_unix_ms)?;
                Ok(RunState::Completed)
            }
            ApplyState::Running => {
                self.finish_run(
                    &run.id,
                    RunState::NeedsResume,
                    finished_unix_ms,
                    Some("apply session remains running and requires explicit resume"),
                )?;
                Ok(RunState::NeedsResume)
            }
            ApplyState::Failed | ApplyState::PartialFailure => {
                self.finish_run(
                    &run.id,
                    RunState::Failed,
                    finished_unix_ms,
                    Some("apply session did not complete"),
                )?;
                Ok(RunState::Failed)
            }
        }
    }

    pub fn run(&self, id: &str) -> Result<Option<MonitoringRun>, Error> {
        Ok(self
            .connection
            .query_row(
                "SELECT id, monitor_id, state, started_unix_ms, finished_unix_ms,
                        plan_path, plan_sha256, apply_session_path, apply_session_id,
                        total_files, rule_matches, name_matches, content_matches,
                        fallback_matches, error
                 FROM monitor_runs WHERE id = ?1",
                [id],
                run_from_row,
            )
            .optional()?)
    }

    pub fn recent_runs(
        &self,
        monitor_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<MonitoringRun>, Error> {
        if limit == 0 {
            return Err(Error::InvalidState(
                "run history limit must be positive".into(),
            ));
        }
        let sql = "SELECT id, monitor_id, state, started_unix_ms, finished_unix_ms,
                          plan_path, plan_sha256, apply_session_path, apply_session_id,
                          total_files, rule_matches, name_matches, content_matches,
                          fallback_matches, error
                   FROM monitor_runs
                   WHERE (?1 IS NULL OR monitor_id = ?1)
                   ORDER BY started_unix_ms DESC, id DESC LIMIT ?2";
        let mut statement = self.connection.prepare(sql)?;
        let rows = statement.query_map(params![monitor_id, i64::from(limit)], run_from_row)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn is_processed(
        &self,
        monitor_id: &str,
        fingerprint: &FileFingerprint,
        processing_signature: &str,
    ) -> Result<bool, Error> {
        validate_digest("processing signature", processing_signature)?;
        let found = self.connection.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM processed_files
                WHERE monitor_id = ?1 AND file_device = ?2 AND file_inode = ?3
                  AND content_sha256 = ?4 AND size_bytes = ?5
                  AND processing_signature = ?6
             )",
            params![
                monitor_id,
                fingerprint.identity.device.to_string(),
                fingerprint.identity.inode.to_string(),
                fingerprint.sha256,
                to_i64(fingerprint.size, "file size")?,
                processing_signature,
            ],
            |row| row.get(0),
        )?;
        Ok(found)
    }

    #[cfg(test)]
    fn connection(&self) -> &Connection {
        &self.connection
    }
}

impl RunState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Planning => "planning",
            Self::Planned => "planned",
            Self::Applying => "applying",
            Self::Completed => "completed",
            Self::Noop => "noop",
            Self::Failed => "failed",
            Self::NeedsResume => "needs_resume",
        }
    }

    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "planning" => Ok(Self::Planning),
            "planned" => Ok(Self::Planned),
            "applying" => Ok(Self::Applying),
            "completed" => Ok(Self::Completed),
            "noop" => Ok(Self::Noop),
            "failed" => Ok(Self::Failed),
            "needs_resume" => Ok(Self::NeedsResume),
            other => Err(format!("unknown monitoring run state {other:?}")),
        }
    }
}

impl ClassificationBasis {
    fn as_str(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Content => "content",
            Self::ExtensionFallback => "extension_fallback",
            Self::Rule => "rule",
        }
    }

    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "name" => Ok(Self::Name),
            "content" => Ok(Self::Content),
            "extension_fallback" => Ok(Self::ExtensionFallback),
            "rule" => Ok(Self::Rule),
            other => Err(format!("unknown classification basis {other:?}")),
        }
    }
}

fn validate_database_target(path: &Path) -> Result<(), Error> {
    if path.as_os_str().is_empty() || path == Path::new("-") {
        return Err(Error::InvalidState(
            "state database requires a persistent file path".into(),
        ));
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.file_type().is_file() || metadata.file_type().is_symlink() => {
            Err(Error::InvalidState(format!(
                "state database must be a regular non-symlink file: {:?}",
                path.display().to_string()
            )))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(Error::FileSystem {
            action: "inspect",
            path: path.display().to_string(),
            source,
        }),
    }
}

fn migrate(connection: &mut Connection) -> Result<(), Error> {
    let transaction = connection.transaction()?;
    transaction.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version INTEGER PRIMARY KEY,
            applied_unix_ms INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS monitors (
            id TEXT PRIMARY KEY,
            source TEXT NOT NULL,
            source_device TEXT NOT NULL,
            source_inode TEXT NOT NULL,
            folder_set_path TEXT NOT NULL,
            folder_set_sha256 TEXT NOT NULL,
            interval_seconds INTEGER NOT NULL CHECK(interval_seconds >= 10),
            enabled INTEGER NOT NULL CHECK(enabled IN (0, 1)),
            last_checked_unix_ms INTEGER,
            created_unix_ms INTEGER NOT NULL,
            updated_unix_ms INTEGER NOT NULL,
            deleted_unix_ms INTEGER
        );
        CREATE UNIQUE INDEX IF NOT EXISTS active_monitor_source
            ON monitors(source) WHERE deleted_unix_ms IS NULL;
        CREATE UNIQUE INDEX IF NOT EXISTS active_monitor_identity
            ON monitors(source_device, source_inode) WHERE deleted_unix_ms IS NULL;
        CREATE TABLE IF NOT EXISTS rules (
            id TEXT PRIMARY KEY,
            monitor_id TEXT NOT NULL REFERENCES monitors(id),
            name_glob TEXT NOT NULL,
            destination_id TEXT NOT NULL,
            priority INTEGER NOT NULL DEFAULT 50,
            enabled INTEGER NOT NULL CHECK(enabled IN (0, 1)),
            created_unix_ms INTEGER NOT NULL,
            updated_unix_ms INTEGER NOT NULL,
            deleted_unix_ms INTEGER
        );
        CREATE INDEX IF NOT EXISTS rules_by_monitor
            ON rules(monitor_id, priority DESC, id);
        CREATE TABLE IF NOT EXISTS monitor_runs (
            id TEXT PRIMARY KEY,
            monitor_id TEXT NOT NULL REFERENCES monitors(id),
            state TEXT NOT NULL CHECK(state IN (
                'planning', 'planned', 'applying', 'completed',
                'noop', 'failed', 'needs_resume'
            )),
            started_unix_ms INTEGER NOT NULL,
            finished_unix_ms INTEGER,
            plan_path TEXT,
            plan_sha256 TEXT,
            apply_session_path TEXT,
            apply_session_id TEXT,
            total_files INTEGER NOT NULL DEFAULT 0,
            rule_matches INTEGER NOT NULL DEFAULT 0,
            name_matches INTEGER NOT NULL DEFAULT 0,
            content_matches INTEGER NOT NULL DEFAULT 0,
            fallback_matches INTEGER NOT NULL DEFAULT 0,
            error TEXT
        );
        CREATE INDEX IF NOT EXISTS runs_by_monitor_time
            ON monitor_runs(monitor_id, started_unix_ms DESC);
        CREATE TABLE IF NOT EXISTS run_files (
            run_id TEXT NOT NULL REFERENCES monitor_runs(id),
            file_id TEXT NOT NULL,
            file_device TEXT NOT NULL,
            file_inode TEXT NOT NULL,
            relative_path TEXT NOT NULL,
            content_sha256 TEXT NOT NULL,
            size_bytes INTEGER NOT NULL,
            processing_signature TEXT NOT NULL,
            classification_basis TEXT NOT NULL CHECK(classification_basis IN (
                'name', 'content', 'extension_fallback', 'rule'
            )),
            rule_id TEXT,
            destination_id TEXT NOT NULL,
            PRIMARY KEY (run_id, file_id),
            UNIQUE (run_id, relative_path),
            CHECK (
                (classification_basis = 'rule' AND rule_id IS NOT NULL) OR
                (classification_basis != 'rule' AND rule_id IS NULL)
            )
        );
        CREATE TABLE IF NOT EXISTS processed_files (
            monitor_id TEXT NOT NULL REFERENCES monitors(id),
            file_device TEXT NOT NULL,
            file_inode TEXT NOT NULL,
            relative_path TEXT NOT NULL,
            content_sha256 TEXT NOT NULL,
            size_bytes INTEGER NOT NULL,
            processing_signature TEXT NOT NULL,
            run_id TEXT NOT NULL REFERENCES monitor_runs(id),
            classification_basis TEXT NOT NULL CHECK(classification_basis IN (
                'name', 'content', 'extension_fallback', 'rule'
            )),
            rule_id TEXT,
            destination_id TEXT NOT NULL,
            processed_unix_ms INTEGER NOT NULL,
            PRIMARY KEY (monitor_id, file_device, file_inode),
            CHECK (
                (classification_basis = 'rule' AND rule_id IS NOT NULL) OR
                (classification_basis != 'rule' AND rule_id IS NULL)
            )
        );",
    )?;
    transaction.execute(
        "INSERT OR IGNORE INTO schema_migrations (version, applied_unix_ms)
         VALUES (?1, CAST(strftime('%s', 'now') AS INTEGER) * 1000)",
        [SCHEMA_VERSION],
    )?;
    transaction.commit()?;
    Ok(())
}

fn reject_overlapping_monitor(transaction: &Transaction<'_>, source: &str) -> Result<(), Error> {
    let mut statement = transaction
        .prepare("SELECT source FROM monitors WHERE deleted_unix_ms IS NULL ORDER BY source")?;
    let sources = statement.query_map([], |row| row.get::<_, String>(0))?;
    let candidate = Path::new(source);
    for existing in sources {
        let existing = existing?;
        let existing_path = Path::new(&existing);
        if candidate.starts_with(existing_path) || existing_path.starts_with(candidate) {
            return Err(Error::InvalidState(format!(
                "monitor source overlaps active source {existing:?}"
            )));
        }
    }
    Ok(())
}

fn monitor_from_row(row: &Row<'_>) -> rusqlite::Result<MonitorRecord> {
    let device: String = row.get(2)?;
    let inode: String = row.get(3)?;
    Ok(MonitorRecord {
        id: row.get(0)?,
        source: row.get(1)?,
        source_identity: FsIdentity {
            device: parse_u64_column(device, 2)?,
            inode: parse_u64_column(inode, 3)?,
        },
        folder_set_path: row.get(4)?,
        folder_set_sha256: row.get(5)?,
        interval_seconds: i64_to_u64(row.get(6)?, 6)?,
        enabled: row.get(7)?,
        last_checked_unix_ms: row.get(8)?,
        created_unix_ms: row.get(9)?,
        updated_unix_ms: row.get(10)?,
        deleted_unix_ms: row.get(11)?,
    })
}

fn run_from_row(row: &Row<'_>) -> rusqlite::Result<MonitoringRun> {
    let state: String = row.get(2)?;
    Ok(MonitoringRun {
        id: row.get(0)?,
        monitor_id: row.get(1)?,
        state: RunState::parse(&state).map_err(|error| conversion_error(2, error))?,
        started_unix_ms: row.get(3)?,
        finished_unix_ms: row.get(4)?,
        plan_path: row.get(5)?,
        plan_sha256: row.get(6)?,
        apply_session_path: row.get(7)?,
        apply_session_id: row.get(8)?,
        total_files: i64_to_u64(row.get(9)?, 9)?,
        rule_matches: i64_to_u64(row.get(10)?, 10)?,
        name_matches: i64_to_u64(row.get(11)?, 11)?,
        content_matches: i64_to_u64(row.get(12)?, 12)?,
        fallback_matches: i64_to_u64(row.get(13)?, 13)?,
        error: row.get(14)?,
    })
}

fn staged_from_row(row: &Row<'_>) -> rusqlite::Result<StagedFileRecord> {
    let device: String = row.get(1)?;
    let inode: String = row.get(2)?;
    let basis: String = row.get(7)?;
    Ok(StagedFileRecord {
        file_id: row.get(0)?,
        file_identity: FsIdentity {
            device: parse_u64_column(device, 1)?,
            inode: parse_u64_column(inode, 2)?,
        },
        relative_path: row.get(3)?,
        content_sha256: row.get(4)?,
        size_bytes: i64_to_u64(row.get(5)?, 5)?,
        processing_signature: row.get(6)?,
        classification_basis: ClassificationBasis::parse(&basis)
            .map_err(|error| conversion_error(7, error))?,
        rule_id: row.get(8)?,
        destination_id: row.get(9)?,
    })
}

fn validate_monitor(monitor: &MonitorRecord) -> Result<(), Error> {
    validate_identifier("monitor ID", &monitor.id)?;
    validate_absolute_path("monitor source", &monitor.source)?;
    validate_absolute_path("folder-set path", &monitor.folder_set_path)?;
    validate_digest("folder-set digest", &monitor.folder_set_sha256)?;
    if monitor.interval_seconds < 10 {
        return Err(Error::InvalidState(
            "monitor interval must be at least 10 seconds".into(),
        ));
    }
    Ok(())
}

fn validate_processed(
    record: &ProcessedFileRecord,
    run_id: &str,
    monitor_id: &str,
) -> Result<(), Error> {
    if record.run_id != run_id || record.monitor_id != monitor_id {
        return Err(Error::InvalidState(
            "processed file must belong to the completed run and monitor".into(),
        ));
    }
    validate_identifier("destination ID", &record.destination_id)?;
    validate_digest("content digest", &record.content_sha256)?;
    validate_digest("processing signature", &record.processing_signature)?;
    if record.relative_path.is_empty() || record.relative_path.chars().any(char::is_control) {
        return Err(Error::InvalidState(
            "processed relative path must be non-empty and contain no control characters".into(),
        ));
    }
    match record.classification_basis {
        ClassificationBasis::Rule
            if record
                .rule_id
                .as_deref()
                .is_none_or(|id| id.trim().is_empty()) =>
        {
            Err(Error::InvalidState(
                "rule-classified processed file must contain a rule ID".into(),
            ))
        }
        ClassificationBasis::Rule => Ok(()),
        _ if record.rule_id.is_some() => Err(Error::InvalidState(
            "only rule-classified processed files may contain a rule ID".into(),
        )),
        _ => Ok(()),
    }
}

fn validate_staged(record: &StagedFileRecord) -> Result<(), Error> {
    validate_identifier("staged file ID", &record.file_id)?;
    normalize_relative_path(&record.relative_path)?;
    validate_digest("staged content digest", &record.content_sha256)?;
    validate_digest("staged processing signature", &record.processing_signature)?;
    validate_identifier("staged destination ID", &record.destination_id)?;
    match (record.classification_basis, record.rule_id.as_deref()) {
        (ClassificationBasis::Rule, Some(rule_id)) => validate_identifier("rule ID", rule_id),
        (ClassificationBasis::Rule, None) => Err(Error::InvalidState(
            "rule-classified staged file must contain a rule ID".into(),
        )),
        (_, None) => Ok(()),
        (_, Some(_)) => Err(Error::InvalidState(
            "non-rule staged file must not contain a rule ID".into(),
        )),
    }
}

fn validate_staged_plan(staged: &[StagedFileRecord], plan: &Plan) -> Result<(), Error> {
    if staged.len() != plan.entries.len() {
        return Err(Error::InvalidState(
            "staged monitoring files do not match the referenced plan".into(),
        ));
    }
    for (staged, entry) in staged.iter().zip(&plan.entries) {
        if staged.file_id != entry.file_id
            || staged.file_identity != entry.source_fingerprint.identity
            || staged.relative_path != entry.source_path
            || staged.content_sha256 != entry.source_fingerprint.sha256
            || staged.size_bytes != entry.source_fingerprint.size
            || staged.classification_basis != entry.classification_basis
            || staged.rule_id != entry.rule_id
            || staged.destination_id != entry.destination_id
        {
            return Err(Error::InvalidState(format!(
                "staged monitoring file {:?} does not match its plan entry",
                staged.file_id
            )));
        }
    }
    Ok(())
}

fn validate_apply_plan(apply: &ApplySession, plan: &Plan) -> Result<(), Error> {
    if apply.moves.len() != plan.entries.len() {
        return Err(Error::InvalidState(
            "apply session moves do not match the referenced plan".into(),
        ));
    }
    for (movement, entry) in apply.moves.iter().zip(&plan.entries) {
        if movement.file_id != entry.file_id
            || movement.source_path != entry.source_path
            || movement.destination_path != entry.destination_path
            || movement.fingerprint != entry.source_fingerprint
        {
            return Err(Error::InvalidState(format!(
                "apply move {:?} does not match its plan entry",
                movement.file_id
            )));
        }
    }
    Ok(())
}

fn required_run_value<'a>(value: Option<&'a str>, name: &str) -> Result<&'a str, Error> {
    value.ok_or_else(|| Error::InvalidState(format!("applying run is missing {name}")))
}

fn validate_identifier(name: &str, value: &str) -> Result<(), Error> {
    if value.trim().is_empty() || value.chars().any(char::is_control) {
        return Err(Error::InvalidState(format!(
            "{name} must be non-empty and contain no control characters"
        )));
    }
    Ok(())
}

fn validate_absolute_path(name: &str, value: &str) -> Result<(), Error> {
    if !Path::new(value).is_absolute() || value.chars().any(char::is_control) {
        return Err(Error::InvalidState(format!(
            "{name} must be an absolute path without control characters"
        )));
    }
    Ok(())
}

fn validate_digest(name: &str, value: &str) -> Result<(), Error> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(Error::InvalidState(format!(
            "{name} must be a lowercase SHA-256 digest"
        )));
    }
    Ok(())
}

fn require_changed(changed: usize, kind: &str, id: &str) -> Result<(), Error> {
    if changed == 0 {
        Err(Error::InvalidState(format!(
            "no matching {kind} found for ID {id:?}"
        )))
    } else {
        Ok(())
    }
}

fn to_i64(value: u64, name: &str) -> Result<i64, Error> {
    i64::try_from(value)
        .map_err(|_| Error::InvalidState(format!("{name} exceeds SQLite integer range")))
}

fn i64_to_u64(value: i64, column: usize) -> rusqlite::Result<u64> {
    u64::try_from(value).map_err(|error| conversion_error(column, error.to_string()))
}

fn parse_u64_column(value: String, column: usize) -> rusqlite::Result<u64> {
    value
        .parse::<u64>()
        .map_err(|error| conversion_error(column, error.to_string()))
}

fn conversion_error(column: usize, message: String) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        column,
        Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            message,
        )),
    )
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    const DIGEST_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const DIGEST_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn monitor(id: &str, source: &Path, artifact: &Path) -> MonitorRecord {
        MonitorRecord {
            id: id.into(),
            source: source.display().to_string(),
            source_identity: FsIdentity {
                device: 10,
                inode: if id == "m1" { 20 } else { 21 },
            },
            folder_set_path: artifact.display().to_string(),
            folder_set_sha256: DIGEST_A.into(),
            interval_seconds: 60,
            enabled: true,
            last_checked_unix_ms: None,
            created_unix_ms: 100,
            updated_unix_ms: 100,
            deleted_unix_ms: None,
        }
    }

    fn setup_monitor(store: &mut StateStore, root: &Path) {
        let source = root.join("source");
        let artifact = root.join("folders.json");
        fs::create_dir(&source).unwrap();
        store
            .insert_monitor(&monitor("m1", &source, &artifact))
            .unwrap();
    }

    #[test]
    fn migrates_idempotently_and_enforces_foreign_keys() {
        let root = tempdir().unwrap();
        let path = root.path().join("state/temari.sqlite3");
        let store = StateStore::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        assert_eq!(
            store
                .connection()
                .pragma_query_value(None, "foreign_keys", |row| row.get::<_, i64>(0))
                .unwrap(),
            1
        );
        drop(store);

        let reopened = StateStore::open(&path).unwrap();
        assert_eq!(reopened.schema_version().unwrap(), SCHEMA_VERSION);
        assert_eq!(
            reopened
                .connection()
                .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }

    #[test]
    fn creates_owner_only_database_and_new_parent() {
        let root = tempdir().unwrap();
        let parent = root.path().join("private-state");
        let path = parent.join("temari.sqlite3");
        StateStore::open(&path).unwrap();

        assert_eq!(
            fs::metadata(&parent).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn rejects_overlapping_active_monitors_but_allows_readding_after_removal() {
        let root = tempdir().unwrap();
        let parent = root.path().join("source");
        let child = parent.join("nested");
        fs::create_dir_all(&child).unwrap();
        let artifact = root.path().join("folders.json");
        let mut store = StateStore::open_in_memory().unwrap();
        store
            .insert_monitor(&monitor("m1", &parent, &artifact))
            .unwrap();

        assert!(
            store
                .insert_monitor(&monitor("m2", &child, &artifact))
                .is_err()
        );
        store.remove_monitor("m1", 200).unwrap();
        store
            .insert_monitor(&monitor("m2", &child, &artifact))
            .unwrap();
    }

    #[test]
    fn persists_and_soft_deletes_rules() {
        let root = tempdir().unwrap();
        let mut store = StateStore::open_in_memory().unwrap();
        setup_monitor(&mut store, root.path());
        let rule = LocalRule {
            id: "r1".into(),
            monitor_id: "m1".into(),
            name_glob: "*.pdf".into(),
            destination_id: "d1".into(),
            priority: 50,
            enabled: true,
        };
        store.insert_rule(&rule, 100).unwrap();
        assert_eq!(store.active_rules("m1").unwrap(), [rule]);

        store.remove_rule("r1", 200).unwrap();
        assert!(store.active_rules("m1").unwrap().is_empty());
        assert_eq!(
            store
                .connection()
                .query_row("SELECT COUNT(*) FROM rules", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            1
        );
    }

    #[test]
    fn records_processed_files_only_as_part_of_completed_run() {
        let root = tempdir().unwrap();
        let mut store = StateStore::open_in_memory().unwrap();
        setup_monitor(&mut store, root.path());
        store.start_run("run1", "m1", 100).unwrap();
        let plan = root.path().join("plan.json").display().to_string();
        let apply = root.path().join("apply.json").display().to_string();
        store
            .record_plan("run1", &plan, DIGEST_A, 1, 1, 0, 0, 0)
            .unwrap();
        store.mark_run_applying("run1", &apply).unwrap();
        let processed = ProcessedFileRecord {
            monitor_id: "m1".into(),
            file_identity: FsIdentity {
                device: u64::MAX,
                inode: u64::MAX,
            },
            relative_path: "report.pdf".into(),
            content_sha256: DIGEST_A.into(),
            size_bytes: 42,
            processing_signature: DIGEST_B.into(),
            run_id: "run1".into(),
            classification_basis: ClassificationBasis::Rule,
            rule_id: Some("r1".into()),
            destination_id: "d1".into(),
            processed_unix_ms: 200,
        };
        store
            .complete_run("run1", "apply1", 200, std::slice::from_ref(&processed))
            .unwrap();

        assert_eq!(
            store.run("run1").unwrap().unwrap().state,
            RunState::Completed
        );
        assert!(
            store
                .is_processed(
                    "m1",
                    &FileFingerprint {
                        identity: processed.file_identity,
                        size: 42,
                        sha256: DIGEST_A.into(),
                    },
                    DIGEST_B,
                )
                .unwrap()
        );
    }

    #[test]
    fn processing_signature_or_content_change_makes_file_eligible_again() {
        let root = tempdir().unwrap();
        let mut store = StateStore::open_in_memory().unwrap();
        setup_monitor(&mut store, root.path());
        store.start_run("run1", "m1", 100).unwrap();
        let plan = root.path().join("plan.json").display().to_string();
        let apply = root.path().join("apply.json").display().to_string();
        store
            .record_plan("run1", &plan, DIGEST_A, 1, 0, 1, 0, 0)
            .unwrap();
        store.mark_run_applying("run1", &apply).unwrap();
        let record = ProcessedFileRecord {
            monitor_id: "m1".into(),
            file_identity: FsIdentity {
                device: 1,
                inode: 2,
            },
            relative_path: "report.pdf".into(),
            content_sha256: DIGEST_A.into(),
            size_bytes: 42,
            processing_signature: DIGEST_A.into(),
            run_id: "run1".into(),
            classification_basis: ClassificationBasis::Name,
            rule_id: None,
            destination_id: "d1".into(),
            processed_unix_ms: 200,
        };
        store
            .complete_run("run1", "apply1", 200, &[record])
            .unwrap();
        let fingerprint = FileFingerprint {
            identity: FsIdentity {
                device: 1,
                inode: 2,
            },
            size: 42,
            sha256: DIGEST_A.into(),
        };

        assert!(!store.is_processed("m1", &fingerprint, DIGEST_B).unwrap());
        let changed = FileFingerprint {
            sha256: DIGEST_B.into(),
            ..fingerprint
        };
        assert!(!store.is_processed("m1", &changed, DIGEST_A).unwrap());
    }

    #[test]
    fn schema_does_not_store_workflow_artifact_bodies_or_extracted_text() {
        let store = StateStore::open_in_memory().unwrap();
        let mut statement = store
            .connection()
            .prepare("SELECT sql FROM sqlite_schema WHERE type = 'table' ORDER BY name")
            .unwrap();
        let schema = statement
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .join("\n")
            .to_ascii_lowercase();

        for forbidden in [
            "artifact_body",
            "extracted_text",
            "api_key",
            "raw_model_response",
        ] {
            assert!(!schema.contains(forbidden), "schema contains {forbidden}");
        }
    }
}
