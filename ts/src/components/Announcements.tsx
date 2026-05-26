/* Phase 4 §10 / Sprint 10 — ARIA live regions.
 *
 * Two visually-hidden live regions side by side: a polite one for
 * routine engine narration (alignment changed, page break inserted, …)
 * and an assertive one for errors / blocked actions. The engine emits
 * `Event::Announcement { priority, message }` per dispatch; the UI
 * routes by priority and never computes ARIA semantics from event
 * payloads (per the Sprint 10 accessibility constraint). */
import type { EngineStore } from '../state/engine-store';

export function Announcements(props: { store: EngineStore }) {
    return (
        <>
            <div role="status" aria-live="polite" class="visually-hidden">
                {props.store.announcement()}
            </div>
            <div role="alert" aria-live="assertive" class="visually-hidden">
                {props.store.assertiveAnnouncement()}
            </div>
        </>
    );
}
