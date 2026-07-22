# ADR 0017: Edit managed AI Library structure as logical FolderSet revisions

## Status

- Accepted

## Context

- A managed workspace binds both its monitor and workspace row to one immutable `FolderSet` path and SHA-256 digest.
- Destination IDs are opaque capabilities used by model output and local rules. Renaming a destination must not change its ID.
- Post-setup structure editing changes the approved classification vocabulary. It is separate from moving files that are already in `AI Library`.

## Decision

- Represent an ordered batch of Add, Rename, Description, and Delete operations as one reviewed, version 2 Library-edit Plan. Draft Add operations do not select IDs; Core assigns each new opaque ID when it builds the Plan.
- Replay and validate every operation in order. A parent Rename or Delete must explicitly reject descendants, cascade through the subtree, or reparent descendants one level upward. System fallbacks are never valid subtree members.
- Write a new immutable `FolderSet` artifact and atomically switch both database bindings after validation. Never overwrite an earlier `FolderSet`.
- Preserve destination IDs for Rename and Description edits. Generate a new non-reused opaque ID only for Add.
- Keep system fallback destinations private and immutable. Reject deletion of the final model-visible destination and any destination referenced by an active local rule.
- Require the workspace to be disabled and all earlier runs to be terminal before Apply.
- Treat these operations as logical configuration changes. They never rename or delete physical directories and never move existing files. Existing files move only through explicit Reprocess or physical Library reorganization workflows.
- Persist one durable Configure Session for the complete batch. Undo and Redo use fixed, run-owned journals beside that Session and switch between the two immutable bindings. Interrupted Apply, Undo, or Redo remains recoverable before the workspace can be enabled.
- Redo requires a completed Undo, the exact previous binding, and no newer Configure run. Neither adapters nor users may select recovery journal paths.

## Consequences

- Positive: historical Plans remain self-contained, stable IDs keep rules valid across Rename, and editing cannot silently relocate private files.
- Positive: stale previews, injected FolderSets, and partially updated monitor/workspace bindings are rejected.
- Negative: a renamed or deleted logical destination leaves existing files at its former physical path until the user explicitly reviews a Reprocess or reorganization Plan.
- Negative: logical Undo and Redo do not move files that already occupy an earlier physical hierarchy; physical changes have an independent Undo lifecycle.

## Adoption and Exceptions

- Core validates that replaying the reviewed operation against the before `FolderSet` produces the exact after `FolderSet`.
- Tauri keeps the Plan behind a single-use preview token; the frontend submits neither artifacts nor journal paths.
- Tests must cover stable IDs, ordered batches, subtree policies, immutable revisions, stale bindings, active rules for every removed subtree ID, unfinished runs, Apply/Undo/Redo crash recovery, and the no-physical-move boundary.
- Physical reorganization remains the separate reviewed Plan/Apply/Undo workflow defined by ADR 0018 and must not be added to logical FolderSet editing.
