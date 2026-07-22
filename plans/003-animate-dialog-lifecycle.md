# 003 — Animate dialog lifecycle

- **Status**: DONE
- **Commit**: 3174975
- **Severity**: MEDIUM
- **Category**: Missed opportunity / Spatial consistency
- **Estimated scope**: 2 files, about 70 lines

## Problem

Setup, library editing, reprocessing, and filesystem confirmation dialogs are
inserted and removed by full renders. Both backdrop and centered surface therefore
appear or disappear in one frame.

```ts
// apps/temari-desktop/src/main.ts:439-452 — current
function render(): void {
  app.innerHTML = `<div class="app-shell">
    // ...
  </div>${setupDialog()}${libraryEditDialog()}${reprocessDialog()}${confirmationDialog()}`;
  bindEvents();
}

// apps/temari-desktop/src/main.ts:617-624 — current
document.querySelectorAll<HTMLElement>("[data-close-setup]").forEach((button) => button.addEventListener("click", () => {
  state.setupOpen = false;
  render();
}));
document.querySelector("#open-library-editor")?.addEventListener("click", () => { state.libraryEditOpen = true; render(); });
document.querySelectorAll("[data-close-library-edit]").forEach((button) => button.addEventListener("click", () => { state.libraryEditOpen = false; render(); }));
```

```css
/* apps/temari-desktop/src/styles.css:174-176 — current */
.sheet-dialog, .small-dialog { position: fixed; inset: 0; z-index: 20; width: 100%; height: 100%; max-width: none; max-height: none; margin: 0; padding: 0; border: 0; background: transparent; }
.sheet-backdrop, .small-dialog::before { content: ""; position: fixed; inset: 0; background: rgba(15,25,39,.62); backdrop-filter: blur(3px); }
.sheet-card { position: relative; width: min(620px, calc(100vw - 36px)); max-height: calc(100vh - 50px); margin: 25px auto; padding: 30px; overflow: auto; border-radius: 11px; background: var(--paper); box-shadow: 0 25px 80px rgba(12,22,35,.35); }
```

## Target

Use WAAPI so the existing full-render architecture stays intact. Modals remain
center-origin: enter over 220ms from `scale(0.97)` and opacity 0; exit over 160ms
to `scale(0.98)` and opacity 0. Reduced-motion mode keeps the opacity transition
but omits transform frames.

```ts
// target constants and helpers
const EASE_OUT = "cubic-bezier(0.23, 1, 0.32, 1)";
const DIALOG_ENTER_MS = 220;
const DIALOG_EXIT_MS = 160;

function dialogSurface(dialog: HTMLDialogElement): HTMLElement | null {
  return dialog.querySelector<HTMLElement>(".sheet-card, :scope > form, :scope > section");
}

function animateDialogEntry(previousDialogId: string | null): void {
  const dialog = document.querySelector<HTMLDialogElement>("dialog[open]");
  if (!dialog || dialog.id === previousDialogId) return;
  const reduced = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  dialog.animate([{ opacity: 0 }, { opacity: 1 }], {
    duration: DIALOG_ENTER_MS,
    easing: EASE_OUT,
  });
  if (!reduced) {
    dialogSurface(dialog)?.animate(
      [{ transform: "scale(0.97)" }, { transform: "scale(1)" }],
      { duration: DIALOG_ENTER_MS, easing: EASE_OUT },
    );
  }
}

async function closeDialog(dialogId: string, close: () => void): Promise<void> {
  const dialog = document.querySelector<HTMLDialogElement>(`#${dialogId}`);
  if (!dialog) return;
  const reduced = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  const animations = [dialog.animate([{ opacity: 1 }, { opacity: 0 }], {
    duration: DIALOG_EXIT_MS,
    easing: EASE_OUT,
  })];
  const surface = dialogSurface(dialog);
  if (surface && !reduced) {
    animations.push(surface.animate(
      [{ transform: "scale(1)" }, { transform: "scale(0.98)" }],
      { duration: DIALOG_EXIT_MS, easing: EASE_OUT },
    ));
  }
  await Promise.all(animations.map((animation) => animation.finished.catch(() => undefined)));
  close();
  render();
}
```

In `render()`, capture the existing open dialog ID before replacing `innerHTML`,
then call `animateDialogEntry(previousDialogId)` after `bindEvents()`:

```ts
const previousDialogId = document.querySelector<HTMLDialogElement>("dialog[open]")?.id ?? null;
// existing app.innerHTML assignment
bindEvents();
animateDialogEntry(previousDialogId);
```

## Repo conventions to follow

- Keep lifecycle orchestration in `apps/temari-desktop/src/main.ts`, beside the
  existing `render()` and event binding functions.
- Use full `transform` strings in WAAPI frames; do not introduce a motion library.
- Dialogs are centered surfaces, so center transform origin is correct. Do not add
  trigger-origin behavior intended for popovers.
- Use the same `cubic-bezier(0.23, 1, 0.32, 1)` value introduced as `--ease-out`
  by plan 001; JS cannot read the CSS token into WAAPI's easing option reliably
  without extra plumbing, so duplicate this exact value once as `EASE_OUT`.

## Steps

1. Add the constants and `dialogSurface`, `animateDialogEntry`, and `closeDialog`
   helpers before `render()` in `apps/temari-desktop/src/main.ts`.
2. Update `render()` exactly as shown so routine rerenders of an already-open
   dialog do not replay its entrance.
3. Route setup close, library-editor close, reprocess close, and confirmation
   cancellation through `closeDialog`. Each callback must mutate only its existing
   state flag after the exit animation completes.
4. Route transitions from the setup or library editor into confirmation through
   the same helper so the outgoing dialog finishes before the confirmation renders.
   Do not delay filesystem actions; only the pre-action UI handoff is animated.
5. Keep the existing CSS backdrop, surfaces, dimensions, and shadows unchanged.

## Boundaries

- Do NOT convert the application to a component framework or change the full-render
  architecture.
- Do NOT animate keyboard-driven workspace navigation or background refreshes.
- Do NOT animate width, height, margin, padding, top, or left.
- Do NOT use keyframes, springs, bounce, or `transition: all`.
- Do NOT change dialog copy, confirmation ordering, tokens, or filesystem actions.
- Do NOT add dependencies.
- If the dialog open/close paths have drifted since commit `3174975`, STOP and
  report instead of improvising.

## Verification

- **Mechanical**: run `corepack pnpm --dir apps/temari-desktop build`; it must
  complete without TypeScript or Vite errors. Search every assignment to
  `setupOpen`, `libraryEditOpen`, `reprocessOpen`, and `pendingConfirmation` and
  confirm user-initiated close paths use `closeDialog`.
- **Feel check**: open and close each of the four dialog types, including backdrop
  close and confirmation cancellation. The backdrop must fade, the centered card
  must begin at 97% rather than zero, and exit must feel faster than entry. Repeated
  renders inside an open dialog—selecting a suggested source or showing a busy
  label—must not replay the whole dialog entrance. At 10% DevTools playback, the
  card must stay center-origin. With reduced motion enabled, only opacity may
  change; position and scale must remain fixed.
- **Done when**: every dialog enters once, exits before removal, does not replay on
  internal rerenders, and honors reduced motion without changing workflow state.

## Implementation note

The final implementation additionally skips lifecycle motion after keyboard input,
serializes duplicate close requests, and snapshots the current computed opacity
and transform before canceling an in-progress entrance. Exit therefore continues
from the visible state instead of snapping back to a hard-coded start.
