/**
 * ReviewControls — toolbar shelf for review workflow.
 *
 * Sprint 14 (#14) — Track Changes toggle wired through real
 * `Command::ToggleTrackChanges`. The toggle's "active" state binds
 * to `state.isTrackingChanges()` (broadcast on every
 * `SELECTION_CHANGED`) — NOT a local Solid signal — so a tracked-
 * changes toggle issued by macro, undo, or another tab stays in
 * sync. New Comment + Accept/Reject All controls keep their Sprint 7
 * behaviour.
 *
 * The author input also feeds `Command::SetReviewIdentity`, so
 * tracked revisions carry the typed author instead of the engine's
 * "You" default.
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
import { focusEditorInput } from './focus';
import './ReviewControls.css';

export interface ReviewControlsProps {
    /** Default author for new comments. */
    defaultAuthor?: string;
}

export const ReviewControls: Component<ReviewControlsProps> = (props) => {
    const cmd = createEditorCommands();
    const state = createEditorState();
    const engine = useEngine();
    /* Sprint 14 (#14) — track state comes from the engine via
     * Event::SelectionChanged.is_tracking_changes, NOT a local
     * Solid signal. */
    const tracking = () => state.isTrackingChanges();
    const [error, setError] = createSignal<string | null>(null);
    const [commentDraftOpen, setCommentDraftOpen] = createSignal(false);
    const [commentText, setCommentText] = createSignal('');
    const [commentAuthor, setCommentAuthor] = createSignal(
        props.defaultAuthor ?? 'You',
    );

    const ready = () => state.selection() !== undefined;

    /* Propagate the author input to the engine review identity so
     * tracked revisions stop being stamped with the "You" default.
     * Empty date keeps the engine's per-mutation date stamping.
     *
     * Issue #55 — gated on `ready()` (the engine has emitted its first
     * SELECTION_CHANGED). This effect used to dispatch unconditionally on
     * mount — ReviewControls sits in the always-mounted toolbar row, so it
     * fired before `Command::Init` resolved, rejecting with "engine not
     * initialized"; void-discarded, that surfaced as an unhandled promise
     * rejection on every single boot. `ready()` tracks `state.selection()`,
     * which only becomes defined post-INIT, so the effect naturally
     * re-fires once the engine comes up. The `.catch` is a backstop for a
     * later, genuinely failed identity push (e.g. mid-flight worker
     * respawn) — never let this dispatch go unobserved again. */
    createEffect(() => {
        if (!ready()) return;
        void cmd.setReviewIdentity(commentAuthor(), '').catch((e: unknown) => {
            console.error('setReviewIdentity failed', e);
        });
    });

    /* Sprint 14 (#14) — dispatch + let the engine's
     * SelectionChanged reply drive the visual active state.
     * `tracking()` reads `state.isTrackingChanges()` so the toggle
     * lights up only when the engine confirms recording is on. */
    const onToggleTracking = async () => {
        const next = !tracking();
        try {
            if (next) {
                /* Guarantee the identity lands before recording starts —
                 * the worker queue serializes it ahead of the toggle. */
                await cmd.setReviewIdentity(commentAuthor(), '');
            }
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

    /* Draft focus lifecycle (issue #35). Opening the draft moves focus
       into its textarea so typing lands in the comment, never the
       document; closing (cancel, Escape, or submit) hands focus back to
       the editor so typing resumes in the document. */
    let draftTextarea: HTMLTextAreaElement | undefined;
    createEffect(() => {
        if (commentDraftOpen()) draftTextarea?.focus();
    });
    const closeDraft = () => {
        setCommentDraftOpen(false);
        setCommentText('');
        focusEditorInput();
    };

    const submitComment = async () => {
        if (!ready() || commentText().trim() === '') return;
        await cmd.insertComment(commentText().trim(), commentAuthor());
        setCommentText('');
        setCommentDraftOpen(false);
        focusEditorInput();
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
                title={tracking() ? 'Track Changes — recording' : 'Track Changes — off'}
                onClick={() => void onToggleTracking()}
            >
                <span aria-hidden="true">⌖</span>
                <span>Track</span>
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
                <div
                    class="nge-review__draft"
                    role="dialog"
                    aria-label="New comment"
                    onKeyDown={(e) => {
                        if (e.key === 'Escape') closeDraft();
                    }}
                >
                    <input
                        class="nge-review__input"
                        type="text"
                        placeholder="Author"
                        value={commentAuthor()}
                        onInput={(e) => setCommentAuthor(e.currentTarget.value)}
                    />
                    <textarea
                        ref={draftTextarea}
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
                            onClick={closeDraft}
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
