use std::{
    collections::{HashMap, HashSet},
    path::Path,
    thread,
    time::Duration,
};

use crate::{
    ApprovedFolder, Classification, ClassificationBasis, Classifier, ContentCandidate,
    ContentPolicy, Error, FallbackCategory, FileCandidate, NameClassification, NameDecision,
    artifact::normalize_relative_path,
};

#[derive(Clone, Copy, Debug)]
pub struct ClassificationOptions {
    pub content_policy: ContentPolicy,
    pub max_content_chars: usize,
    pub max_content_file_bytes: u64,
    pub name_batch_size: usize,
    pub content_batch_size: usize,
    pub batch_delay: Duration,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClassificationSummary {
    pub classifications: Vec<Classification>,
    pub by_name: usize,
    pub by_content: usize,
    pub by_fallback: usize,
}

pub trait ContentExtractor {
    fn extract(
        &self,
        source: &Path,
        file: &FileCandidate,
        max_chars: usize,
        max_file_bytes: u64,
    ) -> Option<ContentCandidate>;
}

pub fn classify_files<C: Classifier, E: ContentExtractor>(
    source: &Path,
    files: &[FileCandidate],
    folders: &[ApprovedFolder],
    classifier: &C,
    extractor: &E,
    options: ClassificationOptions,
) -> Result<ClassificationSummary, Error> {
    validate_options(options)?;
    for file in files {
        if normalize_relative_path(&file.source_path).is_err() {
            return Err(Error::InvalidArtifact(format!(
                "classification input is not a normalized relative source path: {:?}",
                file.source_path
            )));
        }
    }
    if files.is_empty() {
        return Ok(ClassificationSummary {
            classifications: Vec::new(),
            by_name: 0,
            by_content: 0,
            by_fallback: 0,
        });
    }
    let model_folders: Vec<_> = folders
        .iter()
        .filter(|folder| folder.model_visible)
        .cloned()
        .collect();
    if model_folders.is_empty() {
        return Err(Error::InvalidArtifact(
            "folder set contains no model-visible destinations".into(),
        ));
    }

    let mut resolved = Vec::with_capacity(files.len());
    let mut needs_content = Vec::new();
    for (index, batch) in files.chunks(options.name_batch_size).enumerate() {
        let decisions = classifier.classify_names(batch, &model_folders)?;
        let (mut direct, mut ambiguous) = validate_name_batch(batch, &model_folders, decisions)?;
        resolved.append(&mut direct);
        needs_content.append(&mut ambiguous);
        delay_between_batches(
            index,
            files.len(),
            options.name_batch_size,
            options.batch_delay,
        );
    }
    let by_name = resolved.len();

    let mut extracted = Vec::new();
    let mut fallback_files = Vec::new();
    if options.content_policy == ContentPolicy::OnDemand {
        for file in needs_content {
            match extractor.extract(
                source,
                &file,
                options.max_content_chars,
                options.max_content_file_bytes,
            ) {
                Some(content) => extracted.push(content),
                None => fallback_files.push(file),
            }
        }
    } else {
        fallback_files = needs_content;
    }

    let mut by_content = 0;
    for (index, batch) in extracted.chunks(options.content_batch_size).enumerate() {
        let mut classifications = classifier.classify_contents(batch, &model_folders)?;
        validate_content_batch(batch, &model_folders, &classifications)?;
        by_content += classifications.len();
        resolved.append(&mut classifications);
        delay_between_batches(
            index,
            extracted.len(),
            options.content_batch_size,
            options.batch_delay,
        );
    }

    for file in fallback_files {
        let category = fallback_category(&file.extension);
        let folder = folders
            .iter()
            .find(|folder| folder.fallback == Some(category))
            .ok_or_else(|| {
                Error::InvalidArtifact(format!(
                    "folder set is missing fallback category {category:?}"
                ))
            })?;
        resolved.push(Classification {
            file_id: file.id,
            destination_id: folder.id.clone(),
            reasoning: Some(format!("Local extension fallback for .{}", file.extension)),
            basis: ClassificationBasis::ExtensionFallback,
        });
    }
    let by_fallback = resolved.len() - by_name - by_content;
    validate_complete(files, folders, &resolved)?;
    resolved.sort_by(|left, right| left.file_id.cmp(&right.file_id));

    Ok(ClassificationSummary {
        classifications: resolved,
        by_name,
        by_content,
        by_fallback,
    })
}

fn validate_name_batch(
    files: &[FileCandidate],
    folders: &[ApprovedFolder],
    decisions: Vec<NameClassification>,
) -> Result<(Vec<Classification>, Vec<FileCandidate>), Error> {
    if decisions.len() != files.len() {
        return Err(Error::InvalidModelResponse(format!(
            "expected {} name classifications, received {}",
            files.len(),
            decisions.len()
        )));
    }
    let files_by_id: HashMap<_, _> = files.iter().map(|file| (&file.id, file)).collect();
    let folder_ids: HashSet<_> = folders.iter().map(|folder| folder.id.as_str()).collect();
    let mut seen = HashSet::new();
    let mut resolved = Vec::new();
    let mut needs_content = Vec::new();
    for classification in decisions {
        if !seen.insert(classification.file_id.clone()) {
            return Err(Error::InvalidModelResponse(format!(
                "duplicate file ID {:?}",
                classification.file_id
            )));
        }
        let file = files_by_id.get(&classification.file_id).ok_or_else(|| {
            Error::InvalidModelResponse(format!("unknown file ID {:?}", classification.file_id))
        })?;
        match classification.decision {
            NameDecision::Destination { destination_id } => {
                if !folder_ids.contains(destination_id.as_str()) {
                    return Err(Error::InvalidModelResponse(format!(
                        "unknown or local-only destination ID {destination_id:?}"
                    )));
                }
                resolved.push(Classification {
                    file_id: classification.file_id,
                    destination_id,
                    reasoning: classification.reasoning,
                    basis: ClassificationBasis::Name,
                });
            }
            NameDecision::NeedsContent => needs_content.push((*file).clone()),
        }
    }
    Ok((resolved, needs_content))
}

fn validate_content_batch(
    files: &[ContentCandidate],
    folders: &[ApprovedFolder],
    classifications: &[Classification],
) -> Result<(), Error> {
    if classifications.len() != files.len() {
        return Err(Error::InvalidModelResponse(format!(
            "expected {} content classifications, received {}",
            files.len(),
            classifications.len()
        )));
    }
    let file_ids: HashSet<_> = files.iter().map(|file| file.file_id.as_str()).collect();
    let folder_ids: HashSet<_> = folders.iter().map(|folder| folder.id.as_str()).collect();
    let mut seen = HashSet::new();
    for classification in classifications {
        if !file_ids.contains(classification.file_id.as_str()) {
            return Err(Error::InvalidModelResponse(format!(
                "unknown file ID {:?}",
                classification.file_id
            )));
        }
        if !seen.insert(classification.file_id.as_str()) {
            return Err(Error::InvalidModelResponse(format!(
                "duplicate file ID {:?}",
                classification.file_id
            )));
        }
        if !folder_ids.contains(classification.destination_id.as_str()) {
            return Err(Error::InvalidModelResponse(format!(
                "unknown or local-only destination ID {:?}",
                classification.destination_id
            )));
        }
        if classification.basis != ClassificationBasis::Content {
            return Err(Error::InvalidModelResponse(
                "content classification has an invalid basis".into(),
            ));
        }
    }
    Ok(())
}

fn validate_complete(
    files: &[FileCandidate],
    folders: &[ApprovedFolder],
    classifications: &[Classification],
) -> Result<(), Error> {
    if classifications.len() != files.len() {
        return Err(Error::InvalidModelResponse(format!(
            "expected {} resolved classifications, received {}",
            files.len(),
            classifications.len()
        )));
    }
    let file_ids: HashSet<_> = files.iter().map(|file| file.id.as_str()).collect();
    let folder_ids: HashSet<_> = folders.iter().map(|folder| folder.id.as_str()).collect();
    let mut seen = HashSet::new();
    for classification in classifications {
        if !file_ids.contains(classification.file_id.as_str())
            || !seen.insert(classification.file_id.as_str())
            || !folder_ids.contains(classification.destination_id.as_str())
        {
            return Err(Error::InvalidModelResponse(
                "resolved classifications do not match approved files and destinations".into(),
            ));
        }
    }
    Ok(())
}

fn validate_options(options: ClassificationOptions) -> Result<(), Error> {
    if options.name_batch_size == 0
        || options.content_batch_size == 0
        || options.max_content_chars == 0
        || options.max_content_file_bytes == 0
    {
        return Err(Error::InvalidConfig(
            "classification limits and batch sizes must be greater than zero".into(),
        ));
    }
    Ok(())
}

fn delay_between_batches(index: usize, total: usize, size: usize, delay: Duration) {
    if (index + 1) * size < total && !delay.is_zero() {
        thread::sleep(delay);
    }
}

fn fallback_category(extension: &str) -> FallbackCategory {
    match extension.to_ascii_lowercase().as_str() {
        "pdf" => FallbackCategory::Pdf,
        "xlsx" | "xls" | "csv" | "numbers" => FallbackCategory::Spreadsheets,
        "png" | "jpg" | "jpeg" | "heic" | "gif" | "svg" | "webp" | "tiff" | "bmp" => {
            FallbackCategory::Images
        }
        "mp4" | "mov" | "avi" | "mkv" | "wmv" => FallbackCategory::Videos,
        "mp3" | "wav" | "m4a" | "aac" | "flac" | "ogg" => FallbackCategory::Audio,
        "zip" | "rar" | "7z" | "tar" | "gz" | "dmg" => FallbackCategory::Archives,
        "js" | "ts" | "py" | "swift" | "java" | "css" | "html" | "json" | "sh" | "rb" | "go" => {
            FallbackCategory::Code
        }
        "pptx" | "ppt" | "key" => FallbackCategory::Presentations,
        _ => FallbackCategory::Miscellaneous,
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, fs};

    use tempfile::tempdir;

    use super::*;
    use crate::{ExtractionConfig, FolderProposal, LocalContentExtractor, Proposal};

    struct FakeClassifier {
        name_calls: RefCell<Vec<usize>>,
        content_calls: RefCell<Vec<Vec<String>>>,
    }

    struct LocalOnlyClassifier {
        destination_id: String,
    }

    impl Classifier for LocalOnlyClassifier {
        fn classify_names(
            &self,
            files: &[FileCandidate],
            _folders: &[ApprovedFolder],
        ) -> Result<Vec<NameClassification>, Error> {
            Ok(files
                .iter()
                .map(|file| NameClassification {
                    file_id: file.id.clone(),
                    decision: NameDecision::Destination {
                        destination_id: self.destination_id.clone(),
                    },
                    reasoning: None,
                })
                .collect())
        }

        fn classify_contents(
            &self,
            _files: &[ContentCandidate],
            _folders: &[ApprovedFolder],
        ) -> Result<Vec<Classification>, Error> {
            unreachable!()
        }
    }

    impl Classifier for FakeClassifier {
        fn classify_names(
            &self,
            files: &[FileCandidate],
            folders: &[ApprovedFolder],
        ) -> Result<Vec<NameClassification>, Error> {
            self.name_calls.borrow_mut().push(files.len());
            Ok(files
                .iter()
                .map(|file| NameClassification {
                    file_id: file.id.clone(),
                    decision: if file.source_path.starts_with("clear") {
                        NameDecision::Destination {
                            destination_id: folders[0].id.clone(),
                        }
                    } else {
                        NameDecision::NeedsContent
                    },
                    reasoning: None,
                })
                .collect())
        }

        fn classify_contents(
            &self,
            files: &[ContentCandidate],
            folders: &[ApprovedFolder],
        ) -> Result<Vec<Classification>, Error> {
            self.content_calls
                .borrow_mut()
                .push(files.iter().map(|file| file.file_id.clone()).collect());
            Ok(files
                .iter()
                .map(|file| Classification {
                    file_id: file.file_id.clone(),
                    destination_id: folders[0].id.clone(),
                    reasoning: None,
                    basis: ClassificationBasis::Content,
                })
                .collect())
        }
    }

    fn folders(source: &Path) -> Vec<ApprovedFolder> {
        Proposal {
            version: 2,
            source: source.display().to_string(),
            scope: crate::ScanScope::default(),
            files_considered: 2,
            folders: vec![FolderProposal {
                path: "Documents".into(),
                description: "Documents".into(),
            }],
        }
        .approve()
        .unwrap()
        .folders
    }

    fn options(policy: ContentPolicy) -> ClassificationOptions {
        ClassificationOptions {
            content_policy: policy,
            max_content_chars: 100,
            max_content_file_bytes: 1024,
            name_batch_size: 50,
            content_batch_size: 20,
            batch_delay: Duration::ZERO,
        }
    }

    fn extractor() -> LocalContentExtractor {
        LocalContentExtractor::new(ExtractionConfig {
            max_output_bytes: 1024,
            max_archive_entries: 100,
            max_expanded_bytes: 1024 * 1024,
            max_xml_events: 10_000,
            max_xml_depth: 64,
            timeout_seconds: 1,
            ocr: None,
        })
    }

    #[test]
    fn extracts_only_ambiguous_supported_files_and_falls_back_unsupported_files() {
        let source = tempdir().unwrap();
        fs::write(source.path().join("clear.txt"), "clear").unwrap();
        fs::write(source.path().join("ambiguous.txt"), "invoice content").unwrap();
        fs::write(source.path().join("ambiguous.png"), b"image").unwrap();
        let files = vec![
            FileCandidate {
                id: "f1".into(),
                source_path: "clear.txt".into(),
                extension: "txt".into(),
            },
            FileCandidate {
                id: "f2".into(),
                source_path: "ambiguous.txt".into(),
                extension: "txt".into(),
            },
            FileCandidate {
                id: "f3".into(),
                source_path: "ambiguous.png".into(),
                extension: "PNG".into(),
            },
        ];
        let folders = folders(source.path());
        let classifier = FakeClassifier {
            name_calls: RefCell::new(Vec::new()),
            content_calls: RefCell::new(Vec::new()),
        };

        let result = classify_files(
            source.path(),
            &files,
            &folders,
            &classifier,
            &extractor(),
            options(ContentPolicy::OnDemand),
        )
        .unwrap();

        assert_eq!(result.by_name, 1);
        assert_eq!(result.by_content, 1);
        assert_eq!(result.by_fallback, 1);
        assert_eq!(
            classifier.content_calls.borrow().as_slice(),
            &[vec![String::from("f2")]]
        );
        assert_eq!(
            result.classifications[2].basis,
            ClassificationBasis::ExtensionFallback
        );
        assert_eq!(
            folders
                .iter()
                .find(|folder| folder.id == result.classifications[2].destination_id)
                .unwrap()
                .fallback,
            Some(FallbackCategory::Images)
        );
    }

    #[test]
    fn metadata_only_never_calls_content_classifier() {
        let source = tempdir().unwrap();
        fs::write(source.path().join("ambiguous.txt"), "private content").unwrap();
        let files = vec![FileCandidate {
            id: "f1".into(),
            source_path: "ambiguous.txt".into(),
            extension: "txt".into(),
        }];
        let folders = folders(source.path());
        let classifier = FakeClassifier {
            name_calls: RefCell::new(Vec::new()),
            content_calls: RefCell::new(Vec::new()),
        };

        let result = classify_files(
            source.path(),
            &files,
            &folders,
            &classifier,
            &extractor(),
            options(ContentPolicy::MetadataOnly),
        )
        .unwrap();

        assert_eq!(result.by_fallback, 1);
        assert!(classifier.content_calls.borrow().is_empty());
        let plan = crate::build_plan(
            source.path(),
            &crate::ScanScope::default(),
            &files,
            &folders,
            result.classifications,
        )
        .unwrap();
        let artifact = serde_json::to_string(&plan).unwrap();
        assert!(!artifact.contains("private content"));
    }

    #[test]
    fn uses_reference_batch_sizes_without_delaying_tests() {
        let source = tempdir().unwrap();
        let files: Vec<_> = (0..51)
            .map(|index| FileCandidate {
                id: format!("f{index:06}"),
                source_path: format!("clear-{index}.txt"),
                extension: "txt".into(),
            })
            .collect();
        let folders = folders(source.path());
        let classifier = FakeClassifier {
            name_calls: RefCell::new(Vec::new()),
            content_calls: RefCell::new(Vec::new()),
        };

        let result = classify_files(
            source.path(),
            &files,
            &folders,
            &classifier,
            &extractor(),
            options(ContentPolicy::MetadataOnly),
        )
        .unwrap();

        assert_eq!(classifier.name_calls.borrow().as_slice(), &[50, 1]);
        assert_eq!(result.by_name, 51);
    }

    #[test]
    fn rejects_a_model_selected_local_only_fallback_id() {
        let source = tempdir().unwrap();
        let files = vec![FileCandidate {
            id: "f1".into(),
            source_path: "clear.txt".into(),
            extension: "txt".into(),
        }];
        let folders = folders(source.path());
        let fallback_id = folders
            .iter()
            .find(|folder| !folder.model_visible)
            .unwrap()
            .id
            .clone();

        let error = classify_files(
            source.path(),
            &files,
            &folders,
            &LocalOnlyClassifier {
                destination_id: fallback_id,
            },
            &extractor(),
            options(ContentPolicy::MetadataOnly),
        )
        .unwrap_err();

        assert!(error.to_string().contains("local-only destination"));
    }

    #[test]
    fn rejects_parent_traversal_before_content_extraction() {
        let source = tempdir().unwrap();
        let folders = folders(source.path());
        let files = vec![FileCandidate {
            id: "f1".into(),
            source_path: "../secret.txt".into(),
            extension: "txt".into(),
        }];
        let classifier = FakeClassifier {
            name_calls: RefCell::new(Vec::new()),
            content_calls: RefCell::new(Vec::new()),
        };

        let error = classify_files(
            source.path(),
            &files,
            &folders,
            &classifier,
            &extractor(),
            options(ContentPolicy::OnDemand),
        )
        .unwrap_err();

        assert!(error.to_string().contains("relative source path"));
        assert!(classifier.name_calls.borrow().is_empty());
    }
}
