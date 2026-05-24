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
 * portable rich format every target app understands. */
import type { EngineClient } from '../engine/engine-client';

/** Write the selection to the clipboard as `text/plain` + `text/html`. */
async function writeRich(plain: string, html: string): Promise<void> {
    /* One `ClipboardItem` carries every MIME at once; the target app reads
       whichever it understands. `write` can reject (permissions, lost focus,
       no `ClipboardItem` support) — fall back to a plain-text-only write. */
    try {
        const item = new ClipboardItem({
            'text/plain': new Blob([plain], { type: 'text/plain' }),
            'text/html': new Blob([html], { type: 'text/html' }),
        });
        await navigator.clipboard.write([item]);
    } catch {
        await navigator.clipboard.writeText(plain);
    }
}

/** Copy the current selection to the system clipboard (plain + HTML). */
export async function copy(client: EngineClient): Promise<void> {
    const evt = await client.dispatch({ type: 'GET_SELECTION_AS_CLIPBOARD' });
    if (evt.type !== 'CLIPBOARD_PAYLOAD' || evt.plain === '') return;
    await writeRich(evt.plain, evt.html);
}

/** Copy the selection to the clipboard, then delete it from the document. */
export async function cut(client: EngineClient): Promise<void> {
    const evt = await client.dispatch({ type: 'GET_SELECTION_AS_CLIPBOARD' });
    if (evt.type !== 'CLIPBOARD_PAYLOAD' || evt.plain === '') return;
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
