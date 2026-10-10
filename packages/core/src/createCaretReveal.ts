/**
 * createCaretReveal — issue #387: keep the caret visible after keyboard
 * navigation and typing.
 *
 * Follows `createEditorState().caretRevealSeq` (the engine stamps
 * `SELECTION_CHANGED.reveal_caret` from the command's `bridge::meta`
 * classification: navigation, typing, edits, undo — never a pointer
 * gesture, Ctrl+A or a zoom) and scrolls the host's viewport minimally so
 * the caret stays inside it, with a margin. The mapping from the engine's
 * caret rect (absolute document device px) to the viewport's scroll
 * content is the host's — it owns the page layout and the zoom; the
 * scroll runs one frame after the event, when a repaint that grew the
 * document (a new page) has resized the host's DOM.
 */
import { createEffect, on, onCleanup, untrack } from 'solid-js';
import { createEditorState } from './createEditorState';
import { CARET_REVEAL_MARGIN_PX, keepBoxVisible, type ViewBox } from './caretReveal';
import type { Rect } from './types';

export interface CaretRevealOptions {
    /** The scrolling element; `undefined` (not mounted yet) skips. */
    viewport: () => HTMLElement | undefined;
    /** The engine caret rect (document device px) as a box in the
     *  viewport's scroll-content CSS px — zoom applied; `undefined` when it
     *  cannot be placed yet. */
    caretBox: (caret: Rect, viewport: HTMLElement) => ViewBox | undefined;
    /** Breathing room in CSS px (default {@link CARET_REVEAL_MARGIN_PX}). */
    margin?: number;
    /** Gate (default: always on). */
    enabled?: () => boolean;
}

export function createCaretReveal(opts: CaretRevealOptions): void {
    const state = createEditorState();
    let frame: number | undefined;
    let fallback: ReturnType<typeof setTimeout> | undefined;
    const cancel = (): void => {
        if (frame !== undefined) cancelAnimationFrame(frame);
        if (fallback !== undefined) clearTimeout(fallback);
        frame = undefined;
        fallback = undefined;
    };
    const run = (): void => {
        cancel();
        if (opts.enabled && !untrack(opts.enabled)) return;
        const viewport = opts.viewport();
        const caret = untrack(state.caret);
        if (!viewport || !caret) return;
        const box = opts.caretBox(caret, viewport);
        if (box) keepBoxVisible(viewport, box, opts.margin ?? CARET_REVEAL_MARGIN_PX);
    };
    createEffect(
        on(
            state.caretRevealSeq,
            () => {
                /* Coalesce a burst of keystrokes into one scroll per frame;
                   the timeout covers a hidden tab, where rAF is paused. */
                if (frame !== undefined) return;
                frame = requestAnimationFrame(run);
                fallback = setTimeout(run, 120);
            },
            { defer: true },
        ),
    );
    onCleanup(cancel);
}
