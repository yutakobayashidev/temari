use std::{collections::HashSet, fs, path::Path};

use serde::{Deserialize, Serialize};

use crate::Error;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FileCandidate {
    pub id: String,
    pub name: String,
    pub extension: String,
}

pub fn scan_directory(source: &Path) -> Result<Vec<FileCandidate>, Error> {
    let entries = fs::read_dir(source).map_err(|source_error| Error::Scan {
        path: source.display().to_string(),
        source: source_error,
    })?;
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source_error| Error::Scan {
            path: source.display().to_string(),
            source: source_error,
        })?;
        let file_type = entry.file_type().map_err(|source_error| Error::Scan {
            path: entry.path().display().to_string(),
            source: source_error,
        })?;
        if file_type.is_file() {
            let name = entry.file_name().into_string().map_err(|_| {
                Error::InvalidArtifact(format!(
                    "source contains a non-UTF-8 file name: {:?}",
                    entry.path().display().to_string()
                ))
            })?;
            names.push(name);
        }
    }
    names.sort();

    Ok(names
        .into_iter()
        .enumerate()
        .map(|(index, name)| FileCandidate {
            id: format!("f{:06}", index + 1),
            extension: Path::new(&name)
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase(),
            name,
        })
        .collect())
}

pub fn select_representative_files(files: &[FileCandidate], limit: usize) -> Vec<FileCandidate> {
    if files.len() <= limit {
        return files.to_vec();
    }

    let mut selected_ids = HashSet::new();
    let mut selected = Vec::with_capacity(limit);
    let mut extensions = HashSet::new();
    for file in files {
        if selected.len() == limit {
            break;
        }
        if extensions.insert(file.extension.as_str()) {
            selected_ids.insert(file.id.as_str());
            selected.push(file.clone());
        }
    }
    for file in files {
        if selected.len() == limit {
            break;
        }
        if selected_ids.insert(file.id.as_str()) {
            selected.push(file.clone());
        }
    }
    selected.sort_by(|left, right| left.id.cmp(&right.id));
    selected
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::OsString,
        fs::File,
        os::unix::{ffi::OsStringExt, fs::symlink},
    };

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn scans_regular_files_only_in_stable_order() {
        let directory = tempdir().unwrap();
        File::create(directory.path().join("z.PDF")).unwrap();
        File::create(directory.path().join("a.txt")).unwrap();
        std::fs::create_dir(directory.path().join("nested")).unwrap();
        symlink(
            directory.path().join("a.txt"),
            directory.path().join("link.txt"),
        )
        .unwrap();

        let files = scan_directory(directory.path()).unwrap();

        assert_eq!(files.len(), 2);
        assert_eq!(files[0].id, "f000001");
        assert_eq!(files[0].name, "a.txt");
        assert_eq!(files[1].extension, "pdf");
    }

    #[test]
    fn representative_selection_prefers_extension_diversity() {
        let files = [
            FileCandidate {
                id: "f1".into(),
                name: "a.txt".into(),
                extension: "txt".into(),
            },
            FileCandidate {
                id: "f2".into(),
                name: "b.txt".into(),
                extension: "txt".into(),
            },
            FileCandidate {
                id: "f3".into(),
                name: "c.pdf".into(),
                extension: "pdf".into(),
            },
        ];

        let selected = select_representative_files(&files, 2);

        assert_eq!(
            selected
                .iter()
                .map(|file| file.id.as_str())
                .collect::<Vec<_>>(),
            vec!["f1", "f3"]
        );
    }

    #[test]
    fn rejects_non_utf8_file_names() {
        let directory = tempdir().unwrap();
        File::create(directory.path().join(OsString::from_vec(vec![0xff]))).unwrap();

        assert!(scan_directory(directory.path()).is_err());
    }
}
