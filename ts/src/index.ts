import EngineWorker from './engine.worker.ts?worker';
import type { Command, Event } from '../../crates/engine-wasm/pkg/engine_wasm.js';

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
        /** Test hook: send a Command to the worker; resolves with the event. */
        __dispatch?: (cmd: Command) => Promise<Event>;
    }
}

/* The interactive editor and the a4 visual-diff cases all paint a full A4
   page (595 × 842 pt → 1:1 canvas px). Single-glyph cases stay 400 × 400. */
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

    const params = new URLSearchParams(window.location.search);
    const testCase = params.get('test') ?? '';
    const interactive = testCase === '';

    const { w, h } = canvasSizeForCase(testCase);
    canvas.width = w;
    canvas.height = h;
    canvas.style.width = `${w}px`;
    canvas.style.height = `${h}px`;

    if (interactive) {
        status.textContent = 'loading editor…';
        status.style.pointerEvents = 'none';
        /* Center the A4 page horizontally with a small top gap, like a
           document on a desk. Test mode keeps the canvas at (0,0) so the
           visual-diff screenshot clip stays correct. */
        canvas.style.margin = '24px auto';
        canvas.style.boxShadow = '0 2px 12px rgba(0, 0, 0, 0.15)';
    } else {
        /* Visual-diff test mode: hide chrome so the canvas is the only
           thing in the screenshot. */
        status.style.display = 'none';
        document.documentElement.style.background = '#fff';
        document.body.style.background = '#fff';
    }

    if (!self.crossOriginIsolated) {
        console.warn('crossOriginIsolated is false — SAB unavailable');
    }

    const worker = new EngineWorker();
    const offscreen = canvas.transferControlToOffscreen();

    /* Promise-based command channel (main thread + Playwright tests). */
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
                console.log('[editor] engine boot ok');
                window.__engineReady = true;
                break;
            case 'PING_RESULT':
                console.log('[editor] ping/pong:', JSON.stringify(msg.event));
                break;
            case 'FONT_LOADED_RESULT':
                console.log('[editor] font loaded:', JSON.stringify(msg.event));
                break;
            case 'PAINT_RESULT':
                console.log('[editor] paint result:', JSON.stringify(msg.event));
                window.__lastEvent = msg.event;
                if (interactive) {
                    status.textContent =
                        'Ready — click the page and type. Ctrl+Z undo · Ctrl+Y redo';
                } else {
                    status.textContent = `painted: ${JSON.stringify(msg.event)}`;
                }
                break;
            case 'EDIT_RESULT':
                console.log(`[editor] edit/${msg.step}:`, JSON.stringify(msg.event));
                break;
            case 'DOCX_RESULT':
                console.log(`[editor] docx/${msg.step}:`, JSON.stringify(msg.event));
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
                console.log('[editor] worker idle');
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

    if (interactive) {
        wireInteractive(canvas, textarea, status, dispatch);
    }
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
    canvas.addEventListener('pointerdown', focusInput);
    document.body.addEventListener('pointerdown', focusInput);

    const showUndoState = (e: Event): void => {
        if (e.type === 'TEXT_INSERTED' || e.type === 'UNDO_STATE_CHANGED') {
            status.textContent =
                `editing — undo:${e.can_undo ? 'on' : 'off'} ` +
                `redo:${e.can_redo ? 'on' : 'off'} · depth ${e.undo_depth}`;
        }
    };

    const insert = (text: string): void => {
        if (!text) return;
        void dispatch({ type: 'INSERT_TEXT', at: undefined, text } as Command).then(
            showUndoState,
        );
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
            void dispatch({ type: 'UNDO' } as Command).then(showUndoState);
        } else if (k === 'y' || (k === 'z' && e.shiftKey)) {
            e.preventDefault();
            void dispatch({ type: 'REDO' } as Command).then(showUndoState);
        }
    });
}

void main();
