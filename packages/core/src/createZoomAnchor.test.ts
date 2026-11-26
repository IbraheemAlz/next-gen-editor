/* Issue #312 - the zoom-anchor controller: `cmd.setZoom` remembers what the
 * user is looking at BEFORE the page cards resize, the zoom's repaint puts
 * it back at the same viewport offset, and no caret reveal fires on a zoom.
 *
 * The DOM is modelled, not mounted (the vitest harness has no DOM): a fake
 * viewport (`scrollTop` / `scrollHeight` / `clientHeight`) and a page-card
 * layout that follows `domZoom`, which the test advances the way the shell
 * does - on the zoom's `PAINTED`, one task after the `SELECTION_CHANGED`
 * reply carried the new zoom. `useEngine` is mocked (`EngineProvider.tsx`
 * is JSX the node-side harness does not compile). */
import { createRoot } from 'solid-js';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { Event } from './types';
import type { PageSpan } from './zoomAnchor';

interface FakeEngine {
    renderer: string;
    listeners: Array<(e: Event) => void>;
    subscribe(fn: (e: Event) => void): () => void;
    dispatch: ReturnType<typeof vi.fn>;
}

const current: { engine: FakeEngine } = { engine: undefined as unknown as FakeEngine };
vi.mock('./EngineProvider', () => ({
    useEngine: () => current.engine,
    useDocumentDefaults: () => undefined,
}));

const newEngine = (): FakeEngine => {
    const engine: FakeEngine = {
        renderer: 'canvas2d',
        listeners: [],
        subscribe(fn) {
            engine.listeners.push(fn);
            return () => {};
        },
        dispatch: vi.fn(async () => ({ type: 'OK' }) as unknown as Event),
    };
    return engine;
};

const emit = (e: unknown): void => {
    for (const l of current.engine.listeners) l(e as Event);
};

const DESK = 40;
const GAP_100 = 64;
const A4 = 1123;
const N_PAGES = 6;
const CLIENT_H = 700;

/** The page cards at `zoom` (what the shell's DOM holds once painted). */
function cards(zoom: number): PageSpan[] {
    const spans: PageSpan[] = [];
    let top = DESK;
    for (let index = 0; index < N_PAGES; index++) {
        const height = Math.ceil(A4 * zoom);
        spans.push({ index, top, height });
        top += height + GAP_100 * zoom;
    }
    return spans;
}

interface HostModel {
    domZoom: number;
    viewport: { scrollTop: number; clientHeight: number; readonly scrollHeight: number };
    y(page: number, offset100: number, zoom?: number): number;
    caret(page: number, offset100: number, zoom: number): { x: number; y: number; w: number; h: number };
}

/** A modelled host: a scrolled viewport over `cards(domZoom)`. */
function host(): HostModel {
    const model: HostModel = {
        domZoom: 1,
        viewport: {
            scrollTop: 0,
            clientHeight: CLIENT_H,
            get scrollHeight(): number {
                const last = cards(model.domZoom).at(-1)!;
                return last.top + last.height + DESK;
            },
        },
        /** scroll-content y of a document point (page + offset at 100 %). */
        y(page: number, offset100: number, zoom = model.domZoom): number {
            return cards(zoom)[page]!.top + offset100 * zoom;
        },
        /** The engine's caret rect for that point (device px = css px here). */
        caret(page: number, offset100: number, zoom: number) {
            return { x: 0, y: model.y(page, offset100, zoom), w: 1, h: 20 * zoom };
        },
    };
    return model;
}

const POS = { path: { steps: [{ kind: 'BLOCK', idx: 0 }] }, offset: 0 };
const selectionChanged = (zoom: number, caret: unknown, extra: Record<string, unknown> = {}) => ({
    type: 'SELECTION_CHANGED',
    range: { start: POS, end: POS },
    caret,
    rects: [],
    tab_stops: [],
    can_undo: false,
    can_redo: false,
    is_tracking_changes: false,
    field_code_view: false,
    document_revision: 1,
    zoom,
    ...extra,
});
/* A zoom repaint does not touch the document, so `PAINTED.version` stays
   put (it follows document mutations): only the page geometry it reports
   is new - exactly what the controller keys on. */
const painted = (_nth: number) => ({
    type: 'PAINTED',
    version: 1,
    paint_ms: 1,
    page_tops: [0],
    page_heights: [A4],
    page_widths: [800],
    estimated_document_height: 0,
});

/** Let the microtask (and the rAF / timer passes) the controller queued run. */
const flush = async (): Promise<void> => {
    await Promise.resolve();
    await new Promise((resolve) => setTimeout(resolve, 20));
};

beforeEach(() => {
    current.engine = newEngine();
    vi.stubGlobal('requestAnimationFrame', (cb: () => void) => setTimeout(cb, 0));
    vi.stubGlobal('cancelAnimationFrame', (id: number) => clearTimeout(id));
});
afterEach(() => {
    vi.unstubAllGlobals();
});

/** Mount the anchor over `h` and return the facade + state. */
async function mount(h: HostModel, opts: { anchor?: boolean } = {}) {
    const { createEditorCommands } = await import('./createEditorCommands');
    const { createEditorState } = await import('./createEditorState');
    const { createZoomAnchor } = await import('./createZoomAnchor');
    let disposeRoot: () => void = () => {};
    const api = createRoot((dispose) => {
        disposeRoot = dispose;
        const state = createEditorState();
        if (opts.anchor !== false) {
            createZoomAnchor({
                viewport: () => h.viewport as unknown as HTMLElement,
                caretBox: (caret) => ({ top: caret.y, left: 0, width: caret.w, height: caret.h }),
                pages: () => cards(h.domZoom),
            });
        }
        return { state, cmd: createEditorCommands() };
    });
    return { ...api, dispose: disposeRoot };
}

/** The shell's behaviour on a zoom: the reply (new zoom + caret rect), then
 *  the repaint resizes the cards and a `PAINTED` follows. */
async function applyZoom(
    h: HostModel,
    zoom: number,
    caretAt: { page: number; offset100: number },
    version: number,
): Promise<void> {
    emit(selectionChanged(zoom, h.caret(caretAt.page, caretAt.offset100, zoom)));
    h.domZoom = zoom;
    emit(painted(version));
    await flush();
}

describe('createZoomAnchor + cmd.setZoom (#312)', () => {
    it('keeps the caret at the same viewport offset through 100 -> 150 -> 75', async () => {
        const h = host();
        const { cmd, dispose } = await mount(h);
        /* A caret on page 2, mid-viewport. */
        const at = { page: 2, offset100: 400 };
        h.viewport.scrollTop = h.y(2, 400) - 300;
        emit(selectionChanged(1, h.caret(2, 400, 1)));

        for (const [zoom, version] of [
            [1.5, 2],
            [0.75, 3],
        ] as const) {
            const before = h.y(2, 400) - h.viewport.scrollTop;
            await cmd.setZoom(zoom);
            expect(current.engine.dispatch).toHaveBeenLastCalledWith(
                { type: 'SET_ZOOM', scale: zoom },
                [],
            );
            /* Nothing moves before the repaint. */
            expect(h.viewport.scrollTop).toBe(h.y(2, 400) - before);
            await applyZoom(h, zoom, at, version);
            expect(Math.abs(h.y(2, 400) - h.viewport.scrollTop - before)).toBeLessThan(1);
        }
        dispose();
    });

    it('anchors on the viewport centre when the caret is out of view', async () => {
        const h = host();
        const { cmd, dispose } = await mount(h);
        /* The caret is back on page 0; the user scrolled to page 3. */
        emit(selectionChanged(1, h.caret(0, 100, 1)));
        const centre = h.y(3, 250);
        h.viewport.scrollTop = centre - CLIENT_H / 2;

        await cmd.setZoom(1.5);
        await applyZoom(h, 1.5, { page: 0, offset100: 100 }, 2);
        /* The page-3 point that was at the centre is still there. */
        const centreAfter = h.y(3, 250) - h.viewport.scrollTop;
        expect(Math.abs(centreAfter - CLIENT_H / 2)).toBeLessThan(1);
        dispose();
    });

    it('does nothing for a zoom to the current value, or before the zoom lands', async () => {
        const h = host();
        const { cmd, dispose } = await mount(h);
        emit(selectionChanged(1, h.caret(2, 400, 1)));
        h.viewport.scrollTop = h.y(2, 400) - 300;
        const start = h.viewport.scrollTop;

        await cmd.setZoom(1);
        emit(painted(2));
        await flush();
        expect(h.viewport.scrollTop).toBe(start);

        /* A repaint of an EARLIER command lands before the zoom's reply:
           the zoom is still 1, so it is not the zoom's paint. */
        await cmd.setZoom(1.5);
        emit(painted(3));
        await flush();
        expect(h.viewport.scrollTop).toBe(start);
        dispose();
    });

    it('a second zoom before the first lands keeps the first anchor', async () => {
        const h = host();
        const { cmd, dispose } = await mount(h);
        emit(selectionChanged(1, h.caret(2, 400, 1)));
        h.viewport.scrollTop = h.y(2, 400) - 300;

        await cmd.setZoom(1.25);
        h.viewport.scrollTop += 0; // the user has not seen 1.25 yet
        await cmd.setZoom(1.5);
        await applyZoom(h, 1.5, { page: 2, offset100: 400 }, 2);
        expect(Math.abs(h.y(2, 400) - h.viewport.scrollTop - 300)).toBeLessThan(1);
        dispose();
    });

    it('a chain of zooms (Fit width asks twice) shares one anchor through every paint', async () => {
        const h = host();
        const { cmd, dispose } = await mount(h);
        emit(selectionChanged(1, h.caret(2, 400, 1)));
        h.viewport.scrollTop = h.y(2, 400) - 300;
        const at = { page: 2, offset100: 400 };

        /* Both requests leave before the first reply (what the shell's Fit
           mode does: a second fit lands while the first zoom is in flight). */
        await cmd.setZoom(1.12);
        await cmd.setZoom(1.26);
        await applyZoom(h, 1.12, at, 2);
        expect(Math.abs(h.y(2, 400) - h.viewport.scrollTop - 300)).toBeLessThan(1);
        await applyZoom(h, 1.26, at, 3);
        expect(Math.abs(h.y(2, 400) - h.viewport.scrollTop - 300)).toBeLessThan(1);
        dispose();
    });

    it('a later, unrelated repaint does not move the view again', async () => {
        const h = host();
        const { cmd, dispose } = await mount(h);
        emit(selectionChanged(1, h.caret(2, 400, 1)));
        h.viewport.scrollTop = h.y(2, 400) - 300;
        await cmd.setZoom(1.5);
        await applyZoom(h, 1.5, { page: 2, offset100: 400 }, 2);
        /* The user scrolls on; a later paint at the same zoom is not ours. */
        h.viewport.scrollTop += 500;
        const scrolled = h.viewport.scrollTop;
        emit(painted(3));
        await flush();
        expect(h.viewport.scrollTop).toBe(scrolled);
        dispose();
    });

    it('is inert without a mounted host, and after it is disposed', async () => {
        const h = host();
        const bare = await mount(h, { anchor: false });
        h.viewport.scrollTop = 500;
        await bare.cmd.setZoom(1.5);
        await applyZoom(h, 1.5, { page: 0, offset100: 0 }, 2);
        expect(h.viewport.scrollTop).toBe(500);
        expect(current.engine.dispatch).toHaveBeenCalledTimes(1);
        bare.dispose();

        const h2 = host();
        current.engine = newEngine();
        const m = await mount(h2);
        m.dispose();
        h2.viewport.scrollTop = 500;
        await m.cmd.setZoom(1.5);
        await applyZoom(h2, 1.5, { page: 0, offset100: 0 }, 2);
        expect(h2.viewport.scrollTop).toBe(500);
    });

    it('a zoom never bumps caretRevealSeq: the caret rect changes, no reveal fires', async () => {
        const h = host();
        const { state, cmd, dispose } = await mount(h);
        emit(selectionChanged(1, h.caret(2, 400, 1), { reveal_caret: false }));
        expect(state.caretRevealSeq()).toBe(0);
        await cmd.setZoom(1.5);
        /* The reply to SET_ZOOM carries the caret at the new scale and
           `reveal_caret` false (SET_ZOOM is a VIEW command). */
        emit(selectionChanged(1.5, h.caret(2, 400, 1.5), { reveal_caret: false }));
        expect(state.caretRevealSeq()).toBe(0);
        /* The same move WITH the flag (a keyboard step) does reveal. */
        emit(selectionChanged(1.5, h.caret(3, 10, 1.5), { reveal_caret: true }));
        expect(state.caretRevealSeq()).toBe(1);
        dispose();
    });
});
