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

/* Issue #438 - Solid ships a no-op SERVER build (`createEffect` never runs)
 * under Node's default conditions. A test that touches reactive code
 * (`createEffect` / `createMemo` / `createRoot` in `clipboard-cache.ts`,
 * `@nge/core`'s `createEditorState`) must resolve Solid's BROWSER build.
 * vitest resolves test imports through Vite's SSR pipeline, so the
 * `browser` condition goes on `ssr.resolve` (plus `externalConditions`),
 * not only on `resolve`, and the package is inlined (vite-node otherwise
 * hands it to Node, which ignores the condition). Pure modules are
 * unaffected. */
const conditions = ['browser', 'development', 'import', 'module', 'default'];
export const solidBrowserVite = {
    resolve: { conditions },
    ssr: { resolve: { conditions, externalConditions: conditions } },
} as const;
export const solidBrowserDeps = { inline: [/solid-js/] } as const;
