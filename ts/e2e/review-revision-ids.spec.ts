import { test, expect } from '@playwright/test';
import { appendStoredEntry } from './zip-append';

/* Issue #304 — the Track Changes sidebar addresses each row by its
 * stable `revision_id`, not by its range. Tika-792's shape (Apache POI
 * corpus, `tools/roundtrip` `TIKA_792_BODY`) has a deletion NESTED in a
 * move destination over the same byte range (`<w:moveTo><w:del>`): two
 * rows, one range — range addressing could only ever reach the inner
 * one. Accepting / rejecting either half of the tracked move resolves
 * the whole move (both halves share its `move_name`). Assertions ride
 * the sidebar rows (engine truth from `revisions_snapshot`) and command
 * replies — valid under headless Chrome.
 */

declare global {
    interface Window {
        __dispatch: (cmd: unknown) => Promise<any>;
        __paintIdle?: boolean;
    }
}

const TIKA_792_BODY = [
    '<w:p w:rsidR="00910EBC" w:rsidRDefault="004F232C" w:rsidP="00910EBC">',
    '<w:bookmarkStart w:id="0" w:name="_GoBack"/><w:bookmarkEnd w:id="0"/>',
    '<w:del w:id="1" w:author="Author"><w:r><w:delText>s</w:delText></w:r></w:del>',
    '<w:moveToRangeStart w:id="2" w:author="Author" w:name="move256509658"/>',
    '<w:moveTo w:id="3" w:author="Author"><w:del w:id="4" w:author="Author"><w:r><w:delText>.</w:delText></w:r></w:del></w:moveTo>',
    '</w:p>',
    '<w:p w:rsidR="00BE45BB" w:rsidRDefault="004F232C" w:rsidP="00910EBC">',
    '<w:moveFromRangeStart w:id="5" w:author="Author" w:name="move256509658"/><w:moveToRangeEnd w:id="2"/>',
    '<w:moveFrom w:id="6" w:author="Author"><w:ins w:id="7" w:author="Author"><w:r><w:t>b</w:t></w:r></w:ins></w:moveFrom>',
    '<w:moveFromRangeEnd w:id="5"/>',
    '</w:p>',
].join('');

/** A minimal STORED `.docx` around `body`. */
function docx(body: string): number[] {
    const xml = '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n';
    const entries: [string, string][] = [
        [
            '[Content_Types].xml',
            `${xml}<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">` +
                '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>' +
                '<Default Extension="xml" ContentType="application/xml"/>' +
                '<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>' +
                '</Types>',
        ],
        [
            '_rels/.rels',
            `${xml}<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">` +
                '<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>' +
                '</Relationships>',
        ],
        [
            'word/document.xml',
            `${xml}<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">` +
                `<w:body>${body}<w:sectPr/></w:body></w:document>`,
        ],
    ];
    /* An empty archive is just its end-of-central-directory record. */
    const eocd = new Uint8Array(22);
    new DataView(eocd.buffer).setUint32(0, 0x06054b50, true);
    let zip = eocd;
    for (const [name, text] of entries) {
        zip = appendStoredEntry(zip, name, new TextEncoder().encode(text));
    }
    return Array.from(zip);
}

async function depth(page: import('@playwright/test').Page): Promise<number> {
    return page.evaluate(async () => {
        const sel = await (window as any).__dispatch({ type: 'SELECT_ALL' });
        return sel.undo_depth as number;
    });
}

test('every sidebar row is addressable by id; a tracked move resolves as a pair', async ({
    page,
}) => {
    await page.goto('/');
    await page.waitForFunction(() => (window as any).__paintIdle === true, undefined, {
        timeout: 20_000,
    });
    const loaded = await page.evaluate(async (arr: number[]) => {
        const evt = await (window as any).__dispatch({
            type: 'LOAD_DOCX',
            bytes: new Uint8Array(arr),
        });
        return evt.type as string;
    }, docx(TIKA_792_BODY));
    expect(loaded).not.toBe('ERROR');

    const sidebar = page.getByRole('complementary', { name: 'Track changes' });
    const refresh = sidebar.getByRole('button', { name: 'Refresh' });
    const rows = sidebar.locator('.nge-tc__row');
    await refresh.click();
    await expect(rows).toHaveCount(5);
    const ids = await rows.evaluateAll((els) => els.map((e) => e.getAttribute('data-revision-id')));
    expect(ids.every((id) => id !== null && /^\d+$/.test(id))).toBe(true);
    expect(new Set(ids).size).toBe(5);
    /* The deletion and the move destination over "." share one range. */
    const moveTo = sidebar.locator('.nge-tc__row--move-to');
    await expect(moveTo).toHaveCount(1);
    const before = await depth(page);

    /* Accept the OUTER wrapper: the move resolves as a pair (its source
       "b" goes, with the insertion nested in it); both deletions stay
       listed — one undo step. */
    await moveTo.getByRole('button', { name: 'Accept revision' }).click();
    await expect(rows).toHaveCount(2);
    await expect(sidebar.locator('.nge-tc__row--delete')).toHaveCount(2);
    await expect(sidebar.locator('.nge-tc__row--move-from')).toHaveCount(0);
    expect(await depth(page)).toBe(before + 1);

    /* Undo, then reject the SOURCE half: the destination (and the
       deletion nested in it) goes too; the source keeps its insertion. */
    await page.evaluate(() => (window as any).__dispatch({ type: 'UNDO' }));
    await refresh.click();
    await expect(rows).toHaveCount(5);
    await sidebar
        .locator('.nge-tc__row--move-from')
        .getByRole('button', { name: 'Reject revision' })
        .click();
    await expect(rows).toHaveCount(2);
    await expect(sidebar.locator('.nge-tc__row--delete')).toHaveCount(1);
    await expect(sidebar.locator('.nge-tc__row--insert')).toHaveCount(1);
});
