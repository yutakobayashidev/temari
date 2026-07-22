# ADR 0018: Reorganize existing AI Library files as a separate workflow

## Status

- Accepted

## Context

- Logical AI Library edits preserve opaque destination IDs and atomically replace the approved FolderSet binding without moving files.
- A Rename or Delete can therefore leave already classified files below paths that are no longer current.
- Reusing normal classification would contact the model unnecessarily, lose stable-ID intent, and mix a structure change with a separate filesystem mutation.

## Decision

- Bind each physical reorganization Plan to one completed Configure run and its exact before and after FolderSets. Plan and Apply require the workspace to remain disabled on that exact current binding; physical Undo remains available after later logical revisions.
- Match current AI Library files to authoritative processed-file records by filesystem identity, size, and SHA-256. Do not infer ownership from path or extension alone.
- For a preserved destination ID whose path changed, retain the file's suffix below the old destination and map it below the new approved path. For a removed destination ID, return the file directly to Recents for the normal retention and classification flow.
- Report untracked, changed, unknown-destination, and manually relocated files as attention items. Never guess a destination or mutate those files.
- Resolve occupied destinations deterministically before review and never overwrite. Create only required destination parents; do not remove old directories, even when empty.
- Apply through the shared source lock and filesystem journal engine. Store the immutable Plan and fixed Apply and Undo journals in the private run directory. SQLite remains an index for run history, processed identities, and Recents observation.
- Keep reorganization independently undoable. Block Configure Undo while its reorganization is active so physical and logical provenance cannot diverge.

## Consequences

- Positive: a reviewed structure edit can be reflected physically without another model request or model-generated path.
- Positive: stable opaque IDs preserve classification intent, while removed categories deliberately re-enter Recents.
- Positive: exact journals preserve conservative Resume and Undo behavior across process failure.
- Negative: changed and manually moved files require explicit user action, and obsolete empty directories remain visible.
- Negative: incompatible SQLite schema revisions require a fresh mutable state database; immutable JSON journals and user files remain untouched.

## Verification

- Tests cover preserved-ID moves, removed-ID staging, attention items, collision resolution, stale bindings, active-run guards, exact Undo, and rejection of unsafe workflow manifests.
- CLI and desktop adapters call `ManagedService`; neither reconstructs the move sequence or accepts caller-selected Apply and Undo journal paths.
