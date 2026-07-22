# 001 — Unify button feedback

- **Status**: DONE
- **Commit**: 3174975
- **Severity**: MEDIUM
- **Category**: Physicality & origin / Purpose & frequency / Cohesion
- **Estimated scope**: 1 file, about 35 CSS lines

## Problem

The desktop has no shared motion vocabulary and most pressable controls provide no
physical down-state. The Run button is the exception, but it jumps upward on hover
without a transition. This makes the main action feel less stable than the rest of
the calm, utility-focused interface.

```css
/* apps/temari-desktop/src/styles.css:1-18 — current */
:root {
  color: #17202b;
  background: #edf1ef;
  font-family: "Avenir Next", Avenir, "Hiragino Sans", "Yu Gothic", sans-serif;
  font-synthesis: none;
  text-rendering: optimizeLegibility;
  --sumi: #172436;
  --sumi-soft: #223147;
  --paper: #fbfcfa;
  --washi: #edf1ef;
  --ink: #17202b;
  --muted: #68747a;
  --line: #d4dcd8;
  --indigo: #526aa7;
  --moss: #758c68;
  --clay: #cc7655;
  --gold: #b49756;
}

/* apps/temari-desktop/src/styles.css:87-88 — current */
.run-button { display: flex; flex: 0 0 auto; align-items: center; gap: 12px; min-width: 166px; padding: 13px 16px; border: 1px solid var(--sumi); border-radius: 7px; color: white; background: var(--sumi); text-align: left; cursor: pointer; box-shadow: 0 9px 22px rgba(23,36,54,.14); }
.run-button:hover:not(:disabled) { background: var(--sumi-soft); transform: translateY(-1px); }

/* apps/temari-desktop/src/styles.css:152 — current */
.secondary-button, .primary-button, .danger-button { min-height: 37px; padding: 0 13px; border-radius: 5px; font-size: 10px; font-weight: 650; cursor: pointer; }
```

## Target

Add one strong UI easing and two duration tokens. Remove the Run button's
decorative hover movement. Give substantial pointer targets a subtle press scale,
while small text actions use opacity feedback instead of spatial motion.

```css
/* target tokens in :root */
--ease-out: cubic-bezier(0.23, 1, 0.32, 1);
--duration-press: 160ms;
--duration-color: 160ms;

/* target interaction rules */
.rail-heading button,
.workspace-link,
.run-button,
.secondary-button,
.primary-button,
.danger-button,
.dialog-close,
.source-suggestion,
.picker-field button {
  transition:
    transform var(--duration-press) var(--ease-out),
    background-color var(--duration-color) ease,
    border-color var(--duration-color) ease,
    opacity var(--duration-color) ease;
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
  transform: scale(0.97);
}

.quiet-button,
.text-button {
  transition: opacity var(--duration-color) ease;
}

.quiet-button:active,
.text-button:active {
  opacity: 0.7;
}

.run-button:hover:not(:disabled) {
  background: var(--sumi-soft);
}
```

## Repo conventions to follow

- Keep design tokens in the existing `:root` block in
  `apps/temari-desktop/src/styles.css:1`.
- Continue using the existing compact single-file CSS architecture; do not create a
  motion stylesheet.
- Preserve existing focus-visible outlines at `styles.css:24-27` and
  `styles.css:245`; press feedback must not replace keyboard focus feedback.

## Steps

1. Add `--ease-out`, `--duration-press`, and `--duration-color` to the existing
   `:root` block in `apps/temari-desktop/src/styles.css`.
2. Add the grouped transition rules shown in the Target section after the base
   button rules. List explicit properties; do not use `transition: all`.
3. Add `scale(0.97)` press feedback to the substantial button selectors shown in
   the Target section. Keep disabled buttons stationary.
4. Add opacity-only press feedback to `.quiet-button` and `.text-button`.
5. Remove only `transform: translateY(-1px)` from the Run hover rule. Preserve its
   background change.

## Boundaries

- Do NOT animate workspace switching, keyboard shortcuts, or dashboard rerenders.
- Do NOT add hover scaling or spring/bounce motion.
- Do NOT change markup, colors, spacing, or typography.
- Do NOT add dependencies.
- If the cited selectors or token block have drifted since commit `3174975`, STOP
  and report instead of improvising.

## Verification

- **Mechanical**: run `corepack pnpm --dir apps/temari-desktop build`; TypeScript
  and Vite must complete successfully. Run
  `rg -n 'transition:\\s*all|translateY\\(-1px\\)' apps/temari-desktop/src/styles.css`;
  it must return no matches.
- **Feel check**: run `corepack pnpm --dir apps/temari-desktop dev` and press Run,
  primary, secondary, danger, workspace, source-choice, picker, and close buttons.
  Each substantial control should compress subtly and immediately, then return
  without bounce. Run must not move when merely hovered. Tab focus outlines must
  remain unchanged. In DevTools, play the transition at 10% speed and confirm only
  `transform`, color-related properties, and opacity transition.
- **Done when**: pointer presses have consistent feedback, text actions remain
  visually stable, and no hover interaction changes element position.

## Implementation note

The final implementation uses transient `data-pointer-active` attributes set from
pointer events instead of CSS `:active`. This preserves the planned 0.97 feedback
for mouse, touch, and pen input without animating keyboard activation.
