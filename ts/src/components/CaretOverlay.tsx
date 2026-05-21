/* Phase 4 §8 — caret overlay.
 *
 * A blinking DOM <div> tracking the engine's caret rect. DOM-rendered, not
 * canvas-drawn, so CSS owns the blink animation and Solid repositions it via
 * the engine-store signal. */
import { Show } from 'solid-js';
import type { EngineStore } from '../state/engine-store';

export function CaretOverlay(props: { store: EngineStore }) {
    return (
        <Show when={props.store.caret()}>
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
