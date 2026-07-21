# ADR 0002: Separate AI folder proposals from approved directory creation

## Status

- Accepted

## Decision Drivers

- The product should propose a useful folder hierarchy instead of requiring all destinations to be authored manually.
- A model-generated string must never become an immediate filesystem capability.
- Users must be able to edit the hierarchy before any directory is created or file is moved.
- Apply and undo behavior must remain deterministic on Linux and macOS.

## Context

- The product needs two distinct behaviors: AI-assisted folder-structure proposal and automatic creation of missing approved destination directories during apply.
- The current implementation supports only classification into destinations already defined in configuration.
- ADR 0001 requires a new decision before changing the filesystem mutation policy.

## Options Considered

- Create model-proposed paths immediately: shortest flow, but gives untrusted model output direct filesystem authority.
- Require every destination to be authored manually: safest simple boundary, but omits a central product capability.
- Separate proposal, approval, plan, and apply: preserves automatic assistance while keeping filesystem authority local and explicit.

## Decision

- Add a read-only `propose` stage that may suggest display names, relative hierarchy, and descriptions from file-name metadata.
- Require an explicit `approve` stage before proposals become destinations. Approval assigns local opaque IDs and applies deterministic path normalization and validation.
- Classification continues to return only approved destination IDs.
- A plan records which approved directories are missing. It does not create them.
- A future explicit `apply` stage creates only missing approved directories, in parent-first order, immediately before validated moves.
- Record every directory created by an apply session. Undo may remove only those recorded directories and only when they are empty.

## Consequences

- Positive: the application can generate useful folder structures without executing arbitrary model paths.
- Positive: users can review naming, hierarchy, and privacy implications before mutation.
- Positive: plans expose directory creation and file movement as separate auditable operations.
- Negative: the workflow requires an additional approval step.
- Negative: apply must defend against state changes, symlinks, and path replacement after planning.
- Follow-up: define the proposal schema, naming policy, plan hash, apply-time filesystem checks, collision policy, and durable undo store.

## Adoption and Exceptions

- Proposal output must be treated as untrusted data and must never be passed directly to filesystem APIs.
- Approval must reject absolute paths, parent traversal, empty components, duplicate normalized paths, reserved names, and destinations outside the source root.
- Apply must re-resolve every path without following symlinks and reject any component that changed into a symlink or non-directory.
- Tests must cover nested creation order, existing directories, files or symlinks at destination paths, stale plans, partial failure, and undo of created empty directories.
- Any direct model-to-filesystem path requires a superseding ADR and explicit owner approval.
