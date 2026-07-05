/* Phase 3 (#39) — header/footer editing mode overlay, one per
 * `.editor-page` (the PageSelectionOverlay per-page pattern).
 *
 * While a story is active:
 *   - every page's BODY content area dims (the engine routes all input
 *     into the story, so the body is visibly inert — Word's treatment);
 *   - the ACTIVE band on the ANCHOR page gets a dashed outline + a
 *     label chip naming the story and the exit affordances.
 *
 * The whole overlay is `pointer-events: none` (see editor.css) — a
 * double-click on the dimmed body must still reach the canvas, because
 * that double-click IS the exit gesture.
 *
 * Band placement derives from the engine-reported per-page margins
 * (`Painted.page_margin_tops/bottoms`), the same numbers the pointer
 * gate uses — the zone you can double-click and the zone that lights up
 * are one and the same by construction.
 */
import { Show } from 'solid-js';
import type { EngineStore } from '../state/engine-store';

export function StoryModeOverlay(props: { store: EngineStore; pageIdx: number }) {
    const dpr = () => window.devicePixelRatio || 1;
    const story = () => props.store.editingStory();
    const marginTopCss = () => {
        const t = props.store.marginGeometry()?.tops[props.pageIdx];
        return t !== undefined ? t / dpr() : undefined;
    };
    const marginBottomCss = () => {
        const b = props.store.marginGeometry()?.bottoms[props.pageIdx];
        return b !== undefined ? b / dpr() : undefined;
    };
    const pageHeight = () => props.store.pageHeightCss(props.pageIdx);
    const isAnchor = () => story()?.page === props.pageIdx;

    return (
        <Show when={story() && marginTopCss() !== undefined}>
            {/* Dimmed body content area — every page. */}
            <div
                class="story-dim"
                style={{
                    top: `${marginTopCss() ?? 0}px`,
                    height: `${Math.max(
                        0,
                        pageHeight() - (marginTopCss() ?? 0) - (marginBottomCss() ?? 0),
                    )}px`,
                }}
            />
            <Show when={isAnchor()}>
                <div
                    class="story-band"
                    classList={{ 'story-band--footer': story()?.area === 'Footer' }}
                    style={
                        story()?.area === 'Header'
                            ? { top: '2px', height: `${(marginTopCss() ?? 0) - 4}px` }
                            : {
                                  top: `${pageHeight() - (marginBottomCss() ?? 0) + 2}px`,
                                  height: `${(marginBottomCss() ?? 0) - 4}px`,
                              }
                    }
                >
                    <span class="story-band__chip">
                        {chipLabel(story())} — double-click the body or press Esc to
                        finish
                    </span>
                </div>
            </Show>
        </Show>
    );
}

/* Issue #70/#74 — role-aware chip: names WHICH slot is being edited
 * ("First Page Header", "Even Page Footer") and mirrors Word's
 * "Same as Previous" tag on linked bands. */
function chipLabel(
    story:
        | { area: string; role?: string; linked?: boolean }
        | undefined,
): string {
    if (!story) return '';
    const base = story.area === 'Header' ? 'Header' : 'Footer';
    const role =
        story.role === 'First'
            ? `First Page ${base}`
            : story.role === 'Even'
              ? `Even Page ${base}`
              : base;
    return story.linked ? `${role} (same as previous)` : role;
}
