/**
 * createEditorCommands — headless Solid primitive that returns a typed
 * facade over Engine.dispatch(). Components call e.g.
 * `cmd.setZoom(1.25)` instead of building Command JSON by hand.
 *
 * Every method here corresponds 1:1 to a variant of `Command` in
 * `crates/bridge/src/command.rs` — the generated TS shapes live in
 * `crates/engine-wasm/pkg/engine_wasm.d.ts`. Add new methods here when
 * the bridge grows; do not let downstream UI assemble raw Command objects.
 *
 * tsify-next renders `Option<T>` as `T | undefined`. With
 * `exactOptionalPropertyTypes: true` every field must be set explicitly,
 * so `emptyPatch()` provides the canonical zero state for `TextAttrsPatch`.
 *
 * Formatting helpers (setBold, setUnderline, …) omit `range` by default so
 * the ENGINE binds the patch to its own live selection — the UI mirror is
 * async and can be stale while a mutation is in flight. Paragraph / caret
 * helpers still resolve from an internal `createEditorState` subscription.
 * Pass an explicit `range` to override.
 */
import { useEngine, type EngineHandle } from './EngineProvider';
import { createEditorState, type EditorState } from './createEditorState';
import type {
    Command,
    Event,
    DocFormat,
    TextAttrsPatch,
    UnderlineStyle,
    VerticalScript,
    Alignment,
    Direction,
    PdfConformance,
    ImageBlob,
    ImageFit,
    MoveDirection,
    LogicalPos,
    LogicalRange,
    Rect,
    BlockPath,
    Color,
    BridgeCellBorders,
    BridgeTabStop,
    InsertSide,
    PageOrientation,
    ListKind,
    BridgeStyleProperties,
} from './types';

/** Sprint 12 (#11) — paragraph style id. The engine now models real
 *  `<w:styles>` entries on `DocumentTree.styles`; `cmd.applyStyle`
 *  dispatches `Command::ApplyStyle { style_id }` and the engine
 *  cascades the style's `<w:pPr>` into the resolved paragraph
 *  properties. `string` because the live set comes from the loaded
 *  document's stylesheet — not a closed UI enum. */
export type ParagraphStyleId = string;

/** Sprint 5 → 12 — stock preset values used as a FALLBACK only.
 *  When the user picks a heading-style id that the loaded document
 *  doesn't ship (a brand-new file with no `styles.xml` shipped
 *  customizations), the UI degrades to a direct-formatting patch via
 *  the legacy `cmd.applyAttrs` path so the user still sees a visible
 *  change. The real engine path is `Command::ApplyStyle`. */
export const STYLE_PRESETS: Record<string, Partial<TextAttrsPatch>> = {
    Normal: { bold: false, italic: false, font_size: 12 },
    Title: { bold: true, italic: false, font_size: 32 },
    Heading1: { bold: true, italic: false, font_size: 24 },
    Heading2: { bold: true, italic: false, font_size: 20 },
    Heading3: { bold: true, italic: false, font_size: 16 },
};

/** Canonical empty `TextAttrsPatch` — every field set to `undefined`. */
export function emptyPatch(): TextAttrsPatch {
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
        caps: undefined,
        small_caps: undefined,
    };
}

export interface EditorCommands {
    /* Lifecycle */
    requestStats(): Promise<Event>;
    requestPaint(viewport: Rect, dirty?: Rect): Promise<Event>;

    /* Viewport */
    setZoom(scale: number): Promise<Event>;
    /** Post-boot devicePixelRatio change — updates the device scale the
     *  engine composes with the user zoom (`scale = device × zoom`). */
    setDeviceScale(scale: number): Promise<Event>;
    setViewport(rect: Rect): Promise<Event>;
    expandLayout(targetY: number): Promise<Event>;

    /* History */
    undo(): Promise<Event>;
    redo(): Promise<Event>;

    /* Text mutation */
    insertText(text: string, at?: LogicalPos): Promise<Event>;
    deleteRange(range: LogicalRange): Promise<Event>;
    replaceRange(range: LogicalRange, text: string): Promise<Event>;
    deleteAtCaret(forward: boolean, byWord?: boolean): Promise<Event>;
    splitParagraph(at: LogicalPos): Promise<Event>;
    /**
     * Insert a soft line break (Shift+Enter) at `at` (defaults to the
     * current caret). A soft break wraps to the next line WITHOUT
     * splitting the paragraph block — it inserts a U+2028 LINE SEPARATOR
     * into the existing text run, so the layout engine forces a new line
     * there while keeping one paragraph. The first-line indent applies to
     * the paragraph's absolute first line only, so the soft-broken
     * continuation hugs the left indent (Word / Google Docs behaviour).
     * Contrast `splitParagraph`, which creates a new paragraph block.
     */
    insertSoftBreak(at?: LogicalPos): Promise<Event>;

    /* Selection */
    selectAll(): Promise<Event>;
    setSelection(range: LogicalRange, caret: LogicalPos): Promise<Event>;
    extendSelection(to: LogicalPos): Promise<Event>;
    moveCaret(direction: MoveDirection, extend?: boolean): Promise<Event>;

    /* Formatting — `range` defaults to the engine-owned live selection. */
    applyAttrs(attrs: Partial<TextAttrsPatch>, range?: LogicalRange): Promise<Event>;
    setBold(value: boolean, range?: LogicalRange): Promise<Event>;
    setItalic(value: boolean, range?: LogicalRange): Promise<Event>;
    setStrike(value: boolean, range?: LogicalRange): Promise<Event>;
    setUnderline(style: UnderlineStyle, range?: LogicalRange): Promise<Event>;
    setVerticalScript(script: VerticalScript, range?: LogicalRange): Promise<Event>;
    setFontFamily(family: string, range?: LogicalRange): Promise<Event>;
    setFontSize(pt: number, range?: LogicalRange): Promise<Event>;
    setColor(r: number, g: number, b: number, a?: number, range?: LogicalRange): Promise<Event>;
    setHighlight(r: number, g: number, b: number, a?: number, range?: LogicalRange): Promise<Event>;
    /** `<w:caps/>` — render every glyph uppercase. */
    setCaps(value: boolean, range?: LogicalRange): Promise<Event>;
    /** `<w:smallCaps/>` — render lowercase as smaller uppercase glyphs. */
    setSmallCaps(value: boolean, range?: LogicalRange): Promise<Event>;

    /* Paragraph — `range` defaults to current selection. */
    setParagraphAlign(align: Alignment, range?: LogicalRange): Promise<Event>;
    setParagraphDirection(direction: Direction, range?: LogicalRange): Promise<Event>;

    /* Images */
    insertImage(image: ImageBlob, at: LogicalPos, fit?: ImageFit): Promise<Event>;
    insertImageAtCaret(image: ImageBlob, fit?: ImageFit): Promise<Event>;

    /* Tables — every method maps 1:1 onto a `Command` variant in
     * `crates/bridge/src/command.rs`. */
    insertTable(at: BlockPath, rows: number, cols: number): Promise<Event>;
    insertTableAtCaret(rows: number, cols: number): Promise<Event>;
    deleteTable(path: BlockPath): Promise<Event>;
    /** Insert a row at `row` on the named `side` (Before | After).
     *  Sprint 2 (UI Edition) hotfix: the previous `(tablePath,
     *  afterRow)` shape forced callers into `row - 1` math that
     *  underflows `u32` on the wire when `row === 0`. */
    insertRow(tablePath: BlockPath, row: number, side: InsertSide): Promise<Event>;
    deleteRow(tablePath: BlockPath, row: number): Promise<Event>;
    /** Insert a column at `col` on the named `side` — same hotfix
     *  rationale as [`insertRow`]. */
    insertColumn(tablePath: BlockPath, col: number, side: InsertSide): Promise<Event>;
    deleteColumn(tablePath: BlockPath, col: number): Promise<Event>;
    mergeCells(
        tablePath: BlockPath,
        fromRow: number,
        fromCol: number,
        toRow: number,
        toCol: number,
    ): Promise<Event>;
    splitCell(tablePath: BlockPath, row: number, col: number): Promise<Event>;
    setCellShading(
        tablePath: BlockPath,
        row: number,
        col: number,
        color: Color | undefined,
    ): Promise<Event>;
    setCellBorders(
        tablePath: BlockPath,
        row: number,
        col: number,
        borders: BridgeCellBorders,
    ): Promise<Event>;

    /* Layout authoring (Sprint 2 UI Edition — shipped with the
     * matching Rust bridge additions in `crates/bridge/src/command.rs`
     * + handlers in `crates/engine-wasm/src/lib.rs`). */
    setColumns(at: LogicalPos, count: number, gutterPt: number): Promise<Event>;
    setColumnsAtCaret(count: number, gutterPt?: number): Promise<Event>;
    insertPageBreak(at?: LogicalPos): Promise<Event>;
    setParagraphBorders(
        borders: BridgeCellBorders,
        range?: LogicalRange,
    ): Promise<Event>;
    clearParagraphBorders(range?: LogicalRange): Promise<Event>;

    /* Lists (Sprint 5 UI Edition). `Off` clears `list_item` on every
     * paragraph the range spans; `Bullet` / `Number` run the engine's
     * idempotent `numbering.xml` synthesis (Sprint 13 #12). */
    toggleList(kind: ListKind, range?: LogicalRange): Promise<Event>;

    /* Paragraph formatting (Sprint 6 UI Edition). Units are points. */
    setParagraphIndent(
        startPt: number,
        endPt: number,
        firstLinePt: number,
        range?: LogicalRange,
    ): Promise<Event>;
    increaseIndent(stepPt?: number, range?: LogicalRange): Promise<Event>;
    decreaseIndent(stepPt?: number, range?: LogicalRange): Promise<Event>;
    setLineSpacing(multiplier: number, range?: LogicalRange): Promise<Event>;
    setParagraphShading(color: Color | undefined, range?: LogicalRange): Promise<Event>;
    /**
     * Sprint 11 (#13) — replace `<w:pPr><w:tabs>` on every paragraph
     * the range spans. Dispatch ONCE on `pointerup` after a Ruler
     * drag — never on `pointermove` — so one user gesture produces
     * exactly one undo entry.
     */
    setTabStops(stops: BridgeTabStop[], range?: LogicalRange): Promise<Event>;

    /* Review (Sprint 7 UI Edition). `toggleTrackChanges` flips the
     * engine's recording flag (Sprint 14 #14); the live state rides
     * back on `SELECTION_CHANGED.is_tracking_changes`. */
    toggleTrackChanges(enabled: boolean): Promise<Event>;
    /**
     * Sprint 14 (#14) — stamp `author` + `date` on every subsequent
     * tracked revision. Empty `date` leaves the engine's stored
     * date untouched so the worker can stamp `Date.now()` per
     * mutation; empty `author` keeps the prior value (default
     * `"You"`).
     */
    setReviewIdentity(author: string, date: string): Promise<Event>;
    acceptRevision(block: number, start: number, end: number): Promise<Event>;
    rejectRevision(block: number, start: number, end: number): Promise<Event>;
    insertComment(
        text: string,
        author: string,
        range?: LogicalRange,
    ): Promise<Event>;
    deleteComment(id: number): Promise<Event>;
    resolveComment(id: number, resolved: boolean): Promise<Event>;
    /**
     * Issue #27 — append a threaded reply to the comment with
     * `parentId`. The engine anchors the reply on the parent's range
     * and mints the new sequential id; an unknown parent returns
     * `Event::Error`. Deleting the parent cascades over its replies.
     */
    replyToComment(
        parentId: number,
        text: string,
        author: string,
    ): Promise<Event>;

    /* Styles (Sprint 12 #11). Dispatches `APPLY_STYLE` — the engine
     * sets `<w:pStyle>` and cascades the style's `<w:pPr>`. The
     * `STYLE_PRESETS` table remains a UI-side fallback for documents
     * that don't ship the picked style id. */
    applyStyle(styleId: ParagraphStyleId, range?: LogicalRange): Promise<Event>;
    /** Issue #21 — mutate an existing style DEFINITION (assignment is
     *  `applyStyle`); the engine re-cascades every dependent paragraph
     *  and regenerates styles.xml on the next save. */
    modifyStyle(styleId: ParagraphStyleId, properties: BridgeStyleProperties): Promise<Event>;

    /* Page setup (Sprint 4 UI Edition). All margin units are points. */
    setPageMargins(
        at: LogicalPos,
        topPt: number,
        rightPt: number,
        bottomPt: number,
        leftPt: number,
    ): Promise<Event>;
    setPageMarginsAtCaret(
        topPt: number,
        rightPt: number,
        bottomPt: number,
        leftPt: number,
    ): Promise<Event>;
    setPageOrientation(at: LogicalPos, orientation: PageOrientation): Promise<Event>;
    setPageOrientationAtCaret(orientation: PageOrientation): Promise<Event>;

    /* I/O */
    /** Open a `.docx` byte buffer (passed zero-copy as a Transferable).
     *  Engine ships only `Docx`; HTML / PlainText return error events. */
    openDocument(bytes: Uint8Array, name?: string): Promise<Event>;
    saveDocument(format: DocFormat): Promise<Event>;
    saveDocx(): Promise<Event>;
    exportPdf(conformance: PdfConformance): Promise<Event>;
    /** Engine-pending — dispatches but engine returns
     *  `Event::Error` until a Core Engine HTML serializer ships. */
    exportHtml(): Promise<Event>;
    /** Engine-pending — same status as [`exportHtml`]. */
    exportPlainText(): Promise<Event>;
    closeDocument(): Promise<Event>;

    /* Clipboard */
    getSelectionAsClipboard(): Promise<Event>;
    pastePlain(text: string): Promise<Event>;
    pasteHtml(html: string): Promise<Event>;

    /* Escape hatch — for commands not yet covered above. */
    raw(cmd: Command, transfer?: Transferable[]): Promise<Event>;
}

function build(engine: EngineHandle, state: EditorState): EditorCommands {
    const dispatch = (cmd: Command, transfer: Transferable[] = []) =>
        engine.dispatch(cmd, transfer);

    /* Selection helpers — throw clearly if no selection exists yet, so
     * callers see a programming error rather than a silent no-op. */
    const currentRange = (override?: LogicalRange): LogicalRange => {
        if (override) return override;
        const sel = state.selection();
        if (!sel) {
            throw new Error(
                '[@nge/core] formatting command requires a range; no current selection (engine has not emitted SELECTION_CHANGED yet).',
            );
        }
        return sel;
    };
    const currentCaret = (): LogicalPos => {
        const sel = state.selection();
        if (!sel) {
            throw new Error(
                '[@nge/core] insert-at-caret requires a caret; no current selection yet.',
            );
        }
        return sel.end;
    };

    /* `undefined` range (never `null` — tsify renders `Option<T>` as
     * `T | undefined`) makes the ENGINE resolve its authoritative live
     * selection, so a stale UI mirror can never misbind formatting. The
     * cast bridges the wasm-pack d.ts until it regenerates with
     * `range: LogicalRange | undefined`. */
    const fmt = (patch: Partial<TextAttrsPatch>, range?: LogicalRange) =>
        dispatch({
            type: 'APPLY_FORMATTING',
            range: range as LogicalRange,
            attrs: { ...emptyPatch(), ...patch },
        });

    return {
        requestStats: () => dispatch({ type: 'REQUEST_STATS' }),
        requestPaint: (viewport, dirty) =>
            dispatch({ type: 'REQUEST_PAINT', viewport, dirty }),

        setZoom: (scale) => dispatch({ type: 'SET_ZOOM', scale }),
        setDeviceScale: (scale) => dispatch({ type: 'SET_DEVICE_SCALE', scale }),
        setViewport: (rect) => dispatch({ type: 'SET_VIEWPORT', rect }),
        expandLayout: (target_y) => dispatch({ type: 'EXPAND_LAYOUT', target_y }),

        undo: () => dispatch({ type: 'UNDO' }),
        redo: () => dispatch({ type: 'REDO' }),

        insertText: (text, at) => dispatch({ type: 'INSERT_TEXT', at, text }),
        deleteRange: (range) => dispatch({ type: 'DELETE_RANGE', range }),
        replaceRange: (range, text) => dispatch({ type: 'REPLACE_RANGE', range, text }),
        deleteAtCaret: (forward, by_word = false) =>
            dispatch({ type: 'DELETE_AT_CARET', forward, by_word }),
        splitParagraph: (at) => dispatch({ type: 'SPLIT_PARAGRAPH', at }),
        /* U+2028 LINE SEPARATOR — the engine's `insert_text` splices it into
         * the paragraph's text run (no block split), and the layout engine
         * forces a line break on it (see `paragraph.rs` `soft_break_segments`). */
        insertSoftBreak: (at) =>
            dispatch({ type: 'INSERT_TEXT', at: at ?? currentCaret(), text: '\u2028' }),

        selectAll: () => dispatch({ type: 'SELECT_ALL' }),
        setSelection: (range, caret) => dispatch({ type: 'SET_SELECTION', range, caret }),
        extendSelection: (to) => dispatch({ type: 'EXTEND_SELECTION', to, modifier: 'None' }),
        moveCaret: (direction, extend = false) =>
            dispatch({ type: 'MOVE_CARET', direction, extend }),

        applyAttrs: (patch, range) => fmt(patch, range),
        setBold: (value, range) => fmt({ bold: value }, range),
        setItalic: (value, range) => fmt({ italic: value }, range),
        setStrike: (value, range) => fmt({ strike: value }, range),
        setUnderline: (style, range) => fmt({ underline: style }, range),
        /* TextAttrsPatch.script holds a VerticalScript on the wire — the
         * field is named `script` but it carries Super/Sub/Normal. The
         * Sprint 1 (UI Edition) Super/Sub buttons hit this path with no
         * bridge change required. */
        setVerticalScript: (script, range) => fmt({ script }, range),
        setFontFamily: (font_family, range) => fmt({ font_family }, range),
        setFontSize: (font_size, range) => fmt({ font_size }, range),
        setColor: (r, g, b, a = 255, range) => fmt({ color: { r, g, b, a } }, range),
        setHighlight: (r, g, b, a = 255, range) =>
            fmt({ bg_color: { r, g, b, a } }, range),
        setCaps: (value, range) => fmt({ caps: value }, range),
        setSmallCaps: (value, range) => fmt({ small_caps: value }, range),

        setParagraphAlign: (align, range) =>
            dispatch({
                type: 'SET_PARAGRAPH_ALIGN',
                range: currentRange(range),
                align,
            }),
        setParagraphDirection: (direction, range) =>
            dispatch({
                type: 'SET_PARAGRAPH_DIRECTION',
                range: currentRange(range),
                direction,
            }),

        insertImage: (image, at, fit = 'FitWidth') =>
            dispatch({ type: 'INSERT_IMAGE', at, image, fit }, [
                image.bytes.buffer as ArrayBuffer,
            ]),
        insertImageAtCaret: (image, fit = 'FitWidth') =>
            dispatch({ type: 'INSERT_IMAGE', at: currentCaret(), image, fit }, [
                image.bytes.buffer as ArrayBuffer,
            ]),

        insertTable: (at, rows, cols) =>
            dispatch({ type: 'INSERT_TABLE', at, rows, cols }),
        insertTableAtCaret: (rows, cols) => {
            /* Resolve the top-level Block step from the caret and insert
             * the table immediately after that block — matches the
             * existing Toolbar.tsx behaviour and keeps nested-cell carets
             * landing outside the table. */
            const sel = state.selection();
            if (!sel) {
                throw new Error(
                    '[@nge/core] insertTableAtCaret requires a caret; no SELECTION_CHANGED yet.',
                );
            }
            const topStep = sel.end.path.steps.find((s) => s.kind === 'BLOCK');
            const idx =
                topStep && topStep.kind === 'BLOCK' ? topStep.idx + 1 : 0;
            return dispatch({
                type: 'INSERT_TABLE',
                at: { steps: [{ kind: 'BLOCK', idx }] },
                rows,
                cols,
            });
        },
        deleteTable: (path) => dispatch({ type: 'DELETE_TABLE', path }),
        insertRow: (table_path, row, side) =>
            dispatch({ type: 'INSERT_ROW', table_path, row, side }),
        deleteRow: (table_path, row) =>
            dispatch({ type: 'DELETE_ROW', table_path, row }),
        insertColumn: (table_path, col, side) =>
            dispatch({ type: 'INSERT_COLUMN', table_path, col, side }),
        deleteColumn: (table_path, col) =>
            dispatch({ type: 'DELETE_COLUMN', table_path, col }),
        mergeCells: (table_path, from_row, from_col, to_row, to_col) =>
            dispatch({
                type: 'MERGE_CELLS',
                table_path,
                from_row,
                from_col,
                to_row,
                to_col,
            }),
        splitCell: (table_path, row, col) =>
            dispatch({ type: 'SPLIT_CELL', table_path, row, col }),
        setCellShading: (table_path, row, col, color) =>
            dispatch({
                type: 'SET_CELL_SHADING',
                table_path,
                row,
                col,
                color,
            }),
        setCellBorders: (table_path, row, col, borders) =>
            dispatch({
                type: 'SET_CELL_BORDERS',
                table_path,
                row,
                col,
                borders,
            }),

        setColumns: (at, count, gutter_pt) =>
            dispatch({ type: 'SET_COLUMNS', at, count, gutter_pt }),
        setColumnsAtCaret: (count, gutter_pt = 36) =>
            dispatch({
                type: 'SET_COLUMNS',
                at: currentCaret(),
                count,
                gutter_pt,
            }),
        insertPageBreak: (at) =>
            dispatch({ type: 'INSERT_PAGE_BREAK', at: at ?? currentCaret() }),
        setParagraphBorders: (borders, range) =>
            dispatch({
                type: 'SET_PARAGRAPH_BORDERS',
                range: currentRange(range),
                borders,
            }),
        toggleList: (kind, range) =>
            dispatch({
                type: 'TOGGLE_LIST',
                range: currentRange(range),
                kind,
            }),

        setParagraphIndent: (start_pt, end_pt, first_line_pt, range) =>
            dispatch({
                type: 'SET_PARAGRAPH_INDENT',
                range: currentRange(range),
                start_pt,
                end_pt,
                first_line_pt,
            }),
        /* Indent +/- buttons step `start_pt` (the logical leading-edge
         * indent — `<w:start>`) from the live read-back
         * (`SELECTION_CHANGED.paragraph_indent`), clamped ≥ 0, and pass
         * the other two `<w:ind>` fields through unchanged — the engine
         * takes the whole block atomically (mirrors Ruler.tsx's
         * commitDrag). */
        increaseIndent: (step_pt = 36, range) => {
            const ind = state.paragraphIndent();
            return dispatch({
                type: 'SET_PARAGRAPH_INDENT',
                range: currentRange(range),
                start_pt: Math.max(0, ind.start_pt + step_pt),
                end_pt: ind.end_pt,
                first_line_pt: ind.first_line_pt,
            });
        },
        decreaseIndent: (step_pt = 36, range) => {
            const ind = state.paragraphIndent();
            return dispatch({
                type: 'SET_PARAGRAPH_INDENT',
                range: currentRange(range),
                start_pt: Math.max(0, ind.start_pt - step_pt),
                end_pt: ind.end_pt,
                first_line_pt: ind.first_line_pt,
            });
        },
        setLineSpacing: (multiplier, range) =>
            dispatch({
                type: 'SET_LINE_SPACING',
                range: currentRange(range),
                multiplier,
            }),
        setParagraphShading: (color, range) =>
            dispatch({
                type: 'SET_PARAGRAPH_SHADING',
                range: currentRange(range),
                color,
            }),
        setTabStops: (stops, range) =>
            dispatch({
                type: 'SET_TAB_STOPS',
                range: currentRange(range),
                stops,
            }),

        toggleTrackChanges: (enabled) =>
            dispatch({ type: 'TOGGLE_TRACK_CHANGES', enabled }),
        setReviewIdentity: (author, date) =>
            dispatch({ type: 'SET_REVIEW_IDENTITY', author, date }),
        acceptRevision: (block, start, end) =>
            dispatch({ type: 'ACCEPT_REVISION', block, start, end }),
        rejectRevision: (block, start, end) =>
            dispatch({ type: 'REJECT_REVISION', block, start, end }),
        insertComment: (text, author, range) =>
            dispatch({
                type: 'INSERT_COMMENT',
                range: currentRange(range),
                text,
                author,
            }),
        deleteComment: (id) => dispatch({ type: 'DELETE_COMMENT', id }),
        resolveComment: (id, resolved) =>
            dispatch({ type: 'RESOLVE_COMMENT', id, resolved }),
        replyToComment: (parentId, text, author) =>
            dispatch({
                type: 'REPLY_TO_COMMENT',
                parent_id: parentId,
                text,
                author,
            }),
        applyStyle: (styleId, range) =>
            dispatch({
                type: 'APPLY_STYLE',
                range: currentRange(range),
                style_id: styleId,
            }),
        modifyStyle: (styleId, properties) =>
            dispatch({ type: 'MODIFY_STYLE', style_id: styleId, properties }),

        setPageMargins: (at, top_pt, right_pt, bottom_pt, left_pt) =>
            dispatch({
                type: 'SET_PAGE_MARGINS',
                at,
                top_pt,
                right_pt,
                bottom_pt,
                left_pt,
            }),
        setPageMarginsAtCaret: (top_pt, right_pt, bottom_pt, left_pt) =>
            dispatch({
                type: 'SET_PAGE_MARGINS',
                at: currentCaret(),
                top_pt,
                right_pt,
                bottom_pt,
                left_pt,
            }),
        setPageOrientation: (at, orientation) =>
            dispatch({ type: 'SET_PAGE_ORIENTATION', at, orientation }),
        setPageOrientationAtCaret: (orientation) =>
            dispatch({
                type: 'SET_PAGE_ORIENTATION',
                at: currentCaret(),
                orientation,
            }),

        clearParagraphBorders: (range) =>
            dispatch({
                type: 'SET_PARAGRAPH_BORDERS',
                range: currentRange(range),
                borders: {
                    top: undefined,
                    left: undefined,
                    bottom: undefined,
                    right: undefined,
                },
            }),

        openDocument: (bytes, name) =>
            dispatch(
                {
                    type: 'OPEN_DOCUMENT',
                    bytes,
                    format: 'docx',
                    name: name ?? undefined,
                },
                [bytes.buffer as ArrayBuffer],
            ),
        saveDocument: (format) => dispatch({ type: 'SAVE_DOCUMENT', format }),
        saveDocx: () => dispatch({ type: 'SAVE_DOCX' }),
        exportPdf: (conformance) => dispatch({ type: 'EXPORT_PDF', conformance }),
        exportHtml: () => dispatch({ type: 'SAVE_DOCUMENT', format: 'html' }),
        exportPlainText: () =>
            dispatch({ type: 'SAVE_DOCUMENT', format: 'plain_text' }),
        closeDocument: () => dispatch({ type: 'CLOSE_DOCUMENT' }),

        getSelectionAsClipboard: () => dispatch({ type: 'GET_SELECTION_AS_CLIPBOARD' }),
        pastePlain: (text) => dispatch({ type: 'PASTE_PLAIN', text }),
        pasteHtml: (html) => dispatch({ type: 'PASTE_HTML', html }),

        raw: (cmd, transfer = []) => dispatch(cmd, transfer),
    };
}

/**
 * Returns the typed command facade bound to the engine + state supplied
 * via context. Internally instantiates a `createEditorState` so the
 * formatting / caret helpers can auto-resolve `range` from the current
 * selection. Components that also need to read state should call
 * `createEditorState()` themselves — subscriptions are cheap.
 */
export function createEditorCommands(): EditorCommands {
    const engine = useEngine();
    const state = createEditorState();
    return build(engine, state);
}
