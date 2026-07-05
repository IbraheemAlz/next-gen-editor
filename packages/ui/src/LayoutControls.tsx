/**
 * LayoutControls — toolbar ribbon for section + paragraph layout.
 *
 * Controls:
 *   1. Columns dropdown (1 / 2 / 3) → `Command::SetColumns` against the
 *      section containing the caret. Gutter is a fixed 36 pt (½ inch,
 *      Word's stock value). Future revisions can split the gutter into
 *      a separate picker.
 *   2. Breaks menu (Phase 3, #40) — Page Break (`Ctrl+Enter`,
 *      `Command::InsertPageBreak`, a paragraph render hint) plus the two
 *      REAL section breaks (`Command::InsertSectionBreak` Next Page /
 *      Continuous, which split the document into sections with
 *      independent `<w:sectPr>` geometry). The section entries disable
 *      inside table cells — the engine rejects a cell-anchored section
 *      boundary (an interior sectPr is a body-paragraph property), and
 *      Honest UX forbids a button that silently errors.
 *   3. Paragraph Borders button → opens `ParagraphBordersDialog`, the
 *      full per-edge style/width/colour picker (issue #41). Its active
 *      state reflects the live `paragraph_borders` read-back.
 *
 * See `crates/bridge/src/command.rs` for the wire types and
 * `crates/engine/src/lib.rs::set_section_columns_at` /
 * `::set_page_break_before` / `::insert_section_break_at` /
 * `::set_paragraph_borders` for the mutation paths.
 */
import {
    createEffect,
    createMemo,
    createSignal,
    onCleanup,
    Show,
    type Component,
} from 'solid-js';
import {
    createEditorCommands,
    createEditorState,
    type SectionBreakKind,
} from '@nge/core';
import { PageSetupDialog } from './PageSetupDialog';
import { ParagraphBordersDialog } from './ParagraphBordersDialog';
import './LayoutControls.css';

const DEFAULT_GUTTER_PT = 36;

export interface LayoutControlsProps {
    /** Whether to bind `Ctrl+Enter` to InsertPageBreak. Default true. */
    bindShortcuts?: boolean;
    /** Gutter (pt) for SetColumns when count > 1. Default 36 pt. */
    gutterPt?: number;
}

export const LayoutControls: Component<LayoutControlsProps> = (props) => {
    const cmd = createEditorCommands();
    const state = createEditorState();
    const [columnCount, setColumnCount] = createSignal(1);
    const [pageSetupOpen, setPageSetupOpen] = createSignal(false);
    const [bordersOpen, setBordersOpen] = createSignal(false);

    /* Active state reflects the live document (issue #41 read-back):
       the button lights up whenever the caret paragraph has any edge. */
    const hasBorders = createMemo(() => {
        const b = state.paragraphBorders();
        return (
            !!b &&
            (b.top !== undefined ||
                b.right !== undefined ||
                b.bottom !== undefined ||
                b.left !== undefined)
        );
    });

    const gutter = () => props.gutterPt ?? DEFAULT_GUTTER_PT;

    /* Caret-availability gate: every layout command needs a caret. The
     * SDK helpers throw when state.selection() is undefined, so disable
     * the controls until the first SELECTION_CHANGED arrives. Phase 3
     * (#39): every command this ribbon dispatches (columns, breaks,
     * borders, page setup) is engine-gated during header/footer story
     * editing — grey the whole ribbon rather than let clicks die in an
     * invisible Event::Error (Honest UX). */
    const hasCaret = createMemo(() => state.selection() !== undefined);
    const inStory = createMemo(() => state.editingStory() !== undefined);
    const ready = createMemo(() => hasCaret() && !inStory());

    const setColumns = async (n: number) => {
        if (!ready()) return;
        setColumnCount(n);
        await cmd.setColumnsAtCaret(n, gutter());
    };

    const [breaksOpen, setBreaksOpen] = createSignal(false);

    /* Phase 3 (#40) — section boundaries cannot live inside table
     * cells; the entries grey out there rather than silently erroring. */
    const inTable = createMemo(() => state.cellProperties() !== undefined);

    const insertBreak = async () => {
        if (!ready()) return;
        setBreaksOpen(false);
        await cmd.insertPageBreak();
    };

    const insertSectionBreak = async (kind: SectionBreakKind) => {
        if (!ready() || inTable()) return;
        setBreaksOpen(false);
        await cmd.insertSectionBreak(kind);
    };

    /* Ctrl+Enter — Word's page-break shortcut. */
    createEffect(() => {
        if (props.bindShortcuts === false) return;
        const handler = (e: KeyboardEvent) => {
            const ctrl = e.ctrlKey || e.metaKey;
            if (ctrl && e.key === 'Enter') {
                e.preventDefault();
                void insertBreak();
            }
        };
        window.addEventListener('keydown', handler);
        onCleanup(() => window.removeEventListener('keydown', handler));
    });

    return (
        <div class="nge-layout" role="group" aria-label="Layout">
            <label class="nge-layout__field">
                <span class="nge-layout__label">Columns</span>
                <select
                    class="nge-layout__select"
                    aria-label="Section columns"
                    disabled={!ready()}
                    value={columnCount().toString()}
                    onChange={(e) =>
                        void setColumns(parseInt(e.currentTarget.value, 10))
                    }
                >
                    <option value="1">1</option>
                    <option value="2">2</option>
                    <option value="3">3</option>
                </select>
            </label>

            <div class="nge-layout__breaks">
                <button
                    class="nge-btn nge-layout__btn"
                    type="button"
                    aria-haspopup="menu"
                    aria-expanded={breaksOpen()}
                    aria-label="Insert break"
                    title="Insert a page or section break"
                    disabled={!ready()}
                    onClick={() => setBreaksOpen((v) => !v)}
                >
                    <span class="nge-layout__icon" aria-hidden="true">↵</span>
                    <span>Breaks</span>
                    <span aria-hidden="true">▾</span>
                </button>
                <Show when={breaksOpen()}>
                    <ul
                        class="nge-layout__breaks-menu"
                        role="menu"
                        onMouseLeave={() => setBreaksOpen(false)}
                    >
                        <li role="none">
                            <button
                                role="menuitem"
                                class="nge-layout__breaks-item"
                                type="button"
                                title="Force the next line onto a new page (Ctrl+Enter)"
                                onClick={() => void insertBreak()}
                            >
                                <span>Page Break</span>
                                <span class="nge-layout__breaks-kbd">Ctrl+Enter</span>
                            </button>
                        </li>
                        <li role="separator" class="nge-layout__breaks-sep" />
                        <li role="none">
                            <button
                                role="menuitem"
                                class="nge-layout__breaks-item"
                                type="button"
                                disabled={inTable()}
                                title={
                                    inTable()
                                        ? 'Section breaks cannot be inserted inside a table'
                                        : 'Start a new section on the next page'
                                }
                                onClick={() => void insertSectionBreak('NextPage')}
                            >
                                <span>Section Break (Next Page)</span>
                            </button>
                        </li>
                        <li role="none">
                            <button
                                role="menuitem"
                                class="nge-layout__breaks-item"
                                type="button"
                                disabled={inTable()}
                                title={
                                    inTable()
                                        ? 'Section breaks cannot be inserted inside a table'
                                        : 'Start a new section on the same page'
                                }
                                onClick={() => void insertSectionBreak('Continuous')}
                            >
                                <span>Section Break (Continuous)</span>
                            </button>
                        </li>
                        <li role="none">
                            <button
                                role="menuitem"
                                class="nge-layout__breaks-item"
                                type="button"
                                disabled={inTable()}
                                title={
                                    inTable()
                                        ? 'Section breaks cannot be inserted inside a table'
                                        : 'Start a new section on the next EVEN-numbered page (a blank filler page is added when needed)'
                                }
                                onClick={() => void insertSectionBreak('EvenPage')}
                            >
                                <span>Section Break (Even Page)</span>
                            </button>
                        </li>
                        <li role="none">
                            <button
                                role="menuitem"
                                class="nge-layout__breaks-item"
                                type="button"
                                disabled={inTable()}
                                title={
                                    inTable()
                                        ? 'Section breaks cannot be inserted inside a table'
                                        : 'Start a new section on the next ODD-numbered page (a blank filler page is added when needed — book chapters)'
                                }
                                onClick={() => void insertSectionBreak('OddPage')}
                            >
                                <span>Section Break (Odd Page)</span>
                            </button>
                        </li>
                    </ul>
                </Show>
            </div>

            <button
                class="nge-btn nge-layout__btn"
                type="button"
                aria-label="Paragraph borders"
                aria-pressed={hasBorders()}
                data-active={hasBorders()}
                title="Paragraph borders…"
                disabled={!ready()}
                onClick={() => setBordersOpen(true)}
            >
                <span class="nge-layout__borders-icon" aria-hidden="true">▢</span>
                <span>Borders…</span>
            </button>

            <button
                class="nge-btn nge-layout__btn"
                type="button"
                aria-label="Page setup"
                title="Page setup (margins + orientation)"
                disabled={!ready()}
                onClick={() => setPageSetupOpen(true)}
            >
                <span class="nge-layout__icon" aria-hidden="true">▤</span>
                <span>Page Setup…</span>
            </button>

            <Show when={!hasCaret()}>
                <span class="nge-layout__hint" role="status">
                    Place the caret in the document to enable layout controls.
                </span>
            </Show>

            <PageSetupDialog
                open={pageSetupOpen()}
                onClose={() => setPageSetupOpen(false)}
            />

            <ParagraphBordersDialog
                open={bordersOpen()}
                onClose={() => setBordersOpen(false)}
            />
        </div>
    );
};
