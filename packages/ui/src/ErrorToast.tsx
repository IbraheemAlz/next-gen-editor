/**
 * ErrorToast - issue #364: the visible half of an engine refusal.
 *
 * A typed `Event::Error` from a keyboard path (a tracked Backspace over a
 * table boundary, a tracked delete across a cell) used to reach telemetry
 * only, so the key press appeared to do nothing - the Honest UX rule needs
 * a visible refusal. `createEditorState().lastError()` moves on every
 * error reply; this component turns the errors it has copy for into a
 * transient, non-modal, screen-reader-announced toast.
 *
 * - `.nge-toast` is a PERSISTENT `role="status"` live region (a region
 *   inserted together with its text is announced unreliably, one whose
 *   text changes is announced), holding the message card only while a
 *   message is showing.
 * - Auto-dismisses after `durationMs` (4 s); a newer error replaces the
 *   message and restarts the timer; Escape-free, never steals focus.
 * - Copy comes from a table keyed by `ErrorKind` (`ERROR_TOAST_COPY`); a
 *   host can extend or override it with the `copy` prop. Errors without a
 *   kind, or whose kind has no copy, are not toasted: they are internal
 *   probes or have their own presentation (`PackageTooLarge` is shown by
 *   the File menu). Every error still lands in the Dev HUD's last-error
 *   row.
 */
import { createEffect, createSignal, onCleanup, Show, type Component } from 'solid-js';
import { createEditorState, type EditorError, type ErrorKind } from '@nge/core';
import './ErrorToast.css';

/** `ErrorKind` -> the user-facing sentence (what happened + what to do). */
export const ERROR_TOAST_COPY: Partial<Record<ErrorKind, string>> = {
    TrackedDeletionRefused:
        'Tracked deletion cannot cross a table cell — turn off Track Changes or delete inside the cell.',
};

export interface ErrorToastProps {
    /** Auto-dismiss delay in ms (default 4000). */
    durationMs?: number;
    /** Extra / overriding copy per error kind. */
    copy?: Partial<Record<ErrorKind, string>>;
}

/** The toast text for an error, or `undefined` when it is not toasted. */
export function toastMessageFor(
    error: EditorError | undefined,
    copy: Partial<Record<ErrorKind, string>> = ERROR_TOAST_COPY,
): string | undefined {
    if (!error || error.kind === undefined) return undefined;
    return copy[error.kind];
}

export const ErrorToast: Component<ErrorToastProps> = (props) => {
    const state = createEditorState();
    const [message, setMessage] = createSignal<string | undefined>(undefined);
    let timer: ReturnType<typeof setTimeout> | undefined;

    createEffect(() => {
        const text = toastMessageFor(state.lastError(), { ...ERROR_TOAST_COPY, ...props.copy });
        if (text === undefined) return;
        setMessage(text);
        if (timer !== undefined) clearTimeout(timer);
        timer = setTimeout(() => {
            timer = undefined;
            setMessage(undefined);
        }, props.durationMs ?? 4000);
    });
    onCleanup(() => {
        if (timer !== undefined) clearTimeout(timer);
    });

    return (
        <div class="nge-toast" role="status" aria-live="polite" aria-atomic="true">
            <Show when={message()}>
                {(text) => (
                    <div class="nge-toast__message" data-nge-toast="error">
                        <span class="nge-toast__icon" aria-hidden="true">
                            !
                        </span>
                        <span class="nge-toast__text">{text()}</span>
                    </div>
                )}
            </Show>
        </div>
    );
};
