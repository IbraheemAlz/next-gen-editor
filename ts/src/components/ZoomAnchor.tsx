/* Issue #312 - a zoom keeps what the user is looking at where it is.
 *
 * The scroll itself is `@nge/core`'s `createZoomAnchor` (triggered by
 * `cmd.setZoom`, the one place a user zoom is requested); this shell
 * supplies the page-aware mapping: the same caret box the caret reveal uses
 * (`shellCaretBox`) and the page cards' MEASURED extents in the viewport's
 * scroll content - the cards are what the user sees, and each is the ceil'd
 * device bitmap divided by the device ratio, so they are read from the DOM
 * rather than computed. Renders nothing; must mount under the
 * `EngineProvider`. */
import { createZoomAnchor, offsetInViewport, type PageSpan } from '@nge/core';
import type { EngineStore } from '../state/engine-store';
import { shellCaretBox } from './CaretReveal';

/** The mounted page cards of `viewport`, by `data-page-index`. */
export function pageSpans(viewport: HTMLElement): PageSpan[] {
    const spans: PageSpan[] = [];
    for (const card of viewport.querySelectorAll<HTMLElement>('.editor-page[data-page-index]')) {
        const index = Number(card.dataset.pageIndex);
        if (!Number.isInteger(index)) continue;
        spans.push({
            index,
            top: offsetInViewport(card, viewport).top,
            height: card.getBoundingClientRect().height,
        });
    }
    return spans;
}

export function ZoomAnchor(props: {
    store: EngineStore;
    viewport: () => HTMLElement | undefined;
}) {
    createZoomAnchor({
        viewport: props.viewport,
        caretBox: (caret, viewport) => shellCaretBox(props.store, caret, viewport),
        pages: pageSpans,
    });
    return null;
}
