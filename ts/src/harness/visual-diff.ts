/* Visual-diff harness — the Phase 1 `{ type: 'COMMAND', id, cmd }` worker
 * protocol path, kept alive for the `?test=<case>` golden suite
 * (tools/visual-diff/run.mjs). CLAUDE.md mandates this path stays green.
 *
 * `index.tsx` routes here whenever a `?test=` query param is present and
 * never mounts the Solid app — so the screenshot is the bare engine canvas,
 * exactly as the goldens were captured. The interactive editor takes the
 * EngineClient path instead (see App.tsx). */
import EngineWorker from '../engine/engine.worker.ts?worker';
import type { Command, Event } from '../engine/types';

/* Worker → main messages for the Phase 1 visual-diff harness path. */
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

/* The a4 visual-diff cases paint a full A4 page (595 × 842 pt → 1:1 canvas
   px). Single-glyph / single-word cases stay 400 × 400. Keep this in sync
   with the `VIEWPORTS` map in tools/visual-diff/run.mjs. */
function canvasSizeForCase(testCase: string): { w: number; h: number } {
    if (
        testCase === 'a4-justified-mixed' ||
        testCase === 'rich-text' ||
        testCase === 'editing-arabic' ||
        testCase === 'docx-round-trip' ||
        testCase === 'tab-stops-center-kind-ltr' ||
        testCase === 'tab-stops-right-kind-ltr' ||
        testCase === 'tab-stops-decimal-kind-ltr' ||
        testCase === 'table-merges' ||
        testCase === 'autofit-long-url-overflow' ||
        testCase === 'list-bullet-numbered' ||
        testCase === 'rich-text-caps'
    ) {
        return { w: 595, h: 842 };
    }
    return { w: 400, h: 400 };
}

/**
 * Boot the engine worker for a `?test=<case>` golden capture. Builds its own
 * bare canvas at the viewport origin (run.mjs screenshots a `{0,0,w,h}` clip),
 * hides the Solid `#root`, and drives the worker through the Phase 1 harness
 * protocol. Sets `window.__paintIdle` once the case finishes painting.
 */
export function runVisualDiffHarness(testCase: string): void {
    /* Sprint-9-PR-cherry-pick — dual-tier golden routing. The harness
       stays Canvas2D-locked by DEFAULT (CI determinism); the `?renderer=vello`
       query param opts in to Vello for the parallel `golden/vello/`
       corpus. NO auto detect_backend() — coupling the golden bytes to
       GPU presence broke PR #19's QA harness. */
    const renderer =
        new URLSearchParams(window.location.search).get('renderer') === 'vello'
            ? 'vello'
            : 'canvas2d';
    /* Test mode: only the engine canvas should be in the screenshot. */
    document.documentElement.style.background = '#fff';
    document.body.style.background = '#fff';
    const root = document.getElementById('root');
    if (root) root.style.display = 'none';

    const { w, h } = canvasSizeForCase(testCase);
    const canvas = document.createElement('canvas');
    canvas.id = 'doc';
    canvas.width = w;
    canvas.height = h;
    canvas.style.cssText =
        `display:block;width:${w}px;height:${h}px;background:#fff;image-rendering:pixelated;`;
    /* Prepend so the canvas sits at the viewport origin regardless of the
       (hidden) #root that precedes it in the document. */
    document.body.prepend(canvas);

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
                console.log(`[harness] ${msg.type}:`, JSON.stringify(msg.event));
                break;
            case 'PAINT_RESULT':
                window.__lastEvent = msg.event;
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
                console.error('[harness] worker error:', msg.error);
                break;
        }
    };

    worker.onerror = (e: ErrorEvent) => {
        console.error('[harness] worker fatal:', e.message);
    };

    worker.postMessage(
        { type: 'INIT', canvas: offscreen, testCase, renderer },
        [offscreen],
    );
}
