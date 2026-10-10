/* Phase 4 §4 — top-level Solid application.
 *
 * Owns the EngineClient lifecycle: spawns the worker, seeds the first paint,
 * drives crash recovery, and mirrors the `window.__*` hooks the Phase 2
 * exit-gate e2e specs depend on - issue #340: only in dev / `?test=` /
 * `VITE_NGE_DEV_HOOKS=1` builds (`dev-hooks.ts`); live validation against
 * a built bundle needs that flag too. Document state lives in the engine (§9) —
 * App holds only UI signals. */
import { createEffect, createSignal, For, onCleanup, onMount, Show } from 'solid-js';
import { EditorCanvas } from './components/EditorCanvas';
import { ExtraPageCanvas } from './components/ExtraPageCanvas';
import { CaretOverlay } from './components/CaretOverlay';
import { PageSelectionOverlay } from './components/PageSelectionOverlay';
import { ImageHandlesOverlay } from './components/ImageHandlesOverlay';
import { StoryModeOverlay } from './components/StoryModeOverlay';
import { HiddenInput } from './components/HiddenInput';
import { CaretReveal } from './components/CaretReveal';
import { CommentHighlights } from '@nge/ui';
import { SdkShelf } from './sdk-bridge';
import { AccessibilityTree } from './components/AccessibilityTree';
import { Announcements } from './components/Announcements';
import { EngineClient } from './engine/engine-client';
import { createEngineStore, SCREEN_DPI_SCALE, type EngineStore } from './state/engine-store';
import { startTelemetry } from './state/telemetry';
import { attachUnloadGuard } from './state/unload-guard';
import { devHooksEnabled, installDevHook, resolveTelemetryEndpoint } from './dev-hooks';
import { attachDragDrop } from './input/dnd';
import { createFontRegistry, createTelemetryConfig, type FontRegistry } from '@nge/core';
import type { Command, Event } from './engine/types';
import { topPos } from './engine/types';
import './styles/editor.css';
import './styles/caret.css';
import './styles/a11y.css';

/** Load the minimal default font set + seed a blank A4 page. Runs on boot
 *  AND crash recovery, so it must (re)establish fonts from scratch each
 *  time — `Command::Recover` wipes the engine's font map.
 *
 *  Issue #85 — `restored` is true when recovery installed a base snapshot:
 *  the engine then already holds the document, selection, undo window and
 *  layout config, and re-seeding via `RENDER_PAGE` would wipe exactly what
 *  was just recovered. That path only re-asserts the live device scale
 *  (which repaints and re-broadcasts the selection). Issue #97 — the same
 *  holds when no snapshot existed but the replayed tail re-seeded the
 *  session itself (`RecoveryInfo.layoutRestored`). */
async function setupEngine(
    client: EngineClient,
    fonts: FontRegistry,
    restored = false,
): Promise<void> {
    /* Forget any resident-font flags (recovery wiped the engine's map),
       then push the manifest's minimal boot set — one Latin + one
       dual-script Arabic — and its substitute faces. Every other declared
       font lazy-loads (JIT) the first time it is picked in the toolbar. */
    fonts.reset();
    /* Issue #329 — the substitute faces (Carlito for Calibri, Noto Naskh
       Arabic for Simplified Arabic, …) load in parallel with the boot set:
       a document opened (or recovered) next lays out in metric-compatible
       faces from its first paint, and `FONT_LOADED` / `DOCUMENT_LOADED`
       report what they stand in for. */
    await Promise.all([fonts.loadDefaults(), fonts.loadSubstitutes()]);
    if (restored) {
        const repaint = await client.dispatch({
            type: 'SET_DEVICE_SCALE',
            scale: window.devicePixelRatio * SCREEN_DPI_SCALE,
        });
        if (repaint.type === 'ERROR') {
            console.warn('[recovery] post-restore repaint:', repaint.message);
        }
        return;
    }
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

/** D2.5: poll EngineStats every 5 s into `window.__lastStats` - a dev /
 *  live-validation hook, so it is started only under the dev-hooks flag
 *  (`devHooksEnabled()`, issue #389). The visible readout lives in the
 *  Dev HUD (`@nge/ui`, Ctrl+Shift+D), not in a fixed `#stats` box on every
 *  page. */
function startStatsPolling(client: EngineClient): void {
    if (!devHooksEnabled()) return;
    const poll = async (): Promise<void> => {
        try {
            const evt = await client.dispatch({ type: 'REQUEST_STATS' });
            if (evt.type !== 'STATS') return;
            installDevHook('__lastStats', evt);
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
    /* Issue #340 - dev hooks: dev server / `?test=` / VITE_NGE_DEV_HOOKS=1
       builds only (see `dev-hooks.ts`). Live validation and Playwright
       need the flag in any non-dev build; a release build has none of
       these handles on `window`. */
    installDevHook('__engineClient', client);
    installDevHook('__dispatch', dispatch);

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
    installDevHook('__fontRegistry', fontRegistry);

    /* Issue #86 — D5.7 telemetry opt-in flag, shared with `SettingsMenu`
       (via `TelemetryProvider`, wired in `SdkShelf`) and the collector
       started in `onReady` below. Issue #340 - the endpoint comes from the
       build constant `VITE_NGE_TELEMETRY_ENDPOINT` (or a host-supplied
       `EngineProvider` prop); the `?telemetryEndpoint=` URL parameter is
       an e2e / local-debug hook honoured only under the dev-hooks flag. */
    const telemetryConfig = createTelemetryConfig();
    const telemetryEndpoint = resolveTelemetryEndpoint();
    installDevHook('__setTelemetryEnabled', telemetryConfig.setEnabled);

    /* §9 store — mirrors engine SELECTION_CHANGED events into signals the
       caret + selection overlays render from. */
    const store = createEngineStore(client);

    /* Issue #254 — the comments rail selected a comment's text: bring the
       caret into view. The caret overlay is mounted on the page holding
       it; before the next frame it may still be moving. */
    const revealCaret = (): void => {
        requestAnimationFrame(() => {
            const caretEl = viewportEl?.querySelector<HTMLElement>('.caret');
            if (caretEl) {
                caretEl.scrollIntoView({ block: 'center', inline: 'nearest' });
                return;
            }
            const c = store.caret();
            if (viewportEl && c) {
                viewportEl.scrollTop = Math.max(0, c.y - viewportEl.clientHeight / 2);
            }
        });
    };

    /* §10 D4.10 — drop a .docx anywhere on the page to load it. */
    onMount(() => onCleanup(attachDragDrop(client)));

    /* Issue #388 - a reload / tab close with unsaved edits raises the
       browser's prompt (off via VITE_NGE_UNLOAD_GUARD=0 for hosts that
       autosave); the next boot offers the session back regardless. */
    onMount(() => onCleanup(attachUnloadGuard(client)));

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
        }
        /* Issue #66 — every generation, not just boot: a respawned worker
           re-probes the GPU and may land on a different backend than the
           one that trapped. `client.renderer` is the recovered engine's
           own report after `recover()`. */
        window.__renderer = client.renderer;
        await setupEngine(
            client,
            fontRegistry,
            /* Issue #97 — "the engine already holds the session" also
               covers a snapshot-less recovery whose replayed tail carried
               the boot RENDER_PAGE: re-seeding would wipe the replayed
               document and snap the zoom back to 100 %. */
            client.lastRecovery?.layoutRestored === true,
        );
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
        /* Issue #330 — generation 0 can also be a recovery (a carried-over reload). */
        if (generation > 0 || client.lastRecovery !== undefined) {
            window.__recovered = true;
        }
        if (firstReady) {
            firstReady = false;
            startStatsPolling(client);
            /* Issue #86 — D5.7 real telemetry transport. Opt-in only
               (`telemetryConfig.enabled`, off by default, toggled from
               Settings); with no endpoint configured, an
               opted-in session still only logs to the console (dev-safe —
               see `ConsoleTransport`), never fires a real network request. */
            startTelemetry(client, {
                enabled: telemetryConfig.enabled,
                endpoint: telemetryEndpoint,
            });
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
                telemetry={telemetryConfig}
                telemetryEndpoint={telemetryEndpoint}
                engineReady={() => !booting()}
                onRevealCaret={revealCaret}
            >
                <div class="editor-viewport" ref={viewportEl}>
                    {/* Issue #387 — keyboard-driven caret moves scroll the
                        caret into view (pointer clicks never do). */}
                    <CaretReveal store={store} viewport={() => viewportEl} />
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
                    {/* Issue #280 — every page card is sized from the
                        engine's reported page geometry (device px ÷ the
                        device-px-per-CSS-px ratio), and the inter-page gap
                        from the engine zoom, so a zoom visibly resizes the
                        pages and the scroll range grows with them. */}
                    <div
                        class="editor-pages"
                        style={{
                            'min-height': `${store.deviceToCss(
                                Math.max(
                                    store.estimatedDocumentHeight(),
                                    store.documentHeight(),
                                ),
                            )}px`,
                            gap: `${store.pageGapCss()}px`,
                        }}
                    >
                        <div
                            class="editor-page"
                            data-page-index="0"
                            style={{
                                width: `${store.pageCardCss(0).w}px`,
                                height: `${store.pageCardCss(0).h}px`,
                            }}
                        >
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
                            {/* Issue #387 — commented text, tinted. */}
                            <CommentHighlights pageIdx={0} />
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
