import EngineWorker from './engine.worker.ts?worker';
import { EngineClient } from './engine-client';
import type { Command, Event } from '../../crates/engine-wasm/pkg/engine_wasm.js';
import AMIRI_URL from '../fonts/Amiri-Regular.ttf?url';

/* Worker → main messages for the Phase 1 visual-diff harness path. The
   interactive editor uses EngineClient instead (id-routed RPC). */
type WorkerMsg =
    | { type: 'BOOT_OK' }
    | { type: 'PING_RESULT'; event: unknown }
    | { type: 'FONT_LOADED_RESULT'; event: unknown }
    | { type: 'PAINT_RESULT'; event: unknown }
    | { type: 'EDIT_RESULT'; step: string; event: unknown }
    | { type: 'DOCX_RESULT'; step: string; event: unknown }
    | { type: 'COMMAND_RESULT'; id: number; event: Event }
    | { type: 'IDLE' }
    | { type: 'ERROR'; error: string };

declare global {
    interface Window {
        __paintIdle?: boolean;
        __engineReady?: boolean;
        __lastEvent?: unknown;
        /** Test hook: send a Command to the engine; resolves with the Event. */
        __dispatch?: (cmd: Command) => Promise<Event>;
        /** The interactive editor's client (debugging / e2e hook). */
        __engineClient?: EngineClient;
        /** Most recent EngineStats event (debugging / e2e hook). */
        __lastStats?: unknown;
        /** Cold-boot duration in ms (e2e hook for boot.spec.ts). */
        __bootMs?: number;
        /** Set true after a successful crash recovery (e2e hook). */
        __recovered?: boolean;
    }
}

const AMIRI_ID = 'amiri';

/* The interactive editor and the a4 visual-diff cases paint a full A4 page
   (595 × 842 pt → 1:1 canvas px). Single-glyph cases stay 400 × 400. */
function canvasSizeForCase(testCase: string): { w: number; h: number } {
    if (
        testCase === '' ||
        testCase === 'a4-justified-mixed' ||
        testCase === 'editing-arabic' ||
        testCase === 'docx-round-trip'
    ) {
        return { w: 595, h: 842 };
    }
    return { w: 400, h: 400 };
}

async function main(): Promise<void> {
    const status = document.getElementById('status')!;
    const canvas = document.querySelector<HTMLCanvasElement>('#doc')!;
    const textarea = document.querySelector<HTMLTextAreaElement>('#input')!;
    const loading = document.getElementById('loading')!;

    const params = new URLSearchParams(window.location.search);
    const testCase = params.get('test') ?? '';
    const interactive = testCase === '';

    const { w, h } = canvasSizeForCase(testCase);
    canvas.width = w;
    canvas.height = h;
    canvas.style.width = `${w}px`;
    canvas.style.height = `${h}px`;

    if (!self.crossOriginIsolated) {
        console.warn('crossOriginIsolated is false — SAB unavailable');
    }

    if (interactive) {
        await runInteractive(canvas, textarea, status, loading);
    } else {
        runVisualDiffHarness(canvas, status, loading, testCase);
    }
}

/* ===================================================================
   Interactive editor — Phase 2 EngineClient path.
   =================================================================== */

/** Load the editor font and seed a blank A4 page. Reused on boot + recovery. */
async function setupEngine(client: EngineClient): Promise<void> {
    /* Amiri is a dual-script Naskh face — renders mixed Arabic/English without
       engine-side font fallback (that lands in Phase 3). */
    const fontBytes = new Uint8Array(await (await fetch(AMIRI_URL)).arrayBuffer());
    /* D2.4: loadFont hands the buffer to the worker as a Transferable — no copy. */
    await client.loadFont(AMIRI_ID, fontBytes);
    await client.dispatch({
        type: 'RENDER_PAGE',
        text: '',
        font_id: AMIRI_ID,
        base_direction: 'RTL',
        px_size: 24,
        line_height: 36,
        align: 'START',
    });
}

async function runInteractive(
    canvas: HTMLCanvasElement,
    textarea: HTMLTextAreaElement,
    status: HTMLElement,
    loading: HTMLElement,
): Promise<void> {
    status.textContent = 'loading editor…';
    status.style.pointerEvents = 'none';
    /* Center the A4 page like a document on a desk. */
    canvas.style.margin = '24px auto';
    canvas.style.boxShadow = '0 2px 12px rgba(0, 0, 0, 0.15)';

    let current = canvas;

    /* D2.7 crash recovery: the trapped worker consumed the OffscreenCanvas and
       transferControlToOffscreen() is one-shot — so swap in a fresh <canvas>
       element, transfer it, and hand it to recover(). */
    const onCrash = (): void => {
        status.textContent = 'engine crashed — recovering…';
        loading.classList.remove('hidden');
        const fresh = document.createElement('canvas');
        fresh.id = current.id;
        fresh.width = current.width;
        fresh.height = current.height;
        fresh.style.cssText = current.style.cssText;
        current.replaceWith(fresh);
        current = fresh;
        const offscreen = fresh.transferControlToOffscreen();
        void (async () => {
            try {
                await client.recover(offscreen);
                await setupEngine(client);
                loading.classList.add('hidden');
                textarea.focus();
                status.textContent = 'recovered — keep typing';
                window.__recovered = true;
            } catch (e: unknown) {
                const msg = e instanceof Error ? e.message : String(e);
                status.textContent = `recovery failed: ${msg}`;
            }
        })();
    };

    const client = new EngineClient('interactive', onCrash);
    window.__engineClient = client;

    const bootStart = performance.now();
    await client.init(current.transferControlToOffscreen());
    window.__bootMs = performance.now() - bootStart;
    window.__engineReady = true;
    await setupEngine(client);
    window.__paintIdle = true;

    loading.classList.add('hidden');
    textarea.focus();
    status.textContent = 'Ready — click the page and type. Ctrl+Z undo · Ctrl+Y redo';

    const dispatch = (cmd: Command): Promise<Event> => client.dispatch(cmd);
    window.__dispatch = dispatch;
    wireInteractive(current, textarea, status, dispatch);

    /* D2.5: poll engine memory/telemetry stats. */
    startStatsPolling(client);
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

/* ===================================================================
   Visual-diff harness — Phase 1 worker protocol (keeps the goldens green).
   =================================================================== */

function runVisualDiffHarness(
    canvas: HTMLCanvasElement,
    status: HTMLElement,
    loading: HTMLElement,
    testCase: string,
): void {
    /* Test mode: hide chrome so the canvas is the only thing screenshot. */
    status.style.display = 'none';
    loading.classList.add('hidden');
    document.documentElement.style.background = '#fff';
    document.body.style.background = '#fff';

    const worker = new EngineWorker();
    const offscreen = canvas.transferControlToOffscreen();

    let nextCommandId = 1;
    const pending = new Map<number, (e: Event) => void>();
    const dispatch = (cmd: Command): Promise<Event> => {
        const id = nextCommandId++;
        return new Promise<Event>((resolve) => {
            pending.set(id, resolve);
            worker.postMessage({ type: 'COMMAND', id, cmd });
        });
    };
    window.__dispatch = dispatch;

    worker.onmessage = (ev: MessageEvent<WorkerMsg>) => {
        const msg = ev.data;
        switch (msg.type) {
            case 'BOOT_OK':
                window.__engineReady = true;
                break;
            case 'PING_RESULT':
            case 'FONT_LOADED_RESULT':
            case 'EDIT_RESULT':
            case 'DOCX_RESULT':
                console.log(`[editor] ${msg.type}:`, JSON.stringify(msg.event));
                break;
            case 'PAINT_RESULT':
                window.__lastEvent = msg.event;
                status.textContent = `painted: ${JSON.stringify(msg.event)}`;
                break;
            case 'COMMAND_RESULT': {
                const cb = pending.get(msg.id);
                if (cb) {
                    pending.delete(msg.id);
                    cb(msg.event);
                }
                break;
            }
            case 'IDLE':
                window.__paintIdle = true;
                break;
            case 'ERROR':
                status.textContent = `error: ${msg.error}`;
                status.style.display = 'block';
                console.error('[editor] worker error:', msg.error);
                break;
        }
    };

    worker.onerror = (e: ErrorEvent) => {
        status.textContent = `worker fatal: ${e.message}`;
        status.style.display = 'block';
        console.error('[editor] worker fatal', e);
    };

    worker.postMessage({ type: 'INIT', canvas: offscreen, testCase }, [offscreen]);
}

/**
 * Interactive editing: keystrokes + IME → InsertText, Ctrl+Z/Y → Undo/Redo.
 *
 * Phase 1 limitation: there is no cursor positioning yet, so every insert
 * appends at end-of-document (`at: undefined`). Click-to-place-caret lands
 * in Phase 4 (PHASE_4_HEADLESS_UI.md §14.A).
 */
function wireInteractive(
    canvas: HTMLCanvasElement,
    textarea: HTMLTextAreaElement,
    status: HTMLElement,
    dispatch: (cmd: Command) => Promise<Event>,
): void {
    /* Pin the hidden textarea top-left of the page. Phase 4 moves it to
       follow the caret so OS IME popups anchor correctly. */
    textarea.style.left = '8px';
    textarea.style.top = '8px';

    const focusInput = (): void => textarea.focus();
    focusInput();
    /* A single load-time focus() is unreliable — re-grab the caret on any
       pointer-down and when the window regains focus. */
    canvas.addEventListener('pointerdown', focusInput);
    document.body.addEventListener('pointerdown', focusInput);
    window.addEventListener('focus', focusInput);

    const showUndoState = (e: Event): void => {
        if (e.type === 'TEXT_INSERTED' || e.type === 'UNDO_STATE_CHANGED') {
            status.textContent =
                `editing — undo:${e.can_undo ? 'on' : 'off'} ` +
                `redo:${e.can_redo ? 'on' : 'off'} · depth ${e.undo_depth}`;
        }
    };

    const insert = (text: string): void => {
        if (!text) return;
        void dispatch({ type: 'INSERT_TEXT', at: undefined, text }).then(showUndoState);
    };

    /* Direct keyboard input. `beforeinput` with isComposing=true means an IME
       composition is in flight — let compositionend handle the commit. */
    textarea.addEventListener('beforeinput', (e: InputEvent) => {
        if (e.isComposing) return;
        e.preventDefault();
        if (e.inputType === 'insertText' && e.data) {
            insert(e.data);
        }
        textarea.value = '';
    });

    /* IME (Arabic / CJK): commit the composed string on compositionend. */
    textarea.addEventListener('compositionend', (e: CompositionEvent) => {
        if (e.data) insert(e.data);
        textarea.value = '';
    });

    /* Undo / redo. Ctrl+Z, Ctrl+Y, and Ctrl+Shift+Z (mac-style redo). */
    window.addEventListener('keydown', (e: KeyboardEvent) => {
        const mod = e.ctrlKey || e.metaKey;
        if (!mod) return;
        const k = e.key.toLowerCase();
        if (k === 'z' && !e.shiftKey) {
            e.preventDefault();
            void dispatch({ type: 'UNDO' }).then(showUndoState);
        } else if (k === 'y' || (k === 'z' && e.shiftKey)) {
            e.preventDefault();
            void dispatch({ type: 'REDO' }).then(showUndoState);
        }
    });
}

void main();
