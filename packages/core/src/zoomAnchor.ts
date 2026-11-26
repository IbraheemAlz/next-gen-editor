/**
 * zoomAnchor — issue #312: the math of "a zoom must not make the view
 * jump". Pure and DOM-free (unit-tested in `zoomAnchor.test.ts`); the
 * controller that reads the DOM and writes `scrollTop` is
 * `createZoomAnchor.ts`.
 *
 * Word and Docs anchor a zoom on the caret when it is in view, otherwise
 * on the centre of the viewport. Before the zoom is applied we pick that
 * point and remember WHERE IT IS IN THE DOCUMENT — the page card that
 * holds it and its distance below that card's top, divided by the zoom —
 * plus how far below the viewport's top edge it sat. After the new zoom
 * has painted we look the same page up again, scale the distance by the
 * NEW zoom and scroll so the point is back at the same viewport offset.
 *
 * Why "page + offset at 100 %" and not a plain `y × newZoom ÷ oldZoom`:
 * a page card is the ceil'd device bitmap ÷ the device ratio and the
 * desk adds constant padding, so document-wide linear scaling drifts by a
 * pixel per page (30 px at page 30). Within ONE card the scale is exact,
 * and the card's own new top is read from the DOM the user will see.
 *
 * All positions are CSS px in the viewport's SCROLL-CONTENT space (the
 * same space `caretReveal.ts` works in: `offsetInViewport`).
 */

/** A page card's extent in scroll-content CSS px. */
export interface PageSpan {
    /** The page's index in the document (`data-page-index`). */
    readonly index: number;
    readonly top: number;
    readonly height: number;
}

/** The viewport's scroll state, CSS px. */
export interface AnchorView {
    readonly scrollTop: number;
    readonly clientHeight: number;
}

/** A box in scroll-content CSS px (the part of `ViewBox` the anchor needs). */
export interface VerticalBox {
    readonly top: number;
    readonly height: number;
}

/** What a zoom is anchored on. */
export type ZoomAnchorKind = 'caret' | 'centre';

/** The anchor's document position, independent of the zoom. */
export interface ZoomAnchor {
    readonly kind: ZoomAnchorKind;
    /** The page card that holds the anchor. */
    readonly page: number;
    /** The anchor's distance below that card's top, at 100 % zoom (CSS px). */
    readonly offset100: number;
    /** How far below the viewport's top edge the anchor sat (CSS px). */
    readonly viewportY: number;
}

/** The point to anchor on: the caret's top when its centre is inside the
 *  viewport, else the viewport's centre. `y` is in scroll-content px. */
export function chooseAnchorPoint(
    view: AnchorView,
    caret: VerticalBox | undefined,
): { y: number; kind: ZoomAnchorKind } {
    if (caret !== undefined) {
        const centre = caret.top + caret.height / 2;
        if (centre >= view.scrollTop && centre <= view.scrollTop + view.clientHeight) {
            return { y: caret.top, kind: 'caret' };
        }
    }
    return { y: view.scrollTop + view.clientHeight / 2, kind: 'centre' };
}

/** The page whose card owns scroll-content `y`: the last page whose top is
 *  at or above it (so the gap below a card belongs to that card), else the
 *  first page (a point above the first card — the desk padding). */
function ownerOf(pages: readonly PageSpan[], y: number): PageSpan | undefined {
    let owner: PageSpan | undefined;
    for (const p of pages) {
        if (p.top <= y && (owner === undefined || p.top >= owner.top)) owner = p;
    }
    if (owner !== undefined) return owner;
    let first: PageSpan | undefined;
    for (const p of pages) if (first === undefined || p.top < first.top) first = p;
    return first;
}

/** Remember where scroll-content `y` is in the document, at `zoom`.
 *  `undefined` when no page is known or the zoom is unusable. */
export function captureAnchor(
    y: number,
    kind: ZoomAnchorKind,
    view: AnchorView,
    pages: readonly PageSpan[],
    zoom: number,
): ZoomAnchor | undefined {
    if (!(zoom > 0) || !Number.isFinite(y)) return undefined;
    const owner = ownerOf(pages, y);
    if (owner === undefined) return undefined;
    return {
        kind,
        page: owner.index,
        offset100: (y - owner.top) / zoom,
        viewportY: y - view.scrollTop,
    };
}

/** The `scrollTop` that puts `anchor` back at its viewport offset once the
 *  document is at `zoom` and its pages are `pages`. `undefined` when the
 *  anchor's page is not (yet) among them — the caller leaves the scroll
 *  alone. Clamped to what the content can scroll. */
export function anchoredScrollTop(
    anchor: ZoomAnchor,
    pages: readonly PageSpan[],
    zoom: number,
    view: { readonly scrollHeight: number; readonly clientHeight: number },
): number | undefined {
    if (!(zoom > 0)) return undefined;
    const owner = pages.find((p) => p.index === anchor.page);
    if (owner === undefined) return undefined;
    const y = owner.top + anchor.offset100 * zoom;
    const max = Math.max(0, view.scrollHeight - view.clientHeight);
    return Math.min(max, Math.max(0, y - anchor.viewportY));
}
