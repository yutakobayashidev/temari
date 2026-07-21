use std::{collections::HashSet, fs, path::Path};

use serde::{Deserialize, Serialize};

use crate::{Error, ScanScope, filesystem::verify_existing_directory_chain};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FileCandidate {
    pub id: String,
    pub source_path: String,
    pub extension: String,
}

pub fn scan_directory(
    source: &Path,
    scope: &ScanScope,
    excluded_subtrees: &[String],
) -> Result<Vec<FileCandidate>, Error> {
    scope.validate()?;
    let mut paths = Vec::new();
    if scope
        .recursive_roots
        .first()
        .is_some_and(|root| root == ".")
    {
        scan_tree(source, "", excluded_subtrees, &mut paths)?;
    } else {
        scan_level(source, "", excluded_subtrees, false, &mut paths)?;
        for root in &scope.recursive_roots {
            verify_existing_directory_chain(source, root)?;
            scan_tree(&source.join(root), root, excluded_subtrees, &mut paths)?;
        }
    }
    paths.sort();
    paths.dedup();

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

fn scan_tree(
    directory: &Path,
    relative: &str,
    excluded_subtrees: &[String],
    paths: &mut Vec<String>,
) -> Result<(), Error> {
    scan_level(directory, relative, excluded_subtrees, true, paths)
}

fn scan_level(
    directory: &Path,
    relative: &str,
    excluded_subtrees: &[String],
    recursive: bool,
    paths: &mut Vec<String>,
) -> Result<(), Error> {
    let entries = fs::read_dir(directory).map_err(|source_error| Error::Scan {
        path: directory.display().to_string(),
        source: source_error,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source_error| Error::Scan {
            path: directory.display().to_string(),
            source: source_error,
        })?;
        let name = entry.file_name().into_string().map_err(|_| {
            Error::InvalidArtifact(format!(
                "source contains a non-UTF-8 path: {:?}",
                entry.path().display().to_string()
            ))
        })?;
        let child = if relative.is_empty() {
            name
        } else {
            format!("{relative}/{name}")
        };
        if is_excluded(&child, excluded_subtrees) {
            continue;
        }
        let file_type = entry.file_type().map_err(|source_error| Error::Scan {
            path: entry.path().display().to_string(),
            source: source_error,
        })?;
        if file_type.is_file() {
            paths.push(child);
        } else if recursive && file_type.is_dir() {
            scan_tree(&entry.path(), &child, excluded_subtrees, paths)?;
        }
    }
    Ok(())
}

fn is_excluded(path: &str, excluded_subtrees: &[String]) -> bool {
    let path = path.to_lowercase();
    excluded_subtrees.iter().any(|excluded| {
        let excluded = excluded.to_lowercase();
        path == excluded || path.starts_with(&format!("{excluded}/"))
    })
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
    fn scans_root_files_and_only_selected_subtrees_in_stable_order() {
        let directory = tempdir().unwrap();
        File::create(directory.path().join("z.PDF")).unwrap();
        fs::create_dir(directory.path().join("included")).unwrap();
        fs::create_dir(directory.path().join("ignored")).unwrap();
        File::create(directory.path().join("included/a.txt")).unwrap();
        File::create(directory.path().join("ignored/b.txt")).unwrap();
        symlink(
            directory.path().join("included"),
            directory.path().join("linked"),
        )
        .unwrap();

        let files = scan_directory(
            directory.path(),
            &ScanScope::new(vec!["included".into()]).unwrap(),
            &[],
        )
        .unwrap();

        assert_eq!(files.len(), 2);
        assert_eq!(files[0].source_path, "included/a.txt");
        assert_eq!(files[1].source_path, "z.PDF");
        assert_eq!(files[1].extension, "pdf");
    }

    #[test]
    fn dot_scope_scans_the_whole_tree_but_excludes_destination_subtrees() {
        let directory = tempdir().unwrap();
        fs::create_dir_all(directory.path().join("incoming/deep")).unwrap();
        fs::create_dir_all(directory.path().join("Documents/deep")).unwrap();
        File::create(directory.path().join("incoming/deep/a.txt")).unwrap();
        File::create(directory.path().join("Documents/deep/old.txt")).unwrap();

        let files = scan_directory(
            directory.path(),
            &ScanScope::new(vec![".".into()]).unwrap(),
            &["Documents".into()],
        )
        .unwrap();

        assert_eq!(files.len(), 1);
        assert_eq!(files[0].source_path, "incoming/deep/a.txt");
    }

    #[test]
    fn representative_selection_prefers_extension_diversity() {
        let files = [
            FileCandidate {
                id: "f1".into(),
                source_path: "a.txt".into(),
                extension: "txt".into(),
            },
            FileCandidate {
                id: "f2".into(),
                source_path: "b.txt".into(),
                extension: "txt".into(),
            },
            FileCandidate {
                id: "f3".into(),
                source_path: "nested/c.pdf".into(),
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
    fn rejects_non_utf8_paths_in_an_included_subtree() {
        let directory = tempdir().unwrap();
        fs::create_dir(directory.path().join("included")).unwrap();
        File::create(
            directory
                .path()
                .join("included")
                .join(OsString::from_vec(vec![0xff])),
        )
        .unwrap();

        assert!(
            scan_directory(
                directory.path(),
                &ScanScope::new(vec!["included".into()]).unwrap(),
                &[]
            )
            .is_err()
        );
    }

    #[test]
    fn scope_rejects_overlapping_roots() {
        assert!(ScanScope::new(vec!["a".into(), "a/b".into()]).is_err());
        assert!(ScanScope::new(vec![".".into(), "a".into()]).is_err());
    }
}
