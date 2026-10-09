/**
 * Issue #338 / #342 — which `@nge/core` facade member exposes each engine
 * command, checked by `tsc` against the generated command metadata
 * (`commandMeta.generated.ts`, from `crates/bridge/src/meta.rs`).
 *
 * `closeDocument()` used to dispatch `CLOSE_DOCUMENT` into a `phase3_stub`
 * — a promise that always resolved to `Event::Error`, with no badge and no
 * way to tell it was unimplemented: Phantom UI at the SDK level. These two
 * maps make that state unrepresentable:
 *
 * - {@link COMMAND_FACADE} classifies EVERY `Command['type']` (a mapped
 *   type over the tsify union, so a new command does not compile until it
 *   is classified). A stub command (`StubCommandType`) can only be
 *   `'stub'`; a live command can only name a facade member or
 *   `'internal'` — never `'stub'`.
 * - {@link FACADE_METHOD_COMMANDS} lists, for EVERY facade member (a
 *   mapped type over `keyof EditorCommands`), the commands it dispatches.
 *   The element type excludes `StubCommandType`, so a facade method that
 *   dispatches a stub does not compile.
 *
 * `tools/parity` reads both (issue #342): the forward map is the matrix's
 * "facade" column, and its floor test re-checks that no facade member
 * reaches a stub and that the two maps agree.
 */
import type { Command } from './types';
import type { EditorCommands } from './createEditorCommands';
import type { StubCommandType } from './commandMeta.generated';

type CommandType = Command['type'];

/** Every command the engine actually implements (fully or partially). */
export type LiveCommandType = Exclude<CommandType, StubCommandType>;

/** A command object of a live type — what `EditorCommands.raw` accepts. */
export type LiveCommand = Exclude<Command, { type: StubCommandType }>;

/** How a command is reachable from the facade: a facade member, the shell
 *  / worker / harness only (`'internal'`), or not at all (`'stub'`). */
export type FacadeTarget<K extends CommandType> = K extends StubCommandType
    ? 'stub'
    : keyof EditorCommands | 'internal';

export const COMMAND_FACADE: { readonly [K in CommandType]: FacadeTarget<K> } = {
    /* ---- Phase 1 PoC ---------------------------------------------------- */
    PING: 'internal', // event-log exit gate (e2e) + liveness
    LOAD_FONT: 'internal', // FontRegistry → EngineHandle.loadFont
    RASTERIZE_GLYPH: 'internal', // visual-diff harness
    SHAPE_AND_RASTERIZE: 'internal', // visual-diff harness
    RENDER_PAGE: 'internal', // boot seeding + harness
    INSERT_TEXT: 'insertText',
    UNDO: 'undo',
    REDO: 'redo',
    LOAD_DOCX: 'internal', // legacy harness load
    SAVE_DOCX: 'saveDocx',
    /* ---- Phase 2 §4 ----------------------------------------------------- */
    INIT: 'stub',
    RECOVER: 'internal', // EngineClient.recover
    SNAPSHOT: 'snapshot',
    DISPOSE: 'stub',
    TICK: 'stub',
    OPEN_DOCUMENT: 'openDocument',
    SAVE_DOCUMENT: 'saveDocument',
    EXPORT_PDF: 'exportPdf',
    CLOSE_DOCUMENT: 'closeDocument',
    DELETE_RANGE: 'deleteRange',
    REPLACE_RANGE: 'replaceRange',
    APPLY_FORMATTING: 'applyAttrs',
    TOGGLE_FORMATTING: 'toggleFormatting',
    SPLIT_PARAGRAPH: 'splitParagraph',
    MERGE_PARAGRAPH: 'stub',
    INSERT_IMAGE: 'insertImage',
    RESIZE_IMAGE: 'resizeImage',
    MOVE_IMAGE: 'moveImage',
    SET_IMAGE_WRAP: 'setImageWrap',
    SET_SELECTION: 'setSelection',
    EXTEND_SELECTION: 'extendSelection',
    SELECT_ALL: 'selectAll',
    MOVE_CARET: 'moveCaret',
    BEGIN_COMPOSITION: 'internal', // HiddenInput IME
    UPDATE_COMPOSITION: 'internal', // HiddenInput IME
    END_COMPOSITION: 'internal', // HiddenInput IME
    SET_VIEWPORT: 'setViewport',
    SET_ZOOM: 'setZoom',
    SET_DEVICE_SCALE: 'setDeviceScale',
    REQUEST_PAINT: 'requestPaint',
    EXPAND_LAYOUT: 'expandLayout',
    UNLOAD_FONT: 'stub',
    REQUEST_STATS: 'requestStats',
    /* ---- Phase 4 §7 ----------------------------------------------------- */
    HIT_TEST: 'internal', // pointer pipeline
    HIT_TEST_IN_PAGE: 'internal', // pointer pipeline
    PLACE_CARET_AT_POINT: 'internal', // pointer pipeline
    EXTEND_SELECTION_TO_POINT: 'internal', // pointer pipeline
    GET_IMAGE_RECTS: 'getImageRects',
    SELECT_WORD_AT: 'internal', // pointer pipeline (double-click)
    SELECT_PARAGRAPH_AT: 'internal', // pointer pipeline (triple-click)
    SELECT_CELL_AT: 'internal', // pointer pipeline
    DELETE_AT_CARET: 'deleteAtCaret',
    REQUEST_ACCESSIBILITY_DELTA: 'internal', // worker a11y broadcast
    GET_SELECTION_AS_CLIPBOARD: 'getSelectionAsClipboard',
    PASTE_PLAIN: 'pastePlain',
    SET_PARAGRAPH_ALIGN: 'setParagraphAlign',
    SET_PARAGRAPH_DIRECTION: 'setParagraphDirection',
    PASTE_HTML: 'pasteHtml',
    /* ---- Tables / layout / stories -------------------------------------- */
    INSERT_TABLE: 'insertTable',
    DELETE_TABLE: 'deleteTable',
    INSERT_ROW: 'insertRow',
    DELETE_ROW: 'deleteRow',
    INSERT_COLUMN: 'insertColumn',
    DELETE_COLUMN: 'deleteColumn',
    MERGE_CELLS: 'mergeCells',
    SPLIT_CELL: 'splitCell',
    SET_CELL_SHADING: 'setCellShading',
    SET_CELL_BORDERS: 'setCellBorders',
    SET_TABLE_PROPERTIES: 'setTableProperties',
    SET_COLUMNS: 'setColumns',
    INSERT_PAGE_BREAK: 'insertPageBreak',
    INSERT_SECTION_BREAK: 'insertSectionBreak',
    ENTER_HEADER_FOOTER: 'enterHeaderFooter',
    EXIT_HEADER_FOOTER: 'exitHeaderFooter',
    SET_HEADER_FOOTER_LINK: 'setHeaderFooterLink',
    SET_TITLE_PAGE: 'setTitlePage',
    SET_EVEN_ODD_HEADERS: 'setEvenOddHeaders',
    INSERT_FIELD: 'insertField',
    INSERT_FOOTNOTE: 'insertFootnote',
    INSERT_ENDNOTE: 'insertEndnote',
    INSERT_TEXT_BOX: 'insertTextBox',
    SET_RENDER_DATE: 'internal', // worker injects the host clock
    UPDATE_FIELDS: 'updateFields',
    SET_FIELD_CODE_VIEW: 'setFieldCodeView',
    SET_FIELD_INSTRUCTION: 'setFieldInstruction',
    INSERT_TOC: 'insertToc',
    SET_PARAGRAPH_BORDERS: 'setParagraphBorders',
    SET_PAGE_MARGINS: 'setPageMargins',
    SET_PAGE_ORIENTATION: 'setPageOrientation',
    TOGGLE_LIST: 'toggleList',
    CHANGE_LIST_LEVEL: 'internal', // HiddenInput Tab / Shift+Tab
    SET_PARAGRAPH_INDENT: 'setParagraphIndent',
    SET_LINE_SPACING: 'setLineSpacing',
    SET_PARAGRAPH_SHADING: 'setParagraphShading',
    /* ---- Review --------------------------------------------------------- */
    TOGGLE_TRACK_CHANGES: 'toggleTrackChanges',
    ACCEPT_REVISION: 'acceptRevision',
    REJECT_REVISION: 'rejectRevision',
    ACCEPT_ALL_REVISIONS: 'acceptAllRevisions',
    REJECT_ALL_REVISIONS: 'rejectAllRevisions',
    INSERT_COMMENT: 'insertComment',
    DELETE_COMMENT: 'deleteComment',
    SET_TAB_STOPS: 'setTabStops',
    SET_REVIEW_IDENTITY: 'setReviewIdentity',
    APPLY_STYLE: 'applyStyle',
    RESOLVE_COMMENT: 'resolveComment',
    REPLY_TO_COMMENT: 'replyToComment',
    MODIFY_STYLE: 'modifyStyle',
};

/** What a facade member dispatches: live command types, `'host'` (a host
 *  capability that sends no engine command), or `'raw'` (the typed escape
 *  hatch — any {@link LiveCommand}). */
export type FacadeMemberTarget = readonly LiveCommandType[] | 'host' | 'raw';

export const FACADE_METHOD_COMMANDS: { readonly [M in keyof EditorCommands]: FacadeMemberTarget } =
    {
        requestStats: ['REQUEST_STATS'],
        canRetryGpuRenderer: 'host',
        retryGpuRenderer: 'host',
        canSetRenderer: 'host',
        setRenderer: 'host',
        requestPaint: ['REQUEST_PAINT'],
        setZoom: ['SET_ZOOM'],
        setDeviceScale: ['SET_DEVICE_SCALE'],
        setViewport: ['SET_VIEWPORT'],
        expandLayout: ['EXPAND_LAYOUT'],
        undo: ['UNDO'],
        redo: ['REDO'],
        insertText: ['INSERT_TEXT'],
        deleteRange: ['DELETE_RANGE'],
        replaceRange: ['REPLACE_RANGE'],
        deleteAtCaret: ['DELETE_AT_CARET'],
        splitParagraph: ['SPLIT_PARAGRAPH'],
        insertSoftBreak: ['INSERT_TEXT'],
        selectAll: ['SELECT_ALL'],
        setSelection: ['SET_SELECTION'],
        extendSelection: ['EXTEND_SELECTION'],
        moveCaret: ['MOVE_CARET'],
        applyAttrs: ['APPLY_FORMATTING'],
        setBold: ['APPLY_FORMATTING'],
        setItalic: ['APPLY_FORMATTING'],
        setStrike: ['APPLY_FORMATTING'],
        setUnderline: ['APPLY_FORMATTING'],
        setVerticalScript: ['APPLY_FORMATTING'],
        setFontFamily: ['APPLY_FORMATTING'],
        setFontSize: ['APPLY_FORMATTING'],
        setSlotFormat: ['APPLY_FORMATTING'],
        setColor: ['APPLY_FORMATTING'],
        setHighlight: ['APPLY_FORMATTING'],
        setCaps: ['APPLY_FORMATTING'],
        setSmallCaps: ['APPLY_FORMATTING'],
        toggleFormatting: ['TOGGLE_FORMATTING'],
        setParagraphAlign: ['SET_PARAGRAPH_ALIGN'],
        setParagraphDirection: ['SET_PARAGRAPH_DIRECTION'],
        insertImage: ['INSERT_IMAGE'],
        insertImageAtCaret: ['INSERT_IMAGE'],
        resizeImage: ['RESIZE_IMAGE'],
        moveImage: ['MOVE_IMAGE'],
        setImageWrap: ['SET_IMAGE_WRAP'],
        getImageRects: ['GET_IMAGE_RECTS'],
        insertTable: ['INSERT_TABLE'],
        insertTableAtCaret: ['INSERT_TABLE'],
        deleteTable: ['DELETE_TABLE'],
        insertRow: ['INSERT_ROW'],
        deleteRow: ['DELETE_ROW'],
        insertColumn: ['INSERT_COLUMN'],
        deleteColumn: ['DELETE_COLUMN'],
        mergeCells: ['MERGE_CELLS'],
        splitCell: ['SPLIT_CELL'],
        setCellShading: ['SET_CELL_SHADING'],
        setCellBorders: ['SET_CELL_BORDERS'],
        setTableProperties: ['SET_TABLE_PROPERTIES'],
        setTableBidiVisual: ['SET_TABLE_PROPERTIES'],
        setColumns: ['SET_COLUMNS'],
        setColumnsAtCaret: ['SET_COLUMNS'],
        insertPageBreak: ['INSERT_PAGE_BREAK'],
        insertSectionBreak: ['INSERT_SECTION_BREAK'],
        enterHeaderFooter: ['ENTER_HEADER_FOOTER'],
        exitHeaderFooter: ['EXIT_HEADER_FOOTER'],
        setHeaderFooterLink: ['SET_HEADER_FOOTER_LINK'],
        setTitlePage: ['SET_TITLE_PAGE'],
        setEvenOddHeaders: ['SET_EVEN_ODD_HEADERS'],
        insertField: ['INSERT_FIELD'],
        updateFields: ['UPDATE_FIELDS'],
        setFieldCodeView: ['SET_FIELD_CODE_VIEW'],
        setFieldInstruction: ['SET_FIELD_INSTRUCTION'],
        insertToc: ['INSERT_TOC'],
        insertFootnote: ['INSERT_FOOTNOTE'],
        insertEndnote: ['INSERT_ENDNOTE'],
        insertTextBox: ['INSERT_TEXT_BOX'],
        setParagraphBorders: ['SET_PARAGRAPH_BORDERS'],
        clearParagraphBorders: ['SET_PARAGRAPH_BORDERS'],
        toggleList: ['TOGGLE_LIST'],
        setParagraphIndent: ['SET_PARAGRAPH_INDENT'],
        increaseIndent: ['SET_PARAGRAPH_INDENT'],
        decreaseIndent: ['SET_PARAGRAPH_INDENT'],
        setLineSpacing: ['SET_LINE_SPACING'],
        setParagraphShading: ['SET_PARAGRAPH_SHADING'],
        setTabStops: ['SET_TAB_STOPS'],
        toggleTrackChanges: ['TOGGLE_TRACK_CHANGES'],
        setReviewIdentity: ['SET_REVIEW_IDENTITY'],
        snapshot: ['SNAPSHOT'],
        acceptRevision: ['ACCEPT_REVISION'],
        rejectRevision: ['REJECT_REVISION'],
        acceptAllRevisions: ['ACCEPT_ALL_REVISIONS'],
        rejectAllRevisions: ['REJECT_ALL_REVISIONS'],
        insertComment: ['INSERT_COMMENT'],
        deleteComment: ['DELETE_COMMENT'],
        resolveComment: ['RESOLVE_COMMENT'],
        replyToComment: ['REPLY_TO_COMMENT'],
        applyStyle: ['APPLY_STYLE'],
        modifyStyle: ['MODIFY_STYLE'],
        setPageMargins: ['SET_PAGE_MARGINS'],
        setPageMarginsAtCaret: ['SET_PAGE_MARGINS'],
        setPageOrientation: ['SET_PAGE_ORIENTATION'],
        setPageOrientationAtCaret: ['SET_PAGE_ORIENTATION'],
        openDocument: ['SET_ZOOM', 'OPEN_DOCUMENT'],
        saveDocument: ['SAVE_DOCUMENT'],
        saveDocx: ['SAVE_DOCX'],
        exportPdf: ['EXPORT_PDF'],
        exportHtml: ['SAVE_DOCUMENT'],
        exportPlainText: ['SAVE_DOCUMENT'],
        closeDocument: ['CLOSE_DOCUMENT'],
        getSelectionAsClipboard: ['GET_SELECTION_AS_CLIPBOARD'],
        pastePlain: ['PASTE_PLAIN'],
        pasteHtml: ['PASTE_HTML'],
        raw: 'raw',
    };
