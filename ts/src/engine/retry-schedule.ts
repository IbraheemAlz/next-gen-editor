/* Issue #333 / #390 - the bounded retry clock the worker uses for failed
 * event-log writes (snapshot checkpoints and the command journal alike).
 * Pulled out of `engine.worker.ts` (which imports the wasm engine and so
 * cannot be loaded by a unit test) so the schedule is one tested constant
 * with one pure decision function. */

/** Delay before retry N (1-based failure count N maps to index N-1). */
export const SNAPSHOT_RETRY_DELAYS_MS: readonly number[] = [2000, 4000, 8000];

export type RetryDecision =
    | { retry: true; delayMs: number }
    /** Retries exhausted: stop retrying and warn the user. */
    | { retry: false };

/** What to do after the `failures`-th consecutive failure (1-based). */
export function nextRetry(failures: number): RetryDecision {
    const delayMs = SNAPSHOT_RETRY_DELAYS_MS[failures - 1];
    return delayMs === undefined ? { retry: false } : { retry: true, delayMs };
}
