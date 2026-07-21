# ADR 0013: Make managed the primary CLI

## Status

- Accepted

## Decision Drivers

- Normal personal use is a long-lived folder with protected, waiting, and classified areas.
- Multiple public commands for recurring classification make the product model harder to learn.
- Agents and recovery tooling still need explicit versioned artifact primitives.
- One-time cleanup remains useful but should not define the default product experience.

## Context

Temari temporarily exposed two recurring workflows: a generic monitor command family and the newer managed workspace. Both used the same classification, rule, Plan, Apply, SQLite, and reconciliation services, but only managed represented the intended `Kept`, `Inbox`, and `Library` experience. Keeping both public made users choose an implementation mechanism instead of a product outcome.

The CLI has not been released and the repository explicitly avoids compatibility shims for experimental contracts.

## Options Considered

- Keep both command families: preserves experiments but duplicates history, rule, apply, and recovery concepts.
- Keep monitor public and make managed a wrapper: centers the implementation abstraction instead of the user-visible workspace.
- Make managed primary and retain monitoring only inside Core: gives one recurring workflow while preserving tested services.

## Decision

- Use `temari managed` as the only public recurring organization workflow.
- Nest deterministic filename rules under `temari managed rule`; a workspace ID replaces the internal monitor ID at the CLI boundary.
- Remove the public `temari monitor`, top-level `temari rule`, and top-level `temari history` commands without compatibility aliases.
- Keep `temari organize` as a TTY-only, one-time cleanup orchestrator.
- Keep `propose`, `approve`, `plan`, `apply`, `undo`, and `resume` as advanced primitives for agents, scripts, artifact inspection, and recovery.
- Retain monitoring records, processed signatures, rules, polling-independent cycle services, and reconciliation inside `temari-core`. Do not expose internal monitor IDs as the normal user model.
- Do not install a daemon or continuous foreground loop. A future scheduler or GUI must call the same finite managed application services.

## Consequences

- Positive: one command family owns setup, retention, classification, history, rules, apply, resume, and undo.
- Positive: help and documentation lead with the physical workspace users can inspect.
- Positive: Core retains reusable scheduling and reconciliation mechanisms without leaking them into product terminology.
- Negative: scripts written against the experimental monitor commands must move to managed workspace IDs and finite runs.
- Negative: managed setup still exposes explicit artifact stages instead of a single setup wizard; a future interactive orchestrator may reduce that friction.

## Adoption and Exceptions

- Top-level help and README examples must list managed first, organize second, and primitive commands as advanced operations.
- CLI tests must reject removed monitor, top-level rule, and top-level history commands.
- New recurring features, including rules and history queries, belong below `managed` unless a later ADR establishes a distinct product workflow.
- A GUI or service may use internal monitoring APIs, but it must present managed workspace identity and preserve Plan, Apply, resume, and undo boundaries.
