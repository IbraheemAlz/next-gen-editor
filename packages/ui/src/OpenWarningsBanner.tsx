/**
 * OpenWarningsBanner — issue #406: the visible half of a DEGRADED open.
 *
 * The `.docx` reader reports every non-fatal diagnostic of an open (a page
 * margin clamped into range, a measure it had to ignore, a part read
 * through namespace normalisation — which is then rewritten on save — a
 * field closed early, a relationship that escaped the package). Before
 * #406 the engine dropped that report, so such a file looked exactly like
 * a clean open. This dismissible info banner says "Opened with N issues"
 * and lists them under "Details" (`createEditorState().openWarnings()`,
 * worded by `@nge/core`'s `describeReadWarning()`).
 *
 * A clean open shows nothing. Dismissing hides the banner for THIS open's
 * report; the next open with warnings shows it again. `role="status"`:
 * informational, not an alert — the document is open and editable.
 */
import { createMemo, createSignal, For, Show, type Component } from 'solid-js';
import {
    createEditorState,
    describeReadWarning,
    openWarningCount,
    type ReadWarning,
} from '@nge/core';
import './OpenWarningsBanner.css';

export const OpenWarningsBanner: Component = () => {
    const state = createEditorState();
    /* The report the user dismissed (by identity: every open replaces the
       array, so a newer report shows again). */
    const [dismissed, setDismissed] = createSignal<ReadWarning[] | undefined>(undefined);
    const warnings = createMemo(() => state.openWarnings());
    const count = createMemo(() => openWarningCount(warnings()));
    const visible = createMemo(() => count() > 0 && dismissed() !== warnings());

    return (
        <Show when={visible()}>
            <div
                class="nge-open-warnings"
                role="status"
                data-count={count()}
                data-kinds={[...new Set(warnings().map((w) => w.kind))].join(' ')}
            >
                <span class="nge-open-warnings__icon" aria-hidden="true">
                    i
                </span>
                <div class="nge-open-warnings__body">
                    <strong class="nge-open-warnings__title">
                        Opened with {count()} {count() === 1 ? 'issue' : 'issues'}
                    </strong>
                    <p class="nge-open-warnings__summary">
                        Some values in this file could not be read as written and were
                        corrected or ignored.
                    </p>
                    <details class="nge-open-warnings__details">
                        <summary class="nge-open-warnings__toggle">Details</summary>
                        <ul class="nge-open-warnings__list">
                            <For each={warnings()}>
                                {(w) => (
                                    <li
                                        class="nge-open-warnings__item"
                                        data-kind={w.kind}
                                        title={w.kind}
                                    >
                                        {describeReadWarning(w)}
                                    </li>
                                )}
                            </For>
                        </ul>
                    </details>
                </div>
                <button
                    class="nge-btn nge-open-warnings__dismiss"
                    type="button"
                    aria-label="Dismiss open issues notice"
                    onClick={() => setDismissed(() => warnings())}
                >
                    Dismiss
                </button>
            </div>
        </Show>
    );
};
