/* Phase 5 D5.7 — UI telemetry collector, Issue #86 real transport.
 *
 * Subscribes to the engine event stream, accumulates paint / error / font /
 * stats / crash / doc-open samples, and flushes a batch every 60 s (and
 * immediately on a forced crash, plus once more on `visibilitychange` so a
 * batch survives a tab close). The transport is pluggable: `ConsoleTransport`
 * (dev default — no endpoint configured means no network call) or
 * `BeaconTransport` (`navigator.sendBeacon`, batched + retried).
 *
 * Telemetry is opt-in only — see `TelemetryOptions.enabled`. When disabled,
 * NOTHING is collected and NOTHING is sent: the event subscriber and the
 * dispatch-wrapper both no-op, and `flush()` drops any samples still
 * buffered from before the flag flipped off.
 *
 * The batch shape mirrors the canonical Rust schema in
 * `crates/bridge/src/telemetry.rs`. No PII, no document content: `doc_id` is
 * an opaque per-session id; the crash sample's `recent_commands` carries only
 * dispatched `Command.type` tags (e.g. `"INSERT_TEXT"`), never a command's
 * payload (which, for `InsertText`, IS document content). */
import type { Command, Event } from '../engine/types';

type ErrorCode = 'ENGINE_TRAP' | 'DOCUMENT_PARSE' | 'FONT_LOAD' | 'RPC' | 'UNKNOWN';
type RecoveryOutcome = 'RECOVERED' | 'FAILED' | 'PENDING';

type TelemetryKind =
    | { type: 'PAINT_TIMING'; p50: number; p95: number; p99: number }
    | { type: 'COMMAND_TIMING'; kind: string; p50: number; p95: number }
    | {
          type: 'ENGINE_STATS';
          wasm_heap_bytes: number;
          document_tree_bytes: number;
          glyph_cache_entries: number;
          undo_stack_depth: number;
          fonts_resident: number;
          last_paint_ms: number;
          last_command_ms: number;
      }
    | { type: 'ERROR'; code: ErrorCode; recoverable: boolean }
    | { type: 'FONT_FALLBACK'; script: string; requested: string; fallback: string }
    | {
          type: 'CRASH';
          trap_message: string;
          recent_commands: string[];
          recovery_outcome: RecoveryOutcome;
      }
    | { type: 'DOC_OPEN'; size_bytes: number; page_count: number; open_ms: number; backend: string };

interface TelemetryEvent {
    doc_id: string;
    kind: TelemetryKind;
    timestamp_ms: number;
}

interface TelemetryBatch {
    events: TelemetryEvent[];
    sent_at_ms: number;
}

/** Duck-typed subset of `EngineClient` telemetry actually needs — matches
 *  both the concrete `EngineClient` (`ts/src/engine/engine-client.ts`) and
 *  `@nge/core`'s `EngineHandle`, so either can drive this pipeline. */
export interface TelemetryClient {
    dispatch(cmd: Command, transfer?: Transferable[]): Promise<Event>;
    subscribe(fn: (e: Event) => void): () => void;
    readonly renderer: string;
}

const FLUSH_INTERVAL_MS = 60_000;
/** Bounded breadcrumb trail for `CRASH.recent_commands` — enough to
 *  reconstruct what led to a trap without unbounded growth. Every
 *  dispatched command's `type` tag lands here, including internal polling
 *  (`REQUEST_STATS`) — that's still zero PII, just occasionally low signal. */
const MAX_RECENT_COMMANDS = 20;
/** How long to wait for `Event::Recovered` after a trap before a `CRASH`
 *  sample's `recovery_outcome` gives up and reports `FAILED`. */
const RECOVERY_TIMEOUT_MS = 8_000;
/** How long a `DOC_OPEN` sample waits for the confirming `PAINTED` broadcast
 *  that carries `page_count` before giving up on the correlation. */
const DOC_OPEN_CORRELATION_MS = 5_000;

function percentile(samples: number[], p: number): number {
    if (!samples.length) return 0;
    const sorted = [...samples].sort((a, b) => a - b);
    const rank = Math.ceil((p / 100) * sorted.length) - 1;
    return sorted[Math.min(sorted.length - 1, Math.max(0, rank))]!;
}

/** Pluggable sink for a finished `TelemetryBatch`. */
export interface TelemetryTransport {
    send(batch: TelemetryBatch): void | Promise<void>;
    /** Optional: re-attempt anything the transport couldn't send the first
     *  time (e.g. the browser's `sendBeacon` queue was momentarily full).
     *  Called on `visibilitychange` in addition to the normal flush. */
    retry?(): void;
}

/** Dev-safe default: no network call, ever. Used when no `endpoint` and no
 *  explicit `transport` is configured — opting in locally during dev never
 *  fires a real request by accident. */
export class ConsoleTransport implements TelemetryTransport {
    send(batch: TelemetryBatch): void {
        console.log(`[telemetry] batch (console transport, ${batch.events.length} events):`, JSON.stringify(batch));
    }
}

/** Production default once an `endpoint` is configured. `navigator.sendBeacon`
 *  queues the request and survives page unload — the reason beacons exist —
 *  but the browser caps the number of in-flight beacons per origin, so a
 *  rejected `sendBeacon` call keeps its batch queued for the next `send` or
 *  `retry()` instead of dropping it. */
export class BeaconTransport implements TelemetryTransport {
    private queue: TelemetryBatch[] = [];

    constructor(private endpoint: string) {}

    send(batch: TelemetryBatch): void {
        this.queue.push(batch);
        this.retry();
    }

    retry(): void {
        if (typeof navigator === 'undefined' || typeof navigator.sendBeacon !== 'function') {
            /* No `sendBeacon` in this environment (non-browser test runner,
               or a very old browser) — nothing else can carry a batch past
               page unload, so drop rather than buffer forever. */
            this.queue.length = 0;
            return;
        }
        while (this.queue.length) {
            const batch = this.queue[0]!;
            const body = new Blob([JSON.stringify(batch)], { type: 'application/json' });
            const accepted = navigator.sendBeacon(this.endpoint, body);
            if (!accepted) break; // browser's beacon queue is full — retry later
            this.queue.shift();
        }
    }
}

export interface TelemetryOptions {
    /** Reactive opt-in flag, checked on every event and every flush tick —
     *  a toggle takes effect within one flush cycle, no pipeline restart
     *  needed. Defaults to permanently disabled: telemetry is opt-in only
     *  (Issue #86). When `false`, nothing is collected and nothing is sent. */
    enabled?: () => boolean;
    /** Collector URL for the default `BeaconTransport`. With no endpoint and
     *  no explicit `transport`, samples fall back to `ConsoleTransport`.
     *  Typed `| undefined` (not just optional) so a caller can forward an
     *  already-optional value — e.g. a `URLSearchParams` lookup — without
     *  fighting `exactOptionalPropertyTypes`. */
    endpoint?: string | undefined;
    transport?: TelemetryTransport;
}

/**
 * Start the telemetry pipeline. Returns a teardown that stops the flush
 * timer, the `visibilitychange` listener, and the event subscription, and
 * restores the client's original `dispatch`. Exposes
 * `window.__telemetryFlush()` to force a flush without waiting for the
 * interval (debug + e2e hook).
 */
export function startTelemetry(client: TelemetryClient, options: TelemetryOptions = {}): () => void {
    const isEnabled = options.enabled ?? ((): boolean => false);
    const transport: TelemetryTransport =
        options.transport ?? (options.endpoint ? new BeaconTransport(options.endpoint) : new ConsoleTransport());

    /* Opaque per-session identifier — never PII, never a document title. */
    const docId = `anon-${Math.random().toString(36).slice(2, 10)}`;
    const paintSamples: number[] = [];
    const pending: TelemetryEvent[] = [];
    const recentCommands: string[] = [];
    let pendingDocOpen: { sizeBytes: number; openMs: number; deadlineMs: number } | null = null;

    const sample = (kind: TelemetryKind): TelemetryEvent => ({
        doc_id: docId,
        kind,
        timestamp_ms: performance.now(),
    });

    const queueAndFlush = (kind: TelemetryKind): void => {
        if (!isEnabled()) return;
        pending.push(sample(kind));
        void flush();
    };

    /** Issue #86 — a WASM trap can't build its own telemetry (the crashed
     *  module is the thing that would build it), so this synthesizes a
     *  `CRASH` sample from the bridge-shaped `Event::Trap`, the same way
     *  `EngineClient.onTrap` synthesizes `Event::Trap` itself. Emits
     *  immediately with `PENDING` (so a sample reaches the sink without
     *  waiting on recovery), then races `Event::Recovered` against a
     *  timeout to emit a corrected follow-up. */
    const armCrashSample = (trapMessage: string): void => {
        const recentCommandsSnapshot = [...recentCommands];
        const emit = (outcome: RecoveryOutcome): void => {
            queueAndFlush({
                type: 'CRASH',
                trap_message: trapMessage,
                recent_commands: recentCommandsSnapshot,
                recovery_outcome: outcome,
            });
        };
        emit('PENDING');
        if (!isEnabled()) return;
        let settled = false;
        const offRecovered = client.subscribe((e2: Event) => {
            if (settled || e2.type !== 'RECOVERED') return;
            settled = true;
            window.clearTimeout(timer);
            offRecovered();
            emit('RECOVERED');
        });
        const timer = window.setTimeout(() => {
            if (settled) return;
            settled = true;
            offRecovered();
            emit('FAILED');
        }, RECOVERY_TIMEOUT_MS);
    };

    const unsubscribe = client.subscribe((e: Event) => {
        if (!isEnabled()) return;
        if (e.type === 'PAINTED') {
            paintSamples.push(e.paint_ms);
            if (pendingDocOpen) {
                if (performance.now() <= pendingDocOpen.deadlineMs) {
                    pending.push(
                        sample({
                            type: 'DOC_OPEN',
                            size_bytes: pendingDocOpen.sizeBytes,
                            page_count: e.page_count,
                            open_ms: pendingDocOpen.openMs,
                            backend: client.renderer,
                        }),
                    );
                }
                pendingDocOpen = null;
            }
        } else if (e.type === 'FONT_MISSING') {
            pending.push(
                sample({
                    type: 'FONT_FALLBACK',
                    script: e.script,
                    requested: e.requested,
                    fallback: 'system-fallback',
                }),
            );
        } else if (e.type === 'TRAP') {
            armCrashSample(e.stack);
        } else if (e.type === 'ERROR') {
            pending.push(sample({ type: 'ERROR', code: 'UNKNOWN', recoverable: true }));
        }
    });

    /* Issue #86 — instrument `dispatch` in place (rather than wrapping the
       client in a new object) so every other holder of this SAME client
       reference — `createEditorCommands`, the toolbar, `App.tsx` itself —
       transparently gets the instrumented version too. Never records a
       command's payload, only its `type` tag: an `InsertText.text` field IS
       document content. */
    const originalDispatch = client.dispatch.bind(client);
    client.dispatch = (async (cmd: Command, transfer?: Transferable[]) => {
        if (isEnabled()) {
            recentCommands.push(cmd.type);
            if (recentCommands.length > MAX_RECENT_COMMANDS) recentCommands.shift();
        }
        let openStart = 0;
        let openSizeBytes = 0;
        const isOpen = cmd.type === 'OPEN_DOCUMENT';
        if (cmd.type === 'OPEN_DOCUMENT') {
            openStart = performance.now();
            openSizeBytes = cmd.bytes.byteLength;
        }
        const result = await originalDispatch(cmd, transfer);
        if (isOpen && isEnabled() && result.type === 'DOCUMENT_LOADED') {
            pendingDocOpen = {
                sizeBytes: openSizeBytes,
                openMs: performance.now() - openStart,
                deadlineMs: performance.now() + DOC_OPEN_CORRELATION_MS,
            };
        }
        return result;
    }) as typeof client.dispatch;

    const flush = async (): Promise<void> => {
        if (!isEnabled()) {
            /* Opt-out: drop anything buffered from before the flag flipped
               off. Nothing is ever sent while disabled. */
            paintSamples.length = 0;
            pending.length = 0;
            return;
        }
        const events: TelemetryEvent[] = pending.splice(0, pending.length);

        if (paintSamples.length) {
            events.push(
                sample({
                    type: 'PAINT_TIMING',
                    p50: percentile(paintSamples, 50),
                    p95: percentile(paintSamples, 95),
                    p99: percentile(paintSamples, 99),
                }),
            );
            paintSamples.length = 0;
        }

        /* Always sample EngineStats so a window's batch is never empty. */
        try {
            const stats = await originalDispatch({ type: 'REQUEST_STATS' });
            if (stats.type === 'STATS') {
                events.push(
                    sample({
                        type: 'ENGINE_STATS',
                        wasm_heap_bytes: stats.wasm_heap_bytes,
                        document_tree_bytes: stats.document_tree_bytes,
                        glyph_cache_entries: stats.glyph_cache_entries,
                        undo_stack_depth: stats.undo_stack_depth,
                        fonts_resident: stats.fonts_resident,
                        last_paint_ms: stats.last_paint_ms,
                        last_command_ms: stats.last_command_ms,
                    }),
                );
            }
        } catch {
            /* Engine unavailable (e.g. mid-crash) — skip the stats sample. */
        }

        if (!events.length) return;
        await transport.send({ events, sent_at_ms: performance.now() });
    };

    const onVisibilityChange = (): void => {
        if (document.visibilityState !== 'hidden') return;
        void flush();
        transport.retry?.();
    };
    document.addEventListener('visibilitychange', onVisibilityChange);

    const timer = window.setInterval(() => void flush(), FLUSH_INTERVAL_MS);
    window.__telemetryFlush = flush;

    return () => {
        window.clearInterval(timer);
        document.removeEventListener('visibilitychange', onVisibilityChange);
        unsubscribe();
        client.dispatch = originalDispatch;
    };
}
