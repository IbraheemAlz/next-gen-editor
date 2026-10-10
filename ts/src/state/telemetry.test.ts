/* Issue #438 - the telemetry pipeline (#86 / #315 / #333 / #390): opt-in
 * gating, sample batching, percentile folding, the CRASH sample's
 * PENDING -> RECOVERED / FAILED follow-up (with the recovery flags, including
 * `journal_gap`), and the transports. Fake timers + a recording transport;
 * no browser, no network. */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { Command, Event } from '../../../crates/engine-wasm/pkg/engine_wasm.js';
import {
    BeaconTransport,
    startTelemetry,
    type TelemetryClient,
    type TelemetryRecoveryReport,
    type TelemetryTransport,
} from './telemetry';

type Batch = Parameters<TelemetryTransport['send']>[0];
type Kind = Batch['events'][number]['kind'];

const stats = {
    type: 'STATS',
    wasm_heap_bytes: 100,
    document_tree_bytes: 10,
    glyph_cache_entries: 3,
    undo_stack_depth: 2,
    fonts_resident: 1,
    last_paint_ms: 0,
    last_command_ms: 0,
} as unknown as Event;

class FakeClient implements TelemetryClient {
    renderer = 'canvas2d';
    subscribers = new Set<(e: Event) => void>();
    recoveryListeners = new Set<(r: TelemetryRecoveryReport) => void>();
    checkpointListeners = new Set<(n: number) => void>();
    dispatched: Command[] = [];
    statsAvailable = true;
    /** The `OPEN_DOCUMENT` reply (issue #329 tests add `substituted`). */
    openReply: Record<string, unknown> = { type: 'DOCUMENT_LOADED' };
    async dispatch(cmd: Command): Promise<Event> {
        this.dispatched.push(cmd);
        if (cmd.type === 'REQUEST_STATS') {
            if (!this.statsAvailable) throw new Error('engine down');
            return stats;
        }
        if (cmd.type === 'OPEN_DOCUMENT') return this.openReply as unknown as Event;
        return { type: 'PAINTED' } as unknown as Event;
    }
    subscribe(fn: (e: Event) => void): () => void {
        this.subscribers.add(fn);
        return () => this.subscribers.delete(fn);
    }
    onRecovery(fn: (r: TelemetryRecoveryReport) => void): () => void {
        this.recoveryListeners.add(fn);
        return () => this.recoveryListeners.delete(fn);
    }
    onCheckpointFailure(fn: (n: number) => void): () => void {
        this.checkpointListeners.add(fn);
        return () => this.checkpointListeners.delete(fn);
    }
    emit(e: Record<string, unknown>): void {
        for (const s of [...this.subscribers]) s(e as unknown as Event);
    }
}

const report = (o: Partial<TelemetryRecoveryReport> = {}): TelemetryRecoveryReport => ({
    restored: true,
    pinnedBase: false,
    tailDropped: false,
    packageLost: false,
    logTruncated: false,
    snapshotFallbacks: 0,
    packageFallbacks: 0,
    rendererDowngrade: undefined,
    ...o,
});

let client: FakeClient;
let batches: Batch[];
let enabled: boolean;
let stop: (() => void) | undefined;
const transport: TelemetryTransport = {
    send: (b) => {
        batches.push(b);
    },
};
const kinds = (): Kind[] => batches.flatMap((b) => b.events.map((e) => e.kind));
const ofType = <T extends Kind['type']>(t: T) =>
    kinds().filter((k): k is Extract<Kind, { type: T }> => k.type === t);

function start(c: TelemetryClient = client): void {
    stop = startTelemetry(c, { enabled: () => enabled, transport });
}
/** Flush via the interval. */
const tick = async (): Promise<void> => {
    await vi.advanceTimersByTimeAsync(60_000);
};

beforeEach(() => {
    vi.useFakeTimers();
    vi.stubEnv('DEV', false);
    const doc = { visibilityState: 'visible', addEventListener: vi.fn(), removeEventListener: vi.fn() };
    vi.stubGlobal('document', doc);
    vi.stubGlobal('window', globalThis);
    client = new FakeClient();
    batches = [];
    enabled = true;
});
afterEach(() => {
    stop?.();
    stop = undefined;
    vi.useRealTimers();
});

describe('opt-in gating (#86)', () => {
    it('collects nothing and sends nothing while disabled, not even the stats sample', async () => {
        enabled = false;
        start();
        client.emit({ type: 'FONT_MISSING', script: 'Arab', requested: 'X' });
        client.emit({ type: 'ERROR', message: 'x' });
        await tick();
        expect(batches).toEqual([]);
        expect(client.dispatched.map((c) => c.type)).not.toContain('REQUEST_STATS');
    });

    it('drops what was buffered before the flag flipped off', async () => {
        start();
        client.emit({ type: 'ERROR', message: 'x' });
        enabled = false;
        await tick();
        enabled = true;
        await tick();
        expect(ofType('ERROR')).toEqual([]);
    });
});

describe('batching and folding', () => {
    it('always samples engine stats so a window is never empty', async () => {
        start();
        await tick();
        expect(batches).toHaveLength(1);
        expect(ofType('ENGINE_STATS')).toHaveLength(1);
        expect(ofType('ENGINE_STATS')[0]).toMatchObject({ wasm_heap_bytes: 100, undo_stack_depth: 2 });
    });

    it('folds the window\'s paint timings into p50 / p95 / p99', async () => {
        start();
        for (let ms = 1; ms <= 100; ms++) client.emit({ type: 'PAINTED', paint_ms: ms, page_count: 1 });
        await tick();
        expect(ofType('PAINT_TIMING')).toEqual([{ type: 'PAINT_TIMING', p50: 50, p95: 95, p99: 99 }]);
        /* The window resets: the next batch has no paint sample. */
        await tick();
        expect(ofType('PAINT_TIMING')).toHaveLength(1);
    });

    it('puts every queued sample of a window into ONE batch', async () => {
        start();
        client.emit({ type: 'FONT_MISSING', script: 'Arab', requested: 'Amiri' });
        client.emit({ type: 'ERROR', message: 'nope' });
        client.emit({ type: 'PAINTED', paint_ms: 4, page_count: 1 });
        await tick();
        expect(batches).toHaveLength(1);
        expect(batches[0]!.events.map((e) => e.kind.type).sort()).toEqual(
            ['ENGINE_STATS', 'ERROR', 'FONT_FALLBACK', 'PAINT_TIMING'].sort(),
        );
        expect(ofType('FONT_FALLBACK')[0]).toMatchObject({ script: 'Arab', requested: 'Amiri' });
        /* An engine error is sampled by class only: never its message. */
        expect(ofType('ERROR')[0]).toEqual({ type: 'ERROR', code: 'UNKNOWN', recoverable: true });
        expect(JSON.stringify(batches)).not.toContain('nope');
    });

    it('samples a layout degradation once per distinct note set', async () => {
        start();
        const note = [{ reason: 'FrozenBlock', page: 2 }];
        client.emit({ type: 'PAINTED', paint_ms: 1, page_count: 1, layout_degraded: note });
        client.emit({ type: 'PAINTED', paint_ms: 1, page_count: 1, layout_degraded: note });
        await tick();
        expect(ofType('LAYOUT_DEGRADED')).toEqual([
            { type: 'LAYOUT_DEGRADED', reason: 'FrozenBlock', page: 2 },
        ]);
    });

    it('sends no batch at all when stats are unavailable and nothing else was sampled', async () => {
        client.statsAvailable = false;
        start();
        await tick();
        expect(batches).toEqual([]);
    });

    it('still sends the queued samples when the stats request fails (mid-crash)', async () => {
        client.statsAvailable = false;
        start();
        client.emit({ type: 'ERROR', message: 'x' });
        await tick();
        expect(kinds().map((k) => k.type)).toEqual(['ERROR']);
    });

    it('carries the typed ErrorKind on an ERROR sample; an untyped one is UNKNOWN (#461)', async () => {
        start();
        client.emit({ type: 'ERROR', message: 'x', kind: 'InvalidArgument' });
        client.emit({ type: 'ERROR', message: 'y' });
        await tick();
        expect(ofType('ERROR')).toEqual([
            { type: 'ERROR', code: 'UNKNOWN', recoverable: true, kind: 'InvalidArgument' },
            { type: 'ERROR', code: 'UNKNOWN', recoverable: true },
        ]);
    });

    it('counts every failed checkpoint write as an ERROR / CHECKPOINT_FAILED sample (#333)', async () => {
        start();
        for (const l of client.checkpointListeners) l(1);
        for (const l of client.checkpointListeners) l(2);
        await tick();
        expect(ofType('ERROR')).toEqual([
            { type: 'ERROR', code: 'CHECKPOINT_FAILED', recoverable: true },
            { type: 'ERROR', code: 'CHECKPOINT_FAILED', recoverable: true },
        ]);
    });

    it('reports an exhausted journal ONCE per run, and again after it healed (#390)', async () => {
        start();
        const state = (o: Record<string, unknown>) =>
            client.emit({ type: 'CHECKPOINT_STATE', ok: true, failures: 0, journal_failing: false, ...o });
        state({ ok: false, failures: 4, journal_failing: true });
        state({ ok: false, failures: 4, journal_failing: true });
        await tick();
        expect(ofType('ERROR').filter((e) => e.code === 'JOURNAL_FAILED')).toHaveLength(1);
        state({}); // healed
        state({ ok: false, failures: 4, journal_failing: true });
        await tick();
        expect(ofType('ERROR').filter((e) => e.code === 'JOURNAL_FAILED')).toHaveLength(2);
    });

    it('a snapshot-only checkpoint failure is not a journal failure', async () => {
        start();
        client.emit({ type: 'CHECKPOINT_STATE', ok: false, failures: 4, journal_failing: false });
        await tick();
        expect(ofType('ERROR')).toEqual([]);
    });
});

describe('DOC_OPEN correlation', () => {
    const open = { type: 'OPEN_DOCUMENT', bytes: new Uint8Array(1234), format: 'Docx' } as unknown as Command;

    it('pairs the open with the next PAINTED page count', async () => {
        start();
        await client.dispatch(open);
        const wrapped = await (client.dispatch as (c: Command) => Promise<Event>)(open);
        expect(wrapped.type).toBe('DOCUMENT_LOADED');
        client.emit({ type: 'PAINTED', paint_ms: 1, page_count: 7 });
        await tick();
        expect(ofType('DOC_OPEN')).toEqual([
            { type: 'DOC_OPEN', size_bytes: 1234, page_count: 7, open_ms: expect.any(Number), backend: 'canvas2d' },
        ]);
    });

    it('carries the font-substitution count, never the families (#329)', async () => {
        start();
        client.openReply = {
            type: 'DOCUMENT_LOADED',
            substituted: [
                { family: 'Calibri', slot: 'Latin', substitute: 'Carlito', substitute_id: 'carlito', metric_compatible: true },
                { family: 'Arial', slot: 'ComplexScript', substitute: 'Noto Naskh Arabic', substitute_id: 'noto-naskh', metric_compatible: false },
            ],
        };
        await client.dispatch(open);
        client.emit({ type: 'PAINTED', paint_ms: 1, page_count: 2 });
        await tick();
        const [sample] = ofType('DOC_OPEN');
        expect(sample).toMatchObject({ type: 'DOC_OPEN', page_count: 2, font_substitutions: 2 });
        expect(JSON.stringify(sample)).not.toContain('Calibri');
    });

    it('gives up when no confirming paint arrives within 5 s', async () => {
        start();
        await client.dispatch(open);
        await vi.advanceTimersByTimeAsync(5001);
        client.emit({ type: 'PAINTED', paint_ms: 1, page_count: 7 });
        await tick();
        expect(ofType('DOC_OPEN')).toEqual([]);
    });
});

describe('breadcrumbs never carry a payload (#86)', () => {
    it('CRASH.recent_commands lists command TYPES only, bounded to the last 20', async () => {
        start();
        for (let i = 0; i < 25; i++) {
            await client.dispatch({ type: 'INSERT_TEXT', text: `secret-${i}` } as unknown as Command);
        }
        client.emit({ type: 'TRAP', stack: 'RuntimeError: unreachable' });
        await vi.advanceTimersByTimeAsync(0);
        const crash = ofType('CRASH')[0]!;
        expect(crash.recent_commands).toHaveLength(20);
        expect(new Set(crash.recent_commands)).toEqual(new Set(['INSERT_TEXT']));
        expect(JSON.stringify(batches)).not.toContain('secret');
    });
});

describe('CRASH sample (#86 / #315)', () => {
    const crashes = () => ofType('CRASH');

    it('emits PENDING straight away, then RECOVERED with the recovery flags (incl. journal_gap)', async () => {
        start();
        client.emit({ type: 'TRAP', stack: 'RuntimeError: unreachable' });
        await vi.advanceTimersByTimeAsync(0);
        expect(crashes().map((c) => c.recovery_outcome)).toEqual(['PENDING']);
        expect(crashes()[0]).not.toHaveProperty('recovery');

        for (const l of [...client.recoveryListeners]) {
            l(report({ pinnedBase: true, tailDropped: true, snapshotFallbacks: 2, journalGap: 3 }));
        }
        await vi.advanceTimersByTimeAsync(0);
        const recovered = crashes().find((c) => c.recovery_outcome === 'RECOVERED')!;
        expect(recovered.recovery).toEqual({
            snapshot_restored: true,
            pinned_base: true,
            tail_dropped: true,
            package_lost: false,
            log_truncated: false,
            snapshot_fallbacks: 2,
            package_fallbacks: 0,
            journal_gap: 3,
        });
    });

    it('a report without journalGap reports 0 (additive)', async () => {
        start();
        client.emit({ type: 'TRAP', stack: 's' });
        for (const l of [...client.recoveryListeners]) l(report());
        await vi.advanceTimersByTimeAsync(0);
        const recovered = crashes().find((c) => c.recovery_outcome === 'RECOVERED')!;
        expect(recovered.recovery?.journal_gap).toBe(0);
    });

    it('carries the crash-loop downgrade on the follow-up', async () => {
        start();
        client.emit({ type: 'TRAP', stack: 's' });
        const downgrade = { from: 'vello', to: 'canvas2d', reason: 'CRASH_LOOP', consecutive_traps: 2 } as const;
        for (const l of [...client.recoveryListeners]) l(report({ rendererDowngrade: downgrade }));
        await vi.advanceTimersByTimeAsync(0);
        expect(crashes().find((c) => c.recovery_outcome === 'RECOVERED')!.renderer_downgrade).toEqual(downgrade);
    });

    it('settles once: a second recovery report does not emit another follow-up', async () => {
        start();
        client.emit({ type: 'TRAP', stack: 's' });
        for (const l of [...client.recoveryListeners]) l(report());
        expect(client.recoveryListeners.size).toBe(0);
        await vi.advanceTimersByTimeAsync(0);
        expect(crashes().filter((c) => c.recovery_outcome === 'RECOVERED')).toHaveLength(1);
    });

    it('gives up after 8 s with FAILED when the recovery never completes', async () => {
        start();
        client.emit({ type: 'TRAP', stack: 's' });
        await vi.advanceTimersByTimeAsync(7999);
        expect(crashes().map((c) => c.recovery_outcome)).toEqual(['PENDING']);
        await vi.advanceTimersByTimeAsync(1);
        expect(crashes().map((c) => c.recovery_outcome)).toEqual(['PENDING', 'FAILED']);
        expect(client.recoveryListeners.size).toBe(0);
    });

    it('without the completed-recovery feed it settles on the bare RECOVERED event, flag-less', async () => {
        /* No `onRecovery` / `onCheckpointFailure`: a client that predates #315. */
        const bare: TelemetryClient = {
            renderer: client.renderer,
            dispatch: (cmd) => client.dispatch(cmd),
            subscribe: (fn) => client.subscribe(fn),
        };
        start(bare);
        client.emit({ type: 'TRAP', stack: 's' });
        client.emit({ type: 'RECOVERED', renderer_downgrade: undefined });
        await vi.advanceTimersByTimeAsync(0);
        const recovered = crashes().find((c) => c.recovery_outcome === 'RECOVERED')!;
        expect(recovered).toBeDefined();
        expect(recovered).not.toHaveProperty('recovery');
    });
});

describe('teardown', () => {
    it('stops the interval, unsubscribes, and restores the original dispatch', async () => {
        start();
        const instrumented = client.dispatch;
        stop!();
        stop = undefined;
        expect(client.dispatch).not.toBe(instrumented);
        expect(client.subscribers.size).toBe(0);
        /* The restored dispatch no longer records breadcrumbs. */
        await client.dispatch({ type: 'INSERT_TEXT', text: 'x' } as unknown as Command);
        client.emit({ type: 'ERROR', message: 'x' });
        await tick();
        expect(batches).toEqual([]);
    });
});

describe('BeaconTransport', () => {
    const batch: Batch = { events: [], sent_at_ms: 1 };

    afterEach(() => vi.unstubAllGlobals());

    it('keeps a batch the browser rejected and retries it, in order', () => {
        const accept = [false, true, true];
        const sendBeacon = vi.fn(() => accept.shift() ?? true);
        vi.stubGlobal('navigator', { sendBeacon });
        const t = new BeaconTransport('https://collector.invalid/t');
        t.send({ ...batch, sent_at_ms: 1 });
        expect(sendBeacon).toHaveBeenCalledTimes(1); // rejected: stays queued
        t.send({ ...batch, sent_at_ms: 2 });
        /* Retried batch 1 first (accepted), then batch 2. */
        expect(sendBeacon).toHaveBeenCalledTimes(3);
        t.retry();
        expect(sendBeacon).toHaveBeenCalledTimes(3); // nothing left
    });

    it('drops rather than buffers forever when sendBeacon does not exist', () => {
        vi.stubGlobal('navigator', {});
        const t = new BeaconTransport('https://collector.invalid/t');
        t.send(batch);
        vi.stubGlobal('navigator', { sendBeacon: vi.fn(() => true) });
        t.retry();
        expect((navigator.sendBeacon as ReturnType<typeof vi.fn>).mock.calls).toHaveLength(0);
    });
});
