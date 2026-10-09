/* Issue #332 - `EngineClient` against a fake `Worker`: the id-routed
 * pending map, trap handling (#99 / #240), the #260 write epoch, the
 * #388 unsaved-changes fold and the #390 checkpoint feed. No browser, no
 * wasm: the worker is a scripted stub the test drives by hand. */
import 'fake-indexeddb/auto';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { Command, Event } from '../../../crates/engine-wasm/pkg/engine_wasm.js';
import { EngineClient } from './engine-client';

class FakeWorker {
    static instances: FakeWorker[] = [];
    onmessage: ((ev: { data: unknown }) => void) | null = null;
    onerror: ((e: { message: string }) => void) | null = null;
    sent: Array<{ id: number; cmd?: Command; type?: string }> = [];
    terminated = false;
    constructor(
        readonly url: URL,
        readonly options: unknown,
    ) {
        FakeWorker.instances.push(this);
    }
    postMessage(msg: { id: number; cmd?: Command; type?: string }): void {
        this.sent.push(msg);
    }
    terminate(): void {
        this.terminated = true;
    }
    /** The worker answers request `id`. */
    reply(id: number, evt: Event, extra: Record<string, unknown> = {}): void {
        this.onmessage?.({ data: { id, ok: true, evt, ...extra } });
    }
    fail(id: number, error: string): void {
        this.onmessage?.({ data: { id, ok: false, error } });
    }
    /** An unsolicited, id-less message (a11y delta, checkpoint notice). */
    push(evt: Event): void {
        this.onmessage?.({ data: { evt } });
    }
    trap(error = 'unreachable'): void {
        this.onmessage?.({ data: { ok: false, trap: true, error } });
    }
    get last(): { id: number; cmd?: Command } {
        const m = this.sent.at(-1);
        if (!m) throw new Error('nothing sent');
        return m;
    }
}

const insert = { type: 'INSERT_TEXT', text: 'a' } as Command;
const select = { type: 'HIT_TEST', x: 0, y: 0 } as unknown as Command; // read-only
const ev = (e: Record<string, unknown>): Event => e as unknown as Event;
const painted = ev({ type: 'PAINTED' });

let onCrash: ReturnType<typeof vi.fn<() => void>>;
let client: EngineClient;
const worker = (): FakeWorker => FakeWorker.instances.at(-1)!;

beforeEach(() => {
    FakeWorker.instances = [];
    vi.stubGlobal('Worker', FakeWorker);
    vi.spyOn(console, 'error').mockImplementation(() => undefined);
    onCrash = vi.fn<() => void>();
    client = new EngineClient('doc', onCrash);
});
afterEach(() => vi.useRealTimers());

describe('request routing', () => {
    it('spawns one module worker (generation 1)', () => {
        expect(FakeWorker.instances).toHaveLength(1);
        expect(client.generation).toBe(1);
        expect(worker().options).toEqual({ type: 'module' });
    });

    it('matches replies to requests by id, even out of order', async () => {
        const a = client.dispatch(insert);
        const b = client.dispatch(select);
        const [ma, mb] = worker().sent;
        expect(ma!.id).not.toBe(mb!.id);
        worker().reply(mb!.id, ev({ type: 'HIT_RESULT', tag: 'b' }));
        worker().reply(ma!.id, ev({ type: 'PAINTED', tag: 'a' }));
        expect(await a).toMatchObject({ tag: 'a' });
        expect(await b).toMatchObject({ tag: 'b' });
    });

    it('rejects with the worker error text on a failed reply', async () => {
        const p = client.dispatch(insert);
        worker().fail(worker().last.id, 'boom');
        await expect(p).rejects.toThrow('boom');
    });

    it('fans every reply event out to subscribers; unsubscribe stops it', async () => {
        const seen: string[] = [];
        const off = client.subscribe((e) => seen.push(e.type));
        const p = client.dispatch(select);
        worker().reply(worker().last.id, ev({ type: 'HIT_RESULT' }));
        await p;
        worker().push(ev({ type: 'A11Y_DELTA' }));
        off();
        worker().push(ev({ type: 'IGNORED' }));
        expect(seen).toEqual(['HIT_RESULT', 'A11Y_DELTA']);
    });

    it('ignores a reply for an unknown id', async () => {
        const p = client.dispatch(select);
        worker().reply(9999, painted);
        worker().reply(worker().last.id, painted);
        await expect(p).resolves.toBeDefined();
    });
});

describe('write epoch and writes in flight (#260 / #57)', () => {
    it('a write bumps the epoch when its reply lands, BEFORE subscribers see it', async () => {
        const epochs: number[] = [];
        client.subscribe(() => epochs.push(client.writeEpoch));
        const before = client.writeEpoch;
        const p = client.dispatch(insert);
        expect(client.writesInFlight).toBe(1);
        expect(client.writeEpoch).toBe(before);
        worker().reply(worker().last.id, painted);
        await p;
        expect(epochs).toEqual([before + 1]);
        expect(client.writeEpoch).toBe(before + 1);
        expect(client.writesInFlight).toBe(0);
    });

    it('a read-only command never moves the epoch or counts as a write', async () => {
        const before = client.writeEpoch;
        const p = client.dispatch(select);
        expect(client.writesInFlight).toBe(0);
        worker().reply(worker().last.id, ev({ type: 'HIT_RESULT' }));
        await p;
        expect(client.writeEpoch).toBe(before);
    });

    it('an id-less event (a11y delta) does not move the epoch', () => {
        const before = client.writeEpoch;
        worker().push(ev({ type: 'A11Y_DELTA' }));
        expect(client.writeEpoch).toBe(before);
    });

    it('counts concurrent writes and releases them even when the reply is an error', async () => {
        const a = client.dispatch(insert);
        const b = client.dispatch(insert);
        expect(client.writesInFlight).toBe(2);
        worker().fail(worker().sent[0]!.id, 'nope');
        await expect(a).rejects.toThrow();
        expect(client.writesInFlight).toBe(1);
        worker().reply(worker().sent[1]!.id, painted);
        await b;
        expect(client.writesInFlight).toBe(0);
    });
});

describe('trap handling', () => {
    it('rejects every pending request, abandons writes (epoch moves) and hands off to onCrash once', async () => {
        const w = worker();
        const a = client.dispatch(insert);
        const b = client.dispatch(select);
        const epoch = client.writeEpoch;
        const seen: Event[] = [];
        client.subscribe((e) => seen.push(e));
        w.trap('RuntimeError: unreachable');
        await expect(a).rejects.toThrow('engine worker trapped; recovering');
        await expect(b).rejects.toThrow('engine worker trapped; recovering');
        expect(client.writeEpoch).toBeGreaterThan(epoch);
        expect(client.writesInFlight).toBe(0);
        expect(onCrash).toHaveBeenCalledTimes(1);
        // the dead worker never emits TRAP itself: the client synthesizes it
        expect(seen).toEqual([{ type: 'TRAP', stack: 'RuntimeError: unreachable' }]);
    });

    it('drops a duplicate trap report (trap message + onerror)', () => {
        worker().trap();
        worker().onerror?.({ message: 'again' });
        worker().trap();
        expect(onCrash).toHaveBeenCalledTimes(1);
    });

    it('a worker error event is a trap too', async () => {
        const p = client.dispatch(insert);
        worker().onerror?.({ message: 'worker blew up' });
        await expect(p).rejects.toThrow('recovering');
        expect(onCrash).toHaveBeenCalledTimes(1);
    });

    it('until recover() runs, dispatch fails fast instead of posting to a dead worker', async () => {
        worker().trap();
        const w = worker();
        const posted = w.sent.length;
        await expect(client.dispatch(insert)).rejects.toThrow('recovery in progress');
        expect(w.sent).toHaveLength(posted);
    });

    it('a reply that races the trap resolves its request, never both', async () => {
        const p = client.dispatch(insert);
        const id = worker().last.id;
        worker().reply(id, painted);
        worker().trap();
        await expect(p).resolves.toBeDefined();
    });

    it('forceTrap terminates the worker and runs the same path', async () => {
        const p = client.dispatch(insert);
        client.forceTrap();
        expect(worker().terminated).toBe(true);
        await expect(p).rejects.toThrow();
        expect(onCrash).toHaveBeenCalledTimes(1);
    });
});

describe('unsaved-changes fold (#388)', () => {
    async function send(cmd: Command, evt: Event): Promise<void> {
        const p = client.dispatch(cmd);
        worker().reply(worker().last.id, evt);
        await p;
    }

    it('starts clean, goes dirty on an accepted edit, clean again on a successful save', async () => {
        expect(client.hasUnsavedChanges).toBe(false);
        await send(insert, painted);
        expect(client.hasUnsavedChanges).toBe(true);
        await send({ type: 'SAVE_DOCX' } as Command, ev({ type: 'DOCUMENT_SAVED' }));
        expect(client.hasUnsavedChanges).toBe(false);
    });

    it('a refused edit (ERROR reply event) changes nothing', async () => {
        await send(insert, ev({ type: 'ERROR', message: 'no' }));
        expect(client.hasUnsavedChanges).toBe(false);
    });

    it('a rejected request (error reply) leaves the state alone', async () => {
        const p = client.dispatch(insert);
        worker().fail(worker().last.id, 'x');
        await p.catch(() => undefined);
        expect(client.hasUnsavedChanges).toBe(false);
    });
});

describe('checkpoint health feed (#333 / #390)', () => {
    const state = (o: Record<string, unknown>): Event =>
        ev({ type: 'CHECKPOINT_STATE', ok: true, failures: 0, journal_failing: false, ...o });

    it('starts healthy', () => {
        expect(client.checkpointStatus).toEqual({ failing: false, failures: 0 });
    });

    it('folds the worker notice into the status, counts failures, and recovers', () => {
        const seen: unknown[] = [];
        const failed: number[] = [];
        client.onCheckpointStatus((s) => seen.push(s));
        client.onCheckpointFailure((n: number) => failed.push(n));
        worker().push(state({ ok: true, failures: 1, last_error: 'quota' }));
        expect(client.checkpointStatus).toMatchObject({ failing: false, failures: 1, lastError: 'quota' });
        worker().push(state({ ok: false, failures: 4, journal_failing: true }));
        expect(client.checkpointStatus).toMatchObject({
            failing: true,
            failures: 4,
            journalFailing: true,
        });
        worker().push(state({ ok: true, failures: 0 }));
        expect(client.checkpointStatus.failing).toBe(false);
        expect(failed).toEqual([1, 4]);
        expect(seen.length).toBeGreaterThanOrEqual(2);
    });
});
