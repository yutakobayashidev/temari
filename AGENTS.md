# Agent Guidelines

## Product boundaries

- This is an independent implementation. Do not copy third-party code, branding, UI assets, internal identifiers, prompt wording, database schemas, or log strings.
- Linux and macOS are required platforms. Keep reusable behavior in `temari-core`; keep presentation in adapters such as `temari-cli`.
- Localhost is allowed by default. Every non-local model hostname must be explicitly allowlisted in configuration.
- The model may select opaque destination IDs only. Never execute a model-generated filesystem path.
- Name classification may request content only for ambiguous files. Send bounded locally extracted text only under explicit `on_demand` policy or after per-run `ask` consent; never send raw files or persist extracted text in workflow artifacts or logs.
- Treat `privacy.content = "ask"` as per-run consent. Prompt only in interactive `organize`, only after ambiguity is known, and before extraction. Primitive `plan` never prompts.
- Keep document extraction cross-platform and bounded. Parse supported ZIP/XML containers without unpacking them. OCR must be explicitly configured, invoked directly with fixed arguments, time-limited, and treated as a local fallback on every failure.
- Materialize deterministic extension fallbacks as approved destinations in `FolderSet`. Automatically added fallbacks are local-only; the model must never select an ID whose `model_visible` value is false.
- Keep folder proposal, user approval, planning, and filesystem apply as separate stages. A proposal is untrusted data until local approval assigns an opaque ID and validates its relative path.
- Preserve the read-only plan step before mutations. Apply and undo must remain explicit, auditable, collision-safe, and conservative under stale state.
- Keep recursive scope explicit and artifact-bound. Root files are always in scope; traverse only approved `ScanScope` subtrees, never follow symlinks, and exclude approved destination subtrees from planning.
- Treat `Proposal`, `FolderSet`, `Plan`, `ApplySession`, and `UndoSession` as separate versioned artifacts. Keep model connectivity out of those artifacts and approved folders out of application configuration.
- Treat managed AI Library structure edits as ordered logical immutable `FolderSet` revisions. Core assigns Add IDs; parent edits require explicit reject, cascade, or reparent semantics. Preserve existing opaque destination IDs, atomically update monitor/workspace bindings, and leave physical directories and existing files untouched; use explicit reprocessing or reorganization for file movement.
- Reorganize existing AI Library files only through a separate reviewed model-free Plan tied to one completed Configure run. Match authoritative processed identities, map preserved opaque IDs to revised paths, return removed IDs to Recents, leave attention items and old directories untouched, and keep Apply/Undo journals run-owned.
- Keep Configure Apply, Undo, and Redo recovery journals run-owned and fixed beside the Apply Session. Redo requires the exact undone binding and no newer Configure run; crash recovery must reconcile both pre-binding and post-binding interruption windows.
- Keep durable apply and undo state in atomic JSON journals. Add SQLite only when monitoring, cross-session queries, or GUI state require it; do not move immutable workflow artifacts into the database.
- Serialize filesystem writers with an advisory lock on the canonical source directory. Monitoring may reuse a held `SourceLock`; never acquire the same source lock recursively.
- Local rules match file basenames deterministically and select approved opaque IDs only. Persist the rule ID in the Plan; rules may target local-only approved fallbacks because they are user-authored, not model output.
- Preserve the canonical `propose -> approve -> plan -> apply -> undo` command boundaries. The interactive `organize` command must orchestrate the same application services.
- Keep `organize` as TTY-only orchestration; non-interactive callers use the primitive commands. Preserve both destination approval and exact apply confirmation.
- Keep `managed` as the only public recurring organization workflow. Monitoring records and services are internal implementation details; expose local rules, history, apply, resume, and undo through managed workspace IDs instead of a second monitor-oriented CLI.
- Keep recurring workspace orchestration in `temari-core::ManagedService`; CLI and desktop adapters must not duplicate the state transition sequence or shell out to each other.
- Bind each managed workspace to one canonical model configuration path. Revalidate that binding before model-backed runs and reuse it in schedule definitions; never fall back to a process-default configuration for an existing workspace.
- Desktop setup authority must remain backend-held, revisioned, and single-use. Consume the exact latest preview token before mutation and reject stale, replayed, or concurrent Apply requests.
- Keep scheduled execution finite and explicit. Platform schedulers may invoke `managed run`, but workspace setup and enablement must never install a daemon or schedule implicitly; generated definitions must use absolute arguments without a shell or embedded secrets.
- Keep systemd and launchd rendering and installation in `temari-schedule`. Desktop schedule activation must require an explicitly selected stable Temari CLI executable rather than guessing a helper path.
- Treat a classified file manually returned to the workspace root as durable user intent. Do not stage it again unless explicit reprocessing or Undo clears its processed identity.
- Support only `Manual Library`, `Recents`, and `AI Library`. Reject artifacts and state databases from older experimental layouts; do not add compatibility aliases, automatic upgrades, or migration commands.
- Adopt newly created root directories into `Manual Library` through a read-only setup-style Plan and durable journal; never move them through an unjournaled desktop-only path. Treat a directory manually returned from `Manual Library`, including by Undo, as durable user intent and derive that identity from authoritative setup and adoption journals rather than a duplicate database record.
- Reprocess protected or classified files only through a reviewed model-free Plan back to Recents. Do not classify directly from Manual Library or inside AI Library, and do not clear processed markers before the staging Apply succeeds.
- Keep workspace lifecycle metadata transactional with its internal monitor. Registration removal must leave physical areas and authoritative JSON artifacts untouched.
- Resume may update only a `running` ApplySession after conservative filesystem reconciliation. Completed, failed, and partial-failure sessions are immutable, and undo must reject a running session.
- No telemetry or cloud model provider is enabled by default.

## Engineering workflow

- Prefer simple, focused changes. YAGNI, KISS, and DRY.
- Plan non-trivial work before editing.
- Add behavior-focused tests for trust-boundary validation.
- Before committing, check `README.md`, `AGENTS.md`, `CLAUDE.md`, and related files under `docs/` for impact.
- Record reusable lessons in `z-ai/lessons.md` after user corrections.
