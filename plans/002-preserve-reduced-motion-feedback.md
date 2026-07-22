# 002 — Preserve reduced-motion feedback

- **Status**: DONE
- **Commit**: 3174975
- **Severity**: MEDIUM
- **Category**: Accessibility
- **Estimated scope**: 1 file, about 12 CSS lines

## Problem

The current reduced-motion rule indiscriminately makes every animation and
transition effectively instant. That also removes non-spatial color and opacity
feedback that helps users understand state.

```css
/* apps/temari-desktop/src/styles.css:283-285 — current */
@media (prefers-reduced-motion: reduce) {
  *, *::before, *::after { scroll-behavior: auto !important; animation-duration: .01ms !important; transition-duration: .01ms !important; }
}
```

## Target

Keep the scroll override and disable only the press transforms introduced by plan
001. Color and opacity feedback must retain their normal 160ms timing.

```css
/* target */
@media (prefers-reduced-motion: reduce) {
  *, *::before, *::after {
    scroll-behavior: auto !important;
  }

  .rail-heading button:active,
  .workspace-link:active,
  .run-button:active:not(:disabled),
  .secondary-button:active:not(:disabled),
  .primary-button:active:not(:disabled),
  .danger-button:active:not(:disabled),
  .dialog-close:active,
  .source-suggestion:active,
  .picker-field button:active {
    transform: none;
  }
}
```

## Repo conventions to follow

- Extend the existing reduced-motion media query at the end of
  `apps/temari-desktop/src/styles.css`.
- Plan 001 is the selector and token exemplar. Keep the selector list identical so
  standard and reduced-motion modes cannot drift.

## Steps

1. Execute plan 001 first.
2. Replace the global duration override in the existing
   `prefers-reduced-motion: reduce` block with the Target block.
3. Confirm that `.quiet-button` and `.text-button` need no override because their
   feedback is opacity-only.

## Boundaries

- Do NOT set global animation or transition durations to zero or `.01ms`.
- Do NOT remove focus-visible, color, opacity, or disabled-state feedback.
- Do NOT change application state or TypeScript.
- Do NOT add dependencies.
- If plan 001 has not been applied or the selectors have drifted since commit
  `3174975`, STOP and report instead of improvising.

## Verification

- **Mechanical**: run `corepack pnpm --dir apps/temari-desktop build`. Run
  `rg -n 'animation-duration:\\s*\\.01ms|transition-duration:\\s*\\.01ms' apps/temari-desktop/src/styles.css`;
  it must return no matches.
- **Feel check**: emulate `prefers-reduced-motion: reduce` in DevTools. Press every
  selector listed above: none may scale, but background, border, and opacity
  feedback must remain visible. Turn reduced motion off and confirm the 0.97 press
  scale returns. Inspect at 10% playback speed to ensure there is no positional
  interpolation in reduced mode.
- **Done when**: reduced-motion users receive non-spatial state feedback without
  any press movement.

## Implementation note

The reduced-motion selectors follow plan 001's final `data-pointer-active`
implementation. Color and opacity transitions remain available.
