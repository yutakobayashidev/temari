# ADR 0003: Redesign the CLI around versioned workflow artifacts

## Status

- Accepted

## Decision Drivers

- Folder proposal, human approval, classification planning, filesystem apply, and undo have different trust boundaries.
- Humans need a guided workflow, while agents and scripts need deterministic non-interactive primitives.
- Filesystem mutation must be reviewable and recoverable.
- The CLI has not been released, so preserving its current configuration and flag contract has no user value.
- The existing Rust scanning, classification, endpoint, and response-validation code remains useful.

## Context

- The current CLI exposes only `plan` and stores model connection settings and approved folders in one `.temari.toml` file.
- The intended experience is a stateful flow: select a root, scan, choose scope, review or customize a proposed hierarchy, approve execution, observe progress, review completion, optionally keep the folder organized, and undo.
- ADR 0002 already separates proposal, approval, plan, and apply as security boundaries.

## Options Considered

- Extend the current `plan` command and combined configuration: minimal short-term edits, but obscures state transitions and makes safe apply and undo artifacts awkward.
- Rewrite the CLI and core: produces a clean surface, but discards tested scanning and validation without benefit.
- Preserve the core and intentionally redesign the CLI and artifact boundaries now: limits churn to the presentation and application layers while making the workflow explicit.

## Decision

- Preserve and extend `temari-core`; do not rewrite it.
- Replace the experimental CLI contract before the first release. No compatibility shim is required.
- Use these canonical commands:
  - `temari propose <SOURCE> --out <PROPOSAL>`
  - `temari approve <PROPOSAL> --out <FOLDER_SET>`
  - `temari plan <SOURCE> --folders <FOLDER_SET> --out <PLAN>`
  - `temari apply <PLAN> --out <APPLY_SESSION> [--yes]`
  - `temari undo <APPLY_SESSION> --out <UNDO_SESSION> [--yes]`
- `temari organize <SOURCE> --out <RUN_DIR>` is an interactive orchestrator over the canonical services, not a separate implementation.
- Split long-lived model settings from versioned workflow artifacts: `Proposal`, `FolderSet`, `Plan`, `ApplySession`, and `UndoSession`.
- Remove `--quiet`. Primary output must never disappear. Use stdout for results and artifacts; use stderr for progress, prompts, and diagnostics.
- Keep `--json` for machine-readable command results. Artifact files always use canonical versioned JSON regardless of terminal rendering.
- Interactive approval is allowed only on a TTY. Non-interactive approval requires an explicit acceptance flag; non-interactive apply requires `--yes`.

## Consequences

- Positive: each command corresponds to one security boundary and one auditable artifact transition.
- Positive: the GUI can orchestrate the same application services and artifact schemas later.
- Positive: agents can inspect or modify a proposal without receiving filesystem mutation authority.
- Negative: the current example configuration and `plan` syntax will change.
- Negative: artifact schemas and stale-plan checks must be designed before apply is implemented.
- Migration: move folders out of `.temari.toml`, introduce the four artifact types, then change `plan`; do not maintain the experimental combined format.

## Adoption and Exceptions

- New workflow behavior must live in core/application services, not only in CLI handlers.
- Every artifact includes a schema version and source identity. Plans additionally include file fingerprints and apply preconditions; undo writes a separate artifact instead of changing its apply session.
- Help and documentation must distinguish read-only commands from mutating commands.
- Tests must cover non-TTY refusal, explicit non-interactive approval, stale plans, interrupted apply, resumable audit sessions, and undo.
- A command that combines multiple trust-boundary stages requires either orchestration over the canonical commands or a superseding ADR.
