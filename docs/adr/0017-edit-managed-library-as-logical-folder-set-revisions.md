# ADR 0017: Edit managed Library structure as logical FolderSet revisions

## Status

- Accepted

## Context

- A managed workspace binds both its monitor and workspace row to one immutable `FolderSet` path and SHA-256 digest.
- Destination IDs are opaque capabilities used by model output and local rules. Renaming a destination must not change its ID.
- Post-setup structure editing changes the approved classification vocabulary. It is separate from moving files that are already in `Library`.

## Decision

- Represent each Add, Rename, Description, or Delete operation as a reviewed, versioned Library-edit Plan.
- Write a new immutable `FolderSet` artifact and atomically switch both database bindings after validation. Never overwrite an earlier `FolderSet`.
- Preserve destination IDs for Rename and Description edits. Generate a new non-reused opaque ID only for Add.
- Keep system fallback destinations private and immutable. Reject deletion of the final model-visible destination and any destination referenced by an active local rule.
- Require the workspace to be disabled and all earlier runs to be terminal before Apply.
- Treat these operations as logical configuration changes. They never rename or delete physical directories and never move existing files. Existing files move only through the explicit Reprocess workflow.
- Persist a durable Configure Session. Undo switches back to the previous immutable binding; interrupted Apply or Undo remains recoverable before the workspace can be enabled.

## Consequences

- Positive: historical Plans remain self-contained, stable IDs keep rules valid across Rename, and editing cannot silently relocate private files.
- Positive: stale previews, injected FolderSets, and partially updated monitor/workspace bindings are rejected.
- Negative: a renamed or deleted logical destination can leave existing files at its former physical path until the user explicitly reprocesses them.
- Negative: the first editor supports one reviewed operation at a time. Multi-edit transactions, subtree cascading, and Redo are deferred.

## Adoption and Exceptions

- Core validates that replaying the reviewed operation against the before `FolderSet` produces the exact after `FolderSet`.
- Tauri keeps the Plan behind a single-use preview token; the frontend submits neither artifacts nor journal paths.
- Tests must cover stable IDs, immutable revisions, stale bindings, active rules, unfinished runs, Apply/Undo recovery, and the no-physical-move boundary.
- Any future physical reorganization must remain a separate reviewed Plan/Apply/Undo workflow and must not be added to logical FolderSet editing.
