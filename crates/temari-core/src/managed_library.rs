use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{ApprovedFolder, Error, FolderSet, FsIdentity};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case", tag = "kind")]
pub enum ManagedLibraryEditDraft {
    Add {
        path: String,
        description: String,
    },
    Rename {
        id: String,
        path: String,
        descendants: ManagedDescendantPolicy,
    },
    EditDescription {
        id: String,
        description: String,
    },
    Delete {
        id: String,
        descendants: ManagedDescendantPolicy,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case", tag = "kind")]
pub enum ManagedLibraryEdit {
    Add {
        id: String,
        path: String,
        description: String,
    },
    Rename {
        id: String,
        path: String,
        descendants: ManagedDescendantPolicy,
    },
    EditDescription {
        id: String,
        description: String,
    },
    Delete {
        id: String,
        descendants: ManagedDescendantPolicy,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedDescendantPolicy {
    Reject,
    Cascade,
    Reparent,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedLibraryEditPlan {
    pub version: u32,
    pub id: String,
    pub workspace_id: String,
    pub source: String,
    pub source_identity: FsIdentity,
    pub before_folder_set_path: String,
    pub before_folder_set_sha256: String,
    pub before_folders: FolderSet,
    pub after_folders: FolderSet,
    pub after_folder_set_sha256: String,
    pub operations: Vec<ManagedLibraryEdit>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedLibraryEditState {
    Running,
    Completed,
    PartialFailure,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedLibraryEditSession {
    pub version: u32,
    pub id: String,
    pub run_id: String,
    pub plan_id: String,
    pub workspace_id: String,
    pub source: String,
    pub source_identity: FsIdentity,
    pub before_folder_set_path: String,
    pub before_folder_set_sha256: String,
    pub after_folder_set_path: String,
    pub after_folder_set_sha256: String,
    pub operations: Vec<ManagedLibraryEdit>,
    pub state: ManagedLibraryEditState,
    pub started_unix_ms: u64,
    pub finished_unix_ms: Option<u64>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedLibraryEditUndoSession {
    pub version: u32,
    pub id: String,
    pub apply_session_id: String,
    pub workspace_id: String,
    pub state: ManagedLibraryEditState,
    pub started_unix_ms: u64,
    pub finished_unix_ms: Option<u64>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedLibraryEditRedoSession {
    pub version: u32,
    pub id: String,
    pub undo_session_id: String,
    pub workspace_id: String,
    pub state: ManagedLibraryEditState,
    pub started_unix_ms: u64,
    pub finished_unix_ms: Option<u64>,
    pub error: Option<String>,
}

impl ManagedLibraryEditPlan {
    pub fn build(
        id: String,
        workspace_id: String,
        source_identity: FsIdentity,
        before_folder_set_path: String,
        current: &FolderSet,
        operations: Vec<ManagedLibraryEdit>,
    ) -> Result<Self, Error> {
        current.validate()?;
        if operations.is_empty() {
            return Err(Error::InvalidState(
                "AI Library edit Plan requires at least one operation".into(),
            ));
        }
        let before_folder_set_sha256 = current.sha256()?;
        let mut after_folders = current.clone();
        apply_edits(&mut after_folders, &operations)?;
        after_folders.validate()?;
        let after_folder_set_sha256 = after_folders.sha256()?;
        if after_folder_set_sha256 == before_folder_set_sha256 {
            return Err(Error::InvalidState(
                "library edit must change the approved folder set".into(),
            ));
        }
        let plan = Self {
            version: 2,
            id,
            workspace_id,
            source: current.source.clone(),
            source_identity,
            before_folder_set_path,
            before_folder_set_sha256,
            before_folders: current.clone(),
            after_folders,
            after_folder_set_sha256,
            operations,
        };
        plan.validate()?;
        Ok(plan)
    }

    pub fn load(path: &Path) -> Result<Self, Error> {
        let value: Self =
            serde_json::from_reader(std::fs::File::open(path).map_err(|source| {
                Error::ReadFile {
                    path: path.display().to_string(),
                    source,
                }
            })?)?;
        value.validate()?;
        Ok(value)
    }

    pub fn validate(&self) -> Result<(), Error> {
        if self.version != 2 {
            return Err(Error::InvalidArtifact(format!(
                "unsupported managed AI Library edit Plan version {}; expected 2",
                self.version
            )));
        }
        if self.id.trim().is_empty() || self.workspace_id.trim().is_empty() {
            return Err(Error::InvalidArtifact(
                "managed AI Library edit IDs must not be empty".into(),
            ));
        }
        if !Path::new(&self.source).is_absolute()
            || !Path::new(&self.before_folder_set_path).is_absolute()
        {
            return Err(Error::InvalidArtifact(
                "managed AI Library edit paths must be absolute".into(),
            ));
        }
        self.before_folders.validate()?;
        self.after_folders.validate()?;
        if self.before_folders.source != self.source
            || self.before_folders.sha256()? != self.before_folder_set_sha256
            || self.after_folders.source != self.source
            || self.after_folders.sha256()? != self.after_folder_set_sha256
        {
            return Err(Error::InvalidArtifact(
                "managed AI Library edit after FolderSet binding does not match".into(),
            ));
        }
        validate_digest(&self.before_folder_set_sha256)?;
        validate_digest(&self.after_folder_set_sha256)?;
        if self.operations.is_empty() {
            return Err(Error::InvalidArtifact(
                "managed AI Library edit Plan has no operations".into(),
            ));
        }
        let mut expected = self.before_folders.clone();
        apply_edits(&mut expected, &self.operations)?;
        expected.validate()?;
        if expected != self.after_folders {
            return Err(Error::InvalidArtifact(
                "managed AI Library edit operation does not produce its after FolderSet".into(),
            ));
        }
        Ok(())
    }

    pub fn removed_destination_ids(&self) -> Vec<&str> {
        self.before_folders
            .folders
            .iter()
            .filter(|before| {
                !self
                    .after_folders
                    .folders
                    .iter()
                    .any(|after| after.id == before.id)
            })
            .map(|folder| folder.id.as_str())
            .collect()
    }

    pub fn added_destination_ids(&self) -> Vec<&str> {
        self.after_folders
            .folders
            .iter()
            .filter(|after| {
                !self
                    .before_folders
                    .folders
                    .iter()
                    .any(|before| before.id == after.id)
            })
            .map(|folder| folder.id.as_str())
            .collect()
    }
}

impl ManagedLibraryEditSession {
    pub fn load(path: &Path) -> Result<Self, Error> {
        let value: Self =
            serde_json::from_reader(std::fs::File::open(path).map_err(|source| {
                Error::ReadFile {
                    path: path.display().to_string(),
                    source,
                }
            })?)?;
        if value.version != 2 {
            return Err(Error::InvalidArtifact(
                "unsupported managed AI Library edit Session version".into(),
            ));
        }
        Ok(value)
    }
}

impl ManagedLibraryEditRedoSession {
    pub fn load(path: &Path) -> Result<Self, Error> {
        let value: Self =
            serde_json::from_reader(std::fs::File::open(path).map_err(|source| {
                Error::ReadFile {
                    path: path.display().to_string(),
                    source,
                }
            })?)?;
        if value.version != 1 {
            return Err(Error::InvalidArtifact(
                "unsupported managed AI Library edit Redo Session version".into(),
            ));
        }
        Ok(value)
    }
}

fn apply_edits(folders: &mut FolderSet, operations: &[ManagedLibraryEdit]) -> Result<(), Error> {
    validate_managed_library(folders)?;
    for operation in operations {
        let before = folders.sha256()?;
        apply_edit(folders, operation)?;
        folders.validate()?;
        if folders.sha256()? == before {
            return Err(Error::InvalidState(
                "every AI Library edit operation must change the FolderSet".into(),
            ));
        }
    }
    Ok(())
}

fn apply_edit(folders: &mut FolderSet, operation: &ManagedLibraryEdit) -> Result<(), Error> {
    match operation {
        ManagedLibraryEdit::Add {
            id,
            path,
            description,
        } => {
            if id.trim().is_empty() || folders.folders.iter().any(|folder| folder.id == *id) {
                return Err(Error::InvalidArtifact(
                    "new AI Library destination ID is empty or already exists".into(),
                ));
            }
            folders.folders.push(ApprovedFolder {
                id: id.clone(),
                path: library_path(path)?,
                description: description.trim().to_owned(),
                model_visible: true,
                fallback: None,
            });
        }
        ManagedLibraryEdit::Rename {
            id,
            path,
            descendants,
        } => {
            let old_path = editable_folder(folders, id)?.path.clone();
            let new_path = library_path(path)?;
            update_descendants(folders, id, &old_path, &new_path, *descendants, false)?;
            editable_folder_mut(folders, id)?.path = new_path;
        }
        ManagedLibraryEdit::EditDescription { id, description } => {
            editable_folder_mut(folders, id)?.description = description.trim().to_owned();
        }
        ManagedLibraryEdit::Delete { id, descendants } => {
            let old_path = editable_folder(folders, id)?.path.clone();
            let visible = folders
                .folders
                .iter()
                .filter(|folder| folder.model_visible)
                .count();
            if visible == 1 {
                return Err(Error::InvalidState(
                    "the last model-visible Library destination cannot be deleted".into(),
                ));
            }
            update_descendants(folders, id, &old_path, "", *descendants, true)?;
            folders.folders.retain(|folder| folder.id != *id);
        }
    }
    Ok(())
}

fn editable_folder<'a>(folders: &'a FolderSet, id: &str) -> Result<&'a ApprovedFolder, Error> {
    let folder = folders
        .folders
        .iter()
        .find(|folder| folder.id == id)
        .ok_or_else(|| Error::InvalidState(format!("unknown Library destination {id:?}")))?;
    if !folder.model_visible || folder.fallback.is_some() {
        return Err(Error::InvalidState(
            "system fallback destinations cannot be edited".into(),
        ));
    }
    Ok(folder)
}

fn editable_folder_mut<'a>(
    folders: &'a mut FolderSet,
    id: &str,
) -> Result<&'a mut ApprovedFolder, Error> {
    let folder = folders
        .folders
        .iter_mut()
        .find(|folder| folder.id == id)
        .ok_or_else(|| Error::InvalidState(format!("unknown Library destination {id:?}")))?;
    if !folder.model_visible || folder.fallback.is_some() {
        return Err(Error::InvalidState(
            "system fallback destinations cannot be edited".into(),
        ));
    }
    Ok(folder)
}

fn update_descendants(
    folders: &mut FolderSet,
    id: &str,
    parent: &str,
    replacement: &str,
    policy: ManagedDescendantPolicy,
    deleting: bool,
) -> Result<(), Error> {
    let prefix = format!("{parent}/");
    let descendant_ids = folders
        .folders
        .iter()
        .filter(|folder| folder.id != id && folder.path.starts_with(&prefix))
        .map(|folder| folder.id.clone())
        .collect::<Vec<_>>();
    if descendant_ids.is_empty() {
        return Ok(());
    }
    if folders.folders.iter().any(|folder| {
        descendant_ids.contains(&folder.id) && (!folder.model_visible || folder.fallback.is_some())
    }) {
        return Err(Error::InvalidState(
            "AI Library subtree edits cannot modify system fallback destinations".into(),
        ));
    }
    match policy {
        ManagedDescendantPolicy::Reject => Err(Error::InvalidState(
            "an AI Library destination with approved descendants requires an explicit subtree policy"
                .into(),
        )),
        ManagedDescendantPolicy::Cascade if deleting => {
            folders
                .folders
                .retain(|folder| !descendant_ids.contains(&folder.id));
            Ok(())
        }
        ManagedDescendantPolicy::Cascade => {
            for folder in &mut folders.folders {
                if descendant_ids.contains(&folder.id) {
                    folder.path = format!("{replacement}{}", &folder.path[parent.len()..]);
                }
            }
            Ok(())
        }
        ManagedDescendantPolicy::Reparent => {
            let parent_directory = parent
                .rsplit_once('/')
                .map(|(value, _)| value)
                .ok_or_else(|| Error::InvalidArtifact("AI Library path has no parent".into()))?;
            for folder in &mut folders.folders {
                if descendant_ids.contains(&folder.id) {
                    folder.path = format!("{parent_directory}{}", &folder.path[parent.len()..]);
                }
            }
            Ok(())
        }
    }
}

fn validate_managed_library(folders: &FolderSet) -> Result<(), Error> {
    if folders
        .folders
        .iter()
        .all(|folder| folder.path.starts_with("AI Library/"))
    {
        Ok(())
    } else {
        Err(Error::InvalidArtifact(
            "managed FolderSet destinations must be inside AI Library".into(),
        ))
    }
}

fn library_path(path: &str) -> Result<String, Error> {
    if path == "AI Library" || path.starts_with("AI Library/") {
        return Err(Error::InvalidArtifact(
            "AI Library destination paths are relative to AI Library".into(),
        ));
    }
    Ok(format!("AI Library/{path}"))
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
            "FolderSet digest must be lowercase SHA-256".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FolderProposal, Proposal, ScanScope};

    fn folders() -> FolderSet {
        let approved = Proposal {
            version: 2,
            source: "/tmp/library-edit".into(),
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
        crate::ai_library_folder_set(&approved).unwrap()
    }

    fn plan(operation: ManagedLibraryEdit) -> ManagedLibraryEditPlan {
        ManagedLibraryEditPlan::build(
            "plan-1".into(),
            "workspace-1".into(),
            FsIdentity {
                device: 1,
                inode: 2,
            },
            "/tmp/folders.json".into(),
            &folders(),
            vec![operation],
        )
        .unwrap()
    }

    #[test]
    fn rename_and_description_edit_preserve_opaque_id() {
        let original = folders().folders[0].clone();
        let renamed = plan(ManagedLibraryEdit::Rename {
            id: original.id.clone(),
            path: "Archive".into(),
            descendants: ManagedDescendantPolicy::Reject,
        });
        let folder = renamed
            .after_folders
            .folders
            .iter()
            .find(|folder| folder.id == original.id)
            .unwrap();
        assert_eq!(folder.path, "AI Library/Archive");
        assert_eq!(folder.description, original.description);

        let described = plan(ManagedLibraryEdit::EditDescription {
            id: original.id.clone(),
            description: "Long-term records".into(),
        });
        let folder = described
            .after_folders
            .folders
            .iter()
            .find(|folder| folder.id == original.id)
            .unwrap();
        assert_eq!(folder.path, original.path);
        assert_eq!(folder.description, "Long-term records");
    }

    #[test]
    fn add_uses_the_core_issued_id_and_system_fallbacks_cannot_be_edited() {
        let added = plan(ManagedLibraryEdit::Add {
            id: "destination-new".into(),
            path: "Research".into(),
            description: "Research material".into(),
        });
        assert!(added.after_folders.folders.iter().any(|folder| {
            folder.id == "destination-new" && folder.path == "AI Library/Research"
        }));

        let fallback = folders()
            .folders
            .into_iter()
            .find(|folder| folder.fallback.is_some())
            .unwrap();
        assert!(
            ManagedLibraryEditPlan::build(
                "plan-2".into(),
                "workspace-1".into(),
                FsIdentity {
                    device: 1,
                    inode: 2,
                },
                "/tmp/folders.json".into(),
                &folders(),
                vec![ManagedLibraryEdit::Delete {
                    id: fallback.id,
                    descendants: ManagedDescendantPolicy::Reject,
                }],
            )
            .is_err()
        );
    }

    #[test]
    fn delete_rejects_the_last_model_visible_destination() {
        let mut current = folders();
        let first = current
            .folders
            .iter()
            .find(|folder| folder.model_visible)
            .unwrap()
            .id
            .clone();
        current
            .folders
            .retain(|folder| !folder.model_visible || folder.id == first);
        assert!(
            ManagedLibraryEditPlan::build(
                "plan-3".into(),
                "workspace-1".into(),
                FsIdentity {
                    device: 1,
                    inode: 2,
                },
                "/tmp/folders.json".into(),
                &current,
                vec![ManagedLibraryEdit::Delete {
                    id: first,
                    descendants: ManagedDescendantPolicy::Reject,
                }],
            )
            .is_err()
        );
    }

    #[test]
    fn plan_rejects_after_folders_not_produced_by_the_reviewed_operation() {
        let mut value = plan(ManagedLibraryEdit::EditDescription {
            id: "d000001".into(),
            description: "Reviewed description".into(),
        });
        value.after_folders.folders[0].description = "Injected description".into();
        value.after_folder_set_sha256 = value.after_folders.sha256().unwrap();

        assert!(value.validate().is_err());
    }

    #[test]
    fn batch_cascades_nested_paths_in_order_and_preserves_ids() {
        let mut current = folders();
        let parent = current
            .folders
            .iter()
            .find(|folder| folder.path == "AI Library/Documents")
            .unwrap()
            .clone();
        current.folders.push(ApprovedFolder {
            id: "destination-child".into(),
            path: "AI Library/Documents/Reports".into(),
            description: "Reports".into(),
            model_visible: true,
            fallback: None,
        });
        current.validate().unwrap();

        let plan = ManagedLibraryEditPlan::build(
            "plan-batch".into(),
            "workspace-1".into(),
            FsIdentity {
                device: 1,
                inode: 2,
            },
            "/tmp/folders.json".into(),
            &current,
            vec![
                ManagedLibraryEdit::Rename {
                    id: parent.id,
                    path: "Archive".into(),
                    descendants: ManagedDescendantPolicy::Cascade,
                },
                ManagedLibraryEdit::EditDescription {
                    id: "destination-child".into(),
                    description: "Archived reports".into(),
                },
                ManagedLibraryEdit::Add {
                    id: "destination-new".into(),
                    path: "Research".into(),
                    description: "Research".into(),
                },
            ],
        )
        .unwrap();

        let child = plan
            .after_folders
            .folders
            .iter()
            .find(|folder| folder.id == "destination-child")
            .unwrap();
        assert_eq!(child.path, "AI Library/Archive/Reports");
        assert_eq!(child.description, "Archived reports");
        assert!(plan.validate().is_ok());
    }

    #[test]
    fn nested_delete_requires_policy_and_reparent_preserves_child_id() {
        let mut current = folders();
        let parent = current
            .folders
            .iter()
            .find(|folder| folder.path == "AI Library/Documents")
            .unwrap()
            .clone();
        current.folders.push(ApprovedFolder {
            id: "destination-child".into(),
            path: "AI Library/Documents/Reports".into(),
            description: "Reports".into(),
            model_visible: true,
            fallback: None,
        });
        current.validate().unwrap();

        assert!(
            ManagedLibraryEditPlan::build(
                "plan-reject".into(),
                "workspace-1".into(),
                FsIdentity {
                    device: 1,
                    inode: 2
                },
                "/tmp/folders.json".into(),
                &current,
                vec![ManagedLibraryEdit::Delete {
                    id: parent.id.clone(),
                    descendants: ManagedDescendantPolicy::Reject,
                }],
            )
            .is_err()
        );

        let reparented = ManagedLibraryEditPlan::build(
            "plan-reparent".into(),
            "workspace-1".into(),
            FsIdentity {
                device: 1,
                inode: 2,
            },
            "/tmp/folders.json".into(),
            &current,
            vec![ManagedLibraryEdit::Delete {
                id: parent.id,
                descendants: ManagedDescendantPolicy::Reparent,
            }],
        )
        .unwrap();
        let child = reparented
            .after_folders
            .folders
            .iter()
            .find(|folder| folder.id == "destination-child")
            .unwrap();
        assert_eq!(child.path, "AI Library/Reports");
    }
}
