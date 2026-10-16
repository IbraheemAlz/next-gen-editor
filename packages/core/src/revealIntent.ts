/**
 * Issue #486 — when a pending caret reveal must NOT scroll.
 *
 * `createCaretReveal` scrolls the viewport so the caret stays visible after
 * keyboard navigation and typing (#387). That scroll is a *consequence* of
 * an earlier input; if the user scrolled in the meantime the user's scroll
 * is the later word and the reveal must stand down (it used to land a frame
 * after the selection event and yank the view back to the caret — #473).
 *
 * DOM-free and clock-injected so the decision logic is unit-testable: the
 * glue in `createCaretReveal` feeds it `performance.now()` stamps.
 *
 * Two kinds of evidence, with different attribution:
 *
 *  - **Direct intent** (`wheel`, `touchmove`, a pointer press on the
 *    scrollbar, a key pressed on the scroller itself): fired at the moment
 *    the user acts, so it is attributable to the instant. It supersedes a
 *    reveal whose command was SENT before it — including the window
 *    between sending the command and the engine's reply, which is where a
 *    flick right after a key press lands.
 *  - **A `scroll` event** that does not match the reveal's own write
 *    (scrollbar drag, keyboard scrolling, a programmatic `scrollTop`):
 *    delivered up to a frame after the scroll happened, so it cannot be
 *    attributed to before the command was sent. It supersedes only a
 *    reveal whose reply has ALREADY arrived (between the event and the
 *    frame) — the #473 shape.
 */

/** Offsets within this many CSS px count as the same scroll position (the
 *  browser rounds `scrollTop` to device pixels). */
export const SCROLL_POSITION_EPSILON_PX = 1;

/** One pending reveal: when the command that asked for it was sent, and
 *  when the engine's reply (the selection event) arrived. */
export interface RevealRequest {
    /** `performance.now()` when the causing command left the main thread
     *  (clamped to `eventAt`; equal to it when the client does not say). */
    sentAt: number;
    /** `performance.now()` when the selection event arrived. */
    eventAt: number;
}

export class ScrollIntentLedger {
    private lastDirectAt = Number.NEGATIVE_INFINITY;
    private lastScrollAt = Number.NEGATIVE_INFINITY;
    private own: { top: number; left: number } | undefined;

    /** A direct user scroll intent (wheel, touch drag, scrollbar press, a
     *  key on the scroller) at `at`. Forgets the reveal's own pending write:
     *  whatever scroll event follows belongs to the user. */
    noteIntent(at: number): void {
        this.lastDirectAt = at;
        this.own = undefined;
    }

    /** The reveal itself wrote the scroll offsets (`top`, `left` as read
     *  back from the element). Its `scroll` event, which fires later, is
     *  not user intent. */
    noteOwnWrite(top: number, left: number): void {
        this.own = { top, left };
    }

    /**
     * A `scroll` event at offsets (`top`, `left`), observed at `at`.
     * Returns `true` when it is user (or programmatic) intent, `false` when
     * it is the echo of the reveal's own write (consumed).
     */
    noteScroll(top: number, left: number, at: number): boolean {
        const own = this.own;
        if (
            own &&
            Math.abs(own.top - top) <= SCROLL_POSITION_EPSILON_PX &&
            Math.abs(own.left - left) <= SCROLL_POSITION_EPSILON_PX
        ) {
            this.own = undefined;
            return false;
        }
        this.own = undefined;
        this.lastScrollAt = at;
        return true;
    }

    /** Whether a scroll intent has arrived that the reveal for `req` must
     *  yield to. */
    supersedes(req: RevealRequest): boolean {
        return this.lastDirectAt >= req.sentAt || this.lastScrollAt >= req.eventAt;
    }
}

/** What to do with a reveal request at the moment the event arrives. */
export type SyncReveal =
    | 'superseded' // the user already scrolled after sending the command: drop it
    | 'now' // the caret box is known: scroll in this task
    | 'defer'; // no geometry yet: let the frame / timer pass place it

export function decideSync(
    ledger: ScrollIntentLedger,
    req: RevealRequest,
    boxKnown: boolean,
): SyncReveal {
    if (ledger.supersedes(req)) return 'superseded';
    return boxKnown ? 'now' : 'defer';
}

/** What the frame / timer pass does: scroll again (the layout may have
 *  settled late, or the first write was clamped) unless the user scrolled
 *  since. */
export function decideVerify(
    ledger: ScrollIntentLedger,
    req: RevealRequest,
    boxKnown: boolean,
): 'reveal' | 'skip' {
    if (ledger.supersedes(req)) return 'skip';
    return boxKnown ? 'reveal' : 'skip';
}

/** The geometry of a pointer press relative to a scroller. */
export interface ScrollerHit {
    /** Pointer position in viewport (client) coordinates. */
    clientX: number;
    clientY: number;
    /** The scroller's border box (`getBoundingClientRect`). */
    rect: { left: number; top: number; right: number; bottom: number };
    /** The scroller's padding box inside its borders. */
    clientLeft: number;
    clientTop: number;
    clientWidth: number;
    clientHeight: number;
}

/**
 * Whether a pointer press landed on the scroller's own scrollbar (or
 * border): inside the border box but outside the padding box that holds
 * the content. A press there starts a scrollbar drag — a scroll intent
 * that fires no `wheel` event, and whose first `scroll` event comes a
 * frame later.
 */
export function isScrollbarPress(hit: ScrollerHit): boolean {
    const { clientX: x, clientY: y, rect } = hit;
    if (x < rect.left || x >= rect.right || y < rect.top || y >= rect.bottom) return false;
    const innerLeft = rect.left + hit.clientLeft;
    const innerTop = rect.top + hit.clientTop;
    return (
        x < innerLeft ||
        x >= innerLeft + hit.clientWidth ||
        y < innerTop ||
        y >= innerTop + hit.clientHeight
    );
}
