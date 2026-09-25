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
 * Issue #280 — zoom now visibly resizes the page card, so "Fit width" is
 * meaningful: the Fit button enters a sticky mode that computes the zoom
 * from the editor viewport's content width ÷ the page's 100 % width and
 * re-fits whenever the viewport resizes. Any other zoom (a preset, a
 * step, Ctrl+0, a raw `SET_ZOOM`) leaves the mode. The mode is shared by
 * every widget on the same engine, like the zoom itself.
 *
 * Shortcuts:
 *   Ctrl+0 → 100%
 *   Ctrl+= → step up
 *   Ctrl+- → step down
 */
import {
    createEffect,
    createRoot,
    createSignal,
    For,
    onCleanup,
    Show,
    untrack,
    type Accessor,
    type Component,
} from 'solid-js';
import {
    createEditorCommands,
    createEditorState,
    useEngine,
    type EngineHandle,
} from '@nge/core';
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

/** A4 portrait width in CSS px at 100 % (595.3 pt × 96/72) — the fit
 *  fallback before the page card has been laid out. */
const A4_WIDTH_CSS = (595.3 * 96) / 72;

/**
 * Issue #280 — the zoom at which page 0 fills the editor viewport's
 * content width (padding excluded, vertical scrollbar excluded via
 * `clientWidth`), floored to a whole percent so the page never
 * overflows horizontally. The page's 100 % width is its current card
 * width ÷ the current zoom, so a landscape / custom first section fits
 * too. `null` when the shell has no `.editor-viewport` (a host without
 * the default desk).
 */
export function fitWidthZoom(currentZoom: number): number | null {
    const viewport = document.querySelector<HTMLElement>('.editor-viewport');
    if (!viewport) return null;
    const cs = getComputedStyle(viewport);
    const avail =
        viewport.clientWidth -
        (parseFloat(cs.paddingLeft) || 0) -
        (parseFloat(cs.paddingRight) || 0);
    if (!(avail > 0)) return null;
    const page = document.querySelector<HTMLElement>('.editor-page[data-page-index="0"]');
    const cardW = page?.getBoundingClientRect().width ?? 0;
    const pageW100 = cardW > 0 && currentZoom > 0 ? cardW / currentZoom : A4_WIDTH_CSS;
    return clamp(Math.floor((avail / pageW100) * 100) / 100);
}

/** Engine-wide Fit-width mode (one per engine, like the zoom). */
interface FitWidthState {
    fit: Accessor<boolean>;
    setFit: (on: boolean) => void;
}
const fitStates = new WeakMap<EngineHandle, FitWidthState>();

function fitStateFor(
    engine: EngineHandle,
    zoom: Accessor<number>,
    setZoom: (z: number) => Promise<unknown>,
): FitWidthState {
    const existing = fitStates.get(engine);
    if (existing) return existing;
    const state = createRoot(() => {
        const [fit, setFit] = createSignal(false);
        /* The zoom this mode last asked for; a zoom that lands anywhere
           else came from another control → leave the mode. */
        let target: number | undefined;
        const refit = (): void => {
            const current = untrack(zoom);
            const z = fitWidthZoom(current);
            if (z === null) return;
            if (Math.abs(z - current) < 0.005) {
                target = z;
                return;
            }
            /* Already asked for this zoom; its reply is in flight (the
               observer's initial callback lands right after the first
               fit). */
            if (target !== undefined && Math.abs(z - target) < 0.005) return;
            target = z;
            setZoom(z).catch(() => {
                /* Worker trapped mid-dispatch — recovery re-syncs. */
            });
        };
        createEffect(() => {
            if (!fit()) return;
            refit();
            const viewport = document.querySelector<HTMLElement>('.editor-viewport');
            if (!viewport || typeof ResizeObserver === 'undefined') return;
            const ro = new ResizeObserver(() => refit());
            ro.observe(viewport);
            onCleanup(() => ro.disconnect());
        });
        createEffect(() => {
            const z = zoom();
            if (untrack(fit) && target !== undefined && Math.abs(z - target) >= 0.005) {
                setFit(false);
            }
        });
        return {
            fit,
            setFit: (on: boolean) => {
                if (!on) target = undefined;
                setFit(on);
            },
        };
    });
    fitStates.set(engine, state);
    return state;
}

export const ZoomControls: Component<ZoomControlsProps> = (props) => {
    const cmd = createEditorCommands();
    const state = createEditorState();
    const scale = state.zoom;
    const fitMode = fitStateFor(useEngine(), scale, (z) => cmd.setZoom(z));
    let selectEl: HTMLSelectElement | undefined;

    const apply = async (next: number) => {
        /* Any explicit zoom leaves the Fit-width mode. */
        fitMode.setFit(false);
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
            <button
                class="nge-btn nge-zoom__fit"
                type="button"
                aria-label="Fit page width"
                aria-pressed={fitMode.fit()}
                title="Fit page width to the window"
                onClick={() => fitMode.setFit(!fitMode.fit())}
            >
                Fit
            </button>
        </div>
    );
};
