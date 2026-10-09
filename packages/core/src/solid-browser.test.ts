/* Issue #438 - `packages/core` tests that touch Solid resolve its BROWSER
 * build (vitest.shared.ts `solidBrowserVite`): on the server build effects
 * never run and the signals `createEditorState` is made of cannot be tested. */
import { createEffect, createMemo, createRoot, createSignal } from 'solid-js';
import { describe, expect, it } from 'vitest';

describe('solid browser build under vitest (packages/core)', () => {
    it('runs effects and memos reactively', () => {
        const seen: number[] = [];
        let set!: (n: number) => void;
        const dispose = createRoot((d) => {
            const [n, setN] = createSignal(1);
            set = setN;
            const double = createMemo(() => n() * 2);
            createEffect(() => seen.push(double()));
            return d;
        });
        set(5);
        expect(seen).toEqual([2, 10]);
        dispose();
    });
});
