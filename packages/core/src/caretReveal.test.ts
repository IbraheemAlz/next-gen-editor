import { describe, expect, it } from 'vitest';
import { deviceRatioFor, revealScroll, type ScrollView } from './caretReveal';

const view = (scrollTop = 0, scrollLeft = 0): ScrollView => ({
    scrollTop,
    scrollLeft,
    clientWidth: 800,
    clientHeight: 600,
});

describe('revealScroll (issue #387)', () => {
    it('leaves a caret that is already visible alone', () => {
        expect(revealScroll(view(0), { top: 100, left: 50, width: 2, height: 20 }, 32)).toBeNull();
    });

    it('scrolls down just enough to bring a caret below the view in, with the margin', () => {
        const next = revealScroll(view(0), { top: 700, left: 50, width: 2, height: 20 }, 32);
        expect(next).toEqual({ top: 700 + 20 + 32 - 600, left: 0 });
    });

    it('scrolls up to a caret above the view, keeping the margin', () => {
        const next = revealScroll(view(1000), { top: 900, left: 50, width: 2, height: 20 }, 32);
        expect(next).toEqual({ top: 900 - 32, left: 0 });
    });

    it('treats the margin band as outside the view', () => {
        /* 10 px from the bottom edge: inside the view, inside the margin. */
        const next = revealScroll(view(0), { top: 570, left: 50, width: 2, height: 20 }, 32);
        expect(next?.top).toBe(570 + 20 + 32 - 600);
    });

    it('reveals horizontally when the zoomed page is wider than the view', () => {
        const next = revealScroll(view(0, 0), { top: 100, left: 1200, width: 2, height: 20 }, 32);
        expect(next).toEqual({ top: 0, left: 1200 + 2 + 32 - 800 });
    });

    it('never scrolls to a negative offset', () => {
        const next = revealScroll(view(50), { top: 10, left: 0, width: 2, height: 20 }, 32);
        expect(next).toEqual({ top: 0, left: 0 });
    });

    it('aligns the top of a box taller than the view', () => {
        const next = revealScroll(view(0), { top: 2000, left: 0, width: 2, height: 900 }, 32);
        expect(next?.top).toBe(2000);
    });

    it('does nothing for an unlaid-out (zero-size) viewport', () => {
        expect(
            revealScroll(
                { scrollTop: 0, scrollLeft: 0, clientWidth: 0, clientHeight: 0 },
                { top: 900, left: 0, width: 2, height: 20 },
            ),
        ).toBeNull();
    });
});

describe('deviceRatioFor (issue #387, mirrors the engine paint-scale cap)', () => {
    it('is the device pixel ratio below the cap', () => {
        expect(deviceRatioFor(1, 1)).toBeCloseTo(1);
        expect(deviceRatioFor(2, 1)).toBeCloseTo(1);
        expect(deviceRatioFor(1, 2)).toBeCloseTo(2);
    });

    it('drops once the zoomed scale passes MAX_PAINT_SCALE', () => {
        /* dpr 2 × 4/3 × 3 = 8 device px per pt > 4: capped. */
        expect(deviceRatioFor(3, 2)).toBeCloseTo(4 / ((96 / 72) * 3));
    });
});
