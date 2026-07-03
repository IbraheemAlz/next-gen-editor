/* Phase 4 §12 — clipboard. Backlog sprint 7 — rich payloads.
 *
 * Rich clipboard round-trip over the async `navigator.clipboard` API. Copy
 * and cut push the selection as both `text/plain` and `text/html` in one
 * `ClipboardItem`; paste prefers `text/html` (parsed into styled paragraphs
 * by the engine) and falls back to `text/plain`. The engine owns the
 * document — these functions only move content across the boundary.
 *
 * The engine also generates a `.docx` fragment (`ClipboardPayload.docx_fragment`),
 * but a ZIP blob under a custom MIME has poor cross-browser clipboard support,
 * so it is not placed on the system clipboard here — `text/html` is the
 * portable rich format every target app understands.
 *
 * Issue #48 — copy/cut await a worker round-trip (GET_SELECTION_AS_CLIPBOARD)
 * BEFORE touching `navigator.clipboard`, so the write happens outside the
 * trusted copy-event window and focus can lapse ("Document is not focused" →
 * NotAllowedError). The write is therefore a hardened ladder: rich write →
 * plain-text write → guarded refocus + one plain-text retry → typed
 * `ClipboardWriteError`. Callers must observe the rejection and surface it
 * to the user — never `void` these promises. */
import type { EngineClient } from '../engine/engine-client';

/** Typed failure for a totally-blocked clipboard write (issue #48).
 *  Thrown only after every fallback tier failed: rich write → plain-text
 *  write → refocus + plain-text retry. `cut()` uses it to skip the delete
 *  (write-before-delete: no data loss on a blocked write). */
export class ClipboardWriteError extends Error {
    constructor(cause: unknown) {
        super('clipboard write blocked by the browser', { cause });
        this.name = 'ClipboardWriteError';
    }
}

/** Re-grab focus for the hidden textarea so a clipboard write regains a
 *  focused document. Queries both host shapes of the documented focus
 *  hook (`packages/ui/src/focus.ts`).
 *
 *  CRITICAL GUARD (issues #35/#49, mirroring `HiddenInput.onWindowFocus`):
 *  only refocus when nothing else legitimately holds focus — the active
 *  element is `<body>`, `null`, or already the hidden textarea. Never
 *  steal focus from a dialog or form control. */
function refocusHiddenInput(): void {
    const input = document.querySelector<HTMLElement>(
        '#nge-hidden-input, textarea[data-nge-hidden-input]',
    );
    if (!input) return;
    const a = document.activeElement;
    if (a && a !== document.body && a !== input) return;
    input.focus();
}

/** Write the selection to the clipboard as `text/plain` + `text/html`.
 *  Resolves on rich OR plain-text-only success (acceptable degradation);
 *  rejects with `ClipboardWriteError` only when every tier failed. */
async function writeRich(plain: string, html: string): Promise<void> {
    /* Tier 1 — one `ClipboardItem` carries every MIME at once; the target
       app reads whichever it understands. `write` can reject (permissions,
       lost focus, no `ClipboardItem` support). */
    try {
        const item = new ClipboardItem({
            'text/plain': new Blob([plain], { type: 'text/plain' }),
            'text/html': new Blob([html], { type: 'text/html' }),
        });
        await navigator.clipboard.write([item]);
        return;
    } catch {
        /* fall through to the plain-text tier */
    }
    /* Tier 2 — plain-text-only write. */
    try {
        await navigator.clipboard.writeText(plain);
        return;
    } catch {
        /* likely NotAllowedError "Document is not focused" — refocus and
           retry once below */
    }
    /* Tier 3 — guarded refocus, then one final plain-text retry. */
    refocusHiddenInput();
    try {
        await navigator.clipboard.writeText(plain);
    } catch (e: unknown) {
        throw new ClipboardWriteError(e);
    }
}

/** Copy the current selection to the system clipboard (plain + HTML).
 *  Rejects with `ClipboardWriteError` when the browser blocked every
 *  write tier — callers surface the failure to the user. */
export async function copy(client: EngineClient): Promise<void> {
    const evt = await client.dispatch({ type: 'GET_SELECTION_AS_CLIPBOARD' });
    if (evt.type !== 'CLIPBOARD_PAYLOAD' || evt.plain === '') return;
    /* The worker round-trip above left the trusted copy-event window;
       re-establish focus (guarded) before the first write attempt. */
    if (!document.hasFocus()) refocusHiddenInput();
    await writeRich(evt.plain, evt.html);
}

/** Copy the selection to the clipboard, then delete it from the document.
 *
 *  WRITE-BEFORE-DELETE INVARIANT: the delete dispatches ONLY after the
 *  clipboard write succeeded. On a totally-blocked write, `writeRich`
 *  throws, the delete is skipped, and the error propagates — the user's
 *  selection is never destroyed without a copy landing somewhere. */
export async function cut(client: EngineClient): Promise<void> {
    const evt = await client.dispatch({ type: 'GET_SELECTION_AS_CLIPBOARD' });
    if (evt.type !== 'CLIPBOARD_PAYLOAD' || evt.plain === '') return;
    if (!document.hasFocus()) refocusHiddenInput();
    await writeRich(evt.plain, evt.html);
    await client.dispatch({ type: 'DELETE_AT_CARET', forward: false, by_word: false });
}

/** Paste the system clipboard at the caret — HTML when present, else plain.
 *  When `forcePlain` is true (Ctrl/Cmd+Shift+V, UX_BEHAVIOR_SPEC §V.2),
 *  the HTML path is bypassed entirely so the engine never sees any
 *  rich markup — Word's "Paste Special → Unformatted Text". */
export async function paste(client: EngineClient, forcePlain = false): Promise<void> {
    /* The typed `read()` exposes every MIME so a `text/html` payload routes
       to the rich PasteHtml path. It is unavailable or permission-blocked in
       some browsers — fall back to plain-text `readText()`. */
    try {
        const items = await navigator.clipboard.read();
        if (!forcePlain) {
            for (const item of items) {
                if (item.types.includes('text/html')) {
                    const html = await (await item.getType('text/html')).text();
                    if (html) {
                        await client.dispatch({ type: 'PASTE_HTML', html });
                        return;
                    }
                }
            }
        }
        for (const item of items) {
            if (item.types.includes('text/plain')) {
                const text = await (await item.getType('text/plain')).text();
                if (text) await client.dispatch({ type: 'PASTE_PLAIN', text });
                return;
            }
        }
    } catch {
        const text = await navigator.clipboard.readText();
        if (text) await client.dispatch({ type: 'PASTE_PLAIN', text });
    }
}
