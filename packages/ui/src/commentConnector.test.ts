import { describe, expect, it } from 'vitest';
import type { LogicalPos, LogicalRange } from '@nge/core';
import {
    activeCommentId,
    anchorPoint,
    caretInComment,
    connectorPath,
    inBand,
} from './commentConnector';

const pos = (idx: number, offset: number): LogicalPos => ({
    path: { steps: [{ kind: 'BLOCK', idx }] },
    offset,
});
const range = (a: LogicalPos, b: LogicalPos): LogicalRange => ({ start: a, end: b });

describe('anchorPoint', () => {
    const rects = [
        { x: 100, y: 40, w: 50, h: 20 },
        { x: 20, y: 60, w: 80, h: 20 },
    ];
    it('is the first rect leading edge, LTR', () => {
        expect(anchorPoint(rects, false)).toEqual({ x: 100, y: 50 });
    });
    it('is the right edge of the first rect for RTL', () => {
        expect(anchorPoint(rects, true)).toEqual({ x: 150, y: 50 });
    });
    it('uses the marker of a point comment', () => {
        expect(anchorPoint([{ x: 10, y: 0, w: 3, h: 10 }], false)).toEqual({ x: 10, y: 5 });
    });
    it('is undefined without rects', () => {
        expect(anchorPoint([], false)).toBeUndefined();
    });
});

describe('connectorPath', () => {
    it('runs horizontally to the elbow then to the rail edge', () => {
        expect(connectorPath({ x: 10, y: 20 }, 100, 200, 80)).toBe('M 10 20 L 100 20 L 200 80');
    });
    it('never puts the elbow behind the anchor', () => {
        expect(connectorPath({ x: 150, y: 20 }, 100, 200, 80)).toBe('M 150 20 L 150 20 L 200 80');
    });
    it('mirrors toward a rail on the left', () => {
        expect(connectorPath({ x: 150, y: 20 }, 100, 0, 80)).toBe('M 150 20 L 100 20 L 0 80');
    });
});

describe('inBand', () => {
    it('is inclusive', () => {
        expect(inBand(5, 5, 10)).toBe(true);
        expect(inBand(11, 5, 10)).toBe(false);
    });
});

describe('caretInComment', () => {
    const r = range(pos(0, 4), pos(0, 9));
    it('owns [start, end)', () => {
        expect(caretInComment(pos(0, 3), r)).toBe(false);
        expect(caretInComment(pos(0, 4), r)).toBe(true);
        expect(caretInComment(pos(0, 8), r)).toBe(true);
        expect(caretInComment(pos(0, 9), r)).toBe(false);
    });
    it('a point comment owns only its anchor', () => {
        const p = range(pos(1, 2), pos(1, 2));
        expect(caretInComment(pos(1, 2), p)).toBe(true);
        expect(caretInComment(pos(1, 3), p)).toBe(false);
    });
    it('compares blocks first', () => {
        expect(caretInComment(pos(1, 0), range(pos(0, 4), pos(2, 1)))).toBe(true);
        expect(caretInComment(pos(2, 1), range(pos(0, 4), pos(2, 1)))).toBe(false);
    });
});

describe('activeCommentId', () => {
    const cs = [
        { id: 1, range: range(pos(0, 0), pos(0, 20)) },
        { id: 2, range: range(pos(0, 5), pos(0, 8)) },
    ];
    it('picks the innermost', () => {
        expect(activeCommentId(range(pos(0, 6), pos(0, 6)), cs)).toBe(2);
        expect(activeCommentId(range(pos(0, 10), pos(0, 10)), cs)).toBe(1);
    });
    it('is null outside every range or without a selection', () => {
        expect(activeCommentId(range(pos(0, 30), pos(0, 30)), cs)).toBeNull();
        expect(activeCommentId(undefined, cs)).toBeNull();
    });
});
