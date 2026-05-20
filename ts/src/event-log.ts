/* Phase 2 D2.6 — event log + crash-recovery storage (PHASE_2_BRIDGE_MEMORY.md
 * §10). Native IndexedDB; no external dependency.
 *
 * One origin-scoped database `engine-log` with three object stores:
 *   - commands  (keyPath "seq") — every dispatched command, in order
 *   - snapshots (keyPath "seq") — periodic engine snapshots; pruned to 3
 *   - meta      (keyPath "id")  — bookkeeping (which document is logged) */
import type { Command } from '../../crates/engine-wasm/pkg/engine_wasm.js';

const DB_NAME = 'engine-log';
const DB_VERSION = 1;
/** Snapshots retained by `persistSnapshot`; older ones are pruned (§10.2). */
const SNAPSHOTS_KEPT = 3;

interface CommandRow {
    seq: number;
    cmd: Command;
    at: number;
}
interface SnapshotRow {
    seq: number;
    bytes: Uint8Array;
}

/** Resolve when an `IDBRequest` succeeds; reject on error. */
function reqToPromise<T>(req: IDBRequest<T>): Promise<T> {
    return new Promise<T>((resolve, reject) => {
        req.onsuccess = () => resolve(req.result);
        req.onerror = () => reject(req.error ?? new Error('IndexedDB request failed'));
    });
}

/** Resolve when a transaction commits; reject on error/abort. */
function txDone(tx: IDBTransaction): Promise<void> {
    return new Promise<void>((resolve, reject) => {
        tx.oncomplete = () => resolve();
        tx.onerror = () => reject(tx.error ?? new Error('IndexedDB transaction failed'));
        tx.onabort = () => reject(tx.error ?? new Error('IndexedDB transaction aborted'));
    });
}

function openDb(): Promise<IDBDatabase> {
    return new Promise<IDBDatabase>((resolve, reject) => {
        const open = indexedDB.open(DB_NAME, DB_VERSION);
        open.onupgradeneeded = () => {
            const db = open.result;
            if (!db.objectStoreNames.contains('commands')) {
                db.createObjectStore('commands', { keyPath: 'seq' });
            }
            if (!db.objectStoreNames.contains('snapshots')) {
                db.createObjectStore('snapshots', { keyPath: 'seq' });
            }
            if (!db.objectStoreNames.contains('meta')) {
                db.createObjectStore('meta', { keyPath: 'id' });
            }
        };
        open.onsuccess = () => resolve(open.result);
        open.onerror = () => reject(open.error ?? new Error('IndexedDB open failed'));
        open.onblocked = () => reject(new Error('IndexedDB open blocked by another connection'));
    });
}

/** Lazily opened, cached connection — shared by every export below. */
let dbPromise: Promise<IDBDatabase> | null = null;

function getDb(): Promise<IDBDatabase> {
    const existing = dbPromise;
    if (existing) return existing;
    const fresh = openDb();
    dbPromise = fresh;
    return fresh;
}

/** Open the event log and record which document it belongs to. */
export async function openEventLog(documentId: string): Promise<void> {
    const db = await getDb();
    const tx = db.transaction('meta', 'readwrite');
    tx.objectStore('meta').put({ id: 'document', documentId, openedAt: Date.now() });
    await txDone(tx);
}

/** Append one dispatched command to the durable log. */
export async function appendCommand(seq: number, cmd: Command): Promise<void> {
    const db = await getDb();
    const tx = db.transaction('commands', 'readwrite');
    const row: CommandRow = { seq, cmd, at: Date.now() };
    tx.objectStore('commands').put(row);
    await txDone(tx);
}

/** Persist an engine snapshot, pruning all but the newest `SNAPSHOTS_KEPT`. */
export async function persistSnapshot(seq: number, bytes: Uint8Array): Promise<void> {
    const db = await getDb();
    const tx = db.transaction('snapshots', 'readwrite');
    const store = tx.objectStore('snapshots');
    const row: SnapshotRow = { seq, bytes };
    store.put(row);
    /* Prune the oldest. getAllKeys() yields keys in ascending `seq` order, so
       everything before the last SNAPSHOTS_KEPT is stale. The deletes are
       issued synchronously inside onsuccess to stay within this transaction. */
    const keysReq = store.getAllKeys();
    keysReq.onsuccess = () => {
        for (const key of keysReq.result.slice(0, -SNAPSHOTS_KEPT)) {
            store.delete(key);
        }
    };
    await txDone(tx);
}

/**
 * Newest snapshot plus the command tail recorded after it (§10.3). Consumed
 * by `EngineClient` on trap recovery. If no snapshot exists yet, returns an
 * empty snapshot and the full command log.
 */
export async function loadLatestEventLog(documentId: string): Promise<{
    snapshot: Uint8Array;
    log: Command[];
    snapshotSeq: number;
    lastSeq: number;
}> {
    const db = await getDb();

    const snapTx = db.transaction('snapshots', 'readonly');
    const snapshots = (await reqToPromise(
        snapTx.objectStore('snapshots').getAll(),
    )) as SnapshotRow[];

    let latest: SnapshotRow | undefined;
    for (const row of snapshots) {
        if (!latest || row.seq > latest.seq) latest = row;
    }
    const snapshotSeq = latest ? latest.seq : 0;

    /* Commands with seq strictly greater than the snapshot's seq. */
    const cmdTx = db.transaction('commands', 'readonly');
    const tail = (await reqToPromise(
        cmdTx.objectStore('commands').getAll(IDBKeyRange.lowerBound(snapshotSeq, true)),
    )) as CommandRow[];

    /* Highest seq the recovered worker has already consumed — it must resume
       its `logSequence` past this, not restart at 0. getAll() yields rows in
       ascending seq order, so the tail's last row holds the highest. */
    const lastSeq = tail.at(-1)?.seq ?? snapshotSeq;

    return {
        snapshot: latest ? latest.bytes : new Uint8Array(0),
        log: tail.map((row) => row.cmd),
        snapshotSeq,
        lastSeq,
    };
}
