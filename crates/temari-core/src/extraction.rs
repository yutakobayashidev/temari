use std::{
    collections::HashSet,
    fs::{self, File},
    io::{BufRead, BufReader, Read},
    path::Path,
    process::{Command, Stdio},
    time::Duration,
};

use quick_xml::{Reader, events::Event};
use tempfile::tempdir;
use wait_timeout::ChildExt;
use zip::ZipArchive;

use crate::{
    ContentCandidate, ContentExtractor, ExtractionConfig, FileCandidate,
    artifact::normalize_relative_path, filesystem::verify_existing_directory_chain,
};

#[derive(Clone, Debug)]
pub struct LocalContentExtractor {
    config: ExtractionConfig,
}

enum ExtractionOutcome {
    Extracted(String),
    Unsupported,
    Failed,
}

impl LocalContentExtractor {
    pub fn new(config: ExtractionConfig) -> Self {
        Self { config }
    }

    fn extract_path(&self, path: &Path, extension: &str, max_chars: usize) -> ExtractionOutcome {
        let extension = extension.to_ascii_lowercase();
        if extension == "pdf" {
            return pdf_extract::extract_text(path)
                .ok()
                .and_then(|text| bounded_text(&text, max_chars, self.config.max_output_bytes))
                .map_or(ExtractionOutcome::Failed, ExtractionOutcome::Extracted);
        }
        if is_direct_text_extension(&extension) {
            return fs::read(path)
                .ok()
                .and_then(|bytes| String::from_utf8(bytes).ok())
                .and_then(|text| bounded_text(&text, max_chars, self.config.max_output_bytes))
                .map_or(ExtractionOutcome::Failed, ExtractionOutcome::Extracted);
        }
        if is_document_extension(&extension) {
            return extract_document(path, &extension, max_chars, &self.config)
                .map_or(ExtractionOutcome::Failed, ExtractionOutcome::Extracted);
        }
        if is_ocr_extension(&extension) {
            return self
                .config
                .ocr
                .as_ref()
                .and_then(|_| self.extract_ocr(path, max_chars))
                .map_or(ExtractionOutcome::Failed, ExtractionOutcome::Extracted);
        }
        ExtractionOutcome::Unsupported
    }

    fn extract_ocr(&self, path: &Path, max_chars: usize) -> Option<String> {
        let ocr = self.config.ocr.as_ref()?;
        let directory = tempdir().ok()?;
        let output_base = directory.path().join("output");
        let output_path = directory.path().join("output.txt");
        let mut command = Command::new(&ocr.executable);
        command
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .current_dir(directory.path())
            .arg(path)
            .arg(&output_base);
        if let Some(data_dir) = &ocr.data_dir {
            command.arg("--tessdata-dir").arg(data_dir);
        }
        command.arg("-l").arg(ocr.languages.join("+"));

        let mut child = command.spawn().ok()?;
        let status = match child
            .wait_timeout(Duration::from_secs(self.config.timeout_seconds))
            .ok()?
        {
            Some(status) => status,
            None => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        };
        if !status.success() {
            return None;
        }
        let metadata = fs::symlink_metadata(&output_path).ok()?;
        if !metadata.file_type().is_file() || metadata.len() > self.config.max_output_bytes {
            return None;
        }
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        File::open(output_path)
            .ok()?
            .take(self.config.max_output_bytes + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() as u64 > self.config.max_output_bytes {
            return None;
        }
        bounded_text(
            std::str::from_utf8(&bytes).ok()?,
            max_chars,
            self.config.max_output_bytes,
        )
    }
}

impl ContentExtractor for LocalContentExtractor {
    fn extract(
        &self,
        source: &Path,
        file: &FileCandidate,
        max_chars: usize,
        max_file_bytes: u64,
    ) -> Option<ContentCandidate> {
        if normalize_relative_path(&file.source_path).is_err() {
            return None;
        }
        let relative = Path::new(&file.source_path);
        let parent = relative.parent()?;
        if !parent.as_os_str().is_empty()
            && verify_existing_directory_chain(source, parent.to_str()?).is_err()
        {
            return None;
        }
        let path = source.join(relative);
        let metadata = fs::symlink_metadata(&path).ok()?;
        if !metadata.file_type().is_file() || metadata.len() > max_file_bytes {
            return None;
        }
        match self.extract_path(&path, &file.extension, max_chars) {
            ExtractionOutcome::Extracted(content) if !content.trim().is_empty() => {
                Some(ContentCandidate {
                    file_id: file.id.clone(),
                    source_path: file.source_path.clone(),
                    content,
                })
            }
            ExtractionOutcome::Extracted(_)
            | ExtractionOutcome::Unsupported
            | ExtractionOutcome::Failed => None,
        }
    }
}

fn extract_document(
    path: &Path,
    extension: &str,
    max_chars: usize,
    config: &ExtractionConfig,
) -> Option<String> {
    let mut archive = ZipArchive::new(File::open(path).ok()?).ok()?;
    if archive.len() > config.max_archive_entries {
        return None;
    }
    let mut expanded = 0_u64;
    let mut compressed_ranges = Vec::with_capacity(archive.len());
    let mut names = HashSet::with_capacity(archive.len());
    for index in 0..archive.len() {
        let entry = archive.by_index_raw(index).ok()?;
        if entry.encrypted() {
            return None;
        }
        if !names.insert(entry.name().to_owned()) {
            return None;
        }
        expanded = expanded.checked_add(entry.size())?;
        if expanded > config.max_expanded_bytes {
            return None;
        }
        let end = entry.data_start().checked_add(entry.compressed_size())?;
        compressed_ranges.push((entry.data_start(), end));
    }
    compressed_ranges.sort_unstable();
    if compressed_ranges
        .windows(2)
        .any(|pair| pair[0].1 > pair[1].0)
    {
        return None;
    }

    let mut selected = Vec::new();
    for index in 0..archive.len() {
        let name = archive.name_for_index(index)?.to_owned();
        if selected_document_entry(extension, &name) {
            selected.push((document_entry_order(extension, &name), name, index));
        }
    }
    selected.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));

    let mut output = TextBudget::new(max_chars, config.max_output_bytes);
    let mut remaining_events = config.max_xml_events;
    for (_, _, index) in &selected {
        let entry = archive.by_index(*index).ok()?;
        let input = BufReader::new(entry.take(config.max_expanded_bytes + 1));
        parse_xml(input, &mut output, config, &mut remaining_events)?;
        if output.full {
            break;
        }
    }
    if selected.is_empty() || output.text.trim().is_empty() {
        None
    } else {
        Some(output.text)
    }
}

fn parse_xml<R: BufRead>(
    input: R,
    output: &mut TextBudget,
    config: &ExtractionConfig,
    remaining_events: &mut usize,
) -> Option<()> {
    let mut reader = Reader::from_reader(input);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut depth = 0_usize;
    loop {
        if *remaining_events == 0 {
            return None;
        }
        *remaining_events -= 1;
        match reader.read_event_into(&mut buffer).ok()? {
            Event::Start(_) => {
                depth = depth.checked_add(1)?;
                if depth > config.max_xml_depth {
                    return None;
                }
            }
            Event::End(_) => depth = depth.checked_sub(1)?,
            Event::Text(text) => {
                let text = text.decode().ok()?;
                output.push(&text);
                if output.full {
                    return Some(());
                }
            }
            Event::CData(text) => {
                let text = text.decode().ok()?;
                output.push(&text);
                if output.full {
                    return Some(());
                }
            }
            Event::DocType(_) => return None,
            Event::Eof => return (depth == 0).then_some(()),
            _ => {}
        }
        buffer.clear();
    }
}

struct TextBudget {
    text: String,
    chars: usize,
    bytes: u64,
    max_chars: usize,
    max_bytes: u64,
    full: bool,
}

impl TextBudget {
    fn new(max_chars: usize, max_bytes: u64) -> Self {
        Self {
            text: String::new(),
            chars: 0,
            bytes: 0,
            max_chars,
            max_bytes,
            full: false,
        }
    }

    fn push(&mut self, value: &str) {
        for character in value.chars().chain(std::iter::once(' ')) {
            let bytes = character.len_utf8() as u64;
            if self.chars == self.max_chars || self.bytes + bytes > self.max_bytes {
                self.full = true;
                return;
            }
            self.text.push(character);
            self.chars += 1;
            self.bytes += bytes;
        }
    }
}

fn bounded_text(value: &str, max_chars: usize, max_bytes: u64) -> Option<String> {
    let mut output = TextBudget::new(max_chars, max_bytes);
    output.push(value);
    (!output.text.trim().is_empty()).then_some(output.text)
}

fn selected_document_entry(extension: &str, name: &str) -> bool {
    match extension {
        "docx" => {
            name == "word/document.xml"
                || is_numbered_xml(name, "word/header")
                || is_numbered_xml(name, "word/footer")
        }
        "pptx" => is_numbered_xml(name, "ppt/slides/slide"),
        "xlsx" => name == "xl/sharedStrings.xml" || is_numbered_xml(name, "xl/worksheets/sheet"),
        "odt" | "odp" | "ods" => name == "content.xml",
        _ => false,
    }
}

fn is_numbered_xml(name: &str, prefix: &str) -> bool {
    numeric_xml_suffix(name, prefix) != u64::MAX
}

fn document_entry_order(extension: &str, name: &str) -> (u8, u64) {
    match extension {
        "docx" if name == "word/document.xml" => (0, 0),
        "docx" if name.starts_with("word/header") => (1, numeric_xml_suffix(name, "word/header")),
        "docx" => (2, numeric_xml_suffix(name, "word/footer")),
        "pptx" => (0, numeric_xml_suffix(name, "ppt/slides/slide")),
        "xlsx" if name == "xl/sharedStrings.xml" => (0, 0),
        "xlsx" => (1, numeric_xml_suffix(name, "xl/worksheets/sheet")),
        _ => (0, 0),
    }
}

fn numeric_xml_suffix(name: &str, prefix: &str) -> u64 {
    name.strip_prefix(prefix)
        .and_then(|value| value.strip_suffix(".xml"))
        .and_then(|value| value.parse().ok())
        .unwrap_or(u64::MAX)
}

fn is_document_extension(extension: &str) -> bool {
    matches!(extension, "docx" | "pptx" | "xlsx" | "odt" | "odp" | "ods")
}

fn is_ocr_extension(extension: &str) -> bool {
    matches!(extension, "png" | "jpg" | "jpeg" | "tif" | "tiff" | "bmp")
}

fn is_direct_text_extension(extension: &str) -> bool {
    matches!(
        extension,
        "txt"
            | "md"
            | "csv"
            | "json"
            | "xml"
            | "yaml"
            | "yml"
            | "toml"
            | "html"
            | "css"
            | "js"
            | "ts"
            | "py"
            | "rs"
            | "swift"
            | "java"
            | "go"
            | "rb"
            | "sh"
    )
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        io::Write,
        os::unix::fs::{PermissionsExt, symlink},
    };

    use tempfile::tempdir;
    use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

    use super::*;
    use crate::OcrConfig;

    fn config() -> ExtractionConfig {
        ExtractionConfig {
            max_output_bytes: 4096,
            max_archive_entries: 32,
            max_expanded_bytes: 64 * 1024,
            max_xml_events: 1000,
            max_xml_depth: 32,
            timeout_seconds: 1,
            ocr: None,
        }
    }

    fn write_document(path: &Path, entry: &str, xml: &str) {
        write_entries(path, &[(entry, xml)]);
    }

    fn write_entries(path: &Path, entries: &[(&str, &str)]) {
        let file = File::create(path).unwrap();
        let mut archive = ZipWriter::new(file);
        for (entry, xml) in entries {
            archive
                .start_file(
                    *entry,
                    SimpleFileOptions::default().compression_method(CompressionMethod::Deflated),
                )
                .unwrap();
            archive.write_all(xml.as_bytes()).unwrap();
        }
        archive.finish().unwrap();
    }

    fn write_executable(path: &Path, source: &str) {
        fs::write(path, source).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }

    fn candidate(name: &str) -> FileCandidate {
        FileCandidate {
            id: "f000001".into(),
            source_path: name.into(),
            extension: Path::new(name)
                .extension()
                .unwrap()
                .to_str()
                .unwrap()
                .into(),
        }
    }

    #[test]
    fn extracts_bounded_text_from_supported_document_containers() {
        let directory = tempdir().unwrap();
        let cases = [
            ("sample.docx", "word/document.xml"),
            ("sample.pptx", "ppt/slides/slide1.xml"),
            ("sample.xlsx", "xl/sharedStrings.xml"),
            ("sample.odt", "content.xml"),
            ("sample.odp", "content.xml"),
            ("sample.ods", "content.xml"),
        ];
        let extractor = LocalContentExtractor::new(config());

        for (name, entry) in cases {
            write_document(
                &directory.path().join(name),
                entry,
                "<?xml version=\"1.0\"?><root><p>private report</p></root>",
            );
            let result = extractor
                .extract(directory.path(), &candidate(name), 100, 1024 * 1024)
                .unwrap();
            assert!(result.content.contains("private report"), "{name}");
            assert!(result.content.len() <= 100);
        }
    }

    #[test]
    fn orders_numbered_document_entries_deterministically() {
        let directory = tempdir().unwrap();
        write_entries(
            &directory.path().join("slides.pptx"),
            &[
                ("ppt/slides/slide10.xml", "<root>ten</root>"),
                ("ppt/slides/slide2.xml", "<root>two</root>"),
            ],
        );

        let content = LocalContentExtractor::new(config())
            .extract(
                directory.path(),
                &candidate("slides.pptx"),
                100,
                1024 * 1024,
            )
            .unwrap()
            .content;
        assert!(content.find("two").unwrap() < content.find("ten").unwrap());
    }

    #[test]
    fn rejects_document_types_and_excessive_archive_expansion() {
        let directory = tempdir().unwrap();
        write_document(
            &directory.path().join("doctype.docx"),
            "word/document.xml",
            "<!DOCTYPE root [<!ENTITY secret 'value'>]><root>&secret;</root>",
        );
        write_document(
            &directory.path().join("large.docx"),
            "word/document.xml",
            &format!("<root>{}</root>", "x".repeat(1024)),
        );
        let mut limits = config();
        limits.max_expanded_bytes = 128;
        let extractor = LocalContentExtractor::new(limits);

        assert!(
            extractor
                .extract(
                    directory.path(),
                    &candidate("doctype.docx"),
                    100,
                    1024 * 1024
                )
                .is_none()
        );
        assert!(
            extractor
                .extract(directory.path(), &candidate("large.docx"), 100, 1024 * 1024)
                .is_none()
        );
    }

    #[test]
    fn enforces_xml_depth_event_and_output_limits() {
        let directory = tempdir().unwrap();
        write_document(
            &directory.path().join("deep.docx"),
            "word/document.xml",
            "<a><b><c>text</c></b></a>",
        );
        let mut depth_limits = config();
        depth_limits.max_xml_depth = 2;
        assert!(
            LocalContentExtractor::new(depth_limits)
                .extract(directory.path(), &candidate("deep.docx"), 100, 1024 * 1024)
                .is_none()
        );

        let mut event_limits = config();
        event_limits.max_xml_events = 2;
        assert!(
            LocalContentExtractor::new(event_limits)
                .extract(directory.path(), &candidate("deep.docx"), 100, 1024 * 1024)
                .is_none()
        );

        write_document(
            &directory.path().join("unicode.docx"),
            "word/document.xml",
            "<root>éééé</root>",
        );
        let mut byte_limits = config();
        byte_limits.max_output_bytes = 5;
        let byte_limited = LocalContentExtractor::new(byte_limits)
            .extract(
                directory.path(),
                &candidate("unicode.docx"),
                100,
                1024 * 1024,
            )
            .unwrap()
            .content;
        assert!(byte_limited.len() <= 5);

        let char_limited = LocalContentExtractor::new(config())
            .extract(directory.path(), &candidate("unicode.docx"), 2, 1024 * 1024)
            .unwrap()
            .content;
        assert_eq!(char_limited.chars().count(), 2);
    }

    #[test]
    fn rejects_malformed_xml_and_excessive_archive_entries() {
        let directory = tempdir().unwrap();
        write_document(
            &directory.path().join("malformed.docx"),
            "word/document.xml",
            "<root><p>unfinished",
        );
        write_entries(
            &directory.path().join("entries.docx"),
            &[
                ("word/document.xml", "<root>one</root>"),
                ("word/header1.xml", "<root>two</root>"),
            ],
        );
        assert!(
            LocalContentExtractor::new(config())
                .extract(
                    directory.path(),
                    &candidate("malformed.docx"),
                    100,
                    1024 * 1024
                )
                .is_none()
        );

        let mut entry_limits = config();
        entry_limits.max_archive_entries = 1;
        assert!(
            LocalContentExtractor::new(entry_limits)
                .extract(
                    directory.path(),
                    &candidate("entries.docx"),
                    100,
                    1024 * 1024
                )
                .is_none()
        );
    }

    #[test]
    fn rejects_symlinked_source_files_before_extraction() {
        let directory = tempdir().unwrap();
        fs::write(directory.path().join("real.txt"), "secret").unwrap();
        symlink(
            directory.path().join("real.txt"),
            directory.path().join("link.txt"),
        )
        .unwrap();
        let extractor = LocalContentExtractor::new(config());

        assert!(
            extractor
                .extract(directory.path(), &candidate("link.txt"), 100, 1024)
                .is_none()
        );
    }

    #[test]
    fn ocr_is_disabled_without_explicit_configuration() {
        let directory = tempdir().unwrap();
        fs::write(directory.path().join("image.png"), b"image").unwrap();
        let extractor = LocalContentExtractor::new(config());

        assert!(
            extractor
                .extract(directory.path(), &candidate("image.png"), 100, 1024)
                .is_none()
        );
    }

    #[test]
    fn ocr_uses_fixed_arguments_and_removes_its_private_output() {
        let directory = tempdir().unwrap();
        let executable = directory.path().join("ocr-helper");
        let marker = directory.path().join("output-path");
        write_executable(
            &executable,
            &format!(
                "#!/bin/sh\nprintf 'recognized text' > \"$2.txt\"\nprintf '%s' \"$2.txt\" > '{}'\n",
                marker.display()
            ),
        );
        fs::write(directory.path().join("--option.png"), b"image").unwrap();
        let mut limits = config();
        limits.ocr = Some(OcrConfig {
            executable,
            languages: vec!["eng".into()],
            data_dir: None,
        });
        let extractor = LocalContentExtractor::new(limits);

        let result = extractor
            .extract(directory.path(), &candidate("--option.png"), 100, 1024)
            .unwrap();
        let output_path = fs::read_to_string(marker).unwrap();
        assert!(result.content.contains("recognized text"));
        assert!(!Path::new(&output_path).exists());
    }

    #[test]
    fn ocr_timeout_is_a_local_extraction_failure() {
        let directory = tempdir().unwrap();
        let executable = directory.path().join("slow-helper");
        write_executable(&executable, "#!/bin/sh\nwhile :; do :; done\n");
        fs::write(directory.path().join("image.png"), b"image").unwrap();
        let mut limits = config();
        limits.ocr = Some(OcrConfig {
            executable,
            languages: vec!["eng".into()],
            data_dir: None,
        });
        let extractor = LocalContentExtractor::new(limits);

        assert!(
            extractor
                .extract(directory.path(), &candidate("image.png"), 100, 1024)
                .is_none()
        );
    }

    #[test]
    fn missing_nonzero_and_oversized_ocr_outputs_are_local_failures() {
        let directory = tempdir().unwrap();
        fs::write(directory.path().join("image.png"), b"image").unwrap();

        let mut missing_config = config();
        missing_config.ocr = Some(OcrConfig {
            executable: directory.path().join("missing-executable"),
            languages: vec!["eng".into()],
            data_dir: None,
        });
        assert!(
            LocalContentExtractor::new(missing_config)
                .extract(directory.path(), &candidate("image.png"), 100, 1024)
                .is_none()
        );

        let nonzero = directory.path().join("nonzero-helper");
        write_executable(&nonzero, "#!/bin/sh\nexit 7\n");
        let mut nonzero_config = config();
        nonzero_config.ocr = Some(OcrConfig {
            executable: nonzero,
            languages: vec!["eng".into()],
            data_dir: None,
        });
        assert!(
            LocalContentExtractor::new(nonzero_config)
                .extract(directory.path(), &candidate("image.png"), 100, 1024)
                .is_none()
        );

        let oversized = directory.path().join("oversized-helper");
        write_executable(
            &oversized,
            "#!/bin/sh\nprintf 'too much output' > \"$2.txt\"\n",
        );
        let mut oversized_config = config();
        oversized_config.max_output_bytes = 4;
        oversized_config.ocr = Some(OcrConfig {
            executable: oversized,
            languages: vec!["eng".into()],
            data_dir: None,
        });
        assert!(
            LocalContentExtractor::new(oversized_config)
                .extract(directory.path(), &candidate("image.png"), 100, 1024)
                .is_none()
        );
    }
}
