use std::{
    collections::HashMap,
    fs::{self, File},
    io::Write,
    os::unix::fs::PermissionsExt,
    path::Path,
    time::Duration,
};

use serde::Serialize;
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

use crate::{
    ApplySession, ApplyState, ClassificationOptions, Classifier, Config, ContentDecision,
    ContentExtractor, ContentPolicy, Error, FileFingerprint, FolderSet, LocalRule, MonitorRecord,
    Plan, RuleSet, RunState, SourceLock, StagedFileRecord, StateStore, apply_plan_with_lock,
    build_plan, canonical_source_identity, classify_file_names, complete_classification,
    fingerprint_candidate, scan_directory,
};

#[derive(Clone, Copy, Debug)]
pub struct MonitoringOptions {
    pub content_policy: ContentPolicy,
    pub max_content_chars: usize,
    pub max_content_file_bytes: u64,
    pub name_batch_size: usize,
    pub content_batch_size: usize,
    pub batch_delay: Duration,
}

impl MonitoringOptions {
    pub fn from_config(config: &Config) -> Self {
        Self {
            content_policy: config.privacy.content,
            max_content_chars: config.privacy.max_content_chars,
            max_content_file_bytes: config.privacy.max_content_file_bytes,
            name_batch_size: 50,
            content_batch_size: 20,
            batch_delay: Duration::from_millis(500),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MonitoringStats {
    pub total_files: usize,
    pub skipped_processed: usize,
    pub eligible_files: usize,
    pub rule_matches: usize,
    pub name_matches: usize,
    pub content_matches: usize,
    pub fallback_matches: usize,
}

#[derive(Clone, Debug)]
pub struct MonitoringPlan {
    pub plan: Plan,
    pub folder_set_sha256: String,
    pub rule_set_sha256: String,
    pub staged_files: Vec<StagedFileRecord>,
    pub stats: MonitoringStats,
}

pub fn processing_signature(
    fingerprint: &FileFingerprint,
    folder_set_sha256: &str,
    rule_set_sha256: &str,
) -> Result<String, Error> {
    validate_digest(folder_set_sha256)?;
    validate_digest(rule_set_sha256)?;
    #[derive(Serialize)]
    struct Input<'a> {
        fingerprint: &'a FileFingerprint,
        folder_set_sha256: &'a str,
        rule_set_sha256: &'a str,
    }
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&Input {
            fingerprint,
            folder_set_sha256,
            rule_set_sha256,
        })?)
    ))
}

pub fn plan_monitor_cycle<C: Classifier, E: ContentExtractor>(
    store: &StateStore,
    monitor: &MonitorRecord,
    folder_set: &FolderSet,
    rules: &[LocalRule],
    classifier: &C,
    extractor: &E,
    options: MonitoringOptions,
) -> Result<MonitoringPlan, Error> {
    validate_monitor_binding(monitor, folder_set)?;
    let rule_set = RuleSet::compile(rules, &folder_set.folders)?;
    let folder_set_sha256 = folder_set.sha256()?;
    let rule_set_sha256 = rule_set.digest().to_owned();
    let source = Path::new(&monitor.source);
    let excluded: Vec<_> = folder_set
        .folders
        .iter()
        .map(|folder| folder.path.clone())
        .collect();
    let scanned = scan_directory(source, &folder_set.scope, &excluded)?;
    let mut eligible = Vec::new();
    let mut fingerprints = HashMap::new();
    let mut signatures = HashMap::new();
    let mut skipped_processed = 0;
    for file in &scanned {
        let fingerprint = fingerprint_candidate(source, file)?;
        let signature = processing_signature(&fingerprint, &folder_set_sha256, &rule_set_sha256)?;
        if store.is_processed(&monitor.id, &fingerprint, &signature)? {
            skipped_processed += 1;
        } else {
            fingerprints.insert(file.id.clone(), fingerprint);
            signatures.insert(file.id.clone(), signature);
            eligible.push(file.clone());
        }
    }

    let mut ruled = Vec::new();
    let mut unmatched = Vec::new();
    for file in &eligible {
        match rule_set.classify(file) {
            Some(classification) => ruled.push(classification),
            None => unmatched.push(file.clone()),
        }
    }
    let mut name_pass = classify_file_names(
        &unmatched,
        &folder_set.folders,
        classifier,
        options.name_batch_size,
        options.batch_delay,
    )?;
    name_pass.extend_resolved(ruled);
    let summary = complete_classification(
        source,
        &eligible,
        &folder_set.folders,
        classifier,
        extractor,
        ClassificationOptions {
            content_decision: monitoring_content_decision(options.content_policy),
            max_content_chars: options.max_content_chars,
            max_content_file_bytes: options.max_content_file_bytes,
            content_batch_size: options.content_batch_size,
            batch_delay: options.batch_delay,
        },
        name_pass,
    )?;
    let plan = build_plan(
        source,
        &folder_set.scope,
        &eligible,
        &folder_set.folders,
        summary.classifications,
    )?;
    let mut staged_files = Vec::with_capacity(plan.entries.len());
    for entry in &plan.entries {
        let fingerprint = fingerprints.get(&entry.file_id).ok_or_else(|| {
            Error::InvalidState(format!(
                "plan entry {:?} has no monitoring fingerprint",
                entry.file_id
            ))
        })?;
        if fingerprint != &entry.source_fingerprint {
            return Err(Error::InvalidState(format!(
                "source file {:?} changed while its monitoring plan was built",
                entry.source_path
            )));
        }
        staged_files.push(StagedFileRecord {
            file_id: entry.file_id.clone(),
            file_identity: fingerprint.identity.clone(),
            relative_path: entry.source_path.clone(),
            content_sha256: fingerprint.sha256.clone(),
            size_bytes: fingerprint.size,
            processing_signature: signatures
                .get(&entry.file_id)
                .ok_or_else(|| {
                    Error::InvalidState(format!(
                        "plan entry {:?} has no processing signature",
                        entry.file_id
                    ))
                })?
                .clone(),
            classification_basis: entry.classification_basis,
            rule_id: entry.rule_id.clone(),
            destination_id: entry.destination_id.clone(),
        });
    }
    Ok(MonitoringPlan {
        plan,
        folder_set_sha256,
        rule_set_sha256,
        staged_files,
        stats: MonitoringStats {
            total_files: scanned.len(),
            skipped_processed,
            eligible_files: eligible.len(),
            rule_matches: summary.by_rule,
            name_matches: summary.by_name,
            content_matches: summary.by_content,
            fallback_matches: summary.by_fallback,
        },
    })
}

pub fn persist_monitoring_plan(
    store: &mut StateStore,
    run_id: &str,
    path: &Path,
    monitoring: &MonitoringPlan,
) -> Result<(), Error> {
    if !path.is_absolute() {
        return Err(Error::InvalidState(
            "monitoring plan path must be absolute".into(),
        ));
    }
    write_new_json(path, &monitoring.plan)?;
    let path = path
        .to_str()
        .ok_or_else(|| Error::InvalidState("monitoring plan path must be valid UTF-8".into()))?;
    store.record_plan_with_files(
        run_id,
        path,
        &monitoring.plan.sha256()?,
        monitoring.stats.total_files as u64,
        monitoring.stats.rule_matches as u64,
        monitoring.stats.name_matches as u64,
        monitoring.stats.content_matches as u64,
        monitoring.stats.fallback_matches as u64,
        &monitoring.staged_files,
    )
}

pub fn apply_monitoring_plan(
    store: &mut StateStore,
    run_id: &str,
    plan: &Plan,
    apply_path: &Path,
    lock: &SourceLock,
    finished_unix_ms: i64,
) -> Result<ApplySession, Error> {
    let run = store
        .run(run_id)?
        .ok_or_else(|| Error::InvalidState(format!("unknown monitoring run {run_id:?}")))?;
    let expected_sha = run.plan_sha256.as_deref().ok_or_else(|| {
        Error::InvalidState("monitoring run must have a durable plan before apply".into())
    })?;
    if plan.sha256()? != expected_sha {
        return Err(Error::InvalidState(
            "monitoring apply plan does not match the recorded plan digest".into(),
        ));
    }
    let apply_path_text = apply_path
        .to_str()
        .ok_or_else(|| Error::InvalidState("monitoring apply path must be valid UTF-8".into()))?;
    store.mark_run_applying(run_id, apply_path_text)?;
    match apply_plan_with_lock(plan, apply_path, lock) {
        Ok(session) if session.state == ApplyState::Completed => {
            store.complete_from_completed_apply(run_id, plan, &session, finished_unix_ms)?;
            Ok(session)
        }
        Ok(session) => {
            store.finish_run(
                run_id,
                RunState::Failed,
                finished_unix_ms,
                Some("monitoring apply did not complete"),
            )?;
            Ok(session)
        }
        Err(error) => {
            let _ = store.reconcile_applying_runs(Some(&run.monitor_id), finished_unix_ms);
            Err(error)
        }
    }
}

fn monitoring_content_decision(policy: ContentPolicy) -> ContentDecision {
    match policy {
        ContentPolicy::OnDemand => ContentDecision::Extract,
        ContentPolicy::Ask | ContentPolicy::MetadataOnly => ContentDecision::Fallback,
    }
}

fn validate_monitor_binding(monitor: &MonitorRecord, folder_set: &FolderSet) -> Result<(), Error> {
    folder_set.validate()?;
    if monitor.source != folder_set.source || monitor.folder_set_sha256 != folder_set.sha256()? {
        return Err(Error::InvalidState(
            "monitor is not bound to this approved folder set".into(),
        ));
    }
    let (_, identity) = canonical_source_identity(Path::new(&monitor.source))?;
    if identity != monitor.source_identity {
        return Err(Error::InvalidState(
            "monitor source identity has changed".into(),
        ));
    }
    Ok(())
}

fn write_new_json<T: Serialize>(path: &Path, value: &T) -> Result<(), Error> {
    if path.exists() {
        return Err(Error::InvalidState(format!(
            "monitoring artifact already exists: {:?}",
            path.display().to_string()
        )));
    }
    let parent = path.parent().ok_or_else(|| {
        Error::InvalidState("monitoring artifact must have a parent directory".into())
    })?;
    let metadata = fs::symlink_metadata(parent).map_err(|source| Error::FileSystem {
        action: "inspect",
        path: parent.display().to_string(),
        source,
    })?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(Error::InvalidState(
            "monitoring artifact parent must be a real directory".into(),
        ));
    }
    let mut temporary = NamedTempFile::new_in(parent).map_err(|source| Error::FileSystem {
        action: "create temporary artifact in",
        path: parent.display().to_string(),
        source,
    })?;
    temporary
        .as_file_mut()
        .set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|source| Error::FileSystem {
            action: "set permissions on",
            path: path.display().to_string(),
            source,
        })?;
    serde_json::to_writer_pretty(temporary.as_file_mut(), value)?;
    writeln!(temporary.as_file_mut()).map_err(|source| Error::FileSystem {
        action: "write",
        path: path.display().to_string(),
        source,
    })?;
    temporary
        .as_file_mut()
        .sync_all()
        .map_err(|source| Error::FileSystem {
            action: "sync",
            path: path.display().to_string(),
            source,
        })?;
    temporary
        .persist_noclobber(path)
        .map_err(|error| Error::FileSystem {
            action: "persist",
            path: path.display().to_string(),
            source: error.error,
        })?;
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| Error::FileSystem {
            action: "sync",
            path: parent.display().to_string(),
            source,
        })?;
    Ok(())
}

fn validate_digest(value: &str) -> Result<(), Error> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(Error::InvalidState(
            "monitoring digest must contain 64 hexadecimal characters".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        cell::{Cell, RefCell},
        fs,
    };

    use tempfile::tempdir;

    use crate::{
        ApprovedFolder, Classification, ClassificationBasis, ContentCandidate, FileCandidate,
        FolderProposal, NameClassification, NameDecision, Proposal, ScanScope,
        apply_plan_with_lock,
    };

    use super::*;

    struct RecordingClassifier {
        destination_id: String,
        needs_content: bool,
        name_paths: RefCell<Vec<String>>,
        content_calls: Cell<usize>,
    }

    impl RecordingClassifier {
        fn direct(destination_id: &str) -> Self {
            Self {
                destination_id: destination_id.into(),
                needs_content: false,
                name_paths: RefCell::new(Vec::new()),
                content_calls: Cell::new(0),
            }
        }

        fn ambiguous(destination_id: &str) -> Self {
            Self {
                destination_id: destination_id.into(),
                needs_content: true,
                name_paths: RefCell::new(Vec::new()),
                content_calls: Cell::new(0),
            }
        }
    }

    impl Classifier for RecordingClassifier {
        fn classify_names(
            &self,
            files: &[FileCandidate],
            _folders: &[ApprovedFolder],
        ) -> Result<Vec<NameClassification>, Error> {
            self.name_paths
                .borrow_mut()
                .extend(files.iter().map(|file| file.source_path.clone()));
            Ok(files
                .iter()
                .map(|file| NameClassification {
                    file_id: file.id.clone(),
                    decision: if self.needs_content {
                        NameDecision::NeedsContent
                    } else {
                        NameDecision::Destination {
                            destination_id: self.destination_id.clone(),
                        }
                    },
                    reasoning: None,
                })
                .collect())
        }

        fn classify_contents(
            &self,
            files: &[ContentCandidate],
            _folders: &[ApprovedFolder],
        ) -> Result<Vec<Classification>, Error> {
            self.content_calls
                .set(self.content_calls.get() + files.len());
            Ok(files
                .iter()
                .map(|file| Classification {
                    file_id: file.file_id.clone(),
                    destination_id: self.destination_id.clone(),
                    reasoning: None,
                    basis: ClassificationBasis::Content,
                    rule_id: None,
                })
                .collect())
        }
    }

    #[derive(Default)]
    struct RecordingExtractor {
        calls: Cell<usize>,
    }

    impl ContentExtractor for RecordingExtractor {
        fn extract(
            &self,
            _source: &Path,
            file: &FileCandidate,
            _max_chars: usize,
            _max_file_bytes: u64,
        ) -> Option<ContentCandidate> {
            self.calls.set(self.calls.get() + 1);
            Some(ContentCandidate {
                file_id: file.id.clone(),
                source_path: file.source_path.clone(),
                content: "local extracted content".into(),
            })
        }
    }

    fn approved_folders(source: &Path) -> FolderSet {
        Proposal {
            version: 2,
            source: source.display().to_string(),
            scope: ScanScope::default(),
            files_considered: 2,
            folders: vec![
                FolderProposal {
                    path: "Reports".into(),
                    description: "Reports".into(),
                },
                FolderProposal {
                    path: "Receipts".into(),
                    description: "Receipts".into(),
                },
            ],
        }
        .approve()
        .unwrap()
    }

    fn register_monitor(
        store: &mut StateStore,
        folder_set: &FolderSet,
        artifact_root: &Path,
    ) -> MonitorRecord {
        let (_, identity) = canonical_source_identity(Path::new(&folder_set.source)).unwrap();
        let monitor = MonitorRecord {
            id: "m1".into(),
            source: folder_set.source.clone(),
            source_identity: identity,
            folder_set_path: artifact_root.join("folders.json").display().to_string(),
            folder_set_sha256: folder_set.sha256().unwrap(),
            interval_seconds: 60,
            enabled: true,
            last_checked_unix_ms: None,
            created_unix_ms: 100,
            updated_unix_ms: 100,
            deleted_unix_ms: None,
        };
        store.insert_monitor(&monitor).unwrap();
        monitor
    }

    fn options(content_policy: ContentPolicy) -> MonitoringOptions {
        MonitoringOptions {
            content_policy,
            max_content_chars: 20_000,
            max_content_file_bytes: 10 * 1024 * 1024,
            name_batch_size: 50,
            content_batch_size: 20,
            batch_delay: Duration::ZERO,
        }
    }

    fn plan_cycle(
        store: &StateStore,
        monitor: &MonitorRecord,
        folder_set: &FolderSet,
        rules: &[LocalRule],
        classifier: &RecordingClassifier,
        extractor: &RecordingExtractor,
        policy: ContentPolicy,
    ) -> MonitoringPlan {
        plan_monitor_cycle(
            store,
            monitor,
            folder_set,
            rules,
            classifier,
            extractor,
            options(policy),
        )
        .unwrap()
    }

    #[test]
    fn local_rules_route_before_the_name_classifier() {
        let source = tempdir().unwrap();
        let artifacts = tempdir().unwrap();
        fs::write(source.path().join("receipt.pdf"), b"receipt").unwrap();
        fs::write(source.path().join("notes.txt"), b"notes").unwrap();
        let folder_set = approved_folders(source.path());
        let mut store = StateStore::open_in_memory().unwrap();
        let monitor = register_monitor(&mut store, &folder_set, artifacts.path());
        let rules = [LocalRule {
            id: "r1".into(),
            monitor_id: monitor.id.clone(),
            name_glob: "receipt*".into(),
            destination_id: "d000001".into(),
            priority: 50,
            enabled: true,
        }];
        let classifier = RecordingClassifier::direct("d000002");
        let extractor = RecordingExtractor::default();

        let monitoring = plan_cycle(
            &store,
            &monitor,
            &folder_set,
            &rules,
            &classifier,
            &extractor,
            ContentPolicy::MetadataOnly,
        );

        assert_eq!(classifier.name_paths.borrow().as_slice(), ["notes.txt"]);
        let receipt = monitoring
            .plan
            .entries
            .iter()
            .find(|entry| entry.source_path == "receipt.pdf")
            .unwrap();
        assert_eq!(receipt.classification_basis, ClassificationBasis::Rule);
        assert_eq!(receipt.rule_id.as_deref(), Some("r1"));
        assert_eq!(receipt.destination_id, "d000001");
        assert_eq!(monitoring.stats.rule_matches, 1);
        assert_eq!(monitoring.stats.name_matches, 1);
    }

    #[test]
    fn ask_policy_uses_the_local_fallback_without_extracting_content() {
        let source = tempdir().unwrap();
        let artifacts = tempdir().unwrap();
        fs::write(source.path().join("ambiguous.pdf"), b"private content").unwrap();
        let folder_set = approved_folders(source.path());
        let mut store = StateStore::open_in_memory().unwrap();
        let monitor = register_monitor(&mut store, &folder_set, artifacts.path());
        let classifier = RecordingClassifier::ambiguous("d000001");
        let extractor = RecordingExtractor::default();

        let monitoring = plan_cycle(
            &store,
            &monitor,
            &folder_set,
            &[],
            &classifier,
            &extractor,
            ContentPolicy::Ask,
        );

        assert_eq!(extractor.calls.get(), 0);
        assert_eq!(classifier.content_calls.get(), 0);
        assert_eq!(monitoring.stats.fallback_matches, 1);
        assert_eq!(
            monitoring.plan.entries[0].classification_basis,
            ClassificationBasis::ExtensionFallback
        );
    }

    #[test]
    fn persisted_apply_marks_the_file_processed_and_a_returned_file_is_skipped() {
        let source = tempdir().unwrap();
        let artifacts = tempdir().unwrap();
        fs::write(source.path().join("report.txt"), b"report").unwrap();
        let folder_set = approved_folders(source.path());
        let mut store = StateStore::open_in_memory().unwrap();
        let monitor = register_monitor(&mut store, &folder_set, artifacts.path());
        let classifier = RecordingClassifier::direct("d000001");
        let extractor = RecordingExtractor::default();
        store.start_run("run1", &monitor.id, 100).unwrap();
        let monitoring = plan_cycle(
            &store,
            &monitor,
            &folder_set,
            &[],
            &classifier,
            &extractor,
            ContentPolicy::MetadataOnly,
        );
        let plan_path = artifacts.path().join("plan.json");
        persist_monitoring_plan(&mut store, "run1", &plan_path, &monitoring).unwrap();
        let apply_path = artifacts.path().join("apply.json");
        let lock = SourceLock::acquire(source.path()).unwrap();

        let session = apply_monitoring_plan(
            &mut store,
            "run1",
            &monitoring.plan,
            &apply_path,
            &lock,
            200,
        )
        .unwrap();
        assert_eq!(session.state, ApplyState::Completed);
        assert_eq!(
            store.run("run1").unwrap().unwrap().state,
            RunState::Completed
        );
        let staged = &monitoring.staged_files[0];
        assert!(
            store
                .is_processed(
                    &monitor.id,
                    &FileFingerprint {
                        identity: staged.file_identity.clone(),
                        size: staged.size_bytes,
                        sha256: staged.content_sha256.clone(),
                    },
                    &staged.processing_signature,
                )
                .unwrap()
        );

        fs::rename(
            source.path().join("Reports/report.txt"),
            source.path().join("report.txt"),
        )
        .unwrap();
        let second = plan_cycle(
            &store,
            &monitor,
            &folder_set,
            &[],
            &classifier,
            &extractor,
            ContentPolicy::MetadataOnly,
        );
        assert_eq!(second.stats.skipped_processed, 1);
        assert_eq!(second.stats.eligible_files, 0);
        assert!(second.plan.entries.is_empty());
        assert_eq!(classifier.name_paths.borrow().len(), 1);
    }

    #[test]
    fn reconciliation_completes_the_index_from_a_completed_apply_journal() {
        let source = tempdir().unwrap();
        let artifacts = tempdir().unwrap();
        fs::write(source.path().join("report.txt"), b"report").unwrap();
        let folder_set = approved_folders(source.path());
        let mut store = StateStore::open_in_memory().unwrap();
        let monitor = register_monitor(&mut store, &folder_set, artifacts.path());
        let classifier = RecordingClassifier::direct("d000001");
        let extractor = RecordingExtractor::default();
        store.start_run("run1", &monitor.id, 100).unwrap();
        let monitoring = plan_cycle(
            &store,
            &monitor,
            &folder_set,
            &[],
            &classifier,
            &extractor,
            ContentPolicy::MetadataOnly,
        );
        let plan_path = artifacts.path().join("plan.json");
        persist_monitoring_plan(&mut store, "run1", &plan_path, &monitoring).unwrap();
        let apply_path = artifacts.path().join("apply.json");
        store
            .mark_run_applying("run1", apply_path.to_str().unwrap())
            .unwrap();
        let lock = SourceLock::acquire(source.path()).unwrap();
        let session = apply_plan_with_lock(&monitoring.plan, &apply_path, &lock).unwrap();
        assert_eq!(session.state, ApplyState::Completed);
        assert_eq!(
            store.run("run1").unwrap().unwrap().state,
            RunState::Applying
        );

        let reconciled = store.reconcile_applying_runs(None, 300).unwrap();

        assert_eq!(reconciled.completed, 1);
        assert_eq!(
            store.run("run1").unwrap().unwrap().state,
            RunState::Completed
        );
        let staged = &monitoring.staged_files[0];
        assert!(
            store
                .is_processed(
                    &monitor.id,
                    &FileFingerprint {
                        identity: staged.file_identity.clone(),
                        size: staged.size_bytes,
                        sha256: staged.content_sha256.clone(),
                    },
                    &staged.processing_signature,
                )
                .unwrap()
        );
    }
}
