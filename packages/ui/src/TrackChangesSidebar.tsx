/**
 * TrackChangesSidebar — read-only review panel listing every
 * tracked-change revision the engine has parsed from the loaded `.docx`.
 *
 * Consumes `engine.revisionsSnapshot()` (an optional capability on
 * EngineHandle). Re-fetches on every DOCUMENT_LOADED event so the list
 * mirrors the active document.
 *
 * Validates the entire ins/del/rPrChange parser path — before this,
 * the only coverage was the `.docx` round-trip byte-diff harness.
 *
 * Accept/Reject controls are intentionally absent: the engine does not
 * yet expose mutation commands for revisions. Once it does, those
 * controls land here without touching the layout.
 */
import {
    createEffect,
    createSignal,
    For,
    Show,
    onCleanup,
    type Component,
} from 'solid-js';
import {
    createEditorCommands,
    useEngine,
    type RevisionSnapshot,
} from '@nge/core';
import './TrackChangesSidebar.css';

export interface TrackChangesSidebarProps {
    /** Optional title override. */
    title?: string;
}

function fmtDate(iso: string): string {
    if (!iso) return '–';
    const d = new Date(iso);
    if (Number.isNaN(d.getTime())) return iso;
    return d.toLocaleString(undefined, {
        dateStyle: 'medium',
        timeStyle: 'short',
    });
}

export const TrackChangesSidebar: Component<TrackChangesSidebarProps> = (props) => {
    const engine = useEngine();
    const cmd = createEditorCommands();
    const [revisions, setRevisions] = createSignal<RevisionSnapshot[]>([]);
    const [error, setError] = createSignal<string | null>(null);

    const accept = async (rev: RevisionSnapshot) => {
        await cmd.acceptRevision(rev.block, rev.start, rev.end);
        await refresh();
    };
    const reject = async (rev: RevisionSnapshot) => {
        await cmd.rejectRevision(rev.block, rev.start, rev.end);
        await refresh();
    };

    const refresh = async () => {
        if (!engine.revisionsSnapshot) {
            setError('Engine does not expose revisionsSnapshot');
            return;
        }
        try {
            const rows = await engine.revisionsSnapshot();
            setRevisions(rows);
            setError(null);
        } catch (e) {
            setError(String(e));
        }
    };

    /* Initial load + refresh on document mutations. */
    void refresh();

    createEffect(() => {
        const unsub = engine.subscribe((evt) => {
            if (
                evt.type === 'DOCUMENT_LOADED' ||
                evt.type === 'RECOVERED' ||
                evt.type === 'FORMATTING_CHANGED'
            ) {
                void refresh();
            }
        });
        onCleanup(unsub);
    });

    return (
        <aside class="nge-tc" aria-label="Track changes">
            <header class="nge-tc__head">
                <h3 class="nge-tc__title">{props.title ?? 'Track Changes'}</h3>
                <span class="nge-tc__count">{revisions().length}</span>
                <button
                    class="nge-btn nge-btn--icon nge-tc__refresh"
                    type="button"
                    aria-label="Refresh"
                    title="Refresh"
                    onClick={() => void refresh()}
                >
                    ⟳
                </button>
            </header>
            <Show when={error()}>
                <div class="nge-tc__error" role="alert">{error()}</div>
            </Show>
            <Show
                when={revisions().length > 0}
                fallback={
                    <div class="nge-tc__empty">No tracked changes in this document.</div>
                }
            >
                <ul class="nge-tc__list">
                    <For each={revisions()}>
                        {(rev) => (
                            <li
                                class={`nge-tc__row nge-tc__row--${rev.kind}`}
                                data-block={rev.block}
                            >
                                <div class="nge-tc__row-head">
                                    <span class={`nge-tc__kind nge-tc__kind--${rev.kind}`}>
                                        {rev.kind === 'insert' ? 'Inserted' : 'Deleted'}
                                    </span>
                                    <span class="nge-tc__author">{rev.author || 'Anonymous'}</span>
                                </div>
                                <div class="nge-tc__meta">
                                    <span>{fmtDate(rev.date)}</span>
                                    <span class="nge-tc__loc">
                                        block {rev.block} · {rev.start}–{rev.end}
                                    </span>
                                </div>
                                <div class="nge-tc__actions">
                                    <button
                                        class="nge-btn nge-tc__action nge-tc__action--accept"
                                        type="button"
                                        aria-label="Accept revision"
                                        title="Accept revision"
                                        onClick={() => void accept(rev)}
                                    >
                                        ✓ Accept
                                    </button>
                                    <button
                                        class="nge-btn nge-tc__action nge-tc__action--reject"
                                        type="button"
                                        aria-label="Reject revision"
                                        title="Reject revision"
                                        onClick={() => void reject(rev)}
                                    >
                                        ✗ Reject
                                    </button>
                                </div>
                            </li>
                        )}
                    </For>
                </ul>
            </Show>
        </aside>
    );
};
