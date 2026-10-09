/**
 * EditorSurface — the Locked Surface.
 *
 * Owns the <canvas> + hidden <textarea>, hands the OffscreenCanvas to the
 * worker exactly once (transferControlToOffscreen() is one-shot per
 * element), and re-mounts a fresh <canvas> on crash. The hidden textarea
 * is the only legitimate text-input source: pointer events live on the
 * canvas; keyboard + IME events live on the textarea.
 *
 * Downstream UI cannot draw on this surface — that's the point of the
 * "Locked" half of "Locked Surface with Headless Controls".
 *
 * Pointer / IME / a11y / overlay wiring is delegated to side modules
 * (see attachPointerHandlers / attachInputHandlers / attachA11y).
 * Those modules currently live in `ts/src/input/`; the migration into
 * @nge/core is queued as a follow-up sprint.
 */
import {
    createSignal,
    onCleanup,
    onMount,
    Show,
    type Component,
    type JSX,
} from 'solid-js';
import { useEngine } from './EngineProvider';
import { createCaretReveal } from './createCaretReveal';
import { createEditorState } from './createEditorState';
import { deviceRatioFor, offsetInViewport } from './caretReveal';

export interface EditorSurfaceProps {
    /** Logical canvas size in CSS pixels. Backing store sizes itself
     *  using devicePixelRatio. */
    width: number;
    height: number;
    /** Tested in the harness; default is `nge-editor-surface`. */
    className?: string;
    /** Forwarded to the hidden textarea — used by tests to assert focus. */
    textareaId?: string;
    /** Render-prop slot for DOM overlays (caret, selection, a11y mirror)
     *  that need to compose with the surface but never draw on the
     *  canvas itself. */
    overlays?: JSX.Element;
    /**
     * Issue #239 — a starting user-zoom fraction, dispatched as `SET_ZOOM`
     * right after `engine.init()`. The engine now queues a `SetZoom` sent
     * before its first `RenderPage` instead of dropping it (composing it
     * into the layout config the first paint builds), so this is safe
     * regardless of when the host's own boot sequence calls `RenderPage`
     * / `OpenDocument` relative to `init()`. Omit to boot at the engine's
     * default (100 %).
     */
    initialZoom?: number;
    /**
     * Issue #387 — the scrolling element the surface keeps the caret
     * visible in after keyboard navigation and typing (never after a
     * pointer click). Defaults to the nearest `.editor-viewport` ancestor;
     * a host without one (and without this prop) gets no auto-scroll.
     */
    viewport?: () => HTMLElement | undefined;
    /** Issue #387 — set `false` to opt out of the caret auto-scroll. */
    revealCaret?: boolean;
}

export const EditorSurface: Component<EditorSurfaceProps> = (props) => {
    const engine = useEngine();
    const state = createEditorState();
    const [canvasGen, setCanvasGen] = createSignal(0);
    let rootEl: HTMLDivElement | undefined;
    let canvasEl: HTMLCanvasElement | undefined;
    let textareaEl: HTMLTextAreaElement | undefined;

    /* Issue #387 — keep the caret inside the scrolling viewport after
       keyboard navigation and typing. The surface is one canvas at its
       top-left: the engine's device-px caret ÷ the zoom-aware device
       ratio is the CSS offset inside it. */
    createCaretReveal({
        enabled: () => props.revealCaret !== false,
        viewport: () =>
            props.viewport?.() ??
            rootEl?.closest<HTMLElement>('.editor-viewport') ??
            undefined,
        caretBox: (caret, viewport) => {
            if (!rootEl) return undefined;
            const ratio = deviceRatioFor(state.zoom(), window.devicePixelRatio || 1);
            const origin = offsetInViewport(rootEl, viewport);
            return {
                top: origin.top + caret.y / ratio,
                left: origin.left + caret.x / ratio,
                width: Math.max(1, caret.w / ratio),
                height: caret.h / ratio,
            };
        },
    });

    const mount = async (el: HTMLCanvasElement) => {
        canvasEl = el;
        const dpr = window.devicePixelRatio || 1;
        el.width = Math.round(props.width * dpr);
        el.height = Math.round(props.height * dpr);
        el.style.width = `${props.width}px`;
        el.style.height = `${props.height}px`;
        const off = el.transferControlToOffscreen();
        await engine.init(off);
        if (props.initialZoom !== undefined) {
            await engine.dispatch({ type: 'SET_ZOOM', scale: props.initialZoom });
        }
    };

    const handleCrash = () => {
        /* Bump the gen counter; <Show> remounts a fresh <canvas> so
         * transferControlToOffscreen can be called again. The dead
         * canvas is GC'd by the framework. */
        setCanvasGen((n) => n + 1);
    };

    /* Wire crash callback if the engine exposes it (the concrete
     * EngineClient does; mock clients in tests may not). */
    onMount(() => {
        const e = engine as unknown as { onCrash?: () => void };
        if ('onCrash' in e) e.onCrash = handleCrash;
    });

    onCleanup(() => {
        /* No explicit teardown — the worker survives component unmount
         * until the page unloads. Components that want hard shutdown
         * call cmd.closeDocument() + dispose. */
    });

    return (
        <div
            ref={(el) => (rootEl = el)}
            class={props.className ?? 'nge-editor-surface'}
            style={{ position: 'relative' }}
        >
            <Show when={canvasGen() >= 0} keyed>
                {/* The keyed Show guarantees a brand-new <canvas> element
                    every time canvasGen() bumps. Critical: do not reuse
                    a canvas whose OffscreenCanvas was already transferred. */}
                <canvas
                    ref={(el) => void mount(el)}
                    data-canvas-gen={canvasGen()}
                    style={{ display: 'block' }}
                />
            </Show>
            <textarea
                id={props.textareaId ?? 'nge-hidden-input'}
                ref={(el) => (textareaEl = el)}
                style={{
                    position: 'absolute',
                    inset: '0',
                    opacity: '0.01',
                    /* Safari skips events on opacity:0 — keep at 0.01. */
                    'pointer-events': 'auto',
                    background: 'transparent',
                    border: 'none',
                    outline: 'none',
                    resize: 'none',
                    color: 'transparent',
                    'caret-color': 'transparent',
                }}
                spellcheck={false}
                autocomplete="off"
                aria-label="Editor input"
            />
            {props.overlays}
        </div>
    );
};

/**
 * Imperative handle returned by the surface for tests + integrations
 * that need direct DOM refs. Not part of the public consumption path —
 * production code uses `useEngine()` + `createEditorCommands()`.
 */
export interface EditorSurfaceHandle {
    canvas: HTMLCanvasElement | undefined;
    textarea: HTMLTextAreaElement | undefined;
}
