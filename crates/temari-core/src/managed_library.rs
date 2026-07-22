use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{ApprovedFolder, Error, FolderSet, FsIdentity};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case", tag = "kind")]
pub enum ManagedLibraryEdit {
    Add { path: String, description: String },
    Rename { id: String, path: String },
    EditDescription { id: String, description: String },
    Delete { id: String },
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
    pub operation: ManagedLibraryEdit,
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
    pub operation: ManagedLibraryEdit,
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

impl ManagedLibraryEditPlan {
    pub fn build(
        id: String,
        workspace_id: String,
        source_identity: FsIdentity,
        before_folder_set_path: String,
        current: &FolderSet,
        operation: ManagedLibraryEdit,
        added_id: Option<String>,
    ) -> Result<Self, Error> {
        current.validate()?;
        let before_folder_set_sha256 = current.sha256()?;
        let mut after_folders = current.clone();
        apply_edit(&mut after_folders, &operation, added_id)?;
        after_folders.validate()?;
        let after_folder_set_sha256 = after_folders.sha256()?;
        if after_folder_set_sha256 == before_folder_set_sha256 {
            return Err(Error::InvalidState(
                "library edit must change the approved folder set".into(),
            ));
        }
        let plan = Self {
            version: 1,
            id,
            workspace_id,
            source: current.source.clone(),
            source_identity,
            before_folder_set_path,
            before_folder_set_sha256,
            before_folders: current.clone(),
            after_folders,
            after_folder_set_sha256,
            operation,
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
        if self.version != 1 {
            return Err(Error::InvalidArtifact(format!(
                "unsupported managed Library edit Plan version {}; expected 1",
                self.version
            )));
        }
        if self.id.trim().is_empty() || self.workspace_id.trim().is_empty() {
            return Err(Error::InvalidArtifact(
                "managed Library edit IDs must not be empty".into(),
            ));
        }
        if !Path::new(&self.source).is_absolute()
            || !Path::new(&self.before_folder_set_path).is_absolute()
        {
            return Err(Error::InvalidArtifact(
                "managed Library edit paths must be absolute".into(),
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
                "managed Library edit after FolderSet binding does not match".into(),
            ));
        }
        validate_digest(&self.before_folder_set_sha256)?;
        validate_digest(&self.after_folder_set_sha256)?;
        let added_id = match &self.operation {
            ManagedLibraryEdit::Add { .. } => {
                let added = self
                    .after_folders
                    .folders
                    .iter()
                    .filter(|folder| {
                        !self
                            .before_folders
                            .folders
                            .iter()
                            .any(|before| before.id == folder.id)
                    })
                    .map(|folder| folder.id.clone())
                    .collect::<Vec<_>>();
                if added.len() != 1 {
                    return Err(Error::InvalidArtifact(
                        "managed Library Add must introduce exactly one opaque ID".into(),
                    ));
                }
                added.into_iter().next()
            }
            _ => None,
        };
        let mut expected = self.before_folders.clone();
        apply_edit(&mut expected, &self.operation, added_id)?;
        expected.validate()?;
        if expected != self.after_folders {
            return Err(Error::InvalidArtifact(
                "managed Library edit operation does not produce its after FolderSet".into(),
            ));
        }
        Ok(())
    }

    pub fn removed_destination_id(&self) -> Option<&str> {
        match &self.operation {
            ManagedLibraryEdit::Delete { id } => Some(id),
            _ => None,
        }
    }

    pub fn paths(&self) -> Option<(&str, &str)> {
        match &self.operation {
            ManagedLibraryEdit::Rename { id, .. } => {
                let before = self
                    .before_folders
                    .folders
                    .iter()
                    .find(|folder| folder.id == *id)?;
                let after = self
                    .after_folders
                    .folders
                    .iter()
                    .find(|folder| folder.id == *id)?;
                Some((&before.path, &after.path))
            }
            _ => None,
        }
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
        if value.version != 1 {
            return Err(Error::InvalidArtifact(
                "unsupported managed Library edit Session version".into(),
            ));
        }
        Ok(value)
    }
}

fn apply_edit(
    folders: &mut FolderSet,
    operation: &ManagedLibraryEdit,
    added_id: Option<String>,
) -> Result<(), Error> {
    let library_root = managed_library_root(folders)?;
    match operation {
        ManagedLibraryEdit::Add { path, description } => {
            let id = added_id.ok_or_else(|| {
                Error::InvalidState("adding a Library destination requires a new opaque ID".into())
            })?;
            if folders.folders.iter().any(|folder| folder.id == id) {
                return Err(Error::InvalidArtifact(
                    "new Library destination ID already exists".into(),
                ));
            }
            folders.folders.push(ApprovedFolder {
                id,
                path: library_path(library_root, path)?,
                description: description.trim().to_owned(),
                model_visible: true,
                fallback: None,
            });
        }
        ManagedLibraryEdit::Rename { id, path } => {
            let old_path = editable_folder(folders, id)?.path.clone();
            reject_approved_descendant(folders, id, &old_path)?;
            editable_folder_mut(folders, id)?.path = library_path(library_root, path)?;
        }
        ManagedLibraryEdit::EditDescription { id, description } => {
            editable_folder_mut(folders, id)?.description = description.trim().to_owned();
        }
        ManagedLibraryEdit::Delete { id } => {
            let old_path = editable_folder(folders, id)?.path.clone();
            reject_approved_descendant(folders, id, &old_path)?;
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

fn reject_approved_descendant(folders: &FolderSet, id: &str, parent: &str) -> Result<(), Error> {
    let prefix = format!("{parent}/");
    if folders
        .folders
        .iter()
        .any(|folder| folder.id != id && folder.path.starts_with(&prefix))
    {
        return Err(Error::InvalidState(
            "a Library destination with approved descendants cannot be renamed or deleted".into(),
        ));
    }
    Ok(())
}

fn managed_library_root(folders: &FolderSet) -> Result<&'static str, Error> {
    let has_current = folders
        .folders
        .iter()
        .all(|folder| folder.path.starts_with("AI Library/"));
    let has_legacy = folders
        .folders
        .iter()
        .all(|folder| folder.path.starts_with("Library/"));
    match (has_current, has_legacy) {
        (true, false) => Ok("AI Library"),
        (false, true) => Ok("Library"),
        _ => Err(Error::InvalidArtifact(
            "managed FolderSet does not use one recognized Library root".into(),
        )),
    }
}

fn library_path(library_root: &str, path: &str) -> Result<String, Error> {
    if ["Library", "AI Library"]
        .iter()
        .any(|root| path == *root || path.starts_with(&format!("{root}/")))
    {
        return Err(Error::InvalidArtifact(
            "Library destination paths are relative to Library".into(),
        ));
    }
    Ok(format!("{library_root}/{path}"))
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
        crate::library_folder_set(&approved).unwrap()
    }

    fn plan(operation: ManagedLibraryEdit, added_id: Option<&str>) -> ManagedLibraryEditPlan {
        ManagedLibraryEditPlan::build(
            "plan-1".into(),
            "workspace-1".into(),
            FsIdentity {
                device: 1,
                inode: 2,
            },
            "/tmp/folders.json".into(),
            &folders(),
            operation,
            added_id.map(str::to_owned),
        )
        .unwrap()
    }

    #[test]
    fn rename_and_description_edit_preserve_opaque_id() {
        let original = folders().folders[0].clone();
        let renamed = plan(
            ManagedLibraryEdit::Rename {
                id: original.id.clone(),
                path: "Archive".into(),
            },
            None,
        );
        let folder = renamed
            .after_folders
            .folders
            .iter()
            .find(|folder| folder.id == original.id)
            .unwrap();
        assert_eq!(folder.path, "AI Library/Archive");
        assert_eq!(folder.description, original.description);

        let described = plan(
            ManagedLibraryEdit::EditDescription {
                id: original.id.clone(),
                description: "Long-term records".into(),
            },
            None,
        );
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
        let added = plan(
            ManagedLibraryEdit::Add {
                path: "Research".into(),
                description: "Research material".into(),
            },
            Some("destination-new"),
        );
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
                ManagedLibraryEdit::Delete { id: fallback.id },
                None,
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
                ManagedLibraryEdit::Delete { id: first },
                None,
            )
            .is_err()
        );
    }

    #[test]
    fn plan_rejects_after_folders_not_produced_by_the_reviewed_operation() {
        let mut value = plan(
            ManagedLibraryEdit::EditDescription {
                id: "d000001".into(),
                description: "Reviewed description".into(),
            },
            None,
        );
        value.after_folders.folders[0].description = "Injected description".into();
        value.after_folder_set_sha256 = value.after_folders.sha256().unwrap();

        assert!(value.validate().is_err());
    }
}
