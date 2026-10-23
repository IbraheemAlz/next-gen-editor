/* Issues #554 / #506 - the keyboard with a picture object-selected. */
import { describe, expect, it, vi } from 'vitest';
import type { Command, Event } from '../engine/types';
import {
    caretAtEdge,
    isModifierOnly,
    planImageKey,
    runImageKey,
    storyMatches,
    type ImageKeyClient,
    type KeyedImage,
    type KeyInfo,
} from './image-keys';

const key = (k: string, mods: Partial<KeyInfo> = {}): KeyInfo => ({
    key: k,
    shiftKey: false,
    ctrlKey: false,
    metaKey: false,
    altKey: false,
    ...mods,
});

const LTR = { rtl: false };
const RTL = { rtl: true };

describe('planImageKey (#554)', () => {
    it('Delete and Backspace remove the picture', () => {
        expect(planImageKey(key('Delete'), LTR)).toEqual({ kind: 'delete' });
        expect(planImageKey(key('Backspace'), LTR)).toEqual({ kind: 'delete' });
        expect(planImageKey(key('Backspace', { ctrlKey: true }), LTR)).toEqual({ kind: 'delete' });
        expect(planImageKey(key('Delete', { altKey: true }), LTR)).toEqual({ kind: 'keep' });
    });

    it('Left / Right collapse to the visual edge in the direction of travel, no further motion', () => {
        expect(planImageKey(key('ArrowLeft'), LTR)).toEqual({
            kind: 'caret',
            edge: 'before',
            consume: true,
        });
        expect(planImageKey(key('ArrowRight'), LTR)).toEqual({
            kind: 'caret',
            edge: 'after',
            consume: true,
        });
    });

    it('in an RTL paragraph the visual left edge is the byte AFTER the picture', () => {
        expect(planImageKey(key('ArrowLeft'), RTL)).toEqual({
            kind: 'caret',
            edge: 'after',
            consume: true,
        });
        expect(planImageKey(key('ArrowRight'), RTL)).toEqual({
            kind: 'caret',
            edge: 'before',
            consume: true,
        });
    });

    it('Up goes from before the picture, Down from after it, then the motion applies', () => {
        for (const rtl of [false, true]) {
            expect(planImageKey(key('ArrowUp'), { rtl })).toEqual({
                kind: 'caret',
                edge: 'before',
                motion: { direction: 'Up', extend: false },
                consume: true,
            });
            expect(planImageKey(key('ArrowDown'), { rtl })).toEqual({
                kind: 'caret',
                edge: 'after',
                motion: { direction: 'Down', extend: false },
                consume: true,
            });
        }
    });

    it('Shift extends across the picture from the edge behind the travel', () => {
        expect(planImageKey(key('ArrowRight', { shiftKey: true }), LTR)).toEqual({
            kind: 'caret',
            edge: 'before',
            motion: { direction: 'Right', extend: true },
            consume: true,
        });
        expect(planImageKey(key('ArrowLeft', { shiftKey: true }), LTR)).toEqual({
            kind: 'caret',
            edge: 'after',
            motion: { direction: 'Left', extend: true },
            consume: true,
        });
        /* RTL swaps the edges but not the motion. */
        expect(planImageKey(key('ArrowRight', { shiftKey: true }), RTL)).toEqual({
            kind: 'caret',
            edge: 'after',
            motion: { direction: 'Right', extend: true },
            consume: true,
        });
        expect(planImageKey(key('ArrowDown', { shiftKey: true }), LTR)).toMatchObject({
            motion: { direction: 'Down', extend: true },
        });
    });

    it('Ctrl+Left / Ctrl+Right jump a word away from the picture', () => {
        expect(planImageKey(key('ArrowLeft', { ctrlKey: true }), LTR)).toEqual({
            kind: 'caret',
            edge: 'before',
            motion: { direction: 'WordLeft', extend: false },
            consume: true,
        });
        expect(planImageKey(key('ArrowRight', { metaKey: true }), LTR)).toEqual({
            kind: 'caret',
            edge: 'after',
            motion: { direction: 'WordRight', extend: false },
            consume: true,
        });
    });

    it('Escape leaves the selection with the caret before the picture and stops there', () => {
        expect(planImageKey(key('Escape'), LTR)).toEqual({
            kind: 'caret',
            edge: 'before',
            consume: true,
        });
    });

    it('typing, Enter, Tab, Home / End and IME keys put the caret before the picture and go on', () => {
        for (const k of ['a', 'A', 'ي', 'Enter', 'Tab', 'Home', 'End', 'PageDown', 'Process', 'Dead']) {
            expect(planImageKey(key(k, { shiftKey: k === 'A' }), LTR), k).toEqual({
                kind: 'caret',
                edge: 'before',
                consume: false,
            });
        }
    });

    it('AltGr (Ctrl+Alt) types a character, so it is typing', () => {
        expect(planImageKey(key('@', { ctrlKey: true, altKey: true }), LTR)).toMatchObject({
            kind: 'caret',
            consume: false,
        });
    });

    it('Ctrl+V pastes at the caret, so the caret goes before the picture first', () => {
        expect(planImageKey({ ...key('v', { ctrlKey: true }), code: 'KeyV' }, LTR)).toEqual({
            kind: 'caret',
            edge: 'before',
            consume: false,
        });
    });

    it('other chords and function keys are not the picture\'s: the selection just drops', () => {
        expect(planImageKey({ ...key('z', { ctrlKey: true }), code: 'KeyZ' }, LTR)).toEqual({ kind: 'keep' });
        expect(planImageKey(key('b', { metaKey: true }), LTR)).toEqual({ kind: 'keep' });
        expect(planImageKey(key('F9'), LTR)).toEqual({ kind: 'keep' });
        expect(planImageKey(key('ArrowLeft', { altKey: true }), LTR)).toEqual({ kind: 'keep' });
    });

    it('a bare modifier press changes nothing (it must not drop the selection)', () => {
        for (const k of ['Shift', 'Control', 'Alt', 'Meta', 'AltGraph']) {
            expect(isModifierOnly(k)).toBe(true);
            expect(planImageKey(key(k), LTR)).toEqual({ kind: 'ignore' });
        }
        expect(isModifierOnly('a')).toBe(false);
    });
});

const body = (extra: Partial<KeyedImage> = {}): KeyedImage => ({
    path: { steps: [{ kind: 'BLOCK', idx: 2 }] },
    at: 5,
    story: [],
    storyRid: '',
    rtl: false,
    ...extra,
});

const boxed = body({
    path: { steps: [{ kind: 'BLOCK', idx: 0 }] },
    story: [{ path: { steps: [{ kind: 'BLOCK', idx: 1 }] }, at: 0 }],
    storyRid: '1@0',
});

describe('caretAtEdge', () => {
    it('before = the sentinel offset, after = past its 3 bytes', () => {
        const pos = (offset: number) => ({ path: body().path, offset });
        expect(caretAtEdge(body(), 'before')).toEqual({
            type: 'SET_SELECTION',
            range: { start: pos(5), end: pos(5) },
            caret: pos(5),
        });
        expect(caretAtEdge(body(), 'after')).toMatchObject({ caret: pos(8) });
    });
});

describe('storyMatches', () => {
    it('a body picture needs the body, a box picture needs ITS box', () => {
        expect(storyMatches(body(), undefined)).toBe(true);
        expect(storyMatches(body(), { area: 'TextBox', rid: '1@0' })).toBe(false);
        expect(storyMatches(boxed, { area: 'TextBox', rid: '1@0' })).toBe(true);
        expect(storyMatches(boxed, { area: 'TextBox', rid: '2@0' })).toBe(false);
        expect(storyMatches(boxed, undefined)).toBe(false);
        expect(storyMatches(boxed, { area: 'Header', rid: '1@0' })).toBe(false);
    });
});

function fakeClient(replies: Event[] = []): ImageKeyClient & { sent: Command[] } {
    const sent: Command[] = [];
    return {
        sent,
        dispatch: vi.fn(async (cmd: Command) => {
            sent.push(cmd);
            return replies.shift() ?? ({ type: 'PONG' } as Event);
        }),
    };
}

describe('runImageKey (#554)', () => {
    it('Delete on a body picture is ONE DELETE_RANGE over its sentinel, and the key is consumed', () => {
        const c = fakeClient();
        const plan = planImageKey(key('Delete'), LTR);
        expect(runImageKey(plan, body(), c, undefined)).toBe('consumed');
        expect(c.sent).toEqual([
            {
                type: 'DELETE_RANGE',
                range: {
                    start: { path: body().path, offset: 5 },
                    end: { path: body().path, offset: 8 },
                },
            },
        ]);
    });

    it('Left posts only the caret placement; Up posts the placement THEN the motion', () => {
        const left = fakeClient();
        expect(runImageKey(planImageKey(key('ArrowLeft'), LTR), body(), left, undefined)).toBe('consumed');
        expect(left.sent.map((c) => c.type)).toEqual(['SET_SELECTION']);

        const up = fakeClient();
        expect(runImageKey(planImageKey(key('ArrowUp'), LTR), body(), up, undefined)).toBe('consumed');
        expect(up.sent.map((c) => c.type)).toEqual(['SET_SELECTION', 'MOVE_CARET']);
        expect(up.sent[1]).toEqual({ type: 'MOVE_CARET', direction: 'Up', extend: false });
    });

    it('typing places the caret before the picture and lets the key go on', () => {
        const c = fakeClient();
        expect(runImageKey(planImageKey(key('x'), LTR), body(), c, undefined)).toBe('continue');
        expect(c.sent).toEqual([caretAtEdge(body(), 'before')]);
    });

    it('a chord the picture does not own posts nothing', () => {
        const c = fakeClient();
        const plan = planImageKey({ ...key('z', { ctrlKey: true }), code: 'KeyZ' }, LTR);
        expect(runImageKey(plan, body(), c, undefined)).toBe('continue');
        expect(c.sent).toEqual([]);
    });

    it('a box picture selected from outside its box cannot be addressed: Delete is swallowed, never continued', () => {
        const c = fakeClient();
        expect(runImageKey(planImageKey(key('Delete'), LTR), boxed, c, undefined)).toBe('consumed');
        expect(c.sent).toEqual([]);
    });

    it('a box picture\'s arrows fall through when its box is not the active story', () => {
        const c = fakeClient();
        expect(runImageKey(planImageKey(key('ArrowLeft'), LTR), boxed, c, undefined)).toBe('continue');
        expect(c.sent).toEqual([]);
    });

    it('Delete on a box picture selects the sentinel in its story, then deletes the selection', async () => {
        const sel = {
            type: 'SELECTION_CHANGED',
            range: {
                start: { path: boxed.path, offset: 5 },
                end: { path: boxed.path, offset: 8 },
            },
        } as unknown as Event;
        const c = fakeClient([sel]);
        const active = { area: 'TextBox', rid: '1@0' };
        expect(runImageKey(planImageKey(key('Delete'), LTR), boxed, c, active)).toBe('consumed');
        await vi.waitFor(() => expect(c.sent.map((x) => x.type)).toEqual(['SET_SELECTION', 'DELETE_AT_CARET']));
    });

    it('never deletes at a stale caret: if the engine did not select exactly the picture, nothing is deleted', async () => {
        const wrong = {
            type: 'SELECTION_CHANGED',
            range: {
                start: { path: boxed.path, offset: 0 },
                end: { path: boxed.path, offset: 0 },
            },
        } as unknown as Event;
        const c = fakeClient([wrong]);
        runImageKey(planImageKey(key('Backspace'), LTR), boxed, c, { area: 'TextBox', rid: '1@0' });
        await new Promise((r) => setTimeout(r, 10));
        expect(c.sent.map((x) => x.type)).toEqual(['SET_SELECTION']);
    });

    it('a rejected dispatch is reported, not thrown', async () => {
        const err = vi.fn();
        const c: ImageKeyClient = { dispatch: () => Promise.reject(new Error('boom')) };
        runImageKey(planImageKey(key('Delete'), LTR), body(), c, undefined, err);
        await vi.waitFor(() => expect(err).toHaveBeenCalledWith('delete image', expect.any(Error)));
    });
});
