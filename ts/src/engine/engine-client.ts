/* EngineClient — typed RPC wrapper around the engine Web Worker.
 * PHASE_2_BRIDGE_MEMORY.md §7, plus the §10.3 crash-recovery flow.
 *
 * Spawns the dedicated worker, matches request/response by `id`, and fans
 * unidirectional events out to subscribers. On a WASM trap it rejects all
 * in-flight requests and hands off to the UI-supplied `onCrash` callback,
 * which must provide a fresh OffscreenCanvas and call `recover()`.
 *
 * Note: the §7 spec imports types from `crates/bridge/pkg/types`. The bridge
 * crate is not built standalone — its `Command`/`Event` types are generated
 * (via `tsify-next`) into the `engine-wasm` wasm-pack package, so that is the
 * real import site. */
import type {
    BlockPath,
    Command,
    DocFormat,
    DocumentDefaults,
    Event,
    RendererDowngrade,
} from '../../../crates/engine-wasm/pkg/engine_wasm.js';
import {
    clearRendererStreak,
    loadRendererStreak,
    loadRecoveryLog,
    saveRendererStreak,
    type RecoveryLog,
    type RendererStreak,
} from './event-log';

type WorkerReply = {
    ok: boolean;
    evt?: Event;
    error?: string;
    trap?: boolean;
    /** Worker-context cross-origin isolation, reported in the INIT reply. */
    crossOriginIsolated?: boolean;
    /** Active renderer the worker picked at INIT / re-probed at RECOVER —
     *  `vello` or `canvas2d` (issue #66: the RECOVER value is the engine's
     *  own report, never a remembered INIT-time one). */
    renderer?: string;
    /** Issue #85 — RECOVER reply: whether a base snapshot was restored
     *  (false ⇒ the engine recovered onto a fresh document from the
     *  replay tail alone, and the shell must re-seed it). */
    restored?: boolean;
    /** Issue #85 — RECOVER reply: replay-tail commands applied. */
    appliedCommands?: number;
    /** Issue #240 — INIT reply: whether the worker probed the GPU backend
     *  (`false` when the client forced Canvas2D). */
    probed?: boolean;
    /** Issue #241 — RECOVER reply: newer snapshots skipped because they
     *  would not restore before one did (or the log base was used). */
    snapshotFallbacks?: number;
    /** Issue #241 — RECOVER reply: whether the event log still reached
     *  back to the session's first command. */
    logComplete?: boolean;
    /** Issue #268 — RECOVER reply: see `RecoveryInfo.packageFallbacks`. */
    packageFallbacks?: number;
    /** Issue #268 — RECOVER reply: see `RecoveryInfo.packageLost`. */
    packageLost?: boolean;
    /** Issue #268 — RECOVER reply: see `RecoveryInfo.pinnedBase`. */
    pinnedBase?: boolean;
    /** Issue #268 — RECOVER reply: see `RecoveryInfo.tailDropped`. */
    tailDropped?: boolean;
    /** Issue #315 — RECOVER reply: see `RecoveryInfo.baseSnapshotAt`. */
    baseTakenAt?: number;
    /** Phase 8a — payload of a `GET_COMMENTS` side-channel reply. */
    comments?: CommentSnapshot[];
    /** Phase 8b — payload of a `GET_REVISIONS` side-channel reply. */
    revisions?: RevisionSnapshot[];
    /** Issue #96 — payload of the DEV-only `PROBE_PAGE_INK` reply. */
    ink?: Record<number, number>;
};

/** Phase 8a — read-only snapshot row for the comments sidebar. */
export interface CommentSnapshot {
    id: number;
    author: string;
    date: string;
    text: string;
    start_block: number;
    start_offset: number;
    end_block: number;
    end_offset: number;
    /** Issue #254 — full anchor paths (cell comments address their cell). */
    start_path: BlockPath;
    end_path: BlockPath;
    resolved: boolean;
    /** Issue #27 — parent comment `w:id` when this row is a threaded
     *  reply; `undefined` on top-level comments. */
    parent_id?: number;
}

/** Phase 8b — read-only snapshot row for revision tooltips. */
export interface RevisionSnapshot {
    block: number;
    start: number;
    end: number;
    /** Issue #247 — `move-from` / `move-to` are the two halves of a
     *  tracked move; `format` a tracked formatting change. */
    kind: 'insert' | 'delete' | 'format' | 'move-from' | 'move-to';
    author: string;
    date: string;
    /** Issue #247 — the move's range name; both halves share it. */
    move_name?: string;
    /** Issue #262 — a paragraph-MARK revision (a tracked paragraph split
     *  or merge), addressed by the empty range at the paragraph end. */
    mark?: boolean;
    /** Issue #304 — the revision's stable id (`ACCEPT_REVISION` /
     *  `REJECT_REVISION` `revision_id`): unique per row, unchanged by
     *  edits elsewhere. */
    revision_id?: number;
}

/** Issue #85 — what the most recent `recover()` achieved. */
export interface RecoveryInfo {
    /** A base snapshot was decoded and installed before the tail replay. */
    restored: boolean;
    /** Replay-tail commands applied on top of it. */
    appliedCommands: number;
    /** The renderer the recovered engine actually paints with (#66). */
    renderer: string;
    /** Issue #97 — the user zoom the recovered engine renders at. */
    zoom: number;
    /** Issue #97 — the recovered boot device scale; `undefined` when the
     *  engine came back without a layout config. */
    deviceScale: number | undefined;
    /**
     * Issue #97 — the recovered engine holds a live session (document +
     * layout config): either a base snapshot was restored, or the replayed
     * tail itself re-seeded it (it carried the boot `RENDER_PAGE`). The
     * shell must then NOT re-seed via `RENDER_PAGE`, which would wipe the
     * replayed document and reset the zoom to 100 %.
     */
    layoutRestored: boolean;
    /** Issue #99 — set when this generation was forced onto Canvas2D after
     *  a crash loop on Vello (the engine's echo on `Event::Recovered`). */
    rendererDowngrade: RendererDowngrade | undefined;
    /** Issue #241 — persisted snapshots that were skipped (unreadable)
     *  before the one that restored; `0` on an ordinary recovery. */
    snapshotFallbacks: number;
    /**
     * Issue #241 — no snapshot restored AND pruning had already dropped
     * the head of the command log, so the replay could not rebuild the
     * document (only reachable when every retained snapshot is
     * unreadable). The shell re-seeds; the flag makes the loss visible.
     */
    logTruncated: boolean;
    /**
     * Issue #268 — readable snapshots passed over because the source
     * package they name was missing from the event log (a failed
     * `packages` write): recovery preferred an older base that still has
     * its package. `0` on an ordinary recovery.
     */
    packageFallbacks: number;
    /**
     * Issue #268 — the downgrade: the recovered document came from a
     * snapshot whose retained source package could not be re-attached
     * (no retained base had it), so saving goes through the minimal-
     * package writer and drops the `.docx`'s sibling parts (styles,
     * headers, settings, embedded fonts…).
     */
    packageLost: boolean;
    /** Issue #268 — recovery restored the document's pinned base snapshot
     *  (the last-resort base that is never pruned). */
    pinnedBase: boolean;
    /**
     * Issue #268 — the pinned base was restored WITHOUT its replay tail
     * (pruning had passed it): the document is as of that snapshot and
     * the edits made after it are lost. Only reachable when every newer
     * snapshot is unreadable; better than `logTruncated`'s total loss.
     */
    tailDropped: boolean;
    /**
     * Issue #315 — this recovery is the one that tripped the #99 crash-loop
     * downgrade (`rendererDowngrade` is re-sent on every later forced
     * recovery of the session; only the first one is news to the user).
     */
    rendererDowngraded: boolean;
    /** Issue #315 — when the restored base snapshot was persisted (ms
     *  since the epoch); `undefined` when none was restored or the row
     *  predates the timestamp. Lets the shell say since when edits were
     *  lost (`tailDropped`). */
    baseSnapshotAt: number | undefined;
    /** Issue #270 — why the recovery ran: a worker `trap`, or the Dev
     *  HUD's in-place `renderer-retry` (a planned respawn, no crash). */
    cause: RecoveryCause;
}

/** Issue #270 — see `RecoveryInfo.cause`. Issue #330 — `engine-reload`
 *  is the crash overlay's "Reload engine" (an in-place restart from the
 *  log) and `page-reload` a boot that honoured a carry-over. */
export type RecoveryCause = 'trap' | 'renderer-retry' | 'engine-reload' | 'page-reload';

/** Issue #330 — `sessionStorage` key of the one-shot "carry the document
 *  across this reload" token (see `EngineClient.prepareCarryOver`). */
const CARRY_OVER_KEY = 'nge.carry-over';
/** Issue #330 — a carry-over older than this is stale (a reload takes
 *  seconds, not minutes) and ignored. */
const CARRY_OVER_MAX_AGE_MS = 2 * 60 * 1000;
/** Issue #330 — how long a planned retirement may take before the
 *  worker is declared unresponsive and terminated anyway. */
const RETIRE_TIMEOUT_MS = 3000;

function writeCarryOver(documentId: string): void {
    try {
        globalThis.sessionStorage?.setItem(
            CARRY_OVER_KEY,
            JSON.stringify({ documentId, at: Date.now() }),
        );
    } catch {
        /* storage blocked: the reload starts a fresh session */
    }
}

/** Issue #330 — read AND remove the carry-over token: honoured once. */
function takeCarryOver(documentId: string): boolean {
    try {
        const raw = globalThis.sessionStorage?.getItem(CARRY_OVER_KEY);
        globalThis.sessionStorage?.removeItem(CARRY_OVER_KEY);
        if (!raw) return false;
        const t = JSON.parse(raw) as { documentId?: unknown; at?: unknown };
        return (
            t.documentId === documentId &&
            typeof t.at === 'number' &&
            Date.now() - t.at >= 0 &&
            Date.now() - t.at < CARRY_OVER_MAX_AGE_MS
        );
    } catch {
        return false;
    }
}

/** Issue #333 — whether the worker's event-log checkpoints are landing.
 *  `failing` is set once the bounded snapshot-write retries are exhausted
 *  and cleared by the next successful write. */
export interface CheckpointStatus {
    failing: boolean;
    /** Consecutive failed snapshot writes in the current run. */
    failures: number;
}

/** Issue #99 — consecutive traps on the Vello backend after which recovery
 *  stops re-probing the GPU and forces Canvas2D (so a persistently failing
 *  driver / shader path costs at most N + 1 worker generations). */
export const VELLO_TRAP_LIMIT = 2;
/** Issue #99 — a generation that stays up this long ends the streak: two
 *  unrelated traps an hour apart are not a crash loop. */
const STABLE_GENERATION_MS = 60_000;
/** Issue #240 — a persisted crash-loop streak older than this is ignored
 *  at boot: a driver update or a GPU fix since then deserves a new try. */
export const CRASH_LOOP_DECAY_MS = 24 * 60 * 60 * 1000;
/** Issue #240 — `localStorage` key a clean shutdown (`pagehide`) writes
 *  the live generation's token to. Synchronous, so it survives the
 *  unload an IndexedDB write started in `pagehide` does not reliably
 *  survive; the next boot reads and removes it. */
const CLEAN_EXIT_KEY = 'nge.renderer.clean-exit';

function takeCleanExitToken(): string | undefined {
    try {
        const token = globalThis.localStorage?.getItem(CLEAN_EXIT_KEY) ?? undefined;
        globalThis.localStorage?.removeItem(CLEAN_EXIT_KEY);
        return token;
    } catch {
        return undefined;
    }
}

/**
 * Issue #240 — what a boot does with the persisted streak: the streak to
 * resume counting from and, once it reached `VELLO_TRAP_LIMIT`, the
 * downgrade to boot with (Canvas2D, no GPU probe). A record still marked
 * `live` belongs to a generation that neither proved stable nor shut
 * down cleanly — it died with its tab — and counts as one more failure.
 * `record` is the folded record to write back (`null` = delete it).
 */
export function crashLoopBootPolicy(
    persisted: RendererStreak | undefined,
    now: number,
    cleanExitToken?: string,
): {
    streak: number;
    downgrade: RendererDowngrade | undefined;
    record: RendererStreak | null | undefined;
} {
    if (!persisted) return { streak: 0, downgrade: undefined, record: undefined };
    const age = now - persisted.at;
    if (persisted.renderer !== 'vello' || age > CRASH_LOOP_DECAY_MS || age < -CRASH_LOOP_DECAY_MS) {
        return { streak: 0, downgrade: undefined, record: null };
    }
    /* Live, and no clean shutdown recorded for exactly that generation
       ⇒ it died with its tab. */
    const died =
        persisted.live &&
        (persisted.token === undefined || persisted.token !== cleanExitToken);
    const count = persisted.count + (died ? 1 : 0);
    const record: RendererStreak | undefined = persisted.live
        ? { renderer: persisted.renderer, count, at: now, live: false }
        : undefined;
    return {
        streak: count,
        downgrade:
            count >= VELLO_TRAP_LIMIT
                ? { from: 'vello', to: 'canvas2d', reason: 'CRASH_LOOP', consecutive_traps: count }
                : undefined,
        record,
    };
}

type Resolver = (v: WorkerReply) => void;

/** Issue #57 — commands that can never move the selection or change the
 *  document (pure read-backs + view probes). Everything else counts toward
 *  `EngineClient.writesInFlight`, conservatively: a command missing here
 *  only costs the synchronous clipboard path a cache miss, never a stale
 *  copy. */
const READ_ONLY_COMMANDS: ReadonlySet<Command['type']> = new Set<Command['type']>([
    'PING',
    'HIT_TEST',
    'HIT_TEST_IN_PAGE',
    'REQUEST_PAINT',
    'REQUEST_ACCESSIBILITY_DELTA',
    'GET_SELECTION_AS_CLIPBOARD',
    'GET_IMAGE_RECTS',
    'REQUEST_STATS',
    'SNAPSHOT',
]);

export class EngineClient {
    private worker!: Worker;
    private nextId = 1;
    private pending = new Map<number, Resolver>();
    private subscribers = new Set<(e: Event) => void>();
    private pendingWrites = 0;
    private documentId: string;
    private onCrash: () => void;
    private recovering = false;
    private workerIsolated = false;
    private activeRenderer = 'canvas2d';
    private lastRecoveryInfo: RecoveryInfo | undefined;
    /** Issue #99 — traps in a row whose generation painted with Vello. */
    private velloTrapStreak = 0;
    /** Issue #99 — sticky for the session once the crash loop tripped.
     *  Issue #240 — also set at boot from the persisted streak. */
    private downgrade: RendererDowngrade | undefined;
    /** Issue #240 — listeners for `downgrade` changes (the Dev HUD). */
    private downgradeListeners = new Set<(d: RendererDowngrade | undefined) => void>();
    /** Issue #315 — listeners for completed recoveries (the recovery
     *  banner, the Dev HUD, the telemetry `CRASH` sample). */
    private recoveryListeners = new Set<(info: RecoveryInfo) => void>();
    /** Issue #240 — whether the boot worker probed the GPU backend.
     *  Issue #270 — re-set by every recovery (a respawned generation
     *  probes unless the crash-loop downgrade forces Canvas2D). */
    private bootProbed = true;
    /** Issue #270 — an in-place renderer retry is retiring the live
     *  worker (guards against a second click). */
    private retiring = false;
    /** Issue #270 — why the NEXT `recover()` runs (`RecoveryInfo.cause`). */
    private pendingCause: RecoveryCause = 'trap';
    /** Issue #333 — checkpoint (snapshot write) health, fed by the
     *  worker's `CHECKPOINT` notices. */
    private checkpoint: CheckpointStatus = { failing: false, failures: 0 };
    private checkpointListeners = new Set<(s: CheckpointStatus) => void>();
    private checkpointFailureListeners = new Set<(failures: number) => void>();
    private checkpointFailureTotal = 0;
    /** Issue #330 — set from the moment the shell is asked to remount a
     *  canvas and call `recover()` until that recovery settles. Lets
     *  "Reload engine" join a recovery already under way instead of
     *  starting a second one. */
    private recoveryPending: { promise: Promise<void>; settle: () => void } | undefined;
    /** Issue #240 — the persisted streak currently marks a live Vello
     *  generation (cleared on a clean `pagehide`). */
    private streakLive = false;
    /** Issue #240 — token of the live Vello generation's record. */
    private liveToken: string | undefined;
    private stableTimer: ReturnType<typeof setTimeout> | undefined;
    /** Worker generations spawned so far (1 = the boot worker). */
    private generations = 0;
    /** Issue #99 — DEV-only `?mockBackend=vello` test hook, forwarded to
     *  the worker (which also ignores it outside DEV). */
    private readonly mockBackend: 'vello' | undefined =
        import.meta.env.DEV &&
        new URLSearchParams(globalThis.location?.search ?? '').get('mockBackend') === 'vello'
            ? 'vello'
            : undefined;

    /**
     * @param documentId identifies the IndexedDB event log for this document.
     * @param onCrash    invoked when the engine worker traps — and, issue
     *                   #270, when it is retired for an in-place renderer
     *                   retry. The handler must create a fresh `<canvas>`,
     *                   transfer it, and call `recover()` —
     *                   `transferControlToOffscreen()` is one-shot, so the
     *                   old surface cannot be reused.
     */
    constructor(documentId: string, onCrash: () => void) {
        this.documentId = documentId;
        this.onCrash = onCrash;
        this.spawn();
        /* Issue #240 — a clean shutdown (reload, navigation, tab close) is
           not a crash: leave the live generation's token in localStorage
           (synchronous — an IndexedDB write started here is routinely
           dropped by the unload) so the next boot does not count it. */
        globalThis.addEventListener?.('pagehide', () => {
            if (this.streakLive && this.liveToken !== undefined) {
                this.streakLive = false;
                try {
                    globalThis.localStorage?.setItem(CLEAN_EXIT_KEY, this.liveToken);
                } catch {
                    /* storage blocked: the next boot counts one failure */
                }
            }
        });
    }

    /** Issue #240 — write the streak for the generation now running (or
     *  that just trapped) off the critical path. */
    private persistStreak(live: boolean): void {
        const record: RendererStreak = {
            renderer: 'vello',
            count: this.velloTrapStreak,
            at: Date.now(),
            live,
        };
        if (live && this.liveToken !== undefined) record.token = this.liveToken;
        void saveRendererStreak(record).catch((e: unknown) =>
            console.warn('[recovery] renderer streak not persisted', e),
        );
    }

    /** Issue #240 — a worker generation came up on `activeRenderer`. */
    private noteGenerationStart(): void {
        if (this.activeRenderer === 'vello') {
            this.streakLive = true;
            this.liveToken = globalThis.crypto?.randomUUID?.() ?? `${Date.now()}-${Math.random()}`;
            this.persistStreak(true);
        } else {
            this.streakLive = false;
        }
    }

    private setDowngrade(d: RendererDowngrade | undefined): void {
        this.downgrade = d;
        for (const fn of this.downgradeListeners) fn(d);
    }

    private spawn(): void {
        /* Requests addressed to the previous worker can never be answered —
           settle them before the fresh worker takes over the id space. */
        for (const resolve of this.pending.values()) {
            resolve({ ok: false, error: 'engine worker respawned; request abandoned' });
        }
        this.pending.clear();
        this.generations += 1;
        this.setCheckpoint({ failing: false, failures: 0 });
        this.worker = new Worker(new URL('./engine.worker.ts', import.meta.url), {
            type: 'module',
        });
        this.worker.onmessage = (ev) => this.handle(ev.data);
        this.worker.onerror = (e) => this.onWorkerError(e);
    }

    async init(canvas: OffscreenCanvas): Promise<void> {
        /* Issue #240 — honour a crash loop that spanned reloads / tab
           deaths: resume its count, and once it reached the limit boot on
           Canvas2D without probing the GPU. An unreadable log is no
           streak. */
        const cleanExit = takeCleanExitToken();
        const policy = crashLoopBootPolicy(
            await loadRendererStreak().catch(() => undefined),
            Date.now(),
            cleanExit,
        );
        this.velloTrapStreak = policy.streak;
        if (policy.record === null) {
            void clearRendererStreak().catch(() => undefined);
        } else if (policy.record) {
            void saveRendererStreak(policy.record).catch(() => undefined);
        }
        if (policy.downgrade) {
            this.setDowngrade(policy.downgrade);
            console.warn(
                `[boot] ${policy.downgrade.consecutive_traps} consecutive Vello failures ` +
                    'persisted (< 24 h) — booting on Canvas2D without probing the GPU',
            );
        }
        /* Issue #330 - a reload that carries the document: recover from the
           log instead of starting a session (INIT clears it). Only when the
           log actually holds something; an empty one is a normal boot. */
        if (takeCarryOver(this.documentId)) {
            const log = await loadRecoveryLog().catch(() => undefined);
            if (
                log &&
                (log.commands.length > 0 || log.candidates.some((c) => c.snapshot.length > 0))
            ) {
                this.worker.terminate();
                this.pendingCause = 'page-reload';
                await this.recover(canvas);
                return;
            }
        }
        const r = await this.send(
            {
                type: 'INIT',
                canvas,
                documentId: this.documentId,
                ...(this.mockBackend ? { mockBackend: this.mockBackend } : {}),
                ...(this.downgrade ? { forceRenderer: 'canvas2d' } : {}),
            },
            [canvas],
        );
        if (!r.ok) throw new Error(r.error);
        this.workerIsolated = r.crossOriginIsolated === true;
        this.activeRenderer = r.renderer ?? 'canvas2d';
        this.bootProbed = r.probed !== false;
        this.noteGenerationStart();
        this.armStableTimer();
    }

    /** Issue #240 — whether the current worker generation probed the GPU
     *  backend (`false` when a crash-loop downgrade — persisted or
     *  tripped this session — forced Canvas2D). */
    get rendererProbed(): boolean {
        return this.bootProbed;
    }

    /** Issue #240 — observe the crash-loop downgrade (set at boot from the
     *  persisted streak, or by a recovery). Returns the unsubscribe. */
    onRendererDowngrade(fn: (d: RendererDowngrade | undefined) => void): () => void {
        this.downgradeListeners.add(fn);
        return () => {
            this.downgradeListeners.delete(fn);
        };
    }

    /**
     * Issue #240 — the Dev HUD's "retry Vello": forget the crash-loop
     * record and probe the GPU renderer again.
     *
     * Issue #270 — IN PLACE, never a reload: a reload is a new session,
     * whose boot clears the event log, so an unsaved document was lost.
     * Instead the live worker is RETIRED — it applies everything already
     * dispatched, snapshots the log head, flushes its event-log writes and
     * closes itself — and the shell respawns it exactly like a crash
     * recovery, but without a trap: `onCrash` remounts a fresh
     * `<canvas>` (whose context is not fixed yet), `recover()` restores
     * the document, selection, undo window and zoom from the log, and,
     * the downgrade being lifted, re-probes the GPU. Commands sent after
     * the retirement settle like requests to a trapped worker. If the
     * retry generation traps on Vello again, the ordinary #99 streak
     * counts it from zero.
     */
    async retryGpuRenderer(): Promise<void> {
        if (this.recovering || this.retiring) return;
        this.retiring = true;
        const generation = this.generations;
        let retired: WorkerReply;
        try {
            await clearRendererStreak().catch((e: unknown) =>
                console.warn('[recovery] renderer streak not cleared', e),
            );
            if (this.stableTimer !== undefined) {
                clearTimeout(this.stableTimer);
                this.stableTimer = undefined;
            }
            this.velloTrapStreak = 0;
            this.streakLive = false;
            this.setDowngrade(undefined);
            retired = await this.send({ type: 'RETIRE' });
        } finally {
            this.retiring = false;
        }
        /* A trap while retiring already runs the ordinary recovery (with
           the downgrade lifted, it re-probes too): nothing left to do. */
        if (this.recovering || this.generations !== generation || retired.trap) return;
        if (!retired.ok) {
            console.warn('[recovery] renderer retry: the worker did not retire cleanly', retired.error);
        }
        this.markRecoveryPending();
        this.respawnAfterRetire('renderer-retry');
    }

    private markRecoveryPending(): void {
        if (this.recoveryPending) return;
        let settle!: () => void;
        const promise = new Promise<void>((r) => (settle = r));
        this.recoveryPending = { promise, settle };
    }

    /**
     * Issue #330 — the crash overlay's "Reload engine": restart the engine
     * IN PLACE from the event log, never a page reload (a reload is a new
     * session, whose boot clears the log — the document was lost). A
     * recovery already under way is joined, not repeated. Otherwise the
     * live worker is retired (everything dispatched is applied, the log
     * head snapshotted and flushed — the #270 path) and respawned exactly
     * like a crash recovery. A worker that does not answer the retirement
     * within `RETIRE_TIMEOUT_MS` is terminated anyway: the log already
     * holds everything it acknowledged. Resolves once the recovery
     * settles.
     */
    async restartInPlace(): Promise<void> {
        if (this.recoveryPending) return this.recoveryPending.promise;
        if (this.retiring) return;
        this.retiring = true;
        const generation = this.generations;
        let retired: WorkerReply;
        try {
            retired = await this.retireWorker();
        } finally {
            this.retiring = false;
        }
        const joined = this.recoveryPending as { promise: Promise<void> } | undefined;
        if (joined) return joined.promise;
        if (this.generations !== generation || retired.trap) return;
        if (!retired.ok) {
            console.warn('[recovery] engine reload: the worker did not retire cleanly', retired.error);
        }
        this.markRecoveryPending();
        const done = this.recoveryPending!.promise;
        this.respawnAfterRetire('engine-reload');
        return done;
    }

    /** Retire the live worker (RETIRE), bounded by `RETIRE_TIMEOUT_MS`. */
    private retireWorker(): Promise<WorkerReply> {
        let timer: ReturnType<typeof setTimeout> | undefined;
        const timeout = new Promise<WorkerReply>((resolve) => {
            timer = setTimeout(
                () => resolve({ ok: false, error: 'worker did not answer the retirement' }),
                RETIRE_TIMEOUT_MS,
            );
        });
        return Promise.race([this.send({ type: 'RETIRE' }), timeout]).finally(() => {
            if (timer !== undefined) clearTimeout(timer);
        });
    }

    /** Terminate the retired worker, settle its requests and hand over to
     *  the shell (`onCrash` remounts a fresh canvas -> `recover()`). */
    private respawnAfterRetire(cause: RecoveryCause): void {
        this.worker.terminate();
        this.recovering = true;
        for (const resolve of this.pending.values()) {
            resolve({ ok: false, error: 'engine worker restarted in place' });
        }
        this.pending.clear();
        this.pendingCause = cause;
        this.onCrash();
    }

    /**
     * Issue #330 — the crash overlay's "Reload page": make the coming page
     * reload carry the document. The live worker (if any) is retired so
     * the log holds its head, then a one-shot token is left in
     * `sessionStorage`; the next boot's `init()` honours it by recovering
     * from the log instead of starting a new session (which would clear
     * it). A plain reload without this call still starts a new session.
     */
    async prepareCarryOver(): Promise<void> {
        if (!this.recoveryPending && !this.recovering && !this.retiring) {
            this.retiring = true;
            try {
                await this.retireWorker();
            } finally {
                this.retiring = false;
            }
        }
        writeCarryOver(this.documentId);
    }

    /** Issue #333 — current checkpoint health (`failing` once the retries
     *  of a failed snapshot write are exhausted). */
    get checkpointStatus(): CheckpointStatus {
        return this.checkpoint;
    }

    /** Issue #333 — failed snapshot writes seen this session (the
     *  telemetry counter). */
    get checkpointFailures(): number {
        return this.checkpointFailureTotal;
    }

    /** Issue #333 — observe checkpoint health changes. */
    onCheckpointStatus(fn: (s: CheckpointStatus) => void): () => void {
        this.checkpointListeners.add(fn);
        return () => {
            this.checkpointListeners.delete(fn);
        };
    }

    /** Issue #333 — fires once per failed snapshot write (every retry
     *  counts), with the length of the current failure run. */
    onCheckpointFailure(fn: (failures: number) => void): () => void {
        this.checkpointFailureListeners.add(fn);
        return () => {
            this.checkpointFailureListeners.delete(fn);
        };
    }

    private setCheckpoint(next: CheckpointStatus): void {
        if (next.failing === this.checkpoint.failing && next.failures === this.checkpoint.failures) {
            return;
        }
        this.checkpoint = next;
        for (const fn of this.checkpointListeners) fn(next);
    }

    private onCheckpointNotice(msg: { state?: string; failures?: number }): void {
        const failures = typeof msg.failures === 'number' ? msg.failures : 0;
        if (msg.state === 'failed') {
            this.checkpointFailureTotal += 1;
            for (const fn of this.checkpointFailureListeners) fn(failures);
        } else if (msg.state === 'exhausted') {
            this.setCheckpoint({ failing: true, failures });
        } else if (msg.state === 'ok') {
            this.setCheckpoint({ failing: false, failures: 0 });
        }
    }

    /** Worker generations spawned so far (1 = boot; +1 per recovery). */
    get generation(): number {
        return this.generations;
    }

    /** Issue #99 — the crash-loop downgrade in force for this session, or
     *  `undefined` while the GPU backend is still trusted. */
    get rendererDowngrade(): RendererDowngrade | undefined {
        return this.downgrade;
    }

    /** Issue #99 — a generation that survives STABLE_GENERATION_MS ends a
     *  trap streak. Never lifts an active downgrade (that stays for the
     *  session: the GPU path already proved it can crash-loop). */
    private armStableTimer(): void {
        if (this.stableTimer !== undefined) clearTimeout(this.stableTimer);
        this.stableTimer = setTimeout(() => {
            this.stableTimer = undefined;
            this.velloTrapStreak = 0;
            /* Issue #240 — a STABLE Vello generation ends the persisted
               streak too. A Canvas2D generation says nothing about the
               GPU path: a persisted downgrade keeps decaying on its own. */
            if (this.activeRenderer === 'vello') {
                this.streakLive = false;
                void clearRendererStreak().catch(() => undefined);
            }
        }, STABLE_GENERATION_MS);
    }

    /** D2.3: whether the engine worker reported `crossOriginIsolated === true`. */
    get crossOriginIsolated(): boolean {
        return this.workerIsolated;
    }

    /** Backlog #4: the renderer the worker picked at INIT — `vello` or
     *  `canvas2d`. Set by `init()`; `canvas2d` before INIT completes.
     *  Issue #66 — re-set by every `recover()` from the recovered
     *  engine's own report, so it tracks the worker generation that is
     *  actually painting. */
    get renderer(): string {
        return this.activeRenderer;
    }

    /** Issue #85 — outcome of the most recent `recover()`; `undefined`
     *  until the first recovery. The shell reads `restored` to decide
     *  between re-seeding a blank page and repainting the restored one. */
    get lastRecovery(): RecoveryInfo | undefined {
        return this.lastRecoveryInfo;
    }

    /**
     * Issue #315 — observe completed recoveries. Fires once per
     * `recover()`, AFTER `lastRecovery` holds its outcome (unlike the
     * `RECOVERED` event, which subscribers see before the client has
     * folded the worker's reply into `RecoveryInfo`). Returns the
     * unsubscribe.
     */
    onRecovery(fn: (info: RecoveryInfo) => void): () => void {
        this.recoveryListeners.add(fn);
        return () => {
            this.recoveryListeners.delete(fn);
        };
    }

    /**
     * Recover after a trap: load the persisted snapshots + command log,
     * spawn a fresh worker, and replay via `Command::Recover` (newest
     * snapshot first, falling back to older ones — issue #241). `canvas` must be a
     * brand-new OffscreenCanvas — the trapped surface is gone.
     */
    async recover(canvas: OffscreenCanvas): Promise<void> {
        try {
            await this.recoverInner(canvas);
        } finally {
            /* Issue #330 - settle the pending-recovery latch, unless the
               replay itself trapped again (a new recovery is then due). */
            if (!this.recovering && this.recoveryPending) {
                this.recoveryPending.settle();
                this.recoveryPending = undefined;
            }
        }
    }

    private async recoverInner(canvas: OffscreenCanvas): Promise<void> {
        /* Issue #270 — read before any await: a trap during this recovery
           starts the next one as a plain `trap`. */
        const cause = this.pendingCause;
        this.pendingCause = 'trap';
        /* An event-log read failure (IndexedDB rejection / corruption) must
           not strand a dead client — respawn with an empty log instead. */
        const recoveryLog = await loadRecoveryLog().catch((e: unknown): RecoveryLog => {
            console.error('event log unreadable; recovering with an empty log', e);
            return {
                candidates: [{ seq: 0, snapshot: new Uint8Array(0) }],
                commands: [],
                lastSeq: 0,
                logComplete: false,
            };
        });
        /* Issue #99 — N traps in a row on Vello: stop re-probing the GPU
           (it would pick Vello again and crash-loop) and force Canvas2D for
           this and every later generation of the session. */
        /* Issue #315 — whether THIS recovery trips the downgrade (the
           banner announces it once, not on every later recovery). */
        let downgradedNow = false;
        if (this.downgrade === undefined && this.velloTrapStreak >= VELLO_TRAP_LIMIT) {
            downgradedNow = true;
            this.setDowngrade({
                from: 'vello',
                to: 'canvas2d',
                reason: 'CRASH_LOOP',
                consecutive_traps: this.velloTrapStreak,
            });
            console.warn(
                `[recovery] ${this.velloTrapStreak} consecutive traps on Vello — ` +
                    'booting the recovered engine on Canvas2D',
            );
        }
        this.spawn();
        /* Clear the guard BEFORE awaiting the reply: if the RECOVER replay
           itself traps, handle() runs onTrap() synchronously — ahead of this
           await's continuation — and must re-fire onCrash, not drop it. */
        this.recovering = false;
        const r = await this.send(
            {
                type: 'RECOVER',
                canvas,
                candidates: recoveryLog.candidates,
                commands: recoveryLog.commands,
                lastSeq: recoveryLog.lastSeq,
                logComplete: recoveryLog.logComplete,
                ...(this.mockBackend ? { mockBackend: this.mockBackend } : {}),
                ...(this.downgrade
                    ? { forceRenderer: 'canvas2d', rendererDowngrade: this.downgrade }
                    : {}),
            },
            [
                canvas,
                /* Issue #212 — candidates naming one package share its
                   bytes: each buffer is transferred once. */
                ...new Set(
                    recoveryLog.candidates.flatMap((c) =>
                        [c.snapshot, c.package]
                            .filter((b): b is Uint8Array => b !== undefined)
                            .map((b) => b.buffer as ArrayBuffer),
                    ),
                ),
            ],
        );
        if (!r.ok) throw new Error(r.error);
        this.activeRenderer = r.renderer ?? 'canvas2d';
        if (r.crossOriginIsolated !== undefined) this.workerIsolated = r.crossOriginIsolated;
        /* Issue #270 — this generation probed unless it was forced. */
        this.bootProbed = this.downgrade === undefined;
        const recovered = r.evt?.type === 'RECOVERED' ? r.evt : undefined;
        const deviceScale = recovered?.device_scale;
        this.lastRecoveryInfo = {
            restored: r.restored === true,
            appliedCommands: r.appliedCommands ?? 0,
            renderer: this.activeRenderer,
            zoom: recovered?.zoom ?? 1,
            deviceScale,
            layoutRestored: r.restored === true || deviceScale !== undefined,
            rendererDowngrade: recovered?.renderer_downgrade,
            snapshotFallbacks: r.snapshotFallbacks ?? 0,
            logTruncated: r.restored !== true && r.logComplete === false,
            packageFallbacks: r.packageFallbacks ?? 0,
            packageLost: r.packageLost === true,
            pinnedBase: r.pinnedBase === true,
            tailDropped: r.tailDropped === true,
            rendererDowngraded: downgradedNow && recovered?.renderer_downgrade !== undefined,
            baseSnapshotAt: r.restored === true ? r.baseTakenAt : undefined,
            cause,
        };
        this.noteGenerationStart();
        if (this.lastRecoveryInfo.logTruncated) {
            console.error(
                '[recovery] no persisted snapshot restored and the event log was pruned — ' +
                    'the document could not be rebuilt',
            );
        }
        if (this.lastRecoveryInfo.tailDropped) {
            console.error(
                '[recovery] only the pinned base snapshot restored — edits made after it ' +
                    'were lost',
            );
        }
        if (this.lastRecoveryInfo.packageLost) {
            console.error(
                '[recovery] the recovered document lost its source package — saving ' +
                    'drops the original file’s sibling parts',
            );
        }
        this.armStableTimer();
        /* Issue #315 — after `lastRecovery` is final. */
        const info = this.lastRecoveryInfo;
        for (const fn of this.recoveryListeners) fn(info);
    }

    /**
     * Issue #85 — `Command::Snapshot`: the versioned engine snapshot
     * (`engine::snapshot` envelope) of the live session. Read-only; the
     * same bytes the worker persists on its cadence.
     */
    async snapshot(): Promise<Uint8Array> {
        const evt = await this.dispatch({ type: 'SNAPSHOT', seq: undefined });
        if (evt.type !== 'SNAPSHOT') {
            throw new Error(evt.type === 'ERROR' ? evt.message : `snapshot: unexpected ${evt.type}`);
        }
        return evt.bytes;
    }

    /**
     * Issue #85 fault injection (test hook, ts/e2e/crash-recovery.spec.ts):
     * make the worker trap the wasm instance for real after its next
     * `afterCommands` logged commands, flushing the event log first. Unlike
     * `forceTrap()` this exercises the genuine `RuntimeError` → close →
     * respawn → `RECOVER` path.
     */
    async armTrap(afterCommands = 1): Promise<void> {
        const r = await this.send({ type: 'ARM_TRAP', after_commands: afterCommands });
        if (!r.ok) throw new Error(r.error);
    }

    /**
     * Issue #96 test hook (DEV builds only; the worker refuses it in a
     * production build): opaque non-white pixel count per page surface
     * the CURRENT worker generation holds, read back worker-side — the
     * only paint evidence headless Chrome offers for the full app. `-1`
     * for a surface without a 2d context (Vello page 0); a page the
     * shell never registered with this worker is absent.
     */
    async probePageInk(): Promise<Record<number, number>> {
        const r = await this.send({ type: 'PROBE_PAGE_INK' });
        if (!r.ok) throw new Error(r.error);
        return r.ink ?? {};
    }

    async dispatch(cmd: Command, transfer: Transferable[] = []): Promise<Event> {
        const write = !READ_ONLY_COMMANDS.has(cmd.type);
        if (write) this.pendingWrites += 1;
        let r: WorkerReply;
        try {
            r = await this.send({ cmd }, transfer);
        } finally {
            if (write) this.pendingWrites -= 1;
        }
        if (!r.ok) throw new Error(r.error);
        return r.evt!;
    }

    /** Issue #57 — dispatched commands that may move the selection or
     *  change the document and have not replied yet. While non-zero, the
     *  main thread's view of the selection may be behind the engine's, so
     *  the synchronous clipboard cache must not be trusted. */
    get writesInFlight(): number {
        return this.pendingWrites;
    }

    /**
     * D2.4: dispatch `LoadFont`, handing the font buffer to the worker as a
     * Transferable so the payload moves zero-copy instead of being cloned.
     */
    async loadFont(id: string, bytes: Uint8Array): Promise<Event> {
        return this.dispatch({ type: 'LOAD_FONT', id, bytes }, [bytes.buffer as ArrayBuffer]);
    }

    /**
     * D2.4: dispatch `OpenDocument`, transferring the document buffer zero-copy.
     * Issue #221 — `defaults` (page size / widow control fallbacks) rides
     * straight onto `OPEN_DOCUMENT.defaults`; omitted, the wire field is
     * left out entirely (`format_docx::read_docx`'s unchanged behaviour).
     */
    async openDocument(
        bytes: Uint8Array,
        format: DocFormat,
        name?: string,
        defaults?: DocumentDefaults,
    ): Promise<Event> {
        /* `defaults` on `Command::OpenDocument` is `#[tsify(optional)]`;
           under `exactOptionalPropertyTypes` the key must be OMITTED when
           unset, not set to `undefined`. */
        return this.dispatch(
            defaults === undefined
                ? { type: 'OPEN_DOCUMENT', bytes, format, name }
                : { type: 'OPEN_DOCUMENT', bytes, format, name, defaults },
            [bytes.buffer as ArrayBuffer],
        );
    }

    /**
     * Phase 6c — multi-canvas DOM architecture. Register a fresh
     * `OffscreenCanvas` for page `idx`. The TS shell mounts one
     * `<canvas>` per paginated page; this hands its surface to the
     * worker so the engine paints each page into its own DOM
     * element. The next paint fills the registered surface.
     */
    async registerPageCanvas(idx: number, canvas: OffscreenCanvas): Promise<void> {
        const r = await this.send({ type: 'REGISTER_PAGE_CANVAS', idx, canvas }, [
            canvas as unknown as Transferable,
        ]);
        if (!r.ok) throw new Error(r.error);
    }

    /**
     * Phase 8a — read-only snapshot of every `<w:comment>` + the
     * matching `<w:commentRangeStart>`/`<w:commentRangeEnd>` span. The
     * shell renders these in a sidebar; no canvas overlay (per the
     * Phase 8a MVP cut). Pure metadata read — no event-log mutation.
     */
    async commentsSnapshot(): Promise<CommentSnapshot[]> {
        const r = await this.send({ type: 'GET_COMMENTS' });
        if (!r.ok) throw new Error(r.error);
        return (r.comments ?? []) as CommentSnapshot[];
    }

    /**
     * Phase 8b — read-only snapshot of every tracked-change revision.
     * The shell ties the rows to canvas geometry via `document_geometry`
     * so a hover over deleted (strike-through) or inserted (underlined)
     * text surfaces the author + date.
     */
    async revisionsSnapshot(): Promise<RevisionSnapshot[]> {
        const r = await this.send({ type: 'GET_REVISIONS' });
        if (!r.ok) throw new Error(r.error);
        return (r.revisions ?? []) as RevisionSnapshot[];
    }

    subscribe(fn: (e: Event) => void): () => void {
        this.subscribers.add(fn);
        return () => {
            this.subscribers.delete(fn);
        };
    }

    /**
     * Test hook (ts/e2e/crash-recovery.spec.ts): simulate a worker crash by
     * terminating the worker and running the trap-recovery path.
     */
    forceTrap(): void {
        this.worker.terminate();
        this.onTrap('forced trap (test hook)');
    }

    private send(payload: any, transfer: Transferable[] = []) {
        if (this.recovering) {
            /* The worker is dead and the respawn has not completed yet — a
               postMessage would hang forever. Settle immediately instead. */
            return Promise.resolve<WorkerReply>({
                ok: false,
                error: 'engine worker trapped; recovery in progress',
                trap: true,
            });
        }
        return new Promise<WorkerReply>(
            (resolve) => {
                const id = this.nextId++;
                this.pending.set(id, resolve);
                this.worker.postMessage({ id, ...payload }, transfer);
            },
        );
    }

    private handle(msg: any): void {
        /* Issue #333 — an unsolicited worker notice (no id, no reply). */
        if (msg.notice === 'CHECKPOINT') {
            this.onCheckpointNotice(msg);
            return;
        }
        const cb = this.pending.get(msg.id);
        if (cb) {
            this.pending.delete(msg.id);
            cb(msg);
        }
        if (msg.evt) this.subscribers.forEach((s) => s(msg.evt));
        if (msg.trap) {
            this.onTrap(typeof msg.error === 'string' ? msg.error : 'engine worker trapped');
        }
    }

    private onWorkerError(e: ErrorEvent): void {
        console.error('worker error', e);
        this.onTrap(e.message || 'worker error');
    }

    /**
     * Trap handler. Rejects every in-flight request — the dead worker will
     * never answer — then hands off to the UI shell via `onCrash`. The UI
     * supplies a fresh canvas and calls `recover()`. The `recovering` guard
     * drops a duplicate report (the worker posts `{ trap: true }` and may
     * also fire `onerror`); `recover()` clears it.
     */
    private onTrap(stack: string): void {
        if (this.recovering) return;
        this.recovering = true;
        /* Issue #99 — count the streak by the backend the TRAPPED
           generation painted with; any non-Vello trap breaks it. */
        if (this.stableTimer !== undefined) {
            clearTimeout(this.stableTimer);
            this.stableTimer = undefined;
        }
        this.velloTrapStreak = this.activeRenderer === 'vello' ? this.velloTrapStreak + 1 : 0;
        /* Issue #240 — persist it: a reload mid-loop resumes the count.
           A non-Vello trap ends the streak, unless a downgrade is in force
           (then the record keeps decaying on its own clock). */
        if (this.activeRenderer === 'vello') {
            this.streakLive = false;
            this.persistStreak(false);
        } else if (this.downgrade === undefined) {
            this.streakLive = false;
            void clearRendererStreak().catch(() => undefined);
        }
        for (const resolve of this.pending.values()) {
            resolve({ ok: false, error: 'engine worker trapped; recovering', trap: true });
        }
        this.pending.clear();
        /* The worker dies before it can emit `Event::Trap` itself, so
           synthesize the bridge-shaped event (crates/bridge/src/event.rs) —
           subscribers (TrapOverlay, telemetry ENGINE_TRAP) see the crash. */
        const trapEvt: Event = { type: 'TRAP', stack };
        this.subscribers.forEach((s) => s(trapEvt));
        this.markRecoveryPending();
        this.onCrash();
    }
}
