/* Issue #438 - `crashLoopBootPolicy` (#240): what a boot does with the
 * persisted Vello crash-loop streak. */
import { describe, expect, it } from 'vitest';
import { CRASH_LOOP_DECAY_MS, VELLO_TRAP_LIMIT, crashLoopBootPolicy } from './engine-client';
import type { RendererStreak } from './event-log';

const NOW = 1_700_000_000_000;
const streak = (o: Partial<RendererStreak> = {}): RendererStreak => ({
    renderer: 'vello',
    count: 1,
    at: NOW - 1000,
    live: false,
    ...o,
});

describe('crashLoopBootPolicy (#240)', () => {
    it('a boot with no persisted streak starts clean and writes nothing', () => {
        expect(crashLoopBootPolicy(undefined, NOW)).toEqual({
            streak: 0,
            downgrade: undefined,
            record: undefined,
        });
    });

    it('a streak under the limit resumes counting without downgrading', () => {
        const p = crashLoopBootPolicy(streak({ count: VELLO_TRAP_LIMIT - 1 }), NOW);
        expect(p.streak).toBe(VELLO_TRAP_LIMIT - 1);
        expect(p.downgrade).toBeUndefined();
        /* A non-live record is left as is. */
        expect(p.record).toBeUndefined();
    });

    it('a streak at the limit boots on Canvas2D without probing the GPU', () => {
        const p = crashLoopBootPolicy(streak({ count: VELLO_TRAP_LIMIT }), NOW);
        expect(p.downgrade).toEqual({
            from: 'vello',
            to: 'canvas2d',
            reason: 'CRASH_LOOP',
            consecutive_traps: VELLO_TRAP_LIMIT,
        });
    });

    it('a streak older than 24 h is dropped (a driver update deserves a new try)', () => {
        const p = crashLoopBootPolicy(
            streak({ count: 9, at: NOW - CRASH_LOOP_DECAY_MS - 1 }),
            NOW,
        );
        expect(p).toEqual({ streak: 0, downgrade: undefined, record: null });
    });

    it('exactly 24 h old still counts; a record from the far future (clock skew) does not', () => {
        expect(
            crashLoopBootPolicy(streak({ count: 2, at: NOW - CRASH_LOOP_DECAY_MS }), NOW).streak,
        ).toBe(2);
        expect(
            crashLoopBootPolicy(streak({ count: 2, at: NOW + CRASH_LOOP_DECAY_MS + 1 }), NOW),
        ).toEqual({ streak: 0, downgrade: undefined, record: null });
    });

    it('a streak on another renderer is not a Vello crash loop', () => {
        expect(crashLoopBootPolicy(streak({ renderer: 'canvas2d', count: 5 }), NOW)).toEqual({
            streak: 0,
            downgrade: undefined,
            record: null,
        });
    });

    describe('a generation still marked live at the next boot', () => {
        it('with no clean-exit token died with its tab: one more failure, record folded to not-live', () => {
            const p = crashLoopBootPolicy(streak({ count: 1, live: true, token: 'g1' }), NOW);
            expect(p.streak).toBe(2);
            expect(p.downgrade?.consecutive_traps).toBe(2);
            expect(p.record).toEqual({ renderer: 'vello', count: 2, at: NOW, live: false });
        });

        it('with a record that has no token at all also counts as dead', () => {
            expect(crashLoopBootPolicy(streak({ count: 1, live: true }), NOW, 'anything').streak).toBe(2);
        });

        it('a clean-exit token for exactly that generation un-counts it', () => {
            const p = crashLoopBootPolicy(streak({ count: 1, live: true, token: 'g1' }), NOW, 'g1');
            expect(p.streak).toBe(1);
            expect(p.downgrade).toBeUndefined();
            expect(p.record).toEqual({ renderer: 'vello', count: 1, at: NOW, live: false });
        });

        it('a clean-exit token for a DIFFERENT generation does not', () => {
            expect(
                crashLoopBootPolicy(streak({ count: 1, live: true, token: 'g2' }), NOW, 'g1').streak,
            ).toBe(2);
        });
    });
});
