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
 * newest one turns out unreadable (`loadRecoveryLog` → candidates).
 *
 * Issue #268 — the PINNED base: the first snapshot persisted for a
 * document (`persistSnapshot(…, { pin: true })`, requested by the worker
 * after the log opens and after every document-replacing command) is
 * recorded in the `meta` row `pinned` and never pruned while it stays
 * pinned; pinning a newer one hands the old row back to ordinary pruning.
 * Its command tail IS pruned (the log stays bounded), so once pruning has
 * passed it the pinned base restores ALONE — the document as of that
 * snapshot, a last resort that beats losing the document when every
 * other snapshot is unreadable. It is persisted detached like every
 * snapshot, so its size is bounded by the #212 detachment, and the
 * package it names is kept by the same reference count.
 *
 * Issue #314 — a snapshot row never names a package the store does not
 * hold. `persistSnapshot` checks the `packages` store INSIDE the
 * transaction that writes the row: bytes supplied → written if absent;
 * no bytes (the caller believes the store already holds them) → the
 * package must be there, or the whole transaction aborts with
 * `PackageMissingError`. Either the row lands with its package, or
 * nothing lands — no snapshot depends on another transaction's success
 * (the worker marks a hash as stored only once a transaction holding it
 * COMMITTED, see `engine.worker.ts` `takeSnapshot`). */
import type { Command } from '../../../crates/engine-wasm/pkg/engine_wasm.js';

/** The primary database: the first tab's active log, the `archive` ring,
 *  the origin-wide renderer streak. */
export const PRIMARY_DB = 'engine-log';
/** Issue #426 - a tab that does not own the primary log keeps its own
 *  (`engine-log-<tab token>`, same schema); see `tab-session.ts`. */
export const SECONDARY_DB_PREFIX = 'engine-log-';
/* v2 (issue #212): the `packages` store + the snapshots `packageHash` index.
   v3 (issue #388): the `archive` store (the previous session's log, set
   aside at boot until the user recovers or discards it). Issue #426 keeps
   v3: the store is keyed by entry id, so the single `previous` row of #388
   is simply the oldest legacy entry of the new ring. */
const DB_VERSION = 3;
/** The database `getDb()` returns by default - THIS context's active log.
 *  Both the page and the worker call `setActiveLogDb` with the name the
 *  page chose (`tab-session.ts`) before touching the log. */
let activeDbName = PRIMARY_DB;

export function setActiveLogDb(name: string): void {
    activeDbName = name;
}

export function activeLogDb(): string {
    return activeDbName;
}
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
    /** Issue #315 — wall-clock time (ms since the epoch) the row was
     *  persisted; absent on rows written before it existed. */
    at?: number;
}
interface PackageRow {
    hash: string;
    bytes: Uint8Array;
}
/** Issue #212 — the detached package a snapshot is persisted with: its
 *  content key, plus the bytes when the store does not hold them yet.
 *  Issue #314 — with `bytes` the package is written in the snapshot's
 *  own transaction unless the store already holds it; without, the
 *  store MUST already hold it (else `PackageMissingError`). */
export interface SnapshotPackage {
    hash: string;
    bytes?: Uint8Array;
}

/** Issue #314 — `persistSnapshot` was asked to persist a row naming a
 *  package without its bytes, and the `packages` store does not hold that
 *  package (never committed, garbage-collected, or deleted): the
 *  transaction aborted and NOTHING was written. The caller must forget
 *  that it believed the package stored, so the next snapshot ships the
 *  bytes again. */
export class PackageMissingError extends Error {
    readonly hash: string;
    constructor(hash: string) {
        super(`event log: snapshot names package ${hash}, which the store does not hold`);
        this.name = 'PackageMissingError';
        this.hash = hash;
    }
}
/** Issue #241 — `meta` row: highest command seq deleted by pruning (0 =
 *  the log is complete from the session's first command). */
interface PrunedRow {
    id: 'pruned';
    through: number;
}
const PRUNED_ID = 'pruned';
/** Issue #268 — `meta` row: the seq of the document's pinned base
 *  snapshot (exempt from pruning). */
interface PinnedRow {
    id: 'pinned';
    seq: number;
}
const PINNED_ID = 'pinned';
/** Issue #390 - `meta` row: seqs of commands whose row failed to write. */
interface JournalGapRow {
    id: 'journal-gap';
    seqs: number[];
}
const JOURNAL_GAP_ID = 'journal-gap';
/** Issue #268 — options of `persistSnapshot`. */
export interface PersistSnapshotOptions {
    /** Make this snapshot the document's pinned base (see the header). */
    pin?: boolean;
}
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
    /** Issue #268 — every logged command after `seq` is still in the log
     *  (always, except for a pinned base that pruning has passed, and for
     *  the snapshot-less base of a pruned log). A candidate without it is
     *  restored WITHOUT a replay — its gapped tail would replay edits out
     *  of context. Absent = `true`. */
    tailComplete?: boolean;
    /** Issue #268 — this is the document's pinned base snapshot. */
    pinned?: boolean;
    /** Issue #315 — when the snapshot was persisted (ms since the epoch),
     *  so a recovery that loses the edits after it can say since when. */
    takenAt?: number;
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
    /** Issue #390 — seqs of commands whose row could not be written (the
     *  worker's best-effort `journal-gap` record); the worker counts the
     *  ones that are still missing after the base it restores. */
    journalGapSeqs: number[];
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

function openDb(name: string): Promise<IDBDatabase> {
    return new Promise<IDBDatabase>((resolve, reject) => {
        const open = indexedDB.open(name, DB_VERSION);
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
            if (!db.objectStoreNames.contains(ARCHIVE_STORE)) {
                db.createObjectStore(ARCHIVE_STORE, { keyPath: 'id' });
            }
        };
        open.onsuccess = () => {
            const db = open.result;
            /* A newer tab upgrading the schema must not be blocked by this
               connection: let go, the next call re-opens. */
            db.onversionchange = () => {
                db.close();
                dbPromises.delete(name);
            };
            resolve(db);
        };
        open.onerror = () => reject(open.error ?? new Error('IndexedDB open failed'));
        open.onblocked = () => reject(new Error('IndexedDB open blocked by another connection'));
    });
}

/** Lazily opened, cached connections - shared by every export below. */
const dbPromises = new Map<string, Promise<IDBDatabase>>();

function getDb(name: string = activeDbName): Promise<IDBDatabase> {
    const existing = dbPromises.get(name);
    if (existing) return existing;
    const fresh = openDb(name);
    dbPromises.set(name, fresh);
    /* A failed open must not poison later calls. */
    fresh.catch(() => dbPromises.delete(name));
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
    tx.objectStore('meta').delete(PINNED_ID);
    tx.objectStore('meta').delete(JOURNAL_GAP_ID);
    /* Issue #388 - a freshly opened log describes a document that equals
       what is on screen (nothing to lose yet). */
    tx.objectStore('meta').put({ id: CLEAN_ID, clean: true, at: Date.now() } satisfies CleanRow);
    await txDone(tx);
}

/* ===================================================================
   Issue #388 - the `clean` marker and the archived previous session.

   A plain reload starts a new session, and `openEventLog` clears the log
   - so a user who edited, never saved and reloaded lost the edits. The
   `meta` row `clean` records whether the document in the log equals
   what the user last saved (or opened / seeded): the worker flips it to
   `false` on the first document edit and back to `true` on a
   successful `SaveDocx`, an opened / closed / seeded document and an
   empty log. At boot, a log whose marker says "not clean" is not simply
   cleared: it is copied into the `archive` ring first (`archiveLog`)
   and offered back to the user (`restoreArchive` / `discardArchive`).
   Issue #426: the ring keeps the newest 3 entries, each with a decision
   state, and lives in the primary database; every tab's ACTIVE log is its
   own database (`tab-session.ts`).
   Rows from before this marker have none, which reads as "unknown" =
   nothing to offer.
   =================================================================== */

interface CleanRow {
    id: 'clean';
    clean: boolean;
    at: number;
}
const CLEAN_ID = 'clean';
const ARCHIVE_STORE = 'archive';
/** Issue #426 - sessions kept aside, newest N. */
export const ARCHIVE_RING_SIZE = 3;

/** The `meta` rows an archived session carries back with it. */
const ARCHIVED_META_IDS = ['document', PRUNED_ID, PINNED_ID, CLEAN_ID, 'journal-gap'];

/** Issue #426 - whether the user has seen the offer and moved on:
 *  `undecided` = never offered, or offered and still open; `seen` = the
 *  banner was dismissed without a decision. A full ring evicts `seen`
 *  entries (oldest first) before it touches an `undecided` one. */
export type ArchiveDecision = 'undecided' | 'seen';

interface ArchiveRow {
    id: string;
    /** When the session was set aside (ms since the epoch). */
    archivedAt: number;
    /** When the archived log's last document edit was logged. */
    lastEditAt: number | undefined;
    /** Issue #426 - per-entry decision state. Absent on a #388 row. */
    decision?: ArchiveDecision;
    commands: CommandRow[];
    snapshots: SnapshotRow[];
    packages: PackageRow[];
    meta: Array<Record<string, unknown>>;
}

/** What `inspectActiveLog` reports about the log a previous page
 *  generation left behind. */
export interface ActiveLogStatus {
    /** `false` = the marker says the log holds unsaved edits; `undefined`
     *  = no marker (a log from before #388, or no log at all). */
    clean: boolean | undefined;
    /** The log holds at least one command row or snapshot. */
    hasContent: boolean;
    /** When the marker last changed. */
    at: number | undefined;
}

/** A previous session held back for the user's decision. */
export interface ArchiveInfo {
    /** Issue #426 - the ring entry's id (`recoverPreviousSession(id)`). */
    id: string;
    archivedAt: number;
    lastEditAt: number | undefined;
    commandCount: number;
    decision: ArchiveDecision;
}

/** Persist the clean marker (the worker's off-critical-path write). */
export async function writeCleanMarker(clean: boolean): Promise<void> {
    const db = await getDb();
    const tx = db.transaction('meta', 'readwrite');
    tx.objectStore('meta').put({ id: CLEAN_ID, clean, at: Date.now() } satisfies CleanRow);
    await txDone(tx);
}

/** The persisted clean marker (`undefined` = none). */
export async function loadCleanMarker(): Promise<boolean | undefined> {
    const db = await getDb();
    const tx = db.transaction('meta', 'readonly');
    const req = tx.objectStore('meta').get(CLEAN_ID);
    await txDone(tx);
    const row = req.result as Partial<CleanRow> | undefined;
    return typeof row?.clean === 'boolean' ? row.clean : undefined;
}

/** Look at the active log WITHOUT touching it (boot, before `INIT`). */
export async function inspectActiveLog(name: string = activeDbName): Promise<ActiveLogStatus> {
    const db = await getDb(name);
    const tx = db.transaction(['commands', 'snapshots', 'meta'], 'readonly');
    const cmds = tx.objectStore('commands').count();
    const snaps = tx.objectStore('snapshots').count();
    const marker = tx.objectStore('meta').get(CLEAN_ID);
    await txDone(tx);
    const row = marker.result as Partial<CleanRow> | undefined;
    return {
        clean: typeof row?.clean === 'boolean' ? row.clean : undefined,
        hasContent: (cmds.result as number) > 0 || (snaps.result as number) > 0,
        at: typeof row?.at === 'number' ? row.at : undefined,
    };
}

/** Issue #426 - copy the log of database `source` (default: this
 *  context's active one) into a NEW entry of the `archive` ring, which
 *  lives in the primary database whichever tab it came from. Resolves
 *  once committed, so the caller may then let `INIT` clear the source (or
 *  delete it). A full ring evicts per `ARCHIVE_RING_SIZE`. */
export async function archiveLog(source: string = activeDbName): Promise<void> {
    const db = await getDb(source);
    const read = db.transaction(['commands', 'snapshots', 'packages', 'meta'], 'readonly');
    const cmdReq = read.objectStore('commands').getAll();
    const snapReq = read.objectStore('snapshots').getAll();
    const pkgReq = read.objectStore('packages').getAll();
    const metaReqs = ARCHIVED_META_IDS.map((id) => read.objectStore('meta').get(id));
    await txDone(read);
    const commands = cmdReq.result as CommandRow[];
    const now = Date.now();
    const row: ArchiveRow = {
        id: `a-${now.toString(36)}-${Math.random().toString(36).slice(2, 8)}`,
        archivedAt: now,
        lastEditAt: commands.at(-1)?.at,
        decision: 'undecided',
        commands,
        snapshots: snapReq.result as SnapshotRow[],
        packages: pkgReq.result as PackageRow[],
        meta: metaReqs
            .map((r) => r.result as Record<string, unknown> | undefined)
            .filter((r): r is Record<string, unknown> => r !== undefined),
    };
    const primary = await getDb(PRIMARY_DB);
    const write = primary.transaction(ARCHIVE_STORE, 'readwrite');
    const store = write.objectStore(ARCHIVE_STORE);
    store.put(row);
    /* Ring eviction, in the same transaction as the new entry. */
    const all = store.getAll();
    all.onsuccess = () => {
        const rows = all.result as ArchiveRow[];
        let over = rows.length - ARCHIVE_RING_SIZE;
        const order = (r: ArchiveRow): number => (r.decision === 'seen' ? 0 : 1);
        const victims = [...rows]
            .filter((r) => r.id !== row.id)
            .sort((x, y) => order(x) - order(y) || x.archivedAt - y.archivedAt);
        while (over-- > 0 && victims.length > 0) {
            const v = victims.shift()!;
            console.warn(
                `[event-log] archive ring full: dropping the ${v.decision ?? 'undecided'} ` +
                    `session set aside at ${new Date(v.archivedAt).toISOString()}`,
            );
            store.delete(v.id);
        }
    };
    await txDone(write);
}

/** The archived previous sessions waiting for a decision, newest first. */
export async function listArchive(): Promise<ArchiveInfo[]> {
    const db = await getDb(PRIMARY_DB);
    const tx = db.transaction(ARCHIVE_STORE, 'readonly');
    const req = tx.objectStore(ARCHIVE_STORE).getAll();
    await txDone(tx);
    return (req.result as ArchiveRow[])
        .map((row) => ({
            id: row.id,
            archivedAt: row.archivedAt,
            lastEditAt: row.lastEditAt,
            commandCount: row.commands.length,
            decision: row.decision ?? 'undecided',
        }))
        .sort((a, b) => b.archivedAt - a.archivedAt);
}

/** Issue #426 - mark entries as seen-and-dismissed (the banner's
 *  Dismiss): they stay offered at the next boot but are evicted first. */
export async function markArchiveSeen(ids: string[]): Promise<void> {
    if (ids.length === 0) return;
    const db = await getDb(PRIMARY_DB);
    const tx = db.transaction(ARCHIVE_STORE, 'readwrite');
    const store = tx.objectStore(ARCHIVE_STORE);
    for (const id of ids) {
        const req = store.get(id);
        req.onsuccess = () => {
            const row = req.result as ArchiveRow | undefined;
            if (row) store.put({ ...row, decision: 'seen' } satisfies ArchiveRow);
        };
    }
    await txDone(tx);
}

/** Make archive entry `id` (default: the newest) the ACTIVE log of this
 *  context. The active stores are replaced in one transaction; the entry is
 *  removed afterwards - a crash in between leaves a duplicate offer, never a
 *  lost session. Resolves `false` when there is no such entry. */
export async function restoreArchive(id?: string): Promise<boolean> {
    const entries = await listArchive();
    const target = id === undefined ? entries[0]?.id : id;
    if (target === undefined) return false;
    const primary = await getDb(PRIMARY_DB);
    const read = primary.transaction(ARCHIVE_STORE, 'readonly');
    const req = read.objectStore(ARCHIVE_STORE).get(target);
    await txDone(read);
    const row = req.result as ArchiveRow | undefined;
    if (!row) return false;
    const db = await getDb();
    const tx = db.transaction(['commands', 'snapshots', 'meta', 'packages'], 'readwrite');
    tx.objectStore('commands').clear();
    tx.objectStore('snapshots').clear();
    tx.objectStore('packages').clear();
    const meta = tx.objectStore('meta');
    for (const mid of ARCHIVED_META_IDS) meta.delete(mid);
    for (const c of row.commands) tx.objectStore('commands').put(c);
    for (const sn of row.snapshots) tx.objectStore('snapshots').put(sn);
    for (const p of row.packages) tx.objectStore('packages').put(p);
    for (const m of row.meta) meta.put(m);
    /* A session restored from the archive is, by construction, one that
       had unsaved edits. */
    meta.put({ id: CLEAN_ID, clean: false, at: Date.now() } satisfies CleanRow);
    await txDone(tx);
    await discardArchive(target);
    return true;
}

/** Throw archive entry `id` away (the user chose Discard); without an id,
 *  the newest entry. */
export async function discardArchive(id?: string): Promise<void> {
    const target = id ?? (await listArchive())[0]?.id;
    if (target === undefined) return;
    const db = await getDb(PRIMARY_DB);
    const tx = db.transaction(ARCHIVE_STORE, 'readwrite');
    tx.objectStore(ARCHIVE_STORE).delete(target);
    await txDone(tx);
}

/** Issue #426 - delete a (secondary) tab database whose tab is gone. */
export async function deleteLogDb(name: string): Promise<void> {
    if (name === PRIMARY_DB) return;
    const pending = dbPromises.get(name);
    dbPromises.delete(name);
    if (pending) (await pending.catch(() => undefined))?.close();
    await new Promise<void>((resolve) => {
        const req = indexedDB.deleteDatabase(name);
        req.onsuccess = req.onerror = req.onblocked = () => resolve();
    });
}

/** Issue #426 - names of the secondary tab databases that exist. */
export async function listSecondaryDbs(): Promise<string[]> {
    try {
        const dbs = await indexedDB.databases();
        return dbs
            .map((d) => d.name ?? '')
            .filter((n) => n.startsWith(SECONDARY_DB_PREFIX));
    } catch {
        return [];
    }
}

/** Append one dispatched command to the durable log. */
export async function appendCommand(seq: number, cmd: Command): Promise<void> {
    const db = await getDb();
    const tx = db.transaction('commands', 'readwrite');
    const row: CommandRow = { seq, cmd, at: Date.now() };
    tx.objectStore('commands').put(row);
    await txDone(tx);
}

/** Issue #390 - best-effort record of the command seqs whose row could not
 *  be written, so a later recovery can report the gap. Lives in `meta`,
 *  which may keep working when the `commands` store does not. */
export async function writeJournalGap(seqs: number[]): Promise<void> {
    const db = await getDb();
    const tx = db.transaction('meta', 'readwrite');
    tx.objectStore('meta').put({ id: JOURNAL_GAP_ID, seqs } satisfies JournalGapRow);
    await txDone(tx);
}

/** Issue #390 - every missing row landed after all: forget the gap. */
export async function clearJournalGap(): Promise<void> {
    const db = await getDb();
    const tx = db.transaction('meta', 'readwrite');
    tx.objectStore('meta').delete(JOURNAL_GAP_ID);
    await txDone(tx);
}

/** Persist an engine snapshot, pruning all but the newest `SNAPSHOTS_KEPT`.
 *  Issue #212 — `pkg` names the detached source package the snapshot was
 *  taken without; its bytes (first snapshot of a document) go to the
 *  `packages` store in the same transaction, so a snapshot row never
 *  lands without the package it names. Issue #314 — and that holds for
 *  a row persisted WITHOUT bytes too: the transaction verifies the store
 *  holds the package and aborts with `PackageMissingError` otherwise
 *  (see the header). Issue #268 — `opts.pin` makes it the document's
 *  pinned base (exempt from pruning; see the header). */
export async function persistSnapshot(
    seq: number,
    bytes: Uint8Array,
    pkg?: SnapshotPackage,
    opts?: PersistSnapshotOptions,
): Promise<void> {
    const db = await getDb();
    const tx = db.transaction(['snapshots', 'commands', 'meta', 'packages'], 'readwrite');
    const store = tx.objectStore('snapshots');
    const packages = tx.objectStore('packages');
    const meta = tx.objectStore('meta');
    const at = Date.now();
    const row: SnapshotRow = pkg ? { seq, bytes, packageHash: pkg.hash, at } : { seq, bytes, at };
    /* Issue #314 — the package the row names, resolved INSIDE this
       transaction. Requests run in issue order, so this lookup (and the
       put its callback may issue) completes before the package GC below
       counts references — and the GC sees this row's reference, put
       synchronously right after. */
    let abortReason: Error | undefined;
    if (pkg) {
        const held = packages.getKey(pkg.hash);
        held.onsuccess = () => {
            if (held.result !== undefined) return;
            if (pkg.bytes) {
                /* Not stored yet (first snapshot of the document, or an
                   earlier write of it failed): write it with this row.
                   Already stored → no multi-MB rewrite. */
                packages.put({ hash: pkg.hash, bytes: pkg.bytes } satisfies PackageRow);
            } else {
                /* The caller believed it stored — it is not. Land
                   nothing rather than a row naming a missing package. */
                abortReason = new PackageMissingError(pkg.hash);
                tx.abort();
            }
        };
    }
    store.put(row);
    if (opts?.pin) meta.put({ id: PINNED_ID, seq } satisfies PinnedRow);
    /* Requests complete in issue order, so this read sees the pin above. */
    const pinnedReq = meta.get(PINNED_ID);
    /* Prune the oldest. getAllKeys() yields keys in ascending `seq` order, so
       everything before the last SNAPSHOTS_KEPT is stale. The deletes are
       issued synchronously inside onsuccess to stay within this transaction.
       Issue #268 — the pinned base is not part of that window: it is never
       pruned, and it does not hold any other snapshot's slot. */
    const keysReq = store.getAllKeys();
    keysReq.onsuccess = () => {
        const pinnedSeq = (pinnedReq.result as PinnedRow | undefined)?.seq;
        const keys = (keysReq.result as number[]).filter((k) => k !== pinnedSeq);
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
    try {
        await txDone(tx);
    } catch (e: unknown) {
        throw abortReason ?? e;
    }
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
    const pinnedReq = tx.objectStore('meta').get(PINNED_ID);
    const gapReq = tx.objectStore('meta').get(JOURNAL_GAP_ID);
    const pkgReq = tx.objectStore('packages').getAll();
    await txDone(tx);
    const packages = new Map(
        (pkgReq.result as PackageRow[]).map((row) => [row.hash, row.bytes] as const),
    );

    const snapshots = (snapReq.result as SnapshotRow[]).slice().sort((a, b) => b.seq - a.seq);
    const commands = (cmdReq.result as CommandRow[]).map((row) => ({ seq: row.seq, cmd: row.cmd }));
    const prunedThrough = (prunedReq.result as PrunedRow | undefined)?.through ?? 0;
    const pinnedSeq = (pinnedReq.result as PinnedRow | undefined)?.seq;
    const newestSnapshotSeq = snapshots[0]?.seq ?? 0;
    return {
        candidates: [
            ...snapshots.map((row): RecoveryCandidate => {
                const candidate: RecoveryCandidate = {
                    seq: row.seq,
                    snapshot: row.bytes,
                    /* Every command after `row.seq` survives pruning. */
                    tailComplete: row.seq >= prunedThrough,
                };
                if (row.seq === pinnedSeq) candidate.pinned = true;
                if (typeof row.at === 'number') candidate.takenAt = row.at;
                if (row.packageHash !== undefined) {
                    candidate.packageHash = row.packageHash;
                    const pkg = packages.get(row.packageHash);
                    if (pkg) candidate.package = pkg;
                }
                return candidate;
            }),
            { seq: 0, snapshot: new Uint8Array(0), tailComplete: prunedThrough === 0 },
        ],
        commands,
        /* getAll() yields rows in ascending seq order. */
        lastSeq: Math.max(commands.at(-1)?.seq ?? 0, newestSnapshotSeq),
        logComplete: prunedThrough === 0,
        journalGapSeqs: (gapReq.result as JournalGapRow | undefined)?.seqs ?? [],
    };
}

/* ===================================================================
   Issue #240 — the GPU-renderer crash-loop streak, persisted.

   #99 bounds consecutive worker traps on Vello within one EngineClient;
   a failure that takes the whole TAB down (or a reload mid-loop) started
   a fresh client from zero. The streak lives in the `meta` store beside
   the event log — origin-wide, like the GPU it describes — so the next
   boot can honour it. `openEventLog` never clears it.
   =================================================================== */

/** The persisted streak (`meta` row `renderer-streak`). */
export interface RendererStreak {
    /** The backend the failing generations painted with (`"vello"`). */
    renderer: string;
    /** Consecutive generations on `renderer` that ended in a trap, or in
     *  a tab death (see `live`). Reset once a generation stays up. */
    count: number;
    /** When the streak last changed (ms since the epoch) — the decay
     *  clock. */
    at: number;
    /** A generation on `renderer` is running and has neither proved
     *  stable nor shut down cleanly. Still `true` at the next boot ⇒ that
     *  generation died with its tab: it counts as one more failure. */
    live: boolean;
    /** Identifies the `live` generation; a clean shutdown leaves it in
     *  `localStorage` (see `EngineClient`), which un-counts it. */
    token?: string;
}

const STREAK_ID = 'renderer-streak';

export async function loadRendererStreak(): Promise<RendererStreak | undefined> {
    const db = await getDb(PRIMARY_DB);
    const tx = db.transaction('meta', 'readonly');
    const req = tx.objectStore('meta').get(STREAK_ID);
    await txDone(tx);
    const row = req.result as Partial<RendererStreak> | undefined;
    if (
        !row ||
        typeof row.renderer !== 'string' ||
        typeof row.count !== 'number' ||
        typeof row.at !== 'number'
    ) {
        return undefined;
    }
    const streak: RendererStreak = {
        renderer: row.renderer,
        count: row.count,
        at: row.at,
        live: row.live === true,
    };
    if (typeof row.token === 'string') streak.token = row.token;
    return streak;
}

export async function saveRendererStreak(streak: RendererStreak): Promise<void> {
    const db = await getDb(PRIMARY_DB);
    const tx = db.transaction('meta', 'readwrite');
    tx.objectStore('meta').put({ id: STREAK_ID, ...streak });
    await txDone(tx);
}

export async function clearRendererStreak(): Promise<void> {
    const db = await getDb(PRIMARY_DB);
    const tx = db.transaction('meta', 'readwrite');
    tx.objectStore('meta').delete(STREAK_ID);
    await txDone(tx);
}
