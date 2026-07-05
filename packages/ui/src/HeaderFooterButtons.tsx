/**
 * HeaderFooterButtons — toolbar entry points for header/footer editing
 * (Phase 3 #39, reworked for issues #70/#74). The discoverable sibling
 * of the double-click-in-margin gesture:
 *
 *   - Body mode: "Header" / "Footer" buttons dispatch
 *     `Command::EnterHeaderFooter` for the CARET's page (derived from
 *     the caret rect against the engine-reported page tops — exact
 *     under mixed-geometry sections once a paginated paint has landed;
 *     page 0 fallback before that).
 *   - Story mode: the Header & Footer cluster —
 *       · "Link to previous" toggle (issue #70): reflects
 *         `editingStory().linked` (Word's "Same as Previous");
 *         unlinking forks a private part, relinking clears the slot.
 *         Disabled on the first section (nothing precedes it).
 *       · "Different first page" checkbox (issue #74): the covering
 *         section's `<w:titlePg/>`; toggling may re-anchor the story
 *         to the First-role slot.
 *       · "Different odd & even" checkbox (issue #74): the
 *         document-wide `<w:evenAndOddHeaders/>` setting.
 *       · "Close" — `Command::ExitHeaderFooter` (Esc and
 *         double-clicking the dimmed body do the same).
 *
 * Entering resolves the band through §17.10.3 inheritance and edits
 * the resolved part in place (linked editing); a part is only
 * materialized when the whole chain is empty.
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

    /* Issue #70 — the toggle mirrors storage on every SelectionChanged:
       linked = the story's section has NO own slot (inherits). The
       first section can never link (engine rejects with an Error the
       FileMenu toast would surface; disable instead — Honest UX). */
    const linked = () => story()?.linked === true;
    const firstSection = () => story()?.section_index === 0;
    const titlePg = () => state.sectionGeometry()?.title_pg === true;
    const evenOdd = () => state.sectionGeometry()?.even_odd_headers === true;

    return (
        <div class="nge-hf" role="group" aria-label="Header and footer">
            <Show
                when={!story()}
                fallback={
                    <>
                        <button
                            class="nge-btn nge-hf__btn nge-hf__btn--link"
                            type="button"
                            aria-label="Link to previous section"
                            aria-pressed={linked()}
                            data-active={linked()}
                            title={
                                firstSection()
                                    ? 'The first section has no previous section to link to'
                                    : linked()
                                      ? 'Same as previous section — click to make this section’s band independent'
                                      : 'Independent — click to link back to the previous section’s band'
                            }
                            disabled={firstSection() && linked() === false}
                            onClick={() => void cmd.setHeaderFooterLink(!linked())}
                        >
                            <span aria-hidden="true">🔗</span>
                            <span>Link to previous</span>
                        </button>
                        <label
                            class="nge-hf__check"
                            title="First page of this section shows its own header/footer"
                        >
                            <input
                                type="checkbox"
                                checked={titlePg()}
                                onChange={(e) =>
                                    void cmd.setTitlePage(e.currentTarget.checked)
                                }
                            />
                            <span>Different first page</span>
                        </label>
                        <label
                            class="nge-hf__check"
                            title="Odd and even pages show different headers/footers (whole document)"
                        >
                            <input
                                type="checkbox"
                                checked={evenOdd()}
                                onChange={(e) =>
                                    void cmd.setEvenOddHeaders(e.currentTarget.checked)
                                }
                            />
                            <span>Different odd &amp; even</span>
                        </label>
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
                    </>
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
