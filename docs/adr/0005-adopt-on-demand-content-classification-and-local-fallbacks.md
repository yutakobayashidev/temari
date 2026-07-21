# ADR 0005: Adopt on-demand content classification and local extension fallbacks

## Status

- Accepted

## Decision Drivers

- Ambiguous names need more evidence without sending every file's contents.
- Linux and macOS need one shared classification contract.
- A model response must never become an executable filesystem path.
- Planning must remain read-only, reviewable, and deterministic when extraction is unavailable.

## Context

- Name-only classification is already implemented, but it forces the model to guess when a filename is not descriptive.
- The reference behavior uses a name pass, a content pass only for ambiguous files, and deterministic type folders when content cannot be extracted.
- The product has a stricter trust boundary than the reference behavior: every destination, including a fallback, must be represented by an approved opaque ID before apply.

## Options Considered

- Always send extracted content: simple, but violates data minimization.
- Keep metadata-only classification: safest, but produces avoidable guesses and cannot reproduce the desired organization experience.
- Add a per-command content flag: explicit, but makes privacy behavior depend on invocation style and complicates GUI reuse.
- Use an explicit configuration policy with automatic on-demand extraction: consistent across CLI and future GUI while preserving a safe metadata-only mode.

## Decision

- Add the required `privacy.content` policy with `metadata_only` and `on_demand` values.
- Classify names in batches of 50. The model returns either an approved semantic destination ID or a typed `needs_content` decision.
- Under `on_demand`, extract bounded local text only for ambiguous files and classify it in batches of 20. Initial extractors support UTF-8 text and PDF text.
- Under `metadata_only`, or when extraction is unsupported, fails, is empty, or exceeds the configured byte limit, select a deterministic extension fallback locally.
- Add every fallback category to `FolderSet` during approval with an opaque ID. Automatically added fallbacks use `model_visible = false`; an identically named user proposal is reused and remains model-visible. The model receives only model-visible destinations.
- Record `name`, `content`, or `extension_fallback` in each Plan entry. Never store extracted text, raw bytes, endpoint details, or model configuration in workflow artifacts.
- Preserve the existing `propose -> approve -> plan -> apply -> undo` command boundaries. Two-pass classification is internal to `plan` and Stage 3 of `organize`.

## Consequences

- Positive: most files still expose only names and extensions, while ambiguous supported files can use richer evidence.
- Positive: unsupported content remains organizable without granting the model path authority.
- Positive: fallback folders remain visible during destination approval and exact plan review, and apply creates only those actually used.
- Negative: PDF extraction increases binary size and parser attack surface.
- Negative: Office conversion, image OCR, and platform metadata extraction remain future work.
- Migration: configuration, FolderSet, and Plan schemas advance to version 2. The unreleased CLI does not load version 1 instances of those schemas.

## Adoption and Exceptions

- Tests must reject missing, duplicate, unknown, and local-only model destination IDs before planning.
- Tests must prove that `metadata_only` never invokes content classification and that extracted text is absent from persisted artifacts.
- Review must reject extractors without byte, character, and failure bounds.
- Adding raw-file upload, OCR, Office conversion, or another content policy requires an ADR update and behavior-focused privacy tests.
