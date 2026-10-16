import { test, expect, type Page } from '@playwright/test';
import { boot } from './helpers/editor';

/* Issue #485 - a save the writer had to REPAIR says so.
 *
 * `format_docx` writes a character XML 1.0 cannot carry as U+FFFD instead of
 * failing (#467's last line of defence). Before #485 the engine dropped the
 * writer's `WriteNote`s, so the file on disk differed from the document on
 * screen and nothing told the user. `Event::DocumentSaved.notes` now carries
 * them; `createEditorState().lastSaveNotes()` mirrors them; `@nge/ui`'s
 * `RecoveryBanner` shows "N characters could not be saved and were replaced"
 * (`data-kinds="save-notes"`), the Dev HUD has a "Last save" row.
 *
 * HOW THE SPEC PRODUCES A REAL NOTE. Every command door drops a C0 control
 * (#467), so no command can put one into the model. The door that is left is
 * a RESTORED SNAPSHOT: the input boundary guards commands, not the bytes of
 * `Command::Recover.snapshot` (an older build's session, a hand-edited
 * file). The spec takes a real `SNAPSHOT` of a document holding `QQQQ`, flips
 * two of those bytes to U+0001 / U+0002 inside the MessagePack string, and
 * dispatches `RECOVER` with it through `__dispatch`. The next `SAVE_DOCX`
 * then runs the writer over a model that holds two forbidden characters. */

/** Replace the document with a snapshot whose text holds two C0 controls. */
async function restoreSnapshotWithControls(page: Page): Promise<void> {
    const result = await page.evaluate(async () => {
        const dispatch = (cmd: unknown): Promise<any> => (window as any).__dispatch(cmd);
        await dispatch({ type: 'SELECT_ALL' });
        await dispatch({ type: 'INSERT_TEXT', at: undefined, text: 'QQQQ' });
        const snap = await dispatch({ type: 'SNAPSHOT', seq: undefined });
        if (snap.type !== 'SNAPSHOT') return `snapshot: ${snap.type}`;
        const bytes: Uint8Array = new Uint8Array(snap.bytes);
        let at = -1;
        for (let i = 0; i + 4 <= bytes.length && at < 0; i++) {
            if (
                bytes[i] === 0x51 &&
                bytes[i + 1] === 0x51 &&
                bytes[i + 2] === 0x51 &&
                bytes[i + 3] === 0x51
            ) {
                at = i;
            }
        }
        if (at < 0) return 'marker not found in the snapshot';
        bytes[at + 1] = 0x01;
        bytes[at + 2] = 0x02;
        const rec = await dispatch({
            type: 'RECOVER',
            snapshot: bytes,
            log_tail: [],
            renderer_downgrade: undefined,
            package: undefined,
        });
        return rec.type === 'RECOVERED' ? 'ok' : `recover: ${rec.type} ${rec.message ?? ''}`;
    });
    expect(result).toBe('ok');
}

/** Save through the engine; return the reply's notes (the shell's File menu
 *  reacts to the same `DOCUMENT_SAVED` event the banner does). */
async function save(page: Page): Promise<unknown> {
    return page.evaluate(async () => {
        const evt = await (window as any).__dispatch({ type: 'SAVE_DOCX' });
        return evt.type === 'DOCUMENT_SAVED' ? (evt.notes ?? []) : `<${evt.type}>`;
    });
}

test('a save that replaced characters shows a dismissible notice (#485)', async ({ page }) => {
    test.setTimeout(60_000);
    await boot(page);
    const banner = page.locator('.nge-recovery-banner[data-kinds~="save-notes"]');
    await expect(banner).toHaveCount(0);

    /* A faithful save reports nothing: no banner, a clean HUD row. */
    await page.keyboard.press('Control+Shift+D');
    const row = page.locator('.nge-hud__save-notes');
    expect(await save(page)).toEqual([]);
    await expect(banner).toHaveCount(0);
    await expect(row).toHaveText('clean');

    await restoreSnapshotWithControls(page);
    /* Restoring alone saves nothing: no notice yet. */
    await expect(banner).toHaveCount(0);

    expect(await save(page)).toEqual([{ type: 'FORBIDDEN_CHARACTERS_REPLACED', count: 2 }]);

    await expect(banner).toBeVisible();
    await expect(banner).toHaveAttribute('role', 'alert');
    const notice = banner.locator('[data-kind="save-notes"]');
    await expect(notice).toContainText('Your file was saved with repairs');
    await expect(notice.locator('li')).toHaveCount(1);
    await expect(notice.locator('li')).toHaveText(
        '2 characters could not be saved and were replaced.',
    );

    /* The Dev HUD mirrors it. */
    await expect(row).toContainText('repaired');
    await expect(row).toContainText('ForbiddenCharactersReplaced ×2');
    await expect(row).toHaveClass(/nge-hud__warn/);

    /* Dismiss hides it ... */
    await banner.getByRole('button', { name: 'Dismiss recovery notice' }).click();
    await expect(banner).toHaveCount(0);

    /* ... and the NEXT repaired save shows it again. */
    expect(await save(page)).toEqual([{ type: 'FORBIDDEN_CHARACTERS_REPLACED', count: 2 }]);
    await expect(banner).toBeVisible();

    /* Typing the damaged text away and saving faithfully clears the report:
       the banner goes, the HUD row is clean again. */
    await page.evaluate(async () => {
        const dispatch = (cmd: unknown): Promise<any> => (window as any).__dispatch(cmd);
        await dispatch({ type: 'SELECT_ALL' });
        await dispatch({ type: 'INSERT_TEXT', at: undefined, text: 'clean' });
    });
    expect(await save(page)).toEqual([]);
    await expect(banner).toHaveCount(0);
    await expect(row).toHaveText('clean');
});
