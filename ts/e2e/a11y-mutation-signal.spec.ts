import { test, expect } from '@playwright/test';

/* Issue #194 — the worker broadcasts an `ACCESSIBILITY_TREE_DELTA` after
 * EVERY command that changed the document and after none that did not,
 * driven by the engine's own `document_mutation_seq` instead of a
 * hand-kept allowlist of command types (which had drifted: styles, lists,
 * sections, fields, notes, table properties, … never refreshed the mirror).
 *
 * Table-driven through the REAL worker + WASM engine: for each command,
 * count the deltas the EngineClient fans out between dispatching it and a
 * trailing `PING` (the worker queue is serial and posts a command's delta
 * before it starts the next command, so the PING reply is a barrier). */

type Row = { label: string; cmd: Record<string, unknown>; mutates: boolean };

const top = (idx: number, offset: number) => ({
    path: { steps: [{ kind: 'BLOCK', idx }] },
    offset,
});
const para = (idx: number) => ({ start: top(idx, 0), end: top(idx, 0) });

/* Each row runs against the document the previous rows left behind:
   paragraph 0 = "Alpha body text", paragraph 1 = "Second". */
const ROWS: Row[] = [
    /* Queries / selection / view — never a delta. */
    { label: 'ping', cmd: { type: 'PING' }, mutates: false },
    { label: 'stats', cmd: { type: 'REQUEST_STATS' }, mutates: false },
    {
        label: 'set selection',
        cmd: { type: 'SET_SELECTION', range: para(0), caret: top(0, 0) },
        mutates: false,
    },
    { label: 'hit test', cmd: { type: 'HIT_TEST', at: { x: 40, y: 40 } }, mutates: false },
    { label: 'select all', cmd: { type: 'SELECT_ALL' }, mutates: false },
    { label: 'copy', cmd: { type: 'GET_SELECTION_AS_CLIPBOARD' }, mutates: false },
    {
        label: 'collapse caret',
        cmd: { type: 'SET_SELECTION', range: para(0), caret: top(0, 0) },
        mutates: false,
    },
    /* Mutations the old allowlist missed. */
    {
        label: 'paragraph direction',
        cmd: { type: 'SET_PARAGRAPH_DIRECTION', range: para(1), direction: 'Rtl' },
        mutates: true,
    },
    {
        label: 'apply style',
        cmd: { type: 'APPLY_STYLE', range: para(0), style_id: 'Heading1' },
        mutates: true,
    },
    {
        label: 'toggle list',
        cmd: { type: 'TOGGLE_LIST', range: para(1), kind: 'Bullet' },
        mutates: true,
    },
    {
        label: 'line spacing',
        cmd: { type: 'SET_LINE_SPACING', range: para(0), multiplier: 1.5 },
        mutates: true,
    },
    {
        label: 'indent',
        cmd: {
            type: 'SET_PARAGRAPH_INDENT',
            range: para(0),
            start_pt: 12,
            end_pt: 0,
            first_line_pt: 0,
        },
        mutates: true,
    },
    {
        label: 'page orientation',
        cmd: { type: 'SET_PAGE_ORIENTATION', at: top(0, 0), orientation: 'Landscape' },
        mutates: true,
    },
    { label: 'insert field', cmd: { type: 'INSERT_FIELD', at: top(0, 0), kind: 'Page' }, mutates: true },
    { label: 'page break', cmd: { type: 'INSERT_PAGE_BREAK', at: top(0, 3) }, mutates: true },
    /* Undo / redo of one of them. */
    { label: 'undo', cmd: { type: 'UNDO' }, mutates: true },
    { label: 'redo', cmd: { type: 'REDO' }, mutates: true },
    /* A mutation the old allowlist already had — still one delta. */
    { label: 'insert text', cmd: { type: 'INSERT_TEXT', at: undefined, text: '!' }, mutates: true },
    /* Queries again, after the mutations. */
    {
        label: 'set selection (after)',
        cmd: { type: 'SET_SELECTION', range: para(0), caret: top(0, 0) },
        mutates: false,
    },
    { label: 'stats (after)', cmd: { type: 'REQUEST_STATS' }, mutates: false },
];

test('a11y deltas follow the engine mutation signal, per command class', async ({ page }) => {
    test.setTimeout(90_000);
    await page.goto('/');
    await page.waitForFunction(() => (window as any).__paintIdle === true, undefined, {
        timeout: 20_000,
    });

    /* Seed two paragraphs, then start counting deltas. */
    await page.evaluate(async (args) => {
        const w = window as any;
        const dispatch = (cmd: unknown): Promise<any> => w.__dispatch(cmd);
        await dispatch({ type: 'SELECT_ALL' });
        await dispatch({ type: 'INSERT_TEXT', at: undefined, text: 'Alpha body text' });
        await dispatch({ type: 'SPLIT_PARAGRAPH', at: args.at });
        await dispatch({ type: 'INSERT_TEXT', at: undefined, text: 'Second' });
        await dispatch({ type: 'PING' });
        w.__nge194Deltas = 0;
        w.__engineClient.subscribe((ev: any) => {
            if (ev.type === 'ACCESSIBILITY_TREE_DELTA') w.__nge194Deltas += 1;
        });
    }, { at: top(0, 15) });

    const observed: { label: string; deltas: number; mutates: boolean; error?: string }[] = [];
    for (const row of ROWS) {
        const result = await page.evaluate(async (cmd) => {
            const w = window as any;
            const before = w.__nge194Deltas as number;
            const evt = await w.__dispatch(cmd);
            await w.__dispatch({ type: 'PING' });
            return {
                deltas: (w.__nge194Deltas as number) - before,
                error: evt?.type === 'ERROR' ? String(evt.message) : undefined,
            };
        }, row.cmd);
        observed.push({ label: row.label, mutates: row.mutates, ...result });
    }

    for (const o of observed) {
        expect(o.error, `${o.label} dispatched cleanly`).toBeUndefined();
        expect(o.deltas, `${o.label}: ${o.mutates ? 'one delta' : 'no delta'}`).toBe(
            o.mutates ? 1 : 0,
        );
    }

    /* The mirror reflects the latest document without another keystroke. */
    await expect(page.locator('.a11y-mirror')).toContainText('Second');
});
