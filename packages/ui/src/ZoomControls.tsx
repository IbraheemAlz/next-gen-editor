/**
 * ZoomControls — status-bar zoom widget.
 *
 * Dispatches Command::SetZoom { scale }. The engine command already
 * exists in `crates/bridge/src/command.rs` but has no UI consumer
 * before this — biggest blast-radius win in Sprint 1 (UI Edition).
 *
 * Shortcuts:
 *   Ctrl+0 → 100%
 *   Ctrl+= → step up
 *   Ctrl+- → step down
 */
import {
    createEffect,
    createSignal,
    For,
    onCleanup,
    type Component,
} from 'solid-js';
import { createEditorCommands } from '@nge/core';
import './ZoomControls.css';

const PRESETS = [0.5, 0.75, 1.0, 1.25, 1.5, 2.0] as const;
const MIN = 0.25;
const MAX = 4.0;

export interface ZoomControlsProps {
    /** Initial zoom. Default 1.0. */
    defaultScale?: number;
    /** Whether to bind the Ctrl+0/+/- shortcuts. Default true. */
    bindShortcuts?: boolean;
}

function clamp(s: number): number {
    return Math.max(MIN, Math.min(MAX, s));
}

export const ZoomControls: Component<ZoomControlsProps> = (props) => {
    const cmd = createEditorCommands();
    const [scale, setScale] = createSignal(props.defaultScale ?? 1.0);

    const apply = async (next: number) => {
        const clamped = clamp(next);
        setScale(clamped);
        await cmd.setZoom(clamped);
    };

    const stepUp = () => void apply(scale() + 0.1);
    const stepDown = () => void apply(scale() - 0.1);
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
                {!PRESETS.includes(scale() as (typeof PRESETS)[number]) && (
                    <option value={scale().toString()}>
                        {Math.round(scale() * 100)}%
                    </option>
                )}
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
