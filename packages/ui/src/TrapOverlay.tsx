/**
 * TrapOverlay — full-screen modal that takes over when the engine
 * worker traps (WASM `unreachable` / `RuntimeError`).
 *
 * Subscribes to `Event::Trap { stack }`; on receipt, dims the editor
 * surface, shows the stack trace, and offers recovery actions. It goes
 * away on `Event::Recovered`.
 *
 * Issue #330 — no button here may lose the document. A page reload is a
 * NEW session whose boot clears the IndexedDB event log, so:
 *   - "Reload engine" restarts the engine IN PLACE from the log
 *     (`engine.restartInPlace()` -> fresh canvas generation -> `recover()`);
 *   - "Reload page" first asks the client to carry the document across
 *     the reload (`engine.prepareCarryOver()`; the next boot recovers from
 *     the log once), and is not offered when the client cannot;
 *   - "Discard document" is the only action that drops the log, behind an
 *     explicit confirmation: it reloads WITHOUT a carry-over.
 * Hosts may override any action with the matching prop.
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
    /** Host override for "Reload engine". Default: the engine's
     *  `restartInPlace()`; the button is hidden when neither exists. */
    onReload?: () => void | Promise<void>;
    /** Host override for "Reload page". Default: `prepareCarryOver()` then
     *  `location.reload()`; hidden when the engine cannot carry the
     *  document across a reload. */
    onReloadPage?: () => void | Promise<void>;
    /** Host override for "Discard document" (default: a plain page reload,
     *  i.e. a new session, which clears the event log). */
    onDiscard?: () => void | Promise<void>;
}

export const TrapOverlay: Component<TrapOverlayProps> = (props) => {
    const engine = useEngine();
    const [stack, setStack] = createSignal<string | null>(null);
    const [busy, setBusy] = createSignal(false);
    const [confirmDiscard, setConfirmDiscard] = createSignal(false);

    createEffect(() => {
        const unsub = engine.subscribe((evt) => {
            if (evt.type === 'TRAP') {
                setStack(evt.stack || '(no stack available)');
            }
            if (evt.type === 'RECOVERED') {
                setStack(null);
                setConfirmDiscard(false);
            }
        });
        onCleanup(unsub);
    });

    const reloadEngine =
        props.onReload ?? (engine.restartInPlace ? () => engine.restartInPlace!() : undefined);
    const reloadPage =
        props.onReloadPage ??
        (engine.prepareCarryOver
            ? async () => {
                  await engine.prepareCarryOver!();
                  window.location.reload();
              }
            : undefined);
    const discard = props.onDiscard ?? (() => window.location.reload());

    const run = (action: () => void | Promise<void>) => async () => {
        setBusy(true);
        try {
            await action();
        } catch (e: unknown) {
            console.error('[trap-overlay] action failed', e);
        } finally {
            setBusy(false);
        }
    };

    const dismiss = () => setStack(null);

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
                            The WASM engine trapped. Your document is kept in the
                            IndexedDB event log and is being restored; nothing is
                            cleared unless you choose to discard it.
                        </p>
                        <pre class="nge-trap__stack">{stack()}</pre>
                        <footer class="nge-trap__actions">
                            <Show
                                when={confirmDiscard()}
                                fallback={
                                    <button
                                        class="nge-btn"
                                        type="button"
                                        disabled={busy()}
                                        onClick={() => setConfirmDiscard(true)}
                                    >
                                        Discard document
                                    </button>
                                }
                            >
                                <button
                                    class="nge-btn"
                                    type="button"
                                    disabled={busy()}
                                    onClick={run(discard)}
                                >
                                    Confirm: discard unsaved changes
                                </button>
                            </Show>
                            <button class="nge-btn" type="button" onClick={dismiss}>
                                Dismiss
                            </button>
                            <Show when={reloadPage}>
                                {(action) => (
                                    <button
                                        class="nge-btn"
                                        type="button"
                                        disabled={busy()}
                                        onClick={run(action())}
                                    >
                                        Reload page (keeps document)
                                    </button>
                                )}
                            </Show>
                            <Show when={reloadEngine}>
                                {(action) => (
                                    <button
                                        class="nge-btn nge-btn--primary"
                                        type="button"
                                        disabled={busy()}
                                        onClick={run(action())}
                                    >
                                        Reload engine
                                    </button>
                                )}
                            </Show>
                        </footer>
                    </div>
                </div>
            </Portal>
        </Show>
    );
};
