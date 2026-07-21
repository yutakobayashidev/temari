# Agent Guidelines

## Product boundaries

- This is a private, independent implementation. Do not copy third-party code, branding, UI assets, internal identifiers, prompt wording, database schemas, or log strings.
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
- Keep durable apply and undo state in atomic JSON journals. Add SQLite only when monitoring, cross-session queries, or GUI state require it; do not move immutable workflow artifacts into the database.
- Serialize filesystem writers with an advisory lock on the canonical source directory. Monitoring may reuse a held `SourceLock`; never acquire the same source lock recursively.
- Local rules match file basenames deterministically and select approved opaque IDs only. Persist the rule ID in the Plan; rules may target local-only approved fallbacks because they are user-authored, not model output.
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
