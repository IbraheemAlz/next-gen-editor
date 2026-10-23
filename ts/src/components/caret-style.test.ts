import { describe, expect, it } from 'vitest';
import { caretColorCss, restartBlink } from './caret-style';

describe('caretColorCss (issue #557: the caret takes the text colour)', () => {
    it('draws the caret in the colour of the text at it', () => {
        expect(caretColorCss({ r: 192, g: 0, b: 0, a: 255 })).toBe('rgb(192, 0, 0)');
        /* Opaque: a translucent text colour still gives a solid caret. */
        expect(caretColorCss({ r: 0, g: 0, b: 255, a: 128 })).toBe('rgb(0, 0, 255)');
    });

    it('falls back to the stylesheet colour before any selection or for transparent text', () => {
        expect(caretColorCss(undefined)).toBeUndefined();
        expect(caretColorCss(null)).toBeUndefined();
        expect(caretColorCss({ r: 255, g: 255, b: 255, a: 0 })).toBeUndefined();
    });
});

describe('restartBlink (issue #557: every caret move restarts the blink)', () => {
    it('rewinds every running animation of the caret element', () => {
        const anims = [{ currentTime: 812 as number | null }, { currentTime: 40 as number | null }];
        const el = { getAnimations: () => anims } as unknown as Element;
        restartBlink(el);
        expect(anims.map((a) => a.currentTime)).toEqual([0, 0]);
    });

    it('is a no-op without an element or the Web Animations API', () => {
        expect(() => restartBlink(undefined)).not.toThrow();
        expect(() => restartBlink({} as Element)).not.toThrow();
    });
});
