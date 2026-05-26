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
}

export const EditorSurface: Component<EditorSurfaceProps> = (props) => {
    const engine = useEngine();
    const [canvasGen, setCanvasGen] = createSignal(0);
    let canvasEl: HTMLCanvasElement | undefined;
    let textareaEl: HTMLTextAreaElement | undefined;

    const mount = async (el: HTMLCanvasElement) => {
        canvasEl = el;
        const dpr = window.devicePixelRatio || 1;
        el.width = Math.round(props.width * dpr);
        el.height = Math.round(props.height * dpr);
        el.style.width = `${props.width}px`;
        el.style.height = `${props.height}px`;
        const off = el.transferControlToOffscreen();
        await engine.init(off);
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
        <div class={props.className ?? 'nge-editor-surface'} style={{ position: 'relative' }}>
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
