/**
 * LayoutControls — toolbar ribbon for section + paragraph layout.
 *
 * Three controls:
 *   1. Columns dropdown (1 / 2 / 3) → `Command::SetColumns` against the
 *      section containing the caret. Gutter is a fixed 36 pt (½ inch,
 *      Word's stock value). Future revisions can split the gutter into
 *      a separate picker.
 *   2. Insert Page Break button + `Ctrl+Enter` shortcut →
 *      `Command::InsertPageBreak` at the current caret.
 *   3. Paragraph Borders button → opens `ParagraphBordersDialog`, the
 *      full per-edge style/width/colour picker (issue #41). Its active
 *      state reflects the live `paragraph_borders` read-back.
 *
 * All three commands shipped in the Sprint 2 (UI Edition) Rust bridge
 * additions — see `crates/bridge/src/command.rs` for the wire types and
 * `crates/engine/src/lib.rs::set_section_columns_at` /
 * `::set_page_break_before` / `::set_paragraph_borders` for the
 * mutation paths.
 */
import {
    createEffect,
    createMemo,
    createSignal,
    onCleanup,
    Show,
    type Component,
} from 'solid-js';
import { createEditorCommands, createEditorState } from '@nge/core';
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
     * the controls until the first SELECTION_CHANGED arrives. */
    const ready = createMemo(() => state.selection() !== undefined);

    const setColumns = async (n: number) => {
        if (!ready()) return;
        setColumnCount(n);
        await cmd.setColumnsAtCaret(n, gutter());
    };

    const insertBreak = async () => {
        if (!ready()) return;
        await cmd.insertPageBreak();
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

            <button
                class="nge-btn nge-layout__btn"
                type="button"
                aria-label="Insert page break"
                title="Insert page break (Ctrl+Enter)"
                disabled={!ready()}
                onClick={() => void insertBreak()}
            >
                <span class="nge-layout__icon" aria-hidden="true">↵</span>
                <span>Page break</span>
            </button>

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

            <Show when={!ready()}>
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
