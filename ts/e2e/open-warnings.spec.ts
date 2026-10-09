import { test, expect, type Page } from '@playwright/test';
import { createHash } from 'node:crypto';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { documentXml, storedDocx } from './helpers/docx';

/* Issue #406 — the reader's warning report reaches the user and telemetry
 * through the REAL shell + worker + WASM engine.
 *
 * A document whose `<w:pgMar w:top>` is 99 999 twips (far past Word's
 * 22-inch maximum) and whose `w:right` is `NaN` opens fine — the reader
 * clamps the first (`MeasureClamped`) and ignores the second
 * (`InvalidMeasure`) — but it opened DEGRADED, and before #406 nothing
 * said so: `engine-wasm` dropped `DocxArchive.warnings`. Now the
 * `OPEN_DOCUMENT` reply carries them, `@nge/core` mirrors them as
 * `openWarnings()`, `@nge/ui`'s `OpenWarningsBanner` lists them (with a
 * Dev HUD row), and the telemetry `DOC_OPEN` sample carries per-kind
 * counts (codes only, never the detail). A clean open stays silent.
 */

declare global {
    interface Window {
        __dispatch: (cmd: unknown) => Promise<any>;
        __paintIdle?: boolean;
    }
}

const DEGRADED = storedDocx(
    documentXml(
        '<w:p><w:r><w:t>Margins out of range</w:t></w:r></w:p>' +
            '<w:sectPr><w:pgSz w:w="11906" w:h="16838"/>' +
            '<w:pgMar w:top="99999" w:right="NaN" w:bottom="1440" w:left="1440" ' +
            'w:header="720" w:footer="720"/></w:sectPr>',
    ),
);

const CLEAN = storedDocx(
    documentXml(
        '<w:p><w:r><w:t>Clean</w:t></w:r></w:p>' +
            '<w:sectPr><w:pgSz w:w="11906" w:h="16838"/>' +
            '<w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440" ' +
            'w:header="720" w:footer="720"/></w:sectPr>',
    ),
);

async function open(page: Page, bytes: Uint8Array, name: string): Promise<any> {
    return page.evaluate(
        async ([arr, n]) =>
            window.__dispatch({
                type: 'OPEN_DOCUMENT',
                bytes: new Uint8Array(arr as number[]),
                format: 'docx',
                name: n,
            }),
        [Array.from(bytes), name] as const,
    );
}

async function boot(page: Page, query = ''): Promise<void> {
    await page.goto(`/${query}`);
    await page.waitForFunction(() => window.__paintIdle === true, undefined, {
        timeout: 20_000,
    });
}

test('a degraded open shows "Opened with N issues" listing MeasureClamped; a clean open stays silent', async ({
    page,
}) => {
    test.setTimeout(45_000);
    await boot(page);
    await expect(page.locator('.nge-open-warnings')).toHaveCount(0);

    const reply = await open(page, DEGRADED, 'margins.docx');
    expect(reply.type).toBe('DOCUMENT_LOADED');
    const kinds = (reply.warnings as { kind: string }[]).map((w) => w.kind).sort();
    expect(kinds).toEqual(['InvalidMeasure', 'MeasureClamped']);

    const banner = page.locator('.nge-open-warnings');
    await expect(banner).toBeVisible();
    await expect(banner).toHaveAttribute('role', 'status');
    await expect(banner).toContainText('Opened with 2 issues');
    await expect(banner).toHaveAttribute('data-kinds', /MeasureClamped/);

    /* "Details" expands the list: the clamped margin with its raw value. */
    await banner.locator('.nge-open-warnings__toggle').click();
    const clamped = banner.locator('.nge-open-warnings__item[data-kind="MeasureClamped"]');
    await expect(clamped).toBeVisible();
    await expect(clamped).toContainText('w:pgMar/@w:top = "99999" → 31680 twips');
    await expect(
        banner.locator('.nge-open-warnings__item[data-kind="InvalidMeasure"]'),
    ).toContainText('w:pgMar/@w:right = "NaN"');

    /* The Dev HUD mirrors the report. */
    await page.keyboard.press('Control+Shift+D');
    const row = page.locator('.nge-hud__open-warnings');
    await expect(row).toBeVisible();
    await expect(row).toContainText('2 (');
    await expect(row).toContainText('MeasureClamped ×1');
    await expect(row).toHaveClass(/nge-hud__warn/);

    /* Dismiss hides it for this open... */
    await banner.locator('.nge-open-warnings__dismiss').click();
    await expect(page.locator('.nge-open-warnings')).toHaveCount(0);

    /* ...a new degraded open shows it again... */
    await open(page, DEGRADED, 'margins-again.docx');
    await expect(page.locator('.nge-open-warnings')).toBeVisible();

    /* ...and a clean open clears it (and the HUD row). */
    const clean = await open(page, CLEAN, 'clean.docx');
    expect(clean.type).toBe('DOCUMENT_LOADED');
    expect(clean.warnings).toBeUndefined();
    await expect(page.locator('.nge-open-warnings')).toHaveCount(0);
    await expect(row).toHaveText('none');
});

/* Issue #205 — the sink's port is derived per checkout, exactly like
 * `telemetry.spec.ts` / `playwright.config.ts` (same algorithm + salt +
 * range — change one, change all). */
const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..');
function stablePortFor(seed: string, rangeStart: number, rangeSize: number): number {
    const digest = createHash('sha256').update(seed).digest();
    return rangeStart + (digest.readUInt32BE(0) % rangeSize);
}
const SINK_ORIGIN = `http://localhost:${stablePortFor(`${REPO_ROOT}::telemetry-sink`, 4200, 800)}`;

test('the telemetry DOC_OPEN sample carries per-kind read-warning counts, codes only', async ({
    page,
}) => {
    test.setTimeout(45_000);
    expect((await page.request.post(`${SINK_ORIGIN}/reset`)).ok()).toBe(true);
    await boot(page, `?telemetryEndpoint=${encodeURIComponent(`${SINK_ORIGIN}/telemetry`)}`);
    await page.evaluate(() => (window as any).__setTelemetryEnabled(true));

    const reply = await open(page, DEGRADED, 'margins.docx');
    expect(reply.type).toBe('DOCUMENT_LOADED');
    /* DOC_OPEN is queued once the open's PAINTED broadcast lands. */
    await page.waitForFunction(() => window.__paintIdle === true);

    type DocOpen = { type: string; read_warnings?: { kind: string; count: number }[] };
    const docOpens = async (): Promise<DocOpen[]> => {
        await page.evaluate(() => (window as any).__telemetryFlush());
        const res = await page.request.get(`${SINK_ORIGIN}/received`);
        const batches = (await res.json()) as { events: { kind: DocOpen }[] }[];
        return batches.flatMap((b) => b.events.map((e) => e.kind)).filter((k) => k.type === 'DOC_OPEN');
    };
    await expect.poll(async () => (await docOpens()).length, { timeout: 10_000 }).toBeGreaterThan(0);
    const sample = (await docOpens())[0]!;
    expect(sample.read_warnings).toEqual([
        { kind: 'InvalidMeasure', count: 1 },
        { kind: 'MeasureClamped', count: 1 },
    ]);
    /* Codes and counts only: no detail string (it echoes document bytes).
       Not a bare `99999` substring check: a float like `open_ms:
       67.42999997735023` contains one. */
    for (const w of sample.read_warnings ?? []) {
        expect(Object.keys(w).sort()).toEqual(['count', 'kind']);
    }
    const json = JSON.stringify(sample);
    expect(json).not.toContain('pgMar');
    expect(json).not.toContain('"99999"');
});
