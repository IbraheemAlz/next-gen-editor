import { defineConfig, type Plugin } from 'vite';
import path from 'node:path';
import { execFileSync } from 'node:child_process';
import solid from 'vite-plugin-solid';

/**
 * Cross-origin isolation headers — required for SharedArrayBuffer + WASM
 * threading in later phases. Applied to dev + preview.
 */
const isolationHeaders = {
    'Cross-Origin-Opener-Policy': 'same-origin',
    'Cross-Origin-Embedder-Policy': 'require-corp',
    'Cross-Origin-Resource-Policy': 'same-origin',
};

const REPO_ROOT = path.resolve(__dirname, '..');

/**
 * Issue #205 — parallel-worktree port trap. Several worktrees of this repo
 * (plus the main checkout) can each run `pnpm dev`; when a Playwright run
 * (or a developer) hits `localhost:<port>`, it has no way to tell *which*
 * checkout answered short of this line. Printing the served absolute path
 * + short commit SHA on every boot makes a mismatch visible at a glance
 * instead of specs silently exercising the wrong tree.
 */
function printCheckoutBanner(): Plugin {
    return {
        name: 'print-checkout-banner',
        configureServer(server) {
            server.httpServer?.once('listening', () => {
                let commit = 'unknown';
                try {
                    commit = execFileSync('git', ['rev-parse', '--short', 'HEAD'], {
                        cwd: REPO_ROOT,
                        stdio: ['ignore', 'pipe', 'ignore'],
                    })
                        .toString()
                        .trim();
                } catch {
                    /* not a git checkout, or git unavailable — non-fatal, just label it unknown */
                }
                console.log(`  ➔  serving ${REPO_ROOT} @ ${commit}`);
            });
        },
    };
}

export default defineConfig({
    root: '.',
    /* Relative base so the built site works both at the domain root (local
       `pnpm preview`) and under a GitHub Pages subpath
       (https://<user>.github.io/next-gen-editor/). */
    base: './',
    plugins: [solid(), printCheckoutBanner()],
    server: {
        headers: isolationHeaders,
        port: 5173,
        strictPort: true,
        /**
         * Allow Vite to serve files from the workspace root so the worker
         * can import the wasm-pack output at `../crates/engine-wasm/pkg/`.
         */
        fs: {
            allow: [REPO_ROOT],
        },
    },
    preview: {
        headers: isolationHeaders,
        port: 4173,
        strictPort: true,
    },
    worker: {
        format: 'es',
    },
    build: {
        target: 'es2024',
        sourcemap: true,
    },
});
