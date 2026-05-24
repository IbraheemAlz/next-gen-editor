/* Phase 4 §11 — formatting toolbar.
 *
 * Reads engine state via the store (`attrsAtCaret`, `undoState`) and dispatches
 * `ApplyFormatting` / `Undo` / `Redo`. It holds no document state — the B/I/U
 * pressed state is whatever the engine last reported (§9 invariant).
 *
 * Phase 5 D5.4 adds the Export PDF button — it dispatches `ExportPdf` and turns
 * the returned `PdfExported` bytes into a browser download.
 *
 * Backlog sprint 1 added the AlignmentPicker; sprint 6 completes the rich-text
 * controls — strikethrough, a highlight (background) colour, and the
 * FontFamilyPicker (Backlog #1, #9). The B/I/U/S buttons also drive sticky
 * formatting (Backlog #11): clicking one with a collapsed caret arms a pending
 * style the engine applies to the next typed run. */
import { createSignal, For, Show } from 'solid-js';
import type { EngineClient } from '../engine/engine-client';
import type { Alignment, BlockPath, Color, TextAttrsPatch } from '../engine/types';
import type { EngineStore } from '../state/engine-store';

/* Insert-table picker geometry — Word's classic 8×8 grid. The user
   hovers to pick rows×cols and clicks to commit. */
const PICKER_MAX_ROWS = 8;
const PICKER_MAX_COLS = 8;

const FONT_SIZES = [12, 14, 16, 18, 20, 24, 32, 48, 64];
const DEFAULT_COLOR: Color = { r: 0, g: 0, b: 0, a: 255 };
/* Shown in the highlight picker when the run has no background colour. */
const NO_HIGHLIGHT: Color = { r: 255, g: 255, b: 255, a: 255 };
/* Font families loaded by `App.tsx`; `id` is the engine's loaded font id. */
const FONT_FAMILIES: ReadonlyArray<{ id: string; label: string }> = [
    { id: 'amiri', label: 'Amiri' },
    { id: 'liberation', label: 'Liberation Sans' },
    { id: 'noto-naskh', label: 'Noto Naskh Arabic' },
];

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
    downloadBlob(evt.bytes, 'application/pdf', 'document.pdf');
}

/* Phase 9 — Save as DOCX. Engine packs the current document into a
   freshly built `.docx` (regenerated `word/document.xml` + media + rels +
   Content_Types), the TS shell streams the resulting Uint8Array out as a
   file download. Word opens the file directly — no "unreadable content"
   recovery prompt — because the writer matches Word's element-order
   convention (CT_Inline children, run-property children, table-property
   children all schema-strict). */
async function exportDocx(client: EngineClient): Promise<void> {
    const evt = await client.dispatch({ type: 'SAVE_DOCX' });
    if (evt.type === 'ERROR') {
        console.error('[export] DOCX export failed:', evt.message);
        return;
    }
    if (evt.type !== 'DOCUMENT_SAVED') {
        console.error('[export] unexpected reply to SAVE_DOCX:', evt.type);
        return;
    }
    downloadBlob(
        evt.bytes,
        'application/vnd.openxmlformats-officedocument.wordprocessingml.document',
        'document.docx',
    );
}

/* Common Blob → anchor → click → revoke dance for engine-side artifacts. */
function downloadBlob(bytes: Uint8Array, mime: string, filename: string): void {
    /* Copy into a fresh ArrayBuffer-backed view: the worker's Uint8Array is
       typed over `ArrayBufferLike`, which a Blob part will not accept. */
    const blob = new Blob([new Uint8Array(bytes)], { type: mime });
    const url = URL.createObjectURL(blob);
    const link = document.createElement('a');
    link.href = url;
    link.download = filename;
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
    const strike = () => attrs()?.strike ?? false;
    const fontSize = () => attrs()?.font_size ?? 24;
    const color = () => attrs()?.color ?? DEFAULT_COLOR;
    const bgColor = () => attrs()?.bg_color ?? NO_HIGHLIGHT;
    const fontFamily = () => attrs()?.font_family ?? 'amiri';
    const undo = () => props.store.undoState();

    /* ApplyFormatting over a real selection styles it; over a collapsed caret
       the engine arms it as sticky/pending formatting (Backlog #11) — the
       next typed run adopts it. Either way the engine answers with
       SelectionChanged, so `attrsAtCaret` (and the B/I/U pressed state) stays
       correct without the toolbar tracking anything itself. */
    const apply = (patch: Partial<TextAttrsPatch>): void => {
        void props.client.dispatch({
            type: 'APPLY_FORMATTING',
            range: props.store.selection().range,
            attrs: { ...emptyPatch(), ...patch },
        });
    };

    /* Backlog #9 — paragraph alignment. Toolbar buttons are absolute
       (left/center/right/justify); the engine model is direction-relative
       (Start/End/Center/Justify). The mapping pivots on the document base
       direction — in an RTL document the leading edge is the right one, so
       "align left" is the logical `End`. */
    const align = (): Alignment => props.store.paragraphAlignment();
    const rtl = (): boolean => props.store.baseDirection() === 'Rtl';
    const leftAlign = (): Alignment => (rtl() ? 'End' : 'Start');
    const rightAlign = (): Alignment => (rtl() ? 'Start' : 'End');
    const setAlign = (a: Alignment): void => {
        void props.client.dispatch({
            type: 'SET_PARAGRAPH_ALIGN',
            range: props.store.selection().range,
            align: a,
        });
    };
    const alignButtons = (): { cls: string; label: string; value: Alignment }[] => [
        { cls: 'al-left', label: 'Align left', value: leftAlign() },
        { cls: 'al-center', label: 'Align center', value: 'Center' },
        { cls: 'al-right', label: 'Align right', value: rightAlign() },
        { cls: 'al-justify', label: 'Justify', value: 'Justify' },
    ];

    /* Phase 5 PR 3b — Insert Table popover + rows×cols picker.
       `pickerOpen` toggles visibility; `hoverR/hoverC` (1-indexed)
       drive the lit cells. Inserts the table at the block index
       immediately after the caret's top-level paragraph. PR 4:
       resolves the caret's full `BlockPath` and bumps the leading
       `Block` step by 1; nested-cell carets land outside the table
       (the `Cell` prefix is dropped) — Word's default. */
    const [pickerOpen, setPickerOpen] = createSignal(false);
    const [hoverR, setHoverR] = createSignal(0);
    const [hoverC, setHoverC] = createSignal(0);
    const insertTable = (rows: number, cols: number): void => {
        const caretPath = props.store.caretLogical()?.path;
        const topStep = caretPath?.steps.find((s) => s.kind === 'BLOCK');
        const idx = topStep && topStep.kind === 'BLOCK' ? topStep.idx + 1 : 0;
        const at: BlockPath = { steps: [{ kind: 'BLOCK', idx }] };
        void props.client.dispatch({ type: 'INSERT_TABLE', at, rows, cols });
        setPickerOpen(false);
        setHoverR(0);
        setHoverC(0);
    };
    const pickerCells = (): { r: number; c: number }[] => {
        const cells: { r: number; c: number }[] = [];
        for (let r = 1; r <= PICKER_MAX_ROWS; r++) {
            for (let c = 1; c <= PICKER_MAX_COLS; c++) cells.push({ r, c });
        }
        return cells;
    };

    /* `exporting` disables the button for the duration of the round-trip so a
       second click cannot start a concurrent export. One signal covers PDF and
       DOCX — both bounce through the worker, no point letting them race. */
    const [exporting, setExporting] = createSignal(false);
    const doExport = async (): Promise<void> => {
        setExporting(true);
        try {
            await exportPdf(props.client);
        } finally {
            setExporting(false);
        }
    };
    const doSaveDocx = async (): Promise<void> => {
        setExporting(true);
        try {
            await exportDocx(props.client);
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
            <button
                class="tb-btn tb-s"
                classList={{ active: strike() }}
                aria-pressed={strike()}
                aria-label="Strikethrough"
                onClick={() => apply({ strike: !strike() })}
            >
                S
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
            <label class="tb-field">
                Highlight
                <input
                    type="color"
                    value={colorToHex(bgColor())}
                    onChange={(e) => apply({ bg_color: hexToColor(e.currentTarget.value) })}
                />
            </label>
            <label class="tb-field">
                Font
                <select
                    value={fontFamily()}
                    onChange={(e) => apply({ font_family: e.currentTarget.value })}
                >
                    <For each={FONT_FAMILIES}>
                        {(f) => <option value={f.id}>{f.label}</option>}
                    </For>
                </select>
            </label>
            <span class="tb-sep" />
            <For each={alignButtons()}>
                {(b) => (
                    <button
                        class={`tb-btn tb-align ${b.cls}`}
                        classList={{ active: align() === b.value }}
                        aria-pressed={align() === b.value}
                        aria-label={b.label}
                        onClick={() => setAlign(b.value)}
                    >
                        <span class="al-bars">
                            <i />
                            <i />
                            <i />
                        </span>
                    </button>
                )}
            </For>
            <span class="tb-sep" />
            <div class="tb-table-wrap">
                <button
                    class="tb-btn"
                    aria-label="Insert table"
                    aria-haspopup="true"
                    aria-expanded={pickerOpen()}
                    onClick={() => setPickerOpen(!pickerOpen())}
                >
                    Table
                </button>
                <Show when={pickerOpen()}>
                    <div class="tb-table-pop" role="dialog" aria-label="Insert table picker">
                        <div
                            class="tb-table-grid"
                            onMouseLeave={() => {
                                setHoverR(0);
                                setHoverC(0);
                            }}
                        >
                            <For each={pickerCells()}>
                                {(cell) => (
                                    <div
                                        class="tb-table-cell"
                                        classList={{
                                            on: cell.r <= hoverR() && cell.c <= hoverC(),
                                        }}
                                        onMouseEnter={() => {
                                            setHoverR(cell.r);
                                            setHoverC(cell.c);
                                        }}
                                        onClick={() => insertTable(cell.r, cell.c)}
                                    />
                                )}
                            </For>
                        </div>
                        <div class="tb-table-label">
                            {hoverR() && hoverC()
                                ? `${hoverR()} × ${hoverC()}`
                                : 'Hover to pick size'}
                        </div>
                    </div>
                </Show>
            </div>
            <span class="tb-spacer" />
            <button
                class="tb-btn tb-export"
                onClick={() => void doSaveDocx()}
                disabled={exporting()}
                aria-label="Save as DOCX"
            >
                {exporting() ? 'Saving…' : 'Save as DOCX'}
            </button>
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
