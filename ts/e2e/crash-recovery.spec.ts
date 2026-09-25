import { test, expect } from '@playwright/test';
import { readFileSync } from 'node:fs';
import { appendStoredEntry, pseudoRandomBytes } from './zip-append';

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

/* Issue #96 — every page paints after a real trap, not just page 0.
   Page ≥ 1 canvases were transferred to the worker that died; recovery
   must remount each with a FRESH `<canvas>` (a transferred one can never
   be transferred again) and register it with the respawned worker.
   Paint evidence comes from the worker-side opaque-ink probe
   (`EngineClient.probePageInk`) — headless Chrome never composites the
   transferred placeholders, so a DOM `drawImage` readback would lie.
   Run twice: recovery from the replayed tail alone, and from an idle
   base snapshot + tail. */
for (const withSnapshot of [false, true]) {
    test(`all pages of a 3-page document repaint after a real trap (${
        withSnapshot ? 'snapshot + tail' : 'tail only'
    })`, async ({ page }) => {
        test.setTimeout(90_000);
        await page.goto('/');
        await page.waitForFunction(() => (window as any).__paintIdle === true, undefined, {
            timeout: 15_000,
        });

        /* Grow the document until the paginator reports ≥ 3 pages. */
        const lines = Array.from(
            { length: 40 },
            (_, i) => `Line ${i + 1}: the quick brown fox jumps over the lazy dog.`,
        ).join('\n');
        for (let i = 0; i < 6; i++) {
            const pages = await page.evaluate(async (text: string) => {
                const dispatch = (window as any).__dispatch;
                await dispatch({ type: 'PASTE_PLAIN', text });
                const evt = await dispatch({
                    type: 'REQUEST_PAINT',
                    viewport: { x: 0, y: 0, w: 0, h: 0 },
                    dirty: undefined,
                });
                return evt.type === 'PAINTED' ? evt.page_count : 0;
            }, `${lines}\n`);
            if (pages >= 3) break;
        }
        await expect(page.locator('.editor-page')).not.toHaveCount(0);
        await expect
            .poll(() => page.locator('.editor-page canvas').count(), { timeout: 10_000 })
            .toBeGreaterThanOrEqual(3);

        const inkOf = (): Promise<Record<number, number>> =>
            page.evaluate(() => (window as any).__engineClient.probePageInk());
        /* The least-inked of pages 0–2 (`-1` when one is not registered). */
        const minInk = async (): Promise<number> => {
            const ink = await inkOf();
            return Math.min(...[0, 1, 2].map((i) => ink[i] ?? -1));
        };

        /* Baseline: all three pages carry ink before the trap. */
        await expect.poll(minInk, { timeout: 10_000 }).toBeGreaterThan(500);

        /* The worker's idle snapshot fires 1.5 s after the last logged command. */
        if (withSnapshot) await page.waitForTimeout(2_500);
        await page.evaluate(() => {
            (window as any).__prePageCanvases = Array.from(
                document.querySelectorAll('.editor-page canvas'),
            );
        });
        await page.evaluate(async () => {
            const client = (window as any).__engineClient;
            await client.armTrap(1);
            await (window as any).__dispatch({ type: 'PING' }).catch(() => undefined);
            for (let i = 0; i < 600 && (window as any).__recovered !== true; i++) {
                await new Promise((r) => setTimeout(r, 50));
            }
        });
        expect(await page.evaluate(() => (window as any).__recovered)).toBe(true);
        const info = await page.evaluate(() => (window as any).__engineClient.lastRecovery);
        expect(info.restored, 'base snapshot restored').toBe(withSnapshot);
        expect(info.layoutRestored, 'session kept, not re-seeded').toBe(true);

        /* Every page canvas is a brand-new element, registered with the NEW
           worker (its probe only sees surfaces it was handed), and painted. */
        await expect
            .poll(() => page.locator('.editor-page canvas').count(), { timeout: 10_000 })
            .toBeGreaterThanOrEqual(3);
        const reused = await page.evaluate(() => {
            const pre = new Set((window as any).__prePageCanvases as Element[]);
            return Array.from(document.querySelectorAll('.editor-page canvas')).filter((c) =>
                pre.has(c),
            ).length;
        });
        expect(reused, 'no transferred canvas survives into the new generation').toBe(0);
        await expect.poll(minInk, { timeout: 10_000 }).toBeGreaterThan(500);
    });
}

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

/* Issue #212 — a `.docx` whose retained source package (#134) is large:
   the package-parts fixture plus a 2 MiB incompressible
   `word/fonts/font1.odttf`, the shape of a Word document with an
   embedded font. Snapshots are persisted DETACHED — the package lands
   once in the event log's `packages` store and every snapshot row stays
   small — and a recovery re-attaches it, so the recovered session saves
   the byte-identical file. */
const PACKAGE_FIXTURE = new URL(
    '../../crates/format-docx/tests/fixtures/word_package_parts.docx',
    import.meta.url,
);
const FONT_BYTES = 2 * 1024 * 1024;

test('embedded-font document: snapshots stay small, the package is stored once, recovery saves the identical file', async ({
    page,
}) => {
    test.setTimeout(120_000);
    const docx = appendStoredEntry(
        readFileSync(PACKAGE_FIXTURE),
        'word/fonts/font1.odttf',
        pseudoRandomBytes(FONT_BYTES),
    );
    await page.goto('/');
    await page.waitForFunction(() => (window as any).__paintIdle === true, undefined, {
        timeout: 15_000,
    });

    const result = await page.evaluate(async (b64: string) => {
        const w = window as any;
        const dispatch = w.__dispatch as (cmd: unknown) => Promise<any>;
        const client = w.__engineClient;
        const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
        const sha256 = async (bytes: Uint8Array): Promise<string> => {
            const digest = await crypto.subtle.digest('SHA-256', bytes as BufferSource);
            return Array.from(new Uint8Array(digest))
                .map((b) => b.toString(16).padStart(2, '0'))
                .join('');
        };
        type Log = {
            snaps: { seq: number; size: number; packageHash?: string }[];
            packages: { hash: string; size: number }[];
        };
        const readLog = (): Promise<Log> =>
            new Promise((resolve, reject) => {
                const open = indexedDB.open('engine-log');
                open.onsuccess = () => {
                    const db = open.result;
                    const tx = db.transaction(['snapshots', 'packages'], 'readonly');
                    const snaps = tx.objectStore('snapshots').getAll();
                    const packages = tx.objectStore('packages').getAll();
                    tx.oncomplete = () => {
                        db.close();
                        resolve({
                            snaps: snaps.result.map((r: any) => ({
                                seq: r.seq,
                                size: r.bytes.length,
                                packageHash: r.packageHash,
                            })),
                            packages: packages.result.map((r: any) => ({
                                hash: r.hash,
                                size: r.bytes.length,
                            })),
                        });
                    };
                    tx.onerror = () => reject(tx.error);
                };
                open.onerror = () => reject(open.error);
            });
        /* The idle snapshot fires 1.5 s after the last logged command. */
        const awaitSnapshotCount = async (n: number): Promise<Log> => {
            for (let i = 0; i < 200; i++) {
                const log = await readLog();
                if (log.snaps.filter((s) => s.packageHash).length >= n) return log;
                await sleep(50);
            }
            throw new Error(`no ${n} package-bearing snapshot(s)`);
        };
        const save = async (): Promise<Uint8Array> => {
            const evt = await dispatch({ type: 'SAVE_DOCUMENT', format: 'docx' });
            if (evt.type !== 'DOCUMENT_SAVED') throw new Error(`save: ${evt.type}`);
            return evt.bytes;
        };

        const bytes = Uint8Array.from(atob(b64), (c) => c.charCodeAt(0));
        const opened = await dispatch({
            type: 'OPEN_DOCUMENT',
            bytes,
            format: 'docx',
            name: 'embedded-font.docx',
        });
        if (opened.type === 'ERROR') return { failed: `open: ${opened.message}` };
        await dispatch({ type: 'INSERT_TEXT', at: undefined, text: 'A' });
        /* The self-contained (#134) snapshot, for the size comparison. */
        const inline = (await client.snapshot()) as Uint8Array;
        const first = await awaitSnapshotCount(1);

        /* A second idle snapshot of the same document: small again, and
           the package is not written a second time. */
        await dispatch({ type: 'INSERT_TEXT', at: undefined, text: 'B' });
        const second = await awaitSnapshotCount(2);
        const preSave = await save();
        const preHash = await sha256(preSave);

        let recoveredEvt: any = null;
        client.subscribe((e: any) => {
            if (e.type === 'RECOVERED') recoveredEvt = e;
        });
        await client.armTrap(1);
        await dispatch({ type: 'PING' }).catch(() => undefined);
        for (let i = 0; i < 600 && w.__recovered !== true; i++) await sleep(50);
        if (w.__recovered !== true) return { failed: 'recovery did not complete' };
        const postSave = await save();
        return {
            inlineSize: inline.length,
            first,
            second,
            preSize: preSave.length,
            preHash,
            postHash: await sha256(postSave),
            recoveredEvt,
            info: client.lastRecovery,
        };
    }, Buffer.from(docx).toString('base64'));

    expect((result as any).failed, 'in-page failure').toBeUndefined();
    const r = result as any;
    const snaps = r.second.snaps.filter((s: any) => s.packageHash);
    console.log(
        `[recovery #212] inline snapshot ${r.inlineSize} B · persisted snapshots ` +
            `${snaps.map((s: any) => `${s.size} B`).join(', ')} · package ` +
            `${r.second.packages.map((p: any) => `${p.size} B`).join(', ')} · saved ${r.preSize} B`,
    );
    /* Before #212 every snapshot carried the font: > 2 MiB each. */
    expect(r.inlineSize).toBeGreaterThan(FONT_BYTES);
    for (const s of snaps) expect(s.size).toBeLessThan(256 * 1024);
    /* Stored once, named by every snapshot of the document. */
    expect(r.first.packages).toHaveLength(1);
    expect(r.second.packages).toHaveLength(1);
    expect(r.second.packages[0].size).toBeGreaterThan(FONT_BYTES);
    for (const s of snaps) expect(s.packageHash).toBe(r.second.packages[0].hash);

    expect(r.recoveredEvt?.snapshot_restored, 'base snapshot restored').toBe(true);
    expect(r.info.restored).toBe(true);
    expect(r.postHash, 'recovered session saves the identical file').toBe(r.preHash);
});

/* Issue #268 — a failed `packages` write leaves later snapshots naming a
   package the store does not hold. They still restore, but save through
   the minimal writer — silently dropping the `.docx`'s sibling parts.
   Recovery must prefer an OLDER snapshot whose package is present.
   Document A is opened and snapshotted (package hA stored), then document
   B (a different package, hB) is opened and snapshotted twice; the hB row
   is deleted, then a real trap. The newest snapshots restore only without
   their package; the snapshot of A still has hA, and its tail replays the
   logged `OPEN_DOCUMENT` of B — so the recovered session is B WITH its
   package and saves the identical file. */
test('a missing package row: recovery prefers an older snapshot that still has its package', async ({
    page,
}) => {
    test.setTimeout(120_000);
    const docA = readFileSync(PACKAGE_FIXTURE);
    const docB = appendStoredEntry(
        readFileSync(PACKAGE_FIXTURE),
        'word/fonts/font1.odttf',
        pseudoRandomBytes(64 * 1024),
    );
    await page.goto('/');
    await page.waitForFunction(() => (window as any).__paintIdle === true, undefined, {
        timeout: 15_000,
    });

    const result = await page.evaluate(
        async ([a64, b64]: [string, string]) => {
            const w = window as any;
            const dispatch = w.__dispatch as (cmd: unknown) => Promise<any>;
            const client = w.__engineClient;
            const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
            const sha256 = async (bytes: Uint8Array): Promise<string> => {
                const digest = await crypto.subtle.digest('SHA-256', bytes as BufferSource);
                return Array.from(new Uint8Array(digest))
                    .map((b) => b.toString(16).padStart(2, '0'))
                    .join('');
            };
            const withDb = <T,>(fn: (db: IDBDatabase) => Promise<T>): Promise<T> =>
                new Promise((resolve, reject) => {
                    const open = indexedDB.open('engine-log');
                    open.onsuccess = () => {
                        const db = open.result;
                        fn(db).then(
                            (v) => {
                                db.close();
                                resolve(v);
                            },
                            (e) => {
                                db.close();
                                reject(e);
                            },
                        );
                    };
                    open.onerror = () => reject(open.error);
                });
            type Log = { snaps: { seq: number; packageHash?: string }[]; packages: string[] };
            const readLog = () =>
                withDb(
                    (db) =>
                        new Promise<Log>((resolve, reject) => {
                            const tx = db.transaction(['snapshots', 'packages'], 'readonly');
                            const snaps = tx.objectStore('snapshots').getAll();
                            const packages = tx.objectStore('packages').getAllKeys();
                            tx.oncomplete = () =>
                                resolve({
                                    snaps: snaps.result.map((r: any) => ({
                                        seq: r.seq,
                                        packageHash: r.packageHash,
                                    })),
                                    packages: packages.result as string[],
                                });
                            tx.onerror = () => reject(tx.error);
                        }),
                );
            /* Wait for `n` snapshots naming a package other than `not`. */
            const awaitSnapshots = async (n: number, not?: string): Promise<Log> => {
                for (let i = 0; i < 200; i++) {
                    const log = await readLog();
                    const named = log.snaps.filter(
                        (s) => s.packageHash !== undefined && s.packageHash !== not,
                    );
                    if (named.length >= n) return log;
                    await sleep(50);
                }
                throw new Error(`no ${n} snapshot(s) naming a package other than ${not}`);
            };
            const open = async (b64: string, name: string) => {
                const bytes = Uint8Array.from(atob(b64), (c) => c.charCodeAt(0));
                const evt = await dispatch({ type: 'OPEN_DOCUMENT', bytes, format: 'docx', name });
                if (evt.type === 'ERROR') throw new Error(`open ${name}: ${evt.message}`);
            };
            const save = async (): Promise<Uint8Array> => {
                const evt = await dispatch({ type: 'SAVE_DOCUMENT', format: 'docx' });
                if (evt.type !== 'DOCUMENT_SAVED') throw new Error(`save: ${evt.type}`);
                return evt.bytes;
            };

            await open(a64, 'a.docx');
            const logA = await awaitSnapshots(1);
            const hashA = logA.snaps.find((s) => s.packageHash)!.packageHash!;
            await open(b64, 'b.docx');
            await awaitSnapshots(1, hashA);
            await dispatch({ type: 'INSERT_TEXT', at: undefined, text: 'B' });
            const logB = await awaitSnapshots(2, hashA);
            const hashB = logB.snaps.find((s) => s.packageHash && s.packageHash !== hashA)!
                .packageHash!;
            const preSave = await save();

            /* The failed package write: the store never got hB. */
            await withDb(
                (db) =>
                    new Promise<void>((resolve, reject) => {
                        const tx = db.transaction('packages', 'readwrite');
                        tx.objectStore('packages').delete(hashB);
                        tx.oncomplete = () => resolve();
                        tx.onerror = () => reject(tx.error);
                    }),
            );
            const before = await readLog();

            await client.armTrap(1);
            await dispatch({ type: 'PING' }).catch(() => undefined);
            for (let i = 0; i < 600 && w.__recovered !== true; i++) await sleep(50);
            if (w.__recovered !== true) return { failed: 'recovery did not complete' };
            const postSave = await save();
            const name = new TextEncoder().encode('word/fonts/font1.odttf');
            const hasEntry = (b: Uint8Array) => {
                outer: for (let i = 0; i + name.length <= b.length; i++) {
                    for (let j = 0; j < name.length; j++) {
                        if (b[i + j] !== name[j]) continue outer;
                    }
                    return true;
                }
                return false;
            };
            return {
                hashA,
                hashB,
                before,
                info: client.lastRecovery,
                preHash: await sha256(preSave),
                postHash: await sha256(postSave),
                postHasSibling: hasEntry(postSave),
            };
        },
        [Buffer.from(docA).toString('base64'), Buffer.from(docB).toString('base64')] as [
            string,
            string,
        ],
    );

    expect((result as any).failed, 'in-page failure').toBeUndefined();
    const r = result as any;
    console.log(
        `[recovery #268] snaps=${r.before.snaps
            .map((s: any) => `${s.seq}:${(s.packageHash ?? '-').slice(0, 14)}`)
            .join(',')} packages=${r.before.packages.length} ` +
            `packageFallbacks=${r.info.packageFallbacks} packageLost=${r.info.packageLost}`,
    );
    expect(r.hashA).not.toBe(r.hashB);
    expect(r.hashA.startsWith('sha256-'), 'issue #269 key').toBe(true);
    /* Set-up: A's package is stored, B's is not, and snapshots name both. */
    expect(r.before.packages).toEqual([r.hashA]);
    expect(r.before.snaps.some((s: any) => s.packageHash === r.hashB)).toBe(true);

    expect(r.info.restored).toBe(true);
    expect(r.info.packageFallbacks, 'newer package-less snapshot passed over').toBeGreaterThanOrEqual(1);
    expect(r.info.packageLost, 'no silent sibling loss').toBe(false);
    expect(r.info.snapshotFallbacks).toBe(0);
    expect(r.postHasSibling, 'the recovered save keeps the sibling part').toBe(true);
    expect(r.postHash, 'recovered session saves the identical file').toBe(r.preHash);
});
