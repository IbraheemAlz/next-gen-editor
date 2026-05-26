/**
 * TrapOverlay — full-screen modal that takes over when the engine
 * worker traps (WASM `unreachable` / `RuntimeError`).
 *
 * Subscribes to `Event::Trap { stack }`; on receipt, dims the
 * editor surface, displays the stack trace in a monospace block,
 * and offers "Reload Engine" and "Reload Page" buttons.
 *
 * The actual recovery — bumping the canvas generation + remounting
 * the OffscreenCanvas + calling `engine.recover()` — is the host
 * application's responsibility (`onCrash` callback wiring on the
 * concrete `EngineClient`). This overlay is the visible half: it
 * informs the user that a crash happened and gives them an explicit
 * trigger.
 *
 * The `onReload` callback is invoked when the user clicks "Reload
 * Engine"; the host wires it to `engineClient.recover(fresh_canvas)`.
 * If no callback is supplied, the button falls back to a full page
 * reload (more disruptive but always safe).
 */
import {
    createEffect,
    createSignal,
    onCleanup,
    Show,
    type Component,
} from 'solid-js';
import { Portal } from 'solid-js/web';
import { useEngine } from '@nge/core';
import './TrapOverlay.css';

export interface TrapOverlayProps {
    /** Host-supplied recovery hook. If omitted, the button reloads
     *  the whole page instead. */
    onReload?: () => void;
}

export const TrapOverlay: Component<TrapOverlayProps> = (props) => {
    const engine = useEngine();
    const [stack, setStack] = createSignal<string | null>(null);

    createEffect(() => {
        const unsub = engine.subscribe((evt) => {
            if (evt.type === 'TRAP') {
                setStack(evt.stack || '(no stack available)');
            }
            if (evt.type === 'RECOVERED') {
                setStack(null);
            }
        });
        onCleanup(unsub);
    });

    const reload = () => {
        if (props.onReload) {
            props.onReload();
            return;
        }
        window.location.reload();
    };

    const dismiss = () => setStack(null);
    const reloadPage = () => window.location.reload();

    return (
        <Show when={stack() !== null}>
            <Portal>
                <div class="nge-trap" role="alertdialog" aria-modal="true" aria-labelledby="nge-trap-title">
                    <div class="nge-trap__card">
                        <header class="nge-trap__head">
                            <span class="nge-trap__icon" aria-hidden="true">✕</span>
                            <h2 class="nge-trap__title" id="nge-trap-title">
                                Engine crash
                            </h2>
                        </header>
                        <p class="nge-trap__lede">
                            The WASM engine trapped. Your last unsaved
                            edits since the most recent snapshot may be
                            lost. The document model itself is
                            recoverable from the IndexedDB event log.
                        </p>
                        <pre class="nge-trap__stack">{stack()}</pre>
                        <footer class="nge-trap__actions">
                            <button class="nge-btn" type="button" onClick={dismiss}>
                                Dismiss
                            </button>
                            <button class="nge-btn" type="button" onClick={reloadPage}>
                                Reload page
                            </button>
                            <button
                                class="nge-btn nge-btn--primary"
                                type="button"
                                onClick={reload}
                            >
                                Reload engine
                            </button>
                        </footer>
                    </div>
                </div>
            </Portal>
        </Show>
    );
};
