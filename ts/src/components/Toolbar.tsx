/* Phase 4 §11 — formatting toolbar.
 *
 * Reads engine state via the store (`attrsAtCaret`, `undoState`) and dispatches
 * `ApplyFormatting` / `Undo` / `Redo`. It holds no document state — the B/I/U
 * pressed state is whatever the engine last reported (§9 invariant).
 *
 * Phase 5 D5.4 adds the Export PDF button — it dispatches `ExportPdf` and turns
 * the returned `PdfExported` bytes into a browser download.
 *
 * Alignment + font-family pickers from §11 are deferred — neither has engine
 * support yet (see BACKLOG.md). */
import { createSignal, For } from 'solid-js';
import type { EngineClient } from '../engine/engine-client';
import type { Color, TextAttrsPatch } from '../engine/types';
import type { EngineStore } from '../state/engine-store';

const FONT_SIZES = [12, 14, 16, 18, 20, 24, 32, 48, 64];
const DEFAULT_COLOR: Color = { r: 0, g: 0, b: 0, a: 255 };

/** A `TextAttrsPatch` with every field unset — the base for a sparse patch. */
function emptyPatch(): TextAttrsPatch {
    return {
        bold: undefined,
        italic: undefined,
        underline: undefined,
        strike: undefined,
        font_family: undefined,
        font_size: undefined,
        color: undefined,
        bg_color: undefined,
        script: undefined,
        language: undefined,
    };
}

function colorToHex(c: Color): string {
    const h = (n: number) => n.toString(16).padStart(2, '0');
    return `#${h(c.r)}${h(c.g)}${h(c.b)}`;
}

function hexToColor(hex: string): Color {
    return {
        r: parseInt(hex.slice(1, 3), 16),
        g: parseInt(hex.slice(3, 5), 16),
        b: parseInt(hex.slice(5, 7), 16),
        a: 255,
    };
}

/* D5.4 — export the current document and hand the bytes to the browser as a
   download. The engine runs in the worker, so the only main-thread work is
   wrapping the returned `Uint8Array` in a Blob and clicking a synthetic <a>. */
async function exportPdf(client: EngineClient): Promise<void> {
    const evt = await client.dispatch({ type: 'EXPORT_PDF', conformance: 'A1b' });
    if (evt.type === 'ERROR') {
        console.error('[export] PDF export failed:', evt.message);
        return;
    }
    if (evt.type !== 'PDF_EXPORTED') {
        console.error('[export] unexpected reply to EXPORT_PDF:', evt.type);
        return;
    }
    /* Copy into a fresh ArrayBuffer-backed view: the worker's Uint8Array is
       typed over `ArrayBufferLike`, which a Blob part will not accept. */
    const blob = new Blob([new Uint8Array(evt.bytes)], { type: 'application/pdf' });
    const url = URL.createObjectURL(blob);
    const link = document.createElement('a');
    link.href = url;
    link.download = 'document.pdf';
    document.body.appendChild(link);
    link.click();
    link.remove();
    /* Revoke on the next tick so the navigation that the click started has
       already taken the blob. */
    setTimeout(() => URL.revokeObjectURL(url), 0);
}

export function Toolbar(props: { client: EngineClient; store: EngineStore }) {
    const attrs = () => props.store.attrsAtCaret();
    const bold = () => attrs()?.bold ?? false;
    const italic = () => attrs()?.italic ?? false;
    const underline = () => (attrs()?.underline ?? 'None') !== 'None';
    const fontSize = () => attrs()?.font_size ?? 24;
    const color = () => attrs()?.color ?? DEFAULT_COLOR;
    const undo = () => props.store.undoState();

    /* ApplyFormatting acts on the current selection; a collapsed caret is a
       no-op in the engine (nothing to format). */
    const apply = (patch: Partial<TextAttrsPatch>): void => {
        void props.client.dispatch({
            type: 'APPLY_FORMATTING',
            range: props.store.selection().range,
            attrs: { ...emptyPatch(), ...patch },
        });
    };

    /* `exporting` disables the button for the duration of the round-trip so a
       second click cannot start a concurrent export. */
    const [exporting, setExporting] = createSignal(false);
    const doExport = async (): Promise<void> => {
        setExporting(true);
        try {
            await exportPdf(props.client);
        } finally {
            setExporting(false);
        }
    };

    return (
        <div class="toolbar" role="toolbar" aria-label="Formatting">
            <button
                class="tb-btn"
                onClick={() => void props.client.dispatch({ type: 'UNDO' })}
                disabled={!undo().canUndo}
                aria-label="Undo"
            >
                ↶
            </button>
            <button
                class="tb-btn"
                onClick={() => void props.client.dispatch({ type: 'REDO' })}
                disabled={!undo().canRedo}
                aria-label="Redo"
            >
                ↷
            </button>
            <span class="tb-sep" />
            <button
                class="tb-btn tb-b"
                classList={{ active: bold() }}
                aria-pressed={bold()}
                aria-label="Bold"
                onClick={() => apply({ bold: !bold() })}
            >
                B
            </button>
            <button
                class="tb-btn tb-i"
                classList={{ active: italic() }}
                aria-pressed={italic()}
                aria-label="Italic"
                onClick={() => apply({ italic: !italic() })}
            >
                I
            </button>
            <button
                class="tb-btn tb-u"
                classList={{ active: underline() }}
                aria-pressed={underline()}
                aria-label="Underline"
                onClick={() => apply({ underline: underline() ? 'None' : 'Single' })}
            >
                U
            </button>
            <span class="tb-sep" />
            <label class="tb-field">
                Size
                <select
                    value={String(fontSize())}
                    onChange={(e) => apply({ font_size: Number(e.currentTarget.value) })}
                >
                    <For each={FONT_SIZES}>{(s) => <option value={String(s)}>{s}</option>}</For>
                </select>
            </label>
            <label class="tb-field">
                Color
                <input
                    type="color"
                    value={colorToHex(color())}
                    onChange={(e) => apply({ color: hexToColor(e.currentTarget.value) })}
                />
            </label>
            <span class="tb-spacer" />
            <button
                class="tb-btn tb-export"
                onClick={() => void doExport()}
                disabled={exporting()}
                aria-label="Export as PDF/A-1b"
            >
                {exporting() ? 'Exporting…' : 'Export PDF'}
            </button>
        </div>
    );
}
