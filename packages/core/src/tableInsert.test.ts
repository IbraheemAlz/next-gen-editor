import { describe, expect, it } from 'vitest';
import { tableInsertPosition } from './tableInsert';
import type { BlockPath, PathStep } from './types';

const B = (idx: number): PathStep => ({ kind: 'BLOCK', idx });
const C = (row: number, col: number): PathStep => ({ kind: 'CELL', row, col });
const path = (...steps: PathStep[]): BlockPath => ({ steps });

describe('tableInsertPosition (issue #563)', () => {
    it('puts the table after a top-level paragraph', () => {
        expect(tableInsertPosition(path(B(0)))).toEqual(path(B(1)));
        expect(tableInsertPosition(path(B(4)))).toEqual(path(B(5)));
    });

    it('nests the table in the cell the caret is in, after its paragraph', () => {
        expect(tableInsertPosition(path(B(1), C(1, 2), B(0)))).toEqual(path(B(1), C(1, 2), B(1)));
    });

    it('nests at any depth (a caret inside a nested table)', () => {
        expect(tableInsertPosition(path(B(1), C(0, 0), B(2), C(1, 1), B(3)))).toEqual(
            path(B(1), C(0, 0), B(2), C(1, 1), B(4)),
        );
    });

    it('appends to the body for a path that names no paragraph', () => {
        expect(tableInsertPosition(path())).toEqual(path());
        expect(tableInsertPosition(path(B(1), C(0, 0)))).toEqual(path());
    });
});
