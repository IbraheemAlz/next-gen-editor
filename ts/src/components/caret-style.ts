/* Issue #557 (clean-room memo §11) — the caret overlay's colour and blink.
 *
 * Pure helpers, no DOM at import time (unit-tested in
 * `caret-style.test.ts`); `CaretOverlay` wires them. */
import type { Color } from '../engine/types';

/** The CSS colour the caret is drawn in: the text colour at the caret
 *  (`SELECTION_CHANGED.attrs_at_caret.color`), opaque. `undefined` (the
 *  stylesheet's `currentColor`) before the first selection event or for a
 *  fully transparent colour, which would hide the caret. */
export function caretColorCss(color: Color | null | undefined): string | undefined {
    if (!color || color.a === 0) return undefined;
    const ch = (v: number): number => Math.max(0, Math.min(255, Math.round(v)));
    return `rgb(${ch(color.r)}, ${ch(color.g)}, ${ch(color.b)})`;
}

/** Restart the caret's CSS blink from its visible phase: a moving caret is
 *  shown at once and blinks from there (Word / OnlyOffice restart the blink
 *  on every caret move). Rewinds the running animation in place — no
 *  re-mount, no forced style flush. */
export function restartBlink(el: Element | undefined): void {
    if (!el || typeof el.getAnimations !== 'function') return;
    for (const anim of el.getAnimations()) anim.currentTime = 0;
}
