use std::{
    collections::HashSet,
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::Serialize;
use tempfile::NamedTempFile;

use crate::managed::{
    apply_managed_directory_adoption_excluding, build_managed_directory_adoption_plan_excluding,
};

use crate::{
    ApplySession, ApplyState, Config, Error, FolderSet, LocalContentExtractor, MANAGED_AREAS,
    ManagedEntryFingerprint, ManagedLibraryEdit, ManagedLibraryEditPlan, ManagedLibraryEditSession,
    ManagedLibraryEditState, ManagedLibraryEditUndoSession, ManagedMoveOutcome,
    ManagedReprocessArea, ManagedReprocessSelection, ManagedRun, ManagedRunKind, ManagedSetupPlan,
    ManagedSetupSession, ManagedSetupState, ManagedSetupUndoSession, ManagedSetupUndoState,
    ManagedWorkspace, MonitorRecord, MonitoringOptions, OpenAiCompatibleModel, Plan,
    RecentsReconcileSummary, RecentsState, RunState, SourceLock, StateStore, ai_library_folder_set,
    apply_managed_setup, apply_monitoring_plan, apply_plan, build_reprocess_to_recents_plan,
    build_stage_to_recents_plan, canonical_source_identity, filter_recents_candidates,
    fingerprint_candidate, persist_monitoring_plan, plan_monitor_candidates,
    recents_file_candidates, reprocess_file_candidates, resume_apply_session, resume_managed_setup,
    root_file_candidates, undo_managed_directory_adoption,
    validate_managed_workspace_root_candidate,
};

static ID_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug)]
pub struct ManagedService {
    state_path: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ManagedDirectoryAdoption {
    pub run_id: String,
    pub plan_path: String,
    pub apply_path: Option<String>,
    pub move_count: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ManagedCycleResult {
    pub workspace_id: String,
    pub artifact_directory: String,
    pub directory_adoption: Option<ManagedDirectoryAdoption>,
    pub runs: Vec<ManagedRun>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ManagedActivationResult {
    pub workspace: ManagedWorkspace,
    pub setup_run: ManagedRun,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ManagedLibraryEditResult {
    pub workspace: ManagedWorkspace,
    pub run: ManagedRun,
    pub session: ManagedLibraryEditSession,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ManagedLibraryEditUndoResult {
    pub workspace: ManagedWorkspace,
    pub run: ManagedRun,
    pub session: ManagedLibraryEditUndoSession,
}

impl ManagedService {
    pub fn new(state_path: impl Into<PathBuf>) -> Self {
        Self {
            state_path: state_path.into(),
        }
    }

    pub fn state_path(&self) -> &Path {
        &self.state_path
    }

    pub fn activate_workspace(
        &self,
        plan: &ManagedSetupPlan,
        raw_folders: &FolderSet,
        config_path: &Path,
        retention_seconds: u64,
        settle_seconds: u64,
    ) -> Result<ManagedActivationResult, Error> {
        self.activate_workspace_in(
            plan,
            raw_folders,
            config_path,
            retention_seconds,
            settle_seconds,
            None,
        )
    }

    pub fn activate_workspace_in(
        &self,
        plan: &ManagedSetupPlan,
        raw_folders: &FolderSet,
        config_path: &Path,
        retention_seconds: u64,
        settle_seconds: u64,
        run_directory: Option<&Path>,
    ) -> Result<ManagedActivationResult, Error> {
        validate_windows(retention_seconds, settle_seconds)?;
        plan.validate()?;
        raw_folders.validate()?;
        if raw_folders.source != plan.source {
            return Err(Error::InvalidState(
                "approved folders do not belong to the managed source".into(),
            ));
        }
        let config_path = canonical_config_path(config_path, Path::new(&plan.source))?;
        let managed_folders = ai_library_folder_set(raw_folders)?;
        let mut store = self.store()?;
        validate_state_outside_source(&self.state_path, Path::new(&plan.source))?;
        validate_managed_workspace_root_candidate(Path::new(&plan.source))?;
        ensure_no_monitor_overlap(&store, Path::new(&plan.source))?;

        let workspace_id = new_id("workspace")?;
        let run_directory = match run_directory {
            Some(path) => create_requested_run_directory(path, Path::new(&plan.source))?,
            None => self.create_run_directory(&workspace_id, "setup")?,
        };
        let plan_path = run_directory.join("setup-plan.json");
        let folders_path = run_directory.join("folders.json");
        let setup_path = run_directory.join("setup-session.json");
        write_json(&plan_path, plan)?;
        write_json(&folders_path, &managed_folders)?;
        let setup = apply_managed_setup(plan, &setup_path)?;
        if setup.state != ManagedSetupState::Completed {
            return Err(Error::InvalidState(format!(
                "managed setup finished with {:?}; inspect {}",
                setup.state,
                setup_path.display()
            )));
        }

        let recovery_error = |error| activation_recovery_error(error, &setup_path);
        let now = unix_ms().map_err(&recovery_error)?;
        let monitor_id = new_id("managed-monitor").map_err(&recovery_error)?;
        let folder_digest = managed_folders.sha256().map_err(&recovery_error)?;
        let folder_path_text = path_text(&folders_path).map_err(&recovery_error)?;
        let monitor = MonitorRecord {
            id: monitor_id.clone(),
            source: plan.source.clone(),
            source_identity: plan.source_identity.clone(),
            folder_set_path: folder_path_text.clone(),
            folder_set_sha256: folder_digest.clone(),
            interval_seconds: retention_seconds.max(10),
            enabled: true,
            last_checked_unix_ms: None,
            created_unix_ms: now,
            updated_unix_ms: now,
            deleted_unix_ms: None,
        };
        let workspace = ManagedWorkspace {
            id: workspace_id.clone(),
            monitor_id: monitor_id.clone(),
            source: plan.source.clone(),
            source_identity: plan.source_identity.clone(),
            folder_set_path: folder_path_text,
            folder_set_sha256: folder_digest,
            config_path: path_text(&config_path).map_err(&recovery_error)?,
            retention_seconds,
            settle_seconds,
            enabled: true,
            setup_session_path: Some(path_text(&setup_path).map_err(&recovery_error)?),
            created_unix_ms: now,
            updated_unix_ms: now,
        };
        store.insert_monitor(&monitor).map_err(&recovery_error)?;
        if let Err(error) = store.insert_managed_workspace(&workspace) {
            let _ = store.remove_monitor(&monitor_id, now);
            return Err(recovery_error(error));
        }
        let setup_run =
            setup_run(&workspace_id, &plan_path, &setup_path, &setup).map_err(&recovery_error)?;
        if let Err(error) = store.insert_managed_run(&setup_run) {
            let _ = store.delete_managed_workspace(&workspace_id);
            let _ = store.remove_monitor(&monitor_id, now);
            return Err(recovery_error(error));
        }
        Ok(ManagedActivationResult {
            workspace,
            setup_run,
        })
    }

    pub fn run_workspace(
        &self,
        workspace_id: &str,
        apply: bool,
    ) -> Result<ManagedCycleResult, Error> {
        self.run_workspace_in(workspace_id, apply, None)
    }

    pub fn run_workspace_in(
        &self,
        workspace_id: &str,
        apply: bool,
        run_directory: Option<&Path>,
    ) -> Result<ManagedCycleResult, Error> {
        let mut store = self.store()?;
        let workspace = require_workspace(&store, workspace_id)?;
        self.validate_workspace(&store, &workspace)?;
        let source = Path::new(&workspace.source);
        let out = match run_directory {
            Some(path) => create_requested_run_directory(path, source)?,
            None => self.create_run_directory(&workspace.id, "cycle")?,
        };

        let mut runs = Vec::new();
        let excluded_directories = managed_directory_identities(&store, &workspace)?;
        let adoption_plan =
            build_managed_directory_adoption_plan_excluding(source, &excluded_directories)?;
        let directory_adoption = if adoption_plan.moves.is_empty() {
            None
        } else {
            let plan_path = out.join("directory-adoption-plan.json");
            write_json(&plan_path, &adoption_plan)?;
            let mut run = adoption_run(&workspace.id, &plan_path, &adoption_plan)?;
            store.insert_managed_run(&run)?;
            if apply {
                apply_adoption_run(&mut store, &workspace, &mut run)?;
            }
            let result = ManagedDirectoryAdoption {
                run_id: run.id.clone(),
                plan_path: path_text(&plan_path)?,
                apply_path: run.apply_path.clone(),
                move_count: adoption_plan.moves.len() as u64,
            };
            runs.push(run);
            Some(result)
        };

        let mut root_candidates = Vec::new();
        for candidate in root_file_candidates(source)? {
            let fingerprint = fingerprint_candidate(source, &candidate)?;
            if !store.has_processed_identity(&workspace.monitor_id, fingerprint.identity)? {
                root_candidates.push(candidate);
            }
        }
        if !root_candidates.is_empty() {
            runs.push(self.run_stage(&mut store, &workspace, &out, &root_candidates, apply)?);
        }

        observe_recents(&mut store, &workspace, unix_ms()?)?;
        let recents_candidates = recents_file_candidates(source)?;
        let eligible_paths = store
            .eligible_items(workspace_id, unix_ms()?)?
            .into_iter()
            .map(|item| item.relative_path)
            .collect::<HashSet<_>>();
        let eligible =
            filter_recents_candidates(&recents_candidates, &HashSet::new(), &eligible_paths)?;
        runs.push(self.run_classify(&mut store, &workspace, &out, &eligible, apply)?);

        Ok(ManagedCycleResult {
            workspace_id: workspace.id,
            artifact_directory: path_text(&out)?,
            directory_adoption,
            runs,
        })
    }

    pub fn reprocess(
        &self,
        workspace_id: &str,
        area: ManagedReprocessArea,
        selection: &ManagedReprocessSelection,
        apply: bool,
    ) -> Result<ManagedCycleResult, Error> {
        self.reprocess_in(workspace_id, area, selection, apply, None)
    }

    pub fn reprocess_in(
        &self,
        workspace_id: &str,
        area: ManagedReprocessArea,
        selection: &ManagedReprocessSelection,
        apply: bool,
        run_directory: Option<&Path>,
    ) -> Result<ManagedCycleResult, Error> {
        let mut store = self.store()?;
        let workspace = require_workspace(&store, workspace_id)?;
        self.validate_workspace(&store, &workspace)?;
        let source = Path::new(&workspace.source);
        let candidates = reprocess_file_candidates(source, area, selection)?;
        if candidates.is_empty() {
            return Err(Error::InvalidState(
                "the selected managed area contains no regular files".into(),
            ));
        }
        let out = match run_directory {
            Some(path) => create_requested_run_directory(path, source)?,
            None => self.create_run_directory(&workspace.id, "reprocess")?,
        };
        let plan = build_reprocess_to_recents_plan(source, area, &candidates)?;
        let plan_path = out.join("reprocess-plan.json");
        write_json(&plan_path, &plan)?;
        let id = new_id("managed-reprocess")?;
        let mut run = planned_run(&id, &workspace.id, ManagedRunKind::Stage, &plan_path, &plan)?;
        store.insert_managed_run(&run)?;
        if apply {
            apply_indexed_run(&mut store, &workspace, &mut run)?;
        }
        Ok(ManagedCycleResult {
            workspace_id: workspace.id,
            artifact_directory: path_text(&out)?,
            directory_adoption: None,
            runs: vec![run],
        })
    }

    pub fn apply_run(&self, run_id: &str) -> Result<ManagedRun, Error> {
        let mut store = self.store()?;
        let mut run = require_run(&store, run_id)?;
        if run.state != RunState::Planned {
            return Err(Error::InvalidState(format!(
                "managed run {run_id:?} is not waiting for apply"
            )));
        }
        let workspace = require_workspace(&store, &run.workspace_id)?;
        self.validate_workspace(&store, &workspace)?;
        match run.kind {
            ManagedRunKind::Adopt => apply_adoption_run(&mut store, &workspace, &mut run)?,
            ManagedRunKind::Stage | ManagedRunKind::Classify => {
                apply_indexed_run(&mut store, &workspace, &mut run)?
            }
            ManagedRunKind::Setup | ManagedRunKind::Configure => {
                return Err(Error::InvalidState(
                    "this run kind requires its dedicated Apply service".into(),
                ));
            }
        }
        Ok(run)
    }

    pub fn resume_run(&self, run_id: &str) -> Result<ManagedRun, Error> {
        let mut store = self.store()?;
        let mut run = require_run(&store, run_id)?;
        if !matches!(run.state, RunState::Applying | RunState::NeedsResume) {
            return Err(Error::InvalidState(format!(
                "managed run {run_id:?} does not need resume"
            )));
        }
        let workspace = require_workspace(&store, &run.workspace_id)?;
        self.validate_workspace(&store, &workspace)?;
        match run.kind {
            ManagedRunKind::Adopt => resume_adoption_run(&mut store, &mut run)?,
            ManagedRunKind::Stage | ManagedRunKind::Classify => {
                resume_indexed_run(&mut store, &workspace, &mut run)?
            }
            ManagedRunKind::Setup | ManagedRunKind::Configure => {
                return Err(Error::InvalidState(
                    "this run kind requires its dedicated Resume service".into(),
                ));
            }
        }
        Ok(run)
    }

    pub fn undo_adoption_run(
        &self,
        run_id: &str,
        journal_path: &Path,
    ) -> Result<ManagedSetupUndoSession, Error> {
        let mut store = self.store()?;
        let mut run = require_run(&store, run_id)?;
        if run.kind != ManagedRunKind::Adopt || run.state != RunState::Completed {
            return Err(Error::InvalidState(
                "directory adoption Undo requires a completed adoption run".into(),
            ));
        }
        if run.undo_path.is_some() {
            return Err(Error::InvalidState(
                "directory adoption run has already been undone".into(),
            ));
        }
        let apply_path = run
            .apply_path
            .as_deref()
            .ok_or_else(|| Error::InvalidState("adoption run has no Apply session".into()))?;
        let session = ManagedSetupSession::load(Path::new(apply_path))?;
        let undo = undo_managed_directory_adoption(&session, journal_path)?;
        run.undo_path = Some(path_text(journal_path)?);
        store.update_managed_run(&run)?;
        if undo.state != ManagedSetupUndoState::Completed {
            return Err(Error::InvalidState(format!(
                "directory adoption Undo finished with {:?}",
                undo.state
            )));
        }
        Ok(undo)
    }

    pub fn preview_library_edit(
        &self,
        workspace_id: &str,
        operation: ManagedLibraryEdit,
    ) -> Result<ManagedLibraryEditPlan, Error> {
        let store = self.store()?;
        let workspace = require_workspace(&store, workspace_id)?;
        let folders = validate_library_edit_workspace(&store, &workspace)?;
        let added_id = matches!(operation, ManagedLibraryEdit::Add { .. })
            .then(|| new_id("destination"))
            .transpose()?;
        ManagedLibraryEditPlan::build(
            new_id("library-edit-plan")?,
            workspace.id,
            workspace.source_identity,
            workspace.folder_set_path,
            &folders,
            operation,
            added_id,
        )
    }

    pub fn apply_library_edit(
        &self,
        plan: &ManagedLibraryEditPlan,
    ) -> Result<ManagedLibraryEditResult, Error> {
        plan.validate()?;
        let mut store = self.store()?;
        let workspace = require_workspace(&store, &plan.workspace_id)?;
        validate_library_edit_plan_binding(&store, &workspace, plan)?;
        let _lock = SourceLock::acquire(Path::new(&workspace.source))?;
        let workspace = require_workspace(&store, &plan.workspace_id)?;
        validate_library_edit_plan_binding(&store, &workspace, plan)?;

        let run_directory = self.create_run_directory(&workspace.id, "configure")?;
        let plan_path = run_directory.join("library-edit-plan.json");
        let folders_path = run_directory.join("folders.json");
        let session_path = run_directory.join("library-edit-session.json");
        write_json(&plan_path, plan)?;
        write_json(&folders_path, &plan.after_folders)?;
        let started = unix_ms()?;
        let mut run = ManagedRun {
            id: new_id("managed-configure")?,
            workspace_id: workspace.id.clone(),
            kind: ManagedRunKind::Configure,
            state: RunState::Applying,
            plan_path: Some(path_text(&plan_path)?),
            apply_path: Some(path_text(&session_path)?),
            undo_path: None,
            started_unix_ms: started,
            finished_unix_ms: None,
            move_count: 0,
            error: None,
        };
        let mut session = ManagedLibraryEditSession {
            version: 1,
            id: new_id("library-edit-session")?,
            run_id: run.id.clone(),
            plan_id: plan.id.clone(),
            workspace_id: workspace.id.clone(),
            source: workspace.source.clone(),
            source_identity: workspace.source_identity.clone(),
            before_folder_set_path: plan.before_folder_set_path.clone(),
            before_folder_set_sha256: plan.before_folder_set_sha256.clone(),
            after_folder_set_path: path_text(&folders_path)?,
            after_folder_set_sha256: plan.after_folder_set_sha256.clone(),
            operation: plan.operation.clone(),
            state: ManagedLibraryEditState::Running,
            started_unix_ms: to_u64_time(started)?,
            finished_unix_ms: None,
            error: None,
        };
        write_json(&session_path, &session)?;
        store.insert_managed_run(&run)?;
        let updated = unix_ms()?;
        let updated_workspace = match store.replace_managed_folder_set_binding(
            &workspace.id,
            &run.id,
            &plan.before_folder_set_path,
            &plan.before_folder_set_sha256,
            &session.after_folder_set_path,
            &plan.after_folder_set_sha256,
            plan.removed_destination_id(),
            updated,
        ) {
            Ok(workspace) => workspace,
            Err(error) => {
                session.state = ManagedLibraryEditState::PartialFailure;
                session.finished_unix_ms = Some(to_u64_time(updated)?);
                session.error = Some(error.to_string());
                replace_json(&session_path, &session)?;
                run.state = RunState::Failed;
                run.finished_unix_ms = Some(updated);
                run.error = Some(error.to_string());
                store.update_managed_run(&run)?;
                return Err(error);
            }
        };
        session.state = ManagedLibraryEditState::Completed;
        session.finished_unix_ms = Some(to_u64_time(updated)?);
        replace_json(&session_path, &session)?;
        run.state = RunState::Completed;
        run.finished_unix_ms = Some(updated);
        store.update_managed_run(&run)?;
        Ok(ManagedLibraryEditResult {
            workspace: updated_workspace,
            run,
            session,
        })
    }

    pub fn resume_library_edit(&self, run_id: &str) -> Result<ManagedLibraryEditResult, Error> {
        let mut store = self.store()?;
        let mut run = require_run(&store, run_id)?;
        if run.kind != ManagedRunKind::Configure || run.state != RunState::Applying {
            return Err(Error::InvalidState(
                "AI Library edit resume requires a running Configure session".into(),
            ));
        }
        let plan = ManagedLibraryEditPlan::load(Path::new(
            run.plan_path
                .as_deref()
                .ok_or_else(|| Error::InvalidState("Configure run has no Plan".into()))?,
        ))?;
        let session_path = Path::new(
            run.apply_path
                .as_deref()
                .ok_or_else(|| Error::InvalidState("Configure run has no Session".into()))?,
        );
        let mut session = ManagedLibraryEditSession::load(session_path)?;
        let workspace = require_workspace(&store, &run.workspace_id)?;
        if workspace.enabled {
            return Err(Error::InvalidState(
                "managed workspace must remain disabled while resuming AI Library editing".into(),
            ));
        }
        validate_library_edit_session(&run, &workspace, &plan, &session)?;
        let _lock = SourceLock::acquire(Path::new(&workspace.source))?;
        let updated = unix_ms()?;
        let workspace = if workspace.folder_set_path == session.after_folder_set_path
            && workspace.folder_set_sha256 == session.after_folder_set_sha256
        {
            workspace
        } else {
            store.replace_managed_folder_set_binding(
                &workspace.id,
                &run.id,
                &plan.before_folder_set_path,
                &plan.before_folder_set_sha256,
                &session.after_folder_set_path,
                &plan.after_folder_set_sha256,
                plan.removed_destination_id(),
                updated,
            )?
        };
        session.state = ManagedLibraryEditState::Completed;
        session.finished_unix_ms = Some(to_u64_time(updated)?);
        session.error = None;
        replace_json(session_path, &session)?;
        run.state = RunState::Completed;
        run.finished_unix_ms = Some(updated);
        run.error = None;
        store.update_managed_run(&run)?;
        Ok(ManagedLibraryEditResult {
            workspace,
            run,
            session,
        })
    }

    pub fn undo_library_edit(
        &self,
        run_id: &str,
        journal_path: &Path,
    ) -> Result<ManagedLibraryEditUndoResult, Error> {
        let mut store = self.store()?;
        let mut run = require_run(&store, run_id)?;
        if run.kind != ManagedRunKind::Configure || run.state != RunState::Completed {
            return Err(Error::InvalidState(
                "AI Library edit Undo requires a completed Configure session".into(),
            ));
        }
        if run.undo_path.is_some() {
            return Err(Error::InvalidState(
                "AI Library edit session has already been undone".into(),
            ));
        }
        let plan = ManagedLibraryEditPlan::load(Path::new(
            run.plan_path
                .as_deref()
                .ok_or_else(|| Error::InvalidState("Configure run has no Plan".into()))?,
        ))?;
        let apply = ManagedLibraryEditSession::load(Path::new(
            run.apply_path
                .as_deref()
                .ok_or_else(|| Error::InvalidState("Configure run has no Session".into()))?,
        ))?;
        let workspace = require_workspace(&store, &run.workspace_id)?;
        if workspace.enabled {
            return Err(Error::InvalidState(
                "managed workspace must be disabled before undoing AI Library editing".into(),
            ));
        }
        validate_library_edit_session(&run, &workspace, &plan, &apply)?;
        validate_library_edit_undo_path(&run, &workspace, journal_path)?;
        let _lock = SourceLock::acquire(Path::new(&workspace.source))?;
        let started = unix_ms()?;
        let mut undo = ManagedLibraryEditUndoSession {
            version: 1,
            id: new_id("library-edit-undo")?,
            apply_session_id: apply.id,
            workspace_id: workspace.id.clone(),
            state: ManagedLibraryEditState::Running,
            started_unix_ms: to_u64_time(started)?,
            finished_unix_ms: None,
            error: None,
        };
        run.undo_path = Some(path_text(journal_path)?);
        run.state = RunState::NeedsResume;
        run.finished_unix_ms = Some(started);
        run.error = Some("AI Library edit Undo is pending".into());
        store.update_managed_run(&run)?;
        write_json(journal_path, &undo)?;
        let removed_id = match &plan.operation {
            ManagedLibraryEdit::Add { .. } => plan
                .after_folders
                .folders
                .iter()
                .find(|folder| {
                    !plan
                        .before_folders
                        .folders
                        .iter()
                        .any(|before| before.id == folder.id)
                })
                .map(|folder| folder.id.as_str()),
            _ => None,
        };
        let updated = unix_ms()?;
        let workspace = match store.replace_managed_folder_set_binding(
            &workspace.id,
            &run.id,
            &apply.after_folder_set_path,
            &apply.after_folder_set_sha256,
            &apply.before_folder_set_path,
            &apply.before_folder_set_sha256,
            removed_id,
            updated,
        ) {
            Ok(workspace) => workspace,
            Err(error) => {
                run.state = RunState::NeedsResume;
                run.finished_unix_ms = Some(updated);
                run.error = Some(format!("AI Library edit Undo is pending: {error}"));
                store.update_managed_run(&run)?;
                return Err(error);
            }
        };
        undo.state = ManagedLibraryEditState::Completed;
        undo.finished_unix_ms = Some(to_u64_time(updated)?);
        replace_json(journal_path, &undo)?;
        run.state = RunState::Completed;
        run.finished_unix_ms = Some(updated);
        run.error = None;
        store.update_managed_run(&run)?;
        Ok(ManagedLibraryEditUndoResult {
            workspace,
            run,
            session: undo,
        })
    }

    pub fn resume_library_edit_undo(
        &self,
        run_id: &str,
    ) -> Result<ManagedLibraryEditUndoResult, Error> {
        let mut store = self.store()?;
        let mut run = require_run(&store, run_id)?;
        if run.kind != ManagedRunKind::Configure || run.state != RunState::NeedsResume {
            return Err(Error::InvalidState(
                "AI Library edit Undo resume requires a resumable Configure run".into(),
            ));
        }
        let plan = ManagedLibraryEditPlan::load(Path::new(
            run.plan_path
                .as_deref()
                .ok_or_else(|| Error::InvalidState("Configure run has no Plan".into()))?,
        ))?;
        let apply = ManagedLibraryEditSession::load(Path::new(
            run.apply_path
                .as_deref()
                .ok_or_else(|| Error::InvalidState("Configure run has no Session".into()))?,
        ))?;
        let undo_path = Path::new(
            run.undo_path
                .as_deref()
                .ok_or_else(|| Error::InvalidState("Configure run has no Undo journal".into()))?,
        );
        let mut undo: ManagedLibraryEditUndoSession = if undo_path.exists() {
            serde_json::from_reader(fs::File::open(undo_path).map_err(|source| {
                Error::ReadFile {
                    path: undo_path.display().to_string(),
                    source,
                }
            })?)?
        } else {
            let started = unix_ms()?;
            let undo = ManagedLibraryEditUndoSession {
                version: 1,
                id: new_id("library-edit-undo")?,
                apply_session_id: apply.id.clone(),
                workspace_id: run.workspace_id.clone(),
                state: ManagedLibraryEditState::Running,
                started_unix_ms: to_u64_time(started)?,
                finished_unix_ms: None,
                error: None,
            };
            write_json(undo_path, &undo)?;
            undo
        };
        if undo.version != 1
            || undo.apply_session_id != apply.id
            || undo.workspace_id != run.workspace_id
            || undo.state != ManagedLibraryEditState::Running
        {
            return Err(Error::InvalidState(
                "managed AI Library edit Undo journal provenance does not match".into(),
            ));
        }
        let workspace = require_workspace(&store, &run.workspace_id)?;
        if workspace.enabled {
            return Err(Error::InvalidState(
                "managed workspace must remain disabled while resuming AI Library edit Undo".into(),
            ));
        }
        validate_library_edit_session(&run, &workspace, &plan, &apply)?;
        validate_library_edit_undo_path(&run, &workspace, undo_path)?;
        let _lock = SourceLock::acquire(Path::new(&workspace.source))?;
        let updated = unix_ms()?;
        let workspace = if workspace.folder_set_path == apply.before_folder_set_path
            && workspace.folder_set_sha256 == apply.before_folder_set_sha256
        {
            workspace
        } else {
            let removed_id = match &plan.operation {
                ManagedLibraryEdit::Add { .. } => plan
                    .after_folders
                    .folders
                    .iter()
                    .find(|folder| {
                        !plan
                            .before_folders
                            .folders
                            .iter()
                            .any(|before| before.id == folder.id)
                    })
                    .map(|folder| folder.id.as_str()),
                _ => None,
            };
            store.replace_managed_folder_set_binding(
                &workspace.id,
                &run.id,
                &apply.after_folder_set_path,
                &apply.after_folder_set_sha256,
                &apply.before_folder_set_path,
                &apply.before_folder_set_sha256,
                removed_id,
                updated,
            )?
        };
        undo.state = ManagedLibraryEditState::Completed;
        undo.finished_unix_ms = Some(to_u64_time(updated)?);
        undo.error = None;
        replace_json(undo_path, &undo)?;
        run.state = RunState::Completed;
        run.finished_unix_ms = Some(updated);
        run.error = None;
        store.update_managed_run(&run)?;
        Ok(ManagedLibraryEditUndoResult {
            workspace,
            run,
            session: undo,
        })
    }

    fn store(&self) -> Result<StateStore, Error> {
        StateStore::open(&self.state_path)
    }

    fn validate_workspace(
        &self,
        store: &StateStore,
        workspace: &ManagedWorkspace,
    ) -> Result<(), Error> {
        if !workspace.enabled {
            return Err(Error::InvalidState("managed workspace is disabled".into()));
        }
        let (source, identity) = canonical_source_identity(Path::new(&workspace.source))?;
        if source != Path::new(&workspace.source) || identity != workspace.source_identity {
            return Err(Error::InvalidState(
                "managed workspace source identity changed".into(),
            ));
        }
        validate_state_outside_source(&self.state_path, &source)?;
        let config_path = canonical_config_path(Path::new(&workspace.config_path), &source)?;
        if config_path != Path::new(&workspace.config_path) {
            return Err(Error::InvalidState(
                "managed workspace model configuration path changed".into(),
            ));
        }
        let folders = FolderSet::load(Path::new(&workspace.folder_set_path))?;
        if folders.source != workspace.source || folders.sha256()? != workspace.folder_set_sha256 {
            return Err(Error::InvalidState(
                "managed workspace folder set changed".into(),
            ));
        }
        let monitor = store
            .monitor(&workspace.monitor_id)?
            .filter(|monitor| monitor.deleted_unix_ms.is_none())
            .ok_or_else(|| Error::InvalidState("managed monitor is missing".into()))?;
        if monitor.source != workspace.source
            || monitor.source_identity != workspace.source_identity
            || monitor.folder_set_sha256 != workspace.folder_set_sha256
            || monitor.enabled != workspace.enabled
        {
            return Err(Error::InvalidState(
                "managed monitor no longer matches its workspace".into(),
            ));
        }
        for area in MANAGED_AREAS {
            let path = source.join(area);
            let metadata = fs::symlink_metadata(&path)
                .map_err(|error| io_error("inspect managed area", &path, error))?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(Error::InvalidState(format!(
                    "managed area is not a real directory: {}",
                    path.display()
                )));
            }
        }
        Ok(())
    }

    fn create_run_directory(&self, workspace_id: &str, kind: &str) -> Result<PathBuf, Error> {
        let parent = self.state_path.parent().ok_or_else(|| {
            Error::InvalidState("managed state database has no parent directory".into())
        })?;
        let root = parent.join("managed-runs");
        ensure_private_directory(&root)?;
        let workspace_root = root.join(workspace_id);
        ensure_private_directory(&workspace_root)?;
        let run = workspace_root.join(new_id(kind)?);
        fs::create_dir(&run).map_err(|error| io_error("create managed run", &run, error))?;
        fs::set_permissions(&run, fs::Permissions::from_mode(0o700))
            .map_err(|error| io_error("secure managed run", &run, error))?;
        Ok(run)
    }

    fn run_stage(
        &self,
        store: &mut StateStore,
        workspace: &ManagedWorkspace,
        out: &Path,
        candidates: &[crate::FileCandidate],
        apply: bool,
    ) -> Result<ManagedRun, Error> {
        let id = new_id("managed-stage")?;
        let plan = build_stage_to_recents_plan(Path::new(&workspace.source), candidates)?;
        let plan_path = out.join("stage-plan.json");
        write_json(&plan_path, &plan)?;
        let mut run = planned_run(&id, &workspace.id, ManagedRunKind::Stage, &plan_path, &plan)?;
        store.insert_managed_run(&run)?;
        if apply {
            apply_indexed_run(store, workspace, &mut run)?;
        }
        Ok(run)
    }

    fn run_classify(
        &self,
        store: &mut StateStore,
        workspace: &ManagedWorkspace,
        out: &Path,
        candidates: &[crate::FileCandidate],
        apply: bool,
    ) -> Result<ManagedRun, Error> {
        let id = new_id("managed-classify")?;
        let started = unix_ms()?;
        if candidates.is_empty() {
            let run = noop_run(id, &workspace.id, started)?;
            store.insert_managed_run(&run)?;
            return Ok(run);
        }
        let monitor = store
            .monitor(&workspace.monitor_id)?
            .filter(|monitor| monitor.deleted_unix_ms.is_none())
            .ok_or_else(|| Error::InvalidState("managed monitor is missing".into()))?;
        let folders = FolderSet::load(Path::new(&workspace.folder_set_path))?;
        let rules = store.active_rules(&monitor.id)?;
        let config = Config::load(Path::new(&workspace.config_path))?;
        let model = OpenAiCompatibleModel::new(&config.model)?;
        let extractor = LocalContentExtractor::new(config.privacy.extraction.clone());
        let monitoring = plan_monitor_candidates(
            store,
            &monitor,
            &folders,
            &rules,
            candidates,
            &model,
            &extractor,
            MonitoringOptions::from_config(&config),
        )?;
        store.start_run(&id, &monitor.id, started)?;
        if monitoring.plan.entries.is_empty() {
            store.finish_noop(&id, monitoring.stats.total_files as u64, unix_ms()?)?;
            let run = noop_run(id, &workspace.id, started)?;
            store.insert_managed_run(&run)?;
            return Ok(run);
        }
        let plan_path = out.join("classify-plan.json");
        persist_monitoring_plan(store, &id, &plan_path, &monitoring)?;
        let mut run = planned_run(
            &id,
            &workspace.id,
            ManagedRunKind::Classify,
            &plan_path,
            &monitoring.plan,
        )?;
        store.insert_managed_run(&run)?;
        mark_recents_entries(
            store,
            workspace,
            &monitoring.plan,
            RecentsState::Planned,
            &id,
        )?;
        if apply {
            apply_indexed_run(store, workspace, &mut run)?;
        }
        Ok(run)
    }
}

fn validate_library_edit_workspace(
    store: &StateStore,
    workspace: &ManagedWorkspace,
) -> Result<FolderSet, Error> {
    if workspace.enabled {
        return Err(Error::InvalidState(
            "managed workspace must be disabled before editing its Library".into(),
        ));
    }
    let (source, identity) = canonical_source_identity(Path::new(&workspace.source))?;
    if source != Path::new(&workspace.source) || identity != workspace.source_identity {
        return Err(Error::InvalidState(
            "managed workspace source identity changed".into(),
        ));
    }
    let folders = FolderSet::load(Path::new(&workspace.folder_set_path))?;
    if folders.source != workspace.source || folders.sha256()? != workspace.folder_set_sha256 {
        return Err(Error::InvalidState(
            "managed workspace FolderSet binding changed".into(),
        ));
    }
    let monitor = store
        .monitor(&workspace.monitor_id)?
        .filter(|monitor| monitor.deleted_unix_ms.is_none())
        .ok_or_else(|| Error::InvalidState("managed monitor is missing".into()))?;
    if monitor.source != workspace.source
        || monitor.source_identity != workspace.source_identity
        || monitor.folder_set_path != workspace.folder_set_path
        || monitor.folder_set_sha256 != workspace.folder_set_sha256
        || monitor.enabled != workspace.enabled
    {
        return Err(Error::InvalidState(
            "managed monitor no longer matches its workspace".into(),
        ));
    }
    Ok(folders)
}

fn validate_library_edit_plan_binding(
    store: &StateStore,
    workspace: &ManagedWorkspace,
    plan: &ManagedLibraryEditPlan,
) -> Result<(), Error> {
    let folders = validate_library_edit_workspace(store, workspace)?;
    if plan.workspace_id != workspace.id
        || plan.source != workspace.source
        || plan.source_identity != workspace.source_identity
        || plan.before_folder_set_path != workspace.folder_set_path
        || plan.before_folder_set_sha256 != workspace.folder_set_sha256
        || folders != plan.before_folders
    {
        return Err(Error::InvalidState(
            "managed AI Library edit preview is stale".into(),
        ));
    }
    Ok(())
}

fn validate_library_edit_session(
    run: &ManagedRun,
    workspace: &ManagedWorkspace,
    plan: &ManagedLibraryEditPlan,
    session: &ManagedLibraryEditSession,
) -> Result<(), Error> {
    let session_path = Path::new(
        run.apply_path
            .as_deref()
            .ok_or_else(|| Error::InvalidState("Configure run has no Session path".into()))?,
    );
    let run_directory = session_path
        .parent()
        .ok_or_else(|| Error::InvalidState("Configure Session has no parent directory".into()))?;
    let replacement_path = Path::new(&session.after_folder_set_path);
    if replacement_path != run_directory.join("folders.json")
        || !replacement_path.is_absolute()
        || replacement_path.starts_with(Path::new(&workspace.source))
    {
        return Err(Error::InvalidState(
            "managed AI Library replacement FolderSet must be the run-owned folders.json outside the source"
                .into(),
        ));
    }
    if session.run_id != run.id
        || session.plan_id != plan.id
        || session.workspace_id != workspace.id
        || session.source != workspace.source
        || session.source_identity != workspace.source_identity
        || session.before_folder_set_path != plan.before_folder_set_path
        || session.before_folder_set_sha256 != plan.before_folder_set_sha256
        || session.after_folder_set_sha256 != plan.after_folder_set_sha256
        || session.operation != plan.operation
    {
        return Err(Error::InvalidState(
            "managed AI Library edit Session provenance does not match its run and Plan".into(),
        ));
    }
    let replacement = FolderSet::load(Path::new(&session.after_folder_set_path))?;
    if replacement != plan.after_folders || replacement.sha256()? != session.after_folder_set_sha256
    {
        return Err(Error::InvalidState(
            "managed AI Library edit replacement FolderSet changed".into(),
        ));
    }
    let current = FolderSet::load(Path::new(&workspace.folder_set_path))?;
    if current.source != workspace.source || current.sha256()? != workspace.folder_set_sha256 {
        return Err(Error::InvalidState(
            "managed workspace FolderSet binding changed".into(),
        ));
    }
    if (workspace.folder_set_path != session.before_folder_set_path
        || workspace.folder_set_sha256 != session.before_folder_set_sha256)
        && (workspace.folder_set_path != session.after_folder_set_path
            || workspace.folder_set_sha256 != session.after_folder_set_sha256)
    {
        return Err(Error::InvalidState(
            "managed AI Library edit Session is stale for the current binding".into(),
        ));
    }
    Ok(())
}

fn validate_library_edit_undo_path(
    run: &ManagedRun,
    workspace: &ManagedWorkspace,
    journal_path: &Path,
) -> Result<(), Error> {
    if !journal_path.is_absolute() || journal_path.starts_with(Path::new(&workspace.source)) {
        return Err(Error::InvalidState(
            "AI Library edit Undo journal must be an absolute path outside the managed source"
                .into(),
        ));
    }
    let apply_parent = Path::new(
        run.apply_path
            .as_deref()
            .ok_or_else(|| Error::InvalidState("Configure run has no Session path".into()))?,
    )
    .parent()
    .ok_or_else(|| Error::InvalidState("Configure Session has no parent directory".into()))?;
    if journal_path.parent() != Some(apply_parent) {
        return Err(Error::InvalidState(
            "AI Library edit Undo journal must stay beside its Apply Session".into(),
        ));
    }
    let parent = fs::symlink_metadata(apply_parent).map_err(|error| {
        io_error(
            "inspect AI Library edit artifact directory",
            apply_parent,
            error,
        )
    })?;
    if parent.file_type().is_symlink() || !parent.is_dir() {
        return Err(Error::InvalidState(
            "AI Library edit artifact parent is not a real directory".into(),
        ));
    }
    Ok(())
}

fn managed_directory_identities(
    store: &StateStore,
    workspace: &ManagedWorkspace,
) -> Result<HashSet<(u64, u64)>, Error> {
    let setup_path = workspace.setup_session_path.as_deref().ok_or_else(|| {
        Error::InvalidState("managed workspace has no authoritative setup session".into())
    })?;
    let mut identities = HashSet::new();
    collect_moved_directory_identities(
        &mut identities,
        workspace,
        &ManagedSetupSession::load(Path::new(setup_path))?,
    )?;
    for run in store.managed_runs(&workspace.id)? {
        if run.kind != ManagedRunKind::Adopt {
            continue;
        }
        let Some(apply_path) = run.apply_path.as_deref() else {
            if matches!(run.state, RunState::Planned | RunState::Failed) {
                continue;
            }
            return Err(Error::InvalidState(format!(
                "adoption run {:?} has no authoritative Apply session",
                run.id
            )));
        };
        if !Path::new(apply_path).exists() && run.state == RunState::Failed {
            continue;
        }
        collect_moved_directory_identities(
            &mut identities,
            workspace,
            &ManagedSetupSession::load(Path::new(apply_path))?,
        )?;
    }
    Ok(identities)
}

fn collect_moved_directory_identities(
    identities: &mut HashSet<(u64, u64)>,
    workspace: &ManagedWorkspace,
    session: &ManagedSetupSession,
) -> Result<(), Error> {
    if session.source != workspace.source || session.source_identity != workspace.source_identity {
        return Err(Error::InvalidState(
            "managed directory journal does not belong to its workspace".into(),
        ));
    }
    for movement in &session.moves {
        if movement.outcome != ManagedMoveOutcome::Moved {
            continue;
        }
        if let ManagedEntryFingerprint::Directory { fingerprint } = &movement.fingerprint {
            identities.insert((fingerprint.identity.device, fingerprint.identity.inode));
        }
    }
    Ok(())
}

fn adoption_run(
    workspace_id: &str,
    plan_path: &Path,
    plan: &ManagedSetupPlan,
) -> Result<ManagedRun, Error> {
    Ok(ManagedRun {
        id: new_id("managed-adopt")?,
        workspace_id: workspace_id.into(),
        kind: ManagedRunKind::Adopt,
        state: RunState::Planned,
        plan_path: Some(path_text(plan_path)?),
        apply_path: None,
        undo_path: None,
        started_unix_ms: unix_ms()?,
        finished_unix_ms: None,
        move_count: plan.moves.len() as u64,
        error: None,
    })
}

fn apply_adoption_run(
    store: &mut StateStore,
    workspace: &ManagedWorkspace,
    run: &mut ManagedRun,
) -> Result<(), Error> {
    let plan_path = run
        .plan_path
        .as_deref()
        .ok_or_else(|| Error::InvalidState("adoption run has no Plan path".into()))?;
    let plan = ManagedSetupPlan::load(Path::new(plan_path))?;
    let parent = Path::new(plan_path)
        .parent()
        .ok_or_else(|| Error::InvalidState("adoption Plan has no parent directory".into()))?;
    let apply_path = parent.join("directory-adoption-apply.json");
    let excluded_directories = managed_directory_identities(store, workspace)?;
    run.state = RunState::Applying;
    run.apply_path = Some(path_text(&apply_path)?);
    store.update_managed_run(run)?;
    let session =
        match apply_managed_directory_adoption_excluding(&plan, &apply_path, &excluded_directories)
        {
            Ok(session) => session,
            Err(error) => {
                finalize_adoption_error(store, run, &apply_path, &error.to_string())?;
                return Err(error);
            }
        };
    finish_adoption_session(store, run, &session)
}

fn resume_adoption_run(store: &mut StateStore, run: &mut ManagedRun) -> Result<(), Error> {
    let apply_path = run
        .apply_path
        .clone()
        .ok_or_else(|| Error::InvalidState("adoption run has no Apply session".into()))?;
    let current = ManagedSetupSession::load(Path::new(&apply_path))?;
    let session = match current.state {
        ManagedSetupState::Running => match resume_managed_setup(Path::new(&apply_path)) {
            Ok(session) => session,
            Err(error) => {
                finalize_adoption_error(store, run, Path::new(&apply_path), &error.to_string())?;
                return Err(error);
            }
        },
        ManagedSetupState::Completed => current,
        state => {
            run.state = RunState::Failed;
            run.finished_unix_ms = Some(unix_ms()?);
            run.error = Some(format!("adoption session finished with {state:?}"));
            store.update_managed_run(run)?;
            return Err(Error::InvalidState(format!(
                "managed directory adoption is not resumable; found {state:?}"
            )));
        }
    };
    finish_adoption_session(store, run, &session)
}

fn finish_adoption_session(
    store: &mut StateStore,
    run: &mut ManagedRun,
    session: &ManagedSetupSession,
) -> Result<(), Error> {
    run.finished_unix_ms = Some(unix_ms()?);
    if session.state == ManagedSetupState::Completed {
        run.state = RunState::Completed;
        run.error = None;
        store.update_managed_run(run)
    } else {
        run.state = if session.state == ManagedSetupState::Running {
            RunState::NeedsResume
        } else {
            RunState::Failed
        };
        run.error = Some(format!(
            "directory adoption finished with {:?}",
            session.state
        ));
        store.update_managed_run(run)?;
        Err(Error::InvalidState(format!(
            "managed directory adoption finished with {:?}",
            session.state
        )))
    }
}

fn finalize_adoption_error(
    store: &mut StateStore,
    run: &mut ManagedRun,
    apply_path: &Path,
    message: &str,
) -> Result<(), Error> {
    let resumable = ManagedSetupSession::load(apply_path)
        .is_ok_and(|session| session.state == ManagedSetupState::Running);
    run.state = if resumable {
        RunState::NeedsResume
    } else {
        RunState::Failed
    };
    run.finished_unix_ms = Some(unix_ms()?);
    run.error = Some(sanitize_error(message));
    store.update_managed_run(run)
}

fn resume_indexed_run(
    store: &mut StateStore,
    workspace: &ManagedWorkspace,
    run: &mut ManagedRun,
) -> Result<(), Error> {
    let apply_path = run
        .apply_path
        .clone()
        .ok_or_else(|| Error::InvalidState("managed run has no Apply session".into()))?;
    let current = ApplySession::load(Path::new(&apply_path))?;
    let session = match current.state {
        ApplyState::Running => match resume_apply_session(Path::new(&apply_path)) {
            Ok(session) => session,
            Err(error) => {
                finalize_apply_error(store, run, Path::new(&apply_path), &error.to_string())?;
                return Err(error);
            }
        },
        ApplyState::Completed => current,
        state => {
            run.state = RunState::Failed;
            run.finished_unix_ms = Some(unix_ms()?);
            run.error = Some(format!("apply session finished with {state:?}"));
            store.update_managed_run(run)?;
            return Err(Error::InvalidState(format!(
                "managed Apply session is not resumable; found {state:?}"
            )));
        }
    };
    let plan_path = run
        .plan_path
        .as_deref()
        .ok_or_else(|| Error::InvalidState("managed run has no Plan path".into()))?;
    let plan = Plan::load(Path::new(plan_path))?;
    if run.kind == ManagedRunKind::Classify {
        store.reconcile_applying_runs(Some(&workspace.monitor_id), unix_ms()?)?;
    }
    if session.state != ApplyState::Completed {
        return finish_incomplete_apply(store, run, session.state);
    }
    mark_apply_finalization_pending(store, run)?;
    finalize_completed_apply(store, workspace, run, &plan)
}

fn apply_indexed_run(
    store: &mut StateStore,
    workspace: &ManagedWorkspace,
    run: &mut ManagedRun,
) -> Result<(), Error> {
    let plan_path = run
        .plan_path
        .as_deref()
        .ok_or_else(|| Error::InvalidState("managed run has no Plan path".into()))?;
    let plan = Plan::load(Path::new(plan_path))?;
    let parent = Path::new(plan_path)
        .parent()
        .ok_or_else(|| Error::InvalidState("managed Plan has no parent directory".into()))?;
    let apply_path = parent.join(match run.kind {
        ManagedRunKind::Stage => "stage-apply.json",
        ManagedRunKind::Classify => "classify-apply.json",
        ManagedRunKind::Setup | ManagedRunKind::Adopt | ManagedRunKind::Configure => {
            return Err(Error::InvalidState(
                "setup runs cannot be applied through managed run".into(),
            ));
        }
    });
    let apply_time = unix_ms()?;
    run.state = RunState::Applying;
    run.apply_path = Some(path_text(&apply_path)?);
    store.update_managed_run(run)?;
    let applied = match run.kind {
        ManagedRunKind::Stage => apply_plan(&plan, &apply_path),
        ManagedRunKind::Classify => {
            let lock = SourceLock::acquire(Path::new(&workspace.source))?;
            apply_monitoring_plan(store, &run.id, &plan, &apply_path, &lock, apply_time)
        }
        ManagedRunKind::Setup | ManagedRunKind::Adopt | ManagedRunKind::Configure => {
            unreachable!()
        }
    };
    let session = match applied {
        Ok(session) => session,
        Err(error) => {
            finalize_apply_error(store, run, &apply_path, &error.to_string())?;
            return Err(error);
        }
    };
    if session.state != ApplyState::Completed {
        return finish_incomplete_apply(store, run, session.state);
    }
    mark_apply_finalization_pending(store, run)?;
    finalize_completed_apply(store, workspace, run, &plan)
}

fn mark_apply_finalization_pending(
    store: &mut StateStore,
    run: &mut ManagedRun,
) -> Result<(), Error> {
    run.state = RunState::NeedsResume;
    run.finished_unix_ms = Some(unix_ms()?);
    run.error = Some("apply completed; state finalization is pending".into());
    store.update_managed_run(run)
}

fn finalize_completed_apply(
    store: &mut StateStore,
    workspace: &ManagedWorkspace,
    run: &mut ManagedRun,
    plan: &Plan,
) -> Result<(), Error> {
    match run.kind {
        ManagedRunKind::Stage => complete_stage_index(store, workspace, plan, unix_ms()?)?,
        ManagedRunKind::Classify => {
            mark_recents_entries(store, workspace, plan, RecentsState::Moved, &run.id)?
        }
        ManagedRunKind::Setup | ManagedRunKind::Adopt | ManagedRunKind::Configure => {
            unreachable!()
        }
    }
    run.state = RunState::Completed;
    run.finished_unix_ms = Some(unix_ms()?);
    run.error = None;
    store.update_managed_run(run)
}

fn finish_incomplete_apply(
    store: &mut StateStore,
    run: &mut ManagedRun,
    state: ApplyState,
) -> Result<(), Error> {
    run.state = if state == ApplyState::Running {
        RunState::NeedsResume
    } else {
        RunState::Failed
    };
    run.finished_unix_ms = Some(unix_ms()?);
    run.error = Some(format!("apply session finished with {state:?}"));
    store.update_managed_run(run)?;
    Err(Error::InvalidState(format!(
        "managed apply finished with {state:?}"
    )))
}

fn finalize_apply_error(
    store: &mut StateStore,
    run: &mut ManagedRun,
    apply_path: &Path,
    message: &str,
) -> Result<(), Error> {
    run.apply_path = apply_path
        .exists()
        .then(|| path_text(apply_path))
        .transpose()?;
    let resumable =
        ApplySession::load(apply_path).is_ok_and(|session| session.state == ApplyState::Running);
    run.state = if resumable {
        RunState::NeedsResume
    } else {
        RunState::Failed
    };
    run.finished_unix_ms = Some(unix_ms()?);
    run.error = Some(sanitize_error(message));
    store.update_managed_run(run)
}

fn sanitize_error(message: &str) -> String {
    let value = message
        .chars()
        .filter(|value| !value.is_control())
        .collect::<String>();
    if value.is_empty() {
        "managed operation failed".into()
    } else {
        value
    }
}

fn complete_stage_index(
    store: &mut StateStore,
    workspace: &ManagedWorkspace,
    plan: &Plan,
    observed_unix_ms: i64,
) -> Result<(), Error> {
    for entry in &plan.entries {
        if entry.source_path.contains('/') {
            store.forget_processed_file(
                &workspace.monitor_id,
                entry.source_fingerprint.identity.clone(),
            )?;
        }
    }
    observe_recents(store, workspace, observed_unix_ms)
}

fn observe_recents(
    store: &mut StateStore,
    workspace: &ManagedWorkspace,
    now: i64,
) -> Result<(), Error> {
    reconcile_recents(store, workspace, now)?;
    Ok(())
}

fn reconcile_recents(
    store: &mut StateStore,
    workspace: &ManagedWorkspace,
    now: i64,
) -> Result<RecentsReconcileSummary, Error> {
    let previously_moved = store
        .recents_items(&workspace.id)?
        .into_iter()
        .filter(|item| item.state == RecentsState::Moved)
        .map(|item| (item.file_identity.device, item.file_identity.inode))
        .collect::<HashSet<_>>();
    let mut observed = Vec::new();
    for candidate in recents_file_candidates(Path::new(&workspace.source))? {
        let fingerprint = fingerprint_candidate(Path::new(&workspace.source), &candidate)?;
        observed.push(fingerprint.identity.clone());
        store.upsert_observation(&workspace.id, &fingerprint, &candidate.source_path, now)?;
    }
    let summary = store.reconcile_recents_index(&workspace.id, &observed)?;
    for identity in observed
        .into_iter()
        .filter(|identity| previously_moved.contains(&(identity.device, identity.inode)))
    {
        store.forget_processed_file(&workspace.monitor_id, identity)?;
    }
    Ok(summary)
}

fn mark_recents_entries(
    store: &mut StateStore,
    workspace: &ManagedWorkspace,
    plan: &Plan,
    state: RecentsState,
    run_id: &str,
) -> Result<(), Error> {
    for entry in &plan.entries {
        if store
            .recents_item(&workspace.id, entry.source_fingerprint.identity.clone())?
            .is_some()
        {
            store.set_recents_item_state(
                &workspace.id,
                entry.source_fingerprint.identity.clone(),
                state,
                Some(run_id),
            )?;
        }
    }
    Ok(())
}

fn planned_run(
    id: &str,
    workspace_id: &str,
    kind: ManagedRunKind,
    plan_path: &Path,
    plan: &Plan,
) -> Result<ManagedRun, Error> {
    Ok(ManagedRun {
        id: id.into(),
        workspace_id: workspace_id.into(),
        kind,
        state: RunState::Planned,
        plan_path: Some(path_text(plan_path)?),
        apply_path: None,
        undo_path: None,
        started_unix_ms: unix_ms()?,
        finished_unix_ms: None,
        move_count: plan.entries.len() as u64,
        error: None,
    })
}

fn noop_run(id: String, workspace_id: &str, started: i64) -> Result<ManagedRun, Error> {
    Ok(ManagedRun {
        id,
        workspace_id: workspace_id.into(),
        kind: ManagedRunKind::Classify,
        state: RunState::Noop,
        plan_path: None,
        apply_path: None,
        undo_path: None,
        started_unix_ms: started,
        finished_unix_ms: Some(unix_ms()?),
        move_count: 0,
        error: None,
    })
}

fn setup_run(
    workspace_id: &str,
    plan_path: &Path,
    setup_path: &Path,
    setup: &ManagedSetupSession,
) -> Result<ManagedRun, Error> {
    Ok(ManagedRun {
        id: new_id("managed-setup")?,
        workspace_id: workspace_id.into(),
        kind: ManagedRunKind::Setup,
        state: RunState::Completed,
        plan_path: Some(path_text(plan_path)?),
        apply_path: Some(path_text(setup_path)?),
        undo_path: None,
        started_unix_ms: i64::try_from(setup.started_unix_ms)
            .map_err(|_| Error::InvalidState("managed setup start time is too large".into()))?,
        finished_unix_ms: setup
            .finished_unix_ms
            .map(i64::try_from)
            .transpose()
            .map_err(|_| Error::InvalidState("managed setup finish time is too large".into()))?,
        move_count: setup.moves.len() as u64,
        error: None,
    })
}

fn require_workspace(store: &StateStore, id: &str) -> Result<ManagedWorkspace, Error> {
    store
        .managed_workspace(id)?
        .ok_or_else(|| Error::InvalidState(format!("unknown managed workspace {id:?}")))
}

fn require_run(store: &StateStore, id: &str) -> Result<ManagedRun, Error> {
    store
        .managed_run(id)?
        .ok_or_else(|| Error::InvalidState(format!("unknown managed run {id:?}")))
}

fn ensure_no_monitor_overlap(store: &StateStore, source: &Path) -> Result<(), Error> {
    for monitor in store.active_monitors()? {
        let existing = Path::new(&monitor.source);
        if source.starts_with(existing) || existing.starts_with(source) {
            return Err(Error::InvalidState(format!(
                "managed source overlaps active workspace {:?}",
                monitor.source
            )));
        }
    }
    Ok(())
}

fn validate_windows(retention_seconds: u64, settle_seconds: u64) -> Result<(), Error> {
    if retention_seconds == 0 || settle_seconds == 0 {
        return Err(Error::InvalidState(
            "workspace retention and settle windows must be greater than zero".into(),
        ));
    }
    Ok(())
}

fn activation_recovery_error(error: Error, setup_path: &Path) -> Error {
    Error::InvalidState(format!(
        "managed setup completed but activation indexing failed: {}; recover or Undo with setup journal {}",
        sanitize_error(&error.to_string()),
        setup_path.display()
    ))
}

fn validate_state_outside_source(state: &Path, source: &Path) -> Result<(), Error> {
    let state = absolute_target(state)?;
    if state.starts_with(source) {
        return Err(Error::InvalidState(
            "managed state database must be outside the managed source".into(),
        ));
    }
    Ok(())
}

fn canonical_config_path(path: &Path, source: &Path) -> Result<PathBuf, Error> {
    let path = fs::canonicalize(path)
        .map_err(|error| io_error("resolve model configuration", path, error))?;
    let metadata = fs::symlink_metadata(&path)
        .map_err(|error| io_error("inspect model configuration", &path, error))?;
    if !metadata.is_file() {
        return Err(Error::InvalidState(format!(
            "model configuration is not a regular file: {}",
            path.display()
        )));
    }
    if path.starts_with(source) {
        return Err(Error::InvalidState(
            "model configuration must be outside the managed source".into(),
        ));
    }
    Config::load(&path)?;
    Ok(path)
}

fn absolute_target(path: &Path) -> Result<PathBuf, Error> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        std::env::current_dir()
            .map(|directory| directory.join(path))
            .map_err(|error| io_error("resolve", path, error))
    }
}

fn ensure_private_directory(path: &Path) -> Result<(), Error> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(Error::InvalidState(format!(
                "managed artifact path is not a real directory: {}",
                path.display()
            )));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(path)
                .map_err(|error| io_error("create managed artifact directory", path, error))?;
        }
        Err(error) => return Err(io_error("inspect managed artifact directory", path, error)),
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| io_error("secure managed artifact directory", path, error))
}

fn create_requested_run_directory(path: &Path, source: &Path) -> Result<PathBuf, Error> {
    let path = absolute_target(path)?;
    if path.starts_with(source) {
        return Err(Error::InvalidState(
            "managed artifact directory must be outside the managed source".into(),
        ));
    }
    fs::create_dir(&path).map_err(|error| io_error("create managed run", &path, error))?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
        .map_err(|error| io_error("secure managed run", &path, error))?;
    path.canonicalize()
        .map_err(|error| io_error("resolve managed run", &path, error))
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), Error> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::InvalidState("artifact path has no parent directory".into()))?;
    let mut temporary = NamedTempFile::new_in(parent)
        .map_err(|error| io_error("create temporary artifact", path, error))?;
    temporary
        .as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|error| io_error("secure temporary artifact", path, error))?;
    serde_json::to_writer_pretty(&mut temporary, value)?;
    temporary
        .write_all(b"\n")
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|error| io_error("write artifact", path, error))?;
    temporary
        .persist_noclobber(path)
        .map_err(|error| io_error("publish artifact", path, error.error))?;
    fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| io_error("sync artifact directory", parent, error))?;
    Ok(())
}

fn replace_json<T: Serialize>(path: &Path, value: &T) -> Result<(), Error> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::InvalidState("artifact path has no parent directory".into()))?;
    let mut temporary = NamedTempFile::new_in(parent)
        .map_err(|error| io_error("create temporary artifact", path, error))?;
    temporary
        .as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|error| io_error("secure temporary artifact", path, error))?;
    serde_json::to_writer_pretty(&mut temporary, value)?;
    temporary
        .write_all(b"\n")
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|error| io_error("write artifact", path, error))?;
    temporary
        .persist(path)
        .map_err(|error| io_error("replace artifact", path, error.error))?;
    fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| io_error("sync artifact directory", parent, error))
}

fn new_id(prefix: &str) -> Result<String, Error> {
    let sequence = ID_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    Ok(format!(
        "{prefix}-{}-{}-{sequence}",
        unix_ms()?,
        std::process::id()
    ))
}

fn unix_ms() -> Result<i64, Error> {
    let value = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| Error::InvalidState("system clock is before the Unix epoch".into()))?
        .as_millis();
    i64::try_from(value)
        .map_err(|_| Error::InvalidState("current timestamp does not fit in i64".into()))
}

fn to_u64_time(value: i64) -> Result<u64, Error> {
    u64::try_from(value)
        .map_err(|_| Error::InvalidState("timestamp is before the Unix epoch".into()))
}

fn path_text(path: &Path) -> Result<String, Error> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| Error::InvalidState(format!("path is not valid UTF-8: {}", path.display())))
}

fn io_error(action: &'static str, path: &Path, source: std::io::Error) -> Error {
    Error::FileSystem {
        action,
        path: path.display().to_string(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use std::{thread, time::Duration};

    use tempfile::tempdir;

    use super::*;
    use crate::{
        ContentPolicy, ExtractionConfig, FolderProposal, LocalRule, ModelConfig, PrivacyConfig,
        Proposal, ScanScope, build_managed_setup_plan,
    };

    #[test]
    fn rejects_a_managed_area_as_a_new_workspace_root() {
        let root = tempdir().unwrap();
        for name in MANAGED_AREAS {
            fs::create_dir(root.path().join(name)).unwrap();
        }
        assert!(validate_managed_workspace_root_candidate(&root.path().join("Recents")).is_err());
        fs::create_dir_all(root.path().join("Recents/nested/deeper")).unwrap();
        assert!(
            validate_managed_workspace_root_candidate(&root.path().join("Recents/nested/deeper"))
                .is_err()
        );
        assert!(validate_managed_workspace_root_candidate(&root.path().join("ordinary")).is_ok());
    }

    fn config() -> Config {
        Config {
            version: 4,
            model: ModelConfig {
                base_url: "http://127.0.0.1:11434/v1".into(),
                name: "unused-local-model".into(),
                allowed_hosts: vec!["127.0.0.1".into()],
                api_key: None,
                api_key_env: None,
            },
            privacy: PrivacyConfig {
                content: ContentPolicy::MetadataOnly,
                max_content_chars: 1_000,
                max_content_file_bytes: 10_000,
                extraction: ExtractionConfig {
                    max_output_bytes: 2_000,
                    max_archive_entries: 10,
                    max_expanded_bytes: 20_000,
                    max_xml_events: 1_000,
                    max_xml_depth: 20,
                    timeout_seconds: 2,
                    ocr: None,
                },
            },
        }
    }

    #[test]
    fn cycle_adopts_new_directories_and_respects_a_manually_returned_file() {
        let root = tempdir().unwrap();
        let source = root.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("baseline.txt"), b"baseline").unwrap();
        fs::create_dir(source.join("InitialManualDirectory")).unwrap();
        fs::write(
            source.join("InitialManualDirectory/note.txt"),
            b"initial manual",
        )
        .unwrap();
        let state = root.path().join("state.sqlite3");
        let config_path = root.path().join("config.toml");
        fs::write(&config_path, toml::to_string(&config()).unwrap()).unwrap();
        let folders = Proposal {
            version: 2,
            source: source.display().to_string(),
            scope: ScanScope::default(),
            files_considered: 1,
            folders: vec![FolderProposal {
                path: "Documents".into(),
                description: "Documents".into(),
            }],
        }
        .approve()
        .unwrap();
        let destination_id = folders.folders[0].id.clone();
        let setup = build_managed_setup_plan(&source).unwrap();
        let service = ManagedService::new(&state);
        let activation = service
            .activate_workspace(&setup, &folders, &config_path, 1, 1)
            .unwrap();
        assert_eq!(
            activation.workspace.config_path,
            config_path.canonicalize().unwrap().display().to_string()
        );
        let mut store = StateStore::open(&state).unwrap();
        store
            .insert_rule(
                &LocalRule {
                    id: "text-rule".into(),
                    monitor_id: activation.workspace.monitor_id.clone(),
                    name_glob: "*.txt".into(),
                    destination_id,
                    priority: 100,
                    enabled: true,
                },
                unix_ms().unwrap(),
            )
            .unwrap();
        drop(store);

        service
            .run_workspace(&activation.workspace.id, false)
            .unwrap();
        thread::sleep(Duration::from_millis(1_100));
        service
            .run_workspace(&activation.workspace.id, true)
            .unwrap();
        let classified = source.join("AI Library/Documents/baseline.txt");
        assert!(classified.is_file());

        fs::rename(&classified, source.join("baseline.txt")).unwrap();
        fs::rename(
            source.join("Manual Library/InitialManualDirectory"),
            source.join("InitialManualDirectory"),
        )
        .unwrap();
        fs::create_dir(source.join("NewManualDirectory")).unwrap();
        fs::write(source.join("NewManualDirectory/note.txt"), b"manual").unwrap();
        let cycle = service
            .run_workspace(&activation.workspace.id, false)
            .unwrap();

        let adoption = cycle.directory_adoption.unwrap();
        let indexed = service.apply_run(&adoption.run_id).unwrap();
        assert_eq!(indexed.kind, ManagedRunKind::Adopt);
        assert_eq!(indexed.state, RunState::Completed);
        assert!(source.join("baseline.txt").is_file());
        assert!(source.join("InitialManualDirectory/note.txt").is_file());
        assert!(
            source
                .join("Manual Library/NewManualDirectory/note.txt")
                .is_file()
        );
        assert!(!source.join("Recents/baseline.txt").exists());

        fs::rename(
            source.join("Manual Library/NewManualDirectory"),
            source.join("NewManualDirectory"),
        )
        .unwrap();
        let returned = service
            .run_workspace(&activation.workspace.id, true)
            .unwrap();
        assert!(returned.directory_adoption.is_none());
        assert!(source.join("NewManualDirectory/note.txt").is_file());

        fs::remove_dir_all(source.join("NewManualDirectory")).unwrap();
        fs::create_dir(source.join("NewManualDirectory")).unwrap();
        fs::write(
            source.join("NewManualDirectory/recreated.txt"),
            b"new identity",
        )
        .unwrap();
        let recreated = service
            .run_workspace(&activation.workspace.id, true)
            .unwrap();
        assert!(recreated.directory_adoption.is_some());
        assert!(
            source
                .join("Manual Library/NewManualDirectory/recreated.txt")
                .is_file()
        );

        fs::create_dir(source.join("UndoManualDirectory")).unwrap();
        fs::write(source.join("UndoManualDirectory/undo.txt"), b"undo").unwrap();
        let undo_cycle = service
            .run_workspace(&activation.workspace.id, true)
            .unwrap();
        let undo_adoption = undo_cycle.directory_adoption.unwrap();
        let undo_path = root.path().join("adoption-undo.json");
        service
            .undo_adoption_run(&undo_adoption.run_id, &undo_path)
            .unwrap();
        assert!(source.join("UndoManualDirectory/undo.txt").is_file());
        let after_undo = service
            .run_workspace(&activation.workspace.id, true)
            .unwrap();
        assert!(after_undo.directory_adoption.is_none());
        for area in ["Manual Library", "Recents", "AI Library"] {
            assert!(source.join(area).is_dir());
        }

        fs::write(indexed.apply_path.unwrap(), b"{}").unwrap();
        fs::create_dir(source.join("MustRemainAfterCorruption")).unwrap();
        assert!(
            service
                .run_workspace(&activation.workspace.id, true)
                .is_err()
        );
        assert!(source.join("MustRemainAfterCorruption").is_dir());
        assert!(
            !source
                .join("Manual Library/MustRemainAfterCorruption")
                .exists()
        );
    }

    #[test]
    fn activation_index_failure_returns_the_authoritative_recovery_journal() {
        let root = tempdir().unwrap();
        let source = root.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("report.txt"), b"report").unwrap();
        let state = root.path().join("state.sqlite3");
        let config_path = root.path().join("config.toml");
        fs::write(&config_path, toml::to_string(&config()).unwrap()).unwrap();
        let folders = Proposal {
            version: 2,
            source: source.display().to_string(),
            scope: ScanScope::default(),
            files_considered: 1,
            folders: vec![FolderProposal {
                path: "Documents".into(),
                description: "Documents".into(),
            }],
        }
        .approve()
        .unwrap();
        let setup = build_managed_setup_plan(&source).unwrap();
        let run_directory = root.path().join("activation");
        let error = ManagedService::new(&state)
            .activate_workspace_in(
                &setup,
                &folders,
                &config_path,
                u64::MAX,
                1,
                Some(&run_directory),
            )
            .unwrap_err();

        assert!(error.to_string().contains("activation indexing failed"));
        assert!(
            error.to_string().contains(
                &run_directory
                    .join("setup-session.json")
                    .display()
                    .to_string()
            )
        );
        assert_eq!(
            ManagedSetupSession::load(&run_directory.join("setup-session.json"))
                .unwrap()
                .state,
            ManagedSetupState::Completed
        );
        assert!(
            StateStore::open(&state)
                .unwrap()
                .managed_workspaces()
                .unwrap()
                .is_empty()
        );
        assert!(source.join("Manual Library").is_dir());
    }

    #[test]
    fn missing_authoritative_directory_journal_fails_before_adoption() {
        let root = tempdir().unwrap();
        let source = root.path().join("source");
        fs::create_dir(&source).unwrap();
        let state = root.path().join("state.sqlite3");
        let config_path = root.path().join("config.toml");
        fs::write(&config_path, toml::to_string(&config()).unwrap()).unwrap();
        let folders = Proposal {
            version: 2,
            source: source.display().to_string(),
            scope: ScanScope::default(),
            files_considered: 0,
            folders: vec![FolderProposal {
                path: "Documents".into(),
                description: "Documents".into(),
            }],
        }
        .approve()
        .unwrap();
        let setup = build_managed_setup_plan(&source).unwrap();
        let service = ManagedService::new(&state);
        let activation = service
            .activate_workspace(&setup, &folders, &config_path, 1, 1)
            .unwrap();
        let setup_path = activation.workspace.setup_session_path.unwrap();
        fs::remove_file(setup_path).unwrap();
        fs::create_dir(source.join("MustRemainAtRoot")).unwrap();

        assert!(
            service
                .run_workspace(&activation.workspace.id, true)
                .is_err()
        );
        assert!(source.join("MustRemainAtRoot").is_dir());
        assert!(!source.join("Manual Library/MustRemainAtRoot").exists());
    }

    #[test]
    fn library_edits_switch_immutable_bindings_without_moving_existing_files() {
        let root = tempdir().unwrap();
        let source = root.path().join("source");
        fs::create_dir(&source).unwrap();
        let state = root.path().join("state.sqlite3");
        let config_path = root.path().join("config.toml");
        fs::write(&config_path, toml::to_string(&config()).unwrap()).unwrap();
        let folders = Proposal {
            version: 2,
            source: source.display().to_string(),
            scope: ScanScope::default(),
            files_considered: 0,
            folders: vec![
                FolderProposal {
                    path: "Documents".into(),
                    description: "Documents".into(),
                },
                FolderProposal {
                    path: "Images".into(),
                    description: "Images".into(),
                },
            ],
        }
        .approve()
        .unwrap();
        let setup = build_managed_setup_plan(&source).unwrap();
        let service = ManagedService::new(&state);
        let activation = service
            .activate_workspace(&setup, &folders, &config_path, 1, 1)
            .unwrap();
        let mut store = StateStore::open(&state).unwrap();
        let workspace = store
            .set_managed_workspace_enabled(&activation.workspace.id, false, unix_ms().unwrap())
            .unwrap();
        let original = FolderSet::load(Path::new(&workspace.folder_set_path)).unwrap();
        let documents = original
            .folders
            .iter()
            .find(|folder| folder.path == "AI Library/Documents")
            .unwrap()
            .clone();
        let images_id = original
            .folders
            .iter()
            .find(|folder| folder.path == "AI Library/Images")
            .unwrap()
            .id
            .clone();
        fs::create_dir(source.join("AI Library/Documents")).unwrap();
        fs::write(source.join("AI Library/Documents/existing.txt"), "existing").unwrap();
        drop(store);

        let rename = service
            .preview_library_edit(
                &workspace.id,
                ManagedLibraryEdit::Rename {
                    id: documents.id.clone(),
                    path: "Archive".into(),
                },
            )
            .unwrap();
        let renamed = service.apply_library_edit(&rename).unwrap();
        let renamed_folders =
            FolderSet::load(Path::new(&renamed.workspace.folder_set_path)).unwrap();
        assert_ne!(renamed.workspace.folder_set_path, workspace.folder_set_path);
        assert_eq!(
            renamed_folders
                .folders
                .iter()
                .find(|folder| folder.id == documents.id)
                .unwrap()
                .path,
            "AI Library/Archive"
        );
        assert!(source.join("AI Library/Documents/existing.txt").is_file());
        assert!(!source.join("AI Library/Archive").exists());
        assert!(service.apply_library_edit(&rename).is_err());

        let session_path = Path::new(renamed.run.apply_path.as_deref().unwrap());
        let mut interrupted_session = ManagedLibraryEditSession::load(session_path).unwrap();
        interrupted_session.state = ManagedLibraryEditState::Running;
        interrupted_session.finished_unix_ms = None;
        replace_json(session_path, &interrupted_session).unwrap();
        let mut interrupted_run = renamed.run.clone();
        interrupted_run.state = RunState::Applying;
        interrupted_run.finished_unix_ms = None;
        StateStore::open(&state)
            .unwrap()
            .update_managed_run(&interrupted_run)
            .unwrap();
        assert_eq!(
            service
                .resume_library_edit(&interrupted_run.id)
                .unwrap()
                .run
                .state,
            RunState::Completed
        );

        let undo_path = Path::new(renamed.run.apply_path.as_deref().unwrap())
            .parent()
            .unwrap()
            .join("library-edit-undo.json");
        let undone = service
            .undo_library_edit(&renamed.run.id, &undo_path)
            .unwrap();
        let restored = FolderSet::load(Path::new(&undone.workspace.folder_set_path)).unwrap();
        assert_eq!(undone.workspace.folder_set_path, workspace.folder_set_path);
        assert_eq!(restored.sha256().unwrap(), original.sha256().unwrap());
        assert!(source.join("AI Library/Documents/existing.txt").is_file());

        let add = service
            .preview_library_edit(
                &workspace.id,
                ManagedLibraryEdit::Add {
                    path: "Research".into(),
                    description: "Research material".into(),
                },
            )
            .unwrap();
        let added_id = add
            .after_folders
            .folders
            .iter()
            .find(|folder| {
                !add.before_folders
                    .folders
                    .iter()
                    .any(|before| before.id == folder.id)
            })
            .unwrap()
            .id
            .clone();
        let added = service.apply_library_edit(&add).unwrap();
        assert!(!source.join("AI Library/Research").exists());

        let mut store = StateStore::open(&state).unwrap();
        store
            .insert_rule(
                &LocalRule {
                    id: "research-rule".into(),
                    monitor_id: workspace.monitor_id.clone(),
                    name_glob: "*.research".into(),
                    destination_id: added_id.clone(),
                    priority: 100,
                    enabled: true,
                },
                unix_ms().unwrap(),
            )
            .unwrap();
        drop(store);
        let delete = service
            .preview_library_edit(
                &workspace.id,
                ManagedLibraryEdit::Delete {
                    id: added_id.clone(),
                },
            )
            .unwrap();
        assert!(service.apply_library_edit(&delete).is_err());
        assert!(
            service
                .undo_library_edit(&added.run.id, &root.path().join("injected-undo.json"))
                .is_err()
        );
        let add_undo_path = Path::new(added.run.apply_path.as_deref().unwrap())
            .parent()
            .unwrap()
            .join("library-edit-undo.json");
        assert!(
            service
                .undo_library_edit(&added.run.id, &add_undo_path)
                .is_err()
        );
        assert_eq!(
            StateStore::open(&state)
                .unwrap()
                .managed_run(&added.run.id)
                .unwrap()
                .unwrap()
                .state,
            RunState::NeedsResume
        );
        fs::remove_file(&add_undo_path).unwrap();
        let mut store = StateStore::open(&state).unwrap();
        store
            .remove_rule("research-rule", unix_ms().unwrap())
            .unwrap();
        drop(store);
        let resumed = service.resume_library_edit_undo(&added.run.id).unwrap();
        assert!(
            FolderSet::load(Path::new(&resumed.workspace.folder_set_path))
                .unwrap()
                .folders
                .iter()
                .all(|folder| folder.id != added_id)
        );

        let delete = service
            .preview_library_edit(&workspace.id, ManagedLibraryEdit::Delete { id: images_id })
            .unwrap();
        service.apply_library_edit(&delete).unwrap();

        let mut store = StateStore::open(&state).unwrap();
        store
            .insert_managed_run(&ManagedRun {
                id: "unfinished-stage".into(),
                workspace_id: workspace.id.clone(),
                kind: ManagedRunKind::Stage,
                state: RunState::Planned,
                plan_path: None,
                apply_path: None,
                undo_path: None,
                started_unix_ms: unix_ms().unwrap(),
                finished_unix_ms: None,
                move_count: 0,
                error: None,
            })
            .unwrap();
        drop(store);
        let description = service
            .preview_library_edit(
                &workspace.id,
                ManagedLibraryEdit::EditDescription {
                    id: documents.id,
                    description: "Updated documents".into(),
                },
            )
            .unwrap();
        assert!(service.apply_library_edit(&description).is_err());
    }
}
