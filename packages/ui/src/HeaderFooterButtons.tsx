/**
 * HeaderFooterButtons — toolbar entry points for header/footer editing
 * (Phase 3, #39). The discoverable sibling of the double-click-in-margin
 * gesture:
 *
 *   - Body mode: "Header" / "Footer" buttons dispatch
 *     `Command::EnterHeaderFooter` for the CARET's page (derived from
 *     the caret rect against the engine-reported page tops — exact
 *     under mixed-geometry sections once a paginated paint has landed;
 *     page 0 fallback before that).
 *   - Story mode: a single "Close Header/Footer" button dispatches
 *     `Command::ExitHeaderFooter` (Esc and double-clicking the dimmed
 *     body do the same).
 *
 * The engine guarantees the target section owns a private part on
 * enter (materialize-on-enter / fork-on-enter — the v1 stand-in for
 * Word's Link-to-Previous, tracked as a follow-up issue).
 */
import { Show, createMemo, type Component } from 'solid-js';
import {
    createEditorCommands,
    createEditorState,
    type HeaderFooterArea,
} from '@nge/core';
import './HeaderFooterButtons.css';

export const HeaderFooterButtons: Component = () => {
    const cmd = createEditorCommands();
    const state = createEditorState();

    const ready = createMemo(() => state.selection() !== undefined);
    const story = () => state.editingStory();

    /** 0-based page the caret sits on — engine-exact page tops when a
     *  paginated paint has reported them, else page 0. */
    const caretPage = (): number => {
        const caret = state.caret();
        const geo = state.pageGeometry();
        if (!caret || geo.tops.length === 0) return 0;
        let page = 0;
        for (let i = 0; i < geo.tops.length; i += 1) {
            const top = geo.tops[i];
            if (top !== undefined && caret.y >= top) page = i;
        }
        return page;
    };

    const enter = async (area: HeaderFooterArea) => {
        if (!ready()) return;
        await cmd.enterHeaderFooter(caretPage(), area);
    };

    return (
        <div class="nge-hf" role="group" aria-label="Header and footer">
            <Show
                when={!story()}
                fallback={
                    <button
                        class="nge-btn nge-hf__btn nge-hf__btn--close"
                        type="button"
                        aria-label="Close header/footer editing"
                        title="Close header/footer editing (Esc)"
                        onClick={() => void cmd.exitHeaderFooter()}
                    >
                        <span aria-hidden="true">✕</span>
                        <span>
                            Close {story()?.area === 'Footer' ? 'Footer' : 'Header'}
                        </span>
                    </button>
                }
            >
                <button
                    class="nge-btn nge-hf__btn"
                    type="button"
                    aria-label="Edit header"
                    title="Edit the header of the caret's section (or double-click the top margin)"
                    disabled={!ready()}
                    onClick={() => void enter('Header')}
                >
                    <span class="nge-hf__icon" aria-hidden="true">▔</span>
                    <span>Header</span>
                </button>
                <button
                    class="nge-btn nge-hf__btn"
                    type="button"
                    aria-label="Edit footer"
                    title="Edit the footer of the caret's section (or double-click the bottom margin)"
                    disabled={!ready()}
                    onClick={() => void enter('Footer')}
                >
                    <span class="nge-hf__icon" aria-hidden="true">▁</span>
                    <span>Footer</span>
                </button>
            </Show>
        </div>
    );
};
