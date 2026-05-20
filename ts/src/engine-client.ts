/* EngineClient — typed RPC wrapper around the engine Web Worker.
 * PHASE_2_BRIDGE_MEMORY.md §7.
 *
 * Spawns the dedicated worker, matches request/response by `id`, and fans
 * unidirectional events (Painted, SelectionChanged, …) out to subscribers.
 *
 * Note: the §7 spec imports types from `crates/bridge/pkg/types`. The bridge
 * crate is not built standalone — its `Command`/`Event` types are generated
 * (via `tsify-next`) into the `engine-wasm` wasm-pack package, so that is the
 * real import site. */
import type { Command, Event } from '../../crates/engine-wasm/pkg/engine_wasm.js';
import { loadLatestEventLog } from './event-log';

type Resolver = (v: { ok: boolean; evt?: Event; error?: string; trap?: boolean }) => void;

export class EngineClient {
    private worker!: Worker;
    private nextId = 1;
    private pending = new Map<number, Resolver>();
    private subscribers = new Set<(e: Event) => void>();
    private documentId: string;

    constructor(documentId: string) {
        this.documentId = documentId;
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

    async recover(canvas: OffscreenCanvas, snapshot: Uint8Array, log: Command[]): Promise<void> {
        await this.send({ type: 'RECOVER', canvas, snapshot, log }, [
            canvas,
            snapshot.buffer as ArrayBuffer,
        ]);
    }

    async dispatch(cmd: Command, transfer: Transferable[] = []): Promise<Event> {
        const r = await this.send({ cmd }, transfer);
        if (!r.ok) throw new Error(r.error);
        return r.evt!;
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
        if (msg.trap) void this.onTrap();
    }

    private onWorkerError(e: ErrorEvent): void {
        console.error('worker error', e);
        void this.onTrap();
    }

    private async onTrap(): Promise<void> {
        /* Phase 2 D2.7 crash-recovery: read the latest snapshot + command
           tail, respawn the worker, and replay into a fresh engine. */
        const { snapshot, log } = await loadLatestEventLog(this.documentId);
        this.spawn();
        const canvas = newOffscreenForTrapRecovery();
        await this.recover(canvas, snapshot, log);
    }
}

/* Placeholder recovery surface. Phase 2 D2.7 re-acquires the real
   OffscreenCanvas from the main-thread canvas element. */
function newOffscreenForTrapRecovery(): OffscreenCanvas {
    return new OffscreenCanvas(1, 1);
}
