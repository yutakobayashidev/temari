use std::{
    cmp::Ordering,
    collections::{BTreeSet, HashMap, HashSet},
    fs, io,
    path::Path,
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    ApprovedFolder, Classification, ClassificationBasis, Error, FileCandidate, FileFingerprint,
    FsIdentity, ScanScope,
    artifact::normalize_relative_path,
    filesystem::{
        canonical_directory, checked_join, fingerprint, path_exists, verify_directory_chain,
        verify_existing_directory_chain,
    },
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub version: u32,
    pub source: String,
    pub source_identity: FsIdentity,
    pub scope: ScanScope,
    pub collision_policy: CollisionPolicy,
    pub folders: Vec<ApprovedFolder>,
    pub directories: Vec<String>,
    pub entries: Vec<PlanEntry>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CollisionPolicy {
    Rename,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PlanEntry {
    pub file_id: String,
    pub source_path: String,
    pub source_fingerprint: FileFingerprint,
    pub destination_id: String,
    pub requested_destination: String,
    pub destination_path: String,
    pub reasoning: Option<String>,
    pub classification_basis: ClassificationBasis,
}

impl Plan {
    pub fn load(path: &Path) -> Result<Self, Error> {
        let text = fs::read_to_string(path).map_err(|source| Error::ReadFile {
            path: path.display().to_string(),
            source,
        })?;
        let plan: Self = serde_json::from_str(&text)?;
        plan.validate()?;
        Ok(plan)
    }

    pub fn validate(&self) -> Result<(), Error> {
        if self.version != 3 {
            return Err(Error::InvalidArtifact(format!(
                "unsupported plan version {}; expected 3",
                self.version
            )));
        }
        self.scope.validate()?;
        let source = Path::new(&self.source);
        if !source.is_absolute() || self.source.chars().any(char::is_control) {
            return Err(Error::InvalidArtifact(
                "plan source must be an absolute canonical path without control characters".into(),
            ));
        }

        let mut previous_directory: Option<&str> = None;
        let mut directory_set = HashSet::new();
        for directory in &self.directories {
            normalize_relative_path(directory)?;
            if !directory_set.insert(directory.as_str()) {
                return Err(Error::InvalidArtifact(format!(
                    "duplicate planned directory {directory:?}"
                )));
            }
            if let Some(previous) = previous_directory
                && directory_order(previous, directory) == Ordering::Greater
            {
                return Err(Error::InvalidArtifact(
                    "planned directories must be in parent-first lexical order".into(),
                ));
            }
            previous_directory = Some(directory);
        }

        let mut file_ids = HashSet::new();
        let mut source_paths = HashSet::new();
        let mut destinations = HashSet::new();
        let folder_set = crate::FolderSet {
            version: 3,
            source: self.source.clone(),
            scope: self.scope.clone(),
            folders: self.folders.clone(),
        };
        folder_set.validate()?;
        let folders_by_id: HashMap<_, _> = self
            .folders
            .iter()
            .map(|folder| (folder.id.as_str(), folder))
            .collect();
        for entry in &self.entries {
            if entry.file_id.trim().is_empty() || !file_ids.insert(entry.file_id.as_str()) {
                return Err(Error::InvalidArtifact(format!(
                    "plan file IDs must be non-empty and unique: {:?}",
                    entry.file_id
                )));
            }
            normalize_relative_path(&entry.source_path)?;
            if !self.scope.contains(&entry.source_path) {
                return Err(Error::InvalidArtifact(format!(
                    "planned source is outside the approved scope: {:?}",
                    entry.source_path
                )));
            }
            if !source_paths.insert(entry.source_path.as_str()) {
                return Err(Error::InvalidArtifact(format!(
                    "duplicate planned source file {:?}",
                    entry.source_path
                )));
            }
            if self
                .folders
                .iter()
                .any(|folder| path_in_subtree(&entry.source_path, &folder.path))
            {
                return Err(Error::InvalidArtifact(format!(
                    "planned source is inside an approved destination: {:?}",
                    entry.source_path
                )));
            }
            let folder = folders_by_id
                .get(entry.destination_id.as_str())
                .ok_or_else(|| {
                    Error::InvalidArtifact(format!(
                        "unknown approved destination ID {:?}",
                        entry.destination_id
                    ))
                })?;
            normalize_relative_path(&entry.requested_destination)?;
            normalize_relative_path(&entry.destination_path)?;
            let file_name = relative_file_name(&entry.source_path)?;
            let expected_requested = format!("{}/{}", folder.path, file_name);
            if entry.requested_destination != expected_requested
                || destination_parent(&entry.destination_path)? != folder.path
            {
                return Err(Error::InvalidArtifact(format!(
                    "plan entry {:?} escapes its approved destination",
                    entry.file_id
                )));
            }
            match entry.classification_basis {
                ClassificationBasis::Name | ClassificationBasis::Content
                    if !folder.model_visible =>
                {
                    return Err(Error::InvalidArtifact(format!(
                        "plan entry {:?} uses a local-only destination for model classification",
                        entry.file_id
                    )));
                }
                ClassificationBasis::ExtensionFallback if folder.fallback.is_none() => {
                    return Err(Error::InvalidArtifact(format!(
                        "plan entry {:?} uses a non-fallback destination as a fallback",
                        entry.file_id
                    )));
                }
                _ => {}
            }
            if !destinations.insert(entry.destination_path.as_str()) {
                return Err(Error::InvalidArtifact(format!(
                    "duplicate planned destination {:?}",
                    entry.destination_path
                )));
            }
            validate_fingerprint(&entry.source_fingerprint)?;
        }
        if self
            .entries
            .windows(2)
            .any(|pair| pair[0].file_id > pair[1].file_id)
        {
            return Err(Error::InvalidArtifact(
                "plan entries must be sorted by file ID".into(),
            ));
        }
        for directory in &self.directories {
            if !self.entries.iter().any(|entry| {
                destination_parent(&entry.destination_path).is_ok_and(|parent| {
                    parent == directory || parent.starts_with(&format!("{directory}/"))
                })
            }) {
                return Err(Error::InvalidArtifact(format!(
                    "planned directory is not required by any move: {directory:?}"
                )));
            }
        }
        Ok(())
    }

    pub fn sha256(&self) -> Result<String, Error> {
        self.validate()?;
        Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(self)?)))
    }
}

pub fn build_plan(
    source: &Path,
    scope: &ScanScope,
    files: &[FileCandidate],
    folders: &[ApprovedFolder],
    classifications: Vec<Classification>,
) -> Result<Plan, Error> {
    if classifications.len() != files.len() {
        return Err(Error::InvalidModelResponse(format!(
            "expected {} classifications, received {}",
            files.len(),
            classifications.len()
        )));
    }
    let (canonical_source, source_identity) = canonical_directory(source)?;
    scope.validate()?;
    let files_by_id: HashMap<_, _> = files.iter().map(|file| (&file.id, file)).collect();
    let folders_by_id: HashMap<_, _> = folders.iter().map(|folder| (&folder.id, folder)).collect();
    let mut classified = Vec::with_capacity(classifications.len());
    let mut seen = HashSet::new();
    for classification in classifications {
        if !seen.insert(classification.file_id.clone()) {
            return Err(Error::InvalidModelResponse(format!(
                "duplicate file ID {:?}",
                classification.file_id
            )));
        }
        let file = files_by_id.get(&classification.file_id).ok_or_else(|| {
            Error::InvalidModelResponse(format!("unknown file ID {:?}", classification.file_id))
        })?;
        let folder = folders_by_id
            .get(&classification.destination_id)
            .ok_or_else(|| {
                Error::InvalidModelResponse(format!(
                    "unknown destination ID {:?}",
                    classification.destination_id
                ))
            })?;
        classified.push((classification, *file, *folder));
    }
    classified.sort_by(|left, right| left.0.file_id.cmp(&right.0.file_id));

    let mut reserved_destinations = HashSet::new();
    let mut missing_directories = BTreeSet::new();
    let mut entries = Vec::with_capacity(classified.len());
    for (classification, file, folder) in classified {
        normalize_relative_path(&file.source_path)?;
        if !scope.contains(&file.source_path) {
            return Err(Error::InvalidArtifact(format!(
                "planned source is outside the approved scope: {:?}",
                file.source_path
            )));
        }
        if folders
            .iter()
            .any(|approved| path_in_subtree(&file.source_path, &approved.path))
        {
            return Err(Error::InvalidArtifact(format!(
                "planned source is inside an approved destination: {:?}",
                file.source_path
            )));
        }
        let file_name = relative_file_name(&file.source_path)?;
        verify_directory_chain(&canonical_source, &folder.path)?;
        if let Some(parent) = Path::new(&file.source_path).parent()
            && !parent.as_os_str().is_empty()
        {
            verify_existing_directory_chain(
                &canonical_source,
                parent.to_str().ok_or_else(|| {
                    Error::InvalidArtifact("source parent path must be valid UTF-8".into())
                })?,
            )?;
        }
        let source_path = canonical_source.join(&file.source_path);
        let source_fingerprint = fingerprint(&source_path)?;
        let requested_destination = format!("{}/{}", folder.path, file_name);
        let destination_path = resolve_collision(
            &canonical_source,
            &folder.path,
            file_name,
            &reserved_destinations,
        )?;
        reserved_destinations.insert(destination_path.clone());
        collect_missing_directories(
            &canonical_source,
            destination_parent(&destination_path)?,
            &mut missing_directories,
        )?;
        entries.push(PlanEntry {
            file_id: classification.file_id,
            source_path: file.source_path.clone(),
            source_fingerprint,
            destination_id: classification.destination_id,
            requested_destination,
            destination_path,
            reasoning: classification.reasoning,
            classification_basis: classification.basis,
        });
    }

    let mut directories: Vec<_> = missing_directories.into_iter().collect();
    directories.sort_by(|left, right| directory_order(left, right));
    let source_text = canonical_source.to_str().ok_or_else(|| {
        Error::InvalidArtifact("source path must be valid UTF-8 for portable artifacts".into())
    })?;
    if source_text.chars().any(char::is_control) {
        return Err(Error::InvalidArtifact(
            "source path must not contain control characters".into(),
        ));
    }
    let plan = Plan {
        version: 3,
        source: source_text.to_owned(),
        source_identity,
        scope: scope.clone(),
        collision_policy: CollisionPolicy::Rename,
        folders: folders.to_vec(),
        directories,
        entries,
    };
    plan.validate()?;
    Ok(plan)
}

fn resolve_collision(
    source: &Path,
    folder: &str,
    file_name: &str,
    reserved: &HashSet<String>,
) -> Result<String, Error> {
    for suffix in 0_u64.. {
        let candidate_name = if suffix == 0 {
            file_name.to_owned()
        } else {
            suffixed_name(file_name, suffix)?
        };
        let relative = format!("{folder}/{candidate_name}");
        let absolute = checked_join(source, &relative)?;
        if !reserved.contains(&relative) && !path_exists(&absolute)? {
            return Ok(relative);
        }
    }
    unreachable!("the numeric collision suffix space is finite only after u64 exhaustion")
}

fn suffixed_name(file_name: &str, suffix: u64) -> Result<String, Error> {
    let path = Path::new(file_name);
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .ok_or_else(|| {
            Error::InvalidArtifact(format!("file name is not valid UTF-8: {file_name:?}"))
        })?;
    Ok(match path.extension().and_then(|value| value.to_str()) {
        Some(extension) => format!("{stem} {suffix}.{extension}"),
        None => format!("{stem} {suffix}"),
    })
}

fn collect_missing_directories(
    source: &Path,
    relative: &str,
    missing: &mut BTreeSet<String>,
) -> Result<(), Error> {
    let mut current_relative = String::new();
    for component in relative.split('/') {
        if !current_relative.is_empty() {
            current_relative.push('/');
        }
        current_relative.push_str(component);
        let absolute = checked_join(source, &current_relative)?;
        match fs::symlink_metadata(&absolute) {
            Ok(metadata) => {
                if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
                    return Err(Error::InvalidArtifact(format!(
                        "destination component must be a real directory: {:?}",
                        absolute.display().to_string()
                    )));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                missing.insert(current_relative.clone());
            }
            Err(source_error) => {
                return Err(Error::FileSystem {
                    action: "inspect",
                    path: absolute.display().to_string(),
                    source: source_error,
                });
            }
        }
    }
    Ok(())
}

fn destination_parent(destination: &str) -> Result<&str, Error> {
    destination
        .rsplit_once('/')
        .map(|(parent, _)| parent)
        .ok_or_else(|| {
            Error::InvalidArtifact(format!(
                "planned destination must include an approved directory: {destination:?}"
            ))
        })
}

fn validate_file_name(name: &str) -> Result<(), Error> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.chars().any(char::is_control)
    {
        return Err(Error::InvalidArtifact(format!(
            "source file name must be one portable path component: {name:?}"
        )));
    }
    Ok(())
}

fn relative_file_name(path: &str) -> Result<&str, Error> {
    let name = path.rsplit('/').next().unwrap_or_default();
    validate_file_name(name)?;
    Ok(name)
}

fn path_in_subtree(path: &str, subtree: &str) -> bool {
    let path = path.to_lowercase();
    let subtree = subtree.to_lowercase();
    path == subtree || path.starts_with(&format!("{subtree}/"))
}

fn validate_fingerprint(fingerprint: &FileFingerprint) -> Result<(), Error> {
    if fingerprint.sha256.len() != 64
        || !fingerprint
            .sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(Error::InvalidArtifact(
            "file fingerprint must contain a lowercase SHA-256 digest".into(),
        ));
    }
    Ok(())
}

fn directory_order(left: &str, right: &str) -> Ordering {
    left.split('/')
        .count()
        .cmp(&right.split('/').count())
        .then_with(|| left.cmp(right))
}

#[cfg(test)]
mod tests {
    use std::fs::File;

    use tempfile::tempdir;

    use super::*;

    fn file(id: &str, name: &str) -> FileCandidate {
        FileCandidate {
            id: id.into(),
            source_path: name.into(),
            extension: Path::new(name)
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or_default()
                .into(),
        }
    }

    fn folders() -> Vec<ApprovedFolder> {
        crate::Proposal {
            version: 2,
            source: "/tmp/inbox".into(),
            scope: ScanScope::default(),
            files_considered: 1,
            folders: vec![crate::FolderProposal {
                path: "Documents/Reports".into(),
                description: "Documents".into(),
            }],
        }
        .approve()
        .unwrap()
        .folders
    }

    fn classification(file_id: &str, destination_id: &str) -> Classification {
        Classification {
            file_id: file_id.into(),
            destination_id: destination_id.into(),
            reasoning: None,
            basis: ClassificationBasis::Name,
        }
    }

    #[test]
    fn builds_plan_with_fingerprint_and_parent_first_directories() {
        let root = tempdir().unwrap();
        fs::write(root.path().join("report.txt"), b"report").unwrap();

        let folders = folders();
        let plan = build_plan(
            root.path(),
            &ScanScope::default(),
            &[file("f1", "report.txt")],
            &folders,
            vec![classification("f1", "d000001")],
        )
        .unwrap();

        assert_eq!(plan.directories, ["Documents", "Documents/Reports"]);
        assert_eq!(
            plan.entries[0].destination_path,
            "Documents/Reports/report.txt"
        );
        assert_eq!(plan.entries[0].source_fingerprint.size, 6);
    }

    #[test]
    fn resolves_existing_and_reserved_collisions_deterministically() {
        let root = tempdir().unwrap();
        fs::write(root.path().join("report.txt"), b"one").unwrap();
        fs::write(root.path().join("report-copy.txt"), b"two").unwrap();
        fs::create_dir_all(root.path().join("Documents/Reports")).unwrap();
        File::create(root.path().join("Documents/Reports/report.txt")).unwrap();
        let files = [file("f1", "report.txt"), file("f2", "report-copy.txt")];
        let classifications = vec![
            classification("f2", "d000001"),
            classification("f1", "d000001"),
        ];

        let folders = folders();
        let plan = build_plan(
            root.path(),
            &ScanScope::default(),
            &files,
            &folders,
            classifications,
        )
        .unwrap();

        assert_eq!(
            plan.entries[0].destination_path,
            "Documents/Reports/report 1.txt"
        );
        assert_eq!(
            plan.entries[1].destination_path,
            "Documents/Reports/report-copy.txt"
        );
    }

    #[test]
    fn rejects_unknown_destination_id() {
        let root = tempdir().unwrap();
        fs::write(root.path().join("report.txt"), b"report").unwrap();
        let folders = folders();
        let error = build_plan(
            root.path(),
            &ScanScope::default(),
            &[file("f1", "report.txt")],
            &folders,
            vec![classification("f1", "invented")],
        )
        .unwrap_err();

        assert!(error.to_string().contains("unknown destination"));
    }

    #[test]
    fn rejects_missing_classification() {
        let root = tempdir().unwrap();
        fs::write(root.path().join("report.txt"), b"report").unwrap();
        let folders = folders();
        let error = build_plan(
            root.path(),
            &ScanScope::default(),
            &[file("f1", "report.txt")],
            &folders,
            Vec::new(),
        )
        .unwrap_err();

        assert!(error.to_string().contains("expected 1"));
    }

    #[test]
    fn nested_duplicate_basenames_keep_distinct_sources_and_destinations() {
        let root = tempdir().unwrap();
        fs::create_dir_all(root.path().join("incoming/a")).unwrap();
        fs::create_dir_all(root.path().join("incoming/b")).unwrap();
        fs::write(root.path().join("incoming/a/report.txt"), b"one").unwrap();
        fs::write(root.path().join("incoming/b/report.txt"), b"two").unwrap();
        let scope = ScanScope::new(vec!["incoming".into()]).unwrap();
        let files = [
            file("f1", "incoming/a/report.txt"),
            file("f2", "incoming/b/report.txt"),
        ];
        let folders = folders();

        let plan = build_plan(
            root.path(),
            &scope,
            &files,
            &folders,
            vec![
                classification("f1", "d000001"),
                classification("f2", "d000001"),
            ],
        )
        .unwrap();

        assert_eq!(plan.entries[0].source_path, "incoming/a/report.txt");
        assert_eq!(plan.entries[1].source_path, "incoming/b/report.txt");
        assert_eq!(
            plan.entries[0].destination_path,
            "Documents/Reports/report.txt"
        );
        assert_eq!(
            plan.entries[1].destination_path,
            "Documents/Reports/report 1.txt"
        );
    }

    #[test]
    fn rejects_nested_source_outside_scope_or_inside_destination() {
        let root = tempdir().unwrap();
        fs::create_dir_all(root.path().join("nested")).unwrap();
        fs::write(root.path().join("nested/report.txt"), b"report").unwrap();
        let folders = folders();
        let outside = build_plan(
            root.path(),
            &ScanScope::default(),
            &[file("f1", "nested/report.txt")],
            &folders,
            vec![classification("f1", "d000001")],
        )
        .unwrap_err();
        assert!(outside.to_string().contains("outside the approved scope"));

        fs::create_dir_all(root.path().join("Documents/Reports")).unwrap();
        fs::write(root.path().join("Documents/Reports/already.txt"), b"report").unwrap();
        let inside = build_plan(
            root.path(),
            &ScanScope::new(vec![".".into()]).unwrap(),
            &[file("f1", "Documents/Reports/already.txt")],
            &folders,
            vec![classification("f1", "d000001")],
        )
        .unwrap_err();
        assert!(
            inside
                .to_string()
                .contains("inside an approved destination")
        );
    }
}
