use std::{
    cmp::Ordering,
    collections::{BTreeSet, HashMap, HashSet},
    fs, io,
    path::Path,
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    Error, FileFingerprint, FolderSet, FsIdentity, ManagedLibraryEditPlan, ProcessedFileRecord,
    apply::{ValidatedMove, ValidatedMoveManifest},
    artifact::normalize_relative_path,
    filesystem::{checked_join, path_exists},
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ManagedLibraryReorganizationTarget {
    Approved { destination_id: String },
    Recents { removed_destination_id: String },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "reason")]
pub enum ManagedLibraryReorganizationAttentionReason {
    Untracked,
    Changed,
    UnknownDestination { destination_id: String },
    OutsideRecordedDestination { destination_id: String },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedLibraryReorganizationAttention {
    pub source_path: String,
    pub reason: ManagedLibraryReorganizationAttentionReason,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedLibraryReorganizationEntry {
    pub file_id: String,
    pub source_path: String,
    pub source_fingerprint: FileFingerprint,
    pub target: ManagedLibraryReorganizationTarget,
    pub requested_destination: String,
    pub destination_path: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedLibraryReorganizationPlan {
    pub version: u32,
    pub id: String,
    pub workspace_id: String,
    pub configure_run_id: String,
    pub configure_plan_id: String,
    pub source: String,
    pub source_identity: FsIdentity,
    pub folder_set_path: String,
    pub folder_set_sha256: String,
    pub before_folders: FolderSet,
    pub after_folders: FolderSet,
    pub directories: Vec<String>,
    pub entries: Vec<ManagedLibraryReorganizationEntry>,
    pub attention: Vec<ManagedLibraryReorganizationAttention>,
}

impl ManagedLibraryReorganizationPlan {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn build(
        id: String,
        workspace_id: String,
        configure_run_id: String,
        source_identity: FsIdentity,
        folder_set_path: String,
        configure: &ManagedLibraryEditPlan,
        candidates: &[(String, FileFingerprint)],
        processed: &[ProcessedFileRecord],
    ) -> Result<Self, Error> {
        configure.validate()?;
        let processed_by_identity = processed
            .iter()
            .map(|record| {
                (
                    (record.file_identity.device, record.file_identity.inode),
                    record,
                )
            })
            .collect::<HashMap<_, _>>();
        let before_by_id = configure
            .before_folders
            .folders
            .iter()
            .map(|folder| (folder.id.as_str(), folder))
            .collect::<HashMap<_, _>>();
        let after_by_id = configure
            .after_folders
            .folders
            .iter()
            .map(|folder| (folder.id.as_str(), folder))
            .collect::<HashMap<_, _>>();
        let source = Path::new(&configure.source);
        let mut reserved = HashSet::new();
        let mut directories = BTreeSet::new();
        let mut entries = Vec::new();
        let mut attention = Vec::new();

        for (index, (source_path, fingerprint)) in candidates.iter().enumerate() {
            let identity = (fingerprint.identity.device, fingerprint.identity.inode);
            let Some(record) = processed_by_identity.get(&identity) else {
                attention.push(ManagedLibraryReorganizationAttention {
                    source_path: source_path.clone(),
                    reason: ManagedLibraryReorganizationAttentionReason::Untracked,
                });
                continue;
            };
            if record.content_sha256 != fingerprint.sha256 || record.size_bytes != fingerprint.size
            {
                attention.push(ManagedLibraryReorganizationAttention {
                    source_path: source_path.clone(),
                    reason: ManagedLibraryReorganizationAttentionReason::Changed,
                });
                continue;
            }
            let Some(before) = before_by_id.get(record.destination_id.as_str()) else {
                attention.push(ManagedLibraryReorganizationAttention {
                    source_path: source_path.clone(),
                    reason: ManagedLibraryReorganizationAttentionReason::UnknownDestination {
                        destination_id: record.destination_id.clone(),
                    },
                });
                continue;
            };
            let Some(suffix) = subtree_suffix(source_path, &before.path) else {
                attention.push(ManagedLibraryReorganizationAttention {
                    source_path: source_path.clone(),
                    reason:
                        ManagedLibraryReorganizationAttentionReason::OutsideRecordedDestination {
                            destination_id: record.destination_id.clone(),
                        },
                });
                continue;
            };
            let (target, requested_destination) =
                match after_by_id.get(record.destination_id.as_str()) {
                    Some(after) if after.path == before.path => continue,
                    Some(after) => (
                        ManagedLibraryReorganizationTarget::Approved {
                            destination_id: record.destination_id.clone(),
                        },
                        format!("{}/{suffix}", after.path),
                    ),
                    None => (
                        ManagedLibraryReorganizationTarget::Recents {
                            removed_destination_id: record.destination_id.clone(),
                        },
                        format!("Recents/{}", file_name(source_path)?),
                    ),
                };
            let destination_path = resolve_collision(source, &requested_destination, &reserved)?;
            reserved.insert(destination_path.clone());
            collect_missing_directories(source, parent(&destination_path)?, &mut directories)?;
            entries.push(ManagedLibraryReorganizationEntry {
                file_id: format!("r{:06}", index + 1),
                source_path: source_path.clone(),
                source_fingerprint: fingerprint.clone(),
                target,
                requested_destination,
                destination_path,
            });
        }
        entries.sort_by(|left, right| left.file_id.cmp(&right.file_id));
        attention.sort_by(|left, right| left.source_path.cmp(&right.source_path));
        let mut directories = directories.into_iter().collect::<Vec<_>>();
        directories.sort_by(|left, right| directory_order(left, right));
        let plan = Self {
            version: 1,
            id,
            workspace_id,
            configure_run_id,
            configure_plan_id: configure.id.clone(),
            source: configure.source.clone(),
            source_identity,
            folder_set_path,
            folder_set_sha256: configure.after_folder_set_sha256.clone(),
            before_folders: configure.before_folders.clone(),
            after_folders: configure.after_folders.clone(),
            directories,
            entries,
            attention,
        };
        plan.validate()?;
        Ok(plan)
    }

    pub fn load(path: &Path) -> Result<Self, Error> {
        let plan: Self =
            serde_json::from_reader(fs::File::open(path).map_err(|source| Error::ReadFile {
                path: path.display().to_string(),
                source,
            })?)?;
        plan.validate()?;
        Ok(plan)
    }

    pub fn validate(&self) -> Result<(), Error> {
        if self.version != 1
            || self.id.trim().is_empty()
            || self.workspace_id.trim().is_empty()
            || self.configure_run_id.trim().is_empty()
            || self.configure_plan_id.trim().is_empty()
        {
            return Err(Error::InvalidArtifact(
                "invalid managed Library reorganization identity".into(),
            ));
        }
        if !Path::new(&self.source).is_absolute() || !Path::new(&self.folder_set_path).is_absolute()
        {
            return Err(Error::InvalidArtifact(
                "managed Library reorganization paths must be absolute".into(),
            ));
        }
        self.before_folders.validate()?;
        self.after_folders.validate()?;
        if self.before_folders.source != self.source
            || self.after_folders.source != self.source
            || self.after_folders.sha256()? != self.folder_set_sha256
        {
            return Err(Error::InvalidArtifact(
                "managed Library reorganization FolderSet binding does not match".into(),
            ));
        }
        validate_digest(&self.folder_set_sha256)?;
        validate_directories(&self.directories, &self.entries)?;
        let before_by_id = self
            .before_folders
            .folders
            .iter()
            .map(|folder| (folder.id.as_str(), folder))
            .collect::<HashMap<_, _>>();
        let after_by_id = self
            .after_folders
            .folders
            .iter()
            .map(|folder| (folder.id.as_str(), folder))
            .collect::<HashMap<_, _>>();
        let mut ids = HashSet::new();
        let mut sources = HashSet::new();
        let mut destinations = HashSet::new();
        for entry in &self.entries {
            if entry.file_id.trim().is_empty() || !ids.insert(entry.file_id.as_str()) {
                return Err(Error::InvalidArtifact(
                    "reorganization file IDs must be non-empty and unique".into(),
                ));
            }
            normalize_relative_path(&entry.source_path)?;
            normalize_relative_path(&entry.requested_destination)?;
            normalize_relative_path(&entry.destination_path)?;
            if !entry.source_path.starts_with("AI Library/")
                || !sources.insert(entry.source_path.as_str())
                || !destinations.insert(entry.destination_path.as_str())
                || entry.source_path == entry.destination_path
            {
                return Err(Error::InvalidArtifact(
                    "reorganization move paths are invalid or duplicated".into(),
                ));
            }
            validate_fingerprint(&entry.source_fingerprint)?;
            match &entry.target {
                ManagedLibraryReorganizationTarget::Approved { destination_id } => {
                    let before = before_by_id.get(destination_id.as_str()).ok_or_else(|| {
                        Error::InvalidArtifact("unknown previous destination ID".into())
                    })?;
                    let after = after_by_id.get(destination_id.as_str()).ok_or_else(|| {
                        Error::InvalidArtifact("unknown current destination ID".into())
                    })?;
                    let suffix =
                        subtree_suffix(&entry.source_path, &before.path).ok_or_else(|| {
                            Error::InvalidArtifact(
                                "source is outside its previous destination".into(),
                            )
                        })?;
                    if before.path == after.path
                        || entry.requested_destination != format!("{}/{suffix}", after.path)
                        || !path_in_subtree(&entry.destination_path, &after.path)
                    {
                        return Err(Error::InvalidArtifact(
                            "approved reorganization destination does not match its stable ID"
                                .into(),
                        ));
                    }
                }
                ManagedLibraryReorganizationTarget::Recents {
                    removed_destination_id,
                } => {
                    let before = before_by_id
                        .get(removed_destination_id.as_str())
                        .ok_or_else(|| {
                            Error::InvalidArtifact("unknown removed destination ID".into())
                        })?;
                    if after_by_id.contains_key(removed_destination_id.as_str())
                        || subtree_suffix(&entry.source_path, &before.path).is_none()
                        || entry.requested_destination
                            != format!("Recents/{}", file_name(&entry.source_path)?)
                        || parent(&entry.destination_path)? != "Recents"
                    {
                        return Err(Error::InvalidArtifact(
                            "removed destination files must return directly to Recents".into(),
                        ));
                    }
                }
            }
        }
        for item in &self.attention {
            normalize_relative_path(&item.source_path)?;
            if !item.source_path.starts_with("AI Library/")
                || !sources.insert(item.source_path.as_str())
            {
                return Err(Error::InvalidArtifact(
                    "reorganization attention path is outside AI Library or duplicated".into(),
                ));
            }
        }
        Ok(())
    }

    pub fn sha256(&self) -> Result<String, Error> {
        self.validate()?;
        Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(self)?)))
    }

    pub(crate) fn move_manifest(&self) -> Result<ValidatedMoveManifest, Error> {
        self.validate()?;
        Ok(ValidatedMoveManifest {
            digest: self.sha256()?,
            source: self.source.clone(),
            source_identity: self.source_identity.clone(),
            directories: self.directories.clone(),
            moves: self
                .entries
                .iter()
                .map(|entry| ValidatedMove {
                    file_id: entry.file_id.clone(),
                    source_path: entry.source_path.clone(),
                    destination_path: entry.destination_path.clone(),
                    fingerprint: entry.source_fingerprint.clone(),
                })
                .collect(),
        })
    }
}

fn validate_directories(
    directories: &[String],
    entries: &[ManagedLibraryReorganizationEntry],
) -> Result<(), Error> {
    let mut seen = HashSet::new();
    for pair in directories.windows(2) {
        if directory_order(&pair[0], &pair[1]) == Ordering::Greater {
            return Err(Error::InvalidArtifact(
                "reorganization directories must be parent-first".into(),
            ));
        }
    }
    for directory in directories {
        normalize_relative_path(directory)?;
        if !seen.insert(directory.as_str())
            || !entries.iter().any(|entry| {
                parent(&entry.destination_path).is_ok_and(|target| {
                    target == directory || target.starts_with(&format!("{directory}/"))
                })
            })
        {
            return Err(Error::InvalidArtifact(
                "reorganization contains a duplicate or unused directory".into(),
            ));
        }
    }
    Ok(())
}

fn subtree_suffix<'a>(path: &'a str, subtree: &str) -> Option<&'a str> {
    path.strip_prefix(subtree)?.strip_prefix('/')
}

fn path_in_subtree(path: &str, subtree: &str) -> bool {
    path == subtree || path.starts_with(&format!("{subtree}/"))
}

fn file_name(path: &str) -> Result<&str, Error> {
    path.rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| {
            Error::InvalidArtifact(format!("reorganization source has no file name: {path:?}"))
        })
}

fn parent(path: &str) -> Result<&str, Error> {
    path.rsplit_once('/')
        .map(|(value, _)| value)
        .ok_or_else(|| {
            Error::InvalidArtifact(format!(
                "reorganization destination has no parent: {path:?}"
            ))
        })
}

fn resolve_collision(
    source: &Path,
    requested: &str,
    reserved: &HashSet<String>,
) -> Result<String, Error> {
    let directory = parent(requested)?;
    let name = file_name(requested)?;
    let path = Path::new(name);
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .ok_or_else(|| {
            Error::InvalidArtifact(format!("reorganization file name is not UTF-8: {name:?}"))
        })?;
    let extension = path.extension().and_then(|value| value.to_str());
    for suffix in 0_u64.. {
        let candidate_name = if suffix == 0 {
            name.to_owned()
        } else {
            match extension {
                Some(extension) => format!("{stem} {suffix}.{extension}"),
                None => format!("{stem} {suffix}"),
            }
        };
        let relative = format!("{directory}/{candidate_name}");
        if !reserved.contains(&relative) && !path_exists(&checked_join(source, &relative)?)? {
            return Ok(relative);
        }
    }
    unreachable!("numeric collision suffix exhausted")
}

fn collect_missing_directories(
    source: &Path,
    relative: &str,
    missing: &mut BTreeSet<String>,
) -> Result<(), Error> {
    let mut current = String::new();
    for component in relative.split('/') {
        if !current.is_empty() {
            current.push('/');
        }
        current.push_str(component);
        let absolute = checked_join(source, &current)?;
        match fs::symlink_metadata(&absolute) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(_) => {
                return Err(Error::InvalidArtifact(format!(
                    "reorganization destination component is unsafe: {current:?}"
                )));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                missing.insert(current.clone());
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

fn directory_order(left: &str, right: &str) -> Ordering {
    left.split('/')
        .count()
        .cmp(&right.split('/').count())
        .then_with(|| left.cmp(right))
}

fn validate_digest(value: &str) -> Result<(), Error> {
    if value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(Error::InvalidArtifact(
            "reorganization digest must be lowercase SHA-256".into(),
        ))
    }
}

fn validate_fingerprint(value: &FileFingerprint) -> Result<(), Error> {
    validate_digest(&value.sha256)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    use crate::{
        ClassificationBasis, FolderProposal, ManagedDescendantPolicy, ManagedLibraryEdit, Proposal,
        ScanScope, SourceLock, UndoState, ai_library_folder_set,
        apply::apply_validated_move_manifest,
        filesystem::{canonical_directory, fingerprint},
        undo_session,
    };

    fn folders(source: &Path) -> FolderSet {
        ai_library_folder_set(
            &Proposal {
                version: 2,
                source: source.display().to_string(),
                scope: ScanScope::default(),
                files_considered: 2,
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
            .unwrap(),
        )
        .unwrap()
    }

    fn configure(
        source: &Path,
        before: &FolderSet,
        operation: ManagedLibraryEdit,
    ) -> ManagedLibraryEditPlan {
        let (_, source_identity) = canonical_directory(source).unwrap();
        ManagedLibraryEditPlan::build(
            "configure-plan-1".into(),
            "workspace-1".into(),
            source_identity,
            source.join("before-folders.json").display().to_string(),
            before,
            vec![operation],
        )
        .unwrap()
    }

    fn processed(
        relative_path: &str,
        destination_id: &str,
        fingerprint: &FileFingerprint,
    ) -> ProcessedFileRecord {
        ProcessedFileRecord {
            monitor_id: "monitor-1".into(),
            file_identity: fingerprint.identity.clone(),
            relative_path: relative_path.into(),
            content_sha256: fingerprint.sha256.clone(),
            size_bytes: fingerprint.size,
            processing_signature: "signature-1".into(),
            run_id: "classify-run-1".into(),
            classification_basis: ClassificationBasis::Name,
            rule_id: None,
            destination_id: destination_id.into(),
            processed_unix_ms: 1,
        }
    }

    #[test]
    fn moves_a_preserved_destination_id_and_undoes_the_exact_path_change() {
        let root = tempdir().unwrap();
        let source = root.path().join("source");
        let original = source.join("AI Library/Documents/2026/report.txt");
        fs::create_dir_all(original.parent().unwrap()).unwrap();
        fs::write(&original, b"quarterly report").unwrap();
        let before = folders(&source);
        let documents = before
            .folders
            .iter()
            .find(|folder| folder.path == "AI Library/Documents")
            .unwrap();
        let configure = configure(
            &source,
            &before,
            ManagedLibraryEdit::Rename {
                id: documents.id.clone(),
                path: "Archive".into(),
                descendants: ManagedDescendantPolicy::Reject,
            },
        );
        let fingerprint = fingerprint(&original).unwrap();
        let plan = ManagedLibraryReorganizationPlan::build(
            "reorganization-plan-1".into(),
            "workspace-1".into(),
            "configure-run-1".into(),
            configure.source_identity.clone(),
            root.path().join("after-folders.json").display().to_string(),
            &configure,
            &[(
                "AI Library/Documents/2026/report.txt".into(),
                fingerprint.clone(),
            )],
            &[processed(
                "AI Library/Documents/2026/report.txt",
                &documents.id,
                &fingerprint,
            )],
        )
        .unwrap();

        assert_eq!(plan.entries.len(), 1);
        assert_eq!(
            plan.entries[0].target,
            ManagedLibraryReorganizationTarget::Approved {
                destination_id: documents.id.clone()
            }
        );
        assert_eq!(
            plan.entries[0].destination_path,
            "AI Library/Archive/2026/report.txt"
        );

        let apply_path = root.path().join("apply.json");
        let lock = SourceLock::acquire(&source).unwrap();
        let apply =
            apply_validated_move_manifest(plan.move_manifest().unwrap(), &apply_path, &lock)
                .unwrap();
        drop(lock);
        assert!(!original.exists());
        assert_eq!(
            fs::read(source.join("AI Library/Archive/2026/report.txt")).unwrap(),
            b"quarterly report"
        );

        let undo = undo_session(&apply, &root.path().join("undo.json")).unwrap();
        assert_eq!(undo.state, UndoState::Completed);
        assert_eq!(fs::read(&original).unwrap(), b"quarterly report");
        assert!(!source.join("AI Library/Archive/2026/report.txt").exists());
        assert!(!source.join("AI Library/Archive").exists());
    }

    #[test]
    fn returns_only_trusted_removed_destination_files_to_recents() {
        let root = tempdir().unwrap();
        let source = root.path().join("source");
        let tracked = source.join("AI Library/Documents/tracked.txt");
        let changed = source.join("AI Library/Documents/changed.txt");
        let untracked = source.join("AI Library/Documents/untracked.txt");
        fs::create_dir_all(tracked.parent().unwrap()).unwrap();
        fs::create_dir(source.join("Recents")).unwrap();
        fs::write(&tracked, b"tracked").unwrap();
        fs::write(&changed, b"changed").unwrap();
        fs::write(&untracked, b"untracked").unwrap();
        let before = folders(&source);
        let documents = before
            .folders
            .iter()
            .find(|folder| folder.path == "AI Library/Documents")
            .unwrap();
        let configure = configure(
            &source,
            &before,
            ManagedLibraryEdit::Delete {
                id: documents.id.clone(),
                descendants: ManagedDescendantPolicy::Reject,
            },
        );
        let tracked_fingerprint = fingerprint(&tracked).unwrap();
        let changed_fingerprint = fingerprint(&changed).unwrap();
        let untracked_fingerprint = fingerprint(&untracked).unwrap();
        let mut stale_record = processed(
            "AI Library/Documents/changed.txt",
            &documents.id,
            &changed_fingerprint,
        );
        stale_record.content_sha256 = "0".repeat(64);
        let plan = ManagedLibraryReorganizationPlan::build(
            "reorganization-plan-2".into(),
            "workspace-1".into(),
            "configure-run-2".into(),
            configure.source_identity.clone(),
            root.path().join("after-folders.json").display().to_string(),
            &configure,
            &[
                (
                    "AI Library/Documents/tracked.txt".into(),
                    tracked_fingerprint.clone(),
                ),
                (
                    "AI Library/Documents/changed.txt".into(),
                    changed_fingerprint,
                ),
                (
                    "AI Library/Documents/untracked.txt".into(),
                    untracked_fingerprint,
                ),
            ],
            &[
                processed(
                    "AI Library/Documents/tracked.txt",
                    &documents.id,
                    &tracked_fingerprint,
                ),
                stale_record,
            ],
        )
        .unwrap();

        assert_eq!(plan.entries.len(), 1);
        assert_eq!(plan.entries[0].destination_path, "Recents/tracked.txt");
        assert_eq!(
            plan.entries[0].target,
            ManagedLibraryReorganizationTarget::Recents {
                removed_destination_id: documents.id.clone()
            }
        );
        assert_eq!(plan.attention.len(), 2);
        assert!(plan.attention.iter().any(|item| {
            item.source_path == "AI Library/Documents/changed.txt"
                && item.reason == ManagedLibraryReorganizationAttentionReason::Changed
        }));
        assert!(plan.attention.iter().any(|item| {
            item.source_path == "AI Library/Documents/untracked.txt"
                && item.reason == ManagedLibraryReorganizationAttentionReason::Untracked
        }));

        let mut redirected = plan.clone();
        redirected.entries[0].destination_path = "AI Library/Images/tracked.txt".into();
        assert!(redirected.validate().is_err());

        let apply_path = root.path().join("apply.json");
        let lock = SourceLock::acquire(&source).unwrap();
        apply_validated_move_manifest(plan.move_manifest().unwrap(), &apply_path, &lock).unwrap();
        assert_eq!(
            fs::read(source.join("Recents/tracked.txt")).unwrap(),
            b"tracked"
        );
        assert_eq!(fs::read(&changed).unwrap(), b"changed");
        assert_eq!(fs::read(&untracked).unwrap(), b"untracked");
    }
}
