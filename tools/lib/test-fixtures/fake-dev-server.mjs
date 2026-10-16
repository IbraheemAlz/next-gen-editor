/* Test double for `pnpm exec vite --port N --strictPort` (see
 * `../dev-server.test.mjs`): a tiny HTTP server that answers every request
 * with the headers the editor's dev server sends. Usage:
 *   node fake-dev-server.mjs <port> [--exit <code>]
 * `--exit <code>` models a boot failure (e.g. a taken port under
 * `--strictPort`): print a line and exit immediately. */
import { createServer } from 'node:http';

const port = Number.parseInt(process.argv[2] ?? '', 10);
const exitAt = process.argv.indexOf('--exit');
if (exitAt >= 0) {
    console.error(`fake-dev-server: boot failure on port ${port}`);
    process.exit(Number.parseInt(process.argv[exitAt + 1] ?? '1', 10));
}
createServer((_req, res) => {
    res.writeHead(200, {
        'Content-Type': 'text/html',
        'Cross-Origin-Opener-Policy': 'same-origin',
        'Cross-Origin-Embedder-Policy': 'require-corp',
    });
    res.end('<!doctype html><title>fake</title>');
}).listen(port, 'localhost', () => console.log(`fake-dev-server: listening on ${port}`));
process.on('SIGTERM', () => process.exit(0));
