# Animation improvement plans

These plans were produced from a read-only motion audit at commit `3174975`.
They intentionally preserve Temari's calm, crisp desktop personality and add no
motion library.

| Plan | Title | Severity | Status | Depends on |
| --- | --- | --- | --- | --- |
| [001](001-unify-button-feedback.md) | Unify button feedback | MEDIUM | DONE | — |
| [002](002-preserve-reduced-motion-feedback.md) | Preserve reduced-motion feedback | MEDIUM | DONE | 001 |
| [003](003-animate-dialog-lifecycle.md) | Animate dialog lifecycle | MEDIUM | DONE | 001, 002 |
| [004](004-soften-setup-step-changes.md) | Soften setup step changes | MEDIUM | DONE | 003 |

## Recommended execution order

1. **001** establishes the shared easing and duration vocabulary and removes the
   unexplained Run hover jump.
2. **002** establishes the accessibility contract before any larger movement is
   introduced.
3. **003** adds occasional dialog motion while applying that contract in WAAPI.
4. **004** reuses the dialog helper conventions for the rare setup progression.

Execute and verify one plan at a time. If source code no longer matches the cited
commit and excerpts, stop and refresh the plan rather than applying approximate
edits.
