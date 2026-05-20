/* Phase 2 D2.6 — event log + crash-recovery storage.
 *
 * These are deliberate stubs. The IndexedDB-backed event log and snapshot
 * persistence (PHASE_2_BRIDGE_MEMORY.md §10) land in the next D2.6 step; the
 * no-op implementations here let the worker and EngineClient compile and run
 * end-to-end without storage. */
import type { Command } from '../../crates/engine-wasm/pkg/engine_wasm.js';

/** Open (or create) the IndexedDB event log for `documentId`. */
export async function openEventLog(documentId: string): Promise<void> {
    /* TODO Phase 2 D2.6: openDB('engine-log', 1, { upgrade ... }). */
}

/** Append one dispatched command to the durable log. */
export async function appendCommand(seq: number, cmd: Command): Promise<void> {
    /* TODO Phase 2 D2.6: db.put('commands', { seq, cmd, at: Date.now() }). */
}

/** Persist an engine snapshot and prune all but the last three. */
export async function persistSnapshot(seq: number, bytes: Uint8Array): Promise<void> {
    /* TODO Phase 2 D2.6: db.put('snapshots', { seq, bytes }) + prune older. */
}

/** Newest snapshot plus the command tail recorded after it (trap recovery). */
export async function loadLatestEventLog(
    documentId: string,
): Promise<{ snapshot: Uint8Array; log: Command[] }> {
    /* TODO Phase 2 D2.6: read newest snapshot + commands with seq > snapshot.seq. */
    return { snapshot: new Uint8Array(0), log: [] };
}
