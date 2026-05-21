/* Phase 4 §10 — accessibility shadow tree.
 *
 * A <canvas> is invisible to screen readers, so this mirrors the engine's
 * document into a visually-hidden but ARIA-visible DOM tree that NVDA /
 * VoiceOver / Orca can read and navigate. It is rebuilt from the engine's
 * `A11yTree` after every edit (the worker broadcasts a fresh tree). The
 * mirror holds no authority — it only reflects engine state. */
import { For } from 'solid-js';
import type { A11yRun } from '../engine/types';
import type { EngineStore } from '../state/engine-store';

/** Inline CSS for a run, so a screen reader can announce its formatting. */
function runStyle(r: A11yRun): string {
    const parts: string[] = [];
    if (r.bold) parts.push('font-weight:bold');
    if (r.italic) parts.push('font-style:italic');
    if (r.underline) parts.push('text-decoration:underline');
    return parts.join(';');
}

export function AccessibilityTree(props: { store: EngineStore }) {
    return (
        <div role="document" class="a11y-mirror" aria-label="Document">
            <For each={props.store.a11yTree()?.paragraphs ?? []}>
                {(p) => (
                    <p data-pid={p.id} dir={p.direction === 'Rtl' ? 'rtl' : 'ltr'}>
                        <For each={p.runs}>
                            {(r) => <span style={runStyle(r)}>{r.text}</span>}
                        </For>
                    </p>
                )}
            </For>
        </div>
    );
}
