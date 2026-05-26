/* Phase 4 §4 — top-level Solid application.
 *
 * Owns the EngineClient lifecycle: spawns the worker, seeds the first paint,
 * drives crash recovery, and mirrors the `window.__*` hooks the Phase 2
 * exit-gate e2e specs depend on. Document state lives in the engine (§9) —
 * App holds only UI signals. */
import { createSignal, For, onCleanup, onMount, Show } from 'solid-js';
import { EditorCanvas } from './components/EditorCanvas';
import { ExtraPageCanvas } from './components/ExtraPageCanvas';
import { CaretOverlay } from './components/CaretOverlay';
import { PageSelectionOverlay } from './components/PageSelectionOverlay';
import { HiddenInput } from './components/HiddenInput';
import { SdkShelf } from './sdk-bridge';
import { AccessibilityTree } from './components/AccessibilityTree';
import { Announcements } from './components/Announcements';
import { EngineClient } from './engine/engine-client';
import { createEngineStore } from './state/engine-store';
import { startTelemetry } from './state/telemetry';
import { attachDragDrop } from './input/dnd';
import type { Command, Event } from './engine/types';
import { topPos } from './engine/types';
import AMIRI_URL from '../fonts/Amiri-Regular.ttf?url';
import LIBERATION_URL from '../fonts/LiberationSans-Regular.ttf?url';
import NOTO_URL from '../fonts/NotoNaskhArabic-Regular.ttf?url';
import './styles/editor.css';
import './styles/caret.css';
import './styles/a11y.css';

/* Amiri is a dual-script Naskh face — renders mixed Arabic/English from a
   single face. The three families load under the ids the engine's font
   picker resolves against (Backlog #9). */
const AMIRI_ID = 'amiri';
const FONT_URLS: ReadonlyArray<readonly [string, string]> = [
    [AMIRI_ID, AMIRI_URL],
    ['liberation', LIBERATION_URL],
    ['noto-naskh', NOTO_URL],
];

/** Print → screen DPI conversion: 1 pt (engine) = 1/72 inch; 1 CSS px = 1/96
 *  inch; so `screen_dpi_scale = 96 / 72 = 4 / 3 ≈ 1.333`. Multiplied into
 *  the engine's `device_pixel_ratio` so an A4 page renders at the same
 *  physical size as Word / Google Docs at 100% zoom. Phase 6c bug-fix
 *  follow-up — pre-fix the 595 × 842 page rendered tiny at ~75% zoom. */
const SCREEN_DPI_SCALE = 4 / 3;

/** Load the editor fonts and seed a blank A4 page. Runs on boot + recovery. */
async function setupEngine(client: EngineClient): Promise<void> {
    /* loadFont hands each buffer to the worker as a Transferable — zero-copy. */
    for (const [id, url] of FONT_URLS) {
        const bytes = new Uint8Array(await (await fetch(url)).arrayBuffer());
        await client.loadFont(id, bytes);
    }
    await client.dispatch({
        type: 'RENDER_PAGE',
        /* Seed mixed Arabic/English so the pointer + selection overlays have
           BiDi text to hit-test against. */
        text: 'Hello world مرحبا بالعالم',
        font_id: AMIRI_ID,
        base_direction: 'RTL',
        px_size: 24,
        line_height: 36,
        align: 'START',
        /* Engine scale = `dpr × screen_dpi_scale`. The engine outputs
           print-perfect layout points (1 pt = 1/72 in) but CSS pixels are
           1/96 in — multiplying by `96/72 = 4/3` lifts the print page to
           its true screen size so an A4 sheet renders at 794 × 1123 CSS
           px (Google Docs / Word at 100% zoom), not the tiny 595 × 842
           CSS px the raw points would produce. Pointer hit-testing keeps
           working because `pointer.ts` converts CSS clicks to device px
           via the same `dpr`, and the canvas backing store ends up sized
           at `layout_pt × dpr × 4/3 = CSS px × dpr` — clicks land in the
           right coordinate space without any extra math. */
        device_pixel_ratio: window.devicePixelRatio * SCREEN_DPI_SCALE,
    });
    /* Seed a collapsed caret at the document start so the hidden input has a
       position to insert at before the first pointer click. */
    await client.dispatch({
        type: 'SET_SELECTION',
        range: { start: topPos(0, 0), end: topPos(0, 0) },
        caret: topPos(0, 0),
    });
}

/** D2.5: poll EngineStats every 5 s; log to console + a small debug div. */
function startStatsPolling(client: EngineClient): void {
    const debug = document.createElement('div');
    debug.id = 'stats';
    debug.style.cssText =
        'position:fixed;right:8px;bottom:8px;padding:6px 10px;z-index:10;' +
        'background:rgba(0,0,0,0.78);color:#5f5;border-radius:4px;' +
        'font:11px/1.5 ui-monospace,Menlo,Consolas,monospace;white-space:pre;';
    document.body.appendChild(debug);

    const poll = async (): Promise<void> => {
        try {
            const evt = await client.dispatch({ type: 'REQUEST_STATS' });
            if (evt.type !== 'STATS') return;
            const heapMiB = (evt.wasm_heap_bytes / (1024 * 1024)).toFixed(1);
            const line =
                `EngineStats — heap ${evt.wasm_heap_bytes} B (${heapMiB} MiB) · ` +
                `tree ${evt.document_tree_bytes} B · undo depth ${evt.undo_stack_depth} · ` +
                `fonts ${evt.fonts_resident} · glyph cache ${evt.glyph_cache_entries}`;
            console.log(`[stats] ${line}`);
            debug.textContent = line;
            window.__lastStats = evt;
        } catch (e: unknown) {
            console.warn('[stats] poll failed', e);
        }
    };

    void poll();
    setInterval(() => void poll(), 5000);
}

export function App() {
    /* Canvas generation — bumped on every crash so Solid remounts a fresh
       <canvas>; a consumed OffscreenCanvas cannot be re-transferred. */
    const [canvasGen, setCanvasGen] = createSignal(0);
    const [booting, setBooting] = createSignal(true);
    let firstReady = true;

    /* EngineClient.onTrap calls this after rejecting in-flight requests.
       Bumping the generation remounts EditorCanvas → fresh canvas → recover(). */
    const onCrash = (): void => {
        setBooting(true);
        setCanvasGen((g) => g + 1);
    };

    const client = new EngineClient('interactive', onCrash);
    const dispatch = (cmd: Command): Promise<Event> => client.dispatch(cmd);
    window.__engineClient = client;
    window.__dispatch = dispatch;

    /* §9 store — mirrors engine SELECTION_CHANGED events into signals the
       caret + selection overlays render from. */
    const store = createEngineStore(client);

    /* §10 D4.10 — drop a .docx anywhere on the page to load it. */
    onMount(() => onCleanup(attachDragDrop(client)));

    /* Runs once EditorCanvas has handed the engine its surface (init/recover). */
    const onReady = async (generation: number, initMs: number): Promise<void> => {
        if (generation === 0) {
            window.__bootMs = initMs;
            window.__engineReady = true;
            window.__renderer = client.renderer;
        }
        await setupEngine(client);
        setBooting(false);
        window.__paintIdle = true;
        if (generation > 0) {
            window.__recovered = true;
        }
        if (firstReady) {
            firstReady = false;
            startStatsPolling(client);
            /* D5.7 — mock telemetry pipeline: batches engine samples and
               console.logs them every 60 s (a real collector would POST). */
            startTelemetry(client);
        }
    };

    return (
        <div class="editor-shell">
            {/* SDK shelf — every UI surface ships through `@nge/ui`
                wired through `@nge/core`. The legacy Phase-4
                `Toolbar` + `TablePanel` were retired in the post-
                `v0.6.0-beta.2` purge (1109 LOC removed). */}
            <SdkShelf client={client} />
            <div class="editor-body">
                <div class="editor-viewport">
                    {/* Phase 6c multi-canvas DOM — one `.editor-page` per
                        paginated page. Page 0 hosts the boot canvas (the
                        engine's INIT surface + selection / caret overlays
                        anchor here). Pages 1+ are independent
                        `ExtraPageCanvas` elements that transfer a fresh
                        OffscreenCanvas to the worker via
                        `engine.set_page_canvas`. DevTools sees each
                        page as its own DOM node — no more single 30 k-px
                        canvas hitting Safari's 4 k limit. */}
                    <div class="editor-pages">
                        <div class="editor-page" data-page-index="0">
                            <For each={[canvasGen()]}>
                                {(generation) => (
                                    <EditorCanvas
                                        client={client}
                                        store={store}
                                        generation={generation}
                                        onReady={onReady}
                                    />
                                )}
                            </For>
                            <PageSelectionOverlay store={store} pageIdx={0} />
                            <CaretOverlay store={store} pageIdx={0} />
                            <HiddenInput client={client} store={store} />
                        </div>
                        <For each={Array.from({ length: Math.max(0, store.pageCount() - 1) }, (_, i) => i + 1)}>
                            {(idx) => (
                                <ExtraPageCanvas client={client} store={store} pageIdx={idx} />
                            )}
                        </For>
                    </div>
                </div>
            </div>
            <AccessibilityTree client={client} />
            <Announcements store={store} />
            <Show when={booting()}>
                <div class="boot-overlay">Loading editor…</div>
            </Show>
        </div>
    );
}
