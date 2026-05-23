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
    /* Mirror of every table in the document — extracted from the a11y
       delta stream so the Table panel can list/target tables by
       BlockPath::top(block_index). The current paragraph-flat
       LogicalPos can't address a cell, so PR 3b targets tables by
       block index rather than by caret position. */
    const [tables, setTables] = createSignal<A11yTable[]>([]);
    let nodes: A11yNode[] = [];
    let announced = false;

    const recomputeTables = (): void => {
        const out: A11yTable[] = [];
        for (const n of nodes) if (n.kind === 'TABLE') out.push(n);
        setTables(out);
    };

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
        tables,
    };
}

export type EngineStore = ReturnType<typeof createEngineStore>;
