/* Phase 4 §10 (D4.10) — drag-and-drop document loading.
 *
 * Drop a .docx file anywhere on the editor to replace the open document. The
 * engine parses it via the existing `LoadDocx` command (format-docx). */
import type { EngineClient } from '../engine/engine-client';
import { topPos } from '../engine/types';

async function loadDroppedDocx(client: EngineClient, file: File): Promise<void> {
    const bytes = new Uint8Array(await file.arrayBuffer());
    const loaded = await client.dispatch({ type: 'LOAD_DOCX', bytes }, [
        bytes.buffer as ArrayBuffer,
    ]);
    if (loaded.type === 'ERROR') {
        console.error('[dnd] LoadDocx failed:', loaded.message);
        return;
    }
    /* Place the caret at the start of the freshly-loaded document, which also
       emits SelectionChanged so the overlays refresh. */
    await client.dispatch({
        type: 'SET_SELECTION',
        range: { start: topPos(0, 0), end: topPos(0, 0) },
        caret: topPos(0, 0),
    });
}

/**
 * Attach document-wide drag-drop. `dragover` must `preventDefault()` to make
 * the page a drop target; `drop` loads the first dropped `.docx`. Returns a
 * teardown that removes both listeners.
 */
export function attachDragDrop(client: EngineClient): () => void {
    const onDragOver = (e: DragEvent): void => {
        e.preventDefault();
        if (e.dataTransfer) e.dataTransfer.dropEffect = 'copy';
    };
    const onDrop = (e: DragEvent): void => {
        e.preventDefault();
        const file = e.dataTransfer?.files?.[0];
        if (file && file.name.toLowerCase().endsWith('.docx')) {
            void loadDroppedDocx(client, file);
        }
    };
    document.addEventListener('dragover', onDragOver);
    document.addEventListener('drop', onDrop);
    return () => {
        document.removeEventListener('dragover', onDragOver);
        document.removeEventListener('drop', onDrop);
    };
}
