use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;
use temari_core::{
    Config, FileCandidate, FolderProposal, FolderProposer, FolderSet, OpenAiCompatibleModel,
    Proposal, ScanScope, scan_directory, select_representative_files,
};

const SCAN_PREVIEW_LIMIT: usize = 80;
const PROPOSAL_SAMPLE_LIMIT: usize = 100;

#[derive(Clone, Default)]
struct AppState {
    latest_proposal: Arc<Mutex<Option<Proposal>>>,
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

#[tauri::command]
fn select_source(app: AppHandle) -> Result<Option<String>, String> {
    app.dialog()
        .file()
        .set_title("Choose a folder to organize")
        .blocking_pick_folder()
        .map(|selected| {
            selected
                .into_path()
                .map_err(|error| format!("selected folder is not a local path: {error}"))
                .and_then(|path| path_to_string(&path))
        })
        .transpose()
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

    let proposal = tauri::async_runtime::spawn_blocking(move || generate_proposal(request))
        .await
        .map_err(|error| format!("folder proposal task failed: {error}"))??;
    let mut latest = state
        .latest_proposal
        .lock()
        .map_err(|_| "proposal state is unavailable".to_owned())?;
    *latest = Some(proposal.clone());
    Ok(proposal)
}

#[tauri::command]
fn approve_structure(
    request: ApproveRequest,
    state: State<'_, AppState>,
) -> Result<FolderSet, String> {
    let mut proposal = state
        .latest_proposal
        .lock()
        .map_err(|_| "proposal state is unavailable".to_owned())?
        .clone()
        .ok_or_else(|| "generate a proposal before approving it".to_owned())?;
    proposal.folders = request.folders;
    proposal.approve().map_err(error_text)
}

fn generate_proposal(request: ProposeRequest) -> Result<Proposal, String> {
    let config = Config::load(Path::new(&request.config_path)).map_err(error_text)?;
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

    Ok(Proposal {
        version: 2,
        source: path_to_string(&source)?,
        scope,
        files_considered: sample.len(),
        folders,
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

fn path_to_string(path: &Path) -> Result<String, String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("path is not valid UTF-8: {}", path.display()))
}

fn error_text(error: temari_core::Error) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use std::fs::File;

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
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            select_source,
            scan_source,
            propose_structure,
            approve_structure
        ])
        .run(tauri::generate_context!())
        .expect("failed to run Temari desktop");
}
