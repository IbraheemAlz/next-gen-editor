/* Phase 4 §10 — accessibility mirror.
 *
 * A <canvas> is invisible to screen readers, so this mirrors the engine's
 * document into a visually-hidden but ARIA-visible DOM tree that NVDA /
 * VoiceOver / Orca can read and navigate.
 *
 * Backlog #10: the engine broadcasts incremental `A11yPatch` deltas after each
 * mutation; the reconciler patches only the changed <p> nodes, so a keystroke
 * no longer rebuilds the whole mirror. This component is a bare `ref` host —
 * `createA11yReconciler` owns everything inside it; Solid never re-renders the
 * children. The mirror holds no authority — it only reflects engine state. */
import { onCleanup, onMount } from 'solid-js';
import { createA11yReconciler } from '../a11y/tree';
import type { EngineClient } from '../engine/engine-client';

export function AccessibilityTree(props: { client: EngineClient }) {
    let mirror: HTMLDivElement | undefined;
    let unsubscribe: (() => void) | undefined;

    onMount(() => {
        const reconciler = createA11yReconciler(mirror!);
        unsubscribe = props.client.subscribe((ev) => {
            if (ev.type === 'ACCESSIBILITY_TREE_DELTA') reconciler.apply(ev.patches);
        });
    });
    onCleanup(() => unsubscribe?.());

    return <div role="document" class="a11y-mirror" aria-label="Document" ref={mirror} />;
}
