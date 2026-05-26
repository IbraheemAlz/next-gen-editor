---
description: SDK / UI architecture rules — Monaco Standard pnpm workspace, Solid.js primitives, .nge-* CSS, honest UX.
paths:
  - "packages/**"
  - "ts/src/sdk-bridge.tsx"
  - "ts/src/components/**"
  - "pnpm-workspace.yaml"
---

# SDK + UI architecture rules

Load whenever touching `packages/core`, `packages/ui`, the migration
anchor `ts/src/sdk-bridge.tsx`, or the workspace manifest. These rules
formalize the post-`beta.3` "Monaco Standard" split + the Honest UX
discipline.

## Monorepo

- **`pnpm` workspace** rooted at the repo. `pnpm-workspace.yaml` globs
  `packages/*`, `ts`, `tools/*`. Use `workspace:*` for inter-package
  dependencies. Run `pnpm install` after any new package; run
  `pnpm -r tsc` for a full type-check sweep.
- **Build order** when both halves change: Rust → `wasm-pack build` →
  `pnpm -r tsc`. The wasm-pack output (`crates/engine-wasm/pkg/`)
  carries the generated TS types that `@nge/core` re-exports.

## `@nge/core` — the Locked Surface + Headless API

- **Files** (current):
  - `src/EditorSurface.tsx` — owns `<canvas>` + hidden `<textarea>`,
    `transferControlToOffscreen` lifecycle, keyed-Show crash remount.
  - `src/EngineProvider.tsx` — Solid context + `useEngine()`.
  - `src/createEditorCommands.ts` — typed facade over `Engine.dispatch`.
    Every method maps 1:1 onto a `Command` variant in
    `crates/bridge/src/command.rs`. Range-aware helpers
    (`setBold`, `setUnderline`, …) auto-resolve from
    `state.selection()` when `range` is omitted.
  - `src/createEditorState.ts` — Solid signals fed by a single
    `engine.subscribe(...)`. Exposes `selection`, `caret`, `rects`,
    `attrsAtCaret`, `attrsMixed`, `paragraphAlignment`,
    `paragraphDirection`, `selectionKind`, `canUndo`, `canRedo`,
    `stats`, `lastPaintMs`, `paintVersion`,
    `estimatedDocumentHeight`, `renderer`.
  - `src/types.ts` — re-exports every bridge type from the wasm-pack
    output plus the `EngineClientLike` + `EngineClientSnapshots`
    interface contracts. **Single point of coupling** to
    `crates/engine-wasm/pkg/engine_wasm.js`.
- **Framework:** Solid.js (`createSignal`, `createEffect`, `onCleanup`,
  `<Show>`, `<For>`, `<Portal>`). No React patterns.
- **Surface discipline:**
  - Add a new bridge variant → extend `createEditorCommands` with a
    typed method. Do not let downstream UI assemble raw `Command`
    objects (the escape hatch `cmd.raw(...)` exists for emergencies).
  - Add a new event field → extend `createEditorState` with a signal.
    UI never calls `engine.subscribe` directly.
  - Selection-aware helpers (formatting, paragraph) read the current
    selection internally — pass `range` explicitly only when
    overriding.

## `@nge/ui` — default UI components

- **Strict CSS namespace:** every selector starts with `.nge-`. Theme
  variables on `:root` / `.nge-root` (e.g. `--nge-color-bg`,
  `--nge-color-text`, `--nge-color-primary`, `--nge-color-danger`,
  `--nge-color-warning`, `--nge-space-1..5`, `--nge-radius-sm/md/lg`,
  `--nge-shadow-sm/md/lg`, `--nge-z-overlay/popover/hud`,
  `--nge-toolbar-height`, `--nge-sidebar-width`).
- **Per-component `.css` sibling** imported from the `.tsx`. Component
  CSS extends the theme — never hard-codes colours, radii, or font
  stacks.
- **`@nge/ui/theme.css`** is the entry stylesheet consumers import
  once at the app root.
- **Zero Tailwind. Zero Shadcn. Zero MUI / Chakra / Mantine / Radix.**
  Native HTML elements + scoped CSS only. Modals via
  `solid-js/web` `<Portal>` to escape the canvas z-index context.
- **Component pattern:**
  ```tsx
  import { createEditorCommands, createEditorState } from '@nge/core';
  import './MyControl.css';
  export const MyControl: Component = () => {
      const cmd = createEditorCommands();
      const state = createEditorState();
      const ready = () => state.selection() !== undefined;
      return (
          <button class="nge-btn nge-my-control"
                  disabled={!ready()}
                  onClick={() => void cmd.someAction()}>...</button>
      );
  };
  ```
- **Disabled states** must not visually disappear — `disabled` + reduced
  opacity + accessible `title` tooltip explaining why.

## Honest UX — never ship Phantom UI

A control whose engine path is stubbed, missing, or partial must
visibly gate itself. The canonical pattern:

```tsx
<button
    class="nge-btn nge-feature__btn nge-feature__btn--disabled"
    type="button"
    disabled
    title="<Feature>: engine pending (<short reason>) — see backlog"
    onClick={() => void cmd.someAction()}  // still dispatches so the
                                            // ERROR banner / FileMenu
                                            // toast surfaces the gap
>
    <span>Label</span>
    <span class="nge-feature__badge">Engine pending</span>
</button>
```

The matching CSS chip:

```css
.nge-feature__badge {
    padding: 2px 6px;
    background: rgba(217, 119, 6, 0.12);
    border: 1px solid var(--nge-color-warning);
    color: var(--nge-color-warning);
    border-radius: 999px;
    font-size: 10px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.04em;
}
```

Every "engine pending" badge **must** have a matching GitHub issue
filed via the `gh-issue-logger` skill. JSDoc on the component
references the issue by number or title.

## Reference implementations

When unsure, copy the existing shape:

- **Toolbar combo with popover:** `UnderlineStyleDropdown.tsx`.
- **Toolbar shelf chip with grouped controls:** `ParagraphControls.tsx`,
  `LayoutControls.tsx`, `ReviewControls.tsx`.
- **Right-rail read-only data view:** `TrackChangesSidebar.tsx`,
  `CommentsRail.tsx`.
- **Modal dialog:** `Dialog.tsx` (the primitive) +
  `CellPropertiesDialog.tsx`, `PageSetupDialog.tsx`.
- **Engine-pending pattern:** `ListButtons.tsx` (Bullet/Number badges),
  `FileMenu.tsx` (HTML / Plain Text export entries), `ReviewControls.tsx`
  (Track Changes toggle).
- **Pinned bottom strip:** `StatusBar.tsx`.
- **Crash overlay via Portal:** `TrapOverlay.tsx`.

## Migration anchor — `ts/src/sdk-bridge.tsx`

The concrete `EngineClient` (worker spawn + RPC + IndexedDB event log)
still lives at `ts/src/engine/engine-client.ts`. `sdk-bridge.tsx`
casts it to `EngineHandle` and wires it into `EngineProvider`. New
components mount inside the `SdkShelf` JSX. The eventual move of
`EngineClient` into `@nge/core` is a separate refactor — until then,
treat `sdk-bridge.tsx` as the single integration seam.
