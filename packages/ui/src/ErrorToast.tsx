/**
 * ErrorToast - issue #364: the visible half of an engine refusal.
 *
 * A typed `Event::Error` from a keyboard path (a refused tracked deletion)
 * used to reach telemetry only, so the key press appeared to do nothing -
 * the Honest UX rule needs a visible refusal. `createEditorState().
 * lastError()` moves on every error reply; this component turns the
 * errors it has copy for into a transient, non-modal, screen-reader-
 * announced toast. (Issue #365 made a tracked deletion across table cells
 * or over a table a RECORDED one — rows marked deleted — so the engine
 * now refuses only a range whose end addresses no paragraph.)
 *
 * - `.nge-toast` is a PERSISTENT `role="status"` live region (implicitly
 *   polite - the explicit `aria-live` is left off so the engine's own
 *   `Announcements` region stays the only `[aria-live="polite"]` element a
 *   spec / host can select; a region inserted together with its text is
 *   announced unreliably, one whose text changes is announced), holding
 *   the message card only while a message is showing.
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

/** `ErrorKind` -> the user-facing sentence (what happened + what to do).
 *  Issue #427: every kind either has copy here or is listed in
 *  [`ERROR_KINDS_WITH_OWN_PRESENTATION`]; `tools/parity` fails on a kind
 *  with neither (floor 0) and `ERROR_KIND_PRESENTATION` below makes `tsc`
 *  refuse a kind the engine adds that this file has not classified. */
export const ERROR_TOAST_COPY: Partial<Record<ErrorKind, string>> = {
    TrackedDeletionRefused:
        'This deletion could not be tracked, so nothing was changed — select the text again, or turn off Track Changes.',
    NoSelection: 'Nothing is selected, so nothing was changed — click in the text and try again.',
    NotInParagraph:
        'That spot is no longer in a paragraph, so nothing was changed — click in the text and try again.',
    InStory:
        'That is not available while editing a note, text box, header or footer — click back into the document body first.',
    InTableCell:
        'That is not supported inside a table cell yet — place the cursor outside the table.',
    NoFieldAtCaret: 'There is no field at the cursor — place the cursor inside a field first.',
    NoSuchTarget:
        'The item you were changing no longer exists, so nothing was changed — select it again.',
    NotInHeaderFooter:
        'This only works while editing a header or footer — double-click a header or footer first.',
    UnsupportedHere: 'That is not supported here, so nothing was changed.',
    OutOfRange: 'That value is outside the allowed range, so nothing was changed.',
    EmptyInput: 'That needs some text, but none was given — nothing was changed.',
    NestingTooDeep: 'Text boxes cannot be nested any deeper — nothing was changed.',
    NotReady: 'The editor is still getting ready — wait a moment and try again.',
    Unimplemented: 'That feature is not available yet — nothing was changed.',
    Internal:
        'Something went wrong inside the editor and nothing was changed — try again, and save your work if it persists.',
    InvalidArgument:
        'A value was not a valid number, so nothing was changed — try again, and save your work if it keeps happening.',
};

/** Kinds the toast deliberately does not cover: a surface of their own
 *  shows them (`PackageTooLarge` / `EncryptedDocument` / `WrongPassword` /
 *  `InvalidDocument` by the File menu's open banner; `Protected` by the
 *  engine's own mode-specific sentence, see [`toastMessageFor`]). */
export const ERROR_KINDS_WITH_OWN_PRESENTATION = [
    'PackageTooLarge',
    'EncryptedDocument',
    'WrongPassword',
    'InvalidDocument',
    'Protected',
] as const satisfies readonly ErrorKind[];

/** Exhaustiveness guard: `tsc` fails when `ErrorKind` gains a variant that
 *  is in neither list above (the Rust side is `tools/parity`). */
export const ERROR_KIND_PRESENTATION: Record<ErrorKind, 'toast' | 'own'> = {
    TrackedDeletionRefused: 'toast',
    NoSelection: 'toast',
    NotInParagraph: 'toast',
    InStory: 'toast',
    InTableCell: 'toast',
    NoFieldAtCaret: 'toast',
    NoSuchTarget: 'toast',
    NotInHeaderFooter: 'toast',
    UnsupportedHere: 'toast',
    OutOfRange: 'toast',
    EmptyInput: 'toast',
    NestingTooDeep: 'toast',
    NotReady: 'toast',
    Unimplemented: 'toast',
    Internal: 'toast',
    InvalidArgument: 'toast',
    PackageTooLarge: 'own',
    EncryptedDocument: 'own',
    WrongPassword: 'own',
    InvalidDocument: 'own',
    Protected: 'own',
};

export interface ErrorToastProps {
    /** Auto-dismiss delay in ms (default 4000). */
    durationMs?: number;
    /** Extra / overriding copy per error kind. */
    copy?: Partial<Record<ErrorKind, string>>;
}

/** The toast text for an error, or `undefined` when it is not toasted.
 *  Issue #345 — a `Protected` refusal (the open document's enforced
 *  `w:documentProtection`) without host copy shows the engine's own,
 *  mode-specific explanation ("This document is protected (filling in
 *  forms): only form fields …"), its `<Command>: ` prefix dropped. */
export function toastMessageFor(
    error: EditorError | undefined,
    copy: Partial<Record<ErrorKind, string>> = ERROR_TOAST_COPY,
): string | undefined {
    if (!error || error.kind === undefined) return undefined;
    const text = copy[error.kind];
    if (text === undefined && error.kind === 'Protected') {
        return error.message.replace(/^[A-Za-z][A-Za-z0-9]*: /, '');
    }
    return text;
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
        <div class="nge-toast" role="status" aria-atomic="true">
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
