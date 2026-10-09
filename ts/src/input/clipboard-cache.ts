/* Issue #57 — synchronous clipboard payload cache.
 *
 * `copy()` / `cut()` in `clipboard.ts` await a GET_SELECTION_AS_CLIPBOARD
 * worker round-trip between the trusted copy gesture and the async
 * `navigator.clipboard` write; if document focus lapses inside that window
 * the write is blocked. This module keeps the CURRENT selection's
 * `text/plain` + `text/html` payload warm on the main thread so the copy /
 * cut handlers can call `e.clipboardData.setData(…)` synchronously inside
 * the trusted event — no await, no focus dependency.
 *
 * Freshness contract (the cache is only ever a prediction of what the
 * engine would answer right now; a miss falls back to the async path).
 * Issue #260 — an entry is keyed on three engine-derived values, with no
 * hand-kept list of "invalidating" event types:
 *
 * - `EngineClient.writeEpoch` — moves as every write command is answered
 *   (write = `CommandMeta.read_only` is false, generated from
 *   `bridge::meta`) and on trap / respawn, BEFORE the reply reaches
 *   subscribers;
 * - the engine document revision — `SELECTION_CHANGED.document_revision`
 *   and `PAINTED.mutation_seq` (the same #194 counter), which also catches
 *   worker-internal mutations no client command answered;
 * - the selection key — range + editing story from SELECTION_CHANGED.
 *
 * - A prefetch is dispatched only while no write command is in flight
 *   (`EngineClient.writesInFlight === 0`), so the serialized worker queue
 *   reads exactly the state the main thread has already observed; its reply
 *   is kept only when none of the three moved meanwhile.
 * - A hit additionally requires zero writes in flight at copy time (a
 *   Shift+Arrow still on its way would otherwise copy the pre-move range)
 *   and all three to equal the live values.
 *
 * Load: the prefetch fires ~150 ms after the selection SETTLES (trailing
 * debounce — a drag-selection emits no GET_SELECTION_AS_CLIPBOARD until it
 * pauses), only for non-collapsed selections, and asks the engine to skip
 * the `.docx` fragment ZIP (`include_docx: false`). Kill switch:
 * `?clipboardPrefetch=0`. */
import { createEffect, createMemo, on, onCleanup } from 'solid-js';
import type { EngineClient } from '../engine/engine-client';
import type { Event, LogicalPos } from '../engine/types';
import type { EngineStore } from '../state/engine-store';

/** Trailing debounce between the last selection change and the prefetch. */
export const PREFETCH_DEBOUNCE_MS = 150;

export interface CachedClipboardPayload {
    plain: string;
    html: string;
}

export interface ClipboardPrefetchStats {
    /** GET_SELECTION_AS_CLIPBOARD prefetches dispatched so far. */
    prefetches: number;
    /** Whether every prefetch asked the engine to skip the .docx build. */
    allSkippedDocx: boolean;
    /** Copy/cut gestures served synchronously from the cache. */
    hits: number;
    /** Copy/cut gestures that fell back to the async path. */
    misses: number;
    /** Whether a valid entry is warm right now. */
    warm: boolean;
}

export interface ClipboardPrefetch {
    /** The payload for the live selection, or `null` on a miss. Counts the
     *  lookup as a hit or miss. Never awaits. */
    take(): CachedClipboardPayload | null;
    /** Drop the entry and cancel a pending prefetch (e2e hook). */
    invalidate(): void;
    stats(): ClipboardPrefetchStats;
}

const posKey = (p: LogicalPos): string =>
    `${p.path.steps.map((s) => JSON.stringify(s)).join('/')}@${p.offset}`;

const isCollapsed = (a: LogicalPos, b: LogicalPos): boolean => posKey(a) === posKey(b);

/** Selection identity: the range plus the story it is expressed in
 *  (story-mode ranges are story-relative, so the same numbers can name
 *  different text in the body and in a header). */
function selectionKey(ev: Extract<Event, { type: 'SELECTION_CHANGED' }>): string {
    const story = ev.editing_story ? JSON.stringify(ev.editing_story) : 'body';
    return `${story}|${posKey(ev.range.start)}|${posKey(ev.range.end)}`;
}

/**
 * Wire the prefetch cache to `client` + `store`. Must be called inside a
 * Solid owner (component body); the subscription and timer are released on
 * cleanup.
 */
export function createClipboardPrefetch(
    client: EngineClient,
    store: EngineStore,
    enabled = true,
): ClipboardPrefetch {
    /* Bumped only by `invalidate()` (the e2e hook) — drops an in-flight
       prefetch whose epoch / revision / key happen to still match. */
    let dropGen = 0;
    let liveKey: string | null = null;
    /* Issue #260 — the engine document revision last observed. */
    let liveRevision = -1;
    let liveEpoch = client.writeEpoch;
    type Stamp = { dropGen: number; epoch: number; revision: number; key: string };
    let entry: (CachedClipboardPayload & Stamp) | null = null;
    const current = (key: string): Stamp => ({
        dropGen,
        epoch: client.writeEpoch,
        revision: liveRevision,
        key,
    });
    const matches = (s: Stamp): boolean =>
        s.dropGen === dropGen &&
        s.epoch === client.writeEpoch &&
        s.revision === liveRevision &&
        s.key === liveKey;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const counters = { prefetches: 0, allSkippedDocx: true, hits: 0, misses: 0 };

    /* The prefetch gate. A boolean memo (not the raw `selection` signal):
       downstream work re-runs only when the selection flips between
       collapsed and ranged — never once per SELECTION_CHANGED — and the
       prefetch reply feeds no signal at all, so there is no
       dispatch → reply → signal → dispatch loop. */
    const hasRange = createMemo(() => {
        const r = store.selection().range;
        return !isCollapsed(r.start, r.end);
    });

    const cancel = (): void => {
        if (timer !== undefined) clearTimeout(timer);
        timer = undefined;
    };

    const run = (): void => {
        timer = undefined;
        if (!enabled || !hasRange() || liveKey === null) return;
        /* A write still in flight means the engine is ahead of what the
           main thread has seen; its reply will bump `gen` and reschedule. */
        if (client.writesInFlight > 0) {
            schedule();
            return;
        }
        const stamp = current(liveKey);
        const cmd = { type: 'GET_SELECTION_AS_CLIPBOARD', include_docx: false } as const;
        counters.prefetches += 1;
        counters.allSkippedDocx &&= cmd.include_docx === false;
        client
            .dispatch(cmd)
            .then((evt) => {
                if (!matches(stamp) || evt.type !== 'CLIPBOARD_PAYLOAD') return;
                entry = { ...stamp, plain: evt.plain, html: evt.html };
            })
            .catch(() => {
                /* Non-fatal: a failed prefetch only means the next copy
                   takes the async path. */
            });
    };

    function schedule(): void {
        cancel();
        if (!enabled || !hasRange()) return;
        timer = setTimeout(run, PREFETCH_DEBOUNCE_MS);
    }

    const unsubscribe = client.subscribe((ev: Event) => {
        let moved = false;
        if (client.writeEpoch !== liveEpoch) {
            liveEpoch = client.writeEpoch;
            moved = true;
        }
        /* Both revision fields are additive (`#[serde(default)]` → optional
           on the wire); a producer that omits one leaves it unchanged. */
        if (ev.type === 'SELECTION_CHANGED') {
            const key = isCollapsed(ev.range.start, ev.range.end) ? null : selectionKey(ev);
            const revision = ev.document_revision ?? liveRevision;
            if (key !== liveKey || revision !== liveRevision) {
                liveKey = key;
                liveRevision = revision;
                moved = true;
            }
        } else if (ev.type === 'PAINTED') {
            const revision = ev.mutation_seq ?? liveRevision;
            if (revision !== liveRevision) {
                liveRevision = revision;
                moved = true;
            }
        }
        if (!moved) return;
        entry = null;
        schedule();
    });

    /* Also (re)arm when the gate itself flips — covers subscriber ordering
       (the store may update the memo after our callback ran). */
    createEffect(
        on(hasRange, (ranged) => {
            if (ranged) schedule();
            else {
                cancel();
                entry = null;
            }
        }),
    );

    onCleanup(() => {
        cancel();
        unsubscribe();
    });

    return {
        take() {
            const hit =
                enabled && entry !== null && matches(entry) && client.writesInFlight === 0;
            if (hit && entry) {
                counters.hits += 1;
                return { plain: entry.plain, html: entry.html };
            }
            counters.misses += 1;
            return null;
        },
        invalidate() {
            cancel();
            dropGen += 1;
            entry = null;
        },
        stats() {
            return {
                ...counters,
                warm: entry !== null && matches(entry),
            };
        },
    };
}
