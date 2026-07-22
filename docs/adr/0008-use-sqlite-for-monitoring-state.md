# ADR 0008: Use SQLite for monitoring state

## Status

- Accepted
- Extended by [ADR 0014](0014-schedule-finite-managed-runs.md), which defines explicit standing authorization through per-user platform schedulers.
- Public CLI portions are superseded by ADR 0013; the storage and internal-service decisions remain active.

## Decision Drivers

- Monitoring needs durable, indexed queries for schedules, rules, processed files, and run history.
- Filesystem mutations must remain reviewable, recoverable, and conservative after crashes.
- Linux and macOS need one predictable implementation without platform-specific watcher semantics.
- Background work must not weaken destination approval, privacy policy, collision safety, or explicit mutation consent.

## Context

The manual workflow uses versioned JSON artifacts for proposals, approved folders, plans, apply sessions, and undo sessions. Apply and undo journals are checkpointed around every filesystem mutation and are designed for direct inspection and recovery.

Monitoring adds mutable definitions, due-time queries, local classification rules, processed-file signatures, and cross-run history. Rebuilding those views by scanning JSON files would make polling and deduplication unnecessarily expensive, while moving workflow artifacts into a database would make recovery less transparent.

## Options Considered

- Keep all state in JSON: preserves one storage format but makes indexed scheduling, deduplication, and concurrent updates cumbersome.
- Move workflow artifacts and journals into SQLite: simplifies queries but makes database state a prerequisite for reviewing and recovering filesystem changes.
- Use native filesystem event APIs: reduces polling latency but introduces platform-specific behavior and still requires reconciliation for missed or coalesced events.
- Poll folders and use SQLite only for mutable monitoring state: provides portable scheduling and queries while preserving the existing mutation boundary.

## Decision

- Use SQLite for monitor definitions, filename-based local rules, processed-file signatures, and a queryable index of monitoring runs.
- Keep `Proposal`, `FolderSet`, `Plan`, `ApplySession`, and `UndoSession` as authoritative versioned JSON artifacts. SQLite stores their paths, digests, identifiers, counts, and indexed status, not their bodies.
- Poll enabled monitors at bounded intervals. Do not install or start a background service implicitly.
- Permit a read-only single cycle by default. Continuous mutation requires explicit standing authorization through both `--apply` and `--yes`; every cycle must write and validate its exact Plan before calling the normal apply service.
- Evaluate enabled local rules deterministically before model classification. Rules may select only opaque IDs from the monitor's approved `FolderSet`; they never store or execute destination paths supplied by a model.
- Identify already processed files by a signature derived from the local file fingerprint, approved folder-set digest, and enabled-rule digest. Record a file as processed only after its apply journal proves completion. Do not store extracted content, raw model responses, credentials, or API keys in SQLite.
- Acquire an exclusive source-root lock for apply, resume, undo, and each monitoring mutation cycle. A competing operation fails without mutating the source. Locks must be released automatically when their process exits.
- Reconcile interrupted database rows from JSON artifacts only. A completed apply journal may finalize the SQLite index; a running journal requires the existing explicit resume flow; a missing, invalid, failed, or ambiguous journal must never be inferred as successful from filesystem shape or database status.
- Store one explicit schema version, reject any different or malformed existing database, and initialize a fresh schema transactionally. Enforce foreign keys, a bounded busy timeout, owner-only database permissions, and durable SQLite settings. Soft-delete monitor and rule definitions so history remains explainable.

## Consequences

- Positive: monitoring gains efficient scheduling, rule lookup, deduplication, and history queries without weakening inspectable recovery artifacts.
- Positive: polling and the same core services behave consistently on Linux and macOS.
- Positive: a database loss affects monitoring configuration and indexes but does not erase the authoritative record of completed filesystem operations.
- Negative: incompatible releases require recreating mutable SQLite state, and the application must reconcile the database after a crash between journal completion and index update.
- Negative: polling trades immediate event delivery for simpler and more predictable cross-platform behavior.
- Follow-up: service-manager integration and content-based local rules require separate decisions; neither is part of this ADR.

## Adoption and Exceptions

- Tests must prove that a Plan exists before the first monitored mutation, only a completed ApplySession marks files processed, and crash reconciliation never promotes an ambiguous journal.
- Tests must cover rule destination validation, deterministic rule ordering, signature invalidation, source-root lock contention, artifact digest mismatches, and the `--apply --yes` requirement.
- Reviews must reject schema fields that duplicate authoritative workflow artifact bodies or persist extracted content, credentials, or model-generated filesystem paths.
- Any change that makes SQLite authoritative for apply or undo recovery, replaces polling with platform-specific event handling, or permits unattended mutation without explicit standing authorization requires a superseding ADR.
