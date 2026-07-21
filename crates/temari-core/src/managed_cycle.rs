use std::{collections::HashSet, path::Path};

use crate::{
    Classification, ClassificationBasis, Error, FileCandidate, FolderProposal, FolderSet, Plan,
    Proposal, ScanScope, build_plan, canonical_source_identity, scan_directory,
};

pub const KEPT_DIRECTORY: &str = "Kept";
pub const INBOX_DIRECTORY: &str = "Inbox";
pub const LIBRARY_DIRECTORY: &str = "Library";
pub const STAGE_TO_INBOX_RULE_ID: &str = "managed-stage-to-inbox";

/// Bind an approved folder set to the managed Library without changing any
/// destination IDs or classification metadata.
pub fn library_folder_set(approved: &FolderSet) -> Result<FolderSet, Error> {
    approved.validate()?;
    let mut managed = approved.clone();
    managed.scope = ScanScope::new(vec![INBOX_DIRECTORY.into()])?;
    for folder in &mut managed.folders {
        if first_component(&folder.path).eq_ignore_ascii_case(LIBRARY_DIRECTORY) {
            return Err(Error::InvalidArtifact(format!(
                "approved destination is already inside {LIBRARY_DIRECTORY}: {:?}",
                folder.path
            )));
        }
        folder.path = format!("{LIBRARY_DIRECTORY}/{}", folder.path);
    }
    managed.validate()?;
    Ok(managed)
}

/// Return only regular, non-symlink files directly under the managed root.
/// The three physical managed-area names are always excluded.
pub fn root_file_candidates(source: &Path) -> Result<Vec<FileCandidate>, Error> {
    let (source, _) = canonical_source_identity(source)?;
    scan_directory(
        &source,
        &ScanScope::default(),
        &[
            KEPT_DIRECTORY.into(),
            INBOX_DIRECTORY.into(),
            LIBRARY_DIRECTORY.into(),
        ],
    )
}

/// Return only regular, non-symlink files directly inside Inbox. Candidate
/// paths remain relative to the managed root, not to Inbox itself.
pub fn inbox_file_candidates(source: &Path) -> Result<Vec<FileCandidate>, Error> {
    let (source, _) = canonical_source_identity(source)?;
    let inbox = source.join(INBOX_DIRECTORY);
    let (resolved_inbox, inbox_identity) = canonical_source_identity(&inbox)?;
    if resolved_inbox != inbox {
        return Err(Error::InvalidArtifact(
            "managed Inbox must resolve inside the managed source".into(),
        ));
    }

    let mut candidates = scan_directory(&resolved_inbox, &ScanScope::default(), &[])?;
    let (resolved_after_scan, identity_after_scan) = canonical_source_identity(&inbox)?;
    if resolved_after_scan != resolved_inbox || identity_after_scan != inbox_identity {
        return Err(Error::InvalidArtifact(
            "managed Inbox changed while it was being scanned".into(),
        ));
    }
    for candidate in &mut candidates {
        candidate.source_path = format!("{INBOX_DIRECTORY}/{}", candidate.source_path);
    }
    Ok(candidates)
}

/// Build the exact, model-free Plan that stages root files into Inbox.
pub fn build_stage_to_inbox_plan(
    source: &Path,
    candidates: &[FileCandidate],
) -> Result<Plan, Error> {
    for candidate in candidates {
        validate_root_candidate(candidate)?;
    }
    let (source, _) = canonical_source_identity(source)?;
    let source_text = source
        .to_str()
        .ok_or_else(|| Error::InvalidArtifact("managed source path must be valid UTF-8".into()))?;
    let folders = stage_folder_set(source_text, candidates.len())?;
    let inbox_id = folders
        .folders
        .iter()
        .find(|folder| folder.path == INBOX_DIRECTORY)
        .map(|folder| folder.id.clone())
        .ok_or_else(|| Error::InvalidState("stage folder set has no Inbox destination".into()))?;
    let classifications = candidates
        .iter()
        .map(|candidate| Classification {
            file_id: candidate.id.clone(),
            destination_id: inbox_id.clone(),
            reasoning: None,
            basis: ClassificationBasis::Rule,
            rule_id: Some(STAGE_TO_INBOX_RULE_ID.into()),
        })
        .collect();
    build_plan(
        &source,
        &folders.scope,
        candidates,
        &folders.folders,
        classifications,
    )
}

/// Keep Inbox candidates selected by every non-empty eligibility set. This
/// allows callers to filter by ID, path, or the intersection of both.
pub fn filter_inbox_candidates(
    candidates: &[FileCandidate],
    eligible_ids: &HashSet<String>,
    eligible_paths: &HashSet<String>,
) -> Result<Vec<FileCandidate>, Error> {
    let mut seen_ids = HashSet::new();
    let mut seen_paths = HashSet::new();
    for candidate in candidates {
        validate_inbox_candidate(candidate)?;
        if !seen_ids.insert(candidate.id.as_str()) {
            return Err(Error::InvalidArtifact(format!(
                "duplicate Inbox candidate ID {:?}",
                candidate.id
            )));
        }
        if !seen_paths.insert(candidate.source_path.as_str()) {
            return Err(Error::InvalidArtifact(format!(
                "duplicate Inbox candidate path {:?}",
                candidate.source_path
            )));
        }
    }
    for path in eligible_paths {
        validate_inbox_path(path)?;
    }
    if eligible_ids.is_empty() && eligible_paths.is_empty() {
        return Ok(Vec::new());
    }

    Ok(candidates
        .iter()
        .filter(|candidate| {
            (eligible_ids.is_empty() || eligible_ids.contains(&candidate.id))
                && (eligible_paths.is_empty() || eligible_paths.contains(&candidate.source_path))
        })
        .cloned()
        .collect())
}

fn stage_folder_set(source: &str, files_considered: usize) -> Result<FolderSet, Error> {
    let folders = Proposal {
        version: 2,
        source: source.into(),
        scope: ScanScope::default(),
        files_considered,
        folders: vec![FolderProposal {
            path: INBOX_DIRECTORY.into(),
            description: "Files waiting in the managed Inbox".into(),
        }],
    }
    .approve()?;
    folders.validate()?;
    Ok(folders)
}

fn validate_root_candidate(candidate: &FileCandidate) -> Result<(), Error> {
    validate_candidate_id(candidate)?;
    crate::artifact::normalize_relative_path(&candidate.source_path)?;
    if candidate.source_path.contains('/') {
        return Err(Error::InvalidArtifact(format!(
            "stage candidate must be directly under the managed root: {:?}",
            candidate.source_path
        )));
    }
    if is_managed_area(&candidate.source_path) {
        return Err(Error::InvalidArtifact(format!(
            "managed-area name cannot be staged as a root file: {:?}",
            candidate.source_path
        )));
    }
    Ok(())
}

fn validate_inbox_candidate(candidate: &FileCandidate) -> Result<(), Error> {
    validate_candidate_id(candidate)?;
    validate_inbox_path(&candidate.source_path)
}

fn validate_candidate_id(candidate: &FileCandidate) -> Result<(), Error> {
    if candidate.id.trim().is_empty() || candidate.id.chars().any(char::is_control) {
        return Err(Error::InvalidArtifact(
            "managed candidate ID must be non-empty and contain no control characters".into(),
        ));
    }
    Ok(())
}

fn validate_inbox_path(path: &str) -> Result<(), Error> {
    crate::artifact::normalize_relative_path(path)?;
    let file_name = path
        .strip_prefix(&format!("{INBOX_DIRECTORY}/"))
        .ok_or_else(|| {
            Error::InvalidArtifact(format!(
                "Inbox candidate must start with {INBOX_DIRECTORY}/: {path:?}"
            ))
        })?;
    if file_name.is_empty() || file_name.contains('/') {
        return Err(Error::InvalidArtifact(format!(
            "Inbox candidate must be directly inside Inbox: {path:?}"
        )));
    }
    Ok(())
}

fn first_component(path: &str) -> &str {
    path.split('/').next().unwrap_or_default()
}

fn is_managed_area(path: &str) -> bool {
    [KEPT_DIRECTORY, INBOX_DIRECTORY, LIBRARY_DIRECTORY]
        .iter()
        .any(|area| path.eq_ignore_ascii_case(area))
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashSet,
        ffi::OsString,
        fs::{self, File},
        os::unix::{ffi::OsStringExt, fs::symlink},
    };

    use tempfile::tempdir;

    use super::*;

    fn approved(source: &Path) -> FolderSet {
        Proposal {
            version: 2,
            source: source.display().to_string(),
            scope: ScanScope::default(),
            files_considered: 1,
            folders: vec![FolderProposal {
                path: "Documents/Reports".into(),
                description: "Reports".into(),
            }],
        }
        .approve()
        .unwrap()
    }

    fn candidate(id: &str, path: &str) -> FileCandidate {
        FileCandidate {
            id: id.into(),
            source_path: path.into(),
            extension: Path::new(path)
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase(),
        }
    }

    #[test]
    fn prefixes_library_paths_and_preserves_approved_metadata() {
        let root = tempdir().unwrap();
        let original = approved(root.path());
        let managed = library_folder_set(&original).unwrap();

        assert_eq!(managed.source, original.source);
        assert_eq!(managed.scope.recursive_roots, [INBOX_DIRECTORY]);
        assert_eq!(managed.folders.len(), original.folders.len());
        for (before, after) in original.folders.iter().zip(&managed.folders) {
            assert_eq!(after.id, before.id);
            assert_eq!(after.model_visible, before.model_visible);
            assert_eq!(after.fallback, before.fallback);
            assert_eq!(after.path, format!("{LIBRARY_DIRECTORY}/{}", before.path));
        }
        managed.validate().unwrap();
    }

    #[test]
    fn rejects_an_already_library_prefixed_folder_set() {
        let root = tempdir().unwrap();
        let once = library_folder_set(&approved(root.path())).unwrap();
        let error = library_folder_set(&once).unwrap_err();
        assert!(error.to_string().contains("already inside Library"));
    }

    #[test]
    fn scans_only_root_regular_files_with_stable_ids() {
        let root = tempdir().unwrap();
        fs::write(root.path().join("z.txt"), b"z").unwrap();
        fs::write(root.path().join("a.pdf"), b"a").unwrap();
        fs::write(root.path().join(INBOX_DIRECTORY), b"reserved").unwrap();
        fs::create_dir(root.path().join("directory")).unwrap();
        symlink(root.path().join("a.pdf"), root.path().join("linked")).unwrap();

        let candidates = root_file_candidates(root.path()).unwrap();
        assert_eq!(
            candidates
                .iter()
                .map(|file| (file.id.as_str(), file.source_path.as_str()))
                .collect::<Vec<_>>(),
            [("f000001", "a.pdf"), ("f000002", "z.txt")]
        );
        assert_eq!(candidates, root_file_candidates(root.path()).unwrap());
    }

    #[test]
    fn scans_only_direct_inbox_regular_files_and_rejects_a_symlinked_inbox() {
        let root = tempdir().unwrap();
        fs::create_dir(root.path().join(INBOX_DIRECTORY)).unwrap();
        fs::write(root.path().join("Inbox/a.txt"), b"a").unwrap();
        fs::create_dir(root.path().join("Inbox/nested")).unwrap();
        fs::write(root.path().join("Inbox/nested/b.txt"), b"b").unwrap();
        symlink(
            root.path().join("Inbox/a.txt"),
            root.path().join("Inbox/linked"),
        )
        .unwrap();

        let candidates = inbox_file_candidates(root.path()).unwrap();
        assert_eq!(candidates, [candidate("f000001", "Inbox/a.txt")]);

        let other = tempdir().unwrap();
        let linked_root = tempdir().unwrap();
        symlink(other.path(), linked_root.path().join(INBOX_DIRECTORY)).unwrap();
        assert!(inbox_file_candidates(linked_root.path()).is_err());
    }

    #[test]
    fn rejects_non_utf8_scan_entries() {
        let root = tempdir().unwrap();
        File::create(root.path().join(OsString::from_vec(vec![0xff]))).unwrap();
        assert!(root_file_candidates(root.path()).is_err());
    }

    #[test]
    fn builds_a_valid_rule_plan_to_inbox_without_model_selection() {
        let root = tempdir().unwrap();
        fs::write(root.path().join("report.txt"), b"report").unwrap();
        let candidates = root_file_candidates(root.path()).unwrap();

        let plan = build_stage_to_inbox_plan(root.path(), &candidates).unwrap();
        plan.validate().unwrap();
        assert_eq!(plan.version, 4);
        assert_eq!(plan.entries[0].destination_path, "Inbox/report.txt");
        assert_eq!(
            plan.entries[0].classification_basis,
            ClassificationBasis::Rule
        );
        assert_eq!(
            plan.entries[0].rule_id.as_deref(),
            Some(STAGE_TO_INBOX_RULE_ID)
        );
        assert_eq!(
            plan.folders
                .iter()
                .filter(|folder| folder.fallback.is_some())
                .count(),
            crate::FallbackCategory::ALL.len()
        );
    }

    #[test]
    fn stage_plan_rejects_nested_reserved_and_symlink_sources() {
        let root = tempdir().unwrap();
        fs::create_dir(root.path().join(INBOX_DIRECTORY)).unwrap();
        fs::write(root.path().join("Inbox/nested.txt"), b"nested").unwrap();
        assert!(
            build_stage_to_inbox_plan(root.path(), &[candidate("f1", "Inbox/nested.txt")]).is_err()
        );

        fs::write(root.path().join("target.txt"), b"target").unwrap();
        symlink(
            root.path().join("target.txt"),
            root.path().join("linked.txt"),
        )
        .unwrap();
        assert!(build_stage_to_inbox_plan(root.path(), &[candidate("f1", "linked.txt")]).is_err());
    }

    #[test]
    fn filters_by_ids_paths_or_their_intersection_and_rejects_escapes() {
        let candidates = [
            candidate("f000001", "Inbox/a.txt"),
            candidate("f000002", "Inbox/b.txt"),
        ];
        let ids = HashSet::from(["f000001".to_owned()]);
        let paths = HashSet::from(["Inbox/a.txt".to_owned(), "Inbox/b.txt".to_owned()]);
        assert_eq!(
            filter_inbox_candidates(&candidates, &ids, &HashSet::new()).unwrap(),
            [candidates[0].clone()]
        );
        assert_eq!(
            filter_inbox_candidates(&candidates, &HashSet::new(), &paths).unwrap(),
            candidates
        );
        assert_eq!(
            filter_inbox_candidates(&candidates, &ids, &paths).unwrap(),
            [candidates[0].clone()]
        );
        assert!(
            filter_inbox_candidates(&candidates, &HashSet::new(), &HashSet::new())
                .unwrap()
                .is_empty()
        );
        assert!(
            filter_inbox_candidates(
                &[candidate("f3", "Inbox/nested/c.txt")],
                &HashSet::new(),
                &HashSet::new()
            )
            .is_err()
        );
        assert!(
            filter_inbox_candidates(
                &candidates,
                &HashSet::new(),
                &HashSet::from(["../escape".to_owned()])
            )
            .is_err()
        );
    }

    #[test]
    fn empty_stage_candidate_set_builds_an_empty_read_only_plan() {
        let root = tempdir().unwrap();

        let plan = build_stage_to_inbox_plan(root.path(), &[]).unwrap();

        assert!(plan.entries.is_empty());
        assert!(plan.directories.is_empty());
        assert!(!root.path().join(INBOX_DIRECTORY).exists());
    }
}
