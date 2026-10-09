/* Issue #438 - the vitest config resolves Solid's BROWSER build: on the
 * server build `createEffect` is a no-op and reactive code is untestable. */
import { createEffect, createRoot, createSignal } from 'solid-js';
import { describe, expect, it } from 'vitest';

describe('solid resolves its browser build under vitest', () => {
    it('runs effects and re-runs them when a signal changes', () => {
        const seen: number[] = [];
        let setN!: (n: number) => void;
        const dispose = createRoot((d) => {
            const [n, set] = createSignal(1);
            setN = set;
            createEffect(() => seen.push(n()));
            return d;
        });
        expect(seen).toEqual([1]);
        setN(2);
        expect(seen).toEqual([1, 2]);
        dispose();
    });
});
