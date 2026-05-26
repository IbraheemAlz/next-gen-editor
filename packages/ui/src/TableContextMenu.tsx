/**
 * TableContextMenu — floating menu that appears when the user
 * right-clicks (or invokes the "table actions" floater) inside a
 * `<w:tbl>`.
 *
 * Driven entirely by `createEditorState().selectionKind` +
 * `selection().end.path`:
 *   - SelectionKind = `{ kind: "TABLE_CELLS", table_path, from_row,
 *     from_col, to_row, to_col }` → multi-cell selection. Merge is
 *     enabled, single-cell operations target the anchor cell.
 *   - SelectionKind = `{ kind: "LINEAR" }` with a `Cell` step in the
 *     caret path → single-cell caret. Single-cell operations enabled;
 *     Merge disabled.
 *   - Otherwise → not inside a table; the menu is suppressed.
 *
 * The component owns no pointer wiring of its own beyond a `contextmenu`
 * listener installed on a `target` ref (defaults to `document`). Hosts
 * that want menu-on-click (e.g., a floating "Table" affordance) supply
 * their own trigger via `triggerRef` + `open()` imperative handle.
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
    type BlockPath,
    type PathStep,
} from '@nge/core';
import { CellPropertiesDialog } from './CellPropertiesDialog';
import './TableContextMenu.css';

interface TableContext {
    tablePath: BlockPath;
    row: number;
    col: number;
    /* When the user has selected a rectangle of cells. */
    selection?: {
        fromRow: number;
        fromCol: number;
        toRow: number;
        toCol: number;
    };
}

/** Walk a BlockPath looking for a TABLE_CELLS-like prefix: a Block
 *  followed by a Cell. Returns the table path (everything up to and
 *  including the table's Block step) + the cell coordinates. */
function resolveTableContext(steps: PathStep[]): {
    tablePath: BlockPath;
    row: number;
    col: number;
} | undefined {
    for (let i = 0; i < steps.length - 1; i++) {
        const here = steps[i];
        const next = steps[i + 1];
        if (here && next && here.kind === 'BLOCK' && next.kind === 'CELL') {
            return {
                tablePath: { steps: steps.slice(0, i + 1) },
                row: next.row,
                col: next.col,
            };
        }
    }
    return undefined;
}

export interface TableContextMenuProps {
    /** Element to install the contextmenu listener on. Defaults to
     *  document.body so right-click anywhere inside the canvas surface
     *  triggers the menu when the caret is inside a table. */
    target?: HTMLElement | (() => HTMLElement | undefined);
}

export const TableContextMenu: Component<TableContextMenuProps> = (props) => {
    const cmd = createEditorCommands();
    const state = createEditorState();
    const [open, setOpen] = createSignal(false);
    const [pos, setPos] = createSignal<{ x: number; y: number }>({ x: 0, y: 0 });
    /* Cell-properties dialog state. Captures the table context at
     * the moment the user clicks "Cell Properties…" so the menu
     * dismissal doesn't drop the selection mid-flight. */
    const [propsOpen, setPropsOpen] = createSignal(false);
    const [propsCtx, setPropsCtx] = createSignal<TableContext | undefined>(undefined);

    const ctx = createMemo<TableContext | undefined>(() => {
        const kind = state.selectionKind();
        const sel = state.selection();
        if (kind?.kind === 'TABLE_CELLS') {
            return {
                tablePath: kind.table_path,
                row: kind.from_row,
                col: kind.from_col,
                selection: {
                    fromRow: kind.from_row,
                    fromCol: kind.from_col,
                    toRow: kind.to_row,
                    toCol: kind.to_col,
                },
            };
        }
        if (sel) {
            return resolveTableContext(sel.end.path.steps);
        }
        return undefined;
    });

    const close = () => setOpen(false);
    const isInTable = () => ctx() !== undefined;
    const isMultiCell = () => ctx()?.selection !== undefined;

    /* Listen for contextmenu on the target; suppress browser menu only
     * when we're actually inside a table — otherwise the user keeps the
     * native right-click. */
    createEffect(() => {
        const t =
            typeof props.target === 'function'
                ? props.target() ?? document.body
                : props.target ?? document.body;

        const onContext = (e: MouseEvent) => {
            if (!isInTable()) return;
            e.preventDefault();
            setPos({ x: e.clientX, y: e.clientY });
            setOpen(true);
        };
        const onAwayClick = (e: MouseEvent) => {
            if (!open()) return;
            const root = e.target as HTMLElement | null;
            if (root && root.closest('.nge-tcm')) return;
            close();
        };
        const onEsc = (e: KeyboardEvent) => {
            if (e.key === 'Escape') close();
        };

        t.addEventListener('contextmenu', onContext);
        window.addEventListener('mousedown', onAwayClick);
        window.addEventListener('keydown', onEsc);
        onCleanup(() => {
            t.removeEventListener('contextmenu', onContext);
            window.removeEventListener('mousedown', onAwayClick);
            window.removeEventListener('keydown', onEsc);
        });
    });

    /* Sprint 2 hotfix: insert-side is named explicitly so the bridge
     * never has to interpret `row - 1` math. `Before(row=0)` is the
     * canonical "insert above row 0" anchor — no underflow. */
    const insertAbove = async () => {
        const c = ctx();
        if (!c) return;
        await cmd.insertRow(c.tablePath, c.row, 'Before');
        close();
    };
    const insertBelow = async () => {
        const c = ctx();
        if (!c) return;
        await cmd.insertRow(c.tablePath, c.row, 'After');
        close();
    };
    const insertLeft = async () => {
        const c = ctx();
        if (!c) return;
        await cmd.insertColumn(c.tablePath, c.col, 'Before');
        close();
    };
    const insertRight = async () => {
        const c = ctx();
        if (!c) return;
        await cmd.insertColumn(c.tablePath, c.col, 'After');
        close();
    };
    const deleteRowFn = async () => {
        const c = ctx();
        if (!c) return;
        await cmd.deleteRow(c.tablePath, c.row);
        close();
    };
    const deleteColumnFn = async () => {
        const c = ctx();
        if (!c) return;
        await cmd.deleteColumn(c.tablePath, c.col);
        close();
    };
    const mergeSelected = async () => {
        const c = ctx();
        if (!c?.selection) return;
        await cmd.mergeCells(
            c.tablePath,
            c.selection.fromRow,
            c.selection.fromCol,
            c.selection.toRow,
            c.selection.toCol,
        );
        close();
    };
    const splitCellFn = async () => {
        const c = ctx();
        if (!c) return;
        await cmd.splitCell(c.tablePath, c.row, c.col);
        close();
    };
    const openCellProperties = () => {
        const c = ctx();
        if (!c) return;
        setPropsCtx(c);
        setPropsOpen(true);
        close();
    };
    const deleteTableFn = async () => {
        const c = ctx();
        if (!c) return;
        await cmd.deleteTable(c.tablePath);
        close();
    };

    return (
        <>
        <Show when={open() && isInTable()}>
            <ul
                class="nge-tcm"
                role="menu"
                aria-label="Table actions"
                style={{
                    left: `${pos().x}px`,
                    top: `${pos().y}px`,
                }}
            >
                <li role="presentation" class="nge-tcm__group">Rows</li>
                <li role="none">
                    <button class="nge-tcm__item" type="button" role="menuitem" onClick={() => void insertAbove()}>
                        Insert row above
                    </button>
                </li>
                <li role="none">
                    <button class="nge-tcm__item" type="button" role="menuitem" onClick={() => void insertBelow()}>
                        Insert row below
                    </button>
                </li>
                <li role="none">
                    <button class="nge-tcm__item nge-tcm__item--danger" type="button" role="menuitem" onClick={() => void deleteRowFn()}>
                        Delete row
                    </button>
                </li>

                <li role="presentation" class="nge-tcm__separator" />
                <li role="presentation" class="nge-tcm__group">Columns</li>
                <li role="none">
                    <button class="nge-tcm__item" type="button" role="menuitem" onClick={() => void insertLeft()}>
                        Insert column left
                    </button>
                </li>
                <li role="none">
                    <button class="nge-tcm__item" type="button" role="menuitem" onClick={() => void insertRight()}>
                        Insert column right
                    </button>
                </li>
                <li role="none">
                    <button class="nge-tcm__item nge-tcm__item--danger" type="button" role="menuitem" onClick={() => void deleteColumnFn()}>
                        Delete column
                    </button>
                </li>

                <li role="presentation" class="nge-tcm__separator" />
                <li role="presentation" class="nge-tcm__group">Cells</li>
                <li role="none">
                    <button
                        class="nge-tcm__item"
                        type="button"
                        role="menuitem"
                        disabled={!isMultiCell()}
                        title={isMultiCell() ? 'Merge selected cells' : 'Select multiple cells to merge'}
                        onClick={() => void mergeSelected()}
                    >
                        Merge cells
                    </button>
                </li>
                <li role="none">
                    <button class="nge-tcm__item" type="button" role="menuitem" onClick={() => void splitCellFn()}>
                        Split cell
                    </button>
                </li>

                <li role="presentation" class="nge-tcm__separator" />
                <li role="none">
                    <button class="nge-tcm__item" type="button" role="menuitem" onClick={openCellProperties}>
                        Cell Properties…
                    </button>
                </li>
                <li role="presentation" class="nge-tcm__separator" />
                <li role="none">
                    <button class="nge-tcm__item nge-tcm__item--danger" type="button" role="menuitem" onClick={() => void deleteTableFn()}>
                        Delete table
                    </button>
                </li>
            </ul>
        </Show>
        {/* Dialog lives outside the menu Show so it survives the
            menu's immediate close-on-click. */}
        <CellPropertiesDialog
            open={propsOpen()}
            onClose={() => setPropsOpen(false)}
            tablePath={propsCtx()?.tablePath}
            row={propsCtx()?.row ?? 0}
            col={propsCtx()?.col ?? 0}
        />
        </>
    );
};
