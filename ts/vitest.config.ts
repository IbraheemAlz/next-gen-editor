import { defineConfig } from 'vitest/config';
import { sharedTestConfig, solidBrowserDeps, solidBrowserVite } from '../vitest.shared';

export default defineConfig({
    /* Issue #438 - Solid's browser build, so reactive code runs in tests. */
    ...solidBrowserVite,
    test: { ...sharedTestConfig, server: { deps: { inline: [...solidBrowserDeps.inline] } } },
});
