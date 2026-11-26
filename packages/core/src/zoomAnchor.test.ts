/* Issue #312 - the zoom-anchor math: pick what the user is looking at,
 * remember where it is in the DOCUMENT (page card + distance below it at
 * 100 %), put it back at the same viewport offset after the zoom. Pure: the
 * page layout is modelled here, the way the shell's cards behave (heights
 * proportional to the zoom, rounded UP like the ceil'd device bitmap; a
 * constant desk padding above the first card; a gap proportional to the
 * zoom). */
import { describe, expect, it } from 'vitest';
import {
    anchoredScrollTop,
    captureAnchor,
    chooseAnchorPoint,
    type PageSpan,
} from './zoomAnchor';

const DESK = 40;
const GAP_100 = 64;

/** Page cards stacked at `zoom`: `h100[i] * zoom` rounded up, a gap of
 *  `64 * zoom` between them, `DESK` px of padding above the first. */
function layout(h100: readonly number[], zoom: number): PageSpan[] {
    const spans: PageSpan[] = [];
    let top = DESK;
    h100.forEach((h, index) => {
        const height = Math.ceil(h * zoom);
        spans.push({ index, top, height });
        top += height + GAP_100 * zoom;
    });
    return spans;
}

const A4 = 1123;
const pages = (n: number): number[] => Array.from({ length: n }, () => A4);

/** The scroll-content y of a document point at `zoom`: page + offset100. */
const yOf = (spans: PageSpan[], page: number, offset100: number, zoom: number): number =>
    spans[page]!.top + offset100 * zoom;

describe('chooseAnchorPoint (#312)', () => {
    const view = { scrollTop: 1000, clientHeight: 600 };

    it('anchors on the caret when its centre is inside the viewport', () => {
        const p = chooseAnchorPoint(view, { top: 1200, height: 20 });
        expect(p).toEqual({ y: 1200, kind: 'caret' });
    });

    it('anchors on the viewport centre when the caret is above or below it', () => {
        expect(chooseAnchorPoint(view, { top: 100, height: 20 })).toEqual({
            y: 1300,
            kind: 'centre',
        });
        expect(chooseAnchorPoint(view, { top: 1700, height: 20 })).toEqual({
            y: 1300,
            kind: 'centre',
        });
    });

    it('anchors on the viewport centre when there is no caret', () => {
        expect(chooseAnchorPoint(view, undefined)).toEqual({ y: 1300, kind: 'centre' });
    });

    it('a caret half scrolled off keeps the caret anchor while its centre is visible', () => {
        /* top above the viewport, centre inside. */
        expect(chooseAnchorPoint(view, { top: 995, height: 20 })).toEqual({
            y: 995,
            kind: 'caret',
        });
        /* centre exactly on the edges still counts as in view. */
        expect(chooseAnchorPoint(view, { top: 1590, height: 20 }).kind).toBe('caret');
        expect(chooseAnchorPoint(view, { top: 1595, height: 20 }).kind).toBe('centre');
    });
});

describe('captureAnchor / anchoredScrollTop (#312)', () => {
    const h = pages(5);

    /** Zoom `from` -> `to` with the anchor at scroll-content `y`: the
     *  anchor's viewport offset after the zoom. */
    function zoomed(
        y: number,
        scrollTop: number,
        from: number,
        to: number,
        heights: readonly number[] = h,
    ): { before: number; after: number; scrollTop: number } {
        const before = layout(heights, from);
        const view = { scrollTop, clientHeight: 700 };
        const anchor = captureAnchor(y, 'caret', view, before, from)!;
        const next = layout(heights, to);
        const top = anchoredScrollTop(anchor, next, to, {
            scrollHeight: next.at(-1)!.top + next.at(-1)!.height + DESK,
            clientHeight: 700,
        })!;
        const newY = yOf(next, anchor.page, anchor.offset100, to);
        return { before: y - scrollTop, after: newY - top, scrollTop: top };
    }

    it('keeps the anchor at the same viewport offset through 100 -> 150 -> 75', () => {
        /* A caret on page 2, mid-viewport. */
        const spans = layout(h, 1);
        const y = spans[2]!.top + 400;
        const scrollTop = y - 300;
        const up = zoomed(y, scrollTop, 1, 1.5);
        expect(up.after).toBeCloseTo(up.before, 9);
        expect(up.scrollTop).toBeGreaterThan(scrollTop);

        /* ...and the next step starts from where that landed. */
        const spans150 = layout(h, 1.5);
        const y150 = yOf(spans150, 2, 400, 1.5);
        const down = zoomed(y150, up.scrollTop, 1.5, 0.75);
        expect(down.after).toBeCloseTo(down.before, 9);
        expect(down.scrollTop).toBeLessThan(up.scrollTop);
    });

    it('is exact where plain proportional scaling drifts a pixel per page', () => {
        /* 30 pages of 1123.3 css px: each card is ceil'd, so the stack is
           longer than `zoom x` the 100 % stack by up to a pixel per page. */
        const many = Array.from({ length: 30 }, () => 1123.3);
        const spans = layout(many, 1);
        const y = spans[29]!.top + 500;
        const scrollTop = y - 350;
        const r = zoomed(y, scrollTop, 1, 1.5, many);
        expect(r.after).toBeCloseTo(r.before, 9);
        /* What scaling the whole stack by 1.5 would have done: the anchor
           lands elsewhere (the cards were ceil'd, the desk is constant). */
        const next = layout(many, 1.5);
        const trueY = yOf(next, 29, 500, 1.5);
        const naiveY = DESK + (y - DESK) * 1.5;
        expect(Math.abs(trueY - naiveY)).toBeGreaterThan(3);
    });

    it('works with pages of different heights (a landscape section)', () => {
        const mixed = [1123, 1123, 794, 1123, 794];
        const spans = layout(mixed, 1);
        const y = spans[3]!.top + 250;
        const r = zoomed(y, y - 200, 1, 2, mixed);
        expect(r.after).toBeCloseTo(r.before, 9);
    });

    it('anchors a point in the gap below a card to that card', () => {
        const spans = layout(h, 1);
        const inGap = spans[1]!.top + spans[1]!.height + 20;
        const anchor = captureAnchor(inGap, 'centre', { scrollTop: inGap - 300, clientHeight: 700 }, spans, 1)!;
        expect(anchor.page).toBe(1);
        const r = zoomed(inGap, inGap - 300, 1, 1.25);
        expect(r.after).toBeCloseTo(r.before, 9);
    });

    it('a point above the first card (the desk) anchors to the first page', () => {
        const spans = layout(h, 1);
        const anchor = captureAnchor(10, 'centre', { scrollTop: 0, clientHeight: 700 }, spans, 1)!;
        expect(anchor.page).toBe(0);
        expect(anchor.offset100).toBeCloseTo(10 - DESK, 9);
        /* Scaling the (negative) distance keeps it above the card; the
           scroll that would restore it is negative, so it clamps to 0. */
        const next = layout(h, 2);
        const top = anchoredScrollTop(anchor, next, 2, { scrollHeight: 9000, clientHeight: 700 })!;
        expect(top).toBe(0);
        /* From a scrolled view the same anchor is restored exactly. */
        const scrolled = captureAnchor(DESK - 10, 'centre', { scrollTop: 0, clientHeight: 700 }, spans, 1)!;
        const back = anchoredScrollTop(scrolled, layout(h, 1), 1, { scrollHeight: 9000, clientHeight: 700 })!;
        expect(back).toBeCloseTo(0, 9);
    });

    it('clamps to what the content can scroll', () => {
        const spans = layout(h, 1);
        const y = spans[0]!.top + 20;
        const anchor = captureAnchor(y, 'caret', { scrollTop: 0, clientHeight: 700 }, spans, 1)!;
        /* Zooming out: the anchor would want a negative scroll. */
        const small = layout(h, 0.25);
        const top = anchoredScrollTop(anchor, small, 0.25, { scrollHeight: 2000, clientHeight: 700 })!;
        expect(top).toBe(0);
        /* A short document cannot scroll at all. */
        expect(anchoredScrollTop(anchor, small, 0.25, { scrollHeight: 500, clientHeight: 700 })).toBe(0);
        /* The bottom of the page stack. */
        const deep = captureAnchor(
            spans[4]!.top + 1000,
            'caret',
            { scrollTop: spans[4]!.top + 900, clientHeight: 700 },
            spans,
            1,
        )!;
        const big = layout(h, 4);
        const scrollHeight = big.at(-1)!.top + big.at(-1)!.height + DESK;
        const max = scrollHeight - 700;
        expect(anchoredScrollTop(deep, big, 4, { scrollHeight: scrollHeight - 5000, clientHeight: 700 })).toBeLessThanOrEqual(
            max,
        );
    });

    it('cannot place an anchor whose page is not in the new layout', () => {
        const spans = layout(h, 1);
        const anchor = captureAnchor(spans[3]!.top + 5, 'caret', { scrollTop: spans[3]!.top - 100, clientHeight: 700 }, spans, 1)!;
        expect(anchoredScrollTop(anchor, layout(pages(2), 1.5), 1.5, { scrollHeight: 9000, clientHeight: 700 })).toBeUndefined();
    });

    it('refuses unusable input', () => {
        const spans = layout(h, 1);
        const view = { scrollTop: 0, clientHeight: 700 };
        expect(captureAnchor(100, 'caret', view, [], 1)).toBeUndefined();
        expect(captureAnchor(100, 'caret', view, spans, 0)).toBeUndefined();
        expect(captureAnchor(Number.NaN, 'caret', view, spans, 1)).toBeUndefined();
        const anchor = captureAnchor(100, 'caret', view, spans, 1)!;
        expect(anchoredScrollTop(anchor, spans, 0, { scrollHeight: 9000, clientHeight: 700 })).toBeUndefined();
    });

    it('a round trip 100 -> 150 -> 75 -> 100 lands where it started (within a pixel)', () => {
        const spans = layout(h, 1);
        const y0 = spans[3]!.top + 333;
        const s0 = y0 - 410;
        const a = zoomed(y0, s0, 1, 1.5);
        const y1 = yOf(layout(h, 1.5), 3, 333, 1.5);
        const b = zoomed(y1, a.scrollTop, 1.5, 0.75);
        const y2 = yOf(layout(h, 0.75), 3, 333, 0.75);
        const c = zoomed(y2, b.scrollTop, 0.75, 1);
        expect(Math.abs(c.scrollTop - s0)).toBeLessThan(1);
    });
});
