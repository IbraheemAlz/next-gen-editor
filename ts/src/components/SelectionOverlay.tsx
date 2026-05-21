/* Phase 4 §8 — selection overlay.
 *
 * One DOM <div> per engine selection rect. The engine sends a list of rects
 * (one per line in the D4.6 pragmatic subset), so a contiguous selection
 * spanning several lines renders as several rectangles. */
import { For } from 'solid-js';
import type { EngineStore } from '../state/engine-store';

export function SelectionOverlay(props: { store: EngineStore }) {
    return (
        <For each={props.store.selection().rects}>
            {(r) => (
                <div
                    class="selection-rect"
                    style={{
                        left: `${r.x}px`,
                        top: `${r.y}px`,
                        width: `${r.w}px`,
                        height: `${r.h}px`,
                    }}
                />
            )}
        </For>
    );
}
