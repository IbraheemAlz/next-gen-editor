import { test, expect } from '@playwright/test';

/* Issues #70/#71/#72/#73/#74/#43 — the epic's validation scenario,
 * end-to-end through the REAL worker + WASM engine + bridge schema: a
 * multi-section "legal contract" —
 *
 *   Section 1: Different First Page, a First-role cover header;
 *   Section 2: document-wide Different Odd & Even, an UNLINKED footer
 *              carrying a live "Page {PAGE} of {NUMPAGES}" field pair;
 *   save → reload → every section behavior intact.
 *
 * Assertions ride command replies (section_geometry.title_pg /
 * even_odd_headers / section counts, editing_story.role / linked) —
 * engine truth valid under headless Chrome. Pixel-level verification
 * of resolved field text is covered by the native engine-wasm tests
 * (headless Chrome never composites the interactive canvas — see
 * CLAUDE.md).
 */

declare global {
    interface Window {
        __dispatch: (cmd: unknown) => Promise<any>;
        __paintIdle?: boolean;
    }
}

test('legal contract: title page + even/odd + unlinked field footer survive a round trip', async ({
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

        /* Deterministic base. */
        await dispatch({ type: 'SELECT_ALL' });
        await dispatch({
            type: 'INSERT_TEXT',
            at: undefined,
            text: 'Whereas the parties agree to the following terms',
        });

        /* 1 — Section 1 gets Different First Page; the checkbox state
        must read back through section_geometry. */
        const titled = await dispatch({ type: 'SET_TITLE_PAGE', enabled: true });
        const titleGeo = titled.section_geometry;

        /* 2 — the cover header: page 0 is section 1's first page, so
        entering the header edits the FIRST role slot. */
        const enterCover = await dispatch({
            type: 'ENTER_HEADER_FOOTER',
            page: 0,
            area: 'Header',
        });
        const coverStory = enterCover.editing_story;
        await dispatch({ type: 'INSERT_TEXT', at: undefined, text: 'CONTRACT COVER' });
        await dispatch({ type: 'EXIT_HEADER_FOOTER' });

        /* 3 — break into section 2 (next page). */
        const broke = await dispatch({
            type: 'INSERT_SECTION_BREAK',
            at: top(0, 7),
            kind: 'NextPage',
        });

        /* 4 — document-wide Different Odd & Even. */
        const evenOdd = await dispatch({ type: 'SET_EVEN_ODD_HEADERS', enabled: true });
        const evenOddGeo = evenOdd.section_geometry;

        /* 5 — section 2's footer. The break copied the covering
        sectPr wholesale (Word-exact), so section 2 INHERITED
        titlePg — and page 1 is section 2's FIRST page, which
        outranks even/odd: the First role wins. Nothing in the chain
        has a First footer → the engine materializes one on the
        edited section (owned, not linked). Even-role selection on a
        non-first even page is pinned by the native test
        `even_odd_toggle_selects_the_even_role_on_page_two`. */
        const enterFooter = await dispatch({
            type: 'ENTER_HEADER_FOOTER',
            page: 1,
            area: 'Footer',
        });
        const footerStory = enterFooter.editing_story;

        /* 6 — the live field pair: "Page {PAGE} of {NUMPAGES}". */
        await dispatch({ type: 'INSERT_TEXT', at: undefined, text: 'Page ' });
        const afterPage = await dispatch({
            type: 'INSERT_FIELD',
            at: top(0, 5),
            kind: 'Page',
        });
        await dispatch({ type: 'INSERT_TEXT', at: undefined, text: ' of ' });
        const numAt = 5 + 1 + 4; /* "Page " + cached "1" + " of " */
        const afterNum = await dispatch({
            type: 'INSERT_FIELD',
            at: top(0, numAt),
            kind: 'NumPages',
        });

        /* 7 — unlink is a no-op success on an owned slot. */
        const unlink = await dispatch({ type: 'SET_HEADER_FOOTER_LINK', linked: false });
        const ownedStory = unlink.editing_story;
        await dispatch({ type: 'EXIT_HEADER_FOOTER' });

        /* 8 — a story-blocked command still errors loudly. */
        await dispatch({ type: 'ENTER_HEADER_FOOTER', page: 0, area: 'Header' });
        const gated = await dispatch({
            type: 'TOGGLE_TRACK_CHANGES',
            enabled: true,
        });
        await dispatch({ type: 'EXIT_HEADER_FOOTER' });

        /* 9 — save + reload the produced bytes. */
        const saved = await dispatch({ type: 'SAVE_DOCX' });
        const reloaded = await dispatch({ type: 'LOAD_DOCX', bytes: saved.bytes });
        const geoS1 = (
            await dispatch({
                type: 'SET_SELECTION',
                range: { start: top(0, 0), end: top(0, 0) },
                caret: top(0, 0),
            })
        ).section_geometry;
        const geoS2 = (
            await dispatch({
                type: 'SET_SELECTION',
                range: { start: top(1, 0), end: top(1, 0) },
                caret: top(1, 0),
            })
        ).section_geometry;

        return {
            titleGeo,
            coverStory,
            breakGeo: broke.section_geometry,
            evenOddGeo,
            footerStory,
            afterPageType: afterPage.type,
            afterNumType: afterNum.type,
            ownedStory,
            gatedType: gated.type,
            reloadedType: reloaded.type,
            geoS1,
            geoS2,
        };
    });

    /* Different First Page is on and reads back. */
    expect(result.titleGeo?.title_pg).toBe(true);

    /* The cover header edits the FIRST role slot. */
    expect(result.coverStory?.area).toBe('Header');
    expect(result.coverStory?.role).toBe('First');

    /* Two sections after the break. */
    expect(result.breakGeo?.section_count).toBe(2);

    /* Document-wide even/odd is on and reads back. */
    expect(result.evenOddGeo?.even_odd_headers).toBe(true);

    /* Page 2 is section 2's first page with inherited titlePg → the
    FIRST role outranks even/odd; nothing to inherit → materialized
    on the edited section (owned). */
    expect(result.footerStory?.area).toBe('Footer');
    expect(result.footerStory?.role).toBe('First');
    expect(result.footerStory?.linked).toBe(false);
    expect(result.footerStory?.section_index).toBe(1);

    /* Field authoring succeeded in the story. */
    expect(result.afterPageType).toBe('SELECTION_CHANGED');
    expect(result.afterNumType).toBe('SELECTION_CHANGED');

    /* Unlink on an owned slot: no-op success, rid unchanged. */
    expect(result.ownedStory?.rid).toBe(result.footerStory?.rid);
    expect(result.ownedStory?.linked).toBe(false);

    /* Honest UX — review commands stay blocked in stories. */
    expect(result.gatedType).toBe('ERROR');

    /* Round trip: both section behaviors survive save → reload. */
    expect(result.reloadedType).not.toBe('ERROR');
    expect(result.geoS1?.title_pg).toBe(true);
    expect(result.geoS1?.even_odd_headers).toBe(true);
    expect(result.geoS1?.section_count).toBe(2);
    expect(result.geoS2?.section_index).toBe(1);
});
