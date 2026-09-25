/**
 * ZoomControls — status-bar zoom widget.
 *
 * Dispatches Command::SetZoom { scale }. The engine command already
 * exists in `crates/bridge/src/command.rs` but has no UI consumer
 * before this — biggest blast-radius win in Sprint 1 (UI Edition).
 *
 * Issue #52 — the widget holds NO zoom state of its own. The displayed
 * value is `createEditorState().zoom`, the engine's zoom read back from
 * `SELECTION_CHANGED.zoom` (the reply to every `SET_ZOOM`) and shared
 * by every instance on the same engine. The shell mounts two of these
 * (the StatusBar's embedded one and the standalone footer widget);
 * changing zoom through either updates both, and a crash recovery that
 * lands on a different zoom re-syncs both (#97).
 *
 * Issue #239 — this widget never took an initial-zoom prop of its own
 * (the removed `defaultScale` was a no-op since #52 — "ignored", see
 * that issue's history). A host that wants a starting zoom now has a
 * real, non-deprecated path: `EditorSurface`'s `initialZoom` prop, or
 * `cmd.openDocument(bytes, name, { initialZoom })` — both dispatch
 * `SET_ZOOM` through the engine, which queues it even before the first
 * `RenderPage` instead of dropping it, so the value reaches every
 * `ZoomControls` (including this one) the normal way.
 *
 * Shortcuts:
 *   Ctrl+0 → 100%
 *   Ctrl+= → step up
 *   Ctrl+- → step down
 */
import { createEffect, For, onCleanup, Show, type Component } from 'solid-js';
import { createEditorCommands, createEditorState } from '@nge/core';
import './ZoomControls.css';

const PRESETS = [0.5, 0.75, 1.0, 1.25, 1.5, 2.0] as const;
const MIN = 0.25;
const MAX = 4.0;

export interface ZoomControlsProps {
    /** Whether to bind the Ctrl+0/+/- shortcuts. Default true. */
    bindShortcuts?: boolean;
}

function clamp(s: number): number {
    return Math.max(MIN, Math.min(MAX, s));
}

/** Two-decimal step arithmetic, so `1.0 + 0.1` lands on `1.1`, not
 *  `1.1000000000000001`. */
function step(s: number, delta: number): number {
    return Math.round((s + delta) * 100) / 100;
}

export const ZoomControls: Component<ZoomControlsProps> = (props) => {
    const cmd = createEditorCommands();
    const state = createEditorState();
    const scale = state.zoom;
    let selectEl: HTMLSelectElement | undefined;

    const apply = async (next: number) => {
        try {
            /* The reply's SELECTION_CHANGED.zoom updates `scale` for every
               widget; nothing to set locally. */
            await cmd.setZoom(clamp(next));
        } catch {
            /* Worker trapped mid-dispatch — recovery re-syncs the zoom. */
        } finally {
            /* A rejected / refused zoom must not leave the <select>
               showing the value the user picked but the engine refused. */
            if (selectEl) selectEl.value = scale().toString();
        }
    };

    const stepUp = () => void apply(step(scale(), 0.1));
    const stepDown = () => void apply(step(scale(), -0.1));
    const reset = () => void apply(1.0);

    /* Keyboard shortcuts. */
    createEffect(() => {
        if (props.bindShortcuts === false) return;
        const handler = (e: KeyboardEvent) => {
            const ctrl = e.ctrlKey || e.metaKey;
            if (!ctrl) return;
            switch (e.key) {
                case '0':
                    e.preventDefault();
                    reset();
                    break;
                case '+':
                case '=':
                    e.preventDefault();
                    stepUp();
                    break;
                case '-':
                case '_':
                    e.preventDefault();
                    stepDown();
                    break;
            }
        };
        window.addEventListener('keydown', handler);
        onCleanup(() => window.removeEventListener('keydown', handler));
    });

    /* The options (incl. the off-preset one) must exist before the value
       is assigned, or the <select> silently falls back to its first
       option — so the value is pushed from an effect, after render. */
    createEffect(() => {
        const v = scale().toString();
        if (selectEl && selectEl.value !== v) selectEl.value = v;
    });

    return (
        <div class="nge-zoom" role="group" aria-label="Zoom">
            <button
                class="nge-btn nge-btn--icon"
                type="button"
                aria-label="Zoom out"
                title="Zoom out (Ctrl+-)"
                onClick={stepDown}
            >
                −
            </button>
            <select
                ref={selectEl}
                class="nge-zoom__select"
                aria-label="Zoom level"
                value={scale().toString()}
                onChange={(e) => void apply(parseFloat(e.currentTarget.value))}
            >
                <For each={PRESETS}>
                    {(p) => (
                        <option value={p.toString()}>
                            {Math.round(p * 100)}%
                        </option>
                    )}
                </For>
                {/* Allow display of off-preset values without losing them. */}
                <Show when={!PRESETS.includes(scale() as (typeof PRESETS)[number])}>
                    <option value={scale().toString()}>
                        {Math.round(scale() * 100)}%
                    </option>
                </Show>
            </select>
            <button
                class="nge-btn nge-btn--icon"
                type="button"
                aria-label="Zoom in"
                title="Zoom in (Ctrl++)"
                onClick={stepUp}
            >
                +
            </button>
            <button
                class="nge-btn"
                type="button"
                aria-label="Reset zoom to 100%"
                title="Reset zoom (Ctrl+0)"
                onClick={reset}
            >
                100%
            </button>
        </div>
    );
};
