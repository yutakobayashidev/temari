# ADR 0007: Adopt bounded document and OCR extraction

## Status

- Accepted

## Decision Drivers

- Ambiguous files need useful text without uploading raw files.
- Linux and macOS need the same document extraction behavior.
- Untrusted documents and images must not create unbounded CPU, memory, disk, or process use.
- Extraction failures must preserve the approved, deterministic planning workflow.

## Context

- On-demand classification already reads bounded UTF-8 and PDF text only after name classification requests content.
- Documents stored as ZIP/XML containers and images remain unsupported, so they currently use local extension fallbacks.
- Extracted text is transient model input. It is not part of Proposal, FolderSet, Plan, apply, or undo artifacts.
- Recursive scope support requires extraction to receive a locally validated regular-file path instead of reconstructing a path from a filename.

## Options Considered

- Use one general document converter: broad format support, but it adds a large runtime, writable profile state, remote-resource and macro concerns, inconsistent host behavior, and a difficult subprocess boundary.
- Use platform-specific document and OCR frameworks: good host integration, but different Linux and macOS behavior would leak into the reusable core.
- Parse supported document containers in process and use one explicit OCR executable adapter: consistent document behavior with a small, reviewable subprocess boundary.
- Upload raw files to the model: simplest extraction path, but incompatible with data minimization and the local-first privacy boundary.

## Decision

- Resolve and validate each scoped relative path before extraction. Reject symlinks, non-regular files, path escapes, and files above the configured input-byte limit.
- Keep direct UTF-8 and PDF extraction. Add in-process, streaming ZIP/XML extraction for supported word-processing, presentation, and spreadsheet containers on both required platforms.
- Read only format-specific entries. Never unpack an archive to the filesystem. Reject encrypted or overlapping archives, XML document types, malformed containers, excessive XML depth or events, excessive archive entries, and cumulative expanded data above the configured limit.
- Apply independent positive limits for input bytes, expanded bytes, output bytes, output characters, archive entries, XML events, and XML depth. Stop parsing as soon as a limit is reached or violated.
- Represent extraction results with an ephemeral typed outcome such as extracted, unsupported, unavailable, timed out, too large, invalid, or empty. Do not serialize the outcome detail or extracted text into workflow artifacts.
- Add OCR as an explicit optional configuration block. It requires an absolute executable path, a validated language list, a positive timeout, and positive output limits. Absence of the block disables OCR.
- Invoke the OCR executable directly with fixed arguments. Never invoke a shell or accept an argument template. Pass only a validated absolute input path and a private output path, close stdin, suppress child output, and use a private temporary directory.
- On OCR timeout, kill and wait for the child process. Read its output only after a successful exit and a metadata size check. Remove temporary output when the extraction attempt ends.
- Limit the first OCR adapter to common raster image formats. Scanned PDF OCR, legacy binary office formats, and rich-text conversion remain local fallbacks until separately decided.
- Treat unsupported formats and every local extraction failure as deterministic extension fallback. Model connectivity and response failures remain errors and must not silently become local fallbacks.
- Send only the bounded extracted text required by the ambiguous-file content pass. Never send raw files, persist extracted text, or include content, child stderr, or sensitive paths in normal logs.

## Consequences

- Positive: supported documents behave consistently on Linux and macOS without a desktop application dependency.
- Positive: OCR remains local, opt-in, bounded, and isolated behind a fixed process contract.
- Positive: unavailable tools and malformed content do not block organization because approved local fallbacks remain available.
- Negative: container and PDF parsers increase the in-process parser attack surface and require dependency review.
- Negative: the first release does not extract every legacy document format or OCR scanned PDFs.
- Negative: process termination is a resource boundary, not an operating-system sandbox; users remain responsible for the configured executable.

## Adoption and Exceptions

- Configuration validation must reject zero limits, relative executable paths, invalid language tokens, and OCR settings that do not form a complete adapter configuration.
- Behavior-focused tests must cover archive expansion, entry, XML depth/event, output-byte, output-character, and input-byte limits; malformed XML; document types; encrypted or overlapping archives; and early termination.
- Process tests must cover direct invocation, option-looking filenames, missing executables, non-zero exits, timeouts with kill-and-wait cleanup, oversized output, disabled OCR, unsupported images, and temporary-file cleanup.
- Privacy tests must prove that metadata-only mode never invokes extraction and that extracted text, OCR output, child stderr, and endpoint details are absent from every persisted artifact.
- Review must reject general converter integration, raw-file upload, configurable command templates, unbounded parsers, or new OCR inputs unless a new ADR records the threat model, limits, failure behavior, and tests.
