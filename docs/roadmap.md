# Product Parity Roadmap

This roadmap records the remaining parity work identified through binary analysis and the current implementation review. It is deliberately separate from the core safety contract: unfinished items must not weaken read-only planning, opaque destination IDs, explicit consent, durable journals, or conservative recovery.

## Priority order

### P0 — make the current managed workflow complete

- [ ] **Multi-operation Library edit plans** (`temari-core`, CLI, Tauri)
  - Allow one reviewed plan to contain an ordered set of add, rename, description, and delete operations.
  - Validate the complete before/after FolderSet delta and preserve stable IDs across every operation.
  - Keep one atomic binding switch and one Configure Apply/Undo journal for the batch.
  - Acceptance: a stale or partially edited batch is rejected; no physical directory or existing file is changed.
- [ ] **Nested Library editing and subtree policy** (`temari-core`, CLI, Tauri)
  - Add an explicit policy for renaming or deleting a parent with approved descendants: cascade, reparent, or reject.
  - Display the affected subtree in the preview and require exact confirmation.
  - Acceptance: no orphaned destination IDs, duplicate paths, or implicit physical moves.
- [ ] **Library edit Redo** (`temari-core`, CLI, Tauri)
  - Add a journal-backed Redo transition for a completed Configure Undo.
  - Reject Redo after a newer configuration revision or conflicting workspace change.
  - Acceptance: Undo/Redo remains deterministic across restart and does not accept caller-supplied journal paths.
- [ ] **Continuous monitoring UX** (Tauri)
  - Show pending, waiting, eligible, running, failed, and recoverable states per workspace.
  - Expose the next eligibility time and the reason a file is waiting.
  - Acceptance: every displayed action maps to an existing finite Core service; no resident daemon is introduced implicitly.

### P1 — complete the organizer experience

- [ ] **Folder detail view** (Tauri, Core read models)
  - Add a detail screen for Kept, Inbox, and Library with bounded file previews, counts, last run, and recent moves.
  - Keep content extraction opt-in and bounded; never expose raw file contents by default.
  - Acceptance: the view is read-only and uses the same identity and privacy rules as planning.
- [ ] **Physical Library reorganization workflow** (Core, CLI, Tauri)
  - Add a separate reviewed Plan/Apply/Undo flow for moving existing Library files after a logical structure edit.
  - Preview source, destination, collision resolution, created directories, and affected file count.
  - Acceptance: logical editing never silently moves files; reorganization remains independently undoable.
- [ ] **Improved setup wizard orchestration** (CLI, Tauri)
  - Offer one interactive flow that composes propose → approve → setup plan → exact apply confirmation.
  - Preserve all primitive commands for automation and recovery.
  - Acceptance: non-interactive callers never prompt and can reproduce the same artifacts.
- [ ] **Folder prompt and priority editing** (Core, CLI, Tauri)
  - Model descriptions, parent paths, and priorities as versioned logical metadata.
  - Define how prompt edits affect classification signatures and pending files.
  - Acceptance: prompt changes are reviewable, auditable, and never rewrite historical artifacts.

### P2 — cleanup capabilities found in the analyzed product

- [ ] **Duplicate analysis and selection** (Core, CLI, Tauri)
  - Group duplicates using a documented identity policy (content hash, size, and metadata where appropriate).
  - Support keep-newest, keep-oldest, and keep-in-folder strategies.
  - Require a review plan and always retain at least one copy per group.
- [ ] **Recoverable Trash workflow** (Core, CLI, Tauri)
  - Move selected files to the platform Trash rather than permanently deleting them.
  - Record an auditable operation and expose recovery status where the platform permits it.
  - Add an explicit large-operation confirmation threshold for file count and total size.
- [ ] **System junk and large-file reports** (Core, CLI, Tauri)
  - Add read-only reports for installers, archives, screenshots, and large stale files.
  - Keep category selection separate from the managed organizer workflow.
  - Acceptance: no category is destructive without an explicit reviewed Apply.

### P3 — platform and polish parity

- [ ] **Cloud/provider-aware filesystem handling**
  - Detect cloud-backed folders and external volumes before planning.
  - Define copy-and-delete fallback semantics, xattr loss behavior, and recovery journals per provider.
- [ ] **Richer folder tree editing**
  - Add tree navigation, selection, expand/collapse, and per-folder file previews.
  - Keep the current logical-vs-physical boundary explicit in every action.
- [ ] **History and recovery dashboard**
  - Combine file moves, directory adoption, configuration revisions, Undo, Redo, and pending recovery in one timeline.
  - Support filtering by workspace, operation type, and state without making SQLite authoritative for immutable artifacts.

## Evidence and confidence

- High confidence: three-area workflow, move journals, xattr-based move-back markers, folder prompt CRUD metadata, and separate physical reorganization helpers.
- Medium confidence: the exact current call path for directory move-back logs, the UI action that invokes physical subfolder reorganization, and end-to-end prompt transaction semantics.
- Unconfirmed: a dedicated folder-merge operation. Do not add merge semantics until a concrete binary or UX path is identified.

## Non-goals

- Do not copy third-party branding, prompts, schemas, identifiers, or UI assets.
- Do not make logical Library editing mutate physical directories implicitly.
- Do not add a background daemon or telemetry by default.
