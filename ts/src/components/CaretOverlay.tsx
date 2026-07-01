/* Phase 4 §8 — caret overlay.
 *
 * A blinking DOM <div> tracking the engine's caret rect. DOM-rendered, not
 * canvas-drawn, so CSS owns the blink animation and Solid repositions it via
 * the engine-store signal.
 *
 * Phase 6c — multi-canvas DOM: each `.editor-page` mounts its OWN caret
 * overlay. The overlay shows only when the caret's engine-Y falls within
 * THIS page's range; otherwise it renders nothing. CSS y from store is
 * already `engine_y_device / dpr`; the page top in CSS is
 * `pageIdx × (PAGE_H_CSS + PAGE_GAP_CSS)`. */
import { Show } from 'solid-js';
import type { EngineStore } from '../state/engine-store';
import { PAGE_GAP_CSS, PAGE_H_CSS } from '../state/engine-store';

export function CaretOverlay(props: { store: EngineStore; pageIdx?: number }) {
    const idx = () => props.pageIdx ?? 0;
    const pageTop = () => idx() * (PAGE_H_CSS + PAGE_GAP_CSS);
    const visibleCaret = () => {
        const c = props.store.caret();
        if (!c) return null;
        const top = pageTop();
        const bottom = top + PAGE_H_CSS;
        if (c.y + c.h < top || c.y > bottom) return null;
        return { x: c.x, y: c.y - top, w: c.w, h: c.h };
    };
    return (
        <Show when={visibleCaret()}>
            {(caret) => (
                <div
                    class="caret"
                    style={{
                        left: `${caret().x}px`,
                        top: `${caret().y}px`,
                        width: `${caret().w}px`,
                        height: `${caret().h}px`,
                    }}
                />
            )}
        </Show>
    );
}
