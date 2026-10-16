import { describe, expect, it } from 'vitest';
import {
    ScrollIntentLedger,
    decideSync,
    decideVerify,
    isScrollbarPress,
    type RevealRequest,
    type ScrollerHit,
} from './revealIntent';

/* Issue #486 - the DOM-free decisions behind createCaretReveal: a reveal is
   a consequence of an earlier input and must never undo a later user scroll. */

const req = (sentAt: number, eventAt: number): RevealRequest => ({ sentAt, eventAt });

describe('ScrollIntentLedger (issue #486)', () => {
    it('supersedes nothing while no scroll intent has been seen', () => {
        const l = new ScrollIntentLedger();
        expect(l.supersedes(req(10, 12))).toBe(false);
    });

    it('a wheel after the command was sent supersedes it, even before the reply arrived', () => {
        const l = new ScrollIntentLedger();
        /* key sent at 10, wheel at 11, reply (the selection event) at 14 */
        l.noteIntent(11);
        expect(l.supersedes(req(10, 14))).toBe(true);
    });

    it('a wheel BEFORE the command was sent does not', () => {
        const l = new ScrollIntentLedger();
        l.noteIntent(5);
        expect(l.supersedes(req(10, 14))).toBe(false);
    });

    it('a wheel in the same tick as the send counts as after it (the burst case)', () => {
        const l = new ScrollIntentLedger();
        l.noteIntent(10);
        expect(l.supersedes(req(10, 14))).toBe(true);
    });

    it('a foreign scroll event supersedes only a reveal whose reply already arrived', () => {
        const l = new ScrollIntentLedger();
        /* the event is delivered a frame late: it cannot be attributed to
           before the command was sent, only to after the reply */
        expect(l.noteScroll(900, 0, 11)).toBe(true);
        expect(l.supersedes(req(10, 14))).toBe(false);
        expect(l.noteScroll(900, 0, 15)).toBe(true);
        expect(l.supersedes(req(10, 14))).toBe(true);
    });

    it('the echo of the reveal`s own write is not intent', () => {
        const l = new ScrollIntentLedger();
        l.noteOwnWrite(520, 0);
        expect(l.noteScroll(520, 0, 20)).toBe(false);
        expect(l.supersedes(req(10, 14))).toBe(false);
    });

    it('an echo within the device-pixel rounding still matches', () => {
        const l = new ScrollIntentLedger();
        l.noteOwnWrite(520, 0);
        expect(l.noteScroll(520.4, 0, 20)).toBe(false);
    });

    it('the own-write mark is consumed by its echo', () => {
        const l = new ScrollIntentLedger();
        l.noteOwnWrite(520, 0);
        expect(l.noteScroll(520, 0, 20)).toBe(false);
        /* a later scroll to the very same offset is the user's */
        expect(l.noteScroll(520, 0, 40)).toBe(true);
    });

    it('a scroll event at a different offset than the own write is intent (the user moved it on)', () => {
        const l = new ScrollIntentLedger();
        l.noteOwnWrite(520, 0);
        expect(l.noteScroll(900, 0, 20)).toBe(true);
        expect(l.supersedes(req(10, 14))).toBe(true);
    });

    it('a wheel forgets the pending own-write mark', () => {
        const l = new ScrollIntentLedger();
        l.noteOwnWrite(520, 0);
        l.noteIntent(30);
        /* coalesced scroll event at the same offset: after a wheel it is the user's */
        expect(l.noteScroll(520, 0, 31)).toBe(true);
    });

    it('a horizontal difference alone is intent', () => {
        const l = new ScrollIntentLedger();
        l.noteOwnWrite(520, 0);
        expect(l.noteScroll(520, 300, 20)).toBe(true);
    });
});

describe('decideSync / decideVerify (issue #486)', () => {
    it('reveals in the same task when the caret can be placed and nothing superseded it', () => {
        expect(decideSync(new ScrollIntentLedger(), req(10, 12), true)).toBe('now');
    });

    it('defers to the frame pass when the caret cannot be placed yet', () => {
        expect(decideSync(new ScrollIntentLedger(), req(10, 12), false)).toBe('defer');
    });

    it('drops the reveal when the user scrolled after the command was sent', () => {
        const l = new ScrollIntentLedger();
        l.noteIntent(11);
        expect(decideSync(l, req(10, 12), true)).toBe('superseded');
    });

    it('keyboard navigation is unaffected by an old scroll: a wheel before the keypress', () => {
        const l = new ScrollIntentLedger();
        l.noteIntent(100);
        expect(decideSync(l, req(200, 203), true)).toBe('now');
        expect(decideVerify(l, req(200, 203), true)).toBe('reveal');
    });

    it('the frame pass skips when a foreign scroll event landed between the reply and the frame', () => {
        const l = new ScrollIntentLedger();
        const r = req(10, 12);
        expect(decideSync(l, r, true)).toBe('now');
        /* the spec / user set scrollTop right after the reply */
        l.noteScroll(900, 0, 13);
        expect(decideVerify(l, r, true)).toBe('skip');
    });

    it('the frame pass reveals when the layout settled late and nothing was scrolled', () => {
        const l = new ScrollIntentLedger();
        const r = req(10, 12);
        expect(decideSync(l, r, false)).toBe('defer');
        expect(decideVerify(l, r, true)).toBe('reveal');
    });

    it('the frame pass has nothing to do while the caret still cannot be placed', () => {
        expect(decideVerify(new ScrollIntentLedger(), req(10, 12), false)).toBe('skip');
    });

    it('the echo of the sync reveal`s own write does not cancel the verification', () => {
        const l = new ScrollIntentLedger();
        const r = req(10, 12);
        l.noteOwnWrite(520, 0);
        l.noteScroll(520, 0, 13);
        expect(decideVerify(l, r, true)).toBe('reveal');
    });
});

describe('isScrollbarPress (issue #486)', () => {
    /* a 800 x 600 content box with a 15 px vertical scrollbar on the right
       and a 1 px border: border box 818 x 602 at (100, 50) */
    const hit = (clientX: number, clientY: number): ScrollerHit => ({
        clientX,
        clientY,
        rect: { left: 100, top: 50, right: 918, bottom: 652 },
        clientLeft: 1,
        clientTop: 1,
        clientWidth: 800,
        clientHeight: 600,
    });

    it('a press on the content is not a scrollbar press', () => {
        expect(isScrollbarPress(hit(500, 300))).toBe(false);
    });

    it('a press on the vertical scrollbar is', () => {
        expect(isScrollbarPress(hit(910, 300))).toBe(true);
    });

    it('a press on a horizontal scrollbar below the content is', () => {
        const h = hit(500, 645);
        expect(isScrollbarPress({ ...h, rect: { ...h.rect, bottom: 652 }, clientHeight: 585 })).toBe(
            true,
        );
    });

    it('a press outside the scroller entirely is not', () => {
        expect(isScrollbarPress(hit(50, 300))).toBe(false);
        expect(isScrollbarPress(hit(500, 700))).toBe(false);
    });
});
