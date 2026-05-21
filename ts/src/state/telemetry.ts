/* Phase 5 D5.7 — UI telemetry collector (PHASE_5_HARDENING_RELEASE.md §11).
 *
 * Subscribes to the engine event stream, accumulates paint / error / font /
 * stats samples, and every 60 s emits a batch. The transport is a MOCK: the
 * batch is `console.log`'d instead of POSTed to a collector — there is no live
 * Grafana for the MVP.
 *
 * The batch shape mirrors the canonical Rust schema in
 * `crates/bridge/src/telemetry.rs`. No PII: `doc_id` is an opaque per-session
 * id, never a document title or path. */
import type { EngineClient } from '../engine/engine-client';
import type { Event } from '../engine/types';

type ErrorCode = 'ENGINE_TRAP' | 'DOCUMENT_PARSE' | 'FONT_LOAD' | 'RPC' | 'UNKNOWN';

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
    | { type: 'FONT_FALLBACK'; script: string; requested: string; fallback: string };

interface TelemetryEvent {
    doc_id: string;
    kind: TelemetryKind;
    timestamp_ms: number;
}

const FLUSH_INTERVAL_MS = 60_000;

function percentile(samples: number[], p: number): number {
    if (!samples.length) return 0;
    const sorted = [...samples].sort((a, b) => a - b);
    const rank = Math.ceil((p / 100) * sorted.length) - 1;
    return sorted[Math.min(sorted.length - 1, Math.max(0, rank))]!;
}

/**
 * Start the mock telemetry pipeline. Returns a teardown that stops the flush
 * timer and unsubscribes. Exposes `window.__telemetryFlush()` to force a flush
 * without waiting for the interval (debug + e2e hook).
 */
export function startTelemetry(client: EngineClient): () => void {
    /* Opaque per-session identifier — §11: no PII in telemetry. */
    const docId = `anon-${Math.random().toString(36).slice(2, 10)}`;
    const paintSamples: number[] = [];
    const pending: TelemetryEvent[] = [];

    const sample = (kind: TelemetryKind): TelemetryEvent => ({
        doc_id: docId,
        kind,
        timestamp_ms: performance.now(),
    });

    const unsubscribe = client.subscribe((e: Event) => {
        if (e.type === 'PAINTED') {
            paintSamples.push(e.paint_ms);
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
            pending.push(sample({ type: 'ERROR', code: 'ENGINE_TRAP', recoverable: true }));
        } else if (e.type === 'ERROR') {
            pending.push(sample({ type: 'ERROR', code: 'UNKNOWN', recoverable: true }));
        }
    });

    const flush = async (): Promise<void> => {
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
            const stats = await client.dispatch({ type: 'REQUEST_STATS' });
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
        const batch = { events, sent_at_ms: performance.now() };
        /* Mock transport: a real collector would receive an HTTP POST here. */
        console.log(`[telemetry] batch (mock POST, ${events.length} events):`, JSON.stringify(batch));
    };

    const timer = window.setInterval(() => void flush(), FLUSH_INTERVAL_MS);
    window.__telemetryFlush = flush;

    return () => {
        window.clearInterval(timer);
        unsubscribe();
    };
}
