/* Phase 4 §10 — ARIA live region.
 *
 * A screen reader announces this element's text whenever it changes —
 * `aria-live="polite"` waits for a pause in speech. Used for state changes
 * a sighted user sees but a screen-reader user would otherwise miss
 * (document loaded, and later: selection changes, save status). */
import type { EngineStore } from '../state/engine-store';

export function Announcements(props: { store: EngineStore }) {
    return (
        <div role="status" aria-live="polite" class="visually-hidden">
            {props.store.announcement()}
        </div>
    );
}
