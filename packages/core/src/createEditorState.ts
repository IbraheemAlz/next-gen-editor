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
 *     selection_kind, attrs_mixed, paragraph_direction, and (issues
 *     #423 / #420) resolved_font_latin, resolved_font_cs, font_source,
 *     slot_formats, caret_font_slot.
 *   - `STATS` is inline: `({ type: "STATS" } & EngineStats)`.
 *   - `PAINTED` carries: dirty, version, paint_ms, document_height,
 *     page_count, is_full_layout, estimated_document_height.
 */
import { createRoot, createSignal, onCleanup, type Accessor } from 'solid-js';
import { useEngine, type EngineHandle } from './EngineProvider';
import type {
    Alignment,
    AttrsMixed,
    BridgeCellBorders,
    BridgeStoryRef,
    BridgeFieldRef,
    BridgeCellProperties,
    BridgeFontSources,
    BridgeIndent,
    BridgeSectionGeometry,
    BridgeSlotFormats,
    BridgeTabStop,
    CheckpointStatus,
    CommentHighlight,
    Direction,
    EngineStats,
    ErrorKind,
    Event,
    FontSlot,
    FontSubstitution,
    LayoutDegraded,
    PreviousSessionInfo,
    LogicalRange,
    ReadWarning,
    ProtectionMode,
    RecoveryReport,
    Rect,
    RendererDowngrade,
    SelectionKind,
    TextAttrs,
} from './types';

export interface EditorState {
    /** Current selection (anchor + caret), `undefined` before first SELECTION_CHANGED. */
    selection: Accessor<LogicalRange | undefined>;
    /** Caret rectangle (single Rect — the active caret tip), in absolute
     *  document device px (pages stacked with the paint's gap; x is
     *  page-local). */
    caret: Accessor<Rect | undefined>;
    /**
     * Issue #387 — bumps once per `SELECTION_CHANGED` whose command moved
     * the caret by keyboard or programmatically (navigation, typing, an
     * edit, undo — `reveal_caret`), never for a pointer gesture, Ctrl+A or
     * a zoom. A host scrolls {@link EditorState.caret} into view when it
     * moves (`createCaretReveal`, which `EditorSurface` uses). `0` until
     * the first such event.
     */
    caretRevealSeq: Accessor<number>;
    /** Per-line selection rectangles in device px. */
    rects: Accessor<Rect[]>;
    /** Resolved text attributes at the caret. */
    attrsAtCaret: Accessor<TextAttrs | undefined>;
    /** Mixed flags across the selection (bold/italic/underline/strike). */
    attrsMixed: Accessor<AttrsMixed | undefined>;
    /**
     * Issue #423 — the family the caret run's LATIN slot resolves to
     * after the full cascade (run name → theme binding → style chain →
     * docDefaults → the layout's default face), as a display name
     * (`"Calibri"`). `undefined` before the first SELECTION_CHANGED or
     * from an engine that predates the field. Unlike
     * `attrsAtCaret().font_family` (the run's own id, else the layout
     * default), this is what Word's font box shows.
     */
    resolvedFontLatin: Accessor<string | undefined>;
    /** Issue #423 — {@link EditorState.resolvedFontLatin} for the
     *  COMPLEX-SCRIPT slot (`w:cs` / `w:cstheme` + the theme's script
     *  entry): what Arabic / Hebrew text at the caret is shaped with. */
    resolvedFontCs: Accessor<string | undefined>;
    /** Issue #423 — where each slot's family came from (`Explicit` /
     *  `Theme` / `Style` / `Default`, and — issue #329 — `Substituted`
     *  when a substitute face serves the named family); a picker marks
     *  `Theme` fonts. */
    fontSource: Accessor<BridgeFontSources | undefined>;
    /** Issue #420 — the caret run's resolved family id / size / bold /
     *  italic PER SCRIPT SLOT — the Font dialog's "Latin text" and
     *  "Complex scripts" seed. */
    slotFormats: Accessor<BridgeSlotFormats | undefined>;
    /** Issue #423 — the slot `attrsAtCaret` reports: `'ComplexScript'`
     *  when the caret sits in Arabic / Hebrew / … text (or a `<w:rtl/>`
     *  run), else `'Latin'`. The toolbar picker shows that slot's family. */
    caretFontSlot: Accessor<FontSlot | undefined>;
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
     * from uniform page-size constants. Issue #280 — `widths` are the
     * per-page widths (device px, index-aligned; empty from a pre-#280
     * engine); a host sizes its page cards from `widths` / `heights`
     * so a zoom visibly resizes the page. Issue #387 — shared like
     * `zoom` (one signal per engine), so a page overlay mounted after the
     * last paint still positions against the live geometry.
     */
    pageGeometry: Accessor<{ tops: number[]; heights: number[]; widths: number[] }>;
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
     * Issue #345 — the editing restriction the open document enforces
     * (`readOnly` / `comments` / `trackedChanges` / `forms`), or
     * `undefined` for an unrestricted document. Drives the protection
     * badge; the engine refuses whatever the mode does not allow.
     */
    protection: Accessor<ProtectionMode | undefined>;
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
    /**
     * Issue #52 — the ENGINE's user zoom fraction (`1` = 100 %), read
     * back from `SELECTION_CHANGED.zoom` (every `SET_ZOOM` /
     * `SET_DEVICE_SCALE` answers with one). Shared by every
     * `createEditorState()` on the same engine — see `viewStateFor` — so
     * two zoom widgets can never disagree, and a widget mounted late
     * starts from the live value instead of 100 %.
     */
    zoom: Accessor<number>;
    /**
     * Issue #97 — the boot device scale (`devicePixelRatio × 4/3`) the
     * engine reported on its last `RECOVERED`; `undefined` before any
     * recovery, or when the engine came back cold (no layout config — the
     * shell re-seeds it). Shared like `zoom`.
     */
    deviceScale: Accessor<number | undefined>;
    /**
     * Issue #99 — set when the current worker generation was forced onto
     * Canvas2D after a crash loop on Vello (`RECOVERED.renderer_downgrade`);
     * `undefined` otherwise. Surfaced in the Dev HUD next to `renderer`.
     * Shared like `zoom`.
     */
    rendererDowngrade: Accessor<RendererDowngrade | undefined>;
    /**
     * Issue #315 — the report of the most recent completed crash recovery
     * (`engine.onRecovery`), `undefined` before any or when the engine
     * offers no recovery report. Feed it to `recoveryNotices()` for the
     * user-facing losses; the Dev HUD shows the raw flags. Shared like
     * `zoom`, so a component mounted after the recovery still sees it.
     */
    lastRecovery: Accessor<RecoveryReport | undefined>;
    /**
     * Issue #333 - whether the engine's event-log checkpoints have been
     * failing past their bounded retries (`engine.onCheckpointStatus`);
     * `false` when the engine reports no checkpoint health. Shared like
     * `zoom`.
     */
    checkpointFailing: Accessor<boolean>;
    /**
     * Issue #390 - the event log's health as the bridge's
     * `Event::CheckpointState` reports it (broadcast on `subscribe()`
     * whenever it changes; seeded from `engine.checkpointStatus`).
     * `ok: false` once the bounded retries of a failed checkpoint
     * (snapshot dispatch / snapshot write) or of the command journal ran
     * out; `journalFailing` marks the journal specifically (recovery would
     * miss commands). Shared like `zoom`.
     */
    checkpointState: Accessor<CheckpointHealth>;
    /**
     * Issue #364 - the most recent `Event::Error` any command answered
     * (engine refusals such as a tracked deletion across a table cell, or
     * a typed `ErrorKind`), with the command that produced it and a
     * running count; `undefined` until the first one. Every error reply
     * moves it - a repeat of the same message is a NEW object - so a UI
     * can show a transient, visible refusal instead of a key press that
     * silently does nothing. Shared like `zoom`.
     */
    lastError: Accessor<EditorError | undefined>;
    /**
     * Issue #388 - the previous page generation's unsaved session, set
     * aside at boot and waiting for Recover / Discard
     * (`engine.recoverPreviousSession` / `discardPreviousSession`);
     * `undefined` when there is none. Shared like `zoom`.
     */
    previousSession: Accessor<PreviousSessionInfo | undefined>;
    /**
     * Issue #406 - the reader's warning report for the document that is
     * open (`DOCUMENT_LOADED.warnings`, coalesced: an entry's `count` says
     * how many identical warnings it stands for). Empty after a clean
     * open, a `.txt` / `.html` open, a `closeDocument()` or a restored
     * previous session; non-empty means the file opened DEGRADED (a
     * clamped page margin, a part read through namespace normalisation,
     * ...) and the UI must say so (`@nge/ui` `OpenWarningsBanner`; feed
     * entries to `describeReadWarning()`). Survives a crash recovery of
     * the same document. Shared like `zoom`.
     */
    openWarnings: Accessor<ReadWarning[]>;
    /**
     * Issue #426 - every previous session still waiting for a decision
     * (the archive ring, newest first); `previousSession` is the first.
     */
    previousSessions: Accessor<PreviousSessionInfo[]>;
    /**
     * Issue #329 - the font substitutions the open document's layout
     * makes (a family it names that the editor does not ship, and the
     * loaded face standing in for it: Calibri -> Carlito, Simplified
     * Arabic -> Noto Naskh Arabic), sorted by slot then family; `[]` when
     * nothing is substituted. Fed by `DOCUMENT_LOADED` and by every
     * `FONT_LOADED` (a newly loaded face can start serving a family).
     * Shared like `zoom`.
     */
    fontSubstitutions: Accessor<FontSubstitution[]>;
    /**
     * Issue #387 — the on-canvas highlight of every top-level comment
     * (`Event::CommentHighlights`): per-line rects in absolute document
     * device px, the `caret` / `rects` space. Empty for a document without
     * comments. Shared like `zoom`, so an overlay mounted late starts from
     * the live highlights.
     */
    commentHighlights: Accessor<CommentHighlight[]>;
}

/**
 * Engine-wide view state (issue #52). Unlike the per-call signals in
 * `createEditorState`, these are ONE set of signals per engine handle:
 * they are the single source of truth every zoom control reads, and they
 * must outlive any one component (a widget mounted after the last
 * `SELECTION_CHANGED` would otherwise start stale at 100 %). Created
 * lazily under a detached root; the subscription lives as long as the
 * engine handle, which the shell keeps for the page lifetime.
 */
/** Issue #364 - see `EditorState.lastError`. */
export interface EditorError {
    /** The typed class (`Event::Error.kind`), when the engine set one. */
    kind: ErrorKind | undefined;
    /** The command that was refused, parsed from the engine's
     *  `<Command>: <reason>` message prefix; `undefined` when absent. */
    command: string | undefined;
    /** The engine's message, verbatim. */
    message: string;
    /** Errors seen so far this session (this one included). */
    count: number;
    /** When it arrived (ms since the epoch). */
    at: number;
}

/** Issue #390 - see `EditorState.checkpointState`. */
export interface CheckpointHealth {
    ok: boolean;
    failures: number;
    journalFailing: boolean;
    lastError: string | undefined;
}

interface PageGeometryState {
    tops: number[];
    heights: number[];
    widths: number[];
}

interface ViewState {
    zoom: Accessor<number>;
    pageGeometry: Accessor<PageGeometryState>;
    commentHighlights: Accessor<CommentHighlight[]>;
    deviceScale: Accessor<number | undefined>;
    rendererDowngrade: Accessor<RendererDowngrade | undefined>;
    lastRecovery: Accessor<RecoveryReport | undefined>;
    checkpointFailing: Accessor<boolean>;
    checkpointState: Accessor<CheckpointHealth>;
    lastError: Accessor<EditorError | undefined>;
    previousSession: Accessor<PreviousSessionInfo | undefined>;
    openWarnings: Accessor<ReadWarning[]>;
    clearOpenWarnings: () => void;
    previousSessions: Accessor<PreviousSessionInfo[]>;
    fontSubstitutions: Accessor<FontSubstitution[]>;
}

const viewStates = new WeakMap<EngineHandle, ViewState>();

/**
 * Issue #406 - forget the open document's reader warnings (the document
 * they describe is gone). `createEditorCommands().closeDocument()` calls
 * this once the engine confirmed the close; the CLOSE_DOCUMENT reply is a
 * plain `SELECTION_CHANGED`, which the shared subscription cannot tell
 * apart from any other. Internal to `@nge/core`.
 */
export function clearOpenWarnings(engine: EngineHandle): void {
    viewStateFor(engine).clearOpenWarnings();
}

/** f32 → f64 noise (`1.100000023841858`) would defeat the `<select>`'s
 *  preset matching; four decimals is far below any zoom step. */
function roundZoom(z: number): number {
    return Math.round(z * 10_000) / 10_000;
}

function viewStateFor(engine: EngineHandle): ViewState {
    const existing = viewStates.get(engine);
    if (existing) return existing;
    const state = createRoot(() => {
        const [zoom, setZoom] = createSignal(1);
        const [deviceScale, setDeviceScale] = createSignal<number | undefined>(undefined);
        /* Issue #240 — seeded from the engine and fed by its change feed:
           a downgrade persisted across reloads is in force from boot,
           before any RECOVERED event exists. */
        const [rendererDowngrade, setRendererDowngrade] = createSignal<
            RendererDowngrade | undefined
        >(engine.rendererDowngrade);
        engine.onRendererDowngrade?.((d) => setRendererDowngrade(d));
        /* Issue #315 — fed AFTER the client folded the worker reply into
           its report (the RECOVERED event arrives before that). */
        const [lastRecovery, setLastRecovery] = createSignal<RecoveryReport | undefined>(
            engine.lastRecovery,
        );
        /* Issue #406 - the open document's reader warnings. A crash
           recovery of the SAME document keeps them (the degraded read is
           what the restored model holds); restoring the previous session
           swaps the document, so its open's warnings no longer apply. */
        const [openWarnings, setOpenWarnings] = createSignal<ReadWarning[]>([]);
        engine.onRecovery?.((report) => {
            setLastRecovery(() => report);
            if (report.cause === 'session-restore') setOpenWarnings([]);
        });
        /* Issue #333 - checkpoint health, seeded then fed by the client. */
        const toHealth = (s: CheckpointStatus | undefined): CheckpointHealth => ({
            ok: s?.failing !== true,
            failures: s?.failures ?? 0,
            journalFailing: s?.journalFailing === true,
            lastError: s?.lastError,
        });
        const [checkpointState, setCheckpointState] = createSignal<CheckpointHealth>(
            toHealth(engine.checkpointStatus),
        );
        /* Issue #390 - fed by the client's status feed (it also resets on
           a respawned worker) AND by the typed bridge event itself. */
        engine.onCheckpointStatus?.((s) => setCheckpointState(toHealth(s)));
        const checkpointFailing: Accessor<boolean> = () => !checkpointState().ok;
        /* Issue #388 - the unsaved previous session, seeded then fed. */
        const [previousSession, setPreviousSession] = createSignal<
            PreviousSessionInfo | undefined
        >(engine.previousSession);
        engine.onPreviousSession?.((p) => setPreviousSession(p));
        const [previousSessions, setPreviousSessions] = createSignal<PreviousSessionInfo[]>(
            engine.previousSessions ?? (engine.previousSession ? [engine.previousSession] : []),
        );
        engine.onPreviousSessions?.((p) => setPreviousSessions(p));
        /* Issue #329 - what the open document's layout substitutes. */
        const [fontSubstitutions, setFontSubstitutions] = createSignal<FontSubstitution[]>([]);
        /* Issue #364 - every `Event::Error` reply, with its command. */
        const [lastError, setLastError] = createSignal<EditorError | undefined>(undefined);
        let errorCount = 0;
        /* Issue #26 / #387 — the paginator's page geometry, shared. */
        const [pageGeometry, setPageGeometry] = createSignal<PageGeometryState>({
            tops: [],
            heights: [],
            widths: [],
        });
        /* Issue #387 — every commented range's highlight rects. */
        const [commentHighlights, setCommentHighlights] = createSignal<CommentHighlight[]>([]);
        engine.subscribe((evt: Event) => {
            if (evt.type === 'PAINTED' && evt.page_tops.length > 0) {
                setPageGeometry({
                    tops: evt.page_tops,
                    heights: evt.page_heights,
                    widths: evt.page_widths ?? [],
                });
            } else if (evt.type === 'COMMENT_HIGHLIGHTS') {
                setCommentHighlights(evt.highlights);
            } else if (evt.type === 'DOCUMENT_LOADED' || evt.type === 'DOCUMENT_CLOSED') {
                /* The next broadcast (if the new document has comments)
                   repopulates; until then nothing stale is drawn. */
                setCommentHighlights([]);
            }
            if (evt.type === 'ERROR') {
                errorCount += 1;
                const prefix = /^([A-Za-z][A-Za-z0-9]*): /.exec(evt.message);
                setLastError({
                    kind: evt.kind,
                    command: prefix?.[1],
                    message: evt.message,
                    count: errorCount,
                    at: Date.now(),
                });
            }
            if (evt.type === 'DOCUMENT_LOADED' || evt.type === 'FONT_LOADED') {
                setFontSubstitutions(evt.substituted ?? []);
            }
            if (evt.type === 'DOCUMENT_LOADED') {
                /* Issue #406 - every open replaces the report (a clean
                   open, `.txt` / `.html`, sends none). */
                setOpenWarnings(evt.warnings ?? []);
            } else if (evt.type === 'SELECTION_CHANGED' && evt.zoom !== undefined) {
                setZoom(roundZoom(evt.zoom));
            } else if (evt.type === 'ZOOM_PENDING') {
                /* Issue #239 — a `SET_ZOOM` / `SET_DEVICE_SCALE` sent
                   before the engine's first `RENDER_PAGE` answers with
                   this instead of `SELECTION_CHANGED` (there is no
                   selection yet to build one around). Mirror it into the
                   same signals so a zoom control reflects the requested
                   value immediately instead of flashing 100 % until the
                   boot `RENDER_PAGE` + its `SELECTION_CHANGED` land. */
                setZoom(roundZoom(evt.zoom));
                if (evt.device_scale !== undefined) setDeviceScale(evt.device_scale);
            } else if (evt.type === 'CHECKPOINT_STATE') {
                setCheckpointState({
                    ok: evt.ok,
                    failures: evt.failures,
                    journalFailing: evt.journal_failing === true,
                    lastError: evt.last_error,
                });
            } else if (evt.type === 'RECOVERED') {
                /* Issue #97 — the respawned engine folded the replayed
                   SET_ZOOM / SET_DEVICE_SCALE into its restored config (or
                   came back cold at 100 %); the controls follow IT, not
                   whatever they showed before the trap. */
                setZoom(roundZoom(evt.zoom ?? 1));
                setDeviceScale(evt.device_scale);
                /* Issue #99 — per generation: the client re-sends the
                   downgrade on every forced recovery, so it stays set for
                   the rest of the session once the crash loop tripped. */
                setRendererDowngrade(evt.renderer_downgrade);
            }
        });
        return {
            zoom,
            pageGeometry,
            commentHighlights,
            deviceScale,
            rendererDowngrade,
            lastRecovery,
            checkpointFailing,
            checkpointState,
            lastError,
            previousSession,
            openWarnings,
            clearOpenWarnings: () => setOpenWarnings([]),
            previousSessions,
            fontSubstitutions,
        };
    });
    viewStates.set(engine, state);
    return state;
}

export function createEditorState(): EditorState {
    const engine = useEngine();
    const view = viewStateFor(engine);

    const [selection, setSelection] = createSignal<LogicalRange | undefined>(undefined);
    const [caret, setCaret] = createSignal<Rect | undefined>(undefined);
    const [caretRevealSeq, setCaretRevealSeq] = createSignal(0);
    const [rects, setRects] = createSignal<Rect[]>([]);
    const [attrsAtCaret, setAttrsAtCaret] = createSignal<TextAttrs | undefined>(undefined);
    const [attrsMixed, setAttrsMixed] = createSignal<AttrsMixed | undefined>(undefined);
    const [resolvedFontLatin, setResolvedFontLatin] = createSignal<string | undefined>(undefined);
    const [resolvedFontCs, setResolvedFontCs] = createSignal<string | undefined>(undefined);
    const [fontSource, setFontSource] = createSignal<BridgeFontSources | undefined>(undefined);
    const [slotFormats, setSlotFormats] = createSignal<BridgeSlotFormats | undefined>(undefined);
    const [caretFontSlot, setCaretFontSlot] = createSignal<FontSlot | undefined>(undefined);
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
    const [renderer, setRenderer] = createSignal(engine.renderer);
    const [sectionGeometry, setSectionGeometry] =
        createSignal<BridgeSectionGeometry | undefined>(undefined);
    const [cellProperties, setCellProperties] =
        createSignal<BridgeCellProperties | undefined>(undefined);
    const [tabStops, setTabStops] = createSignal<BridgeTabStop[]>([]);
    const ZERO_INDENT: BridgeIndent = { start_pt: 0, end_pt: 0, first_line_pt: 0 };
    const [paragraphIndent, setParagraphIndent] = createSignal<BridgeIndent>(ZERO_INDENT);
    const [isTrackingChanges, setIsTrackingChanges] = createSignal(false);
    const [protection, setProtection] = createSignal<ProtectionMode | undefined>(undefined);
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
                /* Issue #423 — an engine predating the per-slot read-back
                   sends empty names (serde default); surface those as
                   "unknown", not as a family called "". */
                setResolvedFontLatin(evt.resolved_font_latin || undefined);
                setResolvedFontCs(evt.resolved_font_cs || undefined);
                setFontSource(evt.font_source);
                setSlotFormats(evt.slot_formats);
                setCaretFontSlot(evt.caret_font_slot);
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
                setProtection(evt.protection);
                setParagraphBorders(evt.paragraph_borders);
                setEditingStory(evt.editing_story);
                setFieldCodeView(evt.field_code_view);
                setFieldAtCaret(evt.field_at_caret);
                /* Issue #387 — last, so a reveal effect reads the new caret. */
                if (evt.reveal_caret === true) setCaretRevealSeq((n) => n + 1);
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
        caretRevealSeq,
        rects,
        attrsAtCaret,
        attrsMixed,
        resolvedFontLatin,
        resolvedFontCs,
        fontSource,
        slotFormats,
        caretFontSlot,
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
        pageGeometry: view.pageGeometry,
        renderer,
        sectionGeometry,
        cellProperties,
        tabStops,
        paragraphIndent,
        isTrackingChanges,
        protection,
        paragraphBorders,
        editingStory,
        fieldCodeView,
        fieldAtCaret,
        zoom: view.zoom,
        deviceScale: view.deviceScale,
        rendererDowngrade: view.rendererDowngrade,
        lastRecovery: view.lastRecovery,
        checkpointFailing: view.checkpointFailing,
        checkpointState: view.checkpointState,
        lastError: view.lastError,
        previousSession: view.previousSession,
        openWarnings: view.openWarnings,
        previousSessions: view.previousSessions,
        fontSubstitutions: view.fontSubstitutions,
        commentHighlights: view.commentHighlights,
    };
}
