# Agent Guidelines

## Product boundaries

- This is a private, independent implementation. Do not copy third-party code, branding, UI assets, internal identifiers, prompt wording, database schemas, or log strings.
- Linux and macOS are required platforms. Keep reusable behavior in `temari-core`; keep presentation in adapters such as `temari-cli`.
- Localhost is allowed by default. Every non-local model hostname must be explicitly allowlisted in configuration.
- The model may select opaque destination IDs only. Never execute a model-generated filesystem path.
- Keep folder proposal, user approval, planning, and filesystem apply as separate stages. A proposal is untrusted data until local approval assigns an opaque ID and validates its relative path.
- Preserve the read-only plan step before mutations. Apply and undo must remain explicit, auditable, collision-safe, and conservative under stale state.
- Treat `Proposal`, `FolderSet`, `Plan`, `ApplySession`, and `UndoSession` as separate versioned artifacts. Keep model connectivity out of those artifacts and approved folders out of application configuration.
- Keep durable apply and undo state in atomic JSON journals. Add SQLite only when monitoring, cross-session queries, or GUI state require it; do not move immutable workflow artifacts into the database.
- Preserve the canonical `propose -> approve -> plan -> apply -> undo` command boundaries. The interactive `organize` command must orchestrate the same application services.
- Keep `organize` as TTY-only orchestration; non-interactive callers use the primitive commands. Preserve both destination approval and exact apply confirmation.
- Resume may update only a `running` ApplySession after conservative filesystem reconciliation. Completed, failed, and partial-failure sessions are immutable, and undo must reject a running session.
- No telemetry or cloud model provider is enabled by default.

## Engineering workflow

- Prefer simple, focused changes. YAGNI, KISS, and DRY.
- Plan non-trivial work before editing.
- Add behavior-focused tests for trust-boundary validation.
- Before committing, check `README.md`, `AGENTS.md`, `CLAUDE.md`, and related files under `docs/` for impact.
- Record reusable lessons in `z-ai/lessons.md` after user corrections.
