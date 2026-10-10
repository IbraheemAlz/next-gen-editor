/**
 * Issue #387 — keeping the caret visible in the scrolling viewport.
 *
 * Pure geometry (no DOM at import time, unit-tested) plus the one DOM
 * helper that applies it. `createCaretReveal` wires these to the engine's
 * `SELECTION_CHANGED.reveal_caret` stamp; `EditorSurface` and the
 * reference shell both use it.
 */

/** MIRROR of the engine's `MAX_PAINT_SCALE` (`crates/engine-wasm`): the
 *  ceiling on device px per layout pt a zoom may push the backing store
 *  to (issue #280). */
export const MAX_PAINT_SCALE = 4;

/** CSS px per layout pt: the shell seeds the engine scale with
 *  `devicePixelRatio × 96/72`. */
export const SCREEN_DPI_SCALE = 96 / 72;

/** Breathing room (CSS px) kept between a revealed caret and the
 *  viewport edge, so the line above / below stays readable. */
export const CARET_REVEAL_MARGIN_PX = 32;

/**
 * Engine device px per CSS px at user zoom `zoom` on a `dpr` display —
 * `dpr` until the paint-scale cap bites, then smaller (the capped bitmap
 * is stretched to the zoomed CSS size). Mirrors the engine's
 * `compose_paint_scale` and the reference shell's `deviceRatioFor`.
 */
export function deviceRatioFor(zoom: number, dpr: number): number {
    const z = zoom > 0 ? zoom : 1;
    const d = dpr > 0 ? dpr : 1;
    const base = d * SCREEN_DPI_SCALE;
    const scale = Math.min(base * z, Math.max(MAX_PAINT_SCALE, base));
    return scale / (SCREEN_DPI_SCALE * z);
}

/** The scroll state of a viewport (an `HTMLElement` satisfies it). */
export interface ScrollView {
    scrollTop: number;
    scrollLeft: number;
    clientWidth: number;
    clientHeight: number;
}

/** A box in the viewport's scroll-content coordinates (CSS px from the
 *  content's top-left, i.e. independent of the current scroll offset). */
export interface ViewBox {
    top: number;
    left: number;
    width: number;
    height: number;
}

/**
 * The scroll offsets that bring `box` into view with `margin` of
 * breathing room, scrolling as little as possible on each axis (a box
 * already inside the margin-inset view does not move it), or `null` when
 * no scroll is needed. A box taller / wider than the inset view aligns
 * its top / leading edge. Offsets are clamped at 0; the browser clamps
 * the far end.
 */
export function revealScroll(
    view: ScrollView,
    box: ViewBox,
    margin: number = CARET_REVEAL_MARGIN_PX,
): { top: number; left: number } | null {
    const axis = (pos: number, size: number, scroll: number, client: number): number => {
        const m = Math.max(0, Math.min(margin, (client - size) / 2));
        if (pos - m < scroll) return Math.max(0, pos - m);
        if (pos + size + m > scroll + client) {
            return size + 2 * m > client ? Math.max(0, pos - m) : pos + size + m - client;
        }
        return scroll;
    };
    if (!(view.clientHeight > 0) || !(view.clientWidth > 0)) return null;
    const top = axis(box.top, box.height, view.scrollTop, view.clientHeight);
    const left = axis(box.left, box.width, view.scrollLeft, view.clientWidth);
    if (Math.abs(top - view.scrollTop) < 0.5 && Math.abs(left - view.scrollLeft) < 0.5) {
        return null;
    }
    return { top, left };
}

/** Scroll `viewport` (minimally) so `box` is visible; `true` when it
 *  scrolled. */
export function keepBoxVisible(
    viewport: HTMLElement,
    box: ViewBox,
    margin: number = CARET_REVEAL_MARGIN_PX,
): boolean {
    const next = revealScroll(viewport, box, margin);
    if (!next) return false;
    viewport.scrollTop = next.top;
    viewport.scrollLeft = next.left;
    return true;
}

/** `el`'s top-left in `viewport`'s scroll-content coordinates. */
export function offsetInViewport(
    el: Element,
    viewport: HTMLElement,
): { top: number; left: number } {
    const r = el.getBoundingClientRect();
    const v = viewport.getBoundingClientRect();
    return {
        top: r.top - v.top - viewport.clientTop + viewport.scrollTop,
        left: r.left - v.left - viewport.clientLeft + viewport.scrollLeft,
    };
}
