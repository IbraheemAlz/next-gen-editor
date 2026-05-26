/**
 * SettingsMenu — gear-icon popover with engine lifecycle controls.
 *
 * Active renderer (Canvas2D / Vello) is read-only at runtime: the
 * choice is baked at `Engine::new` vs `Engine::with_vello` boot.
 * Switching requires a fresh worker spawn — surfaced here as a
 * "Switch to Vello" / "Switch to Canvas2D" button that reloads the
 * page with `?renderer=vello` or `?renderer=canvas2d`. The TS shell
 * worker boot reads the URL parameter and routes accordingly.
 *
 * Toggle Dev HUD action mirrors the `Ctrl+Shift+D` shortcut so the
 * keyboard-averse can still find it. The HUD itself owns its
 * visibility signal; the SettingsMenu fires a `CustomEvent` on
 * `window` that the HUD subscribes to.
 */
import { createSignal, onCleanup, Show, type Component } from 'solid-js';
import { useEngine } from '@nge/core';
import './SettingsMenu.css';

export const SettingsMenu: Component = () => {
    const engine = useEngine();
    const [open, setOpen] = createSignal(false);

    const switchRenderer = (target: 'vello' | 'canvas2d') => {
        const url = new URL(window.location.href);
        url.searchParams.set('renderer', target);
        window.location.assign(url.toString());
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

    const isVello = () => engine.renderer === 'vello';

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
                        <div class="nge-settings__row">
                            <button
                                class="nge-btn nge-settings__action"
                                type="button"
                                disabled={isVello()}
                                onClick={() => switchRenderer('vello')}
                            >
                                Switch to Vello…
                            </button>
                            <button
                                class="nge-btn nge-settings__action"
                                type="button"
                                disabled={!isVello()}
                                onClick={() => switchRenderer('canvas2d')}
                            >
                                Switch to Canvas2D…
                            </button>
                        </div>
                        <div class="nge-settings__hint">
                            Switching reloads the page so the worker
                            can pick a fresh backend at boot.
                        </div>
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
                </div>
            </Show>
        </div>
    );
};
