/**
 * ReviewControls — toolbar shelf for review workflow.
 *
 * Three controls:
 *   1. Track Changes toggle — dispatches `Command::ToggleTrackChanges`
 *      but the engine currently returns `Event::Error` (recording
 *      infrastructure not yet implemented — see Core Engine backlog).
 *      Rendered with an "Engine pending" badge so QA does not assume
 *      edits are being recorded.
 *   2. New Comment button — opens an inline editor at the bottom of
 *      the surface that collects text + author, then dispatches
 *      `Command::InsertComment` against the current selection range.
 *      Hidden behind a disabled state when no range is selected.
 *   3. Accept All / Reject All — convenience triggers that walk the
 *      latest `revisionsSnapshot` and accept (or reject) each entry
 *      in document order. Wired since `AcceptRevision` /
 *      `RejectRevision` are live.
 */
import {
    createEffect,
    createSignal,
    onCleanup,
    Show,
    type Component,
} from 'solid-js';
import {
    createEditorCommands,
    createEditorState,
    useEngine,
} from '@nge/core';
import './ReviewControls.css';

export interface ReviewControlsProps {
    /** Default author for new comments. */
    defaultAuthor?: string;
}

export const ReviewControls: Component<ReviewControlsProps> = (props) => {
    const cmd = createEditorCommands();
    const state = createEditorState();
    const engine = useEngine();
    const [tracking, setTracking] = createSignal(false);
    const [error, setError] = createSignal<string | null>(null);
    const [commentDraftOpen, setCommentDraftOpen] = createSignal(false);
    const [commentText, setCommentText] = createSignal('');
    const [commentAuthor, setCommentAuthor] = createSignal(
        props.defaultAuthor ?? 'You',
    );

    const ready = () => state.selection() !== undefined;

    /* The track-changes toggle stays a click-and-flag toggle even
     * though the engine returns Error today — flipping the UI state
     * is helpful for the QA flow that wants to "pretend tracking is
     * on and watch this break". The error surfaces in the inline
     * banner below the controls. */
    const onToggleTracking = async () => {
        const next = !tracking();
        setTracking(next);
        try {
            await cmd.toggleTrackChanges(next);
        } catch (e) {
            setError(String(e));
            setTimeout(() => setError(null), 6000);
        }
    };

    /* Surface engine ERROR events tied to review actions in the
     * inline banner (separate from the FileMenu's global toast). */
    createEffect(() => {
        const unsub = engine.subscribe((evt) => {
            if (
                evt.type === 'ERROR' &&
                (evt.message.includes('ToggleTrackChanges') ||
                    evt.message.includes('AcceptRevision') ||
                    evt.message.includes('RejectRevision') ||
                    evt.message.includes('Comment'))
            ) {
                setError(evt.message);
                setTimeout(() => setError(null), 6000);
            }
        });
        onCleanup(unsub);
    });

    const submitComment = async () => {
        if (!ready() || commentText().trim() === '') return;
        await cmd.insertComment(commentText().trim(), commentAuthor());
        setCommentText('');
        setCommentDraftOpen(false);
    };

    const acceptAll = async () => {
        if (!engine.revisionsSnapshot) return;
        const rows = await engine.revisionsSnapshot();
        /* Walk in reverse document order so earlier-row mutations
         * don't shift later rows' byte offsets. */
        for (let i = rows.length - 1; i >= 0; i--) {
            const r = rows[i]!;
            await cmd.acceptRevision(r.block, r.start, r.end);
        }
    };

    const rejectAll = async () => {
        if (!engine.revisionsSnapshot) return;
        const rows = await engine.revisionsSnapshot();
        for (let i = rows.length - 1; i >= 0; i--) {
            const r = rows[i]!;
            await cmd.rejectRevision(r.block, r.start, r.end);
        }
    };

    return (
        <div class="nge-review" role="group" aria-label="Review">
            <button
                class="nge-btn nge-review__btn nge-review__btn--track"
                type="button"
                aria-label="Track changes"
                aria-pressed={tracking()}
                data-active={tracking()}
                title="Track Changes — recording engine path pending (see backlog)"
                onClick={() => void onToggleTracking()}
            >
                <span aria-hidden="true">⌖</span>
                <span>Track</span>
                <span class="nge-review__badge">Engine pending</span>
            </button>

            <button
                class="nge-btn nge-review__btn"
                type="button"
                aria-label="New comment"
                title="New comment on selected range"
                disabled={!ready()}
                onClick={() => setCommentDraftOpen((v) => !v)}
            >
                <span aria-hidden="true">💬</span>
                <span>New comment</span>
            </button>

            <button
                class="nge-btn nge-review__btn"
                type="button"
                aria-label="Accept all revisions"
                title="Accept all tracked changes"
                onClick={() => void acceptAll()}
            >
                <span aria-hidden="true">✓✓</span>
                <span>Accept all</span>
            </button>

            <button
                class="nge-btn nge-review__btn"
                type="button"
                aria-label="Reject all revisions"
                title="Reject all tracked changes"
                onClick={() => void rejectAll()}
            >
                <span aria-hidden="true">✗✗</span>
                <span>Reject all</span>
            </button>

            <Show when={commentDraftOpen()}>
                <div class="nge-review__draft" role="dialog" aria-label="New comment">
                    <input
                        class="nge-review__input"
                        type="text"
                        placeholder="Author"
                        value={commentAuthor()}
                        onInput={(e) => setCommentAuthor(e.currentTarget.value)}
                    />
                    <textarea
                        class="nge-review__textarea"
                        placeholder="Comment text…"
                        value={commentText()}
                        rows={3}
                        onInput={(e) => setCommentText(e.currentTarget.value)}
                    />
                    <div class="nge-review__draft-actions">
                        <button
                            class="nge-btn"
                            type="button"
                            onClick={() => {
                                setCommentDraftOpen(false);
                                setCommentText('');
                            }}
                        >
                            Cancel
                        </button>
                        <button
                            class="nge-btn nge-btn--primary"
                            type="button"
                            disabled={commentText().trim() === ''}
                            onClick={() => void submitComment()}
                        >
                            Comment
                        </button>
                    </div>
                </div>
            </Show>

            <Show when={error()}>
                <div class="nge-review__error" role="alert">{error()}</div>
            </Show>
        </div>
    );
};
