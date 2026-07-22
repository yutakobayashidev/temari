use std::{
    collections::{HashMap, HashSet},
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use tauri::State;
use temari_core::{
    ApplySession, Config, FolderProposal, FolderProposer, FolderSet, InboxState,
    ManagedCycleResult, ManagedLibraryEdit, ManagedLibraryEditPlan, ManagedReprocessArea,
    ManagedReprocessSelection, ManagedRun, ManagedRunKind, ManagedService, ManagedSetupPlan,
    ManagedSetupSession, ManagedSetupUndoSession, ManagedSetupUndoState, ManagedUndoMoveOutcome,
    ManagedWorkspace, OpenAiCompatibleModel, Proposal, RunState, ScanScope, SourceLock, StateStore,
    UndoMoveOutcome, UndoSession, build_managed_setup_plan, inbox_file_candidates, scan_directory,
    select_representative_files, undo_session_files_with_lock, undo_session_with_lock,
};
use temari_schedule::{
    ScheduleSpec, ScheduleStatus as CoreScheduleStatus, SchedulerPlatform, install_schedule,
    schedule_status, uninstall_schedule,
};

const PROPOSAL_SAMPLE_LIMIT: usize = 100;
static SETUP_TOKEN_NONCE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
struct ProposalDraft {
    token: String,
    proposal: Proposal,
    config_path: PathBuf,
}

#[derive(Clone)]
struct PreviewDraft {
    token: String,
    plan: ManagedSetupPlan,
    folders: FolderSet,
    config_path: PathBuf,
    retention_seconds: u64,
    settle_seconds: u64,
}

#[derive(Clone)]
struct LibraryEditDraft {
    token: String,
    plan: ManagedLibraryEditPlan,
}

#[derive(Default)]
struct ManagedDrafts {
    revision: u64,
    proposal: Option<ProposalDraft>,
    preview: Option<PreviewDraft>,
    library_edit: Option<LibraryEditDraft>,
}

impl ManagedDrafts {
    fn begin_proposal(&mut self) -> u64 {
        self.revision = self.revision.wrapping_add(1);
        self.proposal = None;
        self.preview = None;
        self.library_edit = None;
        self.revision
    }

    fn publish_proposal(&mut self, revision: u64, proposal: ProposalDraft) -> Result<(), String> {
        if self.revision != revision {
            return Err("a newer managed proposal request replaced this result".into());
        }
        self.proposal = Some(proposal);
        Ok(())
    }

    fn consume_preview(&mut self, token: &str) -> Result<PreviewDraft, String> {
        if self
            .preview
            .as_ref()
            .is_none_or(|preview| preview.token != token)
        {
            return Err("the reviewed managed preview is no longer active".into());
        }
        let preview = self.preview.take().expect("preview token was checked");
        self.revision = self.revision.wrapping_add(1);
        self.proposal = None;
        Ok(preview)
    }

    fn begin_library_edit(&mut self) -> u64 {
        self.revision = self.revision.wrapping_add(1);
        self.proposal = None;
        self.preview = None;
        self.library_edit = None;
        self.revision
    }

    fn consume_library_edit(&mut self, token: &str) -> Result<LibraryEditDraft, String> {
        if self
            .library_edit
            .as_ref()
            .is_none_or(|draft| draft.token != token)
        {
            return Err("the reviewed Library edit is no longer active".into());
        }
        self.revision = self.revision.wrapping_add(1);
        Ok(self
            .library_edit
            .take()
            .expect("Library edit token was checked"))
    }
}

#[derive(Clone, Default)]
pub(crate) struct ManagedAppState {
    drafts: Arc<Mutex<ManagedDrafts>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct WorkspaceIdRequest {
    pub workspace_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct ManagedProposeRequest {
    pub source: String,
    pub config_path: String,
    pub max_folders: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct ManagedPreviewRequest {
    pub proposal_token: String,
    pub folders: Vec<FolderProposal>,
    pub retention_seconds: u64,
    pub settle_seconds: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct ManagedApplyWorkspaceRequest {
    pub preview_token: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct ManagedRunRequest {
    pub workspace_id: String,
    pub apply: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct ManagedReprocessRequest {
    pub workspace_id: String,
    pub area: String,
    #[serde(default)]
    pub paths: Vec<String>,
    pub apply: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct ManagedScheduleRequest {
    pub workspace_id: String,
    #[serde(default)]
    pub every_seconds: Option<u32>,
    #[serde(default)]
    pub executable_path: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct ManagedHistoryRequest {
    pub workspace_id: String,
    #[serde(default = "default_history_limit")]
    pub limit: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct ManagedUndoRequest {
    pub workspace_id: String,
    pub session_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct ManagedUndoMoveRequest {
    pub workspace_id: String,
    pub session_id: String,
    pub move_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct ManagedLibraryEditPreviewRequest {
    pub workspace_id: String,
    pub operation: ManagedLibraryEdit,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct ManagedLibraryEditApplyRequest {
    pub preview_token: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct ManagedLibraryEditUndoRequest {
    pub workspace_id: String,
    pub run_id: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ManagedWorkspaceView {
    pub id: String,
    pub source: String,
    pub retention_seconds: u64,
    pub settle_seconds: u64,
    pub enabled: bool,
    pub created_unix_ms: i64,
    pub updated_unix_ms: i64,
}

impl From<ManagedWorkspace> for ManagedWorkspaceView {
    fn from(value: ManagedWorkspace) -> Self {
        Self {
            id: value.id,
            source: value.source,
            retention_seconds: value.retention_seconds,
            settle_seconds: value.settle_seconds,
            enabled: value.enabled,
            created_unix_ms: value.created_unix_ms,
            updated_unix_ms: value.updated_unix_ms,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ManagedRunView {
    pub id: String,
    pub kind: ManagedRunKind,
    pub state: RunState,
    pub move_count: u64,
    pub started_unix_ms: i64,
    pub finished_unix_ms: Option<i64>,
    pub error: Option<String>,
}

impl From<ManagedRun> for ManagedRunView {
    fn from(value: ManagedRun) -> Self {
        Self {
            id: value.id,
            kind: value.kind,
            state: value.state,
            move_count: value.move_count,
            started_unix_ms: value.started_unix_ms,
            finished_unix_ms: value.finished_unix_ms,
            error: value.error,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InboxSummaryView {
    pub physical_files: usize,
    pub indexed_pending: usize,
    pub indexed_planned: usize,
    pub indexed_moved: usize,
    pub eligible_now: usize,
    pub next_eligible_unix_ms: Option<i64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ManagedRunsView {
    pub total: usize,
    pub actionable: Vec<ManagedRunView>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LibraryFolderView {
    pub id: String,
    pub path: String,
    pub description: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LibraryConfigurationView {
    pub run_id: String,
    pub state: RunState,
    pub undone: bool,
    pub finished_unix_ms: Option<i64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ManagedWorkspaceStatus {
    pub health: String,
    pub issues: Vec<String>,
    pub workspace: ManagedWorkspaceView,
    pub inbox: InboxSummaryView,
    pub runs: ManagedRunsView,
    pub library_folders: Vec<LibraryFolderView>,
    pub latest_configuration: Option<LibraryConfigurationView>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LibraryEditPreviewView {
    pub token: String,
    pub operation: ManagedLibraryEdit,
    pub before_folders: Vec<LibraryFolderView>,
    pub after_folders: Vec<LibraryFolderView>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SetupProposalView {
    pub token: String,
    pub source: String,
    pub files_considered: usize,
    pub folders: Vec<FolderProposal>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SetupMoveView {
    pub source_path: String,
    pub destination_path: String,
    pub area: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SetupPreviewView {
    pub token: String,
    pub source: String,
    pub directories: Vec<String>,
    pub moves: Vec<SetupMoveView>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ManagedCycleView {
    pub workspace_id: String,
    pub artifact_directory: String,
    pub directory_adoption: Option<ManagedDirectoryAdoptionView>,
    pub runs: Vec<ManagedRunView>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ManagedDirectoryAdoptionView {
    pub plan_path: String,
    pub apply_path: Option<String>,
    pub move_count: u64,
}

impl From<ManagedCycleResult> for ManagedCycleView {
    fn from(value: ManagedCycleResult) -> Self {
        Self {
            workspace_id: value.workspace_id,
            artifact_directory: value.artifact_directory,
            directory_adoption: value.directory_adoption.map(|adoption| {
                ManagedDirectoryAdoptionView {
                    plan_path: adoption.plan_path,
                    apply_path: adoption.apply_path,
                    move_count: adoption.move_count,
                }
            }),
            runs: value.runs.into_iter().map(ManagedRunView::from).collect(),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ManagedMoveView {
    pub session_id: String,
    pub kind: ManagedRunKind,
    pub move_id: String,
    pub source_path: String,
    pub destination_path: String,
    pub undone: bool,
    pub undo_outcome: Option<String>,
    pub finished_unix_ms: Option<i64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ManagedUndoResult {
    pub run_id: String,
    pub state: String,
    pub restored_files: usize,
    pub conflicts: usize,
    pub journal_path: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ScheduleStatusView {
    pub platform: SchedulerPlatform,
    pub installed: bool,
    pub enabled: bool,
    pub active: bool,
    pub interval_seconds: Option<u32>,
}

fn default_history_limit() -> u32 {
    20
}

pub(crate) fn default_state_path() -> Result<PathBuf, String> {
    let directories = ProjectDirs::from("dev", "yutakobayashidev", "temari")
        .ok_or_else(|| "could not determine the user state directory".to_owned())?;
    Ok(directories
        .state_dir()
        .unwrap_or_else(|| directories.data_local_dir())
        .join("state.sqlite3"))
}

fn with_store<T>(
    operation: impl FnOnce(&mut StateStore) -> Result<T, String>,
) -> Result<T, String> {
    let path = default_state_path()?;
    let mut store = StateStore::open(&path)
        .map_err(|error| format!("could not open managed state {}: {error}", path.display()))?;
    operation(&mut store)
}

#[cfg(test)]
pub(crate) fn list_workspaces_at(path: &Path) -> Result<Vec<ManagedWorkspaceView>, String> {
    let store = StateStore::open(path).map_err(error_text)?;
    Ok(store
        .managed_workspaces()
        .map_err(error_text)?
        .into_iter()
        .map(ManagedWorkspaceView::from)
        .collect())
}

#[cfg(test)]
pub(crate) fn get_workspace_at(
    path: &Path,
    workspace_id: &str,
) -> Result<ManagedWorkspaceStatus, String> {
    let store = StateStore::open(path).map_err(error_text)?;
    workspace_status(&store, workspace_id)
}

#[cfg(test)]
pub(crate) fn set_workspace_enabled_at(
    path: &Path,
    workspace_id: &str,
    enabled: bool,
) -> Result<ManagedWorkspaceView, String> {
    let mut store = StateStore::open(path).map_err(error_text)?;
    require_workspace(&store, workspace_id)?;
    store
        .set_managed_workspace_enabled(workspace_id, enabled, unix_ms()?)
        .map(ManagedWorkspaceView::from)
        .map_err(error_text)
}

#[cfg(test)]
pub(crate) fn history_at(
    path: &Path,
    workspace_id: &str,
    limit: u32,
) -> Result<Vec<ManagedMoveView>, String> {
    let store = StateStore::open(path).map_err(error_text)?;
    require_workspace(&store, workspace_id)?;
    move_history(&store, workspace_id, limit)
}

#[tauri::command]
pub(crate) fn managed_list_workspaces() -> Result<Vec<ManagedWorkspaceView>, String> {
    with_store(|store| {
        Ok(store
            .managed_workspaces()
            .map_err(error_text)?
            .into_iter()
            .map(ManagedWorkspaceView::from)
            .collect())
    })
}

#[tauri::command]
pub(crate) fn managed_get_workspace(
    request: WorkspaceIdRequest,
) -> Result<ManagedWorkspaceStatus, String> {
    with_store(|store| workspace_status(store, &request.workspace_id))
}

#[tauri::command]
pub(crate) fn managed_set_workspace_enabled(
    request: WorkspaceIdRequest,
    enabled: bool,
) -> Result<ManagedWorkspaceView, String> {
    with_store(|store| {
        require_workspace(store, &request.workspace_id)?;
        store
            .set_managed_workspace_enabled(&request.workspace_id, enabled, unix_ms()?)
            .map(ManagedWorkspaceView::from)
            .map_err(error_text)
    })
}

#[tauri::command]
pub(crate) fn managed_history(
    request: ManagedHistoryRequest,
) -> Result<Vec<ManagedMoveView>, String> {
    with_store(|store| {
        require_workspace(store, &request.workspace_id)?;
        move_history(store, &request.workspace_id, request.limit)
    })
}

#[tauri::command]
pub(crate) async fn managed_undo_session(
    request: ManagedUndoRequest,
) -> Result<ManagedUndoResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        undo_managed(&request.workspace_id, &request.session_id, None)
    })
    .await
    .map_err(|error| format!("managed Undo task failed: {error}"))?
}

#[tauri::command]
pub(crate) async fn managed_undo_move(
    request: ManagedUndoMoveRequest,
) -> Result<ManagedUndoResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        undo_managed(
            &request.workspace_id,
            &request.session_id,
            Some(request.move_id),
        )
    })
    .await
    .map_err(|error| format!("managed Undo task failed: {error}"))?
}

#[tauri::command]
pub(crate) async fn managed_propose_workspace(
    request: ManagedProposeRequest,
    state: State<'_, ManagedAppState>,
) -> Result<SetupProposalView, String> {
    let revision = {
        let mut drafts = state
            .drafts
            .lock()
            .map_err(|_| "managed setup state is unavailable".to_owned())?;
        drafts.begin_proposal()
    };
    let token = setup_token("proposal", revision);
    let draft = tauri::async_runtime::spawn_blocking(move || propose_workspace(request, token))
        .await
        .map_err(|error| format!("managed proposal task failed: {error}"))??;
    let view = SetupProposalView {
        token: draft.token.clone(),
        source: draft.proposal.source.clone(),
        files_considered: draft.proposal.files_considered,
        folders: draft.proposal.folders.clone(),
    };
    let mut drafts = state
        .drafts
        .lock()
        .map_err(|_| "managed setup state is unavailable".to_owned())?;
    drafts.publish_proposal(revision, draft)?;
    Ok(view)
}

#[tauri::command]
pub(crate) fn managed_preview_workspace(
    request: ManagedPreviewRequest,
    state: State<'_, ManagedAppState>,
) -> Result<SetupPreviewView, String> {
    let mut drafts = state
        .drafts
        .lock()
        .map_err(|_| "managed setup state is unavailable".to_owned())?;
    let proposal = drafts
        .proposal
        .clone()
        .filter(|draft| draft.token == request.proposal_token)
        .ok_or_else(|| "the managed proposal is no longer active".to_owned())?;
    let revision = drafts.revision.wrapping_add(1);
    let preview = preview_workspace(proposal, request, setup_token("preview", revision))?;
    let view = preview_view(&preview)?;
    drafts.revision = revision;
    drafts.preview = Some(preview);
    Ok(view)
}

#[tauri::command]
pub(crate) async fn managed_apply_workspace(
    request: ManagedApplyWorkspaceRequest,
    state: State<'_, ManagedAppState>,
) -> Result<ManagedWorkspaceStatus, String> {
    let preview = {
        let mut drafts = state
            .drafts
            .lock()
            .map_err(|_| "managed setup state is unavailable".to_owned())?;
        drafts.consume_preview(&request.preview_token)?
    };
    let workspace_id = tauri::async_runtime::spawn_blocking(move || apply_workspace(preview))
        .await
        .map_err(|error| format!("managed setup task failed: {error}"))??;
    let status = with_store(|store| workspace_status(store, &workspace_id))?;
    Ok(status)
}

#[tauri::command]
pub(crate) async fn managed_preview_library_edit(
    request: ManagedLibraryEditPreviewRequest,
    state: State<'_, ManagedAppState>,
) -> Result<LibraryEditPreviewView, String> {
    let revision = {
        let mut drafts = state
            .drafts
            .lock()
            .map_err(|_| "managed setup state is unavailable".to_owned())?;
        drafts.begin_library_edit()
    };
    let token = setup_token("library-edit", revision);
    let operation = request.operation.clone();
    let plan = tauri::async_runtime::spawn_blocking(move || {
        managed_service()?
            .preview_library_edit(&request.workspace_id, request.operation)
            .map_err(error_text)
    })
    .await
    .map_err(|error| format!("Library edit preview task failed: {error}"))??;
    let view = LibraryEditPreviewView {
        token: token.clone(),
        operation,
        before_folders: editable_library_folders(&plan.before_folders),
        after_folders: editable_library_folders(&plan.after_folders),
    };
    let mut drafts = state
        .drafts
        .lock()
        .map_err(|_| "managed setup state is unavailable".to_owned())?;
    if drafts.revision != revision {
        return Err("a newer Library edit preview replaced this result".into());
    }
    drafts.library_edit = Some(LibraryEditDraft { token, plan });
    Ok(view)
}

#[tauri::command]
pub(crate) async fn managed_apply_library_edit(
    request: ManagedLibraryEditApplyRequest,
    state: State<'_, ManagedAppState>,
) -> Result<ManagedWorkspaceStatus, String> {
    let draft = {
        let mut drafts = state
            .drafts
            .lock()
            .map_err(|_| "managed setup state is unavailable".to_owned())?;
        drafts.consume_library_edit(&request.preview_token)?
    };
    let workspace_id = draft.plan.workspace_id.clone();
    tauri::async_runtime::spawn_blocking(move || {
        managed_service()?
            .apply_library_edit(&draft.plan)
            .map_err(error_text)
    })
    .await
    .map_err(|error| format!("Library edit Apply task failed: {error}"))??;
    with_store(|store| workspace_status(store, &workspace_id))
}

#[tauri::command]
pub(crate) async fn managed_undo_library_edit(
    request: ManagedLibraryEditUndoRequest,
) -> Result<ManagedWorkspaceStatus, String> {
    let (state_path, journal_path) = library_edit_undo_path(&request)?;
    let workspace_id = request.workspace_id;
    tauri::async_runtime::spawn_blocking(move || {
        ManagedService::new(&state_path)
            .undo_library_edit(&request.run_id, &journal_path)
            .map_err(error_text)
    })
    .await
    .map_err(|error| format!("Library edit Undo task failed: {error}"))??;
    with_store(|store| workspace_status(store, &workspace_id))
}

#[tauri::command]
pub(crate) async fn managed_resume_library_edit(
    request: ManagedLibraryEditUndoRequest,
) -> Result<ManagedWorkspaceStatus, String> {
    let state_path = default_state_path()?;
    let workspace_id = request.workspace_id;
    let status_workspace_id = workspace_id.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let store = StateStore::open(&state_path).map_err(error_text)?;
        let run = store
            .managed_run(&request.run_id)
            .map_err(error_text)?
            .ok_or_else(|| format!("unknown Configure run {:?}", request.run_id))?;
        if run.workspace_id != workspace_id {
            return Err("Configure run does not belong to the requested workspace".into());
        }
        let state = run.state;
        drop(store);
        let service = ManagedService::new(&state_path);
        match state {
            RunState::Applying => service
                .resume_library_edit(&request.run_id)
                .map(|_| ())
                .map_err(error_text),
            RunState::NeedsResume => service
                .resume_library_edit_undo(&request.run_id)
                .map(|_| ())
                .map_err(error_text),
            _ => Err("Configure run does not need Resume".into()),
        }
    })
    .await
    .map_err(|error| format!("Library edit Resume task failed: {error}"))??;
    with_store(|store| workspace_status(store, &status_workspace_id))
}

#[tauri::command]
pub(crate) async fn managed_run(request: ManagedRunRequest) -> Result<ManagedCycleView, String> {
    if !request.apply {
        return Err("desktop managed runs require explicit Apply confirmation".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let service = managed_service()?;
        service
            .run_workspace(&request.workspace_id, true)
            .map(ManagedCycleView::from)
            .map_err(error_text)
    })
    .await
    .map_err(|error| format!("managed run task failed: {error}"))?
}

#[tauri::command]
pub(crate) async fn managed_reprocess(
    request: ManagedReprocessRequest,
) -> Result<ManagedCycleView, String> {
    if !request.apply {
        return Err("desktop reprocessing requires explicit Apply confirmation".into());
    }
    let area = match request.area.as_str() {
        "kept" => ManagedReprocessArea::Kept,
        "library" => ManagedReprocessArea::Library,
        _ => return Err("reprocess area must be kept or library".into()),
    };
    if request.paths.is_empty() {
        return Err("choose at least one file or directory to reprocess".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let service = managed_service()?;
        service
            .reprocess(
                &request.workspace_id,
                area,
                &ManagedReprocessSelection::Paths(request.paths),
                true,
            )
            .map(ManagedCycleView::from)
            .map_err(error_text)
    })
    .await
    .map_err(|error| format!("managed reprocess task failed: {error}"))?
}

#[tauri::command]
pub(crate) async fn managed_schedule_status(
    request: ManagedScheduleRequest,
) -> Result<ScheduleStatusView, String> {
    tauri::async_runtime::spawn_blocking(move || {
        require_workspace_id(&request.workspace_id)?;
        schedule_status(&request.workspace_id, SchedulerPlatform::Auto)
            .map(|status| schedule_view(status, None))
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| format!("schedule status task failed: {error}"))?
}

#[tauri::command]
pub(crate) async fn managed_schedule_enable(
    request: ManagedScheduleRequest,
) -> Result<ScheduleStatusView, String> {
    tauri::async_runtime::spawn_blocking(move || enable_schedule(request))
        .await
        .map_err(|error| format!("schedule enable task failed: {error}"))?
}

#[tauri::command]
pub(crate) async fn managed_schedule_disable(
    request: ManagedScheduleRequest,
) -> Result<ScheduleStatusView, String> {
    tauri::async_runtime::spawn_blocking(move || {
        require_workspace_id(&request.workspace_id)?;
        uninstall_schedule(&request.workspace_id, SchedulerPlatform::Auto)
            .map(|status| schedule_view(status, None))
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| format!("schedule disable task failed: {error}"))?
}

fn enable_schedule(request: ManagedScheduleRequest) -> Result<ScheduleStatusView, String> {
    let executable = request.executable_path.ok_or_else(|| {
        "choose an absolute, stable Temari CLI executable before enabling a schedule".to_owned()
    })?;
    let interval = request
        .every_seconds
        .ok_or_else(|| "schedule interval is required".to_owned())?;
    let state_path = default_state_path()?;
    let store = StateStore::open(&state_path).map_err(error_text)?;
    let workspace = require_workspace(&store, &request.workspace_id)?;
    let config_path = PathBuf::from(&workspace.config_path);
    let config = Config::load(&config_path).map_err(error_text)?;
    if config.model.api_key_env.is_some() {
        return Err(
            "scheduled runs require an owner-only config with model.api_key instead of model.api_key_env"
                .into(),
        );
    }
    let spec = ScheduleSpec::new(
        &workspace.id,
        Path::new(&executable),
        &config_path,
        &state_path,
        Path::new(&workspace.source),
        interval,
    )
    .map_err(|error| error.to_string())?;
    install_schedule(&spec, SchedulerPlatform::Auto)
        .map(|status| schedule_view(status, Some(interval)))
        .map_err(|error| error.to_string())
}

fn schedule_view(status: CoreScheduleStatus, interval: Option<u32>) -> ScheduleStatusView {
    ScheduleStatusView {
        platform: status.platform,
        installed: status.installed,
        enabled: status.enabled,
        active: status.active,
        interval_seconds: interval,
    }
}

fn require_workspace_id(workspace_id: &str) -> Result<(), String> {
    with_store(|store| require_workspace(store, workspace_id).map(|_| ()))
}

fn workspace_status(
    store: &StateStore,
    workspace_id: &str,
) -> Result<ManagedWorkspaceStatus, String> {
    let workspace = require_workspace(store, workspace_id)?;
    let inbox = store.inbox_items(workspace_id).map_err(error_text)?;
    let runs = store.managed_runs(workspace_id).map_err(error_text)?;
    let now = unix_ms()?;
    let mut issues = Vec::new();
    let library_folders = match FolderSet::load(Path::new(&workspace.folder_set_path)) {
        Ok(folders)
            if folders.source == workspace.source
                && folders.sha256().map_err(error_text)? == workspace.folder_set_sha256 =>
        {
            editable_library_folders(&folders)
        }
        Ok(_) => {
            issues.push("Library FolderSet binding does not match the workspace".into());
            Vec::new()
        }
        Err(error) => {
            issues.push(format!("Library FolderSet could not be loaded: {error}"));
            Vec::new()
        }
    };
    let physical_files = match inbox_file_candidates(Path::new(&workspace.source)) {
        Ok(files) => files.len(),
        Err(error) => {
            issues.push(format!("Inbox scan failed: {error}"));
            0
        }
    };
    for area in ["Kept", "Inbox", "Library"] {
        let path = Path::new(&workspace.source).join(area);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(_) => issues.push(format!(
                "managed area is not a real directory: {}",
                path.display()
            )),
            Err(error) => issues.push(format!(
                "could not inspect managed area {}: {error}",
                path.display()
            )),
        }
    }
    let actionable = runs
        .iter()
        .filter(|run| {
            matches!(
                run.state,
                RunState::Planned | RunState::Applying | RunState::NeedsResume | RunState::Failed
            )
        })
        .cloned()
        .map(ManagedRunView::from)
        .collect::<Vec<_>>();
    if !actionable.is_empty() {
        issues.push(format!("{} run(s) need attention", actionable.len()));
    }
    let count_state = |state| inbox.iter().filter(|item| item.state == state).count();
    let eligible_now = inbox
        .iter()
        .filter(|item| item.state == InboxState::Pending && item.eligible_unix_ms <= now)
        .count();
    let next_eligible_unix_ms = inbox
        .iter()
        .filter(|item| item.state == InboxState::Pending && item.eligible_unix_ms > now)
        .map(|item| item.eligible_unix_ms)
        .min();
    let latest_configuration = runs
        .iter()
        .find(|run| run.kind == ManagedRunKind::Configure)
        .map(|run| LibraryConfigurationView {
            run_id: run.id.clone(),
            state: run.state,
            undone: run.undo_path.is_some() && run.state == RunState::Completed,
            finished_unix_ms: run.finished_unix_ms,
        });
    let health = if !issues.is_empty() {
        "attention"
    } else if !workspace.enabled {
        "disabled"
    } else {
        "healthy"
    };
    Ok(ManagedWorkspaceStatus {
        health: health.into(),
        issues,
        workspace: ManagedWorkspaceView::from(workspace),
        inbox: InboxSummaryView {
            physical_files,
            indexed_pending: count_state(InboxState::Pending),
            indexed_planned: count_state(InboxState::Planned),
            indexed_moved: count_state(InboxState::Moved),
            eligible_now,
            next_eligible_unix_ms,
        },
        runs: ManagedRunsView {
            total: runs.len(),
            actionable,
        },
        library_folders,
        latest_configuration,
    })
}

fn editable_library_folders(folders: &FolderSet) -> Vec<LibraryFolderView> {
    folders
        .folders
        .iter()
        .filter(|folder| folder.model_visible && folder.fallback.is_none())
        .map(|folder| LibraryFolderView {
            id: folder.id.clone(),
            path: folder
                .path
                .strip_prefix("Library/")
                .unwrap_or(&folder.path)
                .into(),
            description: folder.description.clone(),
        })
        .collect()
}

fn propose_workspace(
    request: ManagedProposeRequest,
    token: String,
) -> Result<ProposalDraft, String> {
    if request.max_folders == 0 {
        return Err("maximum folder count must be greater than zero".into());
    }
    let config_path = Path::new(&request.config_path)
        .canonicalize()
        .map_err(|error| format!("could not read model configuration: {error}"))?;
    if !config_path.is_file() {
        return Err("model configuration is not a regular file".into());
    }
    let config = Config::load(&config_path).map_err(error_text)?;
    let source = canonical_source(&request.source)?;
    reject_managed_area_source(&source)?;
    let files = scan_directory(&source, &ScanScope::default(), &[]).map_err(error_text)?;
    if files.is_empty() {
        return Err("no regular files were found in the selected folder".into());
    }
    let sample = select_representative_files(&files, PROPOSAL_SAMPLE_LIMIT);
    let model = OpenAiCompatibleModel::new(&config.model).map_err(error_text)?;
    let folders = model
        .propose_folders(&sample, request.max_folders)
        .map_err(error_text)?;
    Ok(ProposalDraft {
        token,
        proposal: Proposal {
            version: 2,
            source: path_text(&source)?,
            scope: ScanScope::default(),
            files_considered: sample.len(),
            folders,
        },
        config_path,
    })
}

pub(crate) fn reject_managed_area_source(source: &Path) -> Result<(), String> {
    const AREA_FAMILIES: [[&str; 3]; 2] = [
        ["Kept", "Inbox", "Library"],
        ["Manual Library", "Recents", "AI Library"],
    ];

    for ancestor in source.ancestors() {
        let Some(name) = ancestor.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        let Some(family) = AREA_FAMILIES.iter().find(|family| family.contains(&name)) else {
            continue;
        };
        let Some(parent) = ancestor.parent() else {
            continue;
        };
        if family
            .iter()
            .filter(|candidate| parent.join(candidate).is_dir())
            .count()
            >= 2
        {
            return Err(format!(
                "choose the workspace root instead of its managed area: {}",
                ancestor.display()
            ));
        }
    }
    Ok(())
}

fn preview_workspace(
    proposal: ProposalDraft,
    request: ManagedPreviewRequest,
    token: String,
) -> Result<PreviewDraft, String> {
    if request.retention_seconds == 0 || request.settle_seconds == 0 {
        return Err("retention and settle windows must be greater than zero".into());
    }
    let mut approved = proposal.proposal;
    approved.folders = request.folders;
    let folders = approved.approve().map_err(error_text)?;
    let plan = build_managed_setup_plan(Path::new(&folders.source)).map_err(error_text)?;
    Ok(PreviewDraft {
        token,
        plan,
        folders,
        config_path: proposal.config_path,
        retention_seconds: request.retention_seconds,
        settle_seconds: request.settle_seconds,
    })
}

fn preview_view(preview: &PreviewDraft) -> Result<SetupPreviewView, String> {
    let mut directories = vec!["Kept".into(), "Inbox".into(), "Library".into()];
    directories.extend(
        preview
            .folders
            .folders
            .iter()
            .map(|folder| format!("Library/{}", folder.path)),
    );
    let moves = preview
        .plan
        .moves
        .iter()
        .map(|movement| SetupMoveView {
            source_path: movement.source_path.clone(),
            destination_path: movement.destination_path.clone(),
            area: if movement.destination_path.starts_with("Kept/") {
                "kept".into()
            } else {
                "inbox".into()
            },
        })
        .collect();
    Ok(SetupPreviewView {
        token: preview.token.clone(),
        source: preview.plan.source.clone(),
        directories,
        moves,
    })
}

fn apply_workspace(preview: PreviewDraft) -> Result<String, String> {
    let service = managed_service()?;
    service
        .activate_workspace(
            &preview.plan,
            &preview.folders,
            &preview.config_path,
            preview.retention_seconds,
            preview.settle_seconds,
        )
        .map(|result| result.workspace.id)
        .map_err(error_text)
}

fn managed_service() -> Result<ManagedService, String> {
    Ok(ManagedService::new(default_state_path()?))
}

fn move_history(
    store: &StateStore,
    workspace_id: &str,
    limit: u32,
) -> Result<Vec<ManagedMoveView>, String> {
    let runs = store
        .recent_managed_moves(workspace_id, limit)
        .map_err(error_text)?;
    let mut moves = Vec::new();
    for run in runs {
        if run.kind == ManagedRunKind::Adopt {
            append_adoption_history(&run, &mut moves, limit)?;
            if moves.len() == limit as usize {
                return Ok(moves);
            }
            continue;
        }
        let apply_path = run
            .apply_path
            .as_deref()
            .ok_or_else(|| format!("managed session {:?} has no Apply journal", run.id))?;
        let apply = ApplySession::load(Path::new(apply_path)).map_err(error_text)?;
        let mut undo_paths = store
            .managed_undo_journal_paths(&run.id)
            .map_err(error_text)?;
        if let Some(path) = run.undo_path.as_ref()
            && !undo_paths.contains(path)
        {
            undo_paths.push(path.clone());
        }
        let mut undo_outcomes = HashMap::new();
        for path in undo_paths {
            for movement in UndoSession::load(Path::new(&path))
                .map_err(error_text)?
                .moves
            {
                undo_outcomes.insert(movement.file_id, movement.outcome);
            }
        }
        for movement in apply.moves {
            let undo_outcome = undo_outcomes
                .get(&movement.file_id)
                .map(undo_move_outcome_label);
            let undone = matches!(
                undo_outcomes.get(&movement.file_id),
                Some(UndoMoveOutcome::Restored | UndoMoveOutcome::AlreadyRestored)
            );
            moves.push(ManagedMoveView {
                session_id: run.id.clone(),
                kind: run.kind,
                move_id: movement.file_id,
                source_path: movement.source_path,
                destination_path: movement.destination_path,
                undone,
                undo_outcome,
                finished_unix_ms: run.finished_unix_ms,
            });
            if moves.len() == limit as usize {
                return Ok(moves);
            }
        }
    }
    Ok(moves)
}

fn append_adoption_history(
    run: &ManagedRun,
    moves: &mut Vec<ManagedMoveView>,
    limit: u32,
) -> Result<(), String> {
    let apply_path = run
        .apply_path
        .as_deref()
        .ok_or_else(|| format!("managed adoption {:?} has no Apply journal", run.id))?;
    let apply = ManagedSetupSession::load(Path::new(apply_path)).map_err(error_text)?;
    let undo = run
        .undo_path
        .as_deref()
        .map(|path| ManagedSetupUndoSession::load(Path::new(path)).map_err(error_text))
        .transpose()?;
    let undo_outcomes = undo
        .map(|session| {
            session
                .moves
                .into_iter()
                .map(|movement| {
                    (
                        (movement.source_path, movement.destination_path),
                        movement.outcome,
                    )
                })
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();
    for (index, movement) in apply.moves.into_iter().enumerate() {
        let outcome = undo_outcomes.get(&(
            movement.source_path.clone(),
            movement.destination_path.clone(),
        ));
        moves.push(ManagedMoveView {
            session_id: run.id.clone(),
            kind: ManagedRunKind::Adopt,
            move_id: format!("directory-{}", index + 1),
            source_path: movement.source_path,
            destination_path: movement.destination_path,
            undone: matches!(outcome, Some(ManagedUndoMoveOutcome::Restored)),
            undo_outcome: outcome.map(managed_undo_move_outcome_label),
            finished_unix_ms: run.finished_unix_ms,
        });
        if moves.len() == limit as usize {
            break;
        }
    }
    Ok(())
}

fn undo_move_outcome_label(outcome: &UndoMoveOutcome) -> String {
    match outcome {
        UndoMoveOutcome::Pending => "pending",
        UndoMoveOutcome::Restoring => "restoring",
        UndoMoveOutcome::Restored => "restored",
        UndoMoveOutcome::AlreadyRestored => "already_restored",
        UndoMoveOutcome::NotApplied => "not_applied",
        UndoMoveOutcome::Conflict { .. } => "conflict",
        UndoMoveOutcome::Failed { .. } => "failed",
    }
    .into()
}

fn managed_undo_move_outcome_label(outcome: &ManagedUndoMoveOutcome) -> String {
    match outcome {
        ManagedUndoMoveOutcome::Pending => "pending",
        ManagedUndoMoveOutcome::Restoring => "restoring",
        ManagedUndoMoveOutcome::Restored => "restored",
        ManagedUndoMoveOutcome::NotApplied => "not_applied",
        ManagedUndoMoveOutcome::Conflict { .. } => "conflict",
        ManagedUndoMoveOutcome::Failed { .. } => "failed",
    }
    .into()
}

fn undo_managed(
    workspace_id: &str,
    session_id: &str,
    move_id: Option<String>,
) -> Result<ManagedUndoResult, String> {
    let path = default_state_path()?;
    undo_managed_at(&path, workspace_id, session_id, move_id)
}

fn undo_managed_at(
    path: &Path,
    workspace_id: &str,
    session_id: &str,
    move_id: Option<String>,
) -> Result<ManagedUndoResult, String> {
    let mut store = StateStore::open(path).map_err(error_text)?;
    (|| {
        let workspace = require_workspace(&store, workspace_id)?;
        let run = store
            .managed_run(session_id)
            .map_err(error_text)?
            .ok_or_else(|| format!("unknown managed session {session_id:?}"))?;
        if run.workspace_id != workspace.id {
            return Err("managed session does not belong to the requested workspace".into());
        }
        if run.kind == ManagedRunKind::Adopt {
            if move_id.is_some() {
                return Err("directory adoption can be undone only as a complete session".into());
            }
            let journal_path = allocate_undo_path(&store, &workspace.id, session_id)?;
            drop(store);
            let undo = ManagedService::new(path)
                .undo_adoption_run(&run.id, &journal_path)
                .map_err(error_text)?;
            let restored_files = undo
                .moves
                .iter()
                .filter(|movement| movement.outcome == ManagedUndoMoveOutcome::Restored)
                .count();
            let conflicts = undo
                .moves
                .iter()
                .filter(|movement| {
                    matches!(
                        movement.outcome,
                        ManagedUndoMoveOutcome::Conflict { .. }
                            | ManagedUndoMoveOutcome::Failed { .. }
                    )
                })
                .count();
            return Ok(ManagedUndoResult {
                run_id: run.id,
                state: managed_setup_undo_state_label(&undo.state),
                restored_files,
                conflicts,
                journal_path: path_text(&journal_path)?,
            });
        }
        if run.state != RunState::Completed || run.kind == ManagedRunKind::Setup {
            return Err("only completed file-move sessions can be undone".into());
        }
        let apply_path = run
            .apply_path
            .as_deref()
            .ok_or_else(|| "managed session has no Apply journal".to_owned())?;
        let apply = ApplySession::load(Path::new(apply_path)).map_err(error_text)?;
        if let Some(id) = move_id.as_ref()
            && !apply.moves.iter().any(|movement| movement.file_id == *id)
        {
            return Err(format!(
                "unknown move {id:?} in managed session {session_id:?}"
            ));
        }
        let journal_path = allocate_undo_path(&store, &workspace.id, session_id)?;
        let lock = SourceLock::acquire(Path::new(&workspace.source)).map_err(error_text)?;
        let undo = match move_id {
            Some(id) => undo_session_files_with_lock(&apply, &[id], &journal_path, &lock),
            None => undo_session_with_lock(&apply, &journal_path, &lock),
        }
        .map_err(error_text)?;
        let restored = undo
            .moves
            .iter()
            .filter(|movement| {
                matches!(
                    movement.outcome,
                    UndoMoveOutcome::Restored | UndoMoveOutcome::AlreadyRestored
                )
            })
            .map(|movement| movement.file_id.as_str())
            .collect::<HashSet<_>>();
        let restored_identities = apply
            .moves
            .iter()
            .filter(|movement| restored.contains(movement.file_id.as_str()))
            .map(|movement| movement.fingerprint.identity.clone())
            .collect::<Vec<_>>();
        let journal = path_text(&journal_path)?;
        store
            .finalize_managed_undo(&run.id, &journal, &restored_identities, unix_ms()?)
            .map_err(error_text)?;
        let conflicts = undo
            .moves
            .iter()
            .filter(|movement| {
                matches!(
                    movement.outcome,
                    UndoMoveOutcome::Conflict { .. } | UndoMoveOutcome::Failed { .. }
                )
            })
            .count();
        Ok(ManagedUndoResult {
            run_id: run.id,
            state: undo_state_label(&undo.state),
            restored_files: restored.len(),
            conflicts,
            journal_path: journal,
        })
    })()
}

fn undo_state_label(state: &temari_core::UndoState) -> String {
    match state {
        temari_core::UndoState::Running => "running",
        temari_core::UndoState::Completed => "completed",
        temari_core::UndoState::PartialFailure => "partial_failure",
    }
    .into()
}

fn managed_setup_undo_state_label(state: &ManagedSetupUndoState) -> String {
    match state {
        ManagedSetupUndoState::Running => "running",
        ManagedSetupUndoState::Completed => "completed",
        ManagedSetupUndoState::PartialFailure => "partial_failure",
    }
    .into()
}

fn allocate_undo_path(
    store: &StateStore,
    workspace_id: &str,
    session_id: &str,
) -> Result<PathBuf, String> {
    let state = store
        .path()
        .ok_or_else(|| "managed state database has no filesystem path".to_owned())?;
    let parent = state
        .parent()
        .ok_or_else(|| "managed state database has no parent directory".to_owned())?;
    let directory = parent.join("managed-runs").join(workspace_id);
    fs::create_dir_all(&directory)
        .map_err(|error| format!("could not create managed Undo directory: {error}"))?;
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("could not secure managed Undo directory: {error}"))?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "system clock is before the Unix epoch".to_owned())?
        .as_millis();
    for suffix in 0..100_u8 {
        let name = if suffix == 0 {
            format!("undo-{session_id}-{now}.json")
        } else {
            format!("undo-{session_id}-{now}-{suffix}.json")
        };
        let path = directory.join(name);
        if !path.exists() {
            return Ok(path);
        }
    }
    Err("could not allocate a unique managed Undo journal".into())
}

fn library_edit_undo_path(
    request: &ManagedLibraryEditUndoRequest,
) -> Result<(PathBuf, PathBuf), String> {
    let state_path = default_state_path()?;
    let store = StateStore::open(&state_path).map_err(error_text)?;
    require_workspace(&store, &request.workspace_id)?;
    let run = store
        .managed_run(&request.run_id)
        .map_err(error_text)?
        .ok_or_else(|| format!("unknown Configure run {:?}", request.run_id))?;
    if run.workspace_id != request.workspace_id || run.kind != ManagedRunKind::Configure {
        return Err("Configure run does not belong to the requested workspace".into());
    }
    let apply_path = Path::new(
        run.apply_path
            .as_deref()
            .ok_or_else(|| "Configure run has no Apply Session".to_owned())?,
    );
    let parent = apply_path
        .parent()
        .ok_or_else(|| "Configure Apply Session has no parent directory".to_owned())?;
    Ok((state_path, parent.join("library-edit-undo.json")))
}

fn require_workspace(store: &StateStore, id: &str) -> Result<ManagedWorkspace, String> {
    store
        .managed_workspace(id)
        .map_err(error_text)?
        .ok_or_else(|| format!("unknown managed workspace {id:?}"))
}

fn unix_ms() -> Result<i64, String> {
    let value = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "system clock is before the Unix epoch".to_owned())?
        .as_millis();
    i64::try_from(value).map_err(|_| "current timestamp does not fit in i64".to_owned())
}

fn setup_token(prefix: &str, revision: u64) -> String {
    let nonce = SETUP_TOKEN_NONCE.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}-{}-{revision}-{nonce}", std::process::id())
}

fn path_text(path: &Path) -> Result<String, String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("path is not valid UTF-8: {}", path.display()))
}

fn canonical_source(source: &str) -> Result<PathBuf, String> {
    if source.trim().is_empty() {
        return Err("source folder must not be empty".into());
    }
    let path = Path::new(source)
        .canonicalize()
        .map_err(|error| format!("could not resolve source folder {source:?}: {error}"))?;
    if !path.is_dir() {
        return Err(format!("source is not a directory: {}", path.display()));
    }
    path_text(&path)?;
    Ok(path)
}

fn error_text(error: temari_core::Error) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use temari_core::{
        FsIdentity, MonitorRecord, apply_plan, build_stage_to_inbox_plan, root_file_candidates,
    };
    use tempfile::tempdir;

    fn insert_workspace(path: &Path, source: &Path) -> ManagedWorkspace {
        let mut store = StateStore::open(path).unwrap();
        let source = source.canonicalize().unwrap();
        let identity = temari_core::canonical_source_identity(&source).unwrap().1;
        let folders = source.parent().unwrap().join("folders.json");
        fs::write(&folders, "{}").unwrap();
        let config = source.parent().unwrap().join("config.toml");
        fs::write(&config, "version = 4\n").unwrap();
        let now = 1_000;
        let monitor = MonitorRecord {
            id: "monitor-1".into(),
            source: source.display().to_string(),
            source_identity: identity.clone(),
            folder_set_path: folders.display().to_string(),
            folder_set_sha256: "a".repeat(64),
            interval_seconds: 300,
            enabled: true,
            last_checked_unix_ms: None,
            created_unix_ms: now,
            updated_unix_ms: now,
            deleted_unix_ms: None,
        };
        store.insert_monitor(&monitor).unwrap();
        let workspace = ManagedWorkspace {
            id: "workspace-1".into(),
            monitor_id: monitor.id,
            source: monitor.source,
            source_identity: identity,
            folder_set_path: monitor.folder_set_path,
            folder_set_sha256: monitor.folder_set_sha256,
            config_path: config.display().to_string(),
            retention_seconds: 259_200,
            settle_seconds: 30,
            enabled: true,
            setup_session_path: None,
            created_unix_ms: now,
            updated_unix_ms: now,
        };
        store.insert_managed_workspace(&workspace).unwrap();
        workspace
    }

    fn proposal_draft(source: &Path, token: &str) -> ProposalDraft {
        ProposalDraft {
            token: token.into(),
            proposal: Proposal {
                version: 2,
                source: source.display().to_string(),
                scope: ScanScope::default(),
                files_considered: 1,
                folders: vec![FolderProposal {
                    path: "Documents".into(),
                    description: "Documents".into(),
                }],
            },
            config_path: source.join("config.toml"),
        }
    }

    fn preview_draft(source: &Path, token: &str) -> PreviewDraft {
        fs::write(source.join("loose.txt"), "loose").unwrap();
        let proposal = proposal_draft(source, "proposal");
        let folders = proposal.proposal.clone().approve().unwrap();
        PreviewDraft {
            token: token.into(),
            plan: build_managed_setup_plan(source).unwrap(),
            folders,
            config_path: proposal.config_path,
            retention_seconds: 259_200,
            settle_seconds: 30,
        }
    }

    fn write_valid_config(path: &Path) {
        fs::write(
            path,
            r#"version = 4

[model]
base_url = "http://127.0.0.1:11434/v1"
name = "unused-local-model"
allowed_hosts = ["127.0.0.1"]

[privacy]
content = "metadata_only"
max_content_chars = 1000
max_content_file_bytes = 10000

[privacy.extraction]
max_output_bytes = 2000
max_archive_entries = 10
max_expanded_bytes = 20000
max_xml_events = 1000
max_xml_depth = 20
timeout_seconds = 2
"#,
        )
        .unwrap();
    }

    #[test]
    fn setup_tokens_are_unique_within_the_process() {
        let tokens = (0..1_000)
            .map(|revision| setup_token("preview", revision))
            .collect::<HashSet<_>>();
        assert_eq!(tokens.len(), 1_000);
    }

    #[test]
    fn managed_areas_and_their_descendants_are_not_setup_sources() {
        let root = tempdir().unwrap();
        for area in ["Manual Library", "Recents", "AI Library"] {
            fs::create_dir(root.path().join(area)).unwrap();
        }
        let nested = root.path().join("Recents/nested");
        fs::create_dir(&nested).unwrap();

        assert!(reject_managed_area_source(&root.path().join("Recents")).is_err());
        assert!(reject_managed_area_source(&nested).is_err());
        assert!(reject_managed_area_source(root.path()).is_ok());
    }

    #[test]
    fn ordinary_folder_with_a_reserved_name_remains_selectable() {
        let root = tempdir().unwrap();
        let ordinary = root.path().join("Recents");
        fs::create_dir(&ordinary).unwrap();

        assert!(reject_managed_area_source(&ordinary).is_ok());
    }

    #[test]
    fn out_of_order_proposal_results_cannot_replace_the_latest_request() {
        let source = tempdir().unwrap();
        let mut drafts = ManagedDrafts::default();
        let first_revision = drafts.begin_proposal();
        let second_revision = drafts.begin_proposal();

        assert!(
            drafts
                .publish_proposal(first_revision, proposal_draft(source.path(), "first"))
                .is_err()
        );
        drafts
            .publish_proposal(second_revision, proposal_draft(source.path(), "second"))
            .unwrap();
        assert_eq!(drafts.proposal.unwrap().token, "second");
    }

    #[test]
    fn exact_latest_preview_is_consumed_once_without_stale_token_damage() {
        let source = tempdir().unwrap();
        let mut drafts = ManagedDrafts {
            revision: 4,
            proposal: Some(proposal_draft(source.path(), "proposal")),
            preview: Some(preview_draft(source.path(), "latest")),
            ..ManagedDrafts::default()
        };

        assert!(drafts.consume_preview("stale").is_err());
        assert_eq!(drafts.preview.as_ref().unwrap().token, "latest");
        let consumed = drafts.consume_preview("latest").unwrap();
        assert_eq!(consumed.token, "latest");
        assert!(drafts.preview.is_none());
        assert!(drafts.proposal.is_none());
        assert!(drafts.consume_preview("latest").is_err());
    }

    #[test]
    fn list_get_and_toggle_share_the_cli_state_database() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("source");
        fs::create_dir(&source).unwrap();
        let state = directory.path().join("state.sqlite3");
        insert_workspace(&state, &source);

        let listed = list_workspaces_at(&state).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "workspace-1");

        let status = get_workspace_at(&state, "workspace-1").unwrap();
        assert_eq!(status.inbox.physical_files, 0);
        assert_eq!(status.runs.total, 0);

        let disabled = set_workspace_enabled_at(&state, "workspace-1", false).unwrap();
        assert!(!disabled.enabled);
        let reopened = StateStore::open(&state).unwrap();
        assert!(!reopened.monitor("monitor-1").unwrap().unwrap().enabled);
    }

    #[test]
    fn requests_reject_unexpected_filesystem_authority() {
        let request = serde_json::json!({
            "workspaceId": "workspace-1",
            "sessionId": "run-1",
            "moveId": "f000001",
            "journalPath": "/tmp/injected.json"
        });
        assert!(serde_json::from_value::<ManagedUndoMoveRequest>(request).is_err());
    }

    #[test]
    fn history_rejects_zero_limit_and_unknown_workspaces() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("source");
        fs::create_dir(&source).unwrap();
        let state = directory.path().join("state.sqlite3");
        insert_workspace(&state, &source);

        assert!(history_at(&state, "workspace-1", 0).is_err());
        assert!(history_at(&state, "missing", 20).is_err());
    }

    #[test]
    fn source_validation_rejects_a_file() {
        let directory = tempdir().unwrap();
        let file = directory.path().join("not-a-folder.txt");
        fs::write(&file, "file").unwrap();

        assert!(canonical_source(file.to_str().unwrap()).is_err());
    }

    #[test]
    fn individual_undo_restores_the_move_and_updates_history() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::create_dir(source.join("Inbox")).unwrap();
        fs::write(source.join("report.txt"), "report").unwrap();
        let state = directory.path().join("state.sqlite3");
        insert_workspace(&state, &source);

        let candidates = root_file_candidates(&source).unwrap();
        let plan = build_stage_to_inbox_plan(&source, &candidates).unwrap();
        let apply_path = directory.path().join("apply.json");
        let apply = apply_plan(&plan, &apply_path).unwrap();
        let move_id = apply.moves[0].file_id.clone();
        let mut store = StateStore::open(&state).unwrap();
        store
            .insert_managed_run(&ManagedRun {
                id: "run-1".into(),
                workspace_id: "workspace-1".into(),
                kind: ManagedRunKind::Stage,
                state: RunState::Completed,
                plan_path: None,
                apply_path: Some(apply_path.display().to_string()),
                undo_path: None,
                started_unix_ms: 2_000,
                finished_unix_ms: Some(3_000),
                move_count: 1,
                error: None,
            })
            .unwrap();
        drop(store);

        let result =
            undo_managed_at(&state, "workspace-1", "run-1", Some(move_id.clone())).unwrap();

        assert_eq!(result.state, "completed");
        assert_eq!(result.restored_files, 1);
        assert!(source.join("report.txt").is_file());
        assert!(!source.join("Inbox/report.txt").exists());
        let history = history_at(&state, "workspace-1", 20).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].move_id, move_id);
        assert!(history[0].undone);
    }

    #[test]
    fn adoption_history_supports_session_undo_only() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("source");
        fs::create_dir(&source).unwrap();
        let state = directory.path().join("state.sqlite3");
        let config_path = directory.path().join("config.toml");
        write_valid_config(&config_path);
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
        let service = ManagedService::new(&state);
        let activation = service
            .activate_workspace(&setup, &folders, &config_path, 259_200, 30)
            .unwrap();

        fs::create_dir(source.join("NewManualDirectory")).unwrap();
        fs::write(source.join("NewManualDirectory/note.txt"), "manual").unwrap();
        let cycle = service
            .run_workspace(&activation.workspace.id, true)
            .unwrap();
        let adoption = cycle.directory_adoption.unwrap();

        let history = history_at(&state, &activation.workspace.id, 20).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].session_id, adoption.run_id);
        assert_eq!(history[0].kind, ManagedRunKind::Adopt);
        assert!(!history[0].undone);
        assert!(source.join("Kept/NewManualDirectory/note.txt").is_file());
        assert!(
            undo_managed_at(
                &state,
                &activation.workspace.id,
                &adoption.run_id,
                Some(history[0].move_id.clone()),
            )
            .unwrap_err()
            .contains("complete session")
        );

        let result =
            undo_managed_at(&state, &activation.workspace.id, &adoption.run_id, None).unwrap();

        assert_eq!(result.state, "completed");
        assert_eq!(result.restored_files, 1);
        assert_eq!(result.conflicts, 0);
        assert!(source.join("NewManualDirectory/note.txt").is_file());
        assert!(!source.join("Kept/NewManualDirectory").exists());
        let history = history_at(&state, &activation.workspace.id, 20).unwrap();
        assert_eq!(history.len(), 1);
        assert!(history[0].undone);
        assert_eq!(history[0].undo_outcome.as_deref(), Some("restored"));
    }

    #[test]
    fn library_edit_token_is_single_use_and_status_exposes_configure_undo() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("source");
        fs::create_dir(&source).unwrap();
        let state_path = directory.path().join("state.sqlite3");
        let config_path = directory.path().join("config.toml");
        write_valid_config(&config_path);
        let folders = Proposal {
            version: 2,
            source: source.display().to_string(),
            scope: ScanScope::default(),
            files_considered: 1,
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
        let service = ManagedService::new(&state_path);
        let activation = service
            .activate_workspace(
                &build_managed_setup_plan(&source).unwrap(),
                &folders,
                &config_path,
                259_200,
                30,
            )
            .unwrap();
        StateStore::open(&state_path)
            .unwrap()
            .set_managed_workspace_enabled(&activation.workspace.id, false, unix_ms().unwrap())
            .unwrap();
        let plan = service
            .preview_library_edit(
                &activation.workspace.id,
                ManagedLibraryEdit::Add {
                    path: "Research".into(),
                    description: "Research material".into(),
                },
            )
            .unwrap();
        let mut drafts = ManagedDrafts::default();
        drafts.begin_library_edit();
        drafts.library_edit = Some(LibraryEditDraft {
            token: "reviewed".into(),
            plan,
        });
        assert!(drafts.consume_library_edit("stale").is_err());
        let reviewed = drafts.consume_library_edit("reviewed").unwrap();
        assert!(drafts.consume_library_edit("reviewed").is_err());

        let applied = service.apply_library_edit(&reviewed.plan).unwrap();
        let status = get_workspace_at(&state_path, &activation.workspace.id).unwrap();
        assert!(
            status
                .library_folders
                .iter()
                .any(|folder| folder.path == "Research")
        );
        assert_eq!(
            status.latest_configuration.as_ref().unwrap().run_id,
            applied.run.id
        );
        let undo_path = Path::new(applied.run.apply_path.as_deref().unwrap())
            .parent()
            .unwrap()
            .join("library-edit-undo.json");
        service
            .undo_library_edit(&applied.run.id, &undo_path)
            .unwrap();
        let status = get_workspace_at(&state_path, &activation.workspace.id).unwrap();
        assert!(
            !status
                .library_folders
                .iter()
                .any(|folder| folder.path == "Research")
        );
        assert!(status.latest_configuration.unwrap().undone);
    }

    #[test]
    fn filesystem_identity_type_remains_serializable_in_workspace_status() {
        let identity = FsIdentity {
            device: 1,
            inode: 2,
        };
        assert_eq!(serde_json::to_value(identity).unwrap()["device"], 1);
    }
}
