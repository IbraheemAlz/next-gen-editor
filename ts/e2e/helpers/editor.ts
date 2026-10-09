import type { Page } from '@playwright/test';

/* Shared e2e helpers (issue #310).
 *
 * WHY a synchronous burst, not `page.keyboard` / `page.mouse`:
 * every shell-mirror race (#53 click->type, #64 shift-click->type and
 * Enter, #286 Ctrl+B->type) needs the second input to reach the shell
 * BEFORE the engine's reply to the first has landed. Playwright's input
 * methods round-trip through CDP per event -- slow enough (several ms)
 * that the worker answers between two simulated events, the mirrored UI
 * state is already fresh, and a spec passes against the buggy shell
 * (the #286 real-keyboard spec did exactly that). `burst` instead
 * dispatches the whole sequence from ONE synchronous `page.evaluate`
 * task: the main thread never yields, so no engine reply (a
 * `postMessage` macrotask) can interleave, which is the deterministic
 * form of "typing speed". Every race-class spec should use it, and every
 * new race spec must be shown to FAIL against a deliberately re-introduced
 * deferral in the shell (see CLAUDE.md, e2e notes). */

/** Navigate to the interactive app and wait for the first idle paint. */
export async function boot(page: Page): Promise<void> {
    await page.goto('/');
    await page.waitForFunction(() => (window as any).__paintIdle === true, undefined, {
        timeout: 30_000,
    });
}

/** The whole document as plain text (paragraphs joined by the engine). Goes
 *  through the worker queue, so it observes every command already posted. */
export async function documentText(page: Page): Promise<string> {
    return page.evaluate(async () => {
        const dispatch = (window as any).__dispatch;
        await dispatch({ type: 'SELECT_ALL' });
        const clip = await dispatch({ type: 'GET_SELECTION_AS_CLIPBOARD' });
        return clip.type === 'CLIPBOARD_PAYLOAD' ? (clip.plain as string) : `<${clip.type}>`;
    });
}

/** Wait until every command already posted has been answered, WITHOUT
 *  touching the selection (a worker round-trip behind the queue). Unlike
 *  {@link documentText}, which SELECT_ALLs. */
export async function settle(page: Page): Promise<void> {
    await page.evaluate(async () => {
        await (window as any).__dispatch({ type: 'GET_SELECTION_AS_CLIPBOARD' });
    });
}

/** One step of a {@link burst}.
 *  - `'B'`: Ctrl/Cmd+B keydown on the hidden textarea
 *  - `'BTN'`: click on the toolbar Bold button
 *  - `'ENTER'`: an `insertLineBreak` beforeinput (the Enter key)
 *  - `{ pointerdown }`: a primary-button `pointerdown` (optionally with
 *    Shift) on page `page`'s canvas at page-local DEVICE px `(x, y)`,
 *    through the shell's real `pointer.ts` listener
 *  - `{ pointerup }`: releases it
 *  - any other string: an `insertText` beforeinput carrying that text */
export type BurstStep =
    | string
    | { pointerdown: { x: number; y: number; page?: number; shift?: boolean } }
    | { pointerup: true; page?: number };

/** Fire `steps` as ONE synchronous burst on the main thread (see the file
 *  header). Resolves once the burst has been POSTED; read results through
 *  a worker round-trip (e.g. {@link documentText}), which is FIFO behind
 *  every command the burst queued. */
export async function burst(page: Page, steps: BurstStep[]): Promise<void> {
    await page.evaluate((steps) => {
        const ta = document.querySelector<HTMLTextAreaElement>('textarea[data-nge-hidden-input]');
        if (!ta) throw new Error('editor hidden input missing');
        const mac = /Mac|iPhone|iPad/.test(navigator.platform);
        const pointer = (
            type: 'pointerdown' | 'pointerup',
            pageIdx: number,
            x: number,
            y: number,
            shift: boolean,
        ): void => {
            const canvas = document.querySelector<HTMLCanvasElement>(
                `.editor-page[data-page-index="${pageIdx}"] canvas`,
            );
            if (!canvas) throw new Error(`page ${pageIdx} canvas missing`);
            /* A synthetic pointerId is not an active pointer, so the real
               setPointerCapture would throw before the shell dispatches. */
            canvas.setPointerCapture = () => {};
            canvas.releasePointerCapture = () => {};
            const r = canvas.getBoundingClientRect();
            const dpr = window.devicePixelRatio || 1;
            canvas.dispatchEvent(
                new PointerEvent(type, {
                    clientX: r.left + x / dpr,
                    clientY: r.top + y / dpr,
                    button: 0,
                    buttons: type === 'pointerdown' ? 1 : 0,
                    pointerId: 1,
                    pointerType: 'mouse',
                    isPrimary: true,
                    shiftKey: shift,
                    bubbles: true,
                    cancelable: true,
                }),
            );
        };
        for (const s of steps) {
            if (typeof s !== 'string') {
                if ('pointerdown' in s) {
                    const p = s.pointerdown;
                    pointer('pointerdown', p.page ?? 0, p.x, p.y, p.shift === true);
                } else {
                    pointer('pointerup', s.page ?? 0, 0, 0, false);
                }
            } else if (s === 'B') {
                ta.dispatchEvent(
                    new KeyboardEvent('keydown', {
                        key: 'b',
                        code: 'KeyB',
                        ctrlKey: !mac,
                        metaKey: mac,
                        bubbles: true,
                        cancelable: true,
                    }),
                );
            } else if (s === 'BTN') {
                const btn = document.querySelector<HTMLButtonElement>('.nge-tfmt__btn--bold');
                if (!btn) throw new Error('bold button missing');
                btn.click();
            } else if (s === 'ENTER') {
                ta.dispatchEvent(
                    new InputEvent('beforeinput', {
                        inputType: 'insertLineBreak',
                        bubbles: true,
                        cancelable: true,
                    }),
                );
            } else {
                ta.dispatchEvent(
                    new InputEvent('beforeinput', {
                        inputType: 'insertText',
                        data: s,
                        bubbles: true,
                        cancelable: true,
                    }),
                );
            }
        }
    }, steps);
}
