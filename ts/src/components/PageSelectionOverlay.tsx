/* Phase 6c — per-page selection overlay.
 *
 * The single-canvas world had one absolute overlay positioned against
 * the page-0 container, painting every rect with `top: rect.y / dpr`.
 * That broke the moment the multi-canvas refactor split pages into
 * separate DOM elements: rects for pages 1+ landed past the bottom of
 * page 0's container (which is now a fixed 1123 CSS px box).
 *
 * Fix: one overlay PER `.editor-page`. Each filters the
 * engine's flat rect list to the rects whose engine-Y falls inside
 * THIS page's range, then re-positions them in the page's LOCAL
 * coordinate space. Engine rect.y comes from the store already
 * converted to CSS px; `pageTopCss` is `pageIdx × (PAGE_H_CSS +
 * GAP_CSS)`, matching the engine's `PAGE_GAP_PT × scale` so the
 * positioning math aligns with hit-test routing. */
import { For } from 'solid-js';
import type { EngineStore } from '../state/engine-store';

/** A4 page height at 96 DPI (engine `841.9 pt × 4/3 ≈ 1123 CSS px`). */
const PAGE_H_CSS = 1123;
/** Inter-page CSS gap — matches engine `PAGE_GAP_PT = 48 pt × 4/3 = 64 CSS px`. */
const PAGE_GAP_CSS = 64;

export function PageSelectionOverlay(props: { store: EngineStore; pageIdx: number }) {
    const pageTopCss = () => props.pageIdx * (PAGE_H_CSS + PAGE_GAP_CSS);
    const pageBottomCss = () => pageTopCss() + PAGE_H_CSS;

    const visibleRects = () => {
        const top = pageTopCss();
        const bottom = pageBottomCss();
        return props.store
            .selection()
            .rects.filter((r) => r.y + r.h > top && r.y < bottom);
    };

    return (
        <For each={visibleRects()}>
            {(r) => {
                const localTop = r.y - pageTopCss();
                return (
                    <div
                        class="selection-rect"
                        style={{
                            left: `${r.x}px`,
                            top: `${localTop}px`,
                            width: `${r.w}px`,
                            height: `${r.h}px`,
                        }}
                    />
                );
            }}
        </For>
    );
}
