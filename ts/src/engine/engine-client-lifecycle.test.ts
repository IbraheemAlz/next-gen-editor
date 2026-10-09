/* Issue #438 - `EngineClient.init` and `EngineClient.recover` against a
 * scripted fake `Worker` and a fake-indexeddb event log: what the client
 * posts (INIT / RECOVER payloads, the transfer list), how it folds the
 * worker's reply into `RecoveryInfo`, and what it does with a persisted
 * crash-loop streak. No browser, no wasm. */
import 'fake-indexeddb/auto';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { Command, Event } from '../../../crates/engine-wasm/pkg/engine_wasm.js';

vi.mock('./tab-session', () => ({
    claimTabSession: async () => ({ token: 't', db: 'engine-log', sweptArchives: 0 }),
}));

import { EngineClient, VELLO_TRAP_LIMIT, type RecoveryInfo } from './engine-client';
import * as eventLog from './event-log';

interface Sent {
    id: number;
    type?: string;
    cmd?: Command;
    [k: string]: unknown;
}

class FakeWorker {
    static instances: FakeWorker[] = [];
    onmessage: ((ev: { data: unknown }) => void) | null = null;
    onerror: ((e: { message: string }) => void) | null = null;
    sent: Sent[] = [];
    transfers: unknown[][] = [];
    terminated = false;
    constructor(
        readonly url: URL,
        readonly options: unknown,
    ) {
        FakeWorker.instances.push(this);
    }
    postMessage(msg: Sent, transfer?: unknown[]): void {
        this.sent.push(msg);
        this.transfers.push(transfer ?? []);
    }
    terminate(): void {
        this.terminated = true;
    }
    reply(id: number, extra: Record<string, unknown> = {}, evt?: Event): void {
        this.onmessage?.({ data: { id, ok: true, ...(evt ? { evt } : {}), ...extra } });
    }
    trap(error = 'unreachable'): void {
        this.onmessage?.({ data: { ok: false, trap: true, error } });
    }
    byType(type: string): Sent | undefined {
        return this.sent.find((m) => m.type === type);
    }
}

const worker = (): FakeWorker => FakeWorker.instances.at(-1)!;
const canvas = (): OffscreenCanvas => ({ tag: 'canvas' }) as unknown as OffscreenCanvas;
const recoveredEvt = (o: Record<string, unknown> = {}): Event =>
    ({
        type: 'RECOVERED',
        applied_commands: 0,
        snapshot_restored: true,
        renderer: 'canvas2d',
        zoom: 1.5,
        package_lost: false,
        ...o,
    }) as unknown as Event;

let client: EngineClient;
let onCrash: ReturnType<typeof vi.fn<() => void>>;

/** Wait until the fake worker has been sent a message of `type`. */
const posted = (type: string, w: () => FakeWorker = worker) =>
    vi.waitFor(() => {
        const m = w().byType(type);
        if (!m) throw new Error(`no ${type} posted yet`);
        return m;
    });

beforeEach(async () => {
    FakeWorker.instances = [];
    vi.stubGlobal('Worker', FakeWorker);
    vi.spyOn(console, 'error').mockImplementation(() => undefined);
    vi.spyOn(console, 'warn').mockImplementation(() => undefined);
    /* A pristine primary log + streak for each test. */
    await eventLog.openEventLog('doc');
    await eventLog.clearRendererStreak();
    onCrash = vi.fn<() => void>();
    client = new EngineClient('doc', onCrash);
});
afterEach(() => vi.useRealTimers());

describe('init', () => {
    it('boots a fresh session: INIT carries the document id, log db and the canvas (transferred)', async () => {
        const c = canvas();
        const p = client.init(c);
        const init = await posted('INIT');
        expect(init).toMatchObject({ documentId: 'doc', logDb: 'engine-log', canvas: c });
        expect(init).not.toHaveProperty('forceRenderer');
        expect(worker().transfers.at(-1)).toEqual([c]);
        worker().reply(init.id, { renderer: 'vello', probed: true, crossOriginIsolated: true });
        await p;
        expect(client.renderer).toBe('vello');
        expect(client.rendererDowngrade).toBeUndefined();
    });

    it('a failed INIT reply rejects with the worker error', async () => {
        const p = client.init(canvas());
        const init = await posted('INIT');
        worker().onmessage?.({ data: { id: init.id, ok: false, error: 'no webgpu adapter' } });
        await expect(p).rejects.toThrow('no webgpu adapter');
    });

    it('a persisted Vello streak at the limit boots on Canvas2D WITHOUT probing (forceRenderer)', async () => {
        await eventLog.saveRendererStreak({
            renderer: 'vello',
            count: VELLO_TRAP_LIMIT,
            at: Date.now() - 1000,
            live: false,
        });
        const p = client.init(canvas());
        const init = await posted('INIT');
        expect(init).toMatchObject({ forceRenderer: 'canvas2d' });
        expect(client.rendererDowngrade).toMatchObject({
            from: 'vello',
            to: 'canvas2d',
            reason: 'CRASH_LOOP',
            consecutive_traps: VELLO_TRAP_LIMIT,
        });
        worker().reply(init.id, { renderer: 'canvas2d', probed: false });
        await p;
        expect(client.renderer).toBe('canvas2d');
    });

    it('a stale streak (> 24 h) is discarded and the boot probes normally', async () => {
        await eventLog.saveRendererStreak({
            renderer: 'vello',
            count: 9,
            at: Date.now() - 25 * 60 * 60 * 1000,
            live: false,
        });
        const p = client.init(canvas());
        const init = await posted('INIT');
        expect(init).not.toHaveProperty('forceRenderer');
        worker().reply(init.id, { renderer: 'vello', probed: true });
        await p;
        /* The stale count of 9 is gone: the Vello generation that booted
           starts a fresh, live record at 0. */
        await vi.waitFor(async () => {
            expect((await eventLog.loadRendererStreak())?.count ?? 0).toBe(0);
        });
    });

    it('an unreadable streak is no streak (the boot still proceeds)', async () => {
        const spy = vi.spyOn(eventLog, 'loadRendererStreak').mockRejectedValue(new Error('idb'));
        const p = client.init(canvas());
        const init = await posted('INIT');
        expect(init).not.toHaveProperty('forceRenderer');
        worker().reply(init.id, { renderer: 'canvas2d', probed: true });
        await p;
        spy.mockRestore();
    });
});

describe('recover', () => {
    /** Seed the log: two commands and a snapshot at seq 1. */
    async function seedLog(): Promise<void> {
        await eventLog.appendCommand(1, { type: 'INSERT_TEXT', text: 'a' } as Command);
        await eventLog.appendCommand(2, { type: 'INSERT_TEXT', text: 'b' } as Command);
        await eventLog.persistSnapshot(1, new Uint8Array([7, 7]), undefined, { pin: true });
    }

    async function trapAndRecover(c = canvas()): Promise<{ p: Promise<void>; recover: Sent; c: OffscreenCanvas }> {
        worker().trap();
        const dead = worker();
        const p = client.recover(c);
        await vi.waitFor(() => {
            if (FakeWorker.instances.at(-1) === dead) throw new Error('not respawned yet');
        });
        const recover = await posted('RECOVER');
        return { p, recover, c };
    }

    it('respawns a fresh worker and posts RECOVER with the persisted candidates, tail and lastSeq', async () => {
        await seedLog();
        const before = client.generation;
        const { p, recover, c } = await trapAndRecover();
        expect(client.generation).toBe(before + 1);
        expect(FakeWorker.instances).toHaveLength(2);
        expect(recover).toMatchObject({
            logDb: 'engine-log',
            canvas: c,
            lastSeq: 2,
        });
        const candidates = recover.candidates as eventLog.RecoveryCandidate[];
        /* Newest snapshot first, then the snapshot-less log base. */
        expect(candidates.map((x) => x.seq)).toEqual([1, 0]);
        expect(candidates[0]!.snapshot).toEqual(new Uint8Array([7, 7]));
        expect(candidates[1]!.snapshot).toHaveLength(0);
        expect((recover.commands as eventLog.LoggedCommand[]).map((x) => x.seq)).toEqual([1, 2]);
        expect(recover).not.toHaveProperty('forceRenderer');
        /* The canvas and every snapshot buffer travel as transferables. */
        const transfer = worker().transfers.at(-1)!;
        expect(transfer[0]).toBe(c);
        expect(transfer).toContain(candidates[0]!.snapshot.buffer);
        worker().reply(recover.id, { restored: true, appliedCommands: 1, renderer: 'canvas2d' }, recoveredEvt());
        await p;
    });

    it('folds the worker reply into RecoveryInfo (restored, applied, zoom, degradations, journal gap)', async () => {
        await seedLog();
        const { p, recover } = await trapAndRecover();
        worker().reply(
            recover.id,
            {
                restored: true,
                appliedCommands: 1,
                renderer: 'canvas2d',
                crossOriginIsolated: true,
                snapshotFallbacks: 1,
                packageFallbacks: 2,
                packageLost: true,
                pinnedBase: true,
                tailDropped: true,
                baseTakenAt: 12345,
                journalGap: 3,
                logComplete: true,
            },
            recoveredEvt({ device_scale: 2.5 }),
        );
        await p;
        expect(client.lastRecovery).toMatchObject({
            restored: true,
            appliedCommands: 1,
            renderer: 'canvas2d',
            zoom: 1.5,
            deviceScale: 2.5,
            layoutRestored: true,
            snapshotFallbacks: 1,
            packageFallbacks: 2,
            packageLost: true,
            pinnedBase: true,
            tailDropped: true,
            baseSnapshotAt: 12345,
            journalGap: 3,
            logTruncated: false,
            cause: 'trap',
        } satisfies Partial<RecoveryInfo>);
    });

    it('a recovery that restored nothing from a pruned log reports logTruncated; absent fields default', async () => {
        await seedLog();
        const { p, recover } = await trapAndRecover();
        worker().reply(
            recover.id,
            { restored: false, logComplete: false },
            recoveredEvt({ snapshot_restored: false }),
        );
        await p;
        expect(client.lastRecovery).toMatchObject({
            restored: false,
            logTruncated: true,
            journalGap: 0,
            snapshotFallbacks: 0,
            packageLost: false,
            baseSnapshotAt: undefined,
            layoutRestored: false,
        });
    });

    it('notifies onRecovery listeners only AFTER lastRecovery is final', async () => {
        await seedLog();
        const seen: Array<RecoveryInfo | undefined> = [];
        client.onRecovery((info) => seen.push(client.lastRecovery === info ? info : undefined));
        const { p, recover } = await trapAndRecover();
        worker().reply(recover.id, { restored: true, appliedCommands: 2 }, recoveredEvt());
        await p;
        expect(seen).toHaveLength(1);
        expect(seen[0]).toBeDefined();
    });

    it('an unreadable event log recovers onto an empty log instead of stranding the client', async () => {
        const spy = vi.spyOn(eventLog, 'loadRecoveryLog').mockRejectedValue(new Error('corrupt'));
        const { p, recover } = await trapAndRecover();
        expect(recover.candidates).toEqual([{ seq: 0, snapshot: new Uint8Array(0) }]);
        expect(recover.commands).toEqual([]);
        expect(recover.logComplete).toBe(false);
        worker().reply(recover.id, { restored: false, logComplete: false }, recoveredEvt({ snapshot_restored: false }));
        await p;
        expect(client.lastRecovery?.logTruncated).toBe(true);
        spy.mockRestore();
    });

    it('a failed RECOVER reply rejects', async () => {
        await seedLog();
        const { p, recover } = await trapAndRecover();
        worker().onmessage?.({ data: { id: recover.id, ok: false, error: 'replay blew up' } });
        await expect(p).rejects.toThrow('replay blew up');
    });

    it('requests sent to the dead worker are abandoned when the new generation spawns', async () => {
        const pending = client.dispatch({ type: 'INSERT_TEXT', text: 'x' } as Command);
        pending.catch(() => undefined);
        worker().trap();
        await expect(pending).rejects.toThrow();
        await seedLog();
        const { p, recover } = await trapAndRecover();
        worker().reply(recover.id, { restored: true }, recoveredEvt());
        await p;
        expect(client.generation).toBe(2);
    });
});
