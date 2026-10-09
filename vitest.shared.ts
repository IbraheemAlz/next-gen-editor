/* Issue #332 - the unit-test harness shared by `ts/`, `packages/core` and
 * `packages/ui`. Node environment on purpose (no jsdom / happy-dom): the
 * modules under test must not touch the DOM. IndexedDB comes from
 * `fake-indexeddb/auto` where a test needs it; anything else a module
 * reads off `globalThis` (`Worker`, `location`, ...) is stubbed in the
 * test, not globally, so a stray DOM dependency fails loudly. */
export const sharedTestConfig = {
    environment: 'node',
    include: ['src/**/*.test.ts'],
    passWithNoTests: true,
    restoreMocks: true,
    unstubEnvs: true,
    unstubGlobals: true,
} as const;
