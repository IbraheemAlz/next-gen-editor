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
 * converted to CSS px; the page top comes from the store's
 * engine-reported geometry (issue #26) so mixed-orientation sections
 * position exactly; uniform-A4 fallback before the first paint. */
import { For } from 'solid-js';
import type { EngineStore } from '../state/engine-store';

export function PageSelectionOverlay(props: { store: EngineStore; pageIdx: number }) {
    const pageTopCss = () => props.store.pageTopCss(props.pageIdx);
    const pageBottomCss = () => pageTopCss() + props.store.pageHeightCss(props.pageIdx);

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
