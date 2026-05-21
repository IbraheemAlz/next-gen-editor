import { defineConfig } from 'vite';
import path from 'node:path';
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

export default defineConfig({
    root: '.',
    /* Relative base so the built site works both at the domain root (local
       `pnpm preview`) and under a GitHub Pages subpath
       (https://<user>.github.io/next-gen-editor/). */
    base: './',
    plugins: [solid()],
    server: {
        headers: isolationHeaders,
        port: 5173,
        strictPort: true,
        /**
         * Allow Vite to serve files from the workspace root so the worker
         * can import the wasm-pack output at `../crates/engine-wasm/pkg/`.
         */
        fs: {
            allow: [path.resolve(__dirname, '..')],
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
