import EngineWorker from './engine.worker.ts?worker';

type WorkerMsg =
    | { type: 'BOOT_OK' }
    | { type: 'PING_RESULT'; event: unknown }
    | { type: 'FONT_LOADED_RESULT'; event: unknown }
    | { type: 'PAINT_RESULT'; event: unknown }
    | { type: 'IDLE' }
    | { type: 'ERROR'; error: string };

/* Smoke-test hook used by tools/visual-diff to wait for paint completion. */
declare global {
    interface Window {
        __paintIdle?: boolean;
        __engineReady?: boolean;
        __lastEvent?: unknown;
    }
}

/* Per-case canvas dimensions. The a4 case uses the full A4 point grid
   (595 × 842 pt = pixels at 1pt/px) so the engine's `A4Page::a4()` matches
   the drawing surface 1:1. */
function canvasSizeForCase(testCase: string): { w: number; h: number } {
    if (testCase === 'a4-justified-mixed') return { w: 595, h: 842 };
    return { w: 400, h: 400 };
}

async function main(): Promise<void> {
    const status = document.getElementById('status')!;
    const canvas = document.querySelector<HTMLCanvasElement>('#doc')!;

    const params = new URLSearchParams(window.location.search);
    const testCase = params.get('test') ?? '';

    const { w, h } = canvasSizeForCase(testCase);
    canvas.width = w;
    canvas.height = h;
    canvas.style.width = `${w}px`;
    canvas.style.height = `${h}px`;

    /* Test mode: hide chrome so the canvas is the only thing in the screenshot. */
    if (testCase) {
        status.style.display = 'none';
        document.documentElement.style.background = '#fff';
        document.body.style.background = '#fff';
    }

    if (!self.crossOriginIsolated) {
        status.textContent = 'WARNING: not cross-origin isolated';
        console.warn('crossOriginIsolated is false — SAB unavailable');
    }

    const worker = new EngineWorker();
    const offscreen = canvas.transferControlToOffscreen();

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
                status.textContent = `font loaded`;
                break;
            case 'PAINT_RESULT':
                console.log('[PoC] paint result:', JSON.stringify(msg.event));
                status.textContent = `painted: ${JSON.stringify(msg.event)}`;
                window.__lastEvent = msg.event;
                break;
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
}

void main();
