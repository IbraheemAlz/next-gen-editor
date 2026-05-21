/* Phase 4 §12 — clipboard.
 *
 * Plain-text round-trip over the async `navigator.clipboard` API. Copy
 * snapshots the engine's selection; paste inserts at the engine's caret. The
 * engine owns the document, so these only move text across the boundary.
 * Rich (HTML / .docx) clipboard payloads are deferred — see BACKLOG.md. */
import type { EngineClient } from '../engine/engine-client';

/** Copy the current selection's plain text to the system clipboard. */
export async function copy(client: EngineClient): Promise<void> {
    const evt = await client.dispatch({ type: 'GET_SELECTION_AS_CLIPBOARD' });
    if (evt.type !== 'CLIPBOARD_PAYLOAD' || evt.plain === '') return;
    await navigator.clipboard.writeText(evt.plain);
}

/** Copy the selection to the clipboard, then delete it from the document. */
export async function cut(client: EngineClient): Promise<void> {
    const evt = await client.dispatch({ type: 'GET_SELECTION_AS_CLIPBOARD' });
    if (evt.type !== 'CLIPBOARD_PAYLOAD' || evt.plain === '') return;
    await navigator.clipboard.writeText(evt.plain);
    await client.dispatch({ type: 'DELETE_AT_CARET', forward: false, by_word: false });
}

/** Paste the system clipboard's plain text at the caret. */
export async function paste(client: EngineClient): Promise<void> {
    const text = await navigator.clipboard.readText();
    if (text) await client.dispatch({ type: 'PASTE_PLAIN', text });
}
