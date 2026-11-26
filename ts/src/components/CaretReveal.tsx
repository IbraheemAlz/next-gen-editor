/* Issue #387 — keep the caret visible in `.editor-viewport` after keyboard
 * navigation and typing (never after a pointer click: the engine stamps
 * `SELECTION_CHANGED.reveal_caret` from the command's `bridge::meta`
 * classification).
 *
 * The scroll itself is `@nge/core`'s `createCaretReveal` — the same
 * primitive `EditorSurface` uses; this shell supplies the page-aware
 * mapping: the engine caret (absolute document device px, x page-local)
 * ÷ the zoom-aware device ratio, placed on the DOM page card that holds
 * it, so mixed page sizes, the inter-page gap and the zoom are all
 * honoured. Renders nothing; must mount under the `EngineProvider`. */
import { createCaretReveal, offsetInViewport, type Rect, type ViewBox } from '@nge/core';
import { deviceRatio, type EngineStore } from '../state/engine-store';

/** The engine caret (absolute document device px, x page-local) as a box in
 *  `viewport`'s scroll-content CSS px — the shell's page-aware mapping, shared
 *  by the caret reveal and the zoom anchor (issue #312). */
export function shellCaretBox(
    store: EngineStore,
    caret: Rect,
    viewport: HTMLElement,
): ViewBox | undefined {
    const ratio = deviceRatio();
    const y = caret.y / ratio;
    const x = caret.x / ratio;
    const size = { width: Math.max(1, caret.w / ratio), height: caret.h / ratio };
    const count = Math.max(1, store.pageCount());
    let idx = count - 1;
    for (let i = 0; i < count; i++) {
        if (y < store.pageTopCss(i) + store.pageHeightCss(i)) {
            idx = i;
            break;
        }
    }
    const page = viewport.querySelector(`.editor-page[data-page-index="${idx}"]`);
    if (page) {
        const o = offsetInViewport(page, viewport);
        return { top: o.top + y - store.pageTopCss(idx), left: o.left + x, ...size };
    }
    /* The card is not mounted yet: place it against the page stack. */
    const pages = viewport.querySelector('.editor-pages');
    if (!pages) return undefined;
    const o = offsetInViewport(pages, viewport);
    return { top: o.top + y, left: o.left + x, ...size };
}

export function CaretReveal(props: {
    store: EngineStore;
    viewport: () => HTMLElement | undefined;
}) {
    createCaretReveal({
        viewport: props.viewport,
        caretBox: (caret, viewport) => shellCaretBox(props.store, caret, viewport),
    });
    return null;
}
