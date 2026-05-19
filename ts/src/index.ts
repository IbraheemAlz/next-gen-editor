import EngineWorker from './engine.worker.ts?worker';

type WorkerMsg =
    | { type: 'BOOT_OK' }
    | { type: 'PING_RESULT'; event: unknown }
    | { type: 'ERROR'; error: string };

async function main(): Promise<void> {
    const status = document.getElementById('status')!;
    const canvas = document.querySelector<HTMLCanvasElement>('#doc')!;

    const dpr = window.devicePixelRatio;
    canvas.width = canvas.clientWidth * dpr;
    canvas.height = canvas.clientHeight * dpr;

    if (!self.crossOriginIsolated) {
        status.textContent = 'WARNING: not cross-origin isolated';
        console.warn('crossOriginIsolated is false — SAB will be unavailable');
    }

    const worker = new EngineWorker();
    const offscreen = canvas.transferControlToOffscreen();

    worker.onmessage = (ev: MessageEvent<WorkerMsg>) => {
        const msg = ev.data;
        switch (msg.type) {
            case 'BOOT_OK':
                status.textContent = 'engine boot ok — green sentinel painted';
                console.log('[PoC] engine boot ok');
                break;
            case 'PING_RESULT':
                status.textContent = `ping → ${JSON.stringify(msg.event)}`;
                console.log('[PoC] ping/pong success:', msg.event);
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

    worker.postMessage({ type: 'INIT', canvas: offscreen }, [offscreen]);
}

void main();
