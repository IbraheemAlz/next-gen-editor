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
import type { Command, DocFormat, Event } from '../../crates/engine-wasm/pkg/engine_wasm.js';
import { loadLatestEventLog } from './event-log';

type Resolver = (v: { ok: boolean; evt?: Event; error?: string; trap?: boolean }) => void;

export class EngineClient {
    private worker!: Worker;
    private nextId = 1;
    private pending = new Map<number, Resolver>();
    private subscribers = new Set<(e: Event) => void>();
    private documentId: string;
    private onCrash: () => void;
    private recovering = false;

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
        this.worker = new Worker(new URL('./engine.worker.ts', import.meta.url), {
            type: 'module',
        });
        this.worker.onmessage = (ev) => this.handle(ev.data);
        this.worker.onerror = (e) => this.onWorkerError(e);
    }

    async init(canvas: OffscreenCanvas): Promise<void> {
        await this.send({ type: 'INIT', canvas, documentId: this.documentId }, [canvas]);
    }

    /**
     * Recover after a trap: load the latest snapshot + command tail, spawn a
     * fresh worker, and replay via `Command::Recover`. `canvas` must be a
     * brand-new OffscreenCanvas — the trapped surface is gone.
     */
    async recover(canvas: OffscreenCanvas): Promise<void> {
        try {
            const { snapshot, log, snapshotSeq, lastSeq } = await loadLatestEventLog(
                this.documentId,
            );
            this.spawn();
            await this.send({ type: 'RECOVER', canvas, snapshot, log, snapshotSeq, lastSeq }, [
                canvas,
                snapshot.buffer as ArrayBuffer,
            ]);
        } finally {
            this.recovering = false;
        }
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

    subscribe(fn: (e: Event) => void): () => void {
        this.subscribers.add(fn);
        return () => {
            this.subscribers.delete(fn);
        };
    }

    private send(payload: any, transfer: Transferable[] = []) {
        return new Promise<{ ok: boolean; evt?: Event; error?: string; trap?: boolean }>(
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
        if (msg.trap) this.onTrap();
    }

    private onWorkerError(e: ErrorEvent): void {
        console.error('worker error', e);
        this.onTrap();
    }

    /**
     * Trap handler. Rejects every in-flight request — the dead worker will
     * never answer — then hands off to the UI shell via `onCrash`. The UI
     * supplies a fresh canvas and calls `recover()`. The `recovering` guard
     * drops a duplicate report (the worker posts `{ trap: true }` and may
     * also fire `onerror`); `recover()` clears it.
     */
    private onTrap(): void {
        if (this.recovering) return;
        this.recovering = true;
        for (const resolve of this.pending.values()) {
            resolve({ ok: false, error: 'engine worker trapped; recovering', trap: true });
        }
        this.pending.clear();
        this.onCrash();
    }
}
