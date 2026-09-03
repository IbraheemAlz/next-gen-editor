/**
 * createEditorState — headless Solid primitive that exposes engine state
 * as reactive signals. UI components read these signals to mirror engine
 * state (selection, caret, undo availability, stats, attrs at caret)
 * without subscribing to raw worker events.
 *
 * Each signal is fed by a single subscription to Engine.subscribe(). The
 * subscription is torn down on owner dispose via onCleanup().
 *
 * Authoritative shapes: see `crates/engine-wasm/pkg/engine_wasm.d.ts`.
 *   - `SELECTION_CHANGED` carries: range, caret, direction, rects,
 *     attrs_at_caret, paragraph_alignment, can_undo, can_redo,
 *     selection_kind, attrs_mixed, paragraph_direction.
 *   - `STATS` is inline: `({ type: "STATS" } & EngineStats)`.
 *   - `PAINTED` carries: dirty, version, paint_ms, document_height,
 *     page_count, is_full_layout, estimated_document_height.
 */
import { createSignal, onCleanup, type Accessor } from 'solid-js';
import { useEngine } from './EngineProvider';
import type {
    Alignment,
    AttrsMixed,
    BridgeCellBorders,
    BridgeStoryRef,
    BridgeFieldRef,
    BridgeCellProperties,
    BridgeIndent,
    BridgeSectionGeometry,
    BridgeTabStop,
    Direction,
    EngineStats,
    Event,
    LayoutDegraded,
    LogicalRange,
    Rect,
    SelectionKind,
    TextAttrs,
} from './types';

export interface EditorState {
    /** Current selection (anchor + caret), `undefined` before first SELECTION_CHANGED. */
    selection: Accessor<LogicalRange | undefined>;
    /** Caret rectangle (single Rect — the active caret tip). */
    caret: Accessor<Rect | undefined>;
    /** Per-line selection rectangles in device px. */
    rects: Accessor<Rect[]>;
    /** Resolved text attributes at the caret. */
    attrsAtCaret: Accessor<TextAttrs | undefined>;
    /** Mixed flags across the selection (bold/italic/underline/strike). */
    attrsMixed: Accessor<AttrsMixed | undefined>;
    /** Paragraph alignment of the paragraph containing the caret. */
    paragraphAlignment: Accessor<Alignment | undefined>;
    /** Paragraph direction (`Ltr` / `Rtl` / `undefined` when mixed across selection). */
    paragraphDirection: Accessor<Direction | undefined>;
    /** Selection kind — linear span or rectangular table-cell block. */
    selectionKind: Accessor<SelectionKind | undefined>;
    /** Issue #29 — the caret paragraph's style id (`<w:pStyle>`), for a
     *  truthful StylesDropdown active indicator. `undefined` when
     *  detached or before the first SELECTION_CHANGED. */
    paragraphStyleId: Accessor<string | undefined>;
    /** Undo availability. */
    canUndo: Accessor<boolean>;
    /** Redo availability. */
    canRedo: Accessor<boolean>;
    /** Latest engine stats; updates every `REQUEST_STATS` reply. */
    stats: Accessor<EngineStats | undefined>;
    /** Last paint timing in ms; updates every PAINTED event. */
    lastPaintMs: Accessor<number>;
    /** Latest paint version — bumps every repaint. */
    paintVersion: Accessor<number>;
    /** Estimated full document height (lazy layout) in pt. */
    estimatedDocumentHeight: Accessor<number>;
    /**
     * Issue #87 — degradation notes of the latest paint (empty on a
     * nominal paint). Non-empty means the layout self-defense fired:
     * the paginator watchdog released a constraint, pinned a churning
     * block or hit the page cap, or an incremental band was demoted to
     * a full reflow. The pixels are on screen either way; surface this
     * in diagnostics (Dev HUD, telemetry), never as a blocking error.
     */
    layoutDegraded: Accessor<LayoutDegraded[]>;
    /**
     * Issue #26 — absolute per-page top offsets + heights in document
     * device px, index-aligned, straight from the paginator. Empty
     * arrays until the first paginated paint. Exact under
     * mixed-orientation sections — consumers must not derive page tops
     * from uniform page-size constants.
     */
    pageGeometry: Accessor<{ tops: number[]; heights: number[] }>;
    /** Active renderer reported by the worker at INIT, re-reported by the
     *  engine on every `RECOVERED` (issue #66). */
    renderer: Accessor<string>;
    /**
     * Sprint 10 — page geometry of the section under the caret. Drives
     * `PageSetupDialog` prefill (margin / orientation / columns).
     * `undefined` before the first `SELECTION_CHANGED`, or when the
     * caret has no addressable top-level block (defensive).
     */
    sectionGeometry: Accessor<BridgeSectionGeometry | undefined>;
    /**
     * Sprint 10 — shading + per-edge borders for the active cell when
     * the caret is inside a table. `undefined` outside any table —
     * `CellPropertiesDialog` then falls back to engine defaults.
     */
    cellProperties: Accessor<BridgeCellProperties | undefined>;
    /**
     * Sprint 11 (#13) — custom tab stops of the paragraph under the
     * caret. Empty array when the paragraph inherits the default
     * 0.5-inch grid. Drives the Ruler's tab-stop markers.
     */
    tabStops: Accessor<BridgeTabStop[]>;
    /**
     * Sprint 15 (#13) — `<w:pPr><w:ind>` indentation of the paragraph
     * under the caret, in layout pt. Drives the Ruler's indent-handle
     * read-back (markers jump to the active paragraph's values) and
     * clobber-free commits (dragging one handle preserves the others).
     * All-zero `BridgeIndent` before the first `SELECTION_CHANGED` or
     * when the caret has no addressable paragraph.
     */
    paragraphIndent: Accessor<BridgeIndent>;
    /**
     * Sprint 14 (#14) — engine track-changes recording state.
     * `ReviewControls`'s Track toggle binds its active state to
     * this accessor instead of carrying local Solid state, so a
     * `ToggleTrackChanges` issued by any path (macro, undo, second
     * tab) stays in sync.
     */
    isTrackingChanges: Accessor<boolean>;
    /**
     * Issue #41 — per-edge borders of the paragraph under the caret, or
     * `undefined` when it has none set. Feeds the paragraph border
     * picker's prefill so it opens reflecting the live document (mirrors
     * how `cellProperties` feeds the cell border editor).
     */
    paragraphBorders: Accessor<BridgeCellBorders | undefined>;
    /**
     * Phase 3 (#39) — the active header/footer story, or `undefined` in
     * body mode. While set, the engine re-roots every caret-relative
     * command into the story; the shell dims the body, outlines the
     * band on the anchor page, and controls whose commands are gated
     * in story mode disable themselves on this accessor (Honest UX —
     * the engine's `Event::Error` backstop is invisible to sighted
     * users by design).
     */
    editingStory: Accessor<BridgeStoryRef | undefined>;
    /**
     * Issue #77 — `true` while the Alt+F9 field-code view is on: fields
     * PAINT `{ INSTRUCTION }` codes in place of results. Pure display
     * state — every `LogicalPos` on the wire stays a source position.
     */
    fieldCodeView: Accessor<boolean>;
    /**
     * Issue #77 — the field the selection addresses: the one it covers
     * exactly (`selected: true` — a click inside a field selects it
     * whole), else the field a collapsed caret touches. `undefined`
     * away from any field. Drives the field-code editor and the
     * "Update fields" affordance in `FieldButtons`.
     */
    fieldAtCaret: Accessor<BridgeFieldRef | undefined>;
}

export function createEditorState(): EditorState {
    const engine = useEngine();

    const [selection, setSelection] = createSignal<LogicalRange | undefined>(undefined);
    const [caret, setCaret] = createSignal<Rect | undefined>(undefined);
    const [rects, setRects] = createSignal<Rect[]>([]);
    const [attrsAtCaret, setAttrsAtCaret] = createSignal<TextAttrs | undefined>(undefined);
    const [attrsMixed, setAttrsMixed] = createSignal<AttrsMixed | undefined>(undefined);
    const [paragraphAlignment, setParagraphAlignment] = createSignal<Alignment | undefined>(undefined);
    const [paragraphDirection, setParagraphDirection] = createSignal<Direction | undefined>(undefined);
    const [selectionKind, setSelectionKind] = createSignal<SelectionKind | undefined>(undefined);
    const [paragraphStyleId, setParagraphStyleId] = createSignal<string | undefined>(undefined);
    const [canUndo, setCanUndo] = createSignal(false);
    const [canRedo, setCanRedo] = createSignal(false);
    const [stats, setStats] = createSignal<EngineStats | undefined>(undefined);
    const [lastPaintMs, setLastPaintMs] = createSignal(0);
    const [paintVersion, setPaintVersion] = createSignal(0);
    const [estimatedDocumentHeight, setEstimatedDocumentHeight] = createSignal(0);
    const [layoutDegraded, setLayoutDegraded] = createSignal<LayoutDegraded[]>([]);
    const [pageGeometry, setPageGeometry] = createSignal<{
        tops: number[];
        heights: number[];
    }>({ tops: [], heights: [] });
    const [renderer, setRenderer] = createSignal(engine.renderer);
    const [sectionGeometry, setSectionGeometry] =
        createSignal<BridgeSectionGeometry | undefined>(undefined);
    const [cellProperties, setCellProperties] =
        createSignal<BridgeCellProperties | undefined>(undefined);
    const [tabStops, setTabStops] = createSignal<BridgeTabStop[]>([]);
    const ZERO_INDENT: BridgeIndent = { start_pt: 0, end_pt: 0, first_line_pt: 0 };
    const [paragraphIndent, setParagraphIndent] = createSignal<BridgeIndent>(ZERO_INDENT);
    const [isTrackingChanges, setIsTrackingChanges] = createSignal(false);
    const [paragraphBorders, setParagraphBorders] =
        createSignal<BridgeCellBorders | undefined>(undefined);
    const [editingStory, setEditingStory] =
        createSignal<BridgeStoryRef | undefined>(undefined);
    const [fieldCodeView, setFieldCodeView] = createSignal(false);
    const [fieldAtCaret, setFieldAtCaret] =
        createSignal<BridgeFieldRef | undefined>(undefined);

    const unsubscribe = engine.subscribe((evt: Event) => {
        switch (evt.type) {
            case 'SELECTION_CHANGED': {
                setSelection(evt.range);
                setCaret(evt.caret);
                setRects(evt.rects);
                setAttrsAtCaret(evt.attrs_at_caret);
                setAttrsMixed(evt.attrs_mixed);
                setParagraphAlignment(evt.paragraph_alignment);
                setParagraphDirection(evt.paragraph_direction);
                setSelectionKind(evt.selection_kind);
                setParagraphStyleId(evt.paragraph_style_id);
                setCanUndo(evt.can_undo);
                setCanRedo(evt.can_redo);
                setSectionGeometry(evt.section_geometry);
                setCellProperties(evt.cell_properties);
                setTabStops(evt.tab_stops);
                setParagraphIndent(evt.paragraph_indent ?? ZERO_INDENT);
                setIsTrackingChanges(evt.is_tracking_changes);
                setParagraphBorders(evt.paragraph_borders);
                setEditingStory(evt.editing_story);
                setFieldCodeView(evt.field_code_view);
                setFieldAtCaret(evt.field_at_caret);
                break;
            }
            case 'UNDO_STATE_CHANGED': {
                setCanUndo(evt.can_undo);
                setCanRedo(evt.can_redo);
                break;
            }
            case 'RECOVERED': {
                /* Issue #66 — the recovered engine reports the backend it
                   actually paints with; the INIT-time value is stale once
                   a respawned worker re-probed the GPU. */
                setRenderer(evt.renderer);
                break;
            }
            case 'TEXT_INSERTED': {
                setCanUndo(evt.can_undo);
                setCanRedo(evt.can_redo);
                break;
            }
            case 'STATS': {
                /* STATS is inline ({ type: "STATS" } & EngineStats), not nested. */
                const { type: _t, ...statsFields } = evt;
                setStats(statsFields as EngineStats);
                break;
            }
            case 'PAINTED': {
                setLastPaintMs(evt.paint_ms);
                setPaintVersion(evt.version);
                setEstimatedDocumentHeight(evt.estimated_document_height);
                setLayoutDegraded(evt.layout_degraded ?? []);
                if (evt.page_tops.length > 0) {
                    setPageGeometry({ tops: evt.page_tops, heights: evt.page_heights });
                }
                break;
            }
            default:
                break;
        }
    });

    onCleanup(unsubscribe);

    return {
        selection,
        caret,
        rects,
        attrsAtCaret,
        attrsMixed,
        paragraphAlignment,
        paragraphDirection,
        selectionKind,
        paragraphStyleId,
        canUndo,
        canRedo,
        stats,
        lastPaintMs,
        paintVersion,
        estimatedDocumentHeight,
        layoutDegraded,
        pageGeometry,
        renderer,
        sectionGeometry,
        cellProperties,
        tabStops,
        paragraphIndent,
        isTrackingChanges,
        paragraphBorders,
        editingStory,
        fieldCodeView,
        fieldAtCaret,
    };
}
