use std::{collections::HashSet, fs, path::Path};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::Error;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Proposal {
    pub version: u32,
    pub source: String,
    pub scope: ScanScope,
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
    pub scope: ScanScope,
    pub folders: Vec<ApprovedFolder>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScanScope {
    pub recursive_roots: Vec<String>,
}

impl ScanScope {
    pub fn new(mut recursive_roots: Vec<String>) -> Result<Self, Error> {
        recursive_roots.sort();
        let scope = Self { recursive_roots };
        scope.validate()?;
        Ok(scope)
    }

    pub fn validate(&self) -> Result<(), Error> {
        let mut previous: Option<&str> = None;
        for root in &self.recursive_roots {
            if root != "." {
                normalize_relative_path(root)?;
            }
            if let Some(previous) = previous {
                if previous >= root.as_str() {
                    return Err(Error::InvalidArtifact(
                        "recursive scope roots must be sorted and unique".into(),
                    ));
                }
                if previous == "." || root.starts_with(&format!("{previous}/")) {
                    return Err(Error::InvalidArtifact(format!(
                        "recursive scope roots must not overlap: {previous:?} and {root:?}"
                    )));
                }
            }
            previous = Some(root);
        }
        Ok(())
    }

    pub fn contains(&self, source_path: &str) -> bool {
        if !source_path.contains('/') {
            return true;
        }
        self.recursive_roots
            .iter()
            .any(|root| root == "." || source_path.starts_with(&format!("{root}/")))
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovedFolder {
    pub id: String,
    pub path: String,
    pub description: String,
    pub model_visible: bool,
    pub fallback: Option<FallbackCategory>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FallbackCategory {
    Pdf,
    Spreadsheets,
    Images,
    Videos,
    Audio,
    Archives,
    Code,
    Presentations,
    Miscellaneous,
}

impl FallbackCategory {
    pub const ALL: [Self; 9] = [
        Self::Pdf,
        Self::Spreadsheets,
        Self::Images,
        Self::Videos,
        Self::Audio,
        Self::Archives,
        Self::Code,
        Self::Presentations,
        Self::Miscellaneous,
    ];

    pub fn path(self) -> &'static str {
        match self {
            Self::Pdf => "Others/PDFs",
            Self::Spreadsheets => "Others/Spreadsheets",
            Self::Images => "Others/Images",
            Self::Videos => "Others/Videos",
            Self::Audio => "Others/Audio",
            Self::Archives => "Others/Archives",
            Self::Code => "Others/Code",
            Self::Presentations => "Others/Presentations",
            Self::Miscellaneous => "Others/Miscellaneous",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Pdf => "PDF files that could not be classified by meaning",
            Self::Spreadsheets => "Spreadsheets that could not be classified by meaning",
            Self::Images => "Images that could not be classified by meaning",
            Self::Videos => "Videos that could not be classified by meaning",
            Self::Audio => "Audio files that could not be classified by meaning",
            Self::Archives => "Archives that could not be classified by meaning",
            Self::Code => "Source files that could not be classified by meaning",
            Self::Presentations => "Presentations that could not be classified by meaning",
            Self::Miscellaneous => "Files that could not be classified by meaning or type",
        }
    }
}

impl Proposal {
    pub fn load(path: &Path) -> Result<Self, Error> {
        let proposal: Self = read_json(path)?;
        if proposal.version != 2 {
            return Err(Error::InvalidArtifact(format!(
                "unsupported proposal version {}; expected 2",
                proposal.version
            )));
        }
        proposal.scope.validate()?;
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
                model_visible: true,
                fallback: None,
            });
        }
        for category in FallbackCategory::ALL {
            if let Some(folder) = folders
                .iter_mut()
                .find(|folder| folder.path.eq_ignore_ascii_case(category.path()))
            {
                folder.fallback = Some(category);
                continue;
            }
            folders.push(ApprovedFolder {
                id: format!("d{:06}", folders.len() + 1),
                path: category.path().into(),
                description: category.description().into(),
                model_visible: false,
                fallback: Some(category),
            });
        }
        let folder_set = FolderSet {
            version: 3,
            source: self.source,
            scope: self.scope,
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
        if self.version != 3 {
            return Err(Error::InvalidArtifact(format!(
                "unsupported folder-set version {}; expected 3",
                self.version
            )));
        }
        self.scope.validate()?;
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
        let mut fallbacks = HashSet::new();
        let mut model_visible = 0;
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
            if let Some(category) = folder.fallback
                && !fallbacks.insert(category)
            {
                return Err(Error::InvalidArtifact(format!(
                    "fallback category must be unique: {category:?}"
                )));
            }
            model_visible += usize::from(folder.model_visible);
        }
        if model_visible == 0 {
            return Err(Error::InvalidArtifact(
                "folder set must contain at least one model-visible destination".into(),
            ));
        }
        for category in FallbackCategory::ALL {
            if !fallbacks.contains(&category) {
                return Err(Error::InvalidArtifact(format!(
                    "folder set is missing fallback category {category:?}"
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
            version: 2,
            source: "/tmp/recents".into(),
            scope: ScanScope::default(),
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
        assert_eq!(folder_set.version, 3);
        assert_eq!(folder_set.folders.len(), 11);
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

    #[test]
    fn approval_reuses_a_proposed_fallback_path() {
        let folder_set = proposal(&["Others/PDFs"]).approve().unwrap();

        assert_eq!(folder_set.folders.len(), FallbackCategory::ALL.len());
        assert_eq!(folder_set.folders[0].fallback, Some(FallbackCategory::Pdf));
    }

    #[test]
    fn folder_set_requires_every_fallback_category() {
        let mut folder_set = proposal(&["Documents"]).approve().unwrap();
        folder_set
            .folders
            .retain(|folder| folder.fallback != Some(FallbackCategory::Images));

        assert!(folder_set.validate().is_err());
    }

    #[test]
    fn folder_set_digest_is_stable_and_covers_approved_content() {
        let folder_set = proposal(&["Documents"]).approve().unwrap();
        assert_eq!(folder_set.sha256().unwrap(), folder_set.sha256().unwrap());

        let mut changed = folder_set.clone();
        changed.folders[0].description = "Changed description".into();
        assert_ne!(folder_set.sha256().unwrap(), changed.sha256().unwrap());
    }
}
