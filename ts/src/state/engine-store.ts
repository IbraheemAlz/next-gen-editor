/* Phase 4 §9 — engine-derived UI state.
 *
 * The store mirrors engine selection events into Solid signals. It never
 * holds document content — only the geometry the engine reports (§9
 * invariant). Engine rects arrive in canvas device pixels; they are
 * converted to CSS pixels here so the DOM overlays land correctly on
 * high-DPI displays. */
import { createSignal } from 'solid-js';
import type { EngineClient } from '../engine/engine-client';
import type {
    A11yNode,
    A11yTable,
    Alignment,
    AttrsMixed,
    BlockPath,
    BridgeStoryRef,
    Direction,
    Event,
    LogicalPos,
    LogicalRange,
    Rect,
    SelectionKind,
    TextAttrs,
} from '../engine/types';
import { topPos } from '../engine/types';

/* ——— Shared page-geometry constants ———
 * ONE source of truth for the pointer path, the per-page overlays, and
 * the layout-config dispatches, so their math can never disagree. Page
 * tops derived from these are uniform-A4 approximations — exact
 * per-section geometry (landscape / custom page sizes) needs
 * engine-reported page tops and is tracked separately. */

/** A4 page height in layout pt — engine `PageGeometry::a4()`. */
export const PAGE_H_PT = 841.9;
/** Inter-page gap in layout pt — engine `render::scene::PAGE_GAP_PT`. */
export const PAGE_GAP_PT = 48;
/** Print → screen DPI conversion: 1 pt (engine) = 1/72 inch; 1 CSS px =
 *  1/96 inch; so `screen_dpi_scale = 96 / 72 = 4 / 3`. Multiplied into
 *  the engine's `device_pixel_ratio` so an A4 page renders at the same
 *  physical size as Word / Google Docs at 100% zoom. */
export const SCREEN_DPI_SCALE = 4 / 3;
/** Exact page height in CSS px (841.9 × 4/3 = 1122.533…). NOT the rounded
 *  1123 — a 0.467 px/page error compounds to over half a line by page 30. */
export const PAGE_H_CSS = PAGE_H_PT * SCREEN_DPI_SCALE;
/** Inter-page gap in CSS px (48 × 4/3 = 64) — matches the `.editor-pages` gap. */
export const PAGE_GAP_CSS = PAGE_GAP_PT * SCREEN_DPI_SCALE;

/* Issue #26 — engine-reported per-page geometry (device px, from the
 * `Painted` event). The signal drives the reactive overlays; the
 * module-level mirror lets the non-reactive pointer path read the
 * latest tops at event time without threading the store through
 * `attachPointer`. */
export interface PageGeometry {
    tops: number[];
    heights: number[];
}
let latestPageTops: number[] | null = null;

/** Device-px top of page `idx` from the last paginated paint, or `null`
 *  before one lands (callers fall back to the uniform constants). */
export function enginePageTopDevice(idx: number): number | null {
    return latestPageTops?.[idx] ?? null;
}

/* Phase 3 (#39) — per-page margins + heights (device px) mirrored for
 * the non-reactive pointer path's header/footer zone gate, and the
 * active story ref for its already-in-this-story exemption. Margins
 * ride every `Painted` (real + synthetic), so the gate stays exact for
 * mixed-geometry sections without a mid-gesture worker round-trip. */
let latestPageMargins: { tops: number[]; bottoms: number[] } | null = null;
let latestPageHeights: number[] | null = null;

const [editingStorySig, setEditingStorySig] = createSignal<
    BridgeStoryRef | undefined
>(undefined);

/** The active header/footer story (reactive) — also read at event time
 *  by the pointer path. */
export function editingStoryForPointer(): BridgeStoryRef | undefined {
    return editingStorySig();
}

/**
 * Which margin band a PAGE-LOCAL device-px Y lands in on page `idx`,
 * or `null` for the content area / before the first paginated paint
 * (no gate — pre-paint clicks keep the legacy caret behaviour, the
 * same fallback-then-exact duality the selection overlays use).
 */
export function headerFooterZoneAt(
    pageIdx: number,
    localDeviceY: number,
): 'Header' | 'Footer' | null {
    const top = latestPageMargins?.tops[pageIdx];
    const bottom = latestPageMargins?.bottoms[pageIdx];
    const height = latestPageHeights?.[pageIdx];
    if (top === undefined || bottom === undefined || height === undefined) {
        return null;
    }
    if (localDeviceY >= 0 && localDeviceY < top) return 'Header';
    if (localDeviceY > height - bottom && localDeviceY <= height) return 'Footer';
    return null;
}

/* Issue #36 — same module-level-mirror pattern for the selection view:
 * the pointer path decides on right-click whether the press lands inside
 * the active selection (preserve it for the context menu) or outside
 * (move the caret, Word semantics) without threading the store through
 * `attachPointer`. Rects are CSS px, document-absolute. */
let latestSelectionView: { rects: Rect[]; kind: SelectionKind } = {
    rects: [],
    kind: { kind: 'LINEAR' },
};

/** Latest selection rects + kind from `SELECTION_CHANGED`, for the
 *  non-reactive pointer path. */
export function selectionViewForPointer(): { rects: Rect[]; kind: SelectionKind } {
    return latestSelectionView;
}

/* Issue #44 — inline-image geometry + selection. `imageRectsSig` holds
 * every image's CSS-px rect + resize address (refreshed after each paint
 * via GET_IMAGE_RECTS); `selectedImageSig` is the image the user clicked,
 * driving the resize-handle overlay. Both are module-level Solid signals
 * (one editor instance) so the non-reactive pointer path can select an
 * image on pointerdown and the reactive overlay can render its handles. */
export interface ImageRectCss {
    path: BlockPath;
    at: number;
    relId: string;
    rect: Rect; // CSS px, document-absolute
    widthEmu: number;
    heightEmu: number;
}
export interface ImageAddr {
    path: BlockPath;
    at: number;
}
const [imageRectsSig, setImageRectsSig] = createSignal<ImageRectCss[]>([]);
const [selectedImageSig, setSelectedImageSig] = createSignal<ImageAddr | null>(null);

/** Latest image rects (CSS px) for the non-reactive pointer path. */
export function imageRectsForPointer(): ImageRectCss[] {
    return imageRectsSig();
}

const sameAddr = (a: ImageAddr, b: ImageAddr): boolean =>
    a.at === b.at &&
    a.path.steps.length === b.path.steps.length &&
    a.path.steps.every((s, i) => {
        const o = b.path.steps[i]!;
        if (s.kind !== o.kind) return false;
        if (s.kind === 'BLOCK' && o.kind === 'BLOCK') return s.idx === o.idx;
        if (s.kind === 'CELL' && o.kind === 'CELL')
            return s.row === o.row && s.col === o.col;
        return false;
    });

/** Select the image under a document-absolute CSS point, if any. Returns
 *  true when an image was hit (and selected); false clears the selection.
 *  Called synchronously by the pointer path before it places a caret. */
export function selectImageByPoint(cssX: number, cssY: number): boolean {
    const hit = imageRectsSig().find(
        (im) =>
            cssX >= im.rect.x &&
            cssX <= im.rect.x + im.rect.w &&
            cssY >= im.rect.y &&
            cssY <= im.rect.y + im.rect.h,
    );
    if (hit) {
        setSelectedImageSig({ path: hit.path, at: hit.at });
        return true;
    }
    setSelectedImageSig(null);
    return false;
}

/** Clear any selected-image state (e.g. on a caret-placing click). */
export function clearSelectedImage(): void {
    setSelectedImageSig(null);
}

/** The current selection: its logical range plus rendered rectangles. */
export interface SelectionView {
    range: LogicalRange;
    rects: Rect[];
}

/** Undo/redo availability — drives the toolbar's Undo/Redo buttons. */
export interface UndoState {
    canUndo: boolean;
    canRedo: boolean;
}

const EMPTY_RANGE: LogicalRange = {
    start: topPos(0, 0),
    end: topPos(0, 0),
};

/** Device-pixel rect → CSS-pixel rect. */
function toCssRect(r: Rect, dpr: number): Rect {
    return { x: r.x / dpr, y: r.y / dpr, w: r.w / dpr, h: r.h / dpr };
}

/**
 * Subscribe to the engine and expose its selection state as Solid signals.
 * Created once by `App` and passed to the overlays.
 */
export function createEngineStore(client: EngineClient) {
    const [caret, setCaret] = createSignal<Rect | null>(null);
    const [caretLogical, setCaretLogical] = createSignal<LogicalPos | null>(null);
    const [selection, setSelection] = createSignal<SelectionView>({
        range: EMPTY_RANGE,
        rects: [],
    });
    const [attrsAtCaret, setAttrsAtCaret] = createSignal<TextAttrs | null>(null);
    /* Phase 9b — per-flag "mixed across the selection" bitmap. The
       toolbar reads it to switch each B/I/U/S button between
       OFF/ON/INDETERMINATE so a selection straddling a styled boundary
       doesn't mis-render the start position's value as the whole
       selection's. Default to all-false on no selection. */
    const [attrsMixed, setAttrsMixed] = createSignal<AttrsMixed>({
        bold: false,
        italic: false,
        underline: false,
        strike: false,
    });
    const [undoState, setUndoState] = createSignal<UndoState>({
        canUndo: false,
        canRedo: false,
    });
    /* Effective alignment of the caret's paragraph + the document base
       direction — together they drive the toolbar's alignment picker. */
    const [paragraphAlignment, setParagraphAlignment] = createSignal<Alignment>('Start');
    const [baseDirection, setBaseDirection] = createSignal<Direction>('Ltr');
    /* Phase 9c — effective paragraph base direction (`<w:bidi>`),
       tri-state: `Ltr` / `Rtl` when every paragraph in the range
       agrees, `null` when they disagree → toolbar's LTR/RTL buttons
       render INDETERMINATE. Distinct from `baseDirection`, which is
       the document-level direction for the alignment picker; this
       reflects the SELECTION's paragraph direction. */
    const [paragraphDirection, setParagraphDirection] = createSignal<Direction | null>('Ltr');
    /* Issue #42 — the caret paragraph's `<w:numPr><w:ilvl>`, or `undefined`
       outside any list. Drives the Tab-key handler's demote/promote
       branch in `HiddenInput`. */
    const [listIlvl, setListIlvl] = createSignal<number | undefined>(undefined);
    const [announcement, setAnnouncement] = createSignal('');
    /* Sprint 10 — separate `aria-live="assertive"` channel. Engine
       emits this priority only for errors / blocked actions; the UI
       interrupts the screen reader instead of waiting for a pause. */
    const [assertiveAnnouncement, setAssertiveAnnouncement] = createSignal('');
    /* Issue #48 — UI-side (non-engine) failure channel. Browser-API
       failures (e.g. a clipboard write blocked by lost focus or
       permissions) never produce an `Event::Error` — the engine was
       never involved — so the shell needs its own signal for the
       visible error banner. Setting a message also re-emits it on the
       assertive live region (mirroring the `Event::Error` handling
       below) so screen readers hear it; clearing (`null`) is silent. */
    /* `equals: false` so a REPEATED identical failure still re-arms the
       banner's 6 s dismiss timer (createSignal's default === comparator
       would swallow the second set as a no-op). No assertive live-region
       mirror here: the visible banner itself carries role="alert", so a
       second pipe would announce every error twice to screen readers. */
    const [uiError, setUiError] = createSignal<string | null>(null, {
        equals: false,
    });
    /* Mirror of every table in the document — extracted from the a11y
       delta stream so the Table panel can list/target tables by
       BlockPath::top(block_index). The current paragraph-flat
       LogicalPos can't address a cell, so PR 3b targets tables by
       block index rather than by caret position. */
    const [tables, setTables] = createSignal<A11yTable[]>([]);
    /* PR 4 — selection flavour. Linear by default; `TableCells` when the
       drag stayed inside a single table and covered multiple cells. */
    const [selectionKind, setSelectionKind] = createSignal<SelectionKind>({ kind: 'LINEAR' });
    /* Phase 6b — paginated layout reach. `documentHeight` is the engine's
       total Y span (every page + the inter-page gap) in device px; the
       canvas mount divides by `devicePixelRatio` to set its CSS height
       so the browser scrollbar exposes every page. `pageCount` drives
       the document-info UI strip. */
    const [documentHeight, setDocumentHeight] = createSignal(0);
    /* Issue #51 — virtual scroll range. With the viewport cull actually
       firing, `documentHeight` covers only the laid-out band; the
       engine's per-paint estimate (real band + avg-height fudge per
       unlaid block) is what the scroller must expose, or the user can
       never scroll INTO the unlaid tail to trigger EXPAND_LAYOUT. */
    const [estimatedDocumentHeight, setEstimatedDocumentHeight] = createSignal(0);
    const [pageCount, setPageCount] = createSignal(1);
    /* Audit gap C.H1 — `true` once the lazy paginator has laid out every
       block; gates the shell's scroll-driven EXPAND_LAYOUT dispatches
       (an expand past a fully-laid-out tail would repaint for nothing). */
    const [isFullLayout, setIsFullLayout] = createSignal(false);
    const [pageGeometry, setPageGeometry] = createSignal<PageGeometry | null>(null);
    /* Phase 3 (#39) — reactive per-page margins (device px) for the
       StoryModeOverlay's dim + band-outline geometry. */
    const [marginGeometry, setMarginGeometry] = createSignal<{
        tops: number[];
        bottoms: number[];
    } | null>(null);
    let nodes: A11yNode[] = [];
    let announced = false;

    const recomputeTables = (): void => {
        const out: A11yTable[] = [];
        for (const n of nodes) if (n.kind === 'TABLE') out.push(n);
        setTables(out);
    };

    /* Issue #44 — query image geometry and mirror it (device px → CSS
       px) into the module-level signal the overlay + pointer path read.
       If the currently-selected image vanished (deleted / undo), drop the
       selection so stale handles never linger. */
    const refreshImageRects = async (): Promise<void> => {
        try {
            const evt = await client.dispatch({ type: 'GET_IMAGE_RECTS' });
            if (evt.type !== 'IMAGE_RECTS') return;
            const dpr = window.devicePixelRatio || 1;
            const rects: ImageRectCss[] = evt.images.map((im) => ({
                path: im.path,
                at: im.at,
                relId: im.rel_id,
                rect: toCssRect(im.rect, dpr),
                widthEmu: im.width_emu,
                heightEmu: im.height_emu,
            }));
            setImageRectsSig(rects);
            const sel = selectedImageSig();
            if (sel && !rects.some((r) => sameAddr({ path: r.path, at: r.at }, sel))) {
                setSelectedImageSig(null);
            }
        } catch (e) {
            console.error('refreshImageRects failed', e);
        }
    };

    client.subscribe((ev: Event) => {
        if (ev.type === 'SELECTION_CHANGED') {
            const dpr = window.devicePixelRatio || 1;
            const cssRects = ev.rects.map((r) => toCssRect(r, dpr));
            latestSelectionView = { rects: cssRects, kind: ev.selection_kind };
            setCaret(toCssRect(ev.caret, dpr));
            setCaretLogical(ev.range.end);
            setSelection({
                range: ev.range,
                rects: cssRects,
            });
            setAttrsAtCaret(ev.attrs_at_caret);
            setAttrsMixed(ev.attrs_mixed);
            setParagraphDirection(ev.paragraph_direction ?? null);
            setUndoState({ canUndo: ev.can_undo, canRedo: ev.can_redo });
            setParagraphAlignment(ev.paragraph_alignment);
            setBaseDirection(ev.direction);
            setSelectionKind(ev.selection_kind);
            setListIlvl(ev.list_ilvl);
            setEditingStorySig(ev.editing_story);
        } else if (ev.type === 'PAINTED') {
            /* Phase 6b — paginator reach. The engine emits
               `document_height` (device px) and `page_count` on every
               paint; mirror them into signals so `EditorCanvas` resizes
               itself to fit every page and the scrollbar exposes
               them. Edits that grow / shrink the page count flow
               through here. */
            setDocumentHeight(ev.document_height);
            setEstimatedDocumentHeight(ev.estimated_document_height);
            setPageCount(ev.page_count);
            setIsFullLayout(ev.is_full_layout);
            /* Issue #26 — harness paths bypass pagination and emit empty
               arrays; keep the last real geometry in that case. */
            if (ev.page_tops.length > 0) {
                latestPageTops = ev.page_tops;
                latestPageHeights = ev.page_heights;
                setPageGeometry({ tops: ev.page_tops, heights: ev.page_heights });
            }
            /* Phase 3 (#39) — margin zones for the pointer gate + the
               story overlay. */
            if (ev.page_margin_tops.length > 0) {
                latestPageMargins = {
                    tops: ev.page_margin_tops,
                    bottoms: ev.page_margin_bottoms,
                };
                setMarginGeometry({
                    tops: ev.page_margin_tops,
                    bottoms: ev.page_margin_bottoms,
                });
            }
            /* Issue #44 — refresh inline-image geometry after a paint so
               the resize-handle overlay tracks images through edits,
               scroll (lazy layout), and resizes. Pure query — no repaint,
               so no feedback loop. Gated on `image_count` so an image-free
               document (the common case) never pays the extra worker
               round-trip; the `imageRects` check still fires the final
               clearing refresh when the last image was just removed. */
            if (ev.image_count > 0 || imageRectsSig().length > 0) {
                void refreshImageRects();
            }
        } else if (ev.type === 'ACCESSIBILITY_TREE_DELTA') {
            /* Mirror the patch stream into the local `nodes` array so the
               Table panel sees the same view the reconciler renders. */
            for (const patch of ev.patches) {
                if (patch.type === 'REPLACE') {
                    nodes = [...patch.tree.nodes];
                } else if (patch.type === 'UPDATE') {
                    nodes[patch.index] = patch.node;
                } else if (patch.type === 'INSERT') {
                    nodes.splice(patch.index, 0, patch.node);
                } else if (patch.type === 'REMOVE') {
                    nodes.splice(patch.index, 1);
                }
            }
            recomputeTables();
            /* The first delta from a fresh engine is a single REPLACE; use its
               node count for the one-time "document loaded" announcement. */
            if (!announced) {
                announced = true;
                const first = ev.patches[0];
                const n = first && first.type === 'REPLACE' ? first.tree.nodes.length : 0;
                setAnnouncement(`Document loaded — ${n} block${n === 1 ? '' : 's'}.`);
            }
        } else if (ev.type === 'ANNOUNCEMENT') {
            /* Sprint 10 — engine-side announcement (priority + message
               computed by the engine; the UI just routes to the right
               aria-live region). Re-emit on every event so a duplicate
               message still narrates (the screen reader otherwise
               skips identical successive content). */
            if (ev.priority === 'Assertive') {
                setAssertiveAnnouncement('');
                queueMicrotask(() => setAssertiveAnnouncement(ev.message));
            } else {
                setAnnouncement('');
                queueMicrotask(() => setAnnouncement(ev.message));
            }
        } else if (ev.type === 'ERROR') {
            /* Sprint 10 — errors interrupt; the engine doesn't emit an
               explicit Assertive announcement for these (Event::Error
               is the primary reply, not a broadcast), so we synthesise
               one client-side. */
            setAssertiveAnnouncement('');
            queueMicrotask(() => setAssertiveAnnouncement(`Error: ${ev.message}`));
        }
    });

    return {
        caret,
        caretLogical,
        selection,
        attrsAtCaret,
        attrsMixed,
        undoState,
        paragraphAlignment,
        baseDirection,
        paragraphDirection,
        listIlvl,
        announcement,
        assertiveAnnouncement,
        uiError,
        setUiError,
        tables,
        selectionKind,
        documentHeight,
        estimatedDocumentHeight,
        pageCount,
        isFullLayout,
        pageGeometry,
        /* Issue #44 — inline-image rects + selection for the resize
           overlay. `imageRects` are CSS px, document-absolute. */
        imageRects: imageRectsSig,
        selectedImage: selectedImageSig,
        setSelectedImage: setSelectedImageSig,
        refreshImageRects,
        /* Phase 3 (#39) — active story for overlays + control gating. */
        editingStory: editingStorySig,
        marginGeometry,
        /** CSS-px top of page `idx` — engine-exact once a paginated paint
         *  reported geometry; uniform-A4 fallback before that. */
        pageTopCss: (idx: number): number => {
            const top = pageGeometry()?.tops[idx];
            return top !== undefined
                ? top / (window.devicePixelRatio || 1)
                : idx * (PAGE_H_CSS + PAGE_GAP_CSS);
        },
        /** CSS-px height of page `idx` — same fallback rules. */
        pageHeightCss: (idx: number): number => {
            const h = pageGeometry()?.heights[idx];
            return h !== undefined ? h / (window.devicePixelRatio || 1) : PAGE_H_CSS;
        },
    };
}

export type EngineStore = ReturnType<typeof createEngineStore>;
