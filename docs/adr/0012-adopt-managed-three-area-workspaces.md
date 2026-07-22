# ADR 0012: Adopt managed three-area workspaces

## Status

- Accepted
- Extended by [ADR 0014](0014-schedule-finite-managed-runs.md) and [ADR 0015](0015-reprocess-managed-files-through-recents.md).

## Decision Drivers

- New files should remain visible and untouched for a configurable grace period before classification.
- Existing directories need an explicit protected area that normal classification never traverses.
- AI-classified files need a distinct reviewed destination hierarchy.
- Initial directory relocation has different verification and recovery requirements from regular-file classification.
- Recent moves and eligibility need indexed queries, while filesystem recovery must remain possible without SQLite.

## Context

The primitive workflow organizes a reviewed set of files in one pass. It does not model a long-lived folder where existing directories are protected, incoming files wait, and only stable files move into an approved library. Foreground monitoring adds scheduling and history, but it classifies eligible source files immediately.

A managed workspace introduces three physical areas below one canonical source:

- `Manual Library` contains directories that existed when the workspace was initialized. Normal managed scans never traverse it.
- `Recents` contains loose files while their retention and stability windows are active.
- `AI Library` contains the approved classification hierarchy and deterministic local fallbacks.

These names and artifacts are local product concepts. The design does not depend on another application's identifiers, schema, prompt text, or implementation.

## Options Considered

- Represent Recents only in SQLite and leave files at the source root: minimizes mutation but does not provide a physical staging area and makes the visible source state ambiguous.
- Extend the regular-file `Plan` with directory entries: uses fewer artifact types but mixes recursive directory verification with file classification and weakens review semantics.
- Copy directories across filesystems: supports more locations but introduces a second publication and deletion protocol that cannot share rename recovery safely.
- Use a dedicated setup artifact, then reuse regular-file Plans for staging and classification: keeps each safety boundary narrow and inspectable.

## Decision

- Create `Manual Library`, `Recents`, and `AI Library` as real, non-symlink directories below the managed source.
- Generate a read-only versioned `ManagedSetupPlan` before initialization. It records the canonical source identity, every root entry, exact destinations, regular-file fingerprints, and deterministic directory manifests.
- Move all existing real directories to `Manual Library` first, then move all existing regular files to `Recents`. Reject reserved-name collisions, special root entries, stale enumeration, destination collisions, cross-filesystem moves, and unsafe path types.
- Write and fsync a `ManagedSetupSession` before the first mutation. Checkpoint directory creation and each atomic rename. Resume only a running session after conservative reconciliation. Undo into a separate `ManagedSetupUndoSession` in reverse order and refuse changed or occupied entries.
- Keep normal file staging and classification on the existing version 4 `Plan`, `ApplySession`, and `UndoSession` services. Staging is a local rule-bound Plan from the source root to `Recents`. Classification scans only direct Recents files and uses a locally transformed `FolderSet` whose paths are prefixed by `AI Library/`; opaque IDs and model visibility remain unchanged.
- Record Recents arrival from Temari's first observation, never from filesystem modification time. A file becomes eligible only after both its retention deadline and a stability window. Content or size changes preserve first observation but reset stability and return the item to pending.
- Use SQLite for managed workspace definitions, Recents eligibility, and recent-run indexes. JSON artifacts remain authoritative for setup, apply, resume, and undo. Database loss must not make completed filesystem operations unrecoverable.
- Support recent move undo by completed session and by selected file IDs. Individual undo restores only selected files and does not remove shared destination directories.
- Keep background execution out of the initial implementation. The managed CLI runs finite setup, planning, apply, history, and recovery commands. ADR 0014 later adopts explicit platform scheduling of the same finite run service without adding a daemon.

## Consequences

- Positive: the filesystem itself communicates protected, waiting, and classified states.
- Positive: retention does not accidentally backdate copied files with old modification times.
- Positive: directory initialization cannot silently inherit regular-file collision or undo behavior.
- Positive: normal classification retains the reviewed opaque-destination and content-consent boundaries.
- Negative: initialization hashes directory trees and may be expensive or hydrate cloud placeholders.
- Negative: version 1 setup supports only identity-preserving same-filesystem rename; copy-and-delete requires a separate future protocol.
- Negative: a changed Manual Library directory cannot be automatically returned by setup undo until the change is resolved.

## Adoption and Exceptions

- Tests must cover stale setup Plans, reserved names, symlink boundaries, special entries, cross-device rejection, partial setup recovery, changed-directory undo refusal, retention, stability reset, AI Library prefixing, and individual file undo.
- Reviews must reject any managed scan that traverses `Manual Library`, classifies a file before both deadlines, accepts a model-generated path, or treats SQLite rows as sufficient proof of a move.
- Cloud-provider copy semantics, filesystem event watchers, implicit services, nested Recents classification, and cross-device copy are separate decisions.
