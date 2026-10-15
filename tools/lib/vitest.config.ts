import { defineConfig } from 'vitest/config';
import { sharedTestConfig } from '../../vitest.shared';

/* Issue #470 - the shared tool helpers are plain `.mjs` modules with
 * `*.test.mjs` tests beside them (no DOM, no Solid). */
export default defineConfig({
    test: { ...sharedTestConfig, include: ['*.test.mjs'] },
});
