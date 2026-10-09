/**
 * SettingsMenu — gear-icon popover with engine lifecycle controls.
 *
 * Active renderer (Canvas2D / Vello) is shown, and - issue #428 - switchable
 * IN PLACE when the engine offers `setRenderer` (`cmd.canSetRenderer`): the
 * #270 retire-respawn path restarts the engine as a recovery generation, so
 * the document, selection, undo history and zoom survive and the page never
 * reloads. (The old `?renderer=` URL toggle was never honoured by the
 * interactive app and is gone.) An engine without `setRenderer` shows the
 * active renderer only.
 *
 * Toggle Dev HUD action mirrors the `Ctrl+Shift+D` shortcut so the
 * keyboard-averse can still find it. The HUD itself owns its
 * visibility signal; the SettingsMenu fires a `CustomEvent` on
 * `window` that the HUD subscribes to.
 */
import { createSignal, onCleanup, Show, type Component } from 'solid-js';
import { createEditorCommands, createEditorState, useEngine, useTelemetryConfig } from '@nge/core';
import './SettingsMenu.css';

export const SettingsMenu: Component = () => {
    const engine = useEngine();
    const telemetry = useTelemetryConfig();
    const cmd = createEditorCommands();
    const state = createEditorState();
    const [switching, setSwitching] = createSignal(false);
    const [open, setOpen] = createSignal(false);

    const switchRenderer = async (target: 'vello' | 'canvas2d') => {
        setSwitching(true);
        try {
            await cmd.setRenderer(target);
        } catch (e: unknown) {
            console.error('[settings] renderer switch failed', e);
        } finally {
            setSwitching(false);
        }
    };

    const toggleHud = () => {
        window.dispatchEvent(new CustomEvent('nge-toggle-hud'));
        setOpen(false);
    };

    /* Click-away dismiss. */
    const onAway = (e: MouseEvent) => {
        if (!open()) return;
        const root = (e.target as HTMLElement | null)?.closest('.nge-settings');
        if (!root) setOpen(false);
    };
    window.addEventListener('mousedown', onAway);
    onCleanup(() => window.removeEventListener('mousedown', onAway));

    /* Reads the engine's live value (the editor-state signal is seeded at
     * mount, possibly before INIT answered) but also tracks that signal and
     * `switching`, so the buttons re-evaluate on the `RECOVERED` re-report
     * and when an in-place switch settles. */
    const isVello = () => {
        state.renderer();
        switching();
        return engine.renderer === 'vello';
    };

    return (
        <div class="nge-settings">
            <button
                class="nge-btn nge-btn--icon"
                type="button"
                aria-label="Settings"
                title="Settings"
                aria-expanded={open()}
                onClick={() => setOpen((v) => !v)}
            >
                ⚙
            </button>
            <Show when={open()}>
                <div class="nge-settings__menu" role="menu">
                    <div class="nge-settings__section">
                        <div class="nge-settings__heading">Renderer</div>
                        <div class="nge-settings__row">
                            <span class="nge-settings__current">
                                Active: <strong>{isVello() ? 'Vello (WebGPU)' : 'Canvas2D'}</strong>
                            </span>
                        </div>
                        <Show when={cmd.canSetRenderer}>
                            <div class="nge-settings__row">
                                <button
                                    class="nge-btn nge-settings__action"
                                    type="button"
                                    data-nge-renderer-target="vello"
                                    disabled={isVello() || switching()}
                                    onClick={() => void switchRenderer('vello')}
                                >
                                    Switch to Vello…
                                </button>
                                <button
                                    class="nge-btn nge-settings__action"
                                    type="button"
                                    data-nge-renderer-target="canvas2d"
                                    disabled={!isVello() || switching()}
                                    onClick={() => void switchRenderer('canvas2d')}
                                >
                                    Switch to Canvas2D…
                                </button>
                            </div>
                            <div class="nge-settings__hint">
                                Switching restarts the engine in place; your
                                document, selection and undo history are kept.
                            </div>
                        </Show>
                    </div>

                    <div class="nge-settings__separator" />

                    <div class="nge-settings__section">
                        <div class="nge-settings__heading">Diagnostics</div>
                        <div class="nge-settings__row">
                            <button
                                class="nge-btn nge-settings__action"
                                type="button"
                                onClick={toggleHud}
                            >
                                Toggle Dev HUD (Ctrl+Shift+D)
                            </button>
                        </div>
                        <div class="nge-settings__hint">
                            COI: <strong>{engine.crossOriginIsolated ? 'on' : 'off'}</strong>
                        </div>
                    </div>

                    <div class="nge-settings__separator" />

                    <div class="nge-settings__section">
                        <div class="nge-settings__heading">Telemetry</div>
                        <div class="nge-settings__row">
                            <label class="nge-settings__checkbox">
                                <input
                                    type="checkbox"
                                    checked={telemetry.enabled()}
                                    onChange={(e) => telemetry.setEnabled(e.currentTarget.checked)}
                                />
                                Share anonymous performance telemetry
                            </label>
                        </div>
                        <div class="nge-settings__hint">
                            Opt-in only, off by default. Never carries document
                            content or personal data — see Issue #86.
                        </div>
                    </div>
                </div>
            </Show>
        </div>
    );
};
