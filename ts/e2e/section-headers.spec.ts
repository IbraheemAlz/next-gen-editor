import { test, expect } from '@playwright/test';

/* Phase 3 (#39/#40) — the epic's validation scenario, end-to-end through
 * the REAL worker + WASM engine + bridge schema:
 *
 *   body text → section break (next page) → Header A on page 1 →
 *   Header B on page 2 (independent parts) → save → reload → intact.
 *
 * Assertions ride command replies (section_geometry.section_index /
 * section_count, editing_story, page_count) — engine-truth that is
 * valid under headless Chrome. Pixel-level verification of the painted
 * bands is covered by the native `header_a_header_b_epic_scenario`
 * unit test (band boxes on both PageBoxes) plus interactive QA in a
 * real browser: headless Chrome never composites the interactive
 * canvas (see CLAUDE.md), so no screenshot assertions here.
 */

declare global {
    interface Window {
        __dispatch: (cmd: unknown) => Promise<any>;
        __engineClient: {
            subscribe: (fn: (e: any) => void) => () => void;
        };
        __paintIdle?: boolean;
    }
}

test('section break + independent headers survive a save/reload round trip', async ({
    page,
}) => {
    test.setTimeout(60_000);
    await page.goto('/');
    await page.waitForFunction(() => (window as any).__paintIdle === true, undefined, {
        timeout: 20_000,
    });

    const result = await page.evaluate(async () => {
        const dispatch = (cmd: unknown): Promise<any> => (window as any).__dispatch(cmd);
        const top = (idx: number, offset: number) => ({
            path: { steps: [{ kind: 'BLOCK', idx }] },
            offset,
        });

        /* Deterministic base: replace the seed document. */
        await dispatch({ type: 'SELECT_ALL' });
        await dispatch({ type: 'INSERT_TEXT', at: undefined, text: 'hello world' });

        /* 1 — section break (next page) mid-paragraph. */
        const afterBreak = await dispatch({
            type: 'INSERT_SECTION_BREAK',
            at: top(0, 5),
            kind: 'NextPage',
        });
        const breakGeo = afterBreak.section_geometry;

        /* 2 — Header A on page 1's section. */
        const enterA = await dispatch({
            type: 'ENTER_HEADER_FOOTER',
            page: 0,
            area: 'Header',
        });
        const storyA = enterA.editing_story;
        await dispatch({ type: 'INSERT_TEXT', at: undefined, text: 'Header A' });
        const exitA = await dispatch({ type: 'EXIT_HEADER_FOOTER' });

        /* 3 — Header B on page 2's section (independent part). */
        const enterB = await dispatch({
            type: 'ENTER_HEADER_FOOTER',
            page: 1,
            area: 'Header',
        });
        const storyB = enterB.editing_story;
        await dispatch({ type: 'INSERT_TEXT', at: undefined, text: 'Header B' });
        await dispatch({ type: 'EXIT_HEADER_FOOTER' });

        /* 4 — a gated command in story mode must error loudly. */
        await dispatch({ type: 'ENTER_HEADER_FOOTER', page: 0, area: 'Header' });
        const gated = await dispatch({ type: 'INSERT_PAGE_BREAK', at: top(0, 0) });
        await dispatch({ type: 'EXIT_HEADER_FOOTER' });

        /* 5 — save, then reload the produced bytes into the engine. */
        const saved = await dispatch({ type: 'SAVE_DOCX' });
        const bytes: Uint8Array = saved.bytes;
        const reloaded = await dispatch({ type: 'LOAD_DOCX', bytes });

        /* 6 — post-reload engine truth: two sections; caret in section 1
           of 2 at doc start, 2 of 2 past the boundary. */
        const selStart = await dispatch({
            type: 'SET_SELECTION',
            range: { start: top(0, 0), end: top(0, 0) },
            caret: top(0, 0),
        });
        const selPast = await dispatch({
            type: 'SET_SELECTION',
            range: { start: top(1, 0), end: top(1, 0) },
            caret: top(1, 0),
        });

        return {
            breakGeo,
            storyA,
            storyB,
            exitAStory: exitA.editing_story ?? null,
            gatedType: gated.type,
            gatedMessage: gated.message ?? '',
            reloadedType: reloaded.type,
            startGeo: selStart.section_geometry,
            pastGeo: selPast.section_geometry,
        };
    });

    /* Section break landed: caret starts section 2 of 2. */
    expect(result.breakGeo?.section_count).toBe(2);
    expect(result.breakGeo?.section_index).toBe(1);

    /* Stories activated with INDEPENDENT parts. */
    expect(result.storyA?.area).toBe('Header');
    expect(result.storyA?.page).toBe(0);
    expect(result.storyB?.area).toBe('Header');
    expect(result.storyB?.rid).not.toBe(result.storyA?.rid);
    expect(result.exitAStory).toBeNull();

    /* Honest-UX backstop: gated commands error, never silently no-op. */
    expect(result.gatedType).toBe('ERROR');
    expect(result.gatedMessage).toContain('header or footer');

    /* Save → reload keeps both sections addressable. */
    expect(result.reloadedType).not.toBe('ERROR');
    expect(result.startGeo?.section_count).toBe(2);
    expect(result.startGeo?.section_index).toBe(0);
    expect(result.pastGeo?.section_index).toBe(1);
});
