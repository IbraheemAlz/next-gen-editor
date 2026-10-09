/* Issue #332 - the #333 / #390 bounded retry clock. */
import { describe, expect, it } from 'vitest';
import { nextRetry, SNAPSHOT_RETRY_DELAYS_MS } from './retry-schedule';

describe('retry schedule (#333 / #390)', () => {
    it('backs off 2 s, 4 s, 8 s', () => {
        expect(SNAPSHOT_RETRY_DELAYS_MS).toEqual([2000, 4000, 8000]);
        expect(nextRetry(1)).toEqual({ retry: true, delayMs: 2000 });
        expect(nextRetry(2)).toEqual({ retry: true, delayMs: 4000 });
        expect(nextRetry(3)).toEqual({ retry: true, delayMs: 8000 });
    });

    it('is exhausted after the last delay failed too (the 4th failure raises the warning)', () => {
        expect(nextRetry(4)).toEqual({ retry: false });
        expect(nextRetry(99)).toEqual({ retry: false });
    });

    it('a failure count of zero (or below) never retries', () => {
        expect(nextRetry(0)).toEqual({ retry: false });
        expect(nextRetry(-1)).toEqual({ retry: false });
    });

    it('delays strictly increase (bounded exponential backoff)', () => {
        for (let i = 1; i < SNAPSHOT_RETRY_DELAYS_MS.length; i++) {
            expect(SNAPSHOT_RETRY_DELAYS_MS[i]!).toBeGreaterThan(SNAPSHOT_RETRY_DELAYS_MS[i - 1]!);
        }
    });
});
