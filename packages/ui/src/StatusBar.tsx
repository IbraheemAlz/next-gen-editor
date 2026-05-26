/**
 * StatusBar — pinned bottom strip with document stats + lifecycle
 * affordances. Sprint 8 (UI Edition) consolidates several pieces
 * that previously lived in disparate toolbar slots:
 *
 *   - Live paragraph / word / character counts from `state.stats()`.
 *     EngineStats was extended this sprint to carry these fields.
 *   - BCP-47 language tag picker (`en-US`, `ar-EG`, …) dispatching
 *     `Command::ApplyFormatting { patch.language }` against the
 *     current selection.
 *   - Zoom controls relocated from Sprint 1's toolbar to where they
 *     conventionally live in a word processor.
 *   - Active renderer indicator (`Canvas2D` / `Vello`) — read-only
 *     status; the `SettingsMenu` below offers the reload-to-switch
 *     affordance.
 *
 * Component is purely visual; mutations all go through the SDK
 * commands or the embedded `ZoomControls` / `SettingsMenu`.
 */
import { createEffect, createSignal, onCleanup, type Component } from 'solid-js';
import { createEditorCommands, createEditorState } from '@nge/core';
import { ZoomControls } from './ZoomControls';
import { SettingsMenu } from './SettingsMenu';
import './StatusBar.css';

const LANGUAGES: { value: string; label: string }[] = [
    { value: 'en-US', label: 'English (US)' },
    { value: 'en-GB', label: 'English (UK)' },
    { value: 'ar-EG', label: 'Arabic (EG)' },
    { value: 'ar-SA', label: 'Arabic (SA)' },
    { value: 'fr-FR', label: 'French' },
    { value: 'de-DE', label: 'German' },
    { value: 'es-ES', label: 'Spanish' },
    { value: 'zh-CN', label: 'Chinese (Simplified)' },
    { value: 'ja-JP', label: 'Japanese' },
    { value: 'he-IL', label: 'Hebrew' },
];

export interface StatusBarProps {
    /** Stats polling cadence in ms. Default 1500. */
    pollMs?: number;
}

export const StatusBar: Component<StatusBarProps> = (props) => {
    const cmd = createEditorCommands();
    const state = createEditorState();
    const [lang, setLang] = createSignal('en-US');

    /* Poll stats so the counts update as the user types. The DevHud
     * polls separately when visible; the cadences cooperate. */
    createEffect(() => {
        const ms = props.pollMs ?? 1500;
        let cancelled = false;
        const tick = async () => {
            if (cancelled) return;
            try {
                await cmd.requestStats();
            } catch {
                /* worker may have trapped — recovery handles it */
            }
        };
        void tick();
        const id = window.setInterval(tick, ms);
        onCleanup(() => {
            cancelled = true;
            window.clearInterval(id);
        });
    });

    const applyLanguage = async (v: string) => {
        setLang(v);
        if (!state.selection()) return;
        await cmd.applyAttrs({ language: v });
    };

    return (
        <div class="nge-statusbar" role="status">
            <div class="nge-statusbar__group">
                <span class="nge-statusbar__stat" title="Paragraphs">
                    ¶ {state.stats()?.paragraph_count ?? '–'}
                </span>
                <span class="nge-statusbar__sep" aria-hidden="true">·</span>
                <span class="nge-statusbar__stat" title="Words">
                    {state.stats()?.word_count ?? '–'} words
                </span>
                <span class="nge-statusbar__sep" aria-hidden="true">·</span>
                <span class="nge-statusbar__stat" title="Characters (with spaces)">
                    {state.stats()?.character_count ?? '–'} chars
                </span>
            </div>

            <div class="nge-statusbar__spacer" />

            <div class="nge-statusbar__group">
                <label class="nge-statusbar__field">
                    <span class="nge-statusbar__sublabel" aria-hidden="true">🌐</span>
                    <select
                        class="nge-statusbar__select"
                        aria-label="Language"
                        value={lang()}
                        onChange={(e) => void applyLanguage(e.currentTarget.value)}
                    >
                        {LANGUAGES.map((l) => (
                            <option value={l.value}>{l.label}</option>
                        ))}
                    </select>
                </label>

                <span class="nge-statusbar__renderer" title="Active renderer">
                    {state.renderer() === 'vello' ? '⚡ Vello' : '🎨 Canvas2D'}
                </span>

                <ZoomControls bindShortcuts={false} />

                <SettingsMenu />
            </div>
        </div>
    );
};
