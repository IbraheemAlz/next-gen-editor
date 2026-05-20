import { test, expect } from '@playwright/test';

/* D2.4 exit gate: zero-copy bulk transfer. A 50 MB ArrayBuffer round-trips
   through a worker via Transferable in well under 50 ms — and the source
   buffer is detached afterward, proving it was MOVED, not structure-cloned
   (a structured clone would leave byteLength === 50 MB and cost ~tens of ms). */
test('50 MB blob transfers zero-copy round-trip under 50 ms', async ({ page }) => {
    await page.goto('/');

    const result = await page.evaluate(async () => {
        const SIZE = 50 * 1024 * 1024;
        /* A worker that echoes its message straight back, transferring the
           buffer in both directions. */
        const src = 'self.onmessage = (e) => { const b = e.data; self.postMessage(b, [b]); };';
        const worker = new Worker(URL.createObjectURL(new Blob([src], { type: 'text/javascript' })));
        try {
            const buf = new ArrayBuffer(SIZE);
            const t0 = performance.now();
            const echoed = await new Promise<ArrayBuffer>((resolve) => {
                worker.onmessage = (e) => resolve(e.data as ArrayBuffer);
                worker.postMessage(buf, [buf]);
            });
            return {
                elapsedMs: performance.now() - t0,
                sourceDetached: buf.byteLength === 0,
                echoedBytes: echoed.byteLength,
                sabAvailable: typeof SharedArrayBuffer === 'function',
            };
        } finally {
            worker.terminate();
        }
    });

    console.log(`[sab-transfer] 50 MB round-trip in ${result.elapsedMs.toFixed(2)} ms`);

    expect(result.sabAvailable, 'SharedArrayBuffer available (cross-origin isolated)').toBe(true);
    expect(result.echoedBytes, 'full 50 MB echoed back').toBe(50 * 1024 * 1024);
    expect(result.sourceDetached, 'source buffer detached — transferred, not copied').toBe(true);
    expect(result.elapsedMs).toBeLessThan(50);
});
