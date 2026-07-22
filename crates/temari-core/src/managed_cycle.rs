use std::{
    collections::{BTreeSet, HashSet},
    fs,
    path::Path,
};

use crate::managed_area_migration::{
    CURRENT_MANAGED_AREAS, LEGACY_MANAGED_AREAS, ManagedAreaLayout, detect_managed_area_layout,
};
use crate::{
    Classification, ClassificationBasis, Error, FileCandidate, FolderProposal, FolderSet, Plan,
    Proposal, ScanScope, build_plan, canonical_source_identity, scan_directory,
};

pub const KEPT_DIRECTORY: &str = CURRENT_MANAGED_AREAS[0];
pub const INBOX_DIRECTORY: &str = CURRENT_MANAGED_AREAS[1];
pub const LIBRARY_DIRECTORY: &str = CURRENT_MANAGED_AREAS[2];
pub const STAGE_TO_INBOX_RULE_ID: &str = "managed-stage-to-inbox";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManagedReprocessArea {
    Kept,
    Library,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ManagedReprocessSelection {
    Paths(Vec<String>),
    All,
}

impl ManagedReprocessArea {
    fn directory(self, layout: ManagedAreaLayout) -> &'static str {
        match self {
            Self::Kept => layout.manual(),
            Self::Library => layout.library(),
        }
    }
}

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
            LEGACY_MANAGED_AREAS[0].into(),
            LEGACY_MANAGED_AREAS[1].into(),
            LEGACY_MANAGED_AREAS[2].into(),
        ],
    )
}

/// Return only regular, non-symlink files directly inside Inbox. Candidate
/// paths remain relative to the managed root, not to Inbox itself.
pub fn inbox_file_candidates(source: &Path) -> Result<Vec<FileCandidate>, Error> {
    let (source, _) = canonical_source_identity(source)?;
    let layout = detect_managed_area_layout(&source)?;
    let inbox_name = layout.recents();
    let inbox = source.join(inbox_name);
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
        candidate.source_path = format!("{inbox_name}/{}", candidate.source_path);
    }
    Ok(candidates)
}

/// Enumerate explicitly selected files below Kept or Library for staging back
/// to Inbox. Selectors are relative to the selected area. Kept deliberately
/// has no full-area mode because it is the protected manual area.
pub fn reprocess_file_candidates(
    source: &Path,
    area: ManagedReprocessArea,
    selection: &ManagedReprocessSelection,
) -> Result<Vec<FileCandidate>, Error> {
    match selection {
        ManagedReprocessSelection::All if area == ManagedReprocessArea::Kept => {
            return Err(Error::InvalidArtifact(
                "full reprocessing is not allowed for managed Kept".into(),
            ));
        }
        ManagedReprocessSelection::Paths(selectors) if selectors.is_empty() => {
            return Err(Error::InvalidArtifact(
                "managed reprocessing requires at least one explicit path".into(),
            ));
        }
        ManagedReprocessSelection::All | ManagedReprocessSelection::Paths(_) => {}
    }

    let (source, _) = canonical_source_identity(source)?;
    let layout = detect_managed_area_layout(&source)?;
    let area_name = area.directory(layout);
    let area_path = source.join(area_name);
    let (resolved_area, area_identity) = canonical_source_identity(&area_path)?;
    if resolved_area != area_path {
        return Err(Error::InvalidArtifact(format!(
            "managed {area_name} must resolve inside the managed source"
        )));
    }

    let mut paths = BTreeSet::new();
    match selection {
        ManagedReprocessSelection::All => {
            collect_directory_files(&area_path, area_name, &mut paths)?;
        }
        ManagedReprocessSelection::Paths(selectors) => {
            for selector in selectors {
                collect_selected_path(&area_path, area_name, selector, &mut paths)?;
            }
        }
    }

    let (resolved_after_scan, identity_after_scan) = canonical_source_identity(&area_path)?;
    if resolved_after_scan != resolved_area || identity_after_scan != area_identity {
        return Err(Error::InvalidArtifact(format!(
            "managed {area_name} changed while it was being scanned"
        )));
    }

    Ok(paths
        .into_iter()
        .enumerate()
        .map(|(index, source_path)| FileCandidate {
            id: format!("f{:06}", index + 1),
            extension: Path::new(&source_path)
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase(),
            source_path,
        })
        .collect())
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
    let layout = detect_managed_area_layout(&source)?;
    let source_text = source
        .to_str()
        .ok_or_else(|| Error::InvalidArtifact("managed source path must be valid UTF-8".into()))?;
    let folders = stage_folder_set(
        source_text,
        ScanScope::default(),
        candidates.len(),
        layout.recents(),
    )?;
    build_to_inbox_plan(&source, candidates, &folders, layout.recents())
}

/// Build an exact, model-free Plan that returns selected Kept or Library files
/// to Inbox. Classification still happens later through the normal managed
/// Inbox workflow.
pub fn build_reprocess_to_inbox_plan(
    source: &Path,
    area: ManagedReprocessArea,
    candidates: &[FileCandidate],
) -> Result<Plan, Error> {
    let (source, _) = canonical_source_identity(source)?;
    let layout = detect_managed_area_layout(&source)?;
    for candidate in candidates {
        validate_reprocess_candidate(area, layout, candidate)?;
    }
    let source_text = source
        .to_str()
        .ok_or_else(|| Error::InvalidArtifact("managed source path must be valid UTF-8".into()))?;
    let folders = stage_folder_set(
        source_text,
        ScanScope::new(vec![area.directory(layout).into()])?,
        candidates.len(),
        layout.recents(),
    )?;
    build_to_inbox_plan(&source, candidates, &folders, layout.recents())
}

fn build_to_inbox_plan(
    source: &Path,
    candidates: &[FileCandidate],
    folders: &FolderSet,
    inbox_name: &str,
) -> Result<Plan, Error> {
    let inbox_id = folders
        .folders
        .iter()
        .find(|folder| folder.path == inbox_name)
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
        source,
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

fn stage_folder_set(
    source: &str,
    scope: ScanScope,
    files_considered: usize,
    inbox_name: &str,
) -> Result<FolderSet, Error> {
    let folders = Proposal {
        version: 2,
        source: source.into(),
        scope,
        files_considered,
        folders: vec![FolderProposal {
            path: inbox_name.into(),
            description: "Files waiting in the managed Inbox".into(),
        }],
    }
    .approve()?;
    folders.validate()?;
    Ok(folders)
}

fn collect_selected_path(
    area_path: &Path,
    area_name: &str,
    selector: &str,
    paths: &mut BTreeSet<String>,
) -> Result<(), Error> {
    crate::artifact::normalize_relative_path(selector)?;
    let selected = area_path.join(selector);
    if let Some(parent) = Path::new(selector).parent()
        && !parent.as_os_str().is_empty()
    {
        crate::filesystem::verify_existing_directory_chain(
            area_path,
            parent.to_str().ok_or_else(|| {
                Error::InvalidArtifact("managed selector parent must be valid UTF-8".into())
            })?,
        )?;
    }
    let metadata = fs::symlink_metadata(&selected).map_err(|source| Error::FileSystem {
        action: "inspect managed reprocessing selector",
        path: selected.display().to_string(),
        source,
    })?;
    if metadata.file_type().is_symlink() {
        return Err(Error::InvalidArtifact(format!(
            "managed reprocessing selector must not be a symlink: {selector:?}"
        )));
    }
    if metadata.is_file() {
        paths.insert(format!("{area_name}/{selector}"));
        return Ok(());
    }
    if metadata.is_dir() {
        crate::filesystem::verify_existing_directory_chain(area_path, selector)?;
        return collect_directory_files(&selected, &format!("{area_name}/{selector}"), paths);
    }
    Err(Error::InvalidArtifact(format!(
        "managed reprocessing selector is not a regular file or directory: {selector:?}"
    )))
}

fn collect_directory_files(
    directory: &Path,
    relative_prefix: &str,
    paths: &mut BTreeSet<String>,
) -> Result<(), Error> {
    let (resolved, identity) = canonical_source_identity(directory)?;
    if resolved != directory {
        return Err(Error::InvalidArtifact(format!(
            "managed reprocessing directory must not be a symlink: {:?}",
            directory.display().to_string()
        )));
    }
    let files = scan_directory(&resolved, &ScanScope::new(vec![".".into()])?, &[])?;
    let (resolved_after_scan, identity_after_scan) = canonical_source_identity(directory)?;
    if resolved_after_scan != resolved || identity_after_scan != identity {
        return Err(Error::InvalidArtifact(
            "managed reprocessing directory changed while it was being scanned".into(),
        ));
    }
    for file in files {
        paths.insert(format!("{relative_prefix}/{}", file.source_path));
    }
    Ok(())
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

fn validate_reprocess_candidate(
    area: ManagedReprocessArea,
    layout: ManagedAreaLayout,
    candidate: &FileCandidate,
) -> Result<(), Error> {
    validate_candidate_id(candidate)?;
    crate::artifact::normalize_relative_path(&candidate.source_path)?;
    let relative = candidate
        .source_path
        .strip_prefix(&format!("{}/", area.directory(layout)))
        .ok_or_else(|| {
            Error::InvalidArtifact(format!(
                "managed reprocessing candidate is outside {}: {:?}",
                area.directory(layout),
                candidate.source_path
            ))
        })?;
    if relative.is_empty() {
        return Err(Error::InvalidArtifact(
            "managed reprocessing candidate must identify a file".into(),
        ));
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
    let file_name = [INBOX_DIRECTORY, LEGACY_MANAGED_AREAS[1]]
        .iter()
        .find_map(|area| path.strip_prefix(&format!("{area}/")))
        .ok_or_else(|| {
            Error::InvalidArtifact(format!(
                "Recents candidate must start with a recognized managed waiting area: {path:?}"
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
    [
        KEPT_DIRECTORY,
        INBOX_DIRECTORY,
        LIBRARY_DIRECTORY,
        LEGACY_MANAGED_AREAS[0],
        LEGACY_MANAGED_AREAS[1],
        LEGACY_MANAGED_AREAS[2],
    ]
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
        assert!(error.to_string().contains("already inside AI Library"));
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
        fs::write(root.path().join("Recents/a.txt"), b"a").unwrap();
        fs::create_dir(root.path().join("Recents/nested")).unwrap();
        fs::write(root.path().join("Recents/nested/b.txt"), b"b").unwrap();
        symlink(
            root.path().join("Recents/a.txt"),
            root.path().join("Recents/linked"),
        )
        .unwrap();

        let candidates = inbox_file_candidates(root.path()).unwrap();
        assert_eq!(candidates, [candidate("f000001", "Recents/a.txt")]);

        let other = tempdir().unwrap();
        let linked_root = tempdir().unwrap();
        symlink(other.path(), linked_root.path().join(INBOX_DIRECTORY)).unwrap();
        assert!(inbox_file_candidates(linked_root.path()).is_err());
    }

    #[test]
    fn scans_only_explicit_kept_paths_with_stable_deduplication() {
        let root = tempdir().unwrap();
        fs::create_dir(root.path().join(KEPT_DIRECTORY)).unwrap();
        fs::create_dir(root.path().join(LIBRARY_DIRECTORY)).unwrap();
        fs::create_dir_all(root.path().join("Manual Library/Project/Nested")).unwrap();
        fs::write(root.path().join("Manual Library/manual.txt"), b"manual").unwrap();
        fs::write(root.path().join("Manual Library/Project/a.txt"), b"a").unwrap();
        fs::write(
            root.path().join("Manual Library/Project/Nested/z.PDF"),
            b"z",
        )
        .unwrap();
        fs::write(
            root.path().join("Manual Library/not-selected.txt"),
            b"ignored",
        )
        .unwrap();
        symlink(
            root.path().join("Manual Library/not-selected.txt"),
            root.path().join("Manual Library/Project/linked.txt"),
        )
        .unwrap();

        let selection = ManagedReprocessSelection::Paths(vec![
            "Project".into(),
            "manual.txt".into(),
            "Project/Nested/z.PDF".into(),
        ]);
        let candidates =
            reprocess_file_candidates(root.path(), ManagedReprocessArea::Kept, &selection).unwrap();

        assert_eq!(
            candidates
                .iter()
                .map(|file| (
                    file.id.as_str(),
                    file.source_path.as_str(),
                    file.extension.as_str()
                ))
                .collect::<Vec<_>>(),
            [
                ("f000001", "Manual Library/Project/Nested/z.PDF", "pdf"),
                ("f000002", "Manual Library/Project/a.txt", "txt"),
                ("f000003", "Manual Library/manual.txt", "txt"),
            ]
        );
        assert_eq!(
            candidates,
            reprocess_file_candidates(root.path(), ManagedReprocessArea::Kept, &selection,)
                .unwrap()
        );
    }

    #[test]
    fn allows_full_library_selection_but_rejects_full_kept_selection() {
        let root = tempdir().unwrap();
        fs::create_dir(root.path().join(KEPT_DIRECTORY)).unwrap();
        fs::create_dir_all(root.path().join("AI Library/Reports/2026")).unwrap();
        fs::write(root.path().join("AI Library/root.txt"), b"root").unwrap();
        fs::write(
            root.path().join("AI Library/Reports/2026/report.pdf"),
            b"report",
        )
        .unwrap();

        let candidates = reprocess_file_candidates(
            root.path(),
            ManagedReprocessArea::Library,
            &ManagedReprocessSelection::All,
        )
        .unwrap();
        assert_eq!(
            candidates
                .iter()
                .map(|file| file.source_path.as_str())
                .collect::<Vec<_>>(),
            ["AI Library/Reports/2026/report.pdf", "AI Library/root.txt"]
        );

        let error = reprocess_file_candidates(
            root.path(),
            ManagedReprocessArea::Kept,
            &ManagedReprocessSelection::All,
        )
        .unwrap_err();
        assert!(error.to_string().contains("not allowed"));
    }

    #[test]
    fn rejects_empty_escaping_and_symlink_reprocessing_selectors() {
        let root = tempdir().unwrap();
        fs::create_dir(root.path().join(KEPT_DIRECTORY)).unwrap();
        fs::create_dir(root.path().join(LIBRARY_DIRECTORY)).unwrap();
        fs::write(root.path().join("outside.txt"), b"outside").unwrap();
        symlink(
            root.path().join("outside.txt"),
            root.path().join("Manual Library/linked.txt"),
        )
        .unwrap();

        for selectors in [Vec::new(), vec!["../outside.txt".into()]] {
            assert!(
                reprocess_file_candidates(
                    root.path(),
                    ManagedReprocessArea::Kept,
                    &ManagedReprocessSelection::Paths(selectors),
                )
                .is_err()
            );
        }
        let error = reprocess_file_candidates(
            root.path(),
            ManagedReprocessArea::Kept,
            &ManagedReprocessSelection::Paths(vec!["linked.txt".into()]),
        )
        .unwrap_err();
        assert!(error.to_string().contains("must not be a symlink"));

        fs::create_dir(root.path().join("Manual Library/InvalidName")).unwrap();
        File::create(
            root.path()
                .join("Manual Library/InvalidName")
                .join(OsString::from_vec(vec![0xff])),
        )
        .unwrap();
        assert!(
            reprocess_file_candidates(
                root.path(),
                ManagedReprocessArea::Kept,
                &ManagedReprocessSelection::Paths(vec!["InvalidName".into()]),
            )
            .is_err()
        );

        let linked_root = tempdir().unwrap();
        symlink(
            root.path().join(LIBRARY_DIRECTORY),
            linked_root.path().join(LIBRARY_DIRECTORY),
        )
        .unwrap();
        assert!(
            reprocess_file_candidates(
                linked_root.path(),
                ManagedReprocessArea::Library,
                &ManagedReprocessSelection::All,
            )
            .is_err()
        );
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
        assert_eq!(plan.entries[0].destination_path, "Recents/report.txt");
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
    fn builds_a_collision_safe_rule_plan_from_library_to_inbox() {
        let root = tempdir().unwrap();
        fs::create_dir(root.path().join(KEPT_DIRECTORY)).unwrap();
        fs::create_dir(root.path().join(INBOX_DIRECTORY)).unwrap();
        fs::create_dir_all(root.path().join("AI Library/Alpha")).unwrap();
        fs::create_dir_all(root.path().join("AI Library/Beta")).unwrap();
        fs::write(root.path().join("AI Library/Alpha/report.txt"), b"alpha").unwrap();
        fs::write(root.path().join("AI Library/Beta/report.txt"), b"beta").unwrap();

        let candidates = reprocess_file_candidates(
            root.path(),
            ManagedReprocessArea::Library,
            &ManagedReprocessSelection::All,
        )
        .unwrap();
        let plan =
            build_reprocess_to_inbox_plan(root.path(), ManagedReprocessArea::Library, &candidates)
                .unwrap();

        plan.validate().unwrap();
        assert_eq!(plan.scope.recursive_roots, [LIBRARY_DIRECTORY]);
        assert_eq!(
            plan.entries
                .iter()
                .map(|entry| (
                    entry.source_path.as_str(),
                    entry.destination_path.as_str(),
                    entry.classification_basis,
                    entry.rule_id.as_deref()
                ))
                .collect::<Vec<_>>(),
            [
                (
                    "AI Library/Alpha/report.txt",
                    "Recents/report.txt",
                    ClassificationBasis::Rule,
                    Some(STAGE_TO_INBOX_RULE_ID),
                ),
                (
                    "AI Library/Beta/report.txt",
                    "Recents/report 1.txt",
                    ClassificationBasis::Rule,
                    Some(STAGE_TO_INBOX_RULE_ID),
                ),
            ]
        );
        assert!(
            plan.entries
                .iter()
                .all(|entry| entry.destination_id.starts_with('d'))
        );
    }

    #[test]
    fn reprocess_plan_rejects_candidates_from_another_area_and_symlinks() {
        let root = tempdir().unwrap();
        fs::create_dir(root.path().join(KEPT_DIRECTORY)).unwrap();
        fs::create_dir(root.path().join(INBOX_DIRECTORY)).unwrap();
        fs::create_dir(root.path().join(LIBRARY_DIRECTORY)).unwrap();
        fs::write(root.path().join("Manual Library/manual.txt"), b"manual").unwrap();
        fs::write(root.path().join("AI Library/target.txt"), b"target").unwrap();
        symlink(
            root.path().join("AI Library/target.txt"),
            root.path().join("AI Library/linked.txt"),
        )
        .unwrap();

        assert!(
            build_reprocess_to_inbox_plan(
                root.path(),
                ManagedReprocessArea::Library,
                &[candidate("f1", "Manual Library/manual.txt")],
            )
            .is_err()
        );
        assert!(
            build_reprocess_to_inbox_plan(
                root.path(),
                ManagedReprocessArea::Library,
                &[candidate("f1", "AI Library/linked.txt")],
            )
            .is_err()
        );
    }

    #[test]
    fn applies_and_undoes_an_explicit_kept_reprocessing_plan() {
        let root = tempdir().unwrap();
        let journals = tempdir().unwrap();
        fs::create_dir(root.path().join(KEPT_DIRECTORY)).unwrap();
        fs::create_dir(root.path().join(INBOX_DIRECTORY)).unwrap();
        fs::create_dir(root.path().join(LIBRARY_DIRECTORY)).unwrap();
        fs::write(root.path().join("Manual Library/manual.txt"), b"manual").unwrap();

        let candidates = reprocess_file_candidates(
            root.path(),
            ManagedReprocessArea::Kept,
            &ManagedReprocessSelection::Paths(vec!["manual.txt".into()]),
        )
        .unwrap();
        let plan =
            build_reprocess_to_inbox_plan(root.path(), ManagedReprocessArea::Kept, &candidates)
                .unwrap();
        let applied = crate::apply_plan(&plan, &journals.path().join("apply.json")).unwrap();

        assert!(!root.path().join("Manual Library/manual.txt").exists());
        assert_eq!(
            fs::read(root.path().join("Recents/manual.txt")).unwrap(),
            b"manual"
        );

        let undone = crate::undo_session(&applied, &journals.path().join("undo.json")).unwrap();
        assert_eq!(undone.state, crate::UndoState::Completed);
        assert_eq!(
            fs::read(root.path().join("Manual Library/manual.txt")).unwrap(),
            b"manual"
        );
        assert!(!root.path().join("Recents/manual.txt").exists());
    }

    #[test]
    fn stage_plan_rejects_nested_reserved_and_symlink_sources() {
        let root = tempdir().unwrap();
        fs::create_dir(root.path().join(INBOX_DIRECTORY)).unwrap();
        fs::write(root.path().join("Recents/nested.txt"), b"nested").unwrap();
        assert!(
            build_stage_to_inbox_plan(root.path(), &[candidate("f1", "Recents/nested.txt")])
                .is_err()
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
            candidate("f000001", "Recents/a.txt"),
            candidate("f000002", "Recents/b.txt"),
        ];
        let ids = HashSet::from(["f000001".to_owned()]);
        let paths = HashSet::from(["Recents/a.txt".to_owned(), "Recents/b.txt".to_owned()]);
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
                &[candidate("f3", "Recents/nested/c.txt")],
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
