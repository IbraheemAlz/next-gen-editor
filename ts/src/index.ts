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
        /** Test hook: send a Command to the worker; returns the event. */
        __dispatch?: (cmd: Command) => Promise<Event>;
    }
}

function canvasSizeForCase(testCase: string): { w: number; h: number } {
    if (
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

    const { w, h } = canvasSizeForCase(testCase);
    canvas.width = w;
    canvas.height = h;
    canvas.style.width = `${w}px`;
    canvas.style.height = `${h}px`;

    if (testCase) {
        status.style.display = 'none';
        document.documentElement.style.background = '#fff';
        document.body.style.background = '#fff';
    }

    if (!self.crossOriginIsolated) {
        console.warn('crossOriginIsolated is false — SAB unavailable');
    }

    const worker = new EngineWorker();
    const offscreen = canvas.transferControlToOffscreen();

    /* Promise-based command channel for the main thread (and Playwright tests). */
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
                status.textContent = 'engine boot ok';
                console.log('[PoC] engine boot ok');
                window.__engineReady = true;
                break;
            case 'PING_RESULT':
                console.log('[PoC] ping/pong:', JSON.stringify(msg.event));
                break;
            case 'FONT_LOADED_RESULT':
                console.log('[PoC] font loaded:', JSON.stringify(msg.event));
                status.textContent = 'font loaded';
                break;
            case 'PAINT_RESULT':
                console.log('[PoC] paint result:', JSON.stringify(msg.event));
                status.textContent = `painted: ${JSON.stringify(msg.event)}`;
                window.__lastEvent = msg.event;
                break;
            case 'EDIT_RESULT':
                console.log(`[PoC] edit/${msg.step}:`, JSON.stringify(msg.event));
                break;
            case 'DOCX_RESULT':
                console.log(`[PoC] docx/${msg.step}:`, JSON.stringify(msg.event));
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
                console.log('[PoC] worker idle');
                window.__paintIdle = true;
                break;
            case 'ERROR':
                status.textContent = `error: ${msg.error}`;
                console.error('[PoC] worker error:', msg.error);
                break;
        }
    };

    worker.onerror = (e: ErrorEvent) => {
        status.textContent = `worker fatal: ${e.message}`;
        console.error('[PoC] worker fatal', e);
    };

    worker.postMessage({ type: 'INIT', canvas: offscreen, testCase }, [offscreen]);

    /* Interactive mode (no testCase): keystrokes route to InsertText. */
    if (!testCase) {
        /* Pin textarea over the canvas top-left so the OS IME popup anchors
           somewhere sensible. Phase 4 will move it to follow the caret. */
        textarea.style.left = '8px';
        textarea.style.top = '8px';
        textarea.focus();
        textarea.addEventListener('beforeinput', (e: InputEvent) => {
            if (e.isComposing) return;
            e.preventDefault();
            if (e.inputType === 'insertText' && e.data) {
                void dispatch({ type: 'INSERT_TEXT', at: undefined, text: e.data } as Command);
            } else if (e.inputType === 'deleteContentBackward') {
                console.warn('[PoC] delete not wired in Phase 1');
            }
            textarea.value = '';
        });
        document.body.addEventListener('click', () => textarea.focus());
    }
}

void main();
