# ADR 0014: Schedule finite managed runs

## Status

- Accepted

## Decision Drivers

- Managed workspaces need unattended Inbox observation and classification.
- Filesystem mutation must retain the same Plan, Apply, journal, lock, and recovery boundaries as an interactive run.
- Linux and macOS already provide reliable per-user schedulers.
- Installing a hidden daemon during workspace setup would make mutation authority difficult to discover and revoke.

## Context

`temari managed run` is a finite application service, but it currently requires a caller to choose a new artifact directory for every invocation. This makes safe manual runs possible while leaving recurring operation unnecessarily difficult.

## Options Considered

- Add a long-running Temari daemon: centralizes timers but adds another lifecycle, IPC, upgrade, and crash-recovery surface.
- Watch filesystem events directly: reduces observation latency but differs by platform and still requires a persistent process and periodic reconciliation.
- Invoke finite runs from the platform user scheduler: reuses the complete tested workflow and leaves process supervision to the operating system.

## Decision

- Keep `temari managed run` finite. Do not add a resident daemon or implicit background process.
- Allow a managed run to allocate a private, unique artifact directory below the Temari state directory when `--out` is omitted. Preserve explicit `--out` for review, tests, and recovery tooling.
- Integrate with systemd user timers on Linux and per-user launchd agents on macOS. Scheduler definitions invoke a stable absolute Temari launcher with absolute configuration and state paths; they never invoke a shell. Reject direct Nix store executables because garbage collection can invalidate a persistent definition.
- Require a separate explicit scheduler install action with mutation confirmation. Workspace setup, enable, and normal run commands never install scheduling implicitly.
- Keep scheduler definitions free of secret values. Configuration may reference an environment variable only when the selected scheduler environment can provide it; otherwise installation must fail with an actionable error.
- Preserve every failed or non-empty run directory for diagnosis and recovery. Automatic cleanup may remove only a newly allocated directory that produced no artifacts.
- Treat a completed filesystem Apply with pending SQLite finalization as resumable. Complete Inbox and history indexes before marking the managed run completed.

## Consequences

- Positive: scheduled and interactive runs share identical planning, mutation, locking, and recovery behavior.
- Positive: no Temari process remains resident between cycles.
- Positive: users can inspect, stop, or remove automation through normal platform tools.
- Negative: observation time is bounded by the configured schedule interval rather than filesystem event delivery.
- Negative: two small platform adapters and their escaping rules require tests.
- Negative: environment-backed credentials require explicit scheduler configuration or an owner-only inline configuration value.

## Adoption and Exceptions

- Renderer tests must prove that generated definitions use argument arrays or direct executable fields, absolute paths, private file modes, and no shell interpolation.
- Install and uninstall may touch only the exact per-workspace user unit or agent files owned by Temari.
- Uninstall must verify that the timer or agent stopped before deleting its owned definitions.
- Integration tests must prove that two automatically allocated runs cannot reuse a directory and that failed journals remain reachable.
- A future filesystem watcher or daemon requires a separate ADR and must continue to call the same finite application services.
