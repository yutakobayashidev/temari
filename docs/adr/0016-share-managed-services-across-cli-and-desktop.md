# ADR 0016: Share managed services across CLI and desktop

## Status

Accepted

## Context

The managed three-area workflow became the primary product experience, while the first desktop proof of concept still implemented a separate one-shot session. Reimplementing workspace activation, finite runs, reprocessing, history, and Undo in Tauri would duplicate safety-critical state transitions and allow the two adapters to drift.

OS scheduling also needs to be available from both adapters, but it is a platform integration rather than filesystem-domain logic. The desktop executable does not implement the CLI command tree, so it cannot safely be substituted for a scheduled helper.

## Decision

- Put workspace activation, binding validation, artifact allocation, directory adoption, staging, classification, Apply finalization, and explicit reprocessing in `temari-core::ManagedService`.
- Keep the CLI and Tauri layers as presentation and confirmation adapters over that service. Neither adapter shells out to the other.
- Keep setup as proposal, read-only preview, and confirmed Apply. Tauri stores the proposal, approved folder set, and setup Plan behind opaque tokens; Apply accepts only the latest preview token.
- Make setup tokens process-unique and revisioned, and consume the exact latest preview atomically before mutation so stale, replayed, and concurrent Apply requests fail closed.
- Bind the canonical model configuration path to each managed workspace. Manual runs and schedule definitions must use that binding instead of a process default.
- Index root-directory adoption as its own managed run kind. History reads its setup-style journal, and Undo restores the complete adoption session without removing the three managed areas.
- Return explicit camel-case desktop view types instead of serializing database records directly.
- Run blocking model and filesystem work outside the Tauri event loop.
- Put systemd and launchd definition rendering, validation, installation, status, and removal in the separate `temari-schedule` crate.
- Require desktop users to select an absolute, stable Temari CLI executable before enabling a schedule.

## Consequences

- CLI and desktop runs now use the same manual move-back protection, root-directory adoption, retention, model privacy, journaling, and recovery behavior.
- Managed workflow changes need one service implementation and adapter-focused tests.
- Desktop setup and mutations cannot accept frontend-selected artifact or journal paths.
- A post-setup indexing failure reports the authoritative setup journal so the completed filesystem mutation remains recoverable even when workspace registration fails.
- The schedule adapter remains testable without placing process-manager concerns in `temari-core`.
- Packaged desktop releases will eventually need to distribute or locate a stable CLI helper explicitly; the proof of concept does not guess or silently install one.
