/* Phase 4 §4 — top-level Solid application.
 *
 * Owns the EngineClient lifecycle: spawns the worker, seeds the first paint,
 * drives crash recovery, and mirrors the `window.__*` hooks the Phase 2
 * exit-gate e2e specs depend on. Document state lives in the engine (§9) —
 * App holds only UI signals. */
import { createEffect, createSignal, For, onCleanup, onMount, Show } from 'solid-js';
import { EditorCanvas } from './components/EditorCanvas';
import { ExtraPageCanvas } from './components/ExtraPageCanvas';
import { CaretOverlay } from './components/CaretOverlay';
import { PageSelectionOverlay } from './components/PageSelectionOverlay';
import { ImageHandlesOverlay } from './components/ImageHandlesOverlay';
import { StoryModeOverlay } from './components/StoryModeOverlay';
import { HiddenInput } from './components/HiddenInput';
import { SdkShelf } from './sdk-bridge';
import { AccessibilityTree } from './components/AccessibilityTree';
import { Announcements } from './components/Announcements';
import { EngineClient } from './engine/engine-client';
import { createEngineStore, SCREEN_DPI_SCALE, type EngineStore } from './state/engine-store';
import { startTelemetry } from './state/telemetry';
import { attachDragDrop } from './input/dnd';
import { createFontRegistry, type FontRegistry } from '@nge/core';
import type { Command, Event } from './engine/types';
import { topPos } from './engine/types';
import './styles/editor.css';
import './styles/caret.css';
import './styles/a11y.css';

/** Load the minimal default font set + seed a blank A4 page. Runs on boot
 *  AND crash recovery, so it must (re)establish fonts from scratch each
 *  time — `Command::Recover` wipes the engine's font map. */
async function setupEngine(client: EngineClient, fonts: FontRegistry): Promise<void> {
    /* Forget any resident-font flags (recovery wiped the engine's map),
       then push only the manifest's minimal boot set — one Latin + one
       dual-script Arabic. Every other declared font lazy-loads (JIT) the
       first time it is picked in the toolbar. */
    fonts.reset();
    await fonts.loadDefaults();
    /* `defaults[0]` is the manifest's seed/primary face (dual-script Amiri
       so the mixed RTL seed text shapes from one face). */
    const seedFont = fonts.defaults()[0] ?? 'amiri';
    const seedPaint = await client.dispatch({
        type: 'RENDER_PAGE',
        /* Seed mixed Arabic/English so the pointer + selection overlays have
           BiDi text to hit-test against. */
        text: 'Hello world مرحبا بالعالم',
        font_id: seedFont,
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
    /* Issue #54 — the boot paint's result event was previously
       discarded unchecked, so a failed first frame (`vello paint: …`)
       left a blank canvas with zero console evidence. */
    if (seedPaint.type === 'ERROR') {
        console.error('[boot] seed paint failed:', seedPaint.message);
    }
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

/** Issue #48 — transient visible banner for UI-side failures (e.g. a
 *  blocked clipboard write) that never reach the engine's `Event::Error`
 *  path. Auto-dismisses after 6 s (the FileMenu error-banner pattern);
 *  a newer error resets the timer. The matching assertive screen-reader
 *  announcement is piped by `store.setUiError` itself. */
function UiErrorBanner(props: { store: EngineStore }) {
    let timer: ReturnType<typeof setTimeout> | undefined;
    createEffect(() => {
        const msg = props.store.uiError();
        if (timer !== undefined) clearTimeout(timer);
        if (msg !== null) {
            timer = setTimeout(() => props.store.setUiError(null), 6000);
        }
    });
    onCleanup(() => {
        if (timer !== undefined) clearTimeout(timer);
    });
    return (
        <Show when={props.store.uiError()}>
            <div class="ui-error-banner" role="alert">
                {props.store.uiError()}
            </div>
        </Show>
    );
}

export function App() {
    /* Canvas generation — bumped on every crash so Solid remounts a fresh
       <canvas>; a consumed OffscreenCanvas cannot be re-transferred. */
    const [canvasGen, setCanvasGen] = createSignal(0);
    const [booting, setBooting] = createSignal(true);
    let firstReady = true;
    let viewportEl: HTMLDivElement | undefined;

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

    /* Issue #51 — the engine restarts its lazy-layout band at the top on
       a document swap; mirror it DOM-side, or the first SET_VIEWPORT
       after open would re-bump the band straight back to the OLD
       document's scroll depth and lay all of that out again. */
    createEffect(() => {
        const unsub = client.subscribe((evt) => {
            if (evt.type === 'DOCUMENT_LOADED' && viewportEl) {
                viewportEl.scrollTop = 0;
            }
        });
        onCleanup(unsub);
    });

    /* Data-driven font registry — single instance shared by the boot
       sequence (loadDefaults) and the toolbar (JIT ensureFont) so a font
       loaded by either path is cached once. Manifest: public/fonts.json. */
    const fontRegistry = createFontRegistry(client);
    window.__fontRegistry = fontRegistry;

    /* §9 store — mirrors engine SELECTION_CHANGED events into signals the
       caret + selection overlays render from. */
    const store = createEngineStore(client);

    /* §10 D4.10 — drop a .docx anywhere on the page to load it. */
    onMount(() => onCleanup(attachDragDrop(client)));

    /* Engine scale follows monitor / browser-zoom DPR changes. A
       `matchMedia('(resolution: …dppx)')` query fires exactly once when
       the DPR leaves the queried value, so the listener re-chains onto a
       fresh query after every change. `SET_DEVICE_SCALE` swaps the boot
       device scale while preserving the user zoom (`SET_ZOOM` owns that
       factor) and repaints WITHOUT resetting the document
       (re-dispatching `RENDER_PAGE` would wipe user content). Skipped
       while booting — recovery's `setupEngine` re-seeds the live DPR. */
    onMount(() => {
        let mq: MediaQueryList | undefined;
        const onDprChange = (): void => {
            rearm();
            if (!booting()) {
                void client.dispatch({
                    type: 'SET_DEVICE_SCALE',
                    scale: window.devicePixelRatio * SCREEN_DPI_SCALE,
                });
            }
        };
        const rearm = (): void => {
            mq?.removeEventListener('change', onDprChange);
            mq = window.matchMedia(`(resolution: ${window.devicePixelRatio}dppx)`);
            mq.addEventListener('change', onDprChange);
        };
        rearm();
        onCleanup(() => mq?.removeEventListener('change', onDprChange));
    });

    /* Runs once EditorCanvas has handed the engine its surface (init/recover). */
    const onReady = async (generation: number, initMs: number): Promise<void> => {
        if (generation === 0) {
            window.__bootMs = initMs;
            window.__engineReady = true;
            window.__renderer = client.renderer;
        }
        await setupEngine(client, fontRegistry);
        setBooting(false);
        /* Issue #54 — the boot paint presents while the opaque
           `.boot-overlay` still covers the canvas, and WebGPU may
           legitimately drop an occluded frame (wgpu documents
           Occluded/Timeout as "skip and try again"); nothing else
           repainted until the first mutation. Present one settle frame
           AFTER the overlay is gone, so `__paintIdle` means "a frame
           was presented unoccluded" on both renderers. */
        /* Timeout-raced: Chrome throttles rAF in hidden tabs, and boot
           must not stall until the tab is foregrounded. */
        await new Promise<void>((r) => {
            const fallback = setTimeout(r, 250);
            requestAnimationFrame(() => {
                clearTimeout(fallback);
                r();
            });
        });
        const settle = await client.dispatch({
            type: 'REQUEST_PAINT',
            viewport: { x: 0, y: 0, w: 0, h: 0 },
            dirty: undefined,
        });
        if (settle.type === 'ERROR') {
            console.error('[boot] settle repaint failed:', settle.message);
        }
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
        <>
            {/* SDK shelf — every UI surface ships through `@nge/ui`
                wired through `@nge/core`. The legacy Phase-4
                `Toolbar` + `TablePanel` were retired in the post-
                `v0.6.0-beta.2` purge (1109 LOC removed). The editor
                canvas mounts inside the shell's main grid track via
                the children slot; `engineReady` gates the right-rail
                snapshot calls so they don't fire before INIT. */}
            <SdkShelf
                client={client}
                fontRegistry={fontRegistry}
                engineReady={() => !booting()}
            >
                <div class="editor-viewport" ref={viewportEl}>
                    {/* Phase 6c multi-canvas DOM — one `.editor-page` per
                        paginated page. Page 0 hosts the boot canvas (the
                        engine's INIT surface + selection / caret overlays
                        anchor here). Pages 1+ are independent
                        `ExtraPageCanvas` elements that transfer a fresh
                        OffscreenCanvas to the worker via
                        `engine.set_page_canvas`. DevTools sees each
                        page as its own DOM node — no more single 30 k-px
                        canvas hitting Safari's 4 k limit. */}
                    {/* Issue #51 — virtual scroll range. Mounted pages
                        cover only the laid-out band; the min-height
                        spacer from the engine's running estimate lets
                        the user scroll INTO the unlaid tail, which is
                        what triggers the scroll-driven EXPAND_LAYOUT.
                        The estimate converges to the real height as
                        layout fills in (it can adjust in either
                        direction — AVG_BLOCK_HEIGHT_PT is a fudge).
                        Device px → CSS px via dpr. */}
                    <div
                        class="editor-pages"
                        style={{
                            'min-height': `${
                                Math.max(
                                    store.estimatedDocumentHeight(),
                                    store.documentHeight(),
                                ) / (window.devicePixelRatio || 1)
                            }px`,
                        }}
                    >
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
                            <ImageHandlesOverlay
                                store={store}
                                client={client}
                                pageIdx={0}
                            />
                            <StoryModeOverlay store={store} pageIdx={0} />
                            <HiddenInput client={client} store={store} />
                        </div>
                        {/* Extra pages live under a `booting` boundary: a
                            crash flips `booting` true, disposing them; the
                            completed recovery flips it back, remounting
                            fresh <canvas> elements whose surfaces
                            re-register with the RESPAWNED worker. The old
                            surfaces died with the trapped worker, and a
                            consumed OffscreenCanvas cannot be
                            re-transferred — plus `registerPageCanvas` must
                            not race the async `recover()` respawn. */}
                        <Show when={!booting()}>
                            <For each={Array.from({ length: Math.max(0, store.pageCount() - 1) }, (_, i) => i + 1)}>
                                {(idx) => (
                                    <ExtraPageCanvas client={client} store={store} pageIdx={idx} />
                                )}
                            </For>
                        </Show>
                    </div>
                </div>
            </SdkShelf>
            <AccessibilityTree client={client} />
            <Announcements store={store} />
            <UiErrorBanner store={store} />
            <Show when={booting()}>
                <div class="boot-overlay">Loading editor…</div>
            </Show>
        </>
    );
}
