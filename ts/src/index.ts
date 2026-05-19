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

async function main(): Promise<void> {
    const status = document.getElementById('status')!;
    const canvas = document.querySelector<HTMLCanvasElement>('#doc')!;

    /* Fixed canvas resolution for deterministic visual-diff. */
    canvas.width = 400;
    canvas.height = 400;
    canvas.style.width = '400px';
    canvas.style.height = '400px';

    const params = new URLSearchParams(window.location.search);
    const testCase = params.get('test') ?? '';

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
