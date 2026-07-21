use std::{collections::HashSet, fs, path::Path};

use serde::{Deserialize, Serialize};

use crate::Error;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Proposal {
    pub version: u32,
    pub source: String,
    pub files_considered: usize,
    pub folders: Vec<FolderProposal>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FolderProposal {
    pub path: String,
    pub description: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FolderSet {
    pub version: u32,
    pub source: String,
    pub folders: Vec<ApprovedFolder>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovedFolder {
    pub id: String,
    pub path: String,
    pub description: String,
}

impl Proposal {
    pub fn load(path: &Path) -> Result<Self, Error> {
        let proposal: Self = read_json(path)?;
        if proposal.version != 1 {
            return Err(Error::InvalidArtifact(format!(
                "unsupported proposal version {}; expected 1",
                proposal.version
            )));
        }
        if !Path::new(&proposal.source).is_absolute()
            || proposal.source.chars().any(char::is_control)
        {
            return Err(Error::InvalidArtifact(
                "proposal source must be an absolute path without control characters".into(),
            ));
        }
        if proposal.folders.is_empty() {
            return Err(Error::InvalidArtifact(
                "proposal must contain at least one folder".into(),
            ));
        }
        Ok(proposal)
    }

    pub fn approve(self) -> Result<FolderSet, Error> {
        let mut paths = HashSet::new();
        let mut folders = Vec::with_capacity(self.folders.len());
        for (index, proposal) in self.folders.into_iter().enumerate() {
            let path = normalize_relative_path(&proposal.path)?;
            let comparison_key = path.to_lowercase();
            if !paths.insert(comparison_key) {
                return Err(Error::InvalidArtifact(format!(
                    "folder paths must be unique, ignoring case: {path:?}"
                )));
            }
            if proposal.description.trim().is_empty()
                || proposal.description.chars().any(char::is_control)
            {
                return Err(Error::InvalidArtifact(format!(
                    "folder {path:?} must have a non-empty single-line description"
                )));
            }
            folders.push(ApprovedFolder {
                id: format!("d{:06}", index + 1),
                path,
                description: proposal.description.trim().to_owned(),
            });
        }
        let folder_set = FolderSet {
            version: 1,
            source: self.source,
            folders,
        };
        folder_set.validate()?;
        Ok(folder_set)
    }
}

impl FolderSet {
    pub fn load(path: &Path) -> Result<Self, Error> {
        let folder_set: Self = read_json(path)?;
        folder_set.validate()?;
        Ok(folder_set)
    }

    pub fn validate(&self) -> Result<(), Error> {
        if self.version != 1 {
            return Err(Error::InvalidArtifact(format!(
                "unsupported folder-set version {}; expected 1",
                self.version
            )));
        }
        if !Path::new(&self.source).is_absolute() || self.source.chars().any(char::is_control) {
            return Err(Error::InvalidArtifact(
                "folder-set source must be an absolute path without control characters".into(),
            ));
        }
        if self.folders.is_empty() {
            return Err(Error::InvalidArtifact(
                "folder set must contain at least one approved folder".into(),
            ));
        }

        let mut ids = HashSet::new();
        let mut paths = HashSet::new();
        for folder in &self.folders {
            if folder.id.trim().is_empty() || !ids.insert(folder.id.as_str()) {
                return Err(Error::InvalidArtifact(format!(
                    "folder IDs must be non-empty and unique: {:?}",
                    folder.id
                )));
            }
            let normalized = normalize_relative_path(&folder.path)?;
            if normalized != folder.path {
                return Err(Error::InvalidArtifact(format!(
                    "approved folder path is not normalized: {:?}",
                    folder.path
                )));
            }
            if !paths.insert(normalized.to_lowercase()) {
                return Err(Error::InvalidArtifact(format!(
                    "folder paths must be unique, ignoring case: {:?}",
                    folder.path
                )));
            }
            if folder.description.trim().is_empty()
                || folder.description.chars().any(char::is_control)
            {
                return Err(Error::InvalidArtifact(format!(
                    "folder {:?} must have a non-empty single-line description",
                    folder.id
                )));
            }
        }
        Ok(())
    }
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, Error> {
    let text = fs::read_to_string(path).map_err(|source| Error::ReadFile {
        path: path.display().to_string(),
        source,
    })?;
    Ok(serde_json::from_str(&text)?)
}

pub(crate) fn normalize_relative_path(path: &str) -> Result<String, Error> {
    if path.is_empty()
        || path.trim() != path
        || Path::new(path).is_absolute()
        || path.contains('\\')
    {
        return Err(Error::InvalidArtifact(format!(
            "folder path must be a non-empty portable relative path: {path:?}"
        )));
    }
    if path.chars().any(char::is_control) {
        return Err(Error::InvalidArtifact(format!(
            "folder path must not contain control characters: {path:?}"
        )));
    }

    let components: Vec<_> = path.split('/').collect();
    if components.iter().any(|component| {
        component.is_empty()
            || *component == "."
            || *component == ".."
            || component.trim() != *component
    }) {
        return Err(Error::InvalidArtifact(format!(
            "folder path must use normalized child components: {path:?}"
        )));
    }
    Ok(path.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proposal(paths: &[&str]) -> Proposal {
        Proposal {
            version: 1,
            source: "/tmp/inbox".into(),
            files_considered: 3,
            folders: paths
                .iter()
                .map(|path| FolderProposal {
                    path: (*path).into(),
                    description: format!("Files for {path}"),
                })
                .collect(),
        }
    }

    #[test]
    fn approval_assigns_local_ids() {
        let folder_set = proposal(&["Work/Reports", "Images"]).approve().unwrap();

        assert_eq!(folder_set.folders[0].id, "d000001");
        assert_eq!(folder_set.folders[0].path, "Work/Reports");
    }

    #[test]
    fn approval_rejects_parent_traversal() {
        assert!(proposal(&["../Secrets"]).approve().is_err());
    }

    #[test]
    fn approval_rejects_case_insensitive_duplicate_paths() {
        assert!(proposal(&["Documents", "documents"]).approve().is_err());
    }

    #[test]
    fn approval_rejects_non_normalized_separators() {
        for path in ["Work//Reports", "Work/", "/Work", " Work", "Work/ Reports"] {
            assert!(proposal(&[path]).approve().is_err(), "accepted {path:?}");
        }
    }

    #[test]
    fn folder_set_rejects_duplicate_ids() {
        let mut folder_set = proposal(&["Documents", "Images"]).approve().unwrap();
        folder_set.folders[1].id = folder_set.folders[0].id.clone();

        assert!(folder_set.validate().is_err());
    }

    #[test]
    fn approval_rejects_control_characters_in_descriptions() {
        let mut proposal = proposal(&["Documents"]);
        proposal.folders[0].description = "Documents\nwith injected output".into();

        assert!(proposal.approve().is_err());
    }
}
