/**
 * Issue #465 - pure maths for the comment connector line and the
 * caret-driven "active card": no DOM, no Solid, unit-tested in
 * `commentConnector.test.ts`.
 */
import type { LogicalPos, LogicalRange, PathStep } from '@nge/core';

export interface BoxRect {
    x: number;
    y: number;
    w: number;
    h: number;
}

export interface Pt {
    x: number;
    y: number;
}

/** The point a connector leaves from: the leading edge of the comment's
 *  FIRST rect in reading direction (left for LTR, right for RTL), at the
 *  rect's vertical middle. A point comment's 3 pt marker is its own first
 *  rect, so its leading edge is the marker. `undefined` when the comment
 *  has no rect (its anchor is beyond the laid-out band). */
export function anchorPoint(rects: readonly BoxRect[], rtl: boolean): Pt | undefined {
    const r = rects[0];
    if (!r) return undefined;
    return { x: rtl ? r.x + r.w : r.x, y: r.y + r.h / 2 };
}

/** Dotted leader: a horizontal run from the anchor to `elbowX` (the
 *  page's margin edge on the rail's side), then a straight line to the
 *  rail edge at the card's height. All coordinates in one space. */
export function connectorPath(anchor: Pt, elbowX: number, railX: number, cardY: number): string {
    /* The elbow never lies behind the anchor relative to the rail. */
    const towardRight = railX >= anchor.x;
    const ex = towardRight ? Math.max(elbowX, anchor.x) : Math.min(elbowX, anchor.x);
    const f = (n: number): string => (Math.round(n * 100) / 100).toString();
    return `M ${f(anchor.x)} ${f(anchor.y)} L ${f(ex)} ${f(anchor.y)} L ${f(railX)} ${f(cardY)}`;
}

/** A y inside `[top, bottom]` (the visible band of a scroller). */
export function inBand(y: number, top: number, bottom: number): boolean {
    return y >= top && y <= bottom;
}

function stepOrder(s: PathStep): [number, number] {
    return s.kind === 'CELL' ? [s.row, s.col] : [s.idx, 0];
}

/** Document order of two positions: the paths depth-first (a block
 *  index, then a cell's row and column), then the byte offset. */
export function comparePos(a: LogicalPos, b: LogicalPos): number {
    const n = Math.min(a.path.steps.length, b.path.steps.length);
    for (let i = 0; i < n; i++) {
        const [x0, x1] = stepOrder(a.path.steps[i]!);
        const [y0, y1] = stepOrder(b.path.steps[i]!);
        if (x0 !== y0) return x0 - y0;
        if (x1 !== y1) return x1 - y1;
    }
    return a.path.steps.length - b.path.steps.length || a.offset - b.offset;
}

/** Whether a caret sits in a comment's range: a ranged comment owns
 *  `[start, end)` (the caret just after its last character is outside,
 *  as in Word), a point comment owns exactly its anchor. */
export function caretInComment(caret: LogicalPos, range: LogicalRange): boolean {
    const toStart = comparePos(caret, range.start);
    if (comparePos(range.start, range.end) === 0) return toStart === 0;
    return toStart >= 0 && comparePos(caret, range.end) < 0;
}

/** The active comment for a selection: the innermost (latest-starting)
 *  comment whose range holds the selection's start. */
export function activeCommentId(
    selection: LogicalRange | undefined,
    comments: readonly { id: number; range: LogicalRange }[],
): number | null {
    if (!selection) return null;
    let best: { id: number; range: LogicalRange } | undefined;
    for (const c of comments) {
        if (!caretInComment(selection.start, c.range)) continue;
        if (!best || comparePos(c.range.start, best.range.start) >= 0) best = c;
    }
    return best ? best.id : null;
}
