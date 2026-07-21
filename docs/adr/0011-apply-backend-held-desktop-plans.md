# ADR 0011: Apply Backend-Held Desktop Plans

## Status

- Accepted

## Decision Drivers

- Desktop users need the reviewed Plan to be directly actionable without weakening the CLI safety model.
- Frontend IPC is an untrusted boundary and must not select filesystem paths or replace workflow artifacts.
- Apply and Undo must remain auditable and recoverable after partial failure or process exit.

## Context

The desktop application could create and display a real Plan but could not mutate the filesystem. Enabling Apply introduces a durable workflow boundary: the application must prove that the confirmed Plan is the Plan it executes, persist it before mutation, expose an honest result, and preserve recovery artifacts outside the organized source.

## Options Considered

- Send the displayed Plan back from the frontend: simple, but allows IPC to replace source and destination data.
- Write artifacts beside the selected source: discoverable, but mixes application state into the data being organized and violates the external-journal boundary.
- Retain the Plan in the backend and accept only its digest: keeps path authority and artifact persistence behind the native boundary.

## Decision

- Retain the latest reviewed Plan and SHA-256 digest in the Tauri backend.
- Apply IPC accepts only that exact digest. It accepts no Plan, filesystem path, or journal path.
- Persist the Plan before mutation in a private run below the platform application-state directory, then call the existing `temari-core` Apply service.
- Retain the resulting Apply session in the backend. Undo IPC accepts only its exact session ID and calls the existing `temari-core` Undo service with a separate journal.
- Keep Plan, Apply, and Undo as authoritative JSON artifacts. Do not store their bodies in desktop application state or SQLite.
- Do not automatically resume or rediscover interrupted runs in this proof of concept. Display artifact paths so explicit CLI recovery remains available after restart.

## Consequences

- Positive: the frontend cannot execute model-generated or injected paths.
- Positive: desktop Apply inherits source locking, stale-plan rejection, symlink checks, collision refusal, and atomic journal updates from `temari-core`.
- Positive: every mutation has a persisted Plan and an inspectable recovery journal.
- Negative: in-app Undo is limited to the active process session; restart recovery uses the CLI.
- Negative: platform state directories contain workflow metadata, including file paths, and therefore require owner-only permissions.

## Adoption and Exceptions

- Tests must cover digest mismatch, unexpected IPC fields, stale source fingerprints, successful Apply, immutable Apply journals, and successful Undo.
- Reviews must reject desktop commands that accept a source path, destination path, Plan body, Apply journal path, or Undo journal path from the frontend.
- Any automatic recovery, cross-session workflow index, or relocation into SQLite requires a separate decision and tests for ambiguous or running journals.
