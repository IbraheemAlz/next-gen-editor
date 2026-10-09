/**
 * CommentsRail — callout column listing every `<w:comment>` the
 * engine has parsed from the loaded `.docx`.
 *
 * Consumes `engine.commentsSnapshot()`. Same lifecycle pattern as
 * TrackChangesSidebar: initial fetch + refresh on document events.
 *
 * Resolve / Delete / Reply are all wired end-to-end. Resolve and
 * Delete dispatch `Command::ResolveComment` / `Command::DeleteComment`
 * (delete cascades over the whole reply thread engine-side). Reply
 * (issue #27, closed by this change) dispatches
 * `Command::ReplyToComment` — the engine anchors the reply on the
 * parent's range, mints the id, and the thread round-trips through
 * `word/commentsExtended.xml` `<w15:commentEx w15:paraIdParent>`.
 * The rail groups snapshot rows into threads on `parent_id`
 * (top-level = `parent_id` undefined/null) and renders replies
 * indented inside the parent card, ordered by id.
 *
 * Issue #254 — threads are listed in DOCUMENT order of their anchors
 * (full `start_path`s: a comment inside a table cell sorts at its cell,
 * not under the cell paragraph's index read as a top-level block), and a
 * card's location button selects the commented text through the same
 * path (`SET_SELECTION`), then asks the host to reveal it
 * (`onNavigate`). The rail refetches on every document mutation
 * (`PAINTED.mutation_seq`), so typing before an anchor keeps the listed
 * position on the same text.
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
    type BlockPath,
    type CommentSnapshot,
    type LogicalPos,
    type LogicalRange,
    type PathStep,
} from '@nge/core';
import './CommentsRail.css';

export interface CommentsRailProps {
    title?: string;
    /** Author stamped on replies composed in the rail. Mirrors
     *  `ReviewControlsProps.defaultAuthor`; falls back to the
     *  engine's own `"You"` default. */
    defaultAuthor?: string;
    /** Issue #254 — called once a card's location button has moved the
     *  editor selection onto the comment's anchored text; the host
     *  scrolls it into view (the rail does not own the viewport). */
    onNavigate?: (comment: CommentSnapshot) => void;
}

/** One top-level comment plus its replies (ordered by id). */
interface CommentThread {
    root: CommentSnapshot;
    replies: CommentSnapshot[];
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

/** Issue #254 — the anchored range of a row: its full paths when the
 *  engine reports them, else the flat index read as a top-level block
 *  (implementations that pre-date the paths). */
function anchorOf(c: CommentSnapshot): LogicalRange {
    const pos = (path: BlockPath | undefined, block: number, offset: number): LogicalPos => ({
        path: path ?? { steps: [{ kind: 'BLOCK', idx: block }] },
        offset,
    });
    return {
        start: pos(c.start_path, c.start_block, c.start_offset),
        end: pos(c.end_path, c.end_block, c.end_offset),
    };
}

function stepOrder(s: PathStep): [number, number] {
    return s.kind === 'CELL' ? [s.row, s.col] : [s.idx, 0];
}

/** Document order of two positions: the paths depth-first (a block
 *  index, then a cell's row and column), then the byte offset. */
function comparePos(a: LogicalPos, b: LogicalPos): number {
    const n = Math.min(a.path.steps.length, b.path.steps.length);
    for (let i = 0; i < n; i++) {
        const [x0, x1] = stepOrder(a.path.steps[i]!);
        const [y0, y1] = stepOrder(b.path.steps[i]!);
        if (x0 !== y0) return x0 - y0;
        if (x1 !== y1) return x1 - y1;
    }
    return a.path.steps.length - b.path.steps.length || a.offset - b.offset;
}

/** Human-readable anchor position: `block 2 › cell 1,0 › block 0:6`. */
function describePos(p: LogicalPos): string {
    const steps = p.path.steps.map((s) =>
        s.kind === 'CELL' ? `cell ${s.row},${s.col}` : `block ${s.idx}`,
    );
    return `${steps.join(' › ')}:${p.offset}`;
}

/** Group flat snapshot rows into threads. Top-level rows key their
 *  own thread; replies attach to their thread root (walking
 *  `parent_id` transitively so replies-to-replies land in the same
 *  flat list, Word-style). A reply whose parent is missing from the
 *  snapshot is promoted to a top-level card rather than dropped. */
function groupThreads(rows: CommentSnapshot[]): CommentThread[] {
    const byId = new Map<number, CommentSnapshot>(rows.map((c) => [c.id, c]));
    const rootOf = (c: CommentSnapshot): CommentSnapshot => {
        let cur = c;
        const seen = new Set<number>([c.id]);
        while (cur.parent_id != null) {
            const parent = byId.get(cur.parent_id);
            if (!parent || seen.has(parent.id)) break;
            seen.add(parent.id);
            cur = parent;
        }
        return cur;
    };
    const threads = new Map<number, CommentThread>();
    for (const c of rows) {
        if (c.parent_id == null) threads.set(c.id, { root: c, replies: [] });
    }
    for (const c of rows) {
        if (c.parent_id == null) continue;
        const root = rootOf(c);
        const t = threads.get(root.id);
        if (t) t.replies.push(c);
        else threads.set(c.id, { root: c, replies: [] });
    }
    for (const t of threads.values()) t.replies.sort((a, b) => a.id - b.id);
    /* Issue #254 — document order of the thread anchors. */
    return [...threads.values()].sort(
        (a, b) =>
            comparePos(anchorOf(a.root).start, anchorOf(b.root).start) ||
            a.root.id - b.root.id,
    );
}

export const CommentsRail: Component<CommentsRailProps> = (props) => {
    const engine = useEngine();
    const cmd = createEditorCommands();
    const [comments, setComments] = createSignal<CommentSnapshot[]>([]);
    const [error, setError] = createSignal<string | null>(null);
    /* Issue #27 — one reply draft open at a time, keyed by the
     * top-level comment id it replies to. */
    const [replyFor, setReplyFor] = createSignal<number | null>(null);
    const [replyText, setReplyText] = createSignal('');
    /* Issue #254 — the thread whose anchor the selection was moved to. */
    const [activeId, setActiveId] = createSignal<number | null>(null);

    const threads = () => groupThreads(comments());

    const remove = async (id: number) => {
        await cmd.deleteComment(id);
        await refresh();
    };
    const toggleResolved = async (c: CommentSnapshot) => {
        await cmd.resolveComment(c.id, !c.resolved);
        await refresh();
    };
    /* Issue #254 — select the commented text through its full anchor
     * path (a cell comment lands in its cell), then let the host reveal
     * it. Focus stays in the rail: typing must not replace the text. */
    const goTo = async (c: CommentSnapshot) => {
        const range = anchorOf(c);
        await cmd.setSelection(range, range.end);
        setActiveId(c.id);
        props.onNavigate?.(c);
    };
    const toggleReplyDraft = (id: number) => {
        setReplyText('');
        setReplyFor((cur) => (cur === id ? null : id));
    };

    /* Focus the reply textarea only on an actual open (issue #35
     * family). A creation-time ref would also re-fire when refresh()
     * rebuilds the referentially-keyed thread list and remounts an
     * open draft — yanking focus from wherever the user put it. The
     * effect runs post-render, so the textarea ref is connected. */
    let replyTextareaEl: HTMLTextAreaElement | undefined;
    createEffect(() => {
        if (replyFor() !== null) replyTextareaEl?.focus();
    });
    const submitReply = async (parentId: number) => {
        const text = replyText().trim();
        if (text === '') return;
        await cmd.replyToComment(parentId, text, props.defaultAuthor ?? 'You');
        setReplyText('');
        setReplyFor(null);
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

    /* Issue #254 / #266 — refetch on every document mutation, not only on
       load: a comment added from the toolbar must appear, and typing
       before an anchor moves its listed offset. `PAINTED.mutation_seq`
       moves exactly when the document changed (never on caret moves or
       view commands); a plain closure variable, read only in the
       callback, so nothing reactive re-subscribes. */
    let lastMutationSeq: number | null = null;
    createEffect(() => {
        const unsub = engine.subscribe((evt) => {
            if (evt.type === 'DOCUMENT_LOADED' || evt.type === 'RECOVERED') {
                setActiveId(null);
                void refresh();
            } else if (evt.type === 'PAINTED') {
                const seq = evt.mutation_seq ?? null;
                if (lastMutationSeq !== null && seq !== lastMutationSeq) {
                    void refresh();
                }
                lastMutationSeq = seq;
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
                when={threads().length > 0}
                fallback={
                    <div class="nge-cm__empty">No comments in this document.</div>
                }
            >
                <ul class="nge-cm__list">
                    <For each={threads()}>
                        {(t) => (
                            <li
                                class={`nge-cm__card ${t.root.resolved ? 'nge-cm__card--resolved' : ''} ${activeId() === t.root.id ? 'nge-cm__card--active' : ''}`}
                                data-comment-id={t.root.id}
                                data-anchor={describePos(anchorOf(t.root).start)}
                            >
                                <div class="nge-cm__card-head">
                                    <div
                                        class="nge-cm__avatar"
                                        aria-hidden="true"
                                        title={t.root.author || 'Anonymous'}
                                    >
                                        {authorInitials(t.root.author)}
                                    </div>
                                    <div class="nge-cm__byline">
                                        <span class="nge-cm__author">
                                            {t.root.author || 'Anonymous'}
                                        </span>
                                        <time class="nge-cm__date">{fmtDate(t.root.date)}</time>
                                    </div>
                                </div>
                                <p class="nge-cm__text">{t.root.text}</p>
                                <div class="nge-cm__loc">
                                    <button
                                        class="nge-cm__goto"
                                        type="button"
                                        aria-label="Go to comment"
                                        title="Select the commented text"
                                        onClick={() => void goTo(t.root)}
                                    >
                                        {describePos(anchorOf(t.root).start)}
                                        {' → '}
                                        {describePos(anchorOf(t.root).end)}
                                    </button>
                                </div>
                                <Show when={t.replies.length > 0}>
                                    <ul class="nge-cm__replies">
                                        <For each={t.replies}>
                                            {(r) => (
                                                <li
                                                    class="nge-cm__reply"
                                                    data-comment-id={r.id}
                                                >
                                                    <div class="nge-cm__card-head">
                                                        <div
                                                            class="nge-cm__avatar nge-cm__avatar--reply"
                                                            aria-hidden="true"
                                                            title={r.author || 'Anonymous'}
                                                        >
                                                            {authorInitials(r.author)}
                                                        </div>
                                                        <div class="nge-cm__byline">
                                                            <span class="nge-cm__author">
                                                                {r.author || 'Anonymous'}
                                                            </span>
                                                            <time class="nge-cm__date">
                                                                {fmtDate(r.date)}
                                                            </time>
                                                        </div>
                                                    </div>
                                                    <p class="nge-cm__text">{r.text}</p>
                                                </li>
                                            )}
                                        </For>
                                    </ul>
                                </Show>
                                <div class="nge-cm__actions">
                                    <button
                                        class="nge-btn nge-cm__action"
                                        type="button"
                                        aria-label="Reply to comment"
                                        aria-expanded={replyFor() === t.root.id}
                                        title="Reply"
                                        onClick={() => toggleReplyDraft(t.root.id)}
                                    >
                                        ↩ Reply
                                    </button>
                                    <button
                                        class="nge-btn nge-cm__action"
                                        type="button"
                                        aria-label={t.root.resolved ? 'Reopen comment' : 'Resolve comment'}
                                        title={t.root.resolved ? 'Reopen' : 'Resolve'}
                                        onClick={() => void toggleResolved(t.root)}
                                    >
                                        {t.root.resolved ? '↺ Reopen' : '✓ Resolve'}
                                    </button>
                                    <button
                                        class="nge-btn nge-cm__action nge-cm__action--danger"
                                        type="button"
                                        aria-label="Delete comment"
                                        title="Delete comment (removes its replies too)"
                                        onClick={() => void remove(t.root.id)}
                                    >
                                        🗑 Delete
                                    </button>
                                </div>
                                <Show when={replyFor() === t.root.id}>
                                    <div
                                        class="nge-cm__reply-draft"
                                        role="form"
                                        aria-label="Reply to comment"
                                    >
                                        <textarea
                                            ref={(el) => (replyTextareaEl = el)}
                                            class="nge-cm__reply-textarea"
                                            placeholder="Reply…"
                                            rows={2}
                                            value={replyText()}
                                            onInput={(e) => setReplyText(e.currentTarget.value)}
                                        />
                                        <div class="nge-cm__reply-draft-actions">
                                            <button
                                                class="nge-btn nge-cm__action"
                                                type="button"
                                                onClick={() => toggleReplyDraft(t.root.id)}
                                            >
                                                Cancel
                                            </button>
                                            <button
                                                class="nge-btn nge-btn--primary nge-cm__action"
                                                type="button"
                                                disabled={replyText().trim() === ''}
                                                onClick={() => void submitReply(t.root.id)}
                                            >
                                                Reply
                                            </button>
                                        </div>
                                    </div>
                                </Show>
                            </li>
                        )}
                    </For>
                </ul>
            </Show>
        </aside>
    );
};
