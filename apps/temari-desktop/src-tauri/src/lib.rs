use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use tauri::State;
use temari_core::{
    ApplySession, ApplyState, ClassificationOptions, Classifier, Config, ContentDecision,
    ContentExtractor, ContentPolicy, DirectoryOutcome, FileCandidate, FolderProposal,
    FolderProposer, FolderSet, LocalContentExtractor, MoveOutcome, OpenAiCompatibleModel, Plan,
    Proposal, ScanScope, UndoDirectoryOutcome, UndoMoveOutcome, UndoSession, UndoState, apply_plan,
    build_plan, classify_file_names, complete_classification, scan_directory,
    select_representative_files, undo_session,
};

const SCAN_PREVIEW_LIMIT: usize = 80;
const PROPOSAL_SAMPLE_LIMIT: usize = 100;

#[derive(Clone)]
struct ProposalState {
    proposal: Proposal,
    config: Config,
}

#[derive(Clone)]
struct ApprovedState {
    folders: FolderSet,
    config: Config,
}

#[derive(Clone)]
struct PlannedState {
    plan: Plan,
    sha256: String,
}

#[derive(Clone)]
struct AppliedState {
    session: ApplySession,
    run_directory: PathBuf,
    plan_path: PathBuf,
    apply_path: PathBuf,
    undo_path: PathBuf,
    undo: Option<UndoSession>,
}

#[derive(Clone, Copy, Default, Eq, PartialEq)]
enum Operation {
    #[default]
    Idle,
    Applying,
    Undoing,
}

#[derive(Default)]
struct WorkflowState {
    revision: u64,
    proposal: Option<ProposalState>,
    approved: Option<ApprovedState>,
    planned: Option<PlannedState>,
    applied: Option<AppliedState>,
    operation: Operation,
}

#[derive(Clone, Default)]
struct AppState {
    workflow: Arc<Mutex<WorkflowState>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ScanRequest {
    source: String,
    #[serde(default)]
    recursive_roots: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ScanPreview {
    source: String,
    scope: ScanScope,
    file_count: usize,
    sampled_files: Vec<FileCandidate>,
    extension_counts: BTreeMap<String, usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ProposeRequest {
    config_path: String,
    source: String,
    #[serde(default)]
    recursive_roots: Vec<String>,
    max_folders: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApproveRequest {
    folders: Vec<FolderProposal>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PlanPreview {
    plan: Plan,
    sha256: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ApplyRequest {
    plan_sha256: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ApplyResult {
    state: ApplyState,
    session_id: String,
    plan_sha256: String,
    planned_files: usize,
    moved_files: usize,
    created_directories: usize,
    conflicts: usize,
    run_directory: String,
    plan_path: String,
    journal_path: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct UndoRequest {
    apply_session_id: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UndoResult {
    state: UndoState,
    apply_session_id: String,
    restored_files: usize,
    removed_directories: usize,
    conflicts: usize,
    journal_path: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ConfigLocation {
    path: Option<String>,
    default_path: String,
}

#[tauri::command]
fn default_config_location() -> Result<ConfigLocation, String> {
    let directories = ProjectDirs::from("dev", "yutakobayashidev", "temari")
        .ok_or_else(|| "could not determine the user configuration directory".to_owned())?;
    let default_path = directories.config_dir().join("config.toml");
    let path = if default_path.is_file() {
        Some(path_to_string(&default_path.canonicalize().map_err(
            |error| {
                format!(
                    "could not read model configuration {:?}: {error}",
                    default_path.display().to_string()
                )
            },
        )?)?)
    } else {
        None
    };
    Ok(ConfigLocation {
        path,
        default_path: path_to_string(&default_path)?,
    })
}

#[tauri::command]
fn scan_source(request: ScanRequest) -> Result<ScanPreview, String> {
    let source = canonical_source(&request.source)?;
    let scope = ScanScope::new(request.recursive_roots).map_err(error_text)?;
    let files = scan_directory(&source, &scope, &[]).map_err(error_text)?;
    let sampled_files = select_representative_files(&files, SCAN_PREVIEW_LIMIT);
    let mut extension_counts = BTreeMap::new();
    for file in &files {
        let label = if file.extension.is_empty() {
            "(none)"
        } else {
            &file.extension
        };
        *extension_counts.entry(label.to_owned()).or_insert(0) += 1;
    }

    Ok(ScanPreview {
        source: path_to_string(&source)?,
        scope,
        file_count: files.len(),
        sampled_files,
        extension_counts,
    })
}

#[tauri::command]
async fn propose_structure(
    request: ProposeRequest,
    state: State<'_, AppState>,
) -> Result<Proposal, String> {
    if request.max_folders == 0 {
        return Err("maximum folder count must be greater than zero".into());
    }

    let revision = {
        let mut workflow = state
            .workflow
            .lock()
            .map_err(|_| "workflow state is unavailable".to_owned())?;
        require_idle(&workflow)?;
        workflow.revision = workflow.revision.wrapping_add(1);
        workflow.proposal = None;
        workflow.approved = None;
        workflow.planned = None;
        workflow.applied = None;
        workflow.revision
    };

    let proposal_state = tauri::async_runtime::spawn_blocking(move || generate_proposal(request))
        .await
        .map_err(|error| format!("folder proposal task failed: {error}"))??;
    let proposal = proposal_state.proposal.clone();
    let mut workflow = state
        .workflow
        .lock()
        .map_err(|_| "workflow state is unavailable".to_owned())?;
    require_idle(&workflow)?;
    if workflow.revision != revision {
        return Err("the workflow changed while the proposal was being created".into());
    }
    workflow.proposal = Some(proposal_state);
    Ok(proposal)
}

#[tauri::command]
fn approve_structure(
    request: ApproveRequest,
    state: State<'_, AppState>,
) -> Result<FolderSet, String> {
    let mut workflow = state
        .workflow
        .lock()
        .map_err(|_| "workflow state is unavailable".to_owned())?;
    require_idle(&workflow)?;
    let proposal_state = workflow
        .proposal
        .clone()
        .ok_or_else(|| "generate a proposal before approving it".to_owned())?;
    let mut proposal = proposal_state.proposal;
    proposal.folders = request.folders;
    let folders = proposal.approve().map_err(error_text)?;
    workflow.approved = Some(ApprovedState {
        folders: folders.clone(),
        config: proposal_state.config,
    });
    workflow.planned = None;
    workflow.applied = None;
    workflow.revision = workflow.revision.wrapping_add(1);
    Ok(folders)
}

#[tauri::command]
async fn preview_plan(state: State<'_, AppState>) -> Result<PlanPreview, String> {
    let (approved, revision) = {
        let workflow = state
            .workflow
            .lock()
            .map_err(|_| "workflow state is unavailable".to_owned())?;
        require_idle(&workflow)?;
        let approved = workflow
            .approved
            .clone()
            .ok_or_else(|| "approve destinations before creating a plan".to_owned())?;
        (approved, workflow.revision)
    };
    let preview = tauri::async_runtime::spawn_blocking(move || generate_plan(approved))
        .await
        .map_err(|error| format!("plan preview task failed: {error}"))??;
    let mut workflow = state
        .workflow
        .lock()
        .map_err(|_| "workflow state is unavailable".to_owned())?;
    require_idle(&workflow)?;
    if workflow.revision != revision || workflow.applied.is_some() {
        return Err("the workflow changed while the Plan was being created".into());
    }
    workflow.planned = Some(PlannedState {
        plan: preview.plan.clone(),
        sha256: preview.sha256.clone(),
    });
    workflow.applied = None;
    Ok(preview)
}

#[tauri::command]
async fn apply_reviewed_plan(
    request: ApplyRequest,
    state: State<'_, AppState>,
) -> Result<ApplyResult, String> {
    let planned = {
        let mut workflow = state
            .workflow
            .lock()
            .map_err(|_| "workflow state is unavailable".to_owned())?;
        if workflow.operation != Operation::Idle {
            return Err("another filesystem operation is already running".into());
        }
        let planned = reviewed_plan(&workflow, &request.plan_sha256)?;
        workflow.operation = Operation::Applying;
        planned
    };

    let result = tauri::async_runtime::spawn_blocking(move || apply_planned(planned))
        .await
        .map_err(|error| format!("apply task failed: {error}"))
        .and_then(|result| result);
    let mut workflow = state
        .workflow
        .lock()
        .map_err(|_| "workflow state is unavailable".to_owned())?;
    workflow.operation = Operation::Idle;
    match result {
        Ok(applied) => {
            let response = summarize_apply(&applied)?;
            workflow.applied = Some(applied);
            Ok(response)
        }
        Err(error) => Err(error),
    }
}

#[tauri::command]
async fn undo_applied_plan(
    request: UndoRequest,
    state: State<'_, AppState>,
) -> Result<UndoResult, String> {
    let applied = {
        let mut workflow = state
            .workflow
            .lock()
            .map_err(|_| "workflow state is unavailable".to_owned())?;
        if workflow.operation != Operation::Idle {
            return Err("another filesystem operation is already running".into());
        }
        let applied = workflow
            .applied
            .clone()
            .ok_or_else(|| "there is no applied desktop session to undo".to_owned())?;
        if applied.session.id != request.apply_session_id {
            return Err("the requested apply session is not the active desktop session".into());
        }
        if applied.undo.is_some() {
            return Err("the active desktop session has already been undone".into());
        }
        workflow.operation = Operation::Undoing;
        applied
    };

    let result = tauri::async_runtime::spawn_blocking(move || undo_applied(applied))
        .await
        .map_err(|error| format!("undo task failed: {error}"))
        .and_then(|result| result);
    let mut workflow = state
        .workflow
        .lock()
        .map_err(|_| "workflow state is unavailable".to_owned())?;
    workflow.operation = Operation::Idle;
    match result {
        Ok((applied, response)) => {
            workflow.applied = Some(applied);
            Ok(response)
        }
        Err(error) => Err(error),
    }
}

fn generate_proposal(request: ProposeRequest) -> Result<ProposalState, String> {
    let config_path = canonical_config(&request.config_path)?;
    let config = Config::load(&config_path).map_err(error_text)?;
    let source = canonical_source(&request.source)?;
    let scope = ScanScope::new(request.recursive_roots).map_err(error_text)?;
    let files = scan_directory(&source, &scope, &[]).map_err(error_text)?;
    if files.is_empty() {
        return Err("no regular files were found in the selected scope".into());
    }
    let sample = select_representative_files(&files, PROPOSAL_SAMPLE_LIMIT);
    let model = OpenAiCompatibleModel::new(&config.model).map_err(error_text)?;
    let folders = model
        .propose_folders(&sample, request.max_folders)
        .map_err(error_text)?;

    Ok(ProposalState {
        proposal: Proposal {
            version: 2,
            source: path_to_string(&source)?,
            scope,
            files_considered: sample.len(),
            folders,
        },
        config,
    })
}

fn generate_plan(approved: ApprovedState) -> Result<PlanPreview, String> {
    let model = OpenAiCompatibleModel::new(&approved.config.model).map_err(error_text)?;
    let extractor = LocalContentExtractor::new(approved.config.privacy.extraction.clone());
    generate_plan_with(&approved.folders, &approved.config, &model, &extractor)
}

fn generate_plan_with<C: Classifier, E: ContentExtractor>(
    folders: &FolderSet,
    config: &Config,
    classifier: &C,
    extractor: &E,
) -> Result<PlanPreview, String> {
    folders.validate().map_err(error_text)?;
    let source = canonical_source(&folders.source)?;
    let excluded: Vec<_> = folders
        .folders
        .iter()
        .map(|folder| folder.path.clone())
        .collect();
    let files = scan_directory(&source, &folders.scope, &excluded).map_err(error_text)?;
    let batch_delay = Duration::from_millis(500);
    let name_pass = classify_file_names(&files, &folders.folders, classifier, 50, batch_delay)
        .map_err(error_text)?;
    let content_decision = match config.privacy.content {
        ContentPolicy::OnDemand => ContentDecision::Extract,
        ContentPolicy::Ask | ContentPolicy::MetadataOnly => ContentDecision::Fallback,
    };
    let summary = complete_classification(
        &source,
        &files,
        &folders.folders,
        classifier,
        extractor,
        ClassificationOptions {
            content_decision,
            max_content_chars: config.privacy.max_content_chars,
            max_content_file_bytes: config.privacy.max_content_file_bytes,
            content_batch_size: 20,
            batch_delay,
        },
        name_pass,
    )
    .map_err(error_text)?;
    let plan = build_plan(
        &source,
        &folders.scope,
        &files,
        &folders.folders,
        summary.classifications,
    )
    .map_err(error_text)?;
    let sha256 = plan.sha256().map_err(error_text)?;
    Ok(PlanPreview { plan, sha256 })
}

fn apply_planned(planned: PlannedState) -> Result<AppliedState, String> {
    let workflow_root = desktop_workflow_root()?;
    apply_planned_at(planned, &workflow_root)
}

fn reviewed_plan(workflow: &WorkflowState, plan_sha256: &str) -> Result<PlannedState, String> {
    if workflow.applied.is_some() {
        return Err("the reviewed plan has already been applied".into());
    }
    let planned = workflow
        .planned
        .clone()
        .ok_or_else(|| "preview a plan before applying it".to_owned())?;
    if plan_sha256 != planned.sha256 {
        return Err("the confirmed plan no longer matches the reviewed plan".into());
    }
    if planned.plan.entries.is_empty() {
        return Err("the reviewed plan contains no moves".into());
    }
    Ok(planned)
}

fn require_idle(workflow: &WorkflowState) -> Result<(), String> {
    if workflow.operation == Operation::Idle {
        Ok(())
    } else {
        Err("another filesystem operation is already running".into())
    }
}

fn apply_planned_at(planned: PlannedState, workflow_root: &Path) -> Result<AppliedState, String> {
    let source = canonical_source(&planned.plan.source)?;
    fs::create_dir_all(workflow_root)
        .map_err(|error| format!("could not create desktop workflow directory: {error}"))?;
    fs::set_permissions(workflow_root, fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("could not secure desktop workflow directory: {error}"))?;
    let workflow_root = workflow_root
        .canonicalize()
        .map_err(|error| format!("could not resolve desktop workflow directory: {error}"))?;
    if workflow_root.starts_with(&source) {
        return Err("desktop workflow journals must be outside the organized source".into());
    }

    let run_directory = create_run_directory(&workflow_root, &planned.sha256)?;
    let plan_path = run_directory.join("plan.json");
    let apply_path = run_directory.join("apply.json");
    let undo_path = run_directory.join("undo.json");
    write_new_json(&plan_path, &planned.plan)?;
    let session = apply_plan(&planned.plan, &apply_path).map_err(|error| {
        format!(
            "apply failed: {error}. The reviewed Plan is saved at {} and any recovery journal is at {}",
            plan_path.display(),
            apply_path.display()
        )
    })?;

    Ok(AppliedState {
        session,
        run_directory,
        plan_path,
        apply_path,
        undo_path,
        undo: None,
    })
}

fn undo_applied(mut applied: AppliedState) -> Result<(AppliedState, UndoResult), String> {
    let undo = undo_session(&applied.session, &applied.undo_path).map_err(|error| {
        format!(
            "undo failed: {error}. Inspect the apply journal at {} and undo journal at {}",
            applied.apply_path.display(),
            applied.undo_path.display()
        )
    })?;
    let response = summarize_undo(&undo, &applied.undo_path)?;
    applied.undo = Some(undo);
    Ok((applied, response))
}

fn desktop_workflow_root() -> Result<PathBuf, String> {
    let directories = ProjectDirs::from("dev", "yutakobayashidev", "temari")
        .ok_or_else(|| "could not determine the user state directory".to_owned())?;
    Ok(directories
        .state_dir()
        .unwrap_or_else(|| directories.data_local_dir())
        .join("workflows"))
}

fn create_run_directory(root: &Path, plan_sha256: &str) -> Result<PathBuf, String> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "system clock is before the Unix epoch".to_owned())?
        .as_millis();
    let digest = plan_sha256.get(..12).unwrap_or(plan_sha256);
    for suffix in 0..100_u8 {
        let name = if suffix == 0 {
            format!("{timestamp}-{}-{digest}", std::process::id())
        } else {
            format!("{timestamp}-{}-{digest}-{suffix}", std::process::id())
        };
        let path = root.join(name);
        match fs::create_dir(&path) {
            Ok(()) => {
                fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
                    .map_err(|error| format!("could not secure desktop workflow run: {error}"))?;
                return Ok(path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!("could not create desktop workflow run: {error}"));
            }
        }
    }
    Err("could not allocate a unique desktop workflow run".into())
}

fn write_new_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let temporary = path.with_extension("json.tmp");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(|error| format!("could not create Plan artifact: {error}"))?;
    serde_json::to_writer_pretty(&mut file, value)
        .map_err(|error| format!("could not serialize Plan artifact: {error}"))?;
    file.write_all(b"\n")
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("could not persist Plan artifact: {error}"))?;
    fs::rename(&temporary, path)
        .map_err(|error| format!("could not publish Plan artifact: {error}"))?;
    sync_workflow_directory(path.parent().expect("artifact path has a parent"))?;
    Ok(())
}

fn sync_workflow_directory(directory: &Path) -> Result<(), String> {
    fs::File::open(directory)
        .and_then(|file| file.sync_all())
        .map_err(|error| format!("could not sync workflow directory: {error}"))
}

fn summarize_apply(applied: &AppliedState) -> Result<ApplyResult, String> {
    let moved_files = applied
        .session
        .moves
        .iter()
        .filter(|record| record.outcome == MoveOutcome::Moved)
        .count();
    let created_directories = applied
        .session
        .directories
        .iter()
        .filter(|record| matches!(record.outcome, DirectoryOutcome::Created { .. }))
        .count();
    let move_conflicts = applied
        .session
        .moves
        .iter()
        .filter(|record| {
            matches!(
                record.outcome,
                MoveOutcome::Conflict { .. } | MoveOutcome::Failed { .. }
            )
        })
        .count();
    let directory_conflicts = applied
        .session
        .directories
        .iter()
        .filter(|record| {
            matches!(
                record.outcome,
                DirectoryOutcome::Conflict { .. } | DirectoryOutcome::Failed { .. }
            )
        })
        .count();
    Ok(ApplyResult {
        state: applied.session.state.clone(),
        session_id: applied.session.id.clone(),
        plan_sha256: applied.session.plan_sha256.clone(),
        planned_files: applied.session.moves.len(),
        moved_files,
        created_directories,
        conflicts: move_conflicts + directory_conflicts,
        run_directory: path_to_string(&applied.run_directory)?,
        plan_path: path_to_string(&applied.plan_path)?,
        journal_path: path_to_string(&applied.apply_path)?,
    })
}

fn summarize_undo(undo: &UndoSession, journal_path: &Path) -> Result<UndoResult, String> {
    let restored_files = undo
        .moves
        .iter()
        .filter(|record| {
            matches!(
                record.outcome,
                UndoMoveOutcome::Restored | UndoMoveOutcome::AlreadyRestored
            )
        })
        .count();
    let removed_directories = undo
        .directories
        .iter()
        .filter(|record| record.outcome == UndoDirectoryOutcome::Removed)
        .count();
    let move_conflicts = undo
        .moves
        .iter()
        .filter(|record| {
            matches!(
                record.outcome,
                UndoMoveOutcome::Conflict { .. } | UndoMoveOutcome::Failed { .. }
            )
        })
        .count();
    let directory_conflicts = undo
        .directories
        .iter()
        .filter(|record| {
            matches!(
                record.outcome,
                UndoDirectoryOutcome::Conflict { .. } | UndoDirectoryOutcome::Failed { .. }
            )
        })
        .count();
    Ok(UndoResult {
        state: undo.state.clone(),
        apply_session_id: undo.apply_session_id.clone(),
        restored_files,
        removed_directories,
        conflicts: move_conflicts + directory_conflicts,
        journal_path: path_to_string(journal_path)?,
    })
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
    path_to_string(&path)?;
    Ok(path)
}

fn canonical_config(config_path: &str) -> Result<PathBuf, String> {
    let requested = Path::new(config_path.trim());
    if !requested.is_absolute() {
        return Err("choose the model configuration file in the app".into());
    }
    let canonical = requested
        .canonicalize()
        .map_err(|error| format!("could not read model configuration {config_path:?}: {error}"))?;
    if !canonical.is_file() {
        return Err(format!(
            "model configuration is not a regular file: {}",
            canonical.display()
        ));
    }
    Ok(canonical)
}

fn path_to_string(path: &Path) -> Result<String, String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("path is not valid UTF-8: {}", path.display()))
}

fn error_text(error: temari_core::Error) -> String {
    error.to_string()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            default_config_location,
            scan_source,
            propose_structure,
            approve_structure,
            preview_plan,
            apply_reviewed_plan,
            undo_applied_plan
        ])
        .run(tauri::generate_context!())
        .expect("failed to run Temari desktop");
}

#[cfg(test)]
mod tests {
    use std::fs::File;

    use temari_core::{
        ApprovedFolder, Classification, ClassificationBasis, ContentCandidate, ExtractionConfig,
        ModelConfig, NameClassification, NameDecision, PrivacyConfig,
    };
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn scan_preview_uses_the_real_scope_rules() {
        let source = tempdir().unwrap();
        std::fs::create_dir(source.path().join("included")).unwrap();
        std::fs::create_dir(source.path().join("ignored")).unwrap();
        File::create(source.path().join("root.txt")).unwrap();
        File::create(source.path().join("included/report.pdf")).unwrap();
        File::create(source.path().join("ignored/private.txt")).unwrap();

        let preview = scan_source(ScanRequest {
            source: source.path().display().to_string(),
            recursive_roots: vec!["included".into()],
        })
        .unwrap();

        assert_eq!(preview.file_count, 2);
        assert_eq!(preview.extension_counts.get("pdf"), Some(&1));
        assert_eq!(preview.extension_counts.get("txt"), Some(&1));
        assert!(
            preview
                .sampled_files
                .iter()
                .all(|file| !file.source_path.starts_with("ignored/"))
        );
    }

    #[test]
    fn canonical_source_rejects_a_file() {
        let directory = tempdir().unwrap();
        let file = directory.path().join("not-a-folder.txt");
        File::create(&file).unwrap();

        assert!(canonical_source(file.to_str().unwrap()).is_err());
    }

    #[test]
    fn config_path_must_be_an_absolute_regular_file() {
        let directory = tempdir().unwrap();
        let config = directory.path().join("temari.toml");
        std::fs::write(&config, "version = 4").unwrap();

        assert_eq!(canonical_config(config.to_str().unwrap()).unwrap(), config);
        assert!(canonical_config(".temari.toml").is_err());
        assert!(canonical_config(directory.path().to_str().unwrap()).is_err());
    }

    #[test]
    fn default_config_location_uses_the_platform_config_directory() {
        let location = default_config_location().unwrap();

        assert!(Path::new(&location.default_path).is_absolute());
        assert!(location.default_path.ends_with("temari/config.toml"));
    }

    struct NameClassifier {
        destination_id: Option<String>,
    }

    impl Classifier for NameClassifier {
        fn classify_names(
            &self,
            files: &[FileCandidate],
            _folders: &[ApprovedFolder],
        ) -> Result<Vec<NameClassification>, temari_core::Error> {
            Ok(files
                .iter()
                .map(|file| NameClassification {
                    file_id: file.id.clone(),
                    decision: self.destination_id.as_ref().map_or(
                        NameDecision::NeedsContent,
                        |destination_id| NameDecision::Destination {
                            destination_id: destination_id.clone(),
                        },
                    ),
                    reasoning: None,
                })
                .collect())
        }

        fn classify_contents(
            &self,
            _files: &[ContentCandidate],
            _folders: &[ApprovedFolder],
        ) -> Result<Vec<Classification>, temari_core::Error> {
            panic!("content classification must not run in these tests")
        }
    }

    struct NeverExtract;

    impl ContentExtractor for NeverExtract {
        fn extract(
            &self,
            _source: &Path,
            _file: &FileCandidate,
            _max_chars: usize,
            _max_file_bytes: u64,
        ) -> Option<ContentCandidate> {
            panic!("content extraction must not run in these tests")
        }
    }

    fn test_config(content: ContentPolicy) -> Config {
        Config {
            version: 4,
            model: ModelConfig {
                base_url: "http://127.0.0.1:4000/v1".into(),
                name: "test-model".into(),
                allowed_hosts: Vec::new(),
                api_key: None,
                api_key_env: None,
            },
            privacy: PrivacyConfig {
                content,
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

    fn approved_folders(source: &Path) -> FolderSet {
        Proposal {
            version: 2,
            source: source.display().to_string(),
            scope: ScanScope::default(),
            files_considered: 1,
            folders: vec![FolderProposal {
                path: "Documents".into(),
                description: "General documents".into(),
            }],
        }
        .approve()
        .unwrap()
    }

    fn reviewed_test_plan(source: &Path) -> PlannedState {
        std::fs::write(source.join("loose.txt"), "loose").unwrap();
        let folders = approved_folders(source);
        let destination_id = folders
            .folders
            .iter()
            .find(|folder| folder.path == "Documents")
            .unwrap()
            .id
            .clone();
        let preview = generate_plan_with(
            &folders,
            &test_config(ContentPolicy::MetadataOnly),
            &NameClassifier {
                destination_id: Some(destination_id),
            },
            &NeverExtract,
        )
        .unwrap();
        PlannedState {
            plan: preview.plan,
            sha256: preview.sha256,
        }
    }

    #[test]
    fn plan_preview_builds_a_real_plan_and_excludes_approved_destinations() {
        let source = tempdir().unwrap();
        std::fs::write(source.path().join("loose.txt"), "loose").unwrap();
        std::fs::create_dir(source.path().join("Documents")).unwrap();
        std::fs::write(
            source.path().join("Documents/already.txt"),
            "already sorted",
        )
        .unwrap();
        let folders = approved_folders(source.path());
        let destination_id = folders
            .folders
            .iter()
            .find(|folder| folder.path == "Documents")
            .unwrap()
            .id
            .clone();

        let preview = generate_plan_with(
            &folders,
            &test_config(ContentPolicy::MetadataOnly),
            &NameClassifier {
                destination_id: Some(destination_id),
            },
            &NeverExtract,
        )
        .unwrap();

        assert_eq!(preview.plan.version, 4);
        assert_eq!(preview.plan.entries.len(), 1);
        assert_eq!(preview.plan.entries[0].source_path, "loose.txt");
        assert_eq!(
            preview.plan.entries[0].destination_path,
            "Documents/loose.txt"
        );
        assert_eq!(preview.sha256, preview.plan.sha256().unwrap());
        assert!(source.path().join("loose.txt").exists());
    }

    #[test]
    fn ask_policy_falls_back_without_reading_content() {
        let source = tempdir().unwrap();
        std::fs::write(source.path().join("ambiguous.pdf"), "private text").unwrap();
        let folders = approved_folders(source.path());

        let preview = generate_plan_with(
            &folders,
            &test_config(ContentPolicy::Ask),
            &NameClassifier {
                destination_id: None,
            },
            &NeverExtract,
        )
        .unwrap();

        assert_eq!(preview.plan.entries.len(), 1);
        assert_eq!(
            preview.plan.entries[0].classification_basis,
            ClassificationBasis::ExtensionFallback
        );
        assert_eq!(
            preview.plan.entries[0].destination_path,
            "Others/PDFs/ambiguous.pdf"
        );
    }

    #[test]
    fn apply_uses_the_exact_backend_held_plan_and_undo_restores_it() {
        let source = tempdir().unwrap();
        let journals = tempdir().unwrap();
        let planned = reviewed_test_plan(source.path());
        let expected_sha256 = planned.sha256.clone();
        let mut workflow = WorkflowState {
            planned: Some(planned),
            ..WorkflowState::default()
        };

        assert!(reviewed_plan(&workflow, "wrong-digest").is_err());
        assert!(source.path().join("loose.txt").exists());

        let confirmed = reviewed_plan(&workflow, &expected_sha256).unwrap();
        let applied = apply_planned_at(confirmed, journals.path()).unwrap();
        assert_eq!(applied.session.state, ApplyState::Completed);
        assert!(source.path().join("Documents/loose.txt").exists());
        assert!(!source.path().join("loose.txt").exists());
        assert_eq!(
            applied.plan_path.metadata().unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            applied.apply_path.metadata().unwrap().permissions().mode() & 0o777,
            0o600
        );
        let apply_bytes = std::fs::read(&applied.apply_path).unwrap();
        workflow.applied = Some(applied.clone());
        assert!(reviewed_plan(&workflow, &expected_sha256).is_err());

        let (applied, undo_result) = undo_applied(applied).unwrap();
        assert_eq!(undo_result.state, UndoState::Completed);
        assert_eq!(undo_result.restored_files, 1);
        assert!(source.path().join("loose.txt").exists());
        assert!(!source.path().join("Documents/loose.txt").exists());
        assert_eq!(std::fs::read(&applied.apply_path).unwrap(), apply_bytes);
        assert!(applied.undo_path.is_file());
    }

    #[test]
    fn apply_rejects_a_file_changed_after_preview() {
        let source = tempdir().unwrap();
        let journals = tempdir().unwrap();
        let planned = reviewed_test_plan(source.path());
        std::fs::write(source.path().join("loose.txt"), "changed after preview").unwrap();

        let error = apply_planned_at(planned, journals.path()).err().unwrap();

        assert!(error.contains("source changed after planning"));
        assert!(source.path().join("loose.txt").exists());
        assert!(!source.path().join("Documents/loose.txt").exists());
    }

    #[test]
    fn apply_ipc_rejects_unexpected_fields() {
        let request = serde_json::json!({
            "planSha256": "reviewed",
            "source": "/tmp/injected"
        });

        assert!(serde_json::from_value::<ApplyRequest>(request).is_err());
    }
}
