# ADR 0015: Reprocess managed files through Recents

## Status

- Accepted

## Decision Drivers

- A user must be able to reconsider selected protected files and previously classified files.
- Reprocessing must not bypass retention, approved opaque destination IDs, content-consent policy, Plan review, or Undo.
- The normal classifier intentionally excludes `Manual Library` and approved `AI Library` destinations.
- Adding a second classifier for files already inside managed areas would duplicate safety rules.

## Context

The initial managed workflow is intentionally one-way: existing directories move to `Manual Library`, new files wait in `Recents`, and eligible files move to `AI Library`. Normal scans never traverse `Manual Library` or `AI Library`. That protects existing and classified material, but it also leaves no explicit way to reconsider a selected subtree after the user changes their intent or folder rules.

## Options Considered

- Classify directly from `Manual Library` or inside `AI Library`: avoids an intermediate move but requires exceptions to artifact scope and approved-destination exclusion rules.
- Copy selected files into Recents: preserves originals but creates duplicate ownership and a later deletion protocol.
- Move selected files into Recents through a model-free reviewed Plan: reuses existing Apply, Undo, collision, observation, and classification services.

## Decision

- Reprocess selected files by first producing a model-free Plan that moves them from `Manual Library` or `AI Library` into `Recents`.
- Require explicit area-relative selectors. Directory selectors recurse without following symlinks. AI Library may support an explicit all-files selection; `Manual Library` requires selectors so its protected meaning remains the default.
- Keep planning and mutation separate. Reprocessing is read-only unless the caller explicitly applies the exact generated Plan.
- After a successful move into Recents, clear matching processed-file markers and observe the files as pending. Do not clear markers before Apply succeeds.
- Let the normal retention, stability, local-rule, content-consent, opaque destination, Plan, and Apply services perform the later classification.
- Record the staging run in managed history and allow normal session or selected-file Undo to restore the original `Manual Library` or `AI Library` path.

## Consequences

- Positive: one classification path owns every model and privacy decision.
- Positive: a reprocessing move is visible, reviewable, collision-safe, and independently undoable.
- Positive: `Manual Library` and `AI Library` remain excluded from unattended classification.
- Negative: reprocessing takes two finite cycles when retention is non-zero.
- Negative: empty source directories are preserved after selected files leave them.
- Negative: moving a large selected subtree requires a correspondingly large reviewed Plan and journal.

## Adoption and Exceptions

- Core tests must reject path escape, wrong-area selectors, symlink traversal, non-portable names, implicit full-Manual Library selection, and stale source fingerprints.
- End-to-end tests must cover AI Library to Recents, later classification, and Undo to the original path.
- Direct model classification from `Manual Library` or `AI Library` is not allowed without a separate ADR and equivalent trust-boundary tests.
