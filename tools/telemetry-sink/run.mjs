#!/usr/bin/env node
/**
 * telemetry-sink — Issue #86, D5.7 real telemetry.
 *
 * A tiny Node HTTP receiver for dev + e2e: accepts a POSTed
 * `TelemetryBatch` (the shape `crates/bridge/src/telemetry.rs` defines and
 * `ts/src/state/telemetry.ts`'s `BeaconTransport` sends) and appends it as
 * one JSON line to an on-disk JSONL file. Zero external dependencies —
 * Node's built-in `http` only, matching this tool's "tiny" brief.
 *
 * This is NOT a production collector (see the issue's "Out of scope: a
 * production backend / dashboards") — no auth, no retention policy, no
 * schema validation beyond "is it JSON". It exists so:
 *   - a developer can point `?telemetryEndpoint=http://localhost:4319/telemetry`
 *     at a running editor and watch real batches land in a file, and
 *   - `ts/e2e/telemetry.spec.ts` has something real to assert against
 *     (a forced-trap CRASH sample "reaching the sink" needs a sink).
 *
 * Usage:
 *   node run.mjs                      — port 4319, ./telemetry.jsonl
 *   node run.mjs --port 4500 --out /tmp/x.jsonl
 *
 * Endpoints:
 *   POST /telemetry  — ingest one TelemetryBatch (JSON body; `sendBeacon`'s
 *                      Blob body and a plain `fetch` JSON body both work).
 *   GET  /received    — JSON array of every batch received since boot (or
 *                      the last /reset) — lets a test assert without
 *                      re-reading the JSONL file from disk.
 *   POST /reset        — clear the in-memory `/received` list. The on-disk
 *                      JSONL file is append-only and untouched by this.
 *   GET  /health       — 200 "ok", for readiness polling.
 */
import { createServer } from 'node:http';
import { appendFile, mkdir } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';

const argv = process.argv.slice(2);
const argValue = (flag, fallback) => {
    const i = argv.indexOf(flag);
    return i >= 0 && i + 1 < argv.length ? argv[i + 1] : fallback;
};

const port = Number.parseInt(argValue('--port', '4319'), 10);
const outPath = resolve(argValue('--out', './telemetry.jsonl'));
const ingestPath = argValue('--path', '/telemetry');

/** In-memory mirror of every batch received since boot/reset — cheap and
 *  avoids every e2e assertion needing filesystem access. */
let received = [];

async function readBody(req) {
    const chunks = [];
    for await (const chunk of req) chunks.push(chunk);
    return Buffer.concat(chunks).toString('utf8');
}

function json(res, status, body) {
    const text = JSON.stringify(body);
    res.writeHead(status, {
        'Content-Type': 'application/json',
        'Content-Length': Buffer.byteLength(text),
    });
    res.end(text);
}

const server = createServer((req, res) => {
    /* The editor dev server (Vite, :5173) and this sink run on different
       origins. `sendBeacon` never lets JS read the response either way, but
       a non-"simple" Content-Type (`application/json`) still triggers a CORS
       preflight, and Chrome's `sendBeacon` sends it with credentials mode
       `include` — which the CORS spec forbids pairing with a wildcard
       `Access-Control-Allow-Origin`. Echo the request's own Origin instead
       (this sink has no auth / cookies to protect either way) and mark
       credentials allowed, so ingestion never silently fails under COEP. */
    const origin = req.headers.origin;
    if (origin) res.setHeader('Access-Control-Allow-Origin', origin);
    res.setHeader('Access-Control-Allow-Credentials', 'true');
    res.setHeader('Vary', 'Origin');
    res.setHeader('Access-Control-Allow-Methods', 'POST, GET, OPTIONS');
    res.setHeader('Access-Control-Allow-Headers', 'Content-Type');
    if (req.method === 'OPTIONS') {
        res.writeHead(204);
        res.end();
        return;
    }

    const url = new URL(req.url ?? '/', `http://localhost:${port}`);

    if (req.method === 'GET' && url.pathname === '/health') {
        res.writeHead(200, { 'Content-Type': 'text/plain' });
        res.end('ok');
        return;
    }

    if (req.method === 'GET' && url.pathname === '/received') {
        json(res, 200, received);
        return;
    }

    if (req.method === 'POST' && url.pathname === '/reset') {
        received = [];
        json(res, 200, { ok: true });
        return;
    }

    if (req.method === 'POST' && url.pathname === ingestPath) {
        void (async () => {
            try {
                const raw = await readBody(req);
                const batch = raw.length ? JSON.parse(raw) : null;
                if (batch && typeof batch === 'object') {
                    received.push(batch);
                    await mkdir(dirname(outPath), { recursive: true });
                    await appendFile(outPath, `${JSON.stringify(batch)}\n`, 'utf8');
                }
                // `sendBeacon` ignores the response, but a plain `fetch`
                // caller (dev / curl testing) gets a real ack.
                json(res, 204, {});
            } catch (e) {
                console.error('[telemetry-sink] failed to ingest batch:', e);
                res.writeHead(400);
                res.end();
            }
        })();
        return;
    }

    res.writeHead(404);
    res.end();
});

server.listen(port, () => {
    console.log(`[telemetry-sink] listening on http://localhost:${port}${ingestPath}`);
    console.log(`[telemetry-sink] writing JSONL to ${outPath}`);
});
