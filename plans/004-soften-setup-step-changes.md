# 004 — Soften setup step changes

- **Status**: DONE
- **Commit**: 3174975
- **Severity**: MEDIUM
- **Category**: Missed opportunity / State indication
- **Estimated scope**: 1 file, about 35 TypeScript lines

## Problem

The rare three-step setup journey replaces its heading and entire body in one
frame when the proposal or preview resolves. The sheet remains conceptually the
same surface, but its contents teleport between states.

```ts
// apps/temari-desktop/src/main.ts:358-382 — current
<p class="eyebrow">Add a managed folder · ${sourceStep ? "1" : structureStep ? "2" : "3"} of 3</p>
<h2 id="setup-title">${sourceStep ? "Choose one folder" : structureStep ? "Approve its AI Library" : "Review the exact setup"}</h2>
${sourceStep ? `...` : ""}
${structureStep ? `...` : ""}
${previewStep ? `...` : ""}

// apps/temari-desktop/src/main.ts:725-747 — current state changes
state.proposal = await proposeManagedWorkspace(state.setupSource, state.configPath);
state.setupStep = "structure";
// ...
state.setupPreview = await previewManagedWorkspace(state.proposal, retentionDays * 86_400, settleSeconds);
state.setupStep = "preview";
```

## Target

Animate only a genuine forward step change, never every full render. Use a 180ms
WAAPI opacity/blur transition on the new `.setup-step` content. Keep blur at 2px.
In reduced-motion mode use opacity only.

```ts
// target module state and helper
let animateSetupStepOnNextRender = false;

function animateSetupStep(): void {
  if (!animateSetupStepOnNextRender) return;
  animateSetupStepOnNextRender = false;
  const step = document.querySelector<HTMLElement>("#setup-dialog .setup-step");
  if (!step) return;
  const reduced = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  step.animate(
    reduced
      ? [{ opacity: 0 }, { opacity: 1 }]
      : [
          { opacity: 0, filter: "blur(2px)" },
          { opacity: 1, filter: "blur(0)" },
        ],
    {
      duration: 180,
      easing: "cubic-bezier(0.23, 1, 0.32, 1)",
    },
  );
}
```

Wrap the changing setup content, excluding the persistent close button, in one
element:

```html
<section class="sheet-card">
  <button class="dialog-close" ...>×</button>
  <div class="setup-step">
    <!-- eyebrow, h2, and the current source/structure/preview body -->
  </div>
</section>
```

Set `animateSetupStepOnNextRender = true` immediately before assigning
`state.setupStep = "structure"` and `state.setupStep = "preview"`. Call
`animateSetupStep()` after `animateDialogEntry(previousDialogId)` in `render()`.

## Repo conventions to follow

- Keep transient animation intent in a module variable, not `AppState`; it must not
  become product or workflow state.
- Follow plan 003's WAAPI and reduced-motion patterns.
- Use the exact strong ease-out curve `cubic-bezier(0.23, 1, 0.32, 1)` and 180ms
  duration. Do not introduce another timing token or curve.

## Steps

1. Execute plans 001, 002, and 003 first.
2. Add `animateSetupStepOnNextRender` and `animateSetupStep()` near the dialog
   animation helpers in `apps/temari-desktop/src/main.ts`.
3. Wrap only the changing setup copy and body in `.setup-step`; leave the close
   button outside so it never fades or blurs during step changes.
4. Set the one-shot flag immediately before each successful forward `setupStep`
   assignment. Do not set it for busy renders, source selection, picker returns,
   errors, or dialog entry.
5. Invoke `animateSetupStep()` once at the end of `render()` after dialog entry
   animation setup.

## Boundaries

- Do NOT delay API calls or block interaction for the animation.
- Do NOT animate setup dialog entry here; plan 003 owns that lifecycle.
- Do NOT animate source selection, field edits, busy labels, errors, or closing.
- Do NOT animate layout properties or add horizontal slide direction.
- Do NOT add dependencies or persist animation state.
- If setup rendering or step assignments have drifted since commit `3174975`, STOP
  and report instead of improvising.

## Verification

- **Mechanical**: run `corepack pnpm --dir apps/temari-desktop build`; it must
  complete successfully. Confirm `animateSetupStepOnNextRender` is assigned only
  for the `structure` and `preview` success paths.
- **Feel check**: enter setup and move through all three steps. The dialog itself
  should animate only once on opening. Steps two and three should resolve from a
  subtle 2px blur and opacity fade in 180ms; selecting a source, picking a config,
  or rendering a busy label must not replay the transition. At 10% playback, there
  must be no double-exposed sharp text and the close button must stay unchanged.
  With reduced motion enabled, confirm the blur is gone and only opacity changes.
- **Done when**: only successful forward step changes receive the one-shot content
  transition, with no effect on workflow timing or repeated renders.

## Implementation note

Review replaced the proposed `blur(2px)` with compositor-friendly
`translateY(4px)` plus opacity because the old step is not retained for a true
crossfade. Keyboard-initiated steps remain instant; reduced motion remains
opacity-only.
