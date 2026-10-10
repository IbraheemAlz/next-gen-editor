import { describe, expect, it } from 'vitest';
import { errorCommand } from './errorCommand';

describe('errorCommand (#469)', () => {
    it('reads the command field when the engine sent it', () => {
        expect(errorCommand({ message: 'no prefix at all', command: 'APPLY_FORMATTING' })).toBe(
            'APPLY_FORMATTING',
        );
    });
    it('prefers the field over a disagreeing prefix', () => {
        expect(errorCommand({ message: 'DeleteRange: x', command: 'SET_ZOOM' })).toBe('SET_ZOOM');
    });
    it('falls back to the legacy prefix, in wire spelling', () => {
        expect(errorCommand({ message: 'ApplyFormatting: attrs.font_size is NaN' })).toBe(
            'APPLY_FORMATTING',
        );
        expect(errorCommand({ message: 'HitTestInPage: bad' })).toBe('HIT_TEST_IN_PAGE');
    });
    it('is undefined with neither', () => {
        expect(errorCommand({ message: 'something went wrong' })).toBeUndefined();
        expect(errorCommand({ message: 'x', command: '' })).toBeUndefined();
    });
});
