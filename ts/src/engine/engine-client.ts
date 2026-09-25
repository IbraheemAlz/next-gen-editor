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
    Command,
    DocFormat,
    Event,
    RendererDowngrade,
} from '../../../crates/engine-wasm/pkg/engine_wasm.js';
import { loadLatestEventLog } from './event-log';

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
    kind: 'insert' | 'delete';
    author: string;
    date: string;
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
}

/** Issue #99 — consecutive traps on the Vello backend after which recovery
 *  stops re-probing the GPU and forces Canvas2D (so a persistently failing
 *  driver / shader path costs at most N + 1 worker generations). */
export const VELLO_TRAP_LIMIT = 2;
/** Issue #99 — a generation that stays up this long ends the streak: two
 *  unrelated traps an hour apart are not a crash loop. */
const STABLE_GENERATION_MS = 60_000;

type Resolver = (v: WorkerReply) => void;

export class EngineClient {
    private worker!: Worker;
    private nextId = 1;
    private pending = new Map<number, Resolver>();
    private subscribers = new Set<(e: Event) => void>();
    private documentId: string;
    private onCrash: () => void;
    private recovering = false;
    private workerIsolated = false;
    private activeRenderer = 'canvas2d';
    private lastRecoveryInfo: RecoveryInfo | undefined;
    /** Issue #99 — traps in a row whose generation painted with Vello. */
    private velloTrapStreak = 0;
    /** Issue #99 — sticky for the session once the crash loop tripped. */
    private downgrade: RendererDowngrade | undefined;
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
     * @param onCrash    invoked when the engine worker traps. The handler must
     *                   create a fresh `<canvas>`, transfer it, and call
     *                   `recover()` — `transferControlToOffscreen()` is
     *                   one-shot, so the trapped surface cannot be reused.
     */
    constructor(documentId: string, onCrash: () => void) {
        this.documentId = documentId;
        this.onCrash = onCrash;
        this.spawn();
    }

    private spawn(): void {
        /* Requests addressed to the previous worker can never be answered —
           settle them before the fresh worker takes over the id space. */
        for (const resolve of this.pending.values()) {
            resolve({ ok: false, error: 'engine worker respawned; request abandoned' });
        }
        this.pending.clear();
        this.generations += 1;
        this.worker = new Worker(new URL('./engine.worker.ts', import.meta.url), {
            type: 'module',
        });
        this.worker.onmessage = (ev) => this.handle(ev.data);
        this.worker.onerror = (e) => this.onWorkerError(e);
    }

    async init(canvas: OffscreenCanvas): Promise<void> {
        const r = await this.send(
            {
                type: 'INIT',
                canvas,
                documentId: this.documentId,
                ...(this.mockBackend ? { mockBackend: this.mockBackend } : {}),
            },
            [canvas],
        );
        if (!r.ok) throw new Error(r.error);
        this.workerIsolated = r.crossOriginIsolated === true;
        this.activeRenderer = r.renderer ?? 'canvas2d';
        this.armStableTimer();
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
     * Recover after a trap: load the latest snapshot + command tail, spawn a
     * fresh worker, and replay via `Command::Recover`. `canvas` must be a
     * brand-new OffscreenCanvas — the trapped surface is gone.
     */
    async recover(canvas: OffscreenCanvas): Promise<void> {
        /* An event-log read failure (IndexedDB rejection / corruption) must
           not strand a dead client — respawn with an empty log instead. */
        const { snapshot, log, snapshotSeq, lastSeq } = await loadLatestEventLog(
            this.documentId,
        ).catch((e: unknown): { snapshot: Uint8Array; log: Command[]; snapshotSeq: number; lastSeq: number } => {
            console.error('event log unreadable; recovering with an empty log', e);
            return { snapshot: new Uint8Array(0), log: [], snapshotSeq: 0, lastSeq: 0 };
        });
        /* Issue #99 — N traps in a row on Vello: stop re-probing the GPU
           (it would pick Vello again and crash-loop) and force Canvas2D for
           this and every later generation of the session. */
        if (this.downgrade === undefined && this.velloTrapStreak >= VELLO_TRAP_LIMIT) {
            this.downgrade = {
                from: 'vello',
                to: 'canvas2d',
                reason: 'CRASH_LOOP',
                consecutive_traps: this.velloTrapStreak,
            };
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
                snapshot,
                log,
                snapshotSeq,
                lastSeq,
                ...(this.mockBackend ? { mockBackend: this.mockBackend } : {}),
                ...(this.downgrade
                    ? { forceRenderer: 'canvas2d', rendererDowngrade: this.downgrade }
                    : {}),
            },
            [canvas, snapshot.buffer as ArrayBuffer],
        );
        if (!r.ok) throw new Error(r.error);
        this.activeRenderer = r.renderer ?? 'canvas2d';
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
        };
        this.armStableTimer();
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
        const r = await this.send({ cmd }, transfer);
        if (!r.ok) throw new Error(r.error);
        return r.evt!;
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
     */
    async openDocument(bytes: Uint8Array, format: DocFormat, name?: string): Promise<Event> {
        return this.dispatch({ type: 'OPEN_DOCUMENT', bytes, format, name }, [
            bytes.buffer as ArrayBuffer,
        ]);
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
        for (const resolve of this.pending.values()) {
            resolve({ ok: false, error: 'engine worker trapped; recovering', trap: true });
        }
        this.pending.clear();
        /* The worker dies before it can emit `Event::Trap` itself, so
           synthesize the bridge-shaped event (crates/bridge/src/event.rs) —
           subscribers (TrapOverlay, telemetry ENGINE_TRAP) see the crash. */
        const trapEvt: Event = { type: 'TRAP', stack };
        this.subscribers.forEach((s) => s(trapEvt));
        this.onCrash();
    }
}
