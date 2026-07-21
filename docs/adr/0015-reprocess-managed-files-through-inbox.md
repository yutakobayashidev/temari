# ADR 0015: Reprocess managed files through Inbox

## Status

- Accepted

## Decision Drivers

- A user must be able to reconsider selected protected files and previously classified files.
- Reprocessing must not bypass retention, approved opaque destination IDs, content-consent policy, Plan review, or Undo.
- The normal classifier intentionally excludes `Kept` and approved `Library` destinations.
- Adding a second classifier for files already inside managed areas would duplicate safety rules.

## Context

The initial managed workflow is intentionally one-way: existing directories move to `Kept`, new files wait in `Inbox`, and eligible files move to `Library`. Normal scans never traverse `Kept` or `Library`. That protects existing and classified material, but it also leaves no explicit way to reconsider a selected subtree after the user changes their intent or folder rules.

## Options Considered

- Classify directly from `Kept` or inside `Library`: avoids an intermediate move but requires exceptions to artifact scope and approved-destination exclusion rules.
- Copy selected files into Inbox: preserves originals but creates duplicate ownership and a later deletion protocol.
- Move selected files into Inbox through a model-free reviewed Plan: reuses existing Apply, Undo, collision, observation, and classification services.

## Decision

- Reprocess selected files by first producing a model-free Plan that moves them from `Kept` or `Library` into `Inbox`.
- Require explicit area-relative selectors. Directory selectors recurse without following symlinks. Library may support an explicit all-files selection; `Kept` requires selectors so its protected meaning remains the default.
- Keep planning and mutation separate. Reprocessing is read-only unless the caller explicitly applies the exact generated Plan.
- After a successful move into Inbox, clear matching processed-file markers and observe the files as pending. Do not clear markers before Apply succeeds.
- Let the normal retention, stability, local-rule, content-consent, opaque destination, Plan, and Apply services perform the later classification.
- Record the staging run in managed history and allow normal session or selected-file Undo to restore the original `Kept` or `Library` path.

## Consequences

- Positive: one classification path owns every model and privacy decision.
- Positive: a reprocessing move is visible, reviewable, collision-safe, and independently undoable.
- Positive: `Kept` and `Library` remain excluded from unattended classification.
- Negative: reprocessing takes two finite cycles when retention is non-zero.
- Negative: empty source directories are preserved after selected files leave them.
- Negative: moving a large selected subtree requires a correspondingly large reviewed Plan and journal.

## Adoption and Exceptions

- Core tests must reject path escape, wrong-area selectors, symlink traversal, non-portable names, implicit full-Kept selection, and stale source fingerprints.
- End-to-end tests must cover Library to Inbox, later classification, and Undo to the original path.
- Direct model classification from `Kept` or `Library` is not allowed without a separate ADR and equivalent trust-boundary tests.
