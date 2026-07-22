use std::{
    collections::HashSet,
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

const SCHEMA_VERSION: i64 = 8;

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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedWorkspace {
    pub id: String,
    pub monitor_id: String,
    pub source: String,
    pub source_identity: FsIdentity,
    pub folder_set_path: String,
    pub folder_set_sha256: String,
    pub config_path: String,
    pub retention_seconds: u64,
    pub settle_seconds: u64,
    pub enabled: bool,
    pub setup_session_path: Option<String>,
    pub created_unix_ms: i64,
    pub updated_unix_ms: i64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecentsState {
    Pending,
    Planned,
    Moved,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecentsItem {
    pub workspace_id: String,
    pub file_identity: FsIdentity,
    pub relative_path: String,
    pub content_sha256: String,
    pub size_bytes: u64,
    pub first_seen_unix_ms: i64,
    pub stable_since_unix_ms: i64,
    pub eligible_unix_ms: i64,
    pub state: RecentsState,
    pub last_run_id: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedRunKind {
    Setup,
    Adopt,
    Stage,
    Classify,
    Configure,
    Reorganize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedRun {
    pub id: String,
    pub workspace_id: String,
    pub kind: ManagedRunKind,
    pub state: RunState,
    pub plan_path: Option<String>,
    pub apply_path: Option<String>,
    pub undo_path: Option<String>,
    pub started_unix_ms: i64,
    pub finished_unix_ms: Option<i64>,
    pub move_count: u64,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ReconcileSummary {
    pub completed: usize,
    pub needs_resume: usize,
    pub failed: usize,
    pub needs_attention: usize,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct RecentsReconcileSummary {
    pub deleted_stale_pending: usize,
    pub reset_returned: usize,
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
        initialize_schema(&mut connection)?;
        Ok(Self { connection, path })
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn schema_version(&self) -> Result<i64, Error> {
        Ok(self.connection.query_row(
            "SELECT version FROM schema_metadata WHERE id = 1",
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

    pub fn insert_managed_workspace(&mut self, workspace: &ManagedWorkspace) -> Result<(), Error> {
        validate_managed_workspace(workspace)?;
        validate_workspace_monitor(&self.connection, workspace)?;
        self.connection.execute(
            "INSERT INTO managed_workspaces (
                id, monitor_id, source, source_device, source_inode, folder_set_path,
                folder_set_sha256, config_path, retention_seconds, settle_seconds, enabled,
                setup_session_path, created_unix_ms, updated_unix_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                workspace.id,
                workspace.monitor_id,
                workspace.source,
                workspace.source_identity.device.to_string(),
                workspace.source_identity.inode.to_string(),
                workspace.folder_set_path,
                workspace.folder_set_sha256,
                workspace.config_path,
                to_i64(workspace.retention_seconds, "workspace retention")?,
                to_i64(workspace.settle_seconds, "workspace settle window")?,
                workspace.enabled,
                workspace.setup_session_path,
                workspace.created_unix_ms,
                workspace.updated_unix_ms,
            ],
        )?;
        Ok(())
    }

    pub fn managed_workspace(&self, id: &str) -> Result<Option<ManagedWorkspace>, Error> {
        Ok(self
            .connection
            .query_row(
                "SELECT id, monitor_id, source, source_device, source_inode, folder_set_path,
                        folder_set_sha256, config_path, retention_seconds, settle_seconds, enabled,
                        setup_session_path, created_unix_ms, updated_unix_ms
                 FROM managed_workspaces WHERE id = ?1",
                [id],
                managed_workspace_from_row,
            )
            .optional()?)
    }

    pub fn managed_workspaces(&self) -> Result<Vec<ManagedWorkspace>, Error> {
        let mut statement = self.connection.prepare(
            "SELECT id, monitor_id, source, source_device, source_inode, folder_set_path,
                    folder_set_sha256, config_path, retention_seconds, settle_seconds, enabled,
                    setup_session_path, created_unix_ms, updated_unix_ms
             FROM managed_workspaces ORDER BY source, id",
        )?;
        let rows = statement.query_map([], managed_workspace_from_row)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn set_managed_workspace_enabled(
        &mut self,
        id: &str,
        enabled: bool,
        updated_unix_ms: i64,
    ) -> Result<ManagedWorkspace, Error> {
        let transaction = self.connection.transaction()?;
        let mut workspace = managed_workspace_for_update(&transaction, id)?;
        validate_workspace_update_time(&workspace, updated_unix_ms)?;
        if enabled {
            let library_work_pending: bool = transaction.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM managed_runs
                    WHERE workspace_id = ?1 AND kind IN ('configure', 'reorganize')
                      AND state IN ('applying', 'needs_resume')
                 )",
                [id],
                |row| row.get(0),
            )?;
            if library_work_pending {
                return Err(Error::InvalidState(
                    "managed workspace cannot be enabled while AI Library work needs recovery"
                        .into(),
                ));
            }
        }
        require_changed(
            transaction.execute(
                "UPDATE monitors SET enabled = ?2, updated_unix_ms = ?3
                 WHERE id = ?1 AND deleted_unix_ms IS NULL",
                params![workspace.monitor_id, enabled, updated_unix_ms],
            )?,
            "active workspace monitor",
            &workspace.monitor_id,
        )?;
        require_changed(
            transaction.execute(
                "UPDATE managed_workspaces SET enabled = ?2, updated_unix_ms = ?3
                 WHERE id = ?1",
                params![id, enabled, updated_unix_ms],
            )?,
            "managed workspace",
            id,
        )?;
        transaction.commit()?;
        workspace.enabled = enabled;
        workspace.updated_unix_ms = updated_unix_ms;
        Ok(workspace)
    }

    pub fn update_managed_workspace_windows(
        &mut self,
        id: &str,
        retention_seconds: u64,
        settle_seconds: u64,
        updated_unix_ms: i64,
    ) -> Result<ManagedWorkspace, Error> {
        let retention_ms = duration_millis(retention_seconds, "workspace retention")?;
        let settle_ms = duration_millis(settle_seconds, "recents settle window")?;
        if retention_seconds == 0 || settle_seconds == 0 {
            return Err(Error::InvalidState(
                "workspace retention and settle windows must be greater than zero".into(),
            ));
        }
        let transaction = self.connection.transaction()?;
        let mut workspace = managed_workspace_for_update(&transaction, id)?;
        validate_workspace_update_time(&workspace, updated_unix_ms)?;
        let pending = {
            let mut statement = transaction.prepare(
                "SELECT file_device, file_inode, first_seen_unix_ms, stable_since_unix_ms
                 FROM recents_items WHERE workspace_id = ?1 AND state = 'pending'",
            )?;
            let rows = statement.query_map([id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let mut deadlines = Vec::with_capacity(pending.len());
        for (device, inode, first_seen, stable_since) in pending {
            let eligible = first_seen
                .checked_add(retention_ms)
                .and_then(|retention_deadline| {
                    stable_since
                        .checked_add(settle_ms)
                        .map(|settle_deadline| retention_deadline.max(settle_deadline))
                })
                .ok_or_else(|| {
                    Error::InvalidState("recents eligibility timestamp overflow".into())
                })?;
            deadlines.push((device, inode, eligible));
        }
        require_changed(
            transaction.execute(
                "UPDATE managed_workspaces
                 SET retention_seconds = ?2, settle_seconds = ?3, updated_unix_ms = ?4
                 WHERE id = ?1",
                params![
                    id,
                    to_i64(retention_seconds, "workspace retention")?,
                    to_i64(settle_seconds, "recents settle window")?,
                    updated_unix_ms,
                ],
            )?,
            "managed workspace",
            id,
        )?;
        for (device, inode, eligible) in deadlines {
            transaction.execute(
                "UPDATE recents_items SET eligible_unix_ms = ?4
                 WHERE workspace_id = ?1 AND file_device = ?2 AND file_inode = ?3
                   AND state = 'pending'",
                params![id, device, inode, eligible],
            )?;
        }
        transaction.commit()?;
        workspace.retention_seconds = retention_seconds;
        workspace.settle_seconds = settle_seconds;
        workspace.updated_unix_ms = updated_unix_ms;
        Ok(workspace)
    }

    pub fn remove_managed_workspace_registration(
        &mut self,
        id: &str,
        deleted_unix_ms: i64,
    ) -> Result<(), Error> {
        let transaction = self.connection.transaction()?;
        let workspace = managed_workspace_for_update(&transaction, id)?;
        validate_workspace_update_time(&workspace, deleted_unix_ms)?;
        if workspace.enabled {
            return Err(Error::InvalidState(
                "managed workspace must be disabled before removal".into(),
            ));
        }
        let monitor_enabled: bool = transaction.query_row(
            "SELECT enabled FROM monitors WHERE id = ?1 AND deleted_unix_ms IS NULL",
            [&workspace.monitor_id],
            |row| row.get(0),
        )?;
        if monitor_enabled {
            return Err(Error::InvalidState(
                "managed workspace monitor must be disabled before removal".into(),
            ));
        }
        let unfinished_managed: bool = transaction.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM managed_runs
                WHERE workspace_id = ?1
                  AND state IN ('planning', 'planned', 'applying', 'needs_resume')
             )",
            [id],
            |row| row.get(0),
        )?;
        let unfinished_monitoring: bool = transaction.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM monitor_runs
                WHERE monitor_id = ?1
                  AND state IN ('planning', 'planned', 'applying', 'needs_resume')
             )",
            [&workspace.monitor_id],
            |row| row.get(0),
        )?;
        if unfinished_managed || unfinished_monitoring {
            return Err(Error::InvalidState(
                "managed workspace has an unfinished or resumable run".into(),
            ));
        }
        require_changed(
            transaction.execute("DELETE FROM managed_workspaces WHERE id = ?1", [id])?,
            "managed workspace",
            id,
        )?;
        transaction.execute(
            "UPDATE rules
             SET enabled = 0, deleted_unix_ms = ?2, updated_unix_ms = ?2
             WHERE monitor_id = ?1 AND deleted_unix_ms IS NULL",
            params![workspace.monitor_id, deleted_unix_ms],
        )?;
        require_changed(
            transaction.execute(
                "UPDATE monitors
                 SET enabled = 0, deleted_unix_ms = ?2, updated_unix_ms = ?2
                 WHERE id = ?1 AND enabled = 0 AND deleted_unix_ms IS NULL",
                params![workspace.monitor_id, deleted_unix_ms],
            )?,
            "disabled workspace monitor",
            &workspace.monitor_id,
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn discard_planned_managed_runs(&mut self, workspace_id: &str) -> Result<(), Error> {
        self.connection.execute(
            "DELETE FROM managed_runs WHERE workspace_id = ?1 AND state IN ('planning', 'planned')",
            [workspace_id],
        )?;
        Ok(())
    }

    pub fn update_managed_workspace(&mut self, workspace: &ManagedWorkspace) -> Result<(), Error> {
        validate_managed_workspace(workspace)?;
        validate_workspace_monitor(&self.connection, workspace)?;
        require_changed(
            self.connection.execute(
                "UPDATE managed_workspaces
                 SET monitor_id = ?2, source = ?3, source_device = ?4, source_inode = ?5,
                     folder_set_path = ?6, folder_set_sha256 = ?7, config_path = ?8,
                     retention_seconds = ?9, settle_seconds = ?10, enabled = ?11,
                     setup_session_path = ?12, created_unix_ms = ?13, updated_unix_ms = ?14
                 WHERE id = ?1",
                params![
                    workspace.id,
                    workspace.monitor_id,
                    workspace.source,
                    workspace.source_identity.device.to_string(),
                    workspace.source_identity.inode.to_string(),
                    workspace.folder_set_path,
                    workspace.folder_set_sha256,
                    workspace.config_path,
                    to_i64(workspace.retention_seconds, "workspace retention")?,
                    to_i64(workspace.settle_seconds, "workspace settle window")?,
                    workspace.enabled,
                    workspace.setup_session_path,
                    workspace.created_unix_ms,
                    workspace.updated_unix_ms,
                ],
            )?,
            "managed workspace",
            &workspace.id,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn replace_managed_folder_set_binding(
        &mut self,
        workspace_id: &str,
        edit_run_id: &str,
        expected_path: &str,
        expected_sha256: &str,
        replacement_path: &str,
        replacement_sha256: &str,
        removed_destination_ids: &[&str],
        updated_unix_ms: i64,
    ) -> Result<ManagedWorkspace, Error> {
        validate_absolute_path("expected FolderSet path", expected_path)?;
        validate_digest("expected FolderSet digest", expected_sha256)?;
        validate_absolute_path("replacement FolderSet path", replacement_path)?;
        validate_digest("replacement FolderSet digest", replacement_sha256)?;
        let transaction = self.connection.transaction()?;
        let mut workspace = managed_workspace_for_update(&transaction, workspace_id)?;
        validate_workspace_update_time(&workspace, updated_unix_ms)?;
        if workspace.enabled {
            return Err(Error::InvalidState(
                "managed workspace must be disabled before editing its Library".into(),
            ));
        }
        if workspace.folder_set_path != expected_path
            || workspace.folder_set_sha256 != expected_sha256
        {
            return Err(Error::InvalidState(
                "managed workspace FolderSet binding changed after preview".into(),
            ));
        }
        let unfinished_managed: bool = transaction.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM managed_runs
                WHERE workspace_id = ?1 AND id != ?2
                  AND state IN ('planning', 'planned', 'applying', 'needs_resume')
             )",
            params![workspace_id, edit_run_id],
            |row| row.get(0),
        )?;
        let unfinished_monitoring: bool = transaction.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM monitor_runs
                WHERE monitor_id = ?1
                  AND state IN ('planning', 'planned', 'applying', 'needs_resume')
             )",
            [&workspace.monitor_id],
            |row| row.get(0),
        )?;
        if unfinished_managed || unfinished_monitoring {
            return Err(Error::InvalidState(
                "AI Library editing requires every managed run to be finished".into(),
            ));
        }
        for destination_id in removed_destination_ids {
            let referenced: bool = transaction.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM rules
                    WHERE monitor_id = ?1 AND destination_id = ?2
                      AND enabled = 1 AND deleted_unix_ms IS NULL
                 )",
                params![workspace.monitor_id, destination_id],
                |row| row.get(0),
            )?;
            if referenced {
                return Err(Error::InvalidState(
                    "a Library destination referenced by an active local rule cannot be deleted"
                        .into(),
                ));
            }
        }
        require_changed(
            transaction.execute(
                "UPDATE monitors
                 SET folder_set_path = ?2, folder_set_sha256 = ?3, updated_unix_ms = ?4
                 WHERE id = ?1 AND folder_set_path = ?5 AND folder_set_sha256 = ?6
                   AND deleted_unix_ms IS NULL",
                params![
                    workspace.monitor_id,
                    replacement_path,
                    replacement_sha256,
                    updated_unix_ms,
                    expected_path,
                    expected_sha256,
                ],
            )?,
            "matching monitor FolderSet binding",
            &workspace.monitor_id,
        )?;
        require_changed(
            transaction.execute(
                "UPDATE managed_workspaces
                 SET folder_set_path = ?2, folder_set_sha256 = ?3, updated_unix_ms = ?4
                 WHERE id = ?1 AND folder_set_path = ?5 AND folder_set_sha256 = ?6
                   AND enabled = 0",
                params![
                    workspace_id,
                    replacement_path,
                    replacement_sha256,
                    updated_unix_ms,
                    expected_path,
                    expected_sha256,
                ],
            )?,
            "matching managed workspace FolderSet binding",
            workspace_id,
        )?;
        transaction.commit()?;
        workspace.folder_set_path = replacement_path.into();
        workspace.folder_set_sha256 = replacement_sha256.into();
        workspace.updated_unix_ms = updated_unix_ms;
        Ok(workspace)
    }

    pub fn delete_managed_workspace(&mut self, id: &str) -> Result<(), Error> {
        require_changed(
            self.connection
                .execute("DELETE FROM managed_workspaces WHERE id = ?1", [id])?,
            "managed workspace",
            id,
        )
    }

    pub fn upsert_observation(
        &mut self,
        workspace_id: &str,
        fingerprint: &FileFingerprint,
        relative_path: &str,
        now_unix_ms: i64,
    ) -> Result<RecentsItem, Error> {
        validate_identifier("workspace ID", workspace_id)?;
        normalize_relative_path(relative_path)?;
        validate_digest("recents content digest", &fingerprint.sha256)?;
        let workspace = self.managed_workspace(workspace_id)?.ok_or_else(|| {
            Error::InvalidState(format!("unknown managed workspace {workspace_id:?}"))
        })?;
        let existing = self.recents_item(workspace_id, fingerprint.identity.clone())?;
        let changed = existing.as_ref().is_some_and(|item| {
            item.relative_path != relative_path
                || item.content_sha256 != fingerprint.sha256
                || item.size_bytes != fingerprint.size
        });
        let first_seen_unix_ms = existing
            .as_ref()
            .map_or(now_unix_ms, |item| item.first_seen_unix_ms);
        let stable_since_unix_ms = if changed {
            now_unix_ms
        } else {
            existing
                .as_ref()
                .map_or(now_unix_ms, |item| item.stable_since_unix_ms)
        };
        let retention_ms = duration_millis(workspace.retention_seconds, "workspace retention")?;
        let settle_ms = duration_millis(workspace.settle_seconds, "recents settle window")?;
        let eligible_unix_ms = first_seen_unix_ms
            .checked_add(retention_ms)
            .and_then(|retention_deadline| {
                stable_since_unix_ms
                    .checked_add(settle_ms)
                    .map(|settle_deadline| retention_deadline.max(settle_deadline))
            })
            .ok_or_else(|| Error::InvalidState("recents eligibility timestamp overflow".into()))?;
        let (state, last_run_id) = if changed {
            (RecentsState::Pending, None)
        } else {
            existing
                .map(|item| (item.state, item.last_run_id))
                .unwrap_or((RecentsState::Pending, None))
        };
        let item = RecentsItem {
            workspace_id: workspace_id.into(),
            file_identity: fingerprint.identity.clone(),
            relative_path: relative_path.into(),
            content_sha256: fingerprint.sha256.clone(),
            size_bytes: fingerprint.size,
            first_seen_unix_ms,
            stable_since_unix_ms,
            eligible_unix_ms,
            state,
            last_run_id,
        };
        validate_recents_item(&item)?;
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "DELETE FROM recents_items
             WHERE workspace_id = ?1 AND relative_path = ?2
               AND (file_device != ?3 OR file_inode != ?4)",
            params![
                item.workspace_id,
                item.relative_path,
                item.file_identity.device.to_string(),
                item.file_identity.inode.to_string(),
            ],
        )?;
        transaction.execute(
            "INSERT INTO recents_items (
                workspace_id, file_device, file_inode, relative_path, content_sha256,
                size_bytes, first_seen_unix_ms, stable_since_unix_ms, eligible_unix_ms,
                state, last_run_id
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(workspace_id, file_device, file_inode) DO UPDATE SET
                relative_path = excluded.relative_path,
                content_sha256 = excluded.content_sha256,
                size_bytes = excluded.size_bytes,
                first_seen_unix_ms = excluded.first_seen_unix_ms,
                stable_since_unix_ms = excluded.stable_since_unix_ms,
                eligible_unix_ms = excluded.eligible_unix_ms,
                state = excluded.state,
                last_run_id = excluded.last_run_id",
            params![
                item.workspace_id,
                item.file_identity.device.to_string(),
                item.file_identity.inode.to_string(),
                item.relative_path,
                item.content_sha256,
                to_i64(item.size_bytes, "recents file size")?,
                item.first_seen_unix_ms,
                item.stable_since_unix_ms,
                item.eligible_unix_ms,
                item.state.as_str(),
                item.last_run_id,
            ],
        )?;
        transaction.commit()?;
        Ok(item)
    }

    pub fn recents_item(
        &self,
        workspace_id: &str,
        file_identity: FsIdentity,
    ) -> Result<Option<RecentsItem>, Error> {
        Ok(self
            .connection
            .query_row(
                "SELECT workspace_id, file_device, file_inode, relative_path,
                        content_sha256, size_bytes, first_seen_unix_ms,
                        stable_since_unix_ms, eligible_unix_ms, state, last_run_id
                 FROM recents_items
                 WHERE workspace_id = ?1 AND file_device = ?2 AND file_inode = ?3",
                params![
                    workspace_id,
                    file_identity.device.to_string(),
                    file_identity.inode.to_string()
                ],
                recents_item_from_row,
            )
            .optional()?)
    }

    pub fn recents_items(&self, workspace_id: &str) -> Result<Vec<RecentsItem>, Error> {
        let mut statement = self.connection.prepare(
            "SELECT workspace_id, file_device, file_inode, relative_path,
                    content_sha256, size_bytes, first_seen_unix_ms,
                    stable_since_unix_ms, eligible_unix_ms, state, last_run_id
             FROM recents_items WHERE workspace_id = ?1
             ORDER BY first_seen_unix_ms, relative_path",
        )?;
        let rows = statement.query_map([workspace_id], recents_item_from_row)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn eligible_items(
        &self,
        workspace_id: &str,
        now_unix_ms: i64,
    ) -> Result<Vec<RecentsItem>, Error> {
        let mut statement = self.connection.prepare(
            "SELECT workspace_id, file_device, file_inode, relative_path,
                    content_sha256, size_bytes, first_seen_unix_ms,
                    stable_since_unix_ms, eligible_unix_ms, state, last_run_id
             FROM recents_items
             WHERE workspace_id = ?1 AND state = 'pending' AND eligible_unix_ms <= ?2
             ORDER BY eligible_unix_ms, relative_path",
        )?;
        let rows =
            statement.query_map(params![workspace_id, now_unix_ms], recents_item_from_row)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn set_recents_item_state(
        &mut self,
        workspace_id: &str,
        file_identity: FsIdentity,
        state: RecentsState,
        last_run_id: Option<&str>,
    ) -> Result<(), Error> {
        validate_recents_state(state, last_run_id)?;
        if let Some(run_id) = last_run_id {
            let belongs: bool = self.connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM managed_runs WHERE id = ?1 AND workspace_id = ?2)",
                params![run_id, workspace_id],
                |row| row.get(0),
            )?;
            if !belongs {
                return Err(Error::InvalidState(format!(
                    "managed run {run_id:?} does not belong to workspace {workspace_id:?}"
                )));
            }
        }
        require_changed(
            self.connection.execute(
                "UPDATE recents_items SET state = ?4, last_run_id = ?5
                 WHERE workspace_id = ?1 AND file_device = ?2 AND file_inode = ?3",
                params![
                    workspace_id,
                    file_identity.device.to_string(),
                    file_identity.inode.to_string(),
                    state.as_str(),
                    last_run_id,
                ],
            )?,
            "recents item",
            &format!(
                "{workspace_id}:{}:{}",
                file_identity.device, file_identity.inode
            ),
        )
    }

    pub fn delete_recents_item(
        &mut self,
        workspace_id: &str,
        file_identity: FsIdentity,
    ) -> Result<(), Error> {
        require_changed(
            self.connection.execute(
                "DELETE FROM recents_items
                 WHERE workspace_id = ?1 AND file_device = ?2 AND file_inode = ?3",
                params![
                    workspace_id,
                    file_identity.device.to_string(),
                    file_identity.inode.to_string()
                ],
            )?,
            "recents item",
            &format!(
                "{workspace_id}:{}:{}",
                file_identity.device, file_identity.inode
            ),
        )
    }

    pub fn reconcile_recents_index(
        &mut self,
        workspace_id: &str,
        observed_recents_identities: &[FsIdentity],
    ) -> Result<RecentsReconcileSummary, Error> {
        validate_identifier("workspace ID", workspace_id)?;
        let observed = observed_recents_identities
            .iter()
            .map(|identity| (identity.device, identity.inode))
            .collect::<HashSet<_>>();
        if observed.len() != observed_recents_identities.len() {
            return Err(Error::InvalidState(
                "Recents reconciliation contains duplicate filesystem identities".into(),
            ));
        }
        let transaction = self.connection.transaction()?;
        managed_workspace_for_update(&transaction, workspace_id)?;
        let indexed = {
            let mut statement = transaction.prepare(
                "SELECT i.file_device, i.file_inode, i.state, r.state
                 FROM recents_items AS i
                 LEFT JOIN managed_runs AS r ON r.id = i.last_run_id
                 WHERE i.workspace_id = ?1 ORDER BY i.relative_path",
            )?;
            let rows = statement.query_map([workspace_id], |row| {
                let device = parse_u64_column(row.get(0)?, 0)?;
                let inode = parse_u64_column(row.get(1)?, 1)?;
                let state = RecentsState::parse(&row.get::<_, String>(2)?)
                    .map_err(|error| conversion_error(2, error))?;
                let run_state = row
                    .get::<_, Option<String>>(3)?
                    .map(|value| {
                        RunState::parse(&value).map_err(|error| conversion_error(3, error))
                    })
                    .transpose()?;
                Ok((device, inode, state, run_state))
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let mut summary = RecentsReconcileSummary::default();
        for (device, inode, state, _) in &indexed {
            if *state == RecentsState::Pending && !observed.contains(&(*device, *inode)) {
                summary.deleted_stale_pending += transaction.execute(
                    "DELETE FROM recents_items
                     WHERE workspace_id = ?1 AND file_device = ?2 AND file_inode = ?3
                       AND state = 'pending'",
                    params![workspace_id, device.to_string(), inode.to_string()],
                )?;
            }
        }
        for (device, inode, state, run_state) in indexed {
            let returned = match state {
                RecentsState::Pending => false,
                RecentsState::Moved => true,
                RecentsState::Planned => !matches!(
                    run_state,
                    Some(
                        RunState::Planning
                            | RunState::Planned
                            | RunState::Applying
                            | RunState::NeedsResume
                    )
                ),
            };
            if returned && observed.contains(&(device, inode)) {
                summary.reset_returned += transaction.execute(
                    "UPDATE recents_items SET state = 'pending', last_run_id = NULL
                     WHERE workspace_id = ?1 AND file_device = ?2 AND file_inode = ?3
                       AND state != 'pending'",
                    params![workspace_id, device.to_string(), inode.to_string()],
                )?;
            }
        }
        transaction.commit()?;
        Ok(summary)
    }

    pub fn insert_managed_run(&mut self, run: &ManagedRun) -> Result<(), Error> {
        validate_managed_run(run)?;
        self.connection.execute(
            "INSERT INTO managed_runs (
                id, workspace_id, kind, state, plan_path, apply_path, undo_path,
                started_unix_ms, finished_unix_ms, move_count, error
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                run.id,
                run.workspace_id,
                run.kind.as_str(),
                run.state.as_str(),
                run.plan_path,
                run.apply_path,
                run.undo_path,
                run.started_unix_ms,
                run.finished_unix_ms,
                to_i64(run.move_count, "managed move count")?,
                run.error,
            ],
        )?;
        Ok(())
    }

    pub fn managed_run(&self, id: &str) -> Result<Option<ManagedRun>, Error> {
        Ok(self
            .connection
            .query_row(
                "SELECT id, workspace_id, kind, state, plan_path, apply_path,
                        undo_path, started_unix_ms, finished_unix_ms, move_count, error
                 FROM managed_runs WHERE id = ?1",
                [id],
                managed_run_from_row,
            )
            .optional()?)
    }

    pub fn managed_runs(&self, workspace_id: &str) -> Result<Vec<ManagedRun>, Error> {
        let mut statement = self.connection.prepare(
            "SELECT id, workspace_id, kind, state, plan_path, apply_path,
                    undo_path, started_unix_ms, finished_unix_ms, move_count, error
             FROM managed_runs WHERE workspace_id = ?1
             ORDER BY started_unix_ms DESC, id DESC",
        )?;
        let rows = statement.query_map([workspace_id], managed_run_from_row)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn update_managed_run(&mut self, run: &ManagedRun) -> Result<(), Error> {
        validate_managed_run(run)?;
        let current_workspace: Option<String> = self
            .connection
            .query_row(
                "SELECT workspace_id FROM managed_runs WHERE id = ?1",
                [&run.id],
                |row| row.get(0),
            )
            .optional()?;
        if current_workspace
            .as_deref()
            .is_some_and(|id| id != run.workspace_id)
        {
            return Err(Error::InvalidState(
                "managed run workspace cannot be changed".into(),
            ));
        }
        require_changed(
            self.connection.execute(
                "UPDATE managed_runs
                 SET kind = ?2, state = ?3, plan_path = ?4,
                     apply_path = ?5, undo_path = ?6, started_unix_ms = ?7,
                     finished_unix_ms = ?8, move_count = ?9, error = ?10
                 WHERE id = ?1",
                params![
                    run.id,
                    run.kind.as_str(),
                    run.state.as_str(),
                    run.plan_path,
                    run.apply_path,
                    run.undo_path,
                    run.started_unix_ms,
                    run.finished_unix_ms,
                    to_i64(run.move_count, "managed move count")?,
                    run.error,
                ],
            )?,
            "managed run",
            &run.id,
        )
    }

    pub fn delete_managed_run(&mut self, id: &str) -> Result<(), Error> {
        let referenced: bool = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM recents_items WHERE last_run_id = ?1)",
            [id],
            |row| row.get(0),
        )?;
        if referenced {
            return Err(Error::InvalidState(format!(
                "managed run {id:?} is still referenced by recents items"
            )));
        }
        require_changed(
            self.connection
                .execute("DELETE FROM managed_runs WHERE id = ?1", [id])?,
            "managed run",
            id,
        )
    }

    pub fn recent_managed_moves(
        &self,
        workspace_id: &str,
        limit: u32,
    ) -> Result<Vec<ManagedRun>, Error> {
        if limit == 0 {
            return Err(Error::InvalidState(
                "managed move history limit must be positive".into(),
            ));
        }
        let mut statement = self.connection.prepare(
            "SELECT id, workspace_id, kind, state, plan_path, apply_path,
                    undo_path, started_unix_ms, finished_unix_ms, move_count, error
             FROM managed_runs
             WHERE workspace_id = ?1 AND kind != 'setup'
               AND state = 'completed' AND move_count > 0
             ORDER BY finished_unix_ms DESC, id DESC LIMIT ?2",
        )?;
        let rows = statement.query_map(
            params![workspace_id, i64::from(limit)],
            managed_run_from_row,
        )?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn record_managed_undo_journal(
        &mut self,
        run_id: &str,
        path: &str,
        created_unix_ms: i64,
    ) -> Result<(), Error> {
        validate_absolute_path("managed Undo journal path", path)?;
        let run = self
            .managed_run(run_id)?
            .ok_or_else(|| Error::InvalidState(format!("unknown managed run {run_id:?}")))?;
        if matches!(
            run.kind,
            ManagedRunKind::Setup | ManagedRunKind::Adopt | ManagedRunKind::Configure
        ) || run.state != RunState::Completed
        {
            return Err(Error::InvalidState(
                "managed Undo journals require a completed file-move run".into(),
            ));
        }
        self.connection.execute(
            "INSERT INTO managed_undo_journals (run_id, path, created_unix_ms)
             VALUES (?1, ?2, ?3)",
            params![run_id, path, created_unix_ms],
        )?;
        Ok(())
    }

    /// Atomically records an Undo journal and reconciles state for restored files.
    pub fn finalize_managed_undo(
        &mut self,
        run_id: &str,
        path: &str,
        restored: &[FsIdentity],
        created_unix_ms: i64,
    ) -> Result<(), Error> {
        validate_identifier("managed run ID", run_id)?;
        validate_absolute_path("managed Undo journal path", path)?;
        let identities = restored
            .iter()
            .map(|identity| (identity.device, identity.inode))
            .collect::<HashSet<_>>();
        if identities.len() != restored.len() {
            return Err(Error::InvalidState(
                "managed Undo contains duplicate restored filesystem identities".into(),
            ));
        }

        let transaction = self.connection.transaction()?;
        let (workspace_id, monitor_id, kind, state) = transaction
            .query_row(
                "SELECT r.workspace_id, w.monitor_id, r.kind, r.state
                 FROM managed_runs AS r
                 JOIN managed_workspaces AS w ON w.id = r.workspace_id
                 WHERE r.id = ?1",
                [run_id],
                |row| {
                    let kind = ManagedRunKind::parse(&row.get::<_, String>(2)?)
                        .map_err(|error| conversion_error(2, error))?;
                    let state = RunState::parse(&row.get::<_, String>(3)?)
                        .map_err(|error| conversion_error(3, error))?;
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        kind,
                        state,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| Error::InvalidState(format!("unknown managed run {run_id:?}")))?;
        let valid_state = match kind {
            ManagedRunKind::Reorganize => state == RunState::NeedsResume,
            ManagedRunKind::Stage | ManagedRunKind::Classify => state == RunState::Completed,
            ManagedRunKind::Setup | ManagedRunKind::Adopt | ManagedRunKind::Configure => false,
        };
        if !valid_state {
            return Err(Error::InvalidState(
                "managed Undo journals require a completed file-move run".into(),
            ));
        }

        transaction.execute(
            "INSERT INTO managed_undo_journals (run_id, path, created_unix_ms)
             VALUES (?1, ?2, ?3)",
            params![run_id, path, created_unix_ms],
        )?;
        require_changed(
            transaction.execute(
                "UPDATE managed_runs SET undo_path = ?2 WHERE id = ?1",
                params![run_id, path],
            )?,
            "managed run",
            run_id,
        )?;
        if kind == ManagedRunKind::Reorganize {
            require_changed(
                transaction.execute(
                    "UPDATE managed_runs
                     SET state = 'completed', finished_unix_ms = ?2, error = NULL
                     WHERE id = ?1 AND state = 'needs_resume'",
                    params![run_id, created_unix_ms],
                )?,
                "recoverable reorganization run",
                run_id,
            )?;
        }
        for identity in restored {
            match kind {
                ManagedRunKind::Stage | ManagedRunKind::Reorganize => {
                    transaction.execute(
                        "DELETE FROM recents_items
                         WHERE workspace_id = ?1 AND file_device = ?2 AND file_inode = ?3",
                        params![
                            workspace_id,
                            identity.device.to_string(),
                            identity.inode.to_string()
                        ],
                    )?;
                }
                ManagedRunKind::Classify => {
                    require_changed(
                        transaction.execute(
                            "UPDATE recents_items SET state = 'pending', last_run_id = NULL
                             WHERE workspace_id = ?1 AND file_device = ?2 AND file_inode = ?3",
                            params![
                                workspace_id,
                                identity.device.to_string(),
                                identity.inode.to_string()
                            ],
                        )?,
                        "recents item",
                        &format!("{workspace_id}:{}:{}", identity.device, identity.inode),
                    )?;
                    transaction.execute(
                        "DELETE FROM processed_files
                         WHERE monitor_id = ?1 AND file_device = ?2 AND file_inode = ?3",
                        params![
                            monitor_id,
                            identity.device.to_string(),
                            identity.inode.to_string()
                        ],
                    )?;
                }
                ManagedRunKind::Setup | ManagedRunKind::Adopt | ManagedRunKind::Configure => {
                    unreachable!()
                }
            }
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn managed_undo_journal_paths(&self, run_id: &str) -> Result<Vec<String>, Error> {
        let mut statement = self.connection.prepare(
            "SELECT path FROM managed_undo_journals
             WHERE run_id = ?1 ORDER BY id",
        )?;
        let rows = statement.query_map([run_id], |row| row.get(0))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
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

    pub fn finish_noop(
        &mut self,
        run_id: &str,
        total_files: u64,
        finished_unix_ms: i64,
    ) -> Result<(), Error> {
        require_changed(
            self.connection.execute(
                "UPDATE monitor_runs
                 SET state = 'noop', finished_unix_ms = ?2,
                     total_files = ?3, error = NULL
                 WHERE id = ?1 AND state = 'planning'",
                params![
                    run_id,
                    finished_unix_ms,
                    to_i64(total_files, "total files")?
                ],
            )?,
            "planning run",
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

    /// Returns whether this exact filesystem identity has already completed
    /// classification for a monitor, regardless of its current path.
    pub fn has_processed_identity(
        &self,
        monitor_id: &str,
        file_identity: FsIdentity,
    ) -> Result<bool, Error> {
        validate_identifier("monitor ID", monitor_id)?;
        Ok(self.connection.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM processed_files
                WHERE monitor_id = ?1 AND file_device = ?2 AND file_inode = ?3
             )",
            params![
                monitor_id,
                file_identity.device.to_string(),
                file_identity.inode.to_string(),
            ],
            |row| row.get(0),
        )?)
    }

    pub fn processed_files(&self, monitor_id: &str) -> Result<Vec<ProcessedFileRecord>, Error> {
        validate_identifier("monitor ID", monitor_id)?;
        let mut statement = self.connection.prepare(
            "SELECT monitor_id, file_device, file_inode, relative_path,
                    content_sha256, size_bytes, processing_signature, run_id,
                    classification_basis, rule_id, destination_id, processed_unix_ms
             FROM processed_files WHERE monitor_id = ?1
             ORDER BY file_device, file_inode",
        )?;
        let rows = statement.query_map([monitor_id], processed_file_from_row)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Forgets the processed marker for exactly one filesystem identity.
    ///
    /// A missing marker is accepted so recovery and repeated Undo reconciliation
    /// remain idempotent. The monitor itself must still exist.
    pub fn forget_processed_file(
        &mut self,
        monitor_id: &str,
        file_identity: FsIdentity,
    ) -> Result<(), Error> {
        validate_identifier("monitor ID", monitor_id)?;
        let monitor_exists: bool = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM monitors WHERE id = ?1)",
            [monitor_id],
            |row| row.get(0),
        )?;
        if !monitor_exists {
            return Err(Error::InvalidState(format!(
                "no matching monitor found for ID {monitor_id:?}"
            )));
        }
        self.connection.execute(
            "DELETE FROM processed_files
             WHERE monitor_id = ?1 AND file_device = ?2 AND file_inode = ?3",
            params![
                monitor_id,
                file_identity.device.to_string(),
                file_identity.inode.to_string()
            ],
        )?;
        Ok(())
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

impl RecentsState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Planned => "planned",
            Self::Moved => "moved",
        }
    }

    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "pending" => Ok(Self::Pending),
            "planned" => Ok(Self::Planned),
            "moved" => Ok(Self::Moved),
            other => Err(format!("unknown recents state {other:?}")),
        }
    }
}

impl ManagedRunKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Setup => "setup",
            Self::Adopt => "adopt",
            Self::Stage => "stage",
            Self::Classify => "classify",
            Self::Configure => "configure",
            Self::Reorganize => "reorganize",
        }
    }

    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "setup" => Ok(Self::Setup),
            "adopt" => Ok(Self::Adopt),
            "stage" => Ok(Self::Stage),
            "classify" => Ok(Self::Classify),
            "configure" => Ok(Self::Configure),
            "reorganize" => Ok(Self::Reorganize),
            other => Err(format!("unknown managed run kind {other:?}")),
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

fn initialize_schema(connection: &mut Connection) -> Result<(), Error> {
    let has_metadata: bool = connection.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM sqlite_master
            WHERE type = 'table' AND name = 'schema_metadata'
         )",
        [],
        |row| row.get(0),
    )?;
    if has_metadata {
        let version = connection
            .query_row(
                "SELECT version FROM schema_metadata WHERE id = 1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .ok_or_else(|| {
                Error::InvalidState("state database schema metadata is missing".into())
            })?;
        if version != SCHEMA_VERSION {
            return Err(Error::InvalidState(format!(
                "unsupported state database schema version {version}; expected {SCHEMA_VERSION}"
            )));
        }
    } else {
        let existing_tables: i64 = connection.query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
            [],
            |row| row.get(0),
        )?;
        if existing_tables != 0 {
            return Err(Error::InvalidState(
                "unsupported or malformed state database schema".into(),
            ));
        }
    }
    let transaction = connection.transaction()?;
    transaction.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_metadata (
            id INTEGER PRIMARY KEY CHECK(id = 1),
            version INTEGER NOT NULL
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
        );
        CREATE TABLE IF NOT EXISTS managed_workspaces (
            id TEXT PRIMARY KEY,
            monitor_id TEXT NOT NULL UNIQUE REFERENCES monitors(id),
            source TEXT NOT NULL UNIQUE,
            source_device TEXT NOT NULL,
            source_inode TEXT NOT NULL,
            folder_set_path TEXT NOT NULL,
            folder_set_sha256 TEXT NOT NULL,
            config_path TEXT NOT NULL CHECK(config_path != ''),
            retention_seconds INTEGER NOT NULL CHECK(retention_seconds > 0),
            settle_seconds INTEGER NOT NULL CHECK(settle_seconds > 0),
            enabled INTEGER NOT NULL CHECK(enabled IN (0, 1)),
            setup_session_path TEXT,
            created_unix_ms INTEGER NOT NULL,
            updated_unix_ms INTEGER NOT NULL,
            UNIQUE(source_device, source_inode),
            CHECK(updated_unix_ms >= created_unix_ms)
        );
        CREATE TABLE IF NOT EXISTS managed_runs (
            id TEXT PRIMARY KEY,
            workspace_id TEXT NOT NULL REFERENCES managed_workspaces(id) ON DELETE CASCADE,
            kind TEXT NOT NULL CHECK(kind IN (
                'setup', 'adopt', 'stage', 'classify', 'configure', 'reorganize'
            )),
            state TEXT NOT NULL CHECK(state IN (
                'planning', 'planned', 'applying', 'completed',
                'noop', 'failed', 'needs_resume'
            )),
            plan_path TEXT,
            apply_path TEXT,
            undo_path TEXT,
            started_unix_ms INTEGER NOT NULL,
            finished_unix_ms INTEGER,
            move_count INTEGER NOT NULL DEFAULT 0 CHECK(move_count >= 0),
            error TEXT,
            CHECK(finished_unix_ms IS NULL OR finished_unix_ms >= started_unix_ms)
        );
        CREATE INDEX IF NOT EXISTS managed_runs_by_workspace_time
            ON managed_runs(workspace_id, started_unix_ms DESC, id DESC);
        CREATE TABLE IF NOT EXISTS managed_undo_journals (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            run_id TEXT NOT NULL REFERENCES managed_runs(id) ON DELETE CASCADE,
            path TEXT NOT NULL UNIQUE,
            created_unix_ms INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS managed_undo_journals_by_run_time
            ON managed_undo_journals(run_id, id);
        CREATE TABLE IF NOT EXISTS recents_items (
            workspace_id TEXT NOT NULL REFERENCES managed_workspaces(id) ON DELETE CASCADE,
            file_device TEXT NOT NULL,
            file_inode TEXT NOT NULL,
            relative_path TEXT NOT NULL,
            content_sha256 TEXT NOT NULL,
            size_bytes INTEGER NOT NULL CHECK(size_bytes >= 0),
            first_seen_unix_ms INTEGER NOT NULL,
            stable_since_unix_ms INTEGER NOT NULL,
            eligible_unix_ms INTEGER NOT NULL,
            state TEXT NOT NULL CHECK(state IN ('pending', 'planned', 'moved')),
            last_run_id TEXT,
            PRIMARY KEY(workspace_id, file_device, file_inode),
            UNIQUE(workspace_id, relative_path),
            CHECK(stable_since_unix_ms >= first_seen_unix_ms),
            CHECK(eligible_unix_ms >= first_seen_unix_ms),
            CHECK((state = 'pending' AND last_run_id IS NULL) OR
                  (state != 'pending' AND last_run_id IS NOT NULL))
        );
        CREATE INDEX IF NOT EXISTS recents_items_by_eligibility
            ON recents_items(workspace_id, state, eligible_unix_ms, relative_path);",
    )?;
    transaction.execute(
        "INSERT OR IGNORE INTO schema_metadata (id, version) VALUES (1, ?1)",
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

fn managed_workspace_from_row(row: &Row<'_>) -> rusqlite::Result<ManagedWorkspace> {
    let device: String = row.get(3)?;
    let inode: String = row.get(4)?;
    Ok(ManagedWorkspace {
        id: row.get(0)?,
        monitor_id: row.get(1)?,
        source: row.get(2)?,
        source_identity: FsIdentity {
            device: parse_u64_column(device, 3)?,
            inode: parse_u64_column(inode, 4)?,
        },
        folder_set_path: row.get(5)?,
        folder_set_sha256: row.get(6)?,
        config_path: row.get(7)?,
        retention_seconds: i64_to_u64(row.get(8)?, 8)?,
        settle_seconds: i64_to_u64(row.get(9)?, 9)?,
        enabled: row.get(10)?,
        setup_session_path: row.get(11)?,
        created_unix_ms: row.get(12)?,
        updated_unix_ms: row.get(13)?,
    })
}

fn managed_workspace_for_update(
    transaction: &Transaction<'_>,
    id: &str,
) -> Result<ManagedWorkspace, Error> {
    validate_identifier("workspace ID", id)?;
    let workspace = transaction
        .query_row(
            "SELECT id, monitor_id, source, source_device, source_inode, folder_set_path,
                    folder_set_sha256, config_path, retention_seconds, settle_seconds, enabled,
                    setup_session_path, created_unix_ms, updated_unix_ms
             FROM managed_workspaces WHERE id = ?1",
            [id],
            managed_workspace_from_row,
        )
        .optional()?
        .ok_or_else(|| Error::InvalidState(format!("unknown managed workspace {id:?}")))?;
    validate_workspace_monitor(transaction, &workspace)?;
    Ok(workspace)
}

fn validate_workspace_update_time(
    workspace: &ManagedWorkspace,
    updated_unix_ms: i64,
) -> Result<(), Error> {
    if updated_unix_ms < workspace.updated_unix_ms {
        return Err(Error::InvalidState(
            "managed workspace update time must not move backwards".into(),
        ));
    }
    Ok(())
}

fn recents_item_from_row(row: &Row<'_>) -> rusqlite::Result<RecentsItem> {
    let device: String = row.get(1)?;
    let inode: String = row.get(2)?;
    let state: String = row.get(9)?;
    Ok(RecentsItem {
        workspace_id: row.get(0)?,
        file_identity: FsIdentity {
            device: parse_u64_column(device, 1)?,
            inode: parse_u64_column(inode, 2)?,
        },
        relative_path: row.get(3)?,
        content_sha256: row.get(4)?,
        size_bytes: i64_to_u64(row.get(5)?, 5)?,
        first_seen_unix_ms: row.get(6)?,
        stable_since_unix_ms: row.get(7)?,
        eligible_unix_ms: row.get(8)?,
        state: RecentsState::parse(&state).map_err(|error| conversion_error(9, error))?,
        last_run_id: row.get(10)?,
    })
}

fn managed_run_from_row(row: &Row<'_>) -> rusqlite::Result<ManagedRun> {
    let kind: String = row.get(2)?;
    let state: String = row.get(3)?;
    Ok(ManagedRun {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        kind: ManagedRunKind::parse(&kind).map_err(|error| conversion_error(2, error))?,
        state: RunState::parse(&state).map_err(|error| conversion_error(3, error))?,
        plan_path: row.get(4)?,
        apply_path: row.get(5)?,
        undo_path: row.get(6)?,
        started_unix_ms: row.get(7)?,
        finished_unix_ms: row.get(8)?,
        move_count: i64_to_u64(row.get(9)?, 9)?,
        error: row.get(10)?,
    })
}

fn processed_file_from_row(row: &Row<'_>) -> rusqlite::Result<ProcessedFileRecord> {
    let device: String = row.get(1)?;
    let inode: String = row.get(2)?;
    let basis: String = row.get(8)?;
    Ok(ProcessedFileRecord {
        monitor_id: row.get(0)?,
        file_identity: FsIdentity {
            device: parse_u64_column(device, 1)?,
            inode: parse_u64_column(inode, 2)?,
        },
        relative_path: row.get(3)?,
        content_sha256: row.get(4)?,
        size_bytes: i64_to_u64(row.get(5)?, 5)?,
        processing_signature: row.get(6)?,
        run_id: row.get(7)?,
        classification_basis: ClassificationBasis::parse(&basis)
            .map_err(|error| conversion_error(8, error))?,
        rule_id: row.get(9)?,
        destination_id: row.get(10)?,
        processed_unix_ms: row.get(11)?,
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

fn validate_managed_workspace(workspace: &ManagedWorkspace) -> Result<(), Error> {
    validate_identifier("workspace ID", &workspace.id)?;
    validate_identifier("workspace monitor ID", &workspace.monitor_id)?;
    validate_absolute_path("workspace source", &workspace.source)?;
    validate_absolute_path("workspace folder-set path", &workspace.folder_set_path)?;
    validate_absolute_path("workspace model configuration path", &workspace.config_path)?;
    validate_digest("workspace folder-set digest", &workspace.folder_set_sha256)?;
    if workspace.retention_seconds == 0 {
        return Err(Error::InvalidState(
            "workspace retention must be greater than zero".into(),
        ));
    }
    if workspace.settle_seconds == 0 {
        return Err(Error::InvalidState(
            "workspace settle window must be greater than zero".into(),
        ));
    }
    if let Some(path) = &workspace.setup_session_path {
        validate_absolute_path("workspace setup-session path", path)?;
    }
    if workspace.updated_unix_ms < workspace.created_unix_ms {
        return Err(Error::InvalidState(
            "workspace update time must not precede its creation time".into(),
        ));
    }
    Ok(())
}

fn validate_workspace_monitor(
    connection: &Connection,
    workspace: &ManagedWorkspace,
) -> Result<(), Error> {
    let monitor = connection
        .query_row(
            "SELECT id, source, source_device, source_inode, folder_set_path,
                    folder_set_sha256, interval_seconds, enabled, last_checked_unix_ms,
                    created_unix_ms, updated_unix_ms, deleted_unix_ms
             FROM monitors WHERE id = ?1 AND deleted_unix_ms IS NULL",
            [&workspace.monitor_id],
            monitor_from_row,
        )
        .optional()?
        .ok_or_else(|| {
            Error::InvalidState(format!("unknown active monitor {:?}", workspace.monitor_id))
        })?;
    if monitor.source != workspace.source
        || monitor.source_identity != workspace.source_identity
        || monitor.folder_set_path != workspace.folder_set_path
        || monitor.folder_set_sha256 != workspace.folder_set_sha256
    {
        return Err(Error::InvalidState(
            "managed workspace must match its active monitor source and folder set".into(),
        ));
    }
    Ok(())
}

fn validate_recents_item(item: &RecentsItem) -> Result<(), Error> {
    validate_identifier("recents workspace ID", &item.workspace_id)?;
    normalize_relative_path(&item.relative_path)?;
    validate_digest("recents content digest", &item.content_sha256)?;
    if item.stable_since_unix_ms < item.first_seen_unix_ms
        || item.eligible_unix_ms < item.first_seen_unix_ms
    {
        return Err(Error::InvalidState(
            "recents stability and eligibility must not precede first observation".into(),
        ));
    }
    validate_recents_state(item.state, item.last_run_id.as_deref())
}

fn validate_recents_state(state: RecentsState, last_run_id: Option<&str>) -> Result<(), Error> {
    match (state, last_run_id) {
        (RecentsState::Pending, None) => Ok(()),
        (RecentsState::Pending, Some(_)) => Err(Error::InvalidState(
            "a pending recents item must not reference a run".into(),
        )),
        (_, Some(run_id)) => validate_identifier("recents run ID", run_id),
        (_, None) => Err(Error::InvalidState(
            "a planned or moved recents item must reference a run".into(),
        )),
    }
}

fn validate_managed_run(run: &ManagedRun) -> Result<(), Error> {
    validate_identifier("managed run ID", &run.id)?;
    validate_identifier("managed run workspace ID", &run.workspace_id)?;
    for (name, path) in [
        ("managed Plan path", run.plan_path.as_deref()),
        ("managed Apply path", run.apply_path.as_deref()),
        ("managed Undo path", run.undo_path.as_deref()),
    ] {
        if let Some(path) = path {
            validate_absolute_path(name, path)?;
        }
    }
    let terminal = matches!(
        run.state,
        RunState::Completed | RunState::Noop | RunState::Failed | RunState::NeedsResume
    );
    if terminal != run.finished_unix_ms.is_some() {
        return Err(Error::InvalidState(
            "managed run completion time must match whether its state is terminal".into(),
        ));
    }
    if run
        .finished_unix_ms
        .is_some_and(|finished| finished < run.started_unix_ms)
    {
        return Err(Error::InvalidState(
            "managed run completion time must not precede its start".into(),
        ));
    }
    match (run.state, run.error.as_deref()) {
        (RunState::Failed | RunState::NeedsResume, Some(error)) => {
            validate_identifier("managed run error", error)
        }
        (RunState::Failed | RunState::NeedsResume, None) => Err(Error::InvalidState(
            "failed or resumable managed run must record an error".into(),
        )),
        (_, Some(_)) => Err(Error::InvalidState(
            "only failed or resumable managed runs may record an error".into(),
        )),
        (_, None) => Ok(()),
    }
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

fn duration_millis(seconds: u64, name: &str) -> Result<i64, Error> {
    seconds
        .checked_mul(1000)
        .ok_or_else(|| Error::InvalidState(format!("{name} exceeds timestamp range")))
        .and_then(|millis| to_i64(millis, name))
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

    fn workspace(root: &Path) -> ManagedWorkspace {
        ManagedWorkspace {
            id: "w1".into(),
            monitor_id: "m1".into(),
            source: root.join("source").display().to_string(),
            source_identity: FsIdentity {
                device: 10,
                inode: 20,
            },
            folder_set_path: root.join("folders.json").display().to_string(),
            folder_set_sha256: DIGEST_A.into(),
            config_path: root.join("config.toml").display().to_string(),
            retention_seconds: 100,
            settle_seconds: 30,
            enabled: true,
            setup_session_path: None,
            created_unix_ms: 100,
            updated_unix_ms: 100,
        }
    }

    fn setup_workspace(store: &mut StateStore, root: &Path) {
        setup_monitor(store, root);
        store.insert_managed_workspace(&workspace(root)).unwrap();
    }

    #[test]
    fn initializes_schema_idempotently_and_enforces_foreign_keys() {
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
                .query_row("SELECT COUNT(*) FROM schema_metadata", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }

    #[test]
    fn rejects_an_older_database_schema() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE schema_migrations (
                version INTEGER PRIMARY KEY,
                applied_unix_ms INTEGER NOT NULL
             );
             INSERT INTO schema_migrations VALUES (6, 0);",
            )
            .unwrap();

        let error = initialize_schema(&mut connection).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unsupported or malformed state database schema")
        );
    }

    #[test]
    fn rejects_a_malformed_database_without_schema_metadata() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection
            .execute("CREATE TABLE unrelated (id INTEGER)", [])
            .unwrap();

        let error = initialize_schema(&mut connection).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unsupported or malformed state database schema")
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
    fn noop_run_preserves_the_scanned_file_count() {
        let root = tempdir().unwrap();
        let mut store = StateStore::open_in_memory().unwrap();
        setup_monitor(&mut store, root.path());
        store.start_run("run1", "m1", 100).unwrap();

        store.finish_noop("run1", 3, 200).unwrap();

        let run = store.run("run1").unwrap().unwrap();
        assert_eq!(run.state, RunState::Noop);
        assert_eq!(run.total_files, 3);
        assert_eq!(run.finished_unix_ms, Some(200));
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
    fn forgets_only_the_exact_processed_identity_idempotently() {
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
        let identity = FsIdentity {
            device: 10,
            inode: 20,
        };
        store
            .complete_run(
                "run1",
                "apply1",
                200,
                &[ProcessedFileRecord {
                    monitor_id: "m1".into(),
                    file_identity: identity.clone(),
                    relative_path: "report.pdf".into(),
                    content_sha256: DIGEST_A.into(),
                    size_bytes: 42,
                    processing_signature: DIGEST_B.into(),
                    run_id: "run1".into(),
                    classification_basis: ClassificationBasis::Rule,
                    rule_id: Some("r1".into()),
                    destination_id: "d1".into(),
                    processed_unix_ms: 200,
                }],
            )
            .unwrap();

        store
            .forget_processed_file(
                "m1",
                FsIdentity {
                    device: 10,
                    inode: 21,
                },
            )
            .unwrap();
        assert_eq!(
            store
                .connection()
                .query_row("SELECT COUNT(*) FROM processed_files", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );

        store.forget_processed_file("m1", identity.clone()).unwrap();
        store.forget_processed_file("m1", identity).unwrap();
        assert_eq!(
            store
                .connection()
                .query_row("SELECT COUNT(*) FROM processed_files", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert!(
            store
                .forget_processed_file(
                    "missing",
                    FsIdentity {
                        device: 10,
                        inode: 20
                    }
                )
                .is_err()
        );
        assert!(
            store
                .forget_processed_file(
                    "\n",
                    FsIdentity {
                        device: 10,
                        inode: 20
                    }
                )
                .is_err()
        );
    }

    #[test]
    fn manages_workspace_records_bound_to_matching_monitors() {
        let root = tempdir().unwrap();
        let mut store = StateStore::open_in_memory().unwrap();
        setup_monitor(&mut store, root.path());
        let mut record = workspace(root.path());
        store.insert_managed_workspace(&record).unwrap();
        assert_eq!(store.managed_workspace("w1").unwrap(), Some(record.clone()));
        assert_eq!(store.managed_workspaces().unwrap(), [record.clone()]);

        record.retention_seconds = 200;
        record.enabled = false;
        record.updated_unix_ms = 200;
        store.update_managed_workspace(&record).unwrap();
        assert_eq!(store.managed_workspace("w1").unwrap(), Some(record));

        let mut mismatch = workspace(root.path());
        mismatch.id = "w2".into();
        mismatch.folder_set_sha256 = DIGEST_B.into();
        assert!(store.insert_managed_workspace(&mismatch).is_err());

        store.delete_managed_workspace("w1").unwrap();
        assert!(store.managed_workspaces().unwrap().is_empty());
    }

    #[test]
    fn workspace_enablement_is_atomic_with_its_monitor() {
        let root = tempdir().unwrap();
        let mut store = StateStore::open_in_memory().unwrap();
        setup_workspace(&mut store, root.path());

        let disabled = store
            .set_managed_workspace_enabled("w1", false, 200)
            .unwrap();
        assert!(!disabled.enabled);
        assert!(!store.monitor("m1").unwrap().unwrap().enabled);

        store
            .connection()
            .execute_batch(
                "CREATE TEMP TRIGGER fail_workspace_enable
                 BEFORE UPDATE ON managed_workspaces
                 WHEN NEW.enabled = 1
                 BEGIN SELECT RAISE(ABORT, 'forced failure'); END;",
            )
            .unwrap();
        assert!(
            store
                .set_managed_workspace_enabled("w1", true, 300)
                .is_err()
        );
        assert!(!store.managed_workspace("w1").unwrap().unwrap().enabled);
        assert!(!store.monitor("m1").unwrap().unwrap().enabled);
    }

    #[test]
    fn updating_windows_recalculates_only_pending_items_and_rolls_back() {
        let root = tempdir().unwrap();
        let mut store = StateStore::open_in_memory().unwrap();
        setup_workspace(&mut store, root.path());
        let pending_identity = FsIdentity {
            device: 1,
            inode: 1,
        };
        let moved_identity = FsIdentity {
            device: 1,
            inode: 2,
        };
        for (identity, path) in [
            (pending_identity.clone(), "Recents/pending.txt"),
            (moved_identity.clone(), "Recents/moved.txt"),
        ] {
            store
                .upsert_observation(
                    "w1",
                    &FileFingerprint {
                        identity,
                        size: 1,
                        sha256: DIGEST_A.into(),
                    },
                    path,
                    1_000,
                )
                .unwrap();
        }
        store
            .insert_managed_run(&ManagedRun {
                id: "run1".into(),
                workspace_id: "w1".into(),
                kind: ManagedRunKind::Classify,
                state: RunState::Completed,
                plan_path: Some(root.path().join("plan.json").display().to_string()),
                apply_path: Some(root.path().join("apply.json").display().to_string()),
                undo_path: None,
                started_unix_ms: 1_000,
                finished_unix_ms: Some(2_000),
                move_count: 1,
                error: None,
            })
            .unwrap();
        store
            .set_recents_item_state(
                "w1",
                moved_identity.clone(),
                RecentsState::Moved,
                Some("run1"),
            )
            .unwrap();

        let updated = store
            .update_managed_workspace_windows("w1", 10, 5, 200)
            .unwrap();
        assert_eq!((updated.retention_seconds, updated.settle_seconds), (10, 5));
        assert_eq!(
            store
                .recents_item("w1", pending_identity.clone())
                .unwrap()
                .unwrap()
                .eligible_unix_ms,
            11_000
        );
        assert_eq!(
            store
                .recents_item("w1", moved_identity)
                .unwrap()
                .unwrap()
                .eligible_unix_ms,
            101_000
        );

        store
            .connection()
            .execute_batch(
                "CREATE TEMP TRIGGER fail_pending_deadline_update
                 BEFORE UPDATE OF eligible_unix_ms ON recents_items
                 BEGIN SELECT RAISE(ABORT, 'forced failure'); END;",
            )
            .unwrap();
        assert!(
            store
                .update_managed_workspace_windows("w1", 20, 10, 300)
                .is_err()
        );
        let unchanged = store.managed_workspace("w1").unwrap().unwrap();
        assert_eq!(
            (unchanged.retention_seconds, unchanged.settle_seconds),
            (10, 5)
        );
        assert_eq!(
            store
                .recents_item("w1", pending_identity)
                .unwrap()
                .unwrap()
                .eligible_unix_ms,
            11_000
        );
    }

    #[test]
    fn removing_registration_requires_disabled_idle_state_and_is_atomic() {
        let root = tempdir().unwrap();
        let mut store = StateStore::open_in_memory().unwrap();
        setup_workspace(&mut store, root.path());
        store
            .insert_rule(
                &LocalRule {
                    id: "rule1".into(),
                    monitor_id: "m1".into(),
                    name_glob: "*.txt".into(),
                    destination_id: "d1".into(),
                    priority: 50,
                    enabled: true,
                },
                100,
            )
            .unwrap();
        store
            .set_managed_workspace_enabled("w1", false, 200)
            .unwrap();
        store
            .insert_managed_run(&ManagedRun {
                id: "run1".into(),
                workspace_id: "w1".into(),
                kind: ManagedRunKind::Stage,
                state: RunState::Planned,
                plan_path: Some(root.path().join("plan.json").display().to_string()),
                apply_path: None,
                undo_path: None,
                started_unix_ms: 200,
                finished_unix_ms: None,
                move_count: 1,
                error: None,
            })
            .unwrap();

        assert!(
            store
                .remove_managed_workspace_registration("w1", 300)
                .is_err()
        );
        assert!(store.managed_workspace("w1").unwrap().is_some());
        assert!(
            store
                .monitor("m1")
                .unwrap()
                .unwrap()
                .deleted_unix_ms
                .is_none()
        );
        assert!(store.rule("rule1").unwrap().is_some());

        store.delete_managed_run("run1").unwrap();
        store
            .remove_managed_workspace_registration("w1", 300)
            .unwrap();
        assert!(store.managed_workspace("w1").unwrap().is_none());
        let monitor = store.monitor("m1").unwrap().unwrap();
        assert!(!monitor.enabled);
        assert_eq!(monitor.deleted_unix_ms, Some(300));
        assert!(store.rule("rule1").unwrap().is_none());
    }

    #[test]
    fn recents_reconcile_deletes_stale_pending_and_resets_returned_items() {
        let root = tempdir().unwrap();
        let mut store = StateStore::open_in_memory().unwrap();
        setup_workspace(&mut store, root.path());
        let identities = (1..=5)
            .map(|inode| FsIdentity { device: 1, inode })
            .collect::<Vec<_>>();
        for (identity, path) in identities.iter().zip([
            "Recents/stale.txt",
            "Recents/pending.txt",
            "Recents/moved.txt",
            "Recents/planned.txt",
            "Recents/live-planned.txt",
        ]) {
            store
                .upsert_observation(
                    "w1",
                    &FileFingerprint {
                        identity: identity.clone(),
                        size: 1,
                        sha256: DIGEST_A.into(),
                    },
                    path,
                    1_000,
                )
                .unwrap();
        }
        store
            .insert_managed_run(&ManagedRun {
                id: "run1".into(),
                workspace_id: "w1".into(),
                kind: ManagedRunKind::Classify,
                state: RunState::Completed,
                plan_path: Some(root.path().join("plan.json").display().to_string()),
                apply_path: Some(root.path().join("apply.json").display().to_string()),
                undo_path: None,
                started_unix_ms: 1_000,
                finished_unix_ms: Some(2_000),
                move_count: 2,
                error: None,
            })
            .unwrap();
        store
            .set_recents_item_state(
                "w1",
                identities[2].clone(),
                RecentsState::Moved,
                Some("run1"),
            )
            .unwrap();
        store
            .set_recents_item_state(
                "w1",
                identities[3].clone(),
                RecentsState::Planned,
                Some("run1"),
            )
            .unwrap();
        store
            .insert_managed_run(&ManagedRun {
                id: "run2".into(),
                workspace_id: "w1".into(),
                kind: ManagedRunKind::Classify,
                state: RunState::Planned,
                plan_path: Some(root.path().join("live-plan.json").display().to_string()),
                apply_path: None,
                undo_path: None,
                started_unix_ms: 3_000,
                finished_unix_ms: None,
                move_count: 1,
                error: None,
            })
            .unwrap();
        store
            .set_recents_item_state(
                "w1",
                identities[4].clone(),
                RecentsState::Planned,
                Some("run2"),
            )
            .unwrap();

        let summary = store
            .reconcile_recents_index(
                "w1",
                &[
                    identities[1].clone(),
                    identities[2].clone(),
                    identities[3].clone(),
                    identities[4].clone(),
                ],
            )
            .unwrap();
        assert_eq!(
            summary,
            RecentsReconcileSummary {
                deleted_stale_pending: 1,
                reset_returned: 2,
            }
        );
        assert!(
            store
                .recents_item("w1", identities[0].clone())
                .unwrap()
                .is_none()
        );
        assert_eq!(
            store
                .recents_item("w1", identities[2].clone())
                .unwrap()
                .unwrap()
                .state,
            RecentsState::Pending
        );
        assert_eq!(
            store
                .recents_item("w1", identities[3].clone())
                .unwrap()
                .unwrap()
                .state,
            RecentsState::Pending
        );
        assert_eq!(
            store
                .recents_item("w1", identities[4].clone())
                .unwrap()
                .unwrap()
                .state,
            RecentsState::Planned
        );
    }

    #[test]
    fn recents_reconcile_rolls_back_all_changes_on_failure() {
        let root = tempdir().unwrap();
        let mut store = StateStore::open_in_memory().unwrap();
        setup_workspace(&mut store, root.path());
        let stale = FsIdentity {
            device: 1,
            inode: 1,
        };
        let returned = FsIdentity {
            device: 1,
            inode: 2,
        };
        for (identity, path) in [
            (stale.clone(), "Recents/stale.txt"),
            (returned.clone(), "Recents/returned.txt"),
        ] {
            store
                .upsert_observation(
                    "w1",
                    &FileFingerprint {
                        identity,
                        size: 1,
                        sha256: DIGEST_A.into(),
                    },
                    path,
                    1_000,
                )
                .unwrap();
        }
        store
            .insert_managed_run(&ManagedRun {
                id: "run1".into(),
                workspace_id: "w1".into(),
                kind: ManagedRunKind::Classify,
                state: RunState::Completed,
                plan_path: Some(root.path().join("plan.json").display().to_string()),
                apply_path: Some(root.path().join("apply.json").display().to_string()),
                undo_path: None,
                started_unix_ms: 1_000,
                finished_unix_ms: Some(2_000),
                move_count: 1,
                error: None,
            })
            .unwrap();
        store
            .set_recents_item_state("w1", returned.clone(), RecentsState::Moved, Some("run1"))
            .unwrap();
        store
            .connection()
            .execute_batch(
                "CREATE TEMP TRIGGER fail_returned_reset
                 BEFORE UPDATE OF state ON recents_items
                 WHEN OLD.state != 'pending'
                 BEGIN SELECT RAISE(ABORT, 'forced failure'); END;",
            )
            .unwrap();

        assert!(
            store
                .reconcile_recents_index("w1", std::slice::from_ref(&returned))
                .is_err()
        );
        assert!(store.recents_item("w1", stale).unwrap().is_some());
        assert_eq!(
            store.recents_item("w1", returned).unwrap().unwrap().state,
            RecentsState::Moved
        );
    }

    #[test]
    fn recents_eligibility_requires_retention_and_a_stable_settle_window() {
        let root = tempdir().unwrap();
        let mut store = StateStore::open_in_memory().unwrap();
        setup_workspace(&mut store, root.path());
        let identity = FsIdentity {
            device: 1,
            inode: 2,
        };
        let initial = FileFingerprint {
            identity: identity.clone(),
            size: 42,
            sha256: DIGEST_A.into(),
        };

        let first = store
            .upsert_observation("w1", &initial, "Recents/report.pdf", 1_000)
            .unwrap();
        assert_eq!(first.first_seen_unix_ms, 1_000);
        assert_eq!(first.stable_since_unix_ms, 1_000);
        assert_eq!(first.eligible_unix_ms, 101_000);
        assert!(store.eligible_items("w1", 100_999).unwrap().is_empty());

        let unchanged = store
            .upsert_observation("w1", &initial, "Recents/report.pdf", 50_000)
            .unwrap();
        assert_eq!(unchanged.stable_since_unix_ms, 1_000);
        assert_eq!(unchanged.eligible_unix_ms, 101_000);

        let changed = FileFingerprint {
            size: 43,
            sha256: DIGEST_B.into(),
            ..initial
        };
        let changed = store
            .upsert_observation("w1", &changed, "Recents/report.pdf", 80_000)
            .unwrap();
        assert_eq!(changed.first_seen_unix_ms, 1_000);
        assert_eq!(changed.stable_since_unix_ms, 80_000);
        assert_eq!(changed.eligible_unix_ms, 110_000);
        assert!(store.eligible_items("w1", 109_999).unwrap().is_empty());
        assert_eq!(store.eligible_items("w1", 110_000).unwrap(), [changed]);
    }

    #[test]
    fn changed_observation_resets_planned_item_but_preserves_first_seen() {
        let root = tempdir().unwrap();
        let mut store = StateStore::open_in_memory().unwrap();
        setup_workspace(&mut store, root.path());
        let identity = FsIdentity {
            device: 1,
            inode: 2,
        };
        let initial = FileFingerprint {
            identity: identity.clone(),
            size: 42,
            sha256: DIGEST_A.into(),
        };
        store
            .upsert_observation("w1", &initial, "Recents/report.pdf", 1_000)
            .unwrap();
        store
            .insert_managed_run(&ManagedRun {
                id: "run1".into(),
                workspace_id: "w1".into(),
                kind: ManagedRunKind::Classify,
                state: RunState::Planned,
                plan_path: Some(root.path().join("plan.json").display().to_string()),
                apply_path: None,
                undo_path: None,
                started_unix_ms: 2_000,
                finished_unix_ms: None,
                move_count: 1,
                error: None,
            })
            .unwrap();
        store
            .set_recents_item_state("w1", identity.clone(), RecentsState::Planned, Some("run1"))
            .unwrap();

        let changed = FileFingerprint {
            size: 99,
            sha256: DIGEST_B.into(),
            ..initial
        };
        let item = store
            .upsert_observation("w1", &changed, "Recents/report.pdf", 5_000)
            .unwrap();
        assert_eq!(item.first_seen_unix_ms, 1_000);
        assert_eq!(item.stable_since_unix_ms, 5_000);
        assert_eq!(item.state, RecentsState::Pending);
        assert_eq!(item.last_run_id, None);
    }

    #[test]
    fn indexes_managed_runs_and_recent_completed_moves() {
        let root = tempdir().unwrap();
        let mut store = StateStore::open_in_memory().unwrap();
        setup_workspace(&mut store, root.path());
        let mut run = ManagedRun {
            id: "run1".into(),
            workspace_id: "w1".into(),
            kind: ManagedRunKind::Setup,
            state: RunState::Completed,
            plan_path: Some(root.path().join("plan.json").display().to_string()),
            apply_path: Some(root.path().join("apply.json").display().to_string()),
            undo_path: None,
            started_unix_ms: 1_000,
            finished_unix_ms: Some(2_000),
            move_count: 3,
            error: None,
        };
        store.insert_managed_run(&run).unwrap();
        assert_eq!(store.managed_run("run1").unwrap(), Some(run.clone()));
        assert!(store.recent_managed_moves("w1", 10).unwrap().is_empty());

        run.undo_path = Some(root.path().join("undo.json").display().to_string());
        store.update_managed_run(&run).unwrap();
        assert_eq!(store.managed_runs("w1").unwrap(), [run]);

        let file_run = ManagedRun {
            id: "run2".into(),
            workspace_id: "w1".into(),
            kind: ManagedRunKind::Stage,
            state: RunState::Completed,
            plan_path: Some(root.path().join("stage-plan.json").display().to_string()),
            apply_path: Some(root.path().join("stage-apply.json").display().to_string()),
            undo_path: None,
            started_unix_ms: 3_000,
            finished_unix_ms: Some(4_000),
            move_count: 1,
            error: None,
        };
        store.insert_managed_run(&file_run).unwrap();
        assert_eq!(
            store.recent_managed_moves("w1", 10).unwrap(),
            std::slice::from_ref(&file_run)
        );

        let undo_one = root.path().join("undo-one.json").display().to_string();
        let undo_two = root.path().join("undo-two.json").display().to_string();
        store
            .record_managed_undo_journal("run2", &undo_one, 5_000)
            .unwrap();
        store
            .record_managed_undo_journal("run2", &undo_two, 6_000)
            .unwrap();
        assert_eq!(
            store.managed_undo_journal_paths("run2").unwrap(),
            [undo_one, undo_two]
        );
        assert!(
            store
                .record_managed_undo_journal(
                    "run1",
                    &root.path().join("bad.json").display().to_string(),
                    7_000
                )
                .is_err()
        );

        store.delete_managed_run("run1").unwrap();
        assert_eq!(store.managed_runs("w1").unwrap(), [file_run]);
        assert!(store.recent_managed_moves("w1", 0).is_err());
    }

    #[test]
    fn managed_undo_finalization_is_atomic_and_retryable() {
        let root = tempdir().unwrap();
        let mut store = StateStore::open_in_memory().unwrap();
        setup_workspace(&mut store, root.path());
        let identity = FsIdentity {
            device: 7,
            inode: 11,
        };
        store
            .upsert_observation(
                "w1",
                &FileFingerprint {
                    identity: identity.clone(),
                    size: 1,
                    sha256: DIGEST_A.into(),
                },
                "Recents/report.txt",
                1_000,
            )
            .unwrap();
        store
            .insert_managed_run(&ManagedRun {
                id: "run1".into(),
                workspace_id: "w1".into(),
                kind: ManagedRunKind::Stage,
                state: RunState::Completed,
                plan_path: Some(root.path().join("plan.json").display().to_string()),
                apply_path: Some(root.path().join("apply.json").display().to_string()),
                undo_path: None,
                started_unix_ms: 1_000,
                finished_unix_ms: Some(2_000),
                move_count: 1,
                error: None,
            })
            .unwrap();
        let undo_path = root.path().join("undo.json").display().to_string();
        store
            .connection()
            .execute_batch(
                "CREATE TEMP TRIGGER fail_undo_run_update
                 BEFORE UPDATE OF undo_path ON managed_runs
                 BEGIN SELECT RAISE(ABORT, 'forced failure'); END;",
            )
            .unwrap();

        assert!(
            store
                .finalize_managed_undo("run1", &undo_path, std::slice::from_ref(&identity), 3_000)
                .is_err()
        );
        assert!(store.managed_undo_journal_paths("run1").unwrap().is_empty());
        assert!(
            store
                .recents_item("w1", identity.clone())
                .unwrap()
                .is_some()
        );

        store
            .connection()
            .execute_batch("DROP TRIGGER fail_undo_run_update")
            .unwrap();
        store
            .finalize_managed_undo("run1", &undo_path, std::slice::from_ref(&identity), 3_000)
            .unwrap();
        assert_eq!(
            store.managed_undo_journal_paths("run1").unwrap(),
            std::slice::from_ref(&undo_path)
        );
        assert_eq!(
            store.managed_run("run1").unwrap().unwrap().undo_path,
            Some(undo_path)
        );
        assert!(store.recents_item("w1", identity).unwrap().is_none());
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
