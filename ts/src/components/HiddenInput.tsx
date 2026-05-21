/* Phase 4 §6 — hidden textarea: the OS text-input surface.
 *
 * The textarea owns IME composition, keyboard repeat, and OS shortcuts. It is
 * positioned at the caret (so IME popups anchor correctly), kept invisible
 * (opacity 0.01 — not 0; Safari drops events on opacity:0) and click-through
 * (pointer-events:none). `beforeinput` is captured and mapped to engine
 * commands; the textarea itself never accumulates text. */
import { createEffect, onCleanup, onMount } from 'solid-js';
import type { EngineClient } from '../engine/engine-client';
import type { Command, LogicalPos } from '../engine/types';
import type { EngineStore } from '../state/engine-store';

/** Map a non-composition `InputEvent` to an engine command. */
function mapInputEventToCommand(e: InputEvent, caret: LogicalPos): Command | null {
    switch (e.inputType) {
        case 'insertText':
            return e.data ? { type: 'INSERT_TEXT', at: caret, text: e.data } : null;
        /* A <textarea> fires `insertLineBreak` for Enter (and Shift+Enter);
           `insertParagraph` is contenteditable semantics. The document model
           has only paragraphs, so both split. */
        case 'insertParagraph':
        case 'insertLineBreak':
            return { type: 'SPLIT_PARAGRAPH', at: caret };
        case 'deleteContentBackward':
            return { type: 'DELETE_AT_CARET', forward: false, by_word: false };
        case 'deleteContentForward':
            return { type: 'DELETE_AT_CARET', forward: true, by_word: false };
        case 'deleteWordBackward':
            return { type: 'DELETE_AT_CARET', forward: false, by_word: true };
        case 'deleteWordForward':
            return { type: 'DELETE_AT_CARET', forward: true, by_word: true };
        default:
            /* insertFromPaste → clipboard handler (D4.9); others ignored. */
            return null;
    }
}

export function HiddenInput(props: { client: EngineClient; store: EngineStore }) {
    let ref: HTMLTextAreaElement | undefined;

    /* Track the textarea to the caret so OS IME popups anchor at the caret. */
    createEffect(() => {
        const c = props.store.caret();
        if (ref && c) {
            ref.style.left = `${c.x}px`;
            ref.style.top = `${c.y}px`;
            ref.style.height = `${c.h}px`;
        }
    });

    const focus = (): void => ref?.focus();
    onMount(() => {
        focus();
        /* A canvas click blurs focus to <body> via the mousedown default —
           re-grab it on pointerup (which fires after that) and when the
           window regains focus. */
        document.addEventListener('pointerup', focus);
        window.addEventListener('focus', focus);
    });
    onCleanup(() => {
        document.removeEventListener('pointerup', focus);
        window.removeEventListener('focus', focus);
    });

    const onCompositionStart = (): void => {
        const caret = props.store.caretLogical();
        if (caret) void props.client.dispatch({ type: 'BEGIN_COMPOSITION', at: caret });
    };
    const onCompositionUpdate = (e: CompositionEvent): void => {
        void props.client.dispatch({
            type: 'UPDATE_COMPOSITION',
            text: e.data,
            target_range: undefined,
        });
    };
    const onCompositionEnd = (): void => {
        void props.client.dispatch({ type: 'END_COMPOSITION', commit: true });
        if (ref) ref.value = '';
    };

    const onBeforeInput = (e: InputEvent): void => {
        if (e.isComposing) return; /* the composition handlers own this */
        e.preventDefault();
        const caret = props.store.caretLogical();
        if (caret) {
            const cmd = mapInputEventToCommand(e, caret);
            if (cmd) void props.client.dispatch(cmd);
        }
        if (ref) ref.value = '';
    };

    const onKeyDown = (e: KeyboardEvent): void => {
        if (e.isComposing) return;
        const mod = e.ctrlKey || e.metaKey;
        if (!mod) return;
        /* Match e.code (physical key) — e.key is layout-dependent, so an
           Arabic layout reports a non-Latin key at the Z position. */
        if (e.code === 'KeyZ' && !e.shiftKey) {
            e.preventDefault();
            void props.client.dispatch({ type: 'UNDO' });
        } else if (e.code === 'KeyY' || (e.code === 'KeyZ' && e.shiftKey)) {
            e.preventDefault();
            void props.client.dispatch({ type: 'REDO' });
        }
    };

    return (
        <textarea
            ref={ref}
            class="hidden-input"
            aria-hidden="true"
            tabindex="-1"
            autocomplete="off"
            autocapitalize="off"
            spellcheck={false}
            onCompositionStart={onCompositionStart}
            onCompositionUpdate={onCompositionUpdate}
            onCompositionEnd={onCompositionEnd}
            onBeforeInput={onBeforeInput}
            onKeyDown={onKeyDown}
        />
    );
}
