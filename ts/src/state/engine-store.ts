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
    Alignment,
    Direction,
    Event,
    LogicalPos,
    LogicalRange,
    Rect,
    TextAttrs,
} from '../engine/types';

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
    start: { para: 0, offset: 0 },
    end: { para: 0, offset: 0 },
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
    const [undoState, setUndoState] = createSignal<UndoState>({
        canUndo: false,
        canRedo: false,
    });
    /* Effective alignment of the caret's paragraph + the document base
       direction — together they drive the toolbar's alignment picker. */
    const [paragraphAlignment, setParagraphAlignment] = createSignal<Alignment>('Start');
    const [baseDirection, setBaseDirection] = createSignal<Direction>('Ltr');
    const [announcement, setAnnouncement] = createSignal('');
    let announced = false;

    client.subscribe((ev: Event) => {
        if (ev.type === 'SELECTION_CHANGED') {
            const dpr = window.devicePixelRatio || 1;
            setCaret(toCssRect(ev.caret, dpr));
            setCaretLogical(ev.range.end);
            setSelection({
                range: ev.range,
                rects: ev.rects.map((r) => toCssRect(r, dpr)),
            });
            setAttrsAtCaret(ev.attrs_at_caret);
            setUndoState({ canUndo: ev.can_undo, canRedo: ev.can_redo });
            setParagraphAlignment(ev.paragraph_alignment);
            setBaseDirection(ev.direction);
        } else if (ev.type === 'ACCESSIBILITY_TREE_DELTA') {
            /* The first delta from a fresh engine is a single REPLACE; use its
               paragraph count for the one-time "document loaded" announcement.
               The reconciler in AccessibilityTree consumes the patches. */
            if (!announced) {
                announced = true;
                const first = ev.patches[0];
                const n = first && first.type === 'REPLACE' ? first.tree.paragraphs.length : 0;
                setAnnouncement(`Document loaded — ${n} paragraph${n === 1 ? '' : 's'}.`);
            }
        }
    });

    return {
        caret,
        caretLogical,
        selection,
        attrsAtCaret,
        undoState,
        paragraphAlignment,
        baseDirection,
        announcement,
    };
}

export type EngineStore = ReturnType<typeof createEngineStore>;
