# Product Parity Roadmap

This roadmap records the remaining parity work identified through binary analysis and the current implementation review. It is deliberately separate from the core safety contract: unfinished items must not weaken read-only planning, opaque destination IDs, explicit consent, durable journals, or conservative recovery.

## Current position

- The managed organizer Core is feature-complete for the three-area lifecycle, logical Library editing, physical Library reorganization, recovery, and finite scheduled runs.
- The largest remaining organizer gaps are read-only area and folder detail, folder classification priority, tree-oriented editing, and long-running desktop feedback.
- Cleanup, natural-language discovery, agent integration, and provider-specific file handling are separate product epics. They must not complicate the organizer state machine.

## Priority order

### P0 — make the current managed workflow complete

- [x] **Separate default roots from app-managed areas** (Core, CLI, Tauri)
  - Treat `Desktop`, `Downloads`, and optionally `Documents` as default onboarding suggestions, never as implicit registrations.
  - Preserve the physical three-area workflow, but use product terminology aligned with the analyzed behavior: Manual Library, Recents, and AI Library.
  - Reject registering a managed area or a nested root beneath an existing workspace; recognize existing app-managed areas by the setup journal, filesystem identity, and reserved-area metadata.
  - Keep journals, SQLite indexes, and immutable workflow artifacts in the application state directory; only the three user-visible workflow areas and approved destination folders belong under the selected root.
  - Reject obsolete artifacts and state schemas instead of maintaining compatibility or migration paths. Treat obsolete area names as ordinary user directories rather than hidden reserved names.
  - Acceptance: selecting `~/Downloads` does not make its three managed areas eligible as separate roots, and status remains healthy after setup Undo.
- [x] **Multi-operation AI Library edit plans** (`temari-core`, CLI, Tauri)
  - Allow one reviewed plan to contain an ordered set of add, rename, description, and delete operations.
  - Validate the complete before/after FolderSet delta and preserve stable IDs across every operation.
  - Keep one atomic binding switch and one Configure Apply/Undo journal for the batch.
  - Acceptance: a stale or partially edited batch is rejected; no physical directory or existing file is changed.
- [x] **Nested AI Library editing and subtree policy** (`temari-core`, CLI, Tauri)
  - Add an explicit policy for renaming or deleting a parent with approved descendants: cascade, reparent, or reject.
  - Display the affected subtree in the preview and require exact confirmation.
  - Acceptance: no orphaned destination IDs, duplicate paths, or implicit physical moves.
- [x] **AI Library edit Redo** (`temari-core`, CLI, Tauri)
  - Add a journal-backed Redo transition for a completed Configure Undo.
  - Reject Redo after a newer configuration revision or conflicting workspace change.
  - Acceptance: Undo/Redo remains deterministic across restart and does not accept caller-supplied journal paths.
- [x] **Continuous monitoring UX** (Tauri)
  - Show pending, waiting, eligible, running, failed, and recoverable states per workspace.
  - Expose the next eligibility time and the reason a file is waiting.
  - Acceptance: every displayed action maps to an existing finite Core service; no resident daemon is introduced implicitly.

### P1 — close the remaining organizer experience gaps

- [ ] **Area and folder detail with completion UX** (Tauri, Core read models)
  - Add detail screens for Manual Library, Recents, and AI Library with bounded file previews, counts, last run, recent moves, and attention state.
  - Show a bounded completion summary with per-folder counts, partial failures, and an explicit open-folder action.
  - Keep content extraction opt-in and bounded; never expose raw file contents by default.
  - Replace desktop copy that still implies existing files can move only through Reprocess; point users to the explicit Reorganize flow.
  - Acceptance: every view is read-only and uses the same identity and privacy rules as planning.
- [x] **Physical Library reorganization workflow** (Core, CLI, Tauri)
  - Add a separate reviewed Plan/Apply/Undo flow for moving existing Library files after a logical structure edit.
  - Preview source, destination, collision resolution, created directories, and affected file count.
  - Acceptance: logical editing never silently moves files; reorganization remains independently undoable.
- [ ] **Finish setup orchestration** (CLI, Tauri polish)
  - Add one managed CLI flow that composes propose → approve → setup plan → exact apply confirmation.
  - Polish the existing three-step desktop wizard with a default-structure preview and a clear transition into recurring organization.
  - Preserve all primitive commands for automation and recovery.
  - Acceptance: non-interactive callers never prompt and can reproduce the same artifacts.
- [ ] **Folder classification priority and rule editing** (Core, CLI, Tauri)
  - Extend versioned logical folder metadata with an explicit classification priority; descriptions and parent paths already participate in logical revisions.
  - Define how prompt edits affect classification signatures and pending files.
  - Consider content-keyword rule predicates only after defining the same bounded-content consent policy used by model classification.
  - Acceptance: metadata and rule changes are reviewable, auditable, deterministic, and never rewrite historical artifacts.
- [ ] **Tree-oriented Library editor** (Tauri)
  - Present the existing nested add, rename, description, cascade, reparent, Undo, and Redo capabilities as an expandable tree instead of flat path rows.
  - Show the affected subtree and whether an action changes logical structure, physical files, or both.
  - Acceptance: the adapter delegates every mutation to the existing Core services and never derives its own tree transition.
- [ ] **Managed run progress and cancellation** (Core read models, Tauri)
  - Expose stable phases, processed and remaining counts, and cancellation at safe operation boundaries.
  - Preserve crash recovery and completed immutable journals when a user cancels.
  - Acceptance: closing or cancelling the desktop never leaves an unowned filesystem mutation.

### P2 — optional cleanup product epic

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
- [ ] **Natural-language file discovery** (Core, CLI, Tauri)
  - Define a private, bounded index and query contract before adding natural-language search.
  - Keep discovery read-only and independent from organization Plans and Apply Sessions.
- [ ] **Agent and MCP integration** (Core service boundary, CLI)
  - Expose narrowly scoped read and reviewed-action tools without granting callers arbitrary filesystem paths.
  - Require the same workspace IDs, opaque destination IDs, consent, and exact Apply confirmation as first-party adapters.

### P3 — platform-specific filesystem support and recovery polish

- [ ] **Cloud/provider-aware filesystem handling**
  - Detect cloud-backed folders and external volumes before planning.
  - Define copy-and-delete fallback semantics, xattr loss behavior, and recovery journals per provider.
- [ ] **History and recovery dashboard**
  - Combine file moves, directory adoption, configuration revisions, Undo, Redo, and pending recovery in one timeline.
  - Support filtering by workspace, operation type, and state without making SQLite authoritative for immutable artifacts.

## Evidence and confidence

- High confidence: three-area workflow, move journals, move-back markers, folder prompt priority metadata, nested tree editing, phase-oriented organization progress, duplicate selection, recoverable Trash, cleanup reports, and separate physical reorganization helpers.
- Medium confidence: the exact current call path for directory move-back logs, whether physical reorganization includes Recents in the current UI, and end-to-end folder prompt transaction semantics.
- Unconfirmed: a dedicated folder-merge operation. Do not add merge semantics until a concrete binary or UX path is identified.

## Non-goals

- Do not copy third-party branding, prompts, schemas, identifiers, or UI assets.
- Do not make logical AI Library editing mutate physical directories implicitly.
- Do not add a background daemon or telemetry by default.
- Do not treat cleanup, discovery, or agent integration as prerequisites for completing the organizer experience.
