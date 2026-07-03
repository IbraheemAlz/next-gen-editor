/* Phase 4 §6 — hidden textarea: the OS text-input surface.
 *
 * The textarea owns IME composition, keyboard repeat, and OS shortcuts. It is
 * positioned at the caret (so IME popups anchor correctly), kept invisible
 * (opacity 0.01 — not 0; Safari drops events on opacity:0) and click-through
 * (pointer-events:none). `beforeinput` is captured and mapped to engine
 * commands; the textarea itself never accumulates text. */
import { createEffect, onCleanup, onMount } from 'solid-js';
import { emptyPatch } from '@nge/core';
import type { EngineClient } from '../engine/engine-client';
import type {
    Command,
    LogicalPos,
    MoveDirection,
    TextAttrs,
    TextAttrsPatch,
} from '../engine/types';
import type { EngineStore } from '../state/engine-store';
import { copy, cut, paste } from '../input/clipboard';

/** Map a non-composition `InputEvent` to an engine command. */
function mapInputEventToCommand(e: InputEvent, caret: LogicalPos): Command | null {
    switch (e.inputType) {
        case 'insertText':
            return e.data ? { type: 'INSERT_TEXT', at: caret, text: e.data } : null;
        /* A <textarea> fires `insertLineBreak` for Enter; Shift+Enter is
           intercepted earlier in `onKeyDown` (soft break) and never reaches
           here. `insertParagraph` is contenteditable semantics. The document
           model has only paragraphs, so a hard Enter splits the block. */
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

    /* Focus stewardship (issues #35/#49). A canvas click blurs focus to
       <body> via the mousedown default, so the textarea re-grabs it on
       pointerup — but it must never steal focus from a control the user
       is actually interacting with. Skip the re-grab when the pointerup
       lands on (or inside) a focus-owning surface: form controls, their
       labels, editable regions, and open dialogs/popovers. Plain buttons
       are deliberately NOT exempt — after a toolbar button click (Bold,
       alignment, …) typing must continue into the document, which is
       exactly the re-grab; buttons inside dialogs are covered by the
       [role="dialog"] ancestor test. */
    const FOCUS_SINK =
        'input, textarea, select, option, label, ' +
        '[contenteditable="true"], [role="dialog"], .nge-dialog-overlay';
    const onPointerUp = (e: PointerEvent): void => {
        const t = e.target;
        if (t instanceof Element && t.closest(FOCUS_SINK)) return;
        focus();
    };
    /* On window re-focus the browser restores the pre-blur activeElement
       (possibly a dialog field) — only re-grab when nothing else holds
       focus, otherwise the restore is immediately undone. */
    const onWindowFocus = (): void => {
        const a = document.activeElement;
        if (!a || a === document.body || a === ref) focus();
    };
    onMount(() => {
        focus();
        document.addEventListener('pointerup', onPointerUp);
        window.addEventListener('focus', onWindowFocus);
    });
    onCleanup(() => {
        document.removeEventListener('pointerup', onPointerUp);
        window.removeEventListener('focus', onWindowFocus);
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

        /* Arrow keys: caret motion. Shift extends the selection
           (anchor stays put); Ctrl/Cmd promotes ArrowLeft/Right to
           word-jump (`WordLeft`/`WordRight`). Up/Down ignore the
           Ctrl/Cmd modifier — no paragraph-jump shortcut at this
           tier. preventDefault stops the textarea from scrolling
           its own (empty) viewport. */
        const wordJump = (e.ctrlKey || e.metaKey) && !e.altKey;
        const arrow: MoveDirection | undefined =
            e.key === 'ArrowUp'
                ? 'Up'
                : e.key === 'ArrowDown'
                  ? 'Down'
                  : e.key === 'ArrowLeft'
                    ? wordJump
                        ? 'WordLeft'
                        : 'Left'
                    : e.key === 'ArrowRight'
                      ? wordJump
                          ? 'WordRight'
                          : 'Right'
                      : undefined;
        if (arrow) {
            e.preventDefault();
            void props.client.dispatch({
                type: 'MOVE_CARET',
                direction: arrow,
                extend: e.shiftKey,
            });
            return;
        }

        /* Home / End. Ctrl/Cmd promotes line-bounded motion to document
           bounds (`DocHome` / `DocEnd`). Shift extends as with arrows. */
        const docJump = (e.ctrlKey || e.metaKey) && !e.altKey;
        const lineMotion: MoveDirection | undefined =
            e.key === 'Home'
                ? docJump
                    ? 'DocHome'
                    : 'LineHome'
                : e.key === 'End'
                  ? docJump
                      ? 'DocEnd'
                      : 'LineEnd'
                  : undefined;
        if (lineMotion) {
            e.preventDefault();
            void props.client.dispatch({
                type: 'MOVE_CARET',
                direction: lineMotion,
                extend: e.shiftKey,
            });
            return;
        }

        /* Tab — context-sensitive (Ctrl+Tab still falls through to the
           textarea default). Inside a table cell it navigates cell-to-cell
           (Shift+Tab = previous). In body text it inserts a real `\t`
           (U+0009), which the layout engine then aligns to the paragraph's
           ruler tab stops (or the default 0.5-inch grid). preventDefault
           stops the browser stealing Tab for focus navigation. */
        if (e.key === 'Tab' && !e.ctrlKey && !e.metaKey) {
            e.preventDefault();
            const caret = props.store.caretLogical();
            const inCell = !!caret && caret.path.steps.some((s) => s.kind === 'CELL');
            if (inCell) {
                void props.client.dispatch({
                    type: 'MOVE_CARET',
                    direction: e.shiftKey ? 'PrevCell' : 'NextCell',
                    extend: false,
                });
            } else if (caret && !e.shiftKey) {
                /* Body paragraph → insert a tab character. Shift+Tab in body
                   has no outdent command yet, so it is a no-op (it still
                   preventDefaults to avoid focus loss). */
                void props.client.dispatch({
                    type: 'INSERT_TEXT',
                    at: caret,
                    text: '\t',
                });
            }
            return;
        }

        /* Shift+Enter — SOFT break. A <textarea> fires `insertLineBreak`
           for both Enter and Shift+Enter, so the two are indistinguishable
           at `beforeinput`; catch the soft break here (where `shiftKey` is
           visible), insert a U+2028 LINE SEPARATOR into the run (no
           paragraph split — the layout engine forces a new line on it),
           and preventDefault so the textarea's `insertLineBreak` does NOT
           also fire and split the paragraph. Plain Enter falls through to
           `onBeforeInput` → SPLIT_PARAGRAPH. Modifier+Enter (Ctrl/Alt) is
           left to its existing path. */
        if (
            e.key === 'Enter' &&
            e.shiftKey &&
            !e.ctrlKey &&
            !e.metaKey &&
            !e.altKey
        ) {
            e.preventDefault();
            const caret = props.store.caretLogical();
            if (caret) {
                void props.client.dispatch({
                    type: 'INSERT_TEXT',
                    at: caret,
                    text: '\u2028',
                });
            }
            return;
        }

        const mod = e.ctrlKey || e.metaKey;
        if (!mod) return;
        /* Match e.code (physical key) — e.key is layout-dependent, so an
           Arabic layout reports a non-Latin key at the Z position. */
        if (e.code === 'KeyA' && !e.shiftKey) {
            e.preventDefault();
            void props.client.dispatch({ type: 'SELECT_ALL' });
        } else if (e.code === 'KeyZ' && !e.shiftKey) {
            e.preventDefault();
            void props.client.dispatch({ type: 'UNDO' });
        } else if (e.code === 'KeyY' || (e.code === 'KeyZ' && e.shiftKey)) {
            e.preventDefault();
            void props.client.dispatch({ type: 'REDO' });
        } else if (e.code === 'KeyV' && e.shiftKey) {
            /* Ctrl/Cmd+Shift+V — Paste Special: Unformatted Text.
               Bypass the HTML path so the engine sees only plain
               text (UX_BEHAVIOR_SPEC §V.2). The default Ctrl+V is
               handled by the `onPaste` handler below via the
               native `paste` ClipboardEvent. */
            e.preventDefault();
            void paste(props.client, true);
        } else if (e.code === 'KeyB' && !e.shiftKey) {
            e.preventDefault();
            toggleFormat((a) => ({ bold: !a.bold }));
        } else if (e.code === 'KeyI' && !e.shiftKey) {
            e.preventDefault();
            toggleFormat((a) => ({ italic: !a.italic }));
        } else if (e.code === 'KeyU' && !e.shiftKey) {
            e.preventDefault();
            toggleFormat((a) => ({
                underline: a.underline === 'None' ? 'Single' : 'None',
            }));
        }
    };

    /* Ctrl/Cmd+B/I/U — toggle relative to the engine-reported caret attrs.
       `range: undefined` binds the patch to the engine-owned live selection
       (never the async UI mirror), matching the facade helpers. */
    const toggleFormat = (
        patch: (a: TextAttrs) => Partial<TextAttrsPatch>,
    ): void => {
        const attrs = props.store.attrsAtCaret();
        if (!attrs) return;
        void props.client.dispatch({
            type: 'APPLY_FORMATTING',
            range: undefined,
            attrs: { ...emptyPatch(), ...patch(attrs) },
        });
    };

    /* §12 — clipboard. The native copy/cut/paste events fire inside a
       user-gesture context, which the async navigator.clipboard API requires;
       preventDefault stops the textarea acting on its own (empty) content. */
    const onCopy = (e: ClipboardEvent): void => {
        e.preventDefault();
        void copy(props.client);
    };
    const onCut = (e: ClipboardEvent): void => {
        e.preventDefault();
        void cut(props.client);
    };
    const onPaste = (e: ClipboardEvent): void => {
        e.preventDefault();
        void paste(props.client);
    };

    return (
        <textarea
            ref={ref}
            class="hidden-input"
            /* `aria-hidden` removed — browsers warn (and the WAI-ARIA spec
               forbids) hiding a focused element from AT. The textarea
               holds the OS text-input focus continuously; the canonical
               AT surface is the visually-hidden `AccessibilityTree`
               component rendered separately, not this one-shot input
               buffer (cleared on every keystroke). `aria-label` gives
               AT a one-word identifier instead of "edit, blank". */
            aria-label="editor input"
            /* Focus hand-back hook — `@nge/ui`'s focusEditorInput()
               (Ruler drag-end, Dialog close, comment-draft close) targets
               `textarea[data-nge-hidden-input]` to return typing focus to
               the document. The SDK's EditorSurface exposes the same hook
               as `#nge-hidden-input`. */
            data-nge-hidden-input=""
            tabindex="-1"
            autocomplete="off"
            autocapitalize="off"
            spellcheck={false}
            onCompositionStart={onCompositionStart}
            onCompositionUpdate={onCompositionUpdate}
            onCompositionEnd={onCompositionEnd}
            onBeforeInput={onBeforeInput}
            onKeyDown={onKeyDown}
            onCopy={onCopy}
            onCut={onCut}
            onPaste={onPaste}
        />
    );
}
