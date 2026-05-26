/**
 * CommentsRail — read-only callout column listing every `<w:comment>`
 * the engine has parsed from the loaded `.docx`.
 *
 * Consumes `engine.commentsSnapshot()`. Same lifecycle pattern as
 * TrackChangesSidebar: initial fetch + refresh on document events.
 *
 * Reply / Resolve / Delete buttons are intentionally absent — those
 * commands do not yet exist on the bridge. Once added, they land
 * here without rework.
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
    type CommentSnapshot,
} from '@nge/core';
import './CommentsRail.css';

export interface CommentsRailProps {
    title?: string;
}

function fmtDate(iso: string): string {
    if (!iso) return '–';
    const d = new Date(iso);
    if (Number.isNaN(d.getTime())) return iso;
    return d.toLocaleDateString(undefined, {
        month: 'short',
        day: 'numeric',
        year: 'numeric',
    });
}

function authorInitials(author: string): string {
    if (!author) return '?';
    const parts = author.trim().split(/\s+/).slice(0, 2);
    return parts.map((p) => p[0]?.toUpperCase() ?? '').join('') || '?';
}

export const CommentsRail: Component<CommentsRailProps> = (props) => {
    const engine = useEngine();
    const cmd = createEditorCommands();
    const [comments, setComments] = createSignal<CommentSnapshot[]>([]);
    const [error, setError] = createSignal<string | null>(null);

    const remove = async (id: number) => {
        await cmd.deleteComment(id);
        await refresh();
    };
    const toggleResolved = async (c: CommentSnapshot) => {
        await cmd.resolveComment(c.id, !c.resolved);
        await refresh();
    };

    const refresh = async () => {
        if (!engine.commentsSnapshot) {
            setError('Engine does not expose commentsSnapshot');
            return;
        }
        try {
            const rows = await engine.commentsSnapshot();
            setComments(rows);
            setError(null);
        } catch (e) {
            setError(String(e));
        }
    };

    void refresh();

    createEffect(() => {
        const unsub = engine.subscribe((evt) => {
            if (evt.type === 'DOCUMENT_LOADED' || evt.type === 'RECOVERED') {
                void refresh();
            }
        });
        onCleanup(unsub);
    });

    return (
        <aside class="nge-cm" aria-label="Comments">
            <header class="nge-cm__head">
                <h3 class="nge-cm__title">{props.title ?? 'Comments'}</h3>
                <span class="nge-cm__count">{comments().length}</span>
            </header>
            <Show when={error()}>
                <div class="nge-cm__error" role="alert">{error()}</div>
            </Show>
            <Show
                when={comments().length > 0}
                fallback={
                    <div class="nge-cm__empty">No comments in this document.</div>
                }
            >
                <ul class="nge-cm__list">
                    <For each={comments()}>
                        {(c) => (
                            <li
                                class={`nge-cm__card ${c.resolved ? 'nge-cm__card--resolved' : ''}`}
                                data-comment-id={c.id}
                            >
                                <div class="nge-cm__card-head">
                                    <div
                                        class="nge-cm__avatar"
                                        aria-hidden="true"
                                        title={c.author || 'Anonymous'}
                                    >
                                        {authorInitials(c.author)}
                                    </div>
                                    <div class="nge-cm__byline">
                                        <span class="nge-cm__author">
                                            {c.author || 'Anonymous'}
                                        </span>
                                        <time class="nge-cm__date">{fmtDate(c.date)}</time>
                                    </div>
                                </div>
                                <p class="nge-cm__text">{c.text}</p>
                                <div class="nge-cm__loc">
                                    <span>
                                        block {c.start_block}:{c.start_offset}
                                        {' → '}
                                        block {c.end_block}:{c.end_offset}
                                    </span>
                                </div>
                                <div class="nge-cm__actions">
                                    <button
                                        class="nge-btn nge-cm__action"
                                        type="button"
                                        aria-label={c.resolved ? 'Reopen comment' : 'Resolve comment'}
                                        title={c.resolved ? 'Reopen' : 'Resolve'}
                                        onClick={() => void toggleResolved(c)}
                                    >
                                        {c.resolved ? '↺ Reopen' : '✓ Resolve'}
                                    </button>
                                    <button
                                        class="nge-btn nge-cm__action nge-cm__action--danger"
                                        type="button"
                                        aria-label="Delete comment"
                                        title="Delete comment"
                                        onClick={() => void remove(c.id)}
                                    >
                                        🗑 Delete
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
