/* Issue #438 - the #57 / #260 synchronous clipboard cache: an entry is keyed
 * on (write epoch, engine document revision, selection key) and a hit also
 * needs zero writes in flight. Needs Solid's browser build (`createMemo` /
 * `createEffect`), which the vitest config provides. */
import { createRoot, createSignal } from 'solid-js';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { PREFETCH_DEBOUNCE_MS, createClipboardPrefetch } from './clipboard-cache';
import type { ClipboardPrefetch } from './clipboard-cache';
import type { EngineClient } from '../engine/engine-client';
import type { EngineStore } from '../state/engine-store';
import type { Event } from '../engine/types';

const pos = (offset: number) => ({ path: { steps: [{ kind: 'BLOCK', idx: 0 }] }, offset });
const range = (a: number, b: number) => ({ start: pos(a), end: pos(b) });

class FakeClient {
    writeEpoch = 0;
    writesInFlight = 0;
    subscribers = new Set<(e: Event) => void>();
    dispatched: Array<Record<string, unknown>> = [];
    /** Replies are held until the test releases them. */
    private waiting: Array<(e: Event) => void> = [];
    subscribe(fn: (e: Event) => void): () => void {
        this.subscribers.add(fn);
        return () => this.subscribers.delete(fn);
    }
    dispatch(cmd: Record<string, unknown>): Promise<Event> {
        this.dispatched.push(cmd);
        return new Promise((resolve) => this.waiting.push(resolve));
    }
    reply(plain: string, html = `<p>${plain}</p>`): void {
        this.waiting.shift()!({ type: 'CLIPBOARD_PAYLOAD', plain, html } as unknown as Event);
    }
    emit(e: Record<string, unknown>): void {
        for (const s of [...this.subscribers]) s(e as unknown as Event);
    }
    selection(start: number, end: number, revision?: number, story?: unknown): void {
        this.emit({
            type: 'SELECTION_CHANGED',
            range: range(start, end),
            ...(revision !== undefined ? { document_revision: revision } : {}),
            ...(story !== undefined ? { editing_story: story } : {}),
        });
    }
}

let client: FakeClient;
let cache: ClipboardPrefetch;
let dispose: () => void;
let setRange: (r: ReturnType<typeof range>) => void;

/** Move the store's selection AND emit the matching engine event. */
function select(start: number, end: number, revision?: number, story?: unknown): void {
    setRange(range(start, end));
    client.selection(start, end, revision, story);
}

const settle = async (): Promise<void> => {
    await vi.advanceTimersByTimeAsync(PREFETCH_DEBOUNCE_MS);
};

beforeEach(() => {
    vi.useFakeTimers();
    client = new FakeClient();
    dispose = createRoot((d) => {
        const [r, set] = createSignal(range(0, 0));
        setRange = set;
        const store = { selection: () => ({ range: r() }) } as unknown as EngineStore;
        cache = createClipboardPrefetch(client as unknown as EngineClient, store);
        return d;
    });
});
afterEach(() => {
    dispose();
    vi.useRealTimers();
});

/** Prefetch for [a, b) at `revision` and let the reply land. */
async function warm(a: number, b: number, revision: number, plain = 'text'): Promise<void> {
    select(a, b, revision);
    await settle();
    client.reply(plain);
    await vi.advanceTimersByTimeAsync(0);
}

describe('clipboard prefetch cache (#57 / #260)', () => {
    it('a collapsed selection never prefetches', async () => {
        select(3, 3, 1);
        await settle();
        expect(client.dispatched).toEqual([]);
        expect(cache.take()).toBeNull();
    });

    it('prefetches once the selection SETTLES (trailing debounce), asking the engine to skip the docx', async () => {
        select(0, 5, 1);
        await vi.advanceTimersByTimeAsync(PREFETCH_DEBOUNCE_MS - 1);
        expect(client.dispatched).toEqual([]);
        /* A further change inside the window restarts the clock. */
        select(0, 6, 1);
        await vi.advanceTimersByTimeAsync(PREFETCH_DEBOUNCE_MS - 1);
        expect(client.dispatched).toEqual([]);
        await vi.advanceTimersByTimeAsync(1);
        expect(client.dispatched).toEqual([
            { type: 'GET_SELECTION_AS_CLIPBOARD', include_docx: false },
        ]);
        expect(cache.stats()).toMatchObject({ prefetches: 1, allSkippedDocx: true });
    });

    it('serves a hit synchronously for the unchanged selection, revision and epoch', async () => {
        await warm(0, 5, 7, 'hello');
        expect(cache.stats().warm).toBe(true);
        expect(cache.take()).toEqual({ plain: 'hello', html: '<p>hello</p>' });
        expect(cache.stats()).toMatchObject({ hits: 1, misses: 0 });
    });

    it('a different engine document revision on the same selection misses (keyed on revision)', async () => {
        await warm(0, 5, 7);
        /* The same range, but the document moved under it (worker-internal
           mutation: no client command answered). */
        client.selection(0, 5, 8);
        expect(cache.take()).toBeNull();
        expect(cache.stats().misses).toBe(1);
    });

    it('PAINTED.mutation_seq moves the revision too', async () => {
        await warm(0, 5, 7);
        client.emit({ type: 'PAINTED', mutation_seq: 7 });
        expect(cache.take()).not.toBeNull();
        client.emit({ type: 'PAINTED', mutation_seq: 9 });
        expect(cache.take()).toBeNull();
    });

    it('an event that omits the revision leaves it unchanged (additive wire field)', async () => {
        await warm(0, 5, 7);
        client.emit({ type: 'PAINTED' });
        expect(cache.take()).not.toBeNull();
        client.selection(0, 5);
        expect(cache.take()).not.toBeNull();
    });

    it('a moved selection range misses', async () => {
        await warm(0, 5, 7);
        select(0, 6, 7);
        expect(cache.take()).toBeNull();
    });

    it('the editing story is part of the selection key (same numbers, different text)', async () => {
        select(0, 5, 7, { kind: 'Header' });
        await settle();
        client.reply('in the header');
        await vi.advanceTimersByTimeAsync(0);
        expect(cache.take()).not.toBeNull();
        client.selection(0, 5, 7); // same range, now the body
        expect(cache.take()).toBeNull();
    });

    it('a write epoch that moved misses, and re-arms a prefetch', async () => {
        await warm(0, 5, 7);
        client.writeEpoch += 1;
        client.emit({ type: 'PAINTED' });
        expect(cache.take()).toBeNull();
        await settle();
        expect(client.dispatched).toHaveLength(2);
    });

    it('a hit needs zero writes in flight at copy time', async () => {
        await warm(0, 5, 7);
        client.writesInFlight = 1;
        expect(cache.take()).toBeNull();
        client.writesInFlight = 0;
        expect(cache.take()).not.toBeNull();
    });

    it('does not prefetch while a write is in flight; retries once it has settled', async () => {
        client.writesInFlight = 1;
        select(0, 5, 7);
        await settle();
        expect(client.dispatched).toEqual([]);
        client.writesInFlight = 0;
        await settle();
        expect(client.dispatched).toHaveLength(1);
    });

    it('drops a prefetch reply that arrives after the revision moved', async () => {
        select(0, 5, 7);
        await settle();
        client.selection(0, 5, 8); // the document changed while the reply was in flight
        client.reply('stale');
        await vi.advanceTimersByTimeAsync(0);
        expect(cache.take()).toBeNull();
        expect(cache.stats().warm).toBe(false);
    });

    it('drops a reply that arrives after the selection collapsed', async () => {
        select(0, 5, 7);
        await settle();
        select(5, 5, 7);
        client.reply('stale');
        await vi.advanceTimersByTimeAsync(0);
        expect(cache.take()).toBeNull();
    });

    it('invalidate() drops the entry and an in-flight prefetch whose key still matches (e2e hook)', async () => {
        await warm(0, 5, 7);
        cache.invalidate();
        expect(cache.take()).toBeNull();
        select(0, 5, 8);
        await settle();
        cache.invalidate();
        client.reply('late');
        await vi.advanceTimersByTimeAsync(0);
        expect(cache.take()).toBeNull();
    });

    it('a failed prefetch is non-fatal (the next copy takes the async path)', async () => {
        const failing = {
            ...client,
            dispatch: () => Promise.reject(new Error('worker gone')),
        };
        client.dispatch = failing.dispatch as unknown as FakeClient['dispatch'];
        select(0, 5, 7);
        await settle();
        await vi.advanceTimersByTimeAsync(0);
        expect(cache.take()).toBeNull();
    });

    it('the disabled switch never prefetches or hits', async () => {
        dispose();
        let off!: ClipboardPrefetch;
        dispose = createRoot((d) => {
            const [r] = createSignal(range(0, 5));
            off = createClipboardPrefetch(
                client as unknown as EngineClient,
                { selection: () => ({ range: r() }) } as unknown as EngineStore,
                false,
            );
            return d;
        });
        client.selection(0, 5, 1);
        await settle();
        expect(client.dispatched).toEqual([]);
        expect(off.take()).toBeNull();
    });

    it('unsubscribes on cleanup', () => {
        expect(client.subscribers.size).toBe(1);
        dispose();
        expect(client.subscribers.size).toBe(0);
    });
});
