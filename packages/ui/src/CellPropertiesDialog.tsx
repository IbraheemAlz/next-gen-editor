/**
 * CellPropertiesDialog — modal editor for a table cell's shading +
 * per-edge borders.
 *
 * Inputs:
 *   - Background colour (hex picker + text input). Empty / blank clears
 *     shading (`setCellShading` with `undefined`).
 *   - Per-edge borders via the shared `BorderEditor` (issue #45): each of
 *     top/right/bottom/left holds an independent style/width/colour.
 *
 * When the current selection spans multiple cells
 * (`selectionKind() === 'TABLE_CELLS'`) the borders + shading apply to
 * every cell in the rectangle (a client-side loop — the engine's
 * `SetCellBorders` is single-cell, one undo push per cell).
 *
 * Driven by an externally-controlled `open` signal so the
 * TableContextMenu can pop it.
 */
import { createSignal, createEffect, type Component } from 'solid-js';
import {
    createEditorCommands,
    createEditorState,
    type BlockPath,
    type BridgeCellBorders,
    type Color,
} from '@nge/core';
import { Dialog } from './Dialog';
import { BorderEditor, hexToColor, colorToHex } from './BorderEditor';

const EMPTY_BORDERS: BridgeCellBorders = {
    top: undefined,
    right: undefined,
    bottom: undefined,
    left: undefined,
};

export interface CellPropertiesDialogProps {
    open: boolean;
    onClose: () => void;
    tablePath: BlockPath | undefined;
    row: number;
    col: number;
}

export const CellPropertiesDialog: Component<CellPropertiesDialogProps> = (props) => {
    const cmd = createEditorCommands();
    const state = createEditorState();
    const [shadingHex, setShadingHex] = createSignal('');
    const [borders, setBorders] = createSignal<BridgeCellBorders>(EMPTY_BORDERS);
    const [error, setError] = createSignal<string | null>(null);

    /* Prefill from `cellProperties()` — shading + every edge's stroke, so
       the per-edge diagram opens reflecting the live cell (issue #45; the
       old dialog collapsed all four edges into one representative). */
    createEffect(() => {
        if (!props.open) return;
        const cell = state.cellProperties();
        setShadingHex(cell?.shading ? colorToHex(cell.shading) : '');
        setBorders(cell?.borders ?? EMPTY_BORDERS);
        setError(null);
    });

    /* The cells the edit targets: the full rectangle when a multi-cell
       table selection is active, else just the context-menu cell. */
    const targetCells = (): Array<{ row: number; col: number }> => {
        const kind = state.selectionKind();
        if (kind && kind.kind === 'TABLE_CELLS') {
            const cells: Array<{ row: number; col: number }> = [];
            const r0 = Math.min(kind.from_row, kind.to_row);
            const r1 = Math.max(kind.from_row, kind.to_row);
            const c0 = Math.min(kind.from_col, kind.to_col);
            const c1 = Math.max(kind.from_col, kind.to_col);
            for (let r = r0; r <= r1; r++)
                for (let c = c0; c <= c1; c++) cells.push({ row: r, col: c });
            return cells;
        }
        return [{ row: props.row, col: props.col }];
    };

    const apply = async () => {
        if (!props.tablePath) {
            setError('No table cell selected.');
            return;
        }
        const hex = shadingHex().trim();
        const shading: Color | undefined = hex === '' ? undefined : hexToColor(hex);
        if (hex !== '' && !shading) {
            setError(`Invalid shading hex: "${hex}". Use #RRGGBB or RRGGBB.`);
            return;
        }
        try {
            for (const { row, col } of targetCells()) {
                await cmd.setCellShading(props.tablePath, row, col, shading);
                await cmd.setCellBorders(props.tablePath, row, col, borders());
            }
            props.onClose();
        } catch (e) {
            setError(String(e));
        }
    };

    const cellCount = () => targetCells().length;

    return (
        <Dialog
            open={props.open}
            title="Cell Properties"
            description={
                props.tablePath
                    ? cellCount() > 1
                        ? `${cellCount()} cells selected`
                        : `Row ${props.row + 1}, Column ${props.col + 1}`
                    : undefined
            }
            onClose={props.onClose}
            size="md"
            footer={
                <>
                    <button class="nge-btn" type="button" onClick={props.onClose}>
                        Cancel
                    </button>
                    <button
                        class="nge-btn nge-btn--primary"
                        type="button"
                        onClick={() => void apply()}
                    >
                        Apply
                    </button>
                </>
            }
        >
            <form class="nge-form" onSubmit={(e) => e.preventDefault()}>
                <div class="nge-form__row">
                    <label class="nge-form__label" for="nge-cell-shading">
                        Shading
                    </label>
                    <div class="nge-form__inline">
                        <input
                            id="nge-cell-shading-picker"
                            class="nge-form__input nge-form__input--color"
                            type="color"
                            value={shadingHex() || '#ffffff'}
                            onInput={(e) => setShadingHex(e.currentTarget.value)}
                            aria-label="Shading colour picker"
                        />
                        <input
                            id="nge-cell-shading"
                            class="nge-form__input"
                            type="text"
                            placeholder="#RRGGBB or blank to clear"
                            value={shadingHex()}
                            onInput={(e) => setShadingHex(e.currentTarget.value)}
                        />
                    </div>
                </div>

                <div class="nge-form__row nge-form__row--block">
                    <span class="nge-form__label">Borders</span>
                    <BorderEditor
                        value={borders()}
                        onChange={setBorders}
                        idPrefix="nge-cell"
                    />
                </div>

                {error() && (
                    <div class="nge-form__hint" role="alert" style={{ color: 'var(--nge-color-danger)' }}>
                        {error()}
                    </div>
                )}
            </form>
        </Dialog>
    );
};
