/* Phase 2 D2.6 — event log + crash-recovery storage (PHASE_2_BRIDGE_MEMORY.md
 * §10). Native IndexedDB; no external dependency.
 *
 * One origin-scoped database `engine-log` with four object stores:
 *   - commands  (keyPath "seq") — replay-relevant commands, in order; rows
 *                 at/before a pruned snapshot are dropped with it
 *   - snapshots (keyPath "seq") — periodic engine snapshots; pruned to 3
 *   - meta      (keyPath "id")  — bookkeeping (which document is logged,
 *                 how far the command log has been pruned)
 *   - packages  (keyPath "hash") — issue #212: the opened `.docx`'s retained
 *                 source package (#134), stored ONCE per document under its
 *                 content key. Snapshots are taken detached
 *                 (`Command::Snapshot.detach_package`) and name the package
 *                 by `packageHash`, so a 2–7 MB package of embedded fonts /
 *                 OLE parts is not re-written with every snapshot. A package
 *                 no retained snapshot names is dropped with the pruning.
 *
 * Issue #241 — pruning invariant: a command row is only ever deleted in
 * the same transaction that persists a snapshot, and only when it is at
 * or before a PRUNED snapshot's seq — i.e. strictly before every
 * retained snapshot. So (a) the log is never pruned while no snapshot is
 * present, and (b) EVERY retained snapshot still has its complete tail,
 * which is what lets recovery fall back to an older snapshot when the
 * newest one turns out unreadable (`loadRecoveryLog` → candidates). */
import type { Command } from '../../../crates/engine-wasm/pkg/engine_wasm.js';

const DB_NAME = 'engine-log';
/* v2 (issue #212): the `packages` store + the snapshots `packageHash` index. */
const DB_VERSION = 2;
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
    /** Issue #212 — the detached source package this snapshot names. */
    packageHash?: string;
}
interface PackageRow {
    hash: string;
    bytes: Uint8Array;
}
/** Issue #212 — the detached package a snapshot is persisted with: its
 *  content key, plus the bytes when the store does not hold them yet. */
export interface SnapshotPackage {
    hash: string;
    bytes?: Uint8Array;
}
/** Issue #241 — `meta` row: highest command seq deleted by pruning (0 =
 *  the log is complete from the session's first command). */
interface PrunedRow {
    id: 'pruned';
    through: number;
}
const PRUNED_ID = 'pruned';
/** Issue #212 — snapshots index over `packageHash` (package GC). */
const PACKAGE_INDEX = 'packageHash';

/** One logged command with its log position. */
export interface LoggedCommand {
    seq: number;
    cmd: Command;
}

/** Issue #241 — one base a recovery can start from: a persisted snapshot
 *  (replayed with every logged command after `seq`), or — the last
 *  candidate, `seq` 0 with empty bytes — the bare command log. */
export interface RecoveryCandidate {
    seq: number;
    snapshot: Uint8Array;
    /** Issue #212 — the detached package the snapshot names, and its
     *  bytes when the `packages` store still holds them (absent → the
     *  engine restores the document and saves through the minimal
     *  writer). */
    packageHash?: string;
    package?: Uint8Array;
}

/** Everything `EngineClient.recover()` hands the respawned worker. */
export interface RecoveryLog {
    /** Newest snapshot first; always ends with the snapshot-less base. */
    candidates: RecoveryCandidate[];
    /** Every retained command row, ascending. Candidate `c` replays the
     *  rows with `seq > c.seq`. */
    commands: LoggedCommand[];
    /** Highest seq already consumed — the recovered worker resumes its
     *  `logSequence` past it, never restarting at 0. */
    lastSeq: number;
    /** Issue #241 — `false` once pruning dropped the session's first
     *  commands (the boot `RENDER_PAGE` among them): the snapshot-less
     *  candidate can then no longer rebuild the document. */
    logComplete: boolean;
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
            const snapshots = db.objectStoreNames.contains('snapshots')
                ? open.transaction!.objectStore('snapshots')
                : db.createObjectStore('snapshots', { keyPath: 'seq' });
            if (!snapshots.indexNames.contains(PACKAGE_INDEX)) {
                snapshots.createIndex(PACKAGE_INDEX, 'packageHash');
            }
            if (!db.objectStoreNames.contains('meta')) {
                db.createObjectStore('meta', { keyPath: 'id' });
            }
            if (!db.objectStoreNames.contains('packages')) {
                db.createObjectStore('packages', { keyPath: 'hash' });
            }
        };
        open.onsuccess = () => {
            const db = open.result;
            /* A newer tab upgrading the schema must not be blocked by this
               connection: let go, the next call re-opens. */
            db.onversionchange = () => {
                db.close();
                dbPromise = null;
            };
            resolve(db);
        };
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

/** Open the event log for a fresh session: record which document it belongs
 *  to and drop every row a previous session left behind. `logSequence`
 *  restarts at 0 per worker boot, so surviving stale rows would be upserted
 *  over piecemeal and leak another session's commands into the next recovery
 *  tail. Crash recovery WITHIN a session never re-opens the log (the RECOVER
 *  path resumes the sequence past what is persisted), so this clear cannot
 *  eat a live session's history. */
export async function openEventLog(documentId: string): Promise<void> {
    const db = await getDb();
    const tx = db.transaction(['commands', 'snapshots', 'meta', 'packages'], 'readwrite');
    tx.objectStore('commands').clear();
    tx.objectStore('snapshots').clear();
    tx.objectStore('packages').clear();
    tx.objectStore('meta').put({ id: 'document', documentId, openedAt: Date.now() });
    tx.objectStore('meta').put({ id: PRUNED_ID, through: 0 } satisfies PrunedRow);
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

/** Persist an engine snapshot, pruning all but the newest `SNAPSHOTS_KEPT`.
 *  Issue #212 — `pkg` names the detached source package the snapshot was
 *  taken without; its bytes (first snapshot of a document) go to the
 *  `packages` store in the same transaction, so a snapshot row never
 *  lands without the package it names. */
export async function persistSnapshot(
    seq: number,
    bytes: Uint8Array,
    pkg?: SnapshotPackage,
): Promise<void> {
    const db = await getDb();
    const tx = db.transaction(['snapshots', 'commands', 'meta', 'packages'], 'readwrite');
    const store = tx.objectStore('snapshots');
    const packages = tx.objectStore('packages');
    const row: SnapshotRow = pkg ? { seq, bytes, packageHash: pkg.hash } : { seq, bytes };
    if (pkg?.bytes) {
        packages.put({ hash: pkg.hash, bytes: pkg.bytes } satisfies PackageRow);
    }
    store.put(row);
    /* Prune the oldest. getAllKeys() yields keys in ascending `seq` order, so
       everything before the last SNAPSHOTS_KEPT is stale. The deletes are
       issued synchronously inside onsuccess to stay within this transaction. */
    const keysReq = store.getAllKeys();
    keysReq.onsuccess = () => {
        const keys = keysReq.result as number[];
        const pruned = keys.slice(0, -SNAPSHOTS_KEPT);
        const oldestRetained = keys.slice(-SNAPSHOTS_KEPT)[0];
        for (const key of pruned) {
            store.delete(key);
        }
        /* Command rows at or before a pruned snapshot's seq are only
           reachable by replaying from that (now deleted) snapshot — every
           retained snapshot's tail starts strictly after its own seq — so
           drop them in the same transaction. Bounds the commands store to
           roughly SNAPSHOTS_KEPT × SNAPSHOT_EVERY rows.
           Issue #241 — only while a snapshot is retained (the one just
           put always is: pruning never runs without a snapshot present),
           and never past the oldest retained one, so every retained
           snapshot keeps its full tail for the recovery fallback chain. */
        const newestPruned = pruned.at(-1);
        if (
            newestPruned !== undefined &&
            oldestRetained !== undefined &&
            newestPruned < oldestRetained
        ) {
            tx.objectStore('commands').delete(IDBKeyRange.upperBound(newestPruned));
            const meta = tx.objectStore('meta');
            const prev = meta.get(PRUNED_ID);
            prev.onsuccess = () => {
                const before = (prev.result as PrunedRow | undefined)?.through ?? 0;
                const through = Math.max(before, newestPruned);
                meta.put({ id: PRUNED_ID, through } satisfies PrunedRow);
            };
        }
        /* Issue #212 — drop every package no retained snapshot names (a
           document opened earlier in the session). Requests run in
           order, so the counts see the deletes above. */
        const index = store.index(PACKAGE_INDEX);
        const pkgKeys = packages.getAllKeys();
        pkgKeys.onsuccess = () => {
            for (const hash of pkgKeys.result) {
                const refs = index.count(IDBKeyRange.only(hash));
                refs.onsuccess = () => {
                    if (refs.result === 0) packages.delete(hash);
                };
            }
        };
    };
    await txDone(tx);
}

/**
 * Issue #85 / #241 — the recovery inputs (§10.3), consumed by
 * `EngineClient` on trap recovery: every retained snapshot, newest first,
 * then the snapshot-less base, plus every retained command row. The
 * worker tries the candidates in order and keeps the first whose
 * snapshot restores, so one unreadable snapshot row costs a longer
 * replay instead of the document.
 */
export async function loadRecoveryLog(): Promise<RecoveryLog> {
    const db = await getDb();
    const tx = db.transaction(['snapshots', 'commands', 'meta', 'packages'], 'readonly');
    const snapReq = tx.objectStore('snapshots').getAll();
    const cmdReq = tx.objectStore('commands').getAll();
    const prunedReq = tx.objectStore('meta').get(PRUNED_ID);
    const pkgReq = tx.objectStore('packages').getAll();
    await txDone(tx);
    const packages = new Map(
        (pkgReq.result as PackageRow[]).map((row) => [row.hash, row.bytes] as const),
    );

    const snapshots = (snapReq.result as SnapshotRow[]).slice().sort((a, b) => b.seq - a.seq);
    const commands = (cmdReq.result as CommandRow[]).map((row) => ({ seq: row.seq, cmd: row.cmd }));
    const prunedThrough = (prunedReq.result as PrunedRow | undefined)?.through ?? 0;
    const newestSnapshotSeq = snapshots[0]?.seq ?? 0;
    return {
        candidates: [
            ...snapshots.map((row): RecoveryCandidate => {
                const candidate: RecoveryCandidate = { seq: row.seq, snapshot: row.bytes };
                if (row.packageHash !== undefined) {
                    candidate.packageHash = row.packageHash;
                    const pkg = packages.get(row.packageHash);
                    if (pkg) candidate.package = pkg;
                }
                return candidate;
            }),
            { seq: 0, snapshot: new Uint8Array(0) },
        ],
        commands,
        /* getAll() yields rows in ascending seq order. */
        lastSeq: Math.max(commands.at(-1)?.seq ?? 0, newestSnapshotSeq),
        logComplete: prunedThrough === 0,
    };
}
