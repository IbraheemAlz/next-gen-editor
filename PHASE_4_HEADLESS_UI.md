# Phase 4 — Headless UI Shell Integration

> **Parent:** [`MASTER_PLAN.md`](./MASTER_PLAN.md) §5.
> **Owning tracks:** E (UI), G (a11y QA).
> **Calendar:** Months 10–18.
> **Exit gate:** §11.

---

## 1. Objective

Build the production UI shell on top of the WASM engine:

1. **Solid.js application** owning chrome (toolbars, menus, dialogs, sidebar).
2. **Canvas mount** that transfers control to the engine worker.
3. **Input layer** — pointer, keyboard, IME (Arabic + CJK), clipboard, drag-drop.
4. **Overlay layer** — caret, selection rectangles, in-progress IME composition.
5. **Accessibility shadow tree** mirroring document structure for AT (NVDA / VoiceOver / Orca).
6. **State separation** — UI signals locally; document state strictly in WASM.

No iframes. Every UI piece is owned by us.

---

## 2. Deliverables

| ID | Deliverable | Acceptance signal |
| --- | --- | --- |
| D4.1 | Solid app skeleton | Vite + Solid + TS strict; routes; theming |
| D4.2 | Canvas mount + OffscreenCanvas transfer | First paint visible |
| D4.3 | Pointer → engine selection | Click + drag selects range |
| D4.4 | Hidden textarea + IME | Arabic + Japanese composition commits correctly |
| D4.5 | Caret overlay | Blinking DOM div tracks engine caret in real time |
| D4.6 | Selection overlay | Discontinuous BiDi selection renders |
| D4.7 | Toolbar | Bold/italic/underline/strike, font/size/color, align, list, undo/redo |
| D4.8 | Accessibility tree | NVDA/VoiceOver/Orca pass: read document, navigate by heading |
| D4.9 | Clipboard | Copy/paste with rich + plain MIMEs |
| D4.10 | Drag-drop | Drop .docx anywhere on viewport to open |

---

## 3. Stack choice

**Primary: Solid.js.**

- Fine-grained reactivity matches engine's event-driven model — signals update precisely the components watching a given event field.
- Runtime ~7 KB gzipped; no VDOM reconciliation jitter near canvas.
- TS-first.
- Compiles to direct DOM ops; fewer surprises for layout-adjacent code.

Acceptable alternatives if team experience dictates: React 19 with `useSyncExternalStore` + Million.js. Vue 3.5 with Composition API.

---

## 4. Component file structure

```
ts/src/
├── index.tsx                 (Solid root)
├── App.tsx                   (top-level layout: Toolbar + Editor + Sidebar)
├── engine/
│   ├── engine.worker.ts
│   ├── engine-client.ts
│   ├── event-log.ts          (IndexedDB)
│   └── types.ts              (re-export tsify-generated)
├── components/
│   ├── EditorCanvas.tsx
│   ├── CaretOverlay.tsx
│   ├── SelectionOverlay.tsx
│   ├── ImeOverlay.tsx
│   ├── HiddenInput.tsx
│   ├── AccessibilityTree.tsx
│   ├── Toolbar.tsx
│   ├── Sidebar.tsx
│   ├── StatusBar.tsx
│   └── Dialogs/
│       ├── OpenDocument.tsx
│       ├── ExportPdf.tsx
│       └── About.tsx
├── state/
│   ├── engine-store.ts       (signals derived from engine events)
│   ├── ui-store.ts           (chrome-only state)
│   └── prefs-store.ts        (user prefs, localStorage-backed)
├── input/
│   ├── pointer.ts
│   ├── keyboard.ts
│   ├── ime.ts
│   ├── clipboard.ts
│   └── dnd.ts
├── a11y/
│   ├── tree.tsx
│   └── announcements.ts
└── styles/
    ├── editor.css
    ├── toolbar.css
    └── caret.css
```

---

## 5. EditorCanvas — canvas mount + transfer

```tsx
import { onMount, onCleanup } from 'solid-js';
import { EngineClient } from '../engine/engine-client';

export function EditorCanvas(props: { client: EngineClient }) {
  let canvasRef: HTMLCanvasElement | undefined;
  let resizeObserver: ResizeObserver | undefined;

  onMount(async () => {
    const canvas = canvasRef!;
    const dpr = window.devicePixelRatio;
    canvas.width  = canvas.clientWidth  * dpr;
    canvas.height = canvas.clientHeight * dpr;
    const offscreen = canvas.transferControlToOffscreen();
    await props.client.init(offscreen);

    /* Notify engine on resize */
    resizeObserver = new ResizeObserver(() => {
      props.client.dispatch({
        type: 'SET_VIEWPORT',
        rect: { x: 0, y: 0, w: canvas.clientWidth * dpr, h: canvas.clientHeight * dpr },
      });
    });
    resizeObserver.observe(canvas);
  });

  onCleanup(() => resizeObserver?.disconnect());

  return <canvas ref={canvasRef} class="editor-canvas" tabindex="-1" />;
}
```

CSS:

```css
.editor-canvas {
    display: block;
    width: 100%;
    height: 100%;
    background: #fff;
    cursor: text;
    user-select: none;
    -webkit-user-select: none;
}
```

---

## 6. Hidden textarea + IME (Arabic + CJK)

The textarea is the OS's text-input citizen — it owns IME composition, keyboard repeat, autocorrect (disabled), and OS keyboard shortcuts. We position it at the caret so IME popups anchor correctly.

```tsx
import { createEffect, onMount } from 'solid-js';
import { engineStore } from '../state/engine-store';
import { EngineClient } from '../engine/engine-client';

export function HiddenInput(props: { client: EngineClient }) {
  let ref: HTMLTextAreaElement | undefined;
  const { caret } = engineStore;

  createEffect(() => {
    const c = caret();
    if (ref && c) {
      ref.style.left   = `${c.x}px`;
      ref.style.top    = `${c.y}px`;
      ref.style.height = `${c.h}px`;
    }
  });

  onMount(() => ref!.focus());

  const onCompositionStart = () => {
    props.client.dispatch({ type: 'BEGIN_COMPOSITION', at: engineStore.caretLogical()! });
  };
  const onCompositionUpdate = (e: CompositionEvent) => {
    props.client.dispatch({
      type: 'UPDATE_COMPOSITION',
      text: e.data,
      target_range: null,
    });
  };
  const onCompositionEnd = (e: CompositionEvent) => {
    props.client.dispatch({ type: 'END_COMPOSITION', commit: true });
    if (ref) ref.value = '';
  };
  const onBeforeInput = (e: InputEvent) => {
    if (e.isComposing) return;          /* composition path takes over */
    e.preventDefault();
    const cmd = mapInputEventToCommand(e, engineStore.caretLogical()!);
    if (cmd) props.client.dispatch(cmd);
    if (ref) ref.value = '';
  };
  const onKeyDown = (e: KeyboardEvent) => {
    if (e.isComposing) return;
    const cmd = mapKeydownToCommand(e, engineStore);
    if (cmd) { e.preventDefault(); props.client.dispatch(cmd); }
  };

  return (
    <textarea
      ref={ref}
      class="hidden-input"
      aria-hidden="true"
      tabindex="-1"
      autocomplete="off"
      autocorrect="off"
      autocapitalize="off"
      spellcheck={false}
      onCompositionStart={onCompositionStart}
      onCompositionUpdate={onCompositionUpdate}
      onCompositionEnd={onCompositionEnd}
      onBeforeInput={onBeforeInput}
      onKeyDown={onKeyDown}
    />
  );
}
```

CSS (positioned at caret so IME popups + composition underline anchor correctly):

```css
.hidden-input {
    position: absolute;
    opacity: 0.01;        /* not 0 — Safari skips events on opacity:0 */
    pointer-events: none;
    width: 1px;
    height: 1em;
    font-size: 16px;
    border: 0;
    outline: 0;
    resize: none;
    overflow: hidden;
    z-index: 1;
}
```

`mapInputEventToCommand`:

```ts
function mapInputEventToCommand(e: InputEvent, caret: LogicalPos): Command | null {
  switch (e.inputType) {
    case 'insertText':
      return e.data ? { type: 'INSERT_TEXT', at: caret, text: e.data, ime: false } : null;
    case 'insertParagraph':
      return { type: 'SPLIT_PARAGRAPH', at: caret };
    case 'deleteContentBackward':
      return { type: 'DELETE_RANGE', range: { start: prev(caret), end: caret } };
    case 'deleteContentForward':
      return { type: 'DELETE_RANGE', range: { start: caret, end: next(caret) } };
    case 'deleteWordBackward':
      return { type: 'DELETE_RANGE', range: { start: prevWord(caret), end: caret } };
    case 'insertFromPaste':
      /* clipboard handler takes over; ignore */
      return null;
    default:
      return null;
  }
}
```

---

## 7. Pointer input → engine

The engine owns hit-testing; the UI only forwards coordinates.

```ts
export function attachPointer(canvas: HTMLCanvasElement, client: EngineClient) {
  let dragging = false;
  let dragStart: { x: number; y: number } | null = null;
  const dpr = () => window.devicePixelRatio;

  const toCanvas = (e: PointerEvent) => {
    const r = canvas.getBoundingClientRect();
    return { x: (e.clientX - r.left) * dpr(), y: (e.clientY - r.top) * dpr() };
  };

  canvas.addEventListener('pointerdown', async (e) => {
    canvas.setPointerCapture(e.pointerId);
    dragging = true;
    dragStart = toCanvas(e);
    const hit = await client.dispatch({ type: 'HIT_TEST', at: dragStart });
    if (hit.type === 'HIT_RESULT') {
      client.dispatch({
        type: 'SET_SELECTION',
        range: { start: hit.pos, end: hit.pos },
        caret: hit.pos,
      });
    }
  });

  canvas.addEventListener('pointermove', async (e) => {
    if (!dragging) return;
    const p = toCanvas(e);
    const hit = await client.dispatch({ type: 'HIT_TEST', at: p });
    if (hit.type === 'HIT_RESULT') {
      client.dispatch({ type: 'EXTEND_SELECTION', to: hit.pos, modifier: 'shift' });
    }
  });

  canvas.addEventListener('pointerup', (e) => {
    dragging = false;
    dragStart = null;
    canvas.releasePointerCapture(e.pointerId);
  });

  canvas.addEventListener('dblclick', async (e) => {
    const p = toCanvas(e);
    const hit = await client.dispatch({ type: 'SELECT_WORD_AT', at: p });
    /* engine emits SelectionChanged */
  });

  canvas.addEventListener('wheel', (e) => {
    e.preventDefault();
    client.dispatch({ type: 'SCROLL_BY', dx: e.deltaX, dy: e.deltaY });
  }, { passive: false });
}
```

---

## 8. Caret + selection overlays

DOM-rendered, not canvas-drawn. CSS handles blink animation. Repositioned via Solid signals.

```tsx
export function CaretOverlay() {
  const { caret } = engineStore;
  return (
    <Show when={caret()}>
      <div
        class="caret"
        style={{
          left:   `${caret()!.x}px`,
          top:    `${caret()!.y}px`,
          width:  `${caret()!.w}px`,
          height: `${caret()!.h}px`,
        }}
      />
    </Show>
  );
}

export function SelectionOverlay() {
  const { selection } = engineStore;
  return (
    <For each={selection().rects}>{(r) => (
      <div class="selection-rect" style={{
        left:   `${r.x}px`,
        top:    `${r.y}px`,
        width:  `${r.w}px`,
        height: `${r.h}px`,
      }} />
    )}</For>
  );
}
```

CSS:

```css
.caret {
    position: absolute;
    background: var(--caret-color, currentColor);
    pointer-events: none;
    animation: caret-blink 1.06s steps(1) infinite;
    z-index: 2;
}
@keyframes caret-blink { 50% { opacity: 0; } }

.selection-rect {
    position: absolute;
    background: rgba(0, 100, 200, 0.25);
    pointer-events: none;
    z-index: 1;
}
```

---

## 9. State management

```ts
// state/engine-store.ts
import { createSignal } from 'solid-js';
import type { Event, LogicalRange, LogicalPos, Rect } from '../engine/types';
import { EngineClient } from '../engine/engine-client';

export function createEngineStore(client: EngineClient) {
  const [caret, setCaret] = createSignal<Rect | null>(null);
  const [caretLogical, setCaretLogical] = createSignal<LogicalPos | null>(null);
  const [selection, setSelection] = createSignal<{ range: LogicalRange; rects: Rect[] }>({ range: emptyRange(), rects: [] });
  const [a11yTree, setA11yTree] = createSignal(emptyA11yTree());
  const [docMeta, setDocMeta] = createSignal<DocMeta | null>(null);
  const [undoState, setUndoState] = createSignal({ can_undo: false, can_redo: false });
  const [attrsAtCaret, setAttrsAtCaret] = createSignal<TextAttrs>(defaultAttrs());

  client.subscribe((ev: Event) => {
    switch (ev.type) {
      case 'SELECTION_CHANGED':
        setCaret(ev.caret);
        setCaretLogical(ev.range.end);
        setSelection({ range: ev.range, rects: ev.rects });
        setAttrsAtCaret(ev.attrs_at_caret);
        break;
      case 'DOCUMENT_LOADED':
        setDocMeta(ev.meta);
        break;
      case 'UNDO_STATE_CHANGED':
        setUndoState({ can_undo: ev.can_undo, can_redo: ev.can_redo });
        break;
      case 'ACCESSIBILITY_TREE_CHANGED':
        setA11yTree(applyA11yDelta(a11yTree(), ev.delta));
        break;
      case 'FORMATTING_CHANGED':
        setAttrsAtCaret(ev.attrs);
        break;
    }
  });

  return { caret, caretLogical, selection, a11yTree, docMeta, undoState, attrsAtCaret };
}
```

**Invariant: UI does not duplicate document content.** It only mirrors signals that the engine sends. The toolbar reads `attrsAtCaret` to highlight Bold/Italic; it never asks the engine for "current text".

---

## 10. Accessibility tree

Engine emits an `A11yDelta` per command. UI applies it to a shadow `<div role="document">` placed offscreen but ARIA-visible.

```tsx
import { For } from 'solid-js';
import { engineStore } from '../state/engine-store';

export function AccessibilityTree() {
  const { a11yTree } = engineStore;
  return (
    <div role="document" class="a11y-mirror" aria-label="Document">
      <For each={a11yTree().paragraphs}>{(p) => (
        <p
          data-pid={p.id}
          dir={p.direction === 'rtl' ? 'rtl' : 'ltr'}
          role={p.heading ? `heading` : undefined}
          aria-level={p.heading?.level}
          lang={p.language}
        >
          <For each={p.runs}>{(r) => (
            <span style={r.style}>{r.text}</span>
          )}</For>
        </p>
      )}</For>
    </div>
  );
}
```

CSS:

```css
.a11y-mirror {
    position: absolute;
    left: -10000px;
    top: 0;
    width: 1px;
    height: 1px;
    overflow: hidden;
}
```

Screen reader live region for status announcements (selection changes, document loaded):

```tsx
export function Announcements() {
  const { announcement } = engineStore;
  return <div role="status" aria-live="polite" class="visually-hidden">{announcement()}</div>;
}
```

---

## 11. Toolbar wiring

```tsx
import { engineStore } from '../state/engine-store';

export function Toolbar(props: { client: EngineClient }) {
  const { attrsAtCaret, undoState, selection } = engineStore;
  const dispatch = (patch: Partial<TextAttrsPatch>) =>
    props.client.dispatch({ type: 'APPLY_FORMATTING', range: selection().range, attrs: patch });

  return (
    <div class="toolbar" role="toolbar" aria-label="Formatting">
      <button onClick={() => props.client.dispatch({ type: 'UNDO' })} disabled={!undoState().can_undo} aria-label="Undo">↶</button>
      <button onClick={() => props.client.dispatch({ type: 'REDO' })} disabled={!undoState().can_redo} aria-label="Redo">↷</button>
      <span class="sep" />
      <button onClick={() => dispatch({ bold: !attrsAtCaret().bold })}     aria-pressed={attrsAtCaret().bold}     class="b">B</button>
      <button onClick={() => dispatch({ italic: !attrsAtCaret().italic })} aria-pressed={attrsAtCaret().italic} class="i">I</button>
      <button onClick={() => dispatch({ underline: attrsAtCaret().underline ? null : 'single' })} aria-pressed={!!attrsAtCaret().underline} class="u">U</button>
      <FontFamilyPicker value={attrsAtCaret().font_family} onChange={(v) => dispatch({ font_family: v })} />
      <FontSizePicker value={attrsAtCaret().font_size} onChange={(v) => dispatch({ font_size: v })} />
      <ColorPicker value={attrsAtCaret().color} onChange={(v) => dispatch({ color: v })} />
      <AlignmentPicker value={attrsAtCaret().align} onChange={(v) => props.client.dispatch({ type: 'SET_PARAGRAPH_ALIGN', align: v, range: selection().range })} />
    </div>
  );
}
```

---

## 12. Clipboard

```ts
export async function copy(client: EngineClient) {
  const evt = await client.dispatch({ type: 'GET_SELECTION_AS_CLIPBOARD' });
  if (evt.type === 'CLIPBOARD_PAYLOAD') {
    const item = new ClipboardItem({
      'text/plain': new Blob([evt.plain], { type: 'text/plain' }),
      'text/html':  new Blob([evt.html],  { type: 'text/html'  }),
      'application/vnd.openxmlformats-officedocument.wordprocessingml.document':
        new Blob([evt.docx_fragment], { type: 'application/vnd.openxmlformats-officedocument.wordprocessingml.document' }),
    });
    await navigator.clipboard.write([item]);
  }
}

export async function paste(client: EngineClient) {
  const items = await navigator.clipboard.read();
  for (const item of items) {
    if (item.types.includes('application/vnd.openxmlformats-officedocument.wordprocessingml.document')) {
      const blob = await item.getType('application/vnd.openxmlformats-officedocument.wordprocessingml.document');
      const bytes = new Uint8Array(await blob.arrayBuffer());
      await client.dispatch({ type: 'PASTE_DOCX_FRAGMENT', bytes });
      return;
    }
    if (item.types.includes('text/html')) {
      const blob = await item.getType('text/html');
      await client.dispatch({ type: 'PASTE_HTML', html: await blob.text() });
      return;
    }
    if (item.types.includes('text/plain')) {
      const blob = await item.getType('text/plain');
      await client.dispatch({ type: 'PASTE_PLAIN', text: await blob.text() });
      return;
    }
  }
}
```

Requires user-gesture context. Bound to `Cmd/Ctrl + C / V` in `keyboard.ts`.

---

## 13. Exit gate (Phase 4)

```bash
# 1. E2E typing including Arabic IME
playwright test ts/e2e/typing-arabic.spec.ts

# 2. Selection + cursor behavior on mixed-direction text
playwright test ts/e2e/bidi-selection.spec.ts

# 3. Accessibility audit (axe-core)
playwright test ts/e2e/a11y.spec.ts

# 4. Screen reader integration (manual, signed off)
#    - NVDA (Windows): read 5-page Arabic+English doc, navigate by heading
#    - VoiceOver (macOS): same
#    - Orca (Linux): same

# 5. Keyboard shortcuts
playwright test ts/e2e/shortcuts.spec.ts

# 6. Clipboard rich + plain round-trip
playwright test ts/e2e/clipboard.spec.ts

# 7. Drag-drop .docx open
playwright test ts/e2e/dnd-open.spec.ts

# 8. Theming + RTL UI layout
playwright test ts/e2e/rtl-ui.spec.ts
```

---

## 14. Risk register (Phase 4 specific)

| # | Risk | Likelihood | Detection | Mitigation |
| --- | --- | --- | --- | --- |
| 1 | Browser IME quirks (Safari Arabic) | High | Phase-4 manual test | Per-browser textarea positioning hacks; per-platform shortcut maps |
| 2 | Pointer-capture loss during drag (Firefox quirk) | Med | E2E flake | Re-acquire on `pointermove` if lost |
| 3 | A11y tree drift from engine | High | NVDA test corpus | Engine emits full subtree replacements every N commands as recovery |
| 4 | Toolbar state lag vs caret | Med | Manual | Pre-cache `attrsAtCaret` on selection change; emit synchronously from engine |
| 5 | Solid build size > 200 KB | Low | bundle-size CI | Code-split toolbar / dialogs; lazy-load |
| 6 | DOM caret overlay flicker on rapid edits | Med | Visual QA | `will-change: transform`; batch updates via `queueMicrotask` |

---

## 15. Hand-off into Phase 5

UI shell stable; engine + UI stable; Phase 5 hardens both and ships MVP. Phase 4 must deliver:

- Stable Solid app routable to `/edit/:docId`.
- All editing keyboard + pointer shortcuts wired.
- A11y tree validated against three screen readers.
- Telemetry hooks for paint p95, command latency, error rates.
- Feature flag scaffolding for staged rollout.
