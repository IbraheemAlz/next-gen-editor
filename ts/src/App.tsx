/* Phase 4 §4 — top-level Solid application.
 *
 * Owns the EngineClient lifecycle: spawns the worker, seeds the first paint,
 * drives crash recovery, and mirrors the `window.__*` hooks the Phase 2
 * exit-gate e2e specs depend on. Document state lives in the engine (§9) —
 * App holds only UI signals. */
import { createSignal, For, Show } from 'solid-js';
import { EditorCanvas } from './components/EditorCanvas';
import { CaretOverlay } from './components/CaretOverlay';
import { SelectionOverlay } from './components/SelectionOverlay';
import { EngineClient } from './engine/engine-client';
import { createEngineStore } from './state/engine-store';
import type { Command, Event } from './engine/types';
import AMIRI_URL from '../fonts/Amiri-Regular.ttf?url';
import './styles/editor.css';
import './styles/caret.css';

/* Amiri is a dual-script Naskh face — renders mixed Arabic/English from a
   single face without engine-side font fallback. */
const AMIRI_ID = 'amiri';

/** Load the editor font and seed a blank A4 page. Runs on boot + recovery. */
async function setupEngine(client: EngineClient): Promise<void> {
    const fontBytes = new Uint8Array(await (await fetch(AMIRI_URL)).arrayBuffer());
    /* loadFont hands the buffer to the worker as a Transferable — zero-copy. */
    await client.loadFont(AMIRI_ID, fontBytes);
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

    /* Runs once EditorCanvas has handed the engine its surface (init/recover). */
    const onReady = async (generation: number, initMs: number): Promise<void> => {
        if (generation === 0) {
            window.__bootMs = initMs;
            window.__engineReady = true;
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
        }
    };

    return (
        <div class="editor-shell">
            <div class="editor-viewport">
                <For each={[canvasGen()]}>
                    {(generation) => (
                        <EditorCanvas
                            client={client}
                            generation={generation}
                            onReady={onReady}
                        />
                    )}
                </For>
                <SelectionOverlay store={store} />
                <CaretOverlay store={store} />
            </div>
            <Show when={booting()}>
                <div class="boot-overlay">Loading editor…</div>
            </Show>
        </div>
    );
}
