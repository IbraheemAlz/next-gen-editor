/* Phase 5 PR 3b — Tables side panel.
 *
 * Lists every `A11yTable` reported on the accessibility delta stream and
 * exposes the PR 3 mutation commands against a UI-selected table + cell.
 * The cell context menu the user expects (right-click inside a cell) needs
 * the caret to be addressable inside a cell — `LogicalPos.para` is
 * paragraph-flat, so PR 3b keeps the panel as the surface area; the
 * right-click context menu lands with the `BlockPath` migration (PR 4).
 *
 * The panel reads the engine's `tables` signal and never holds document
 * state — selecting a row/col only sets local UI state, the engine
 * answers each click with an `AccessibilityTreeDelta` so the panel
 * stays in sync without a refresh round-trip. */
import { createMemo, createSignal, For, Show } from 'solid-js';
import type { EngineClient } from '../engine/engine-client';
import type {
    A11yTable,
    BlockPath,
    BridgeBorderStroke,
    BridgeBorderStyle,
    BridgeCellBorders,
    Color,
} from '../engine/types';
import type { EngineStore } from '../state/engine-store';

/* Word's default cell border: 4 eighths of a point ≈ 0.5pt single-line. */
const DEFAULT_BORDER_SIZE_EIGHTH_PT = 4;

interface CellAddr {
    row: number;
    col: number;
}

function colorToHex(c: Color): string {
    const h = (n: number) => n.toString(16).padStart(2, '0');
    return `#${h(c.r)}${h(c.g)}${h(c.b)}`;
}

function hexToColor(hex: string): Color {
    return {
        r: parseInt(hex.slice(1, 3), 16),
        g: parseInt(hex.slice(3, 5), 16),
        b: parseInt(hex.slice(5, 7), 16),
        a: 255,
    };
}

function makeStroke(style: BridgeBorderStyle, color: Color): BridgeBorderStroke {
    return { style, size_eighth_pt: DEFAULT_BORDER_SIZE_EIGHTH_PT, color };
}

function pathFor(table: A11yTable): BlockPath {
    return { steps: [{ kind: 'BLOCK', idx: table.block_index }] };
}

/** Clamp `(row, col)` into `table`'s logical row/col extents. */
function clampCell(table: A11yTable, cell: CellAddr): CellAddr {
    const maxRow = Math.max(0, table.rows.length - 1);
    const maxCol = Math.max(
        0,
        table.rows.reduce((m, r) => Math.max(m, r.cells.length), 0) - 1,
    );
    return {
        row: Math.max(0, Math.min(maxRow, cell.row)),
        col: Math.max(0, Math.min(maxCol, cell.col)),
    };
}

export function TablePanel(props: { client: EngineClient; store: EngineStore }) {
    const tables = props.store.tables;
    const [selectedIdx, setSelectedIdx] = createSignal(0);
    const [from, setFrom] = createSignal<CellAddr>({ row: 0, col: 0 });
    const [to, setTo] = createSignal<CellAddr>({ row: 0, col: 0 });
    const [borderColor, setBorderColor] = createSignal<Color>({
        r: 0,
        g: 0,
        b: 0,
        a: 255,
    });
    const [borderStyle, setBorderStyle] = createSignal<BridgeBorderStyle>('Single');

    /* The selected table is clamped against the live table list — when a
       table is deleted the panel falls back to the first table without a
       stale index. */
    const table = createMemo<A11yTable | undefined>(() => {
        const list = tables();
        if (list.length === 0) return undefined;
        const idx = Math.min(selectedIdx(), list.length - 1);
        return list[idx];
    });

    const targetPath = (): BlockPath | undefined => {
        const t = table();
        return t ? pathFor(t) : undefined;
    };

    /* Dispatch helpers — each command repaints; the engine answers with
       an a11y delta so the panel re-renders with the new geometry. */
    const dispatch = (cmd: Parameters<EngineClient['dispatch']>[0]): void => {
        void props.client.dispatch(cmd);
    };

    const insertRowAbove = (): void => {
        const path = targetPath();
        if (!path) return;
        const row = clampCell(table()!, from()).row;
        dispatch({ type: 'INSERT_ROW', table_path: path, after_row: Math.max(0, row - 1) });
    };
    const insertRowBelow = (): void => {
        const path = targetPath();
        if (!path) return;
        const row = clampCell(table()!, from()).row;
        dispatch({ type: 'INSERT_ROW', table_path: path, after_row: row });
    };
    const deleteRow = (): void => {
        const path = targetPath();
        if (!path) return;
        const row = clampCell(table()!, from()).row;
        dispatch({ type: 'DELETE_ROW', table_path: path, row });
    };
    const insertColLeft = (): void => {
        const path = targetPath();
        if (!path) return;
        const col = clampCell(table()!, from()).col;
        dispatch({ type: 'INSERT_COLUMN', table_path: path, after_col: Math.max(0, col - 1) });
    };
    const insertColRight = (): void => {
        const path = targetPath();
        if (!path) return;
        const col = clampCell(table()!, from()).col;
        dispatch({ type: 'INSERT_COLUMN', table_path: path, after_col: col });
    };
    const deleteCol = (): void => {
        const path = targetPath();
        if (!path) return;
        const col = clampCell(table()!, from()).col;
        dispatch({ type: 'DELETE_COLUMN', table_path: path, col });
    };
    const mergeCells = (): void => {
        const path = targetPath();
        if (!path) return;
        const a = clampCell(table()!, from());
        const b = clampCell(table()!, to());
        dispatch({
            type: 'MERGE_CELLS',
            table_path: path,
            from_row: Math.min(a.row, b.row),
            from_col: Math.min(a.col, b.col),
            to_row: Math.max(a.row, b.row),
            to_col: Math.max(a.col, b.col),
        });
    };
    const splitCell = (): void => {
        const path = targetPath();
        if (!path) return;
        const a = clampCell(table()!, from());
        dispatch({ type: 'SPLIT_CELL', table_path: path, row: a.row, col: a.col });
    };
    const setShading = (color: Color | undefined): void => {
        const path = targetPath();
        if (!path) return;
        const a = clampCell(table()!, from());
        dispatch({
            type: 'SET_CELL_SHADING',
            table_path: path,
            row: a.row,
            col: a.col,
            color,
        });
    };
    const setBorders = (): void => {
        const path = targetPath();
        if (!path) return;
        const a = clampCell(table()!, from());
        const stroke = makeStroke(borderStyle(), borderColor());
        const borders: BridgeCellBorders = {
            top: stroke,
            left: stroke,
            bottom: stroke,
            right: stroke,
        };
        dispatch({
            type: 'SET_CELL_BORDERS',
            table_path: path,
            row: a.row,
            col: a.col,
            borders,
        });
    };
    const deleteTable = (): void => {
        const path = targetPath();
        if (!path) return;
        dispatch({ type: 'DELETE_TABLE', path });
    };

    const rowsCount = (): number => table()?.rows.length ?? 0;
    const colsCount = (): number =>
        table()?.rows.reduce((m, r) => Math.max(m, r.cells.length), 0) ?? 0;

    return (
        <aside class="table-panel" aria-label="Tables">
            <h2>Tables</h2>
            <Show
                when={tables().length > 0}
                fallback={
                    <p class="empty">No tables. Use the Table button in the toolbar.</p>
                }
            >
                <label for="table-pick">Table</label>
                <select
                    id="table-pick"
                    value={String(selectedIdx())}
                    onChange={(e) => setSelectedIdx(Number(e.currentTarget.value))}
                >
                    <For each={tables()}>
                        {(t, i) => (
                            <option value={String(i())}>
                                #{i() + 1} (block {t.block_index}) — {t.rows.length} ×{' '}
                                {t.rows[0]?.cells.length ?? 0}
                            </option>
                        )}
                    </For>
                </select>

                <div class="sect">
                    <div class="row">
                        <div>
                            <label for="cell-row">Cell row</label>
                            <input
                                id="cell-row"
                                type="number"
                                min="0"
                                max={Math.max(0, rowsCount() - 1)}
                                value={from().row}
                                onInput={(e) =>
                                    setFrom({
                                        row: Math.max(0, Number(e.currentTarget.value)),
                                        col: from().col,
                                    })
                                }
                            />
                        </div>
                        <div>
                            <label for="cell-col">Cell col</label>
                            <input
                                id="cell-col"
                                type="number"
                                min="0"
                                max={Math.max(0, colsCount() - 1)}
                                value={from().col}
                                onInput={(e) =>
                                    setFrom({
                                        row: from().row,
                                        col: Math.max(0, Number(e.currentTarget.value)),
                                    })
                                }
                            />
                        </div>
                    </div>
                    <button onClick={insertRowAbove}>Insert row above</button>
                    <button onClick={insertRowBelow}>Insert row below</button>
                    <button onClick={deleteRow} class="danger" disabled={rowsCount() <= 1}>
                        Delete row
                    </button>
                    <button onClick={insertColLeft}>Insert column left</button>
                    <button onClick={insertColRight}>Insert column right</button>
                    <button onClick={deleteCol} class="danger" disabled={colsCount() <= 1}>
                        Delete column
                    </button>
                </div>

                <div class="sect">
                    <div class="row">
                        <div>
                            <label for="merge-to-row">To row</label>
                            <input
                                id="merge-to-row"
                                type="number"
                                min="0"
                                max={Math.max(0, rowsCount() - 1)}
                                value={to().row}
                                onInput={(e) =>
                                    setTo({
                                        row: Math.max(0, Number(e.currentTarget.value)),
                                        col: to().col,
                                    })
                                }
                            />
                        </div>
                        <div>
                            <label for="merge-to-col">To col</label>
                            <input
                                id="merge-to-col"
                                type="number"
                                min="0"
                                max={Math.max(0, colsCount() - 1)}
                                value={to().col}
                                onInput={(e) =>
                                    setTo({
                                        row: to().row,
                                        col: Math.max(0, Number(e.currentTarget.value)),
                                    })
                                }
                            />
                        </div>
                    </div>
                    <button onClick={mergeCells}>Merge cells</button>
                    <button onClick={splitCell}>Split cell</button>
                </div>

                <div class="sect">
                    <label for="shading">Shading</label>
                    <div class="row">
                        <input
                            id="shading"
                            type="color"
                            value="#ffff99"
                            onChange={(e) => setShading(hexToColor(e.currentTarget.value))}
                        />
                        <button onClick={() => setShading(undefined)}>Clear</button>
                    </div>
                </div>

                <div class="sect">
                    <label for="border-style">Borders</label>
                    <select
                        id="border-style"
                        value={borderStyle()}
                        onChange={(e) =>
                            setBorderStyle(e.currentTarget.value as BridgeBorderStyle)
                        }
                    >
                        <option value="Single">Single</option>
                        <option value="Double">Double</option>
                        <option value="Dotted">Dotted</option>
                        <option value="Dashed">Dashed</option>
                        <option value="None">None</option>
                    </select>
                    <label for="border-color">Border color</label>
                    <input
                        id="border-color"
                        type="color"
                        value={colorToHex(borderColor())}
                        onChange={(e) => setBorderColor(hexToColor(e.currentTarget.value))}
                    />
                    <button onClick={setBorders}>Apply borders to cell</button>
                </div>

                <div class="sect">
                    <button onClick={deleteTable} class="danger">
                        Delete table
                    </button>
                </div>
            </Show>
        </aside>
    );
}
