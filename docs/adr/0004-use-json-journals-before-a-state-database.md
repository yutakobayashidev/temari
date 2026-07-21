# ADR 0004: Use JSON journals before a state database

## Status

- Accepted

## Decision Drivers

- Apply and undo need durable, inspectable state even after partial failure.
- The current product runs one explicit local workflow at a time and does not monitor folders in the background.
- Linux and macOS must share the same source of truth.
- Workflow artifacts must remain easy for a person or agent to review without database tooling.

## Context

- `Proposal`, `FolderSet`, and `Plan` are already versioned JSON artifacts.
- Apply introduces incremental state that must be checkpointed before and after filesystem mutations.
- Future monitoring may require queryable state for rules, prompts, processed-file state, sessions, and metrics.
- Temari does not yet need monitoring queries, rule statistics, or multi-session GUI state.

## Options Considered

- Adopt SQLite now: provides transactions and indexed queries, but introduces schema migrations and a second persistence model before the product needs queryable state.
- Use only extended attributes: keeps metadata with moved files on some filesystems, but is not reliably portable across Linux, macOS, cloud providers, or cross-volume operations.
- Use atomic JSON journals now and add an indexed state store when monitoring requires it: preserves inspectability and keeps the current workflow simple.

## Decision

- Keep versioned JSON as the authoritative format for `Proposal`, `FolderSet`, `Plan`, `ApplySession`, and `UndoSession`.
- Create an apply or undo journal at a new owner-only file before the first mutation.
- Atomically rewrite and sync the active journal around every directory or move operation. Explicit resume may continue only a `running` journal after conservative reconciliation; a finalized journal is immutable.
- Store fingerprints, operation outcomes, actual destinations, and exactly which directories the session created.
- Do not add SQLite, GRDB, or mandatory extended attributes in the manual CLI milestone.
- Introduce SQLite later only as an index and mutable state store for monitoring, rules, cross-session queries, or GUI state. It must not replace immutable workflow artifacts.

## Consequences

- Positive: users and agents can inspect, archive, and recover sessions with ordinary JSON tools.
- Positive: the CLI has no database location, migration, locking, or backup policy yet.
- Positive: apply and undo remain portable across the required platforms.
- Negative: cross-session analytics and indexed queries require reading multiple files.
- Negative: a process crash between a filesystem mutation and its following checkpoint can leave an in-progress outcome that requires conservative reconciliation.
- Follow-up: add cross-process locking before background execution or concurrent mutation commands are introduced.

## Adoption and Exceptions

- Apply and undo output must use a new path outside the organized source and must never overwrite an existing journal.
- Tests must cover stale fingerprints, occupied destinations, partial outcomes, immutable apply journals, conservative undo, and session-created directory removal.
- Resume must preserve the session ID, plan digest, resolved destinations, and start time. Ambiguous state becomes a conflict instead of being retried.
- Any state database proposal must identify a concrete query or concurrency requirement, keep workflow artifacts exportable, and be recorded in a superseding ADR.
