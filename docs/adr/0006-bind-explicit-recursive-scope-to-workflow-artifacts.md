# ADR 0006: Bind Explicit Recursive Scope to Workflow Artifacts

## Status

Accepted

## Context

Organizing only root-level files is predictable but insufficient for selected existing directories. Unrestricted recursion would expand privacy exposure, revisit already organized destinations, and make the reviewed proposal differ from the files later planned.

## Decision

Root-level regular files remain in scope by default. A caller may repeat `--include-subtree <PATH>` on `propose` or `organize` to select source-relative directories recursively. The special path `.` selects the complete source tree.

The normalized `ScanScope` is stored unchanged in `Proposal`, `FolderSet`, and `Plan`. The `plan` command reads it from the approved folder set and has no scope override. Recursive roots must be sorted, unique, non-overlapping portable relative paths.

Scanning never follows symlink files or directories. Planning automatically excludes all approved destination subtrees. Every file is represented by a source-relative path, while destination naming uses only its final component. Apply, resume, and undo validate each existing source-parent component before operating on a nested path. Source directories are never removed.

## Consequences

- Users can review the exact privacy and filesystem scope before planning.
- Duplicate basenames in different directories remain distinct and receive deterministic collision handling.
- Repeated runs do not recursively ingest approved destinations.
- Artifact schema versions change without compatibility shims.
- Selecting the complete tree remains explicit rather than becoming a surprising default.

## Rejected Alternatives

- Always recurse from the source root: too broad for privacy-conscious operation.
- Accept scope flags on `plan`: this would allow planning a different population than the approved proposal.
- Preserve only basenames in artifacts: duplicate names would be ambiguous and undo could not restore the original nested location.
