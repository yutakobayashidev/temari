use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use tauri::State;
use temari_core::{
    ClassificationOptions, Classifier, Config, ContentDecision, ContentExtractor, ContentPolicy,
    FileCandidate, FolderProposal, FolderProposer, FolderSet, LocalContentExtractor,
    OpenAiCompatibleModel, Plan, Proposal, ScanScope, build_plan, classify_file_names,
    complete_classification, scan_directory, select_representative_files,
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

#[derive(Default)]
struct WorkflowState {
    proposal: Option<ProposalState>,
    approved: Option<ApprovedState>,
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

    {
        let mut workflow = state
            .workflow
            .lock()
            .map_err(|_| "workflow state is unavailable".to_owned())?;
        workflow.proposal = None;
        workflow.approved = None;
    }

    let proposal_state = tauri::async_runtime::spawn_blocking(move || generate_proposal(request))
        .await
        .map_err(|error| format!("folder proposal task failed: {error}"))??;
    let proposal = proposal_state.proposal.clone();
    let mut workflow = state
        .workflow
        .lock()
        .map_err(|_| "workflow state is unavailable".to_owned())?;
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
    Ok(folders)
}

#[tauri::command]
async fn preview_plan(state: State<'_, AppState>) -> Result<PlanPreview, String> {
    let approved = state
        .workflow
        .lock()
        .map_err(|_| "workflow state is unavailable".to_owned())?
        .approved
        .clone()
        .ok_or_else(|| "approve destinations before creating a plan".to_owned())?;
    tauri::async_runtime::spawn_blocking(move || generate_plan(approved))
        .await
        .map_err(|error| format!("plan preview task failed: {error}"))?
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
            preview_plan
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
}
