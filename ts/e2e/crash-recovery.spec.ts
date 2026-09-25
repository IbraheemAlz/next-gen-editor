import { test, expect } from '@playwright/test';

/* D2.7 exit gate: crash recovery. Force a worker trap, then verify the UI
   shell handles the crash callback — swaps in a fresh <canvas>, respawns the
   worker via recover() — and that the respawned engine answers RPC again. */
test('UI recovers from a worker crash', async ({ page }) => {
    await page.goto('/');
    await page.waitForFunction(() => (window as any).__paintIdle === true, undefined, {
        timeout: 15_000,
    });

    /* Trigger the crash: forceTrap() terminates the worker and runs onTrap. */
    await page.evaluate(() => (window as any).__engineClient.forceTrap());

    /* The UI shell's onCrash callback swaps the canvas and calls recover();
       `index.ts` sets __recovered once the respawned engine is set up. */
    await page.waitForFunction(() => (window as any).__recovered === true, undefined, {
        timeout: 15_000,
    });

    /* The respawned worker answers a Ping. */
    const evt = await page.evaluate(
        () => (window as any).__dispatch({ type: 'PING' }) as Promise<{ type: string }>,
    );
    expect(evt.type).toBe('PONG');
});

/* The boot seed `setupEngine` paints, with the caret parked at (0, 0) — so
   typed text lands in front of it. */
const SEED = 'Hello world مرحبا بالعالم';

/* Issue #85 — real recovery: base snapshot + replayed tail.
   Type 200 characters, take a `SNAPSHOT`, arm a REAL wasm trap after the
   next logged command (`Engine.debug_force_trap` → `RuntimeError` → worker
   close → respawn → `RECOVER`), then prove on the recovered engine:
     - its `SNAPSHOT` is byte-identical to the pre-trap one (document,
       styles, stories, undo window, selection, layout config);
     - the caret is where it was (the next insert lands after the 200 chars);
     - undo works past the replayed tail (two undos each drop one char);
     - `__renderer` reports the recovered engine's own backend (issue #66). */
test('document, caret and undo survive a real wasm trap; renderer reported truthfully', async ({
    page,
}) => {
    test.setTimeout(120_000);
    await page.goto('/');
    await page.waitForFunction(() => (window as any).__paintIdle === true, undefined, {
        timeout: 15_000,
    });

    const typed = Array.from({ length: 200 }, (_, i) => String.fromCharCode(97 + (i % 26))).join('');

    const result = await page.evaluate(async (typedText: string) => {
        const dispatch = (window as any).__dispatch as (cmd: unknown) => Promise<any>;
        const client = (window as any).__engineClient;
        const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
        const sha256 = async (bytes: Uint8Array): Promise<string> => {
            const digest = await crypto.subtle.digest('SHA-256', bytes as BufferSource);
            return Array.from(new Uint8Array(digest))
                .map((b) => b.toString(16).padStart(2, '0'))
                .join('');
        };
        /* Whole-document plain text via select-all + the clipboard read-back. */
        const documentText = async (): Promise<string> => {
            await dispatch({ type: 'SELECT_ALL' });
            const payload = await dispatch({ type: 'GET_SELECTION_AS_CLIPBOARD' });
            return payload.type === 'CLIPBOARD_PAYLOAD' ? payload.plain : `<${payload.type}>`;
        };

        for (const ch of typedText) {
            await dispatch({ type: 'INSERT_TEXT', at: undefined, text: ch });
        }

        const pre = await dispatch({ type: 'SNAPSHOT', seq: undefined });
        if (pre.type !== 'SNAPSHOT') return { failed: `pre-trap snapshot: ${pre.type}` };
        const preHash = await sha256(pre.bytes);

        let recoveredEvt: any = null;
        client.subscribe((e: any) => {
            if (e.type === 'RECOVERED') recoveredEvt = e;
        });

        /* Arm the fault: the worker traps right after acknowledging + logging
           the next command. PING is logged and state-free, so the pre-trap
           snapshot above is the exact state the trap interrupts. */
        await client.armTrap(1);
        const pingType = await dispatch({ type: 'PING' }).then(
            (e: any) => e.type,
            () => 'rejected',
        );
        for (let i = 0; i < 600 && (window as any).__recovered !== true; i++) {
            await sleep(50);
        }
        const recovered = (window as any).__recovered === true;
        if (!recovered) return { failed: 'recovery did not complete' };

        const post = await dispatch({ type: 'SNAPSHOT', seq: undefined });
        if (post.type !== 'SNAPSHOT') return { failed: `post-recovery snapshot: ${post.type}` };
        const postHash = await sha256(post.bytes);

        /* Caret restored: an insert at the LIVE caret lands after the typed run. */
        await dispatch({ type: 'INSERT_TEXT', at: undefined, text: '!' });
        const afterInsert = await documentText();
        /* Undo depth: the insert above, then one of the pre-trap keystrokes. */
        await dispatch({ type: 'UNDO' });
        const afterUndo1 = await documentText();
        await dispatch({ type: 'UNDO' });
        const afterUndo2 = await documentText();

        return {
            pingType,
            preLen: pre.bytes.length,
            preHash,
            postLen: post.bytes.length,
            postHash,
            formatVersion: post.format_version,
            recoveredEvt,
            lastRecovery: client.lastRecovery,
            windowRenderer: (window as any).__renderer,
            clientRenderer: client.renderer,
            afterInsert,
            afterUndo1,
            afterUndo2,
        };
    }, typed);

    expect((result as any).failed, 'in-page failure').toBeUndefined();
    const r = result as any;
    console.log(
        `[recovery] snapshot ${r.preLen} B (format v${r.formatVersion}) · ` +
            `replayed ${r.recoveredEvt?.applied_commands} · renderer ${r.recoveredEvt?.renderer}`,
    );
    /* The armed trap fires AFTER the command's reply — the ping itself succeeds. */
    expect(r.pingType).toBe('PONG');
    expect(r.recoveredEvt, 'Event::Recovered broadcast').not.toBeNull();
    expect(r.recoveredEvt.snapshot_restored, 'base snapshot restored').toBe(true);
    expect(r.lastRecovery?.restored).toBe(true);
    /* Byte-identical: document + selection + undo window + layout config. */
    expect(r.postLen).toBe(r.preLen);
    expect(r.postHash).toBe(r.preHash);
    /* Caret + undo. */
    expect(r.afterInsert).toBe(`${typed}!${SEED}`);
    expect(r.afterUndo1).toBe(`${typed}${SEED}`);
    expect(r.afterUndo2).toBe(`${typed.slice(0, -1)}${SEED}`);
    /* Issue #66 — `__renderer` matches what the recovered engine paints with. */
    expect(['vello', 'canvas2d']).toContain(r.recoveredEvt.renderer);
    expect(r.windowRenderer).toBe(r.recoveredEvt.renderer);
    expect(r.clientRenderer).toBe(r.recoveredEvt.renderer);
    expect(r.lastRecovery?.renderer).toBe(r.recoveredEvt.renderer);
});

/* Issue #97 — zoom / device scale after a real trap. Set 150 % through the
   zoom control, trap, and prove the control, the engine's own report
   (`Event::Recovered.zoom` / `.device_scale`) and the painted page scale
   all agree on the recovered generation. */
test('zoom and device scale re-sync from Event::Recovered after a real trap', async ({
    page,
}) => {
    test.setTimeout(60_000);
    await page.goto('/');
    await page.waitForFunction(() => (window as any).__paintIdle === true, undefined, {
        timeout: 15_000,
    });
    const pageHeight = (): Promise<number> =>
        page.evaluate(async () => {
            const evt = await (window as any).__dispatch({
                type: 'REQUEST_PAINT',
                viewport: { x: 0, y: 0, w: 0, h: 0 },
                dirty: undefined,
            });
            return evt.type === 'PAINTED' ? (evt.page_heights[0] ?? -1) : -1;
        });
    const zoomValues = (): Promise<string[]> =>
        page.$$eval('.nge-zoom__select', (els) =>
            els.map((el) => (el as HTMLSelectElement).value),
        );

    /* An edit before the trap: the recovery below usually has no base
       snapshot yet (the idle snapshot needs 1.5 s of quiet), so this also
       proves the shell keeps a session the replayed tail re-seeded instead
       of re-seeding a blank page over it. */
    await page.evaluate(() =>
        (window as any).__dispatch({ type: 'INSERT_TEXT', at: undefined, text: 'Z' }),
    );
    const h100 = await pageHeight();
    expect(h100).toBeGreaterThan(0);
    await page.locator('.nge-zoom__select').first().selectOption('1.5');
    await expect.poll(zoomValues).toEqual(['1.5', '1.5']);
    const h150 = await pageHeight();
    expect(h150 / h100).toBeCloseTo(1.5, 3);

    const recovered = await page.evaluate(async () => {
        const client = (window as any).__engineClient;
        let evt: any = null;
        client.subscribe((e: any) => {
            if (e.type === 'RECOVERED') evt = e;
        });
        await client.armTrap(1);
        await (window as any).__dispatch({ type: 'PING' }).catch(() => undefined);
        for (let i = 0; i < 600 && (window as any).__recovered !== true; i++) {
            await new Promise((r) => setTimeout(r, 50));
        }
        return { evt, dpr: window.devicePixelRatio };
    });
    expect(recovered.evt, 'Event::Recovered broadcast').not.toBeNull();
    expect(recovered.evt.zoom).toBeCloseTo(1.5, 5);
    expect(recovered.evt.device_scale).toBeCloseTo((recovered.dpr * 4) / 3, 4);

    /* The controls show the engine's zoom, and the recovered engine paints
       at exactly the pre-trap 150 % scale. */
    await expect.poll(zoomValues).toEqual(['1.5', '1.5']);
    expect(await pageHeight()).toBeCloseTo(h150, 3);
    const text = await page.evaluate(async () => {
        const dispatch = (window as any).__dispatch;
        await dispatch({ type: 'SELECT_ALL' });
        const p = await dispatch({ type: 'GET_SELECTION_AS_CLIPBOARD' });
        return p.type === 'CLIPBOARD_PAYLOAD' ? p.plain : `<${p.type}>`;
    });
    expect(text).toBe(`Z${SEED}`);
});
