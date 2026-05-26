/**
 * CellPropertiesDialog — modal editor for a single table cell's
 * shading + per-edge borders.
 *
 * Inputs:
 *   - Background colour (hex picker + text input). Empty / blank
 *     clears shading (dispatches `setCellShading` with `undefined`).
 *   - Border style (Single / Double / Dotted / Dashed / None) +
 *     width in eighths of a point (`<w:sz>` semantics) + colour.
 *     Applied uniformly to all 4 edges via `setCellBorders` — per-
 *     edge UI is filed as a future enhancement.
 *
 * Driven by an externally-controlled `open` signal so the
 * TableContextMenu can pop it.
 */
import { createSignal, createEffect, type Component } from 'solid-js';
import {
    createEditorCommands,
    createEditorState,
    type BlockPath,
    type BridgeBorderStyle,
    type BridgeCellBorders,
    type BridgeBorderStroke,
    type Color,
} from '@nge/core';
import { Dialog } from './Dialog';

const BORDER_STYLES: BridgeBorderStyle[] = [
    'Single',
    'Double',
    'Dotted',
    'Dashed',
    'None',
];

function hexToColor(hex: string): Color | undefined {
    const m = /^#?([0-9a-f]{6})$/i.exec(hex.trim());
    if (!m) return undefined;
    const v = parseInt(m[1]!, 16);
    return { r: (v >> 16) & 0xff, g: (v >> 8) & 0xff, b: v & 0xff, a: 255 };
}

function colorToHex(c: Color | undefined): string {
    if (!c) return '';
    const hh = (n: number) => n.toString(16).padStart(2, '0');
    return `#${hh(c.r)}${hh(c.g)}${hh(c.b)}`;
}

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
    const [borderStyle, setBorderStyle] = createSignal<BridgeBorderStyle>('Single');
    const [borderWidthEighth, setBorderWidthEighth] = createSignal(4); // 0.5pt
    const [borderColorHex, setBorderColorHex] = createSignal('#000000');
    const [error, setError] = createSignal<string | null>(null);

    /* Sprint 10 — prefill from `cellProperties()` so the dialog opens
     * with the cell's current shading + top-edge stroke style. The
     * dialog still applies the stroke uniformly across all 4 edges,
     * so the prefill picks ONE representative edge (top, then left,
     * then any present) — per-edge editing remains a future
     * enhancement. */
    createEffect(() => {
        if (!props.open) return;
        const cell = state.cellProperties();
        const shading = cell?.shading;
        setShadingHex(shading ? colorToHex(shading) : '');
        const representative =
            cell?.borders?.top ?? cell?.borders?.left ?? cell?.borders?.bottom ?? cell?.borders?.right;
        if (representative) {
            setBorderStyle(representative.style);
            setBorderWidthEighth(Math.max(1, representative.size_eighth_pt));
            setBorderColorHex(colorToHex(representative.color) || '#000000');
        } else {
            setBorderStyle('Single');
            setBorderWidthEighth(4);
            setBorderColorHex('#000000');
        }
        setError(null);
    });

    const apply = async () => {
        if (!props.tablePath) {
            setError('No table cell selected.');
            return;
        }

        /* Shading: blank string = clear; otherwise validate hex. */
        const hex = shadingHex().trim();
        const shading = hex === '' ? undefined : hexToColor(hex);
        if (hex !== '' && !shading) {
            setError(`Invalid shading hex: "${hex}". Use #RRGGBB or RRGGBB.`);
            return;
        }

        /* Borders: a Single/Double/Dotted/Dashed/None style applied
         * uniformly to all 4 edges. `None` style clears each edge by
         * passing `style: None` (engine collapses no-op strokes). */
        const stroke: BridgeBorderStroke = {
            style: borderStyle(),
            size_eighth_pt: borderWidthEighth(),
            color: hexToColor(borderColorHex()),
        };
        const borders: BridgeCellBorders =
            borderStyle() === 'None'
                ? {
                      top: undefined,
                      left: undefined,
                      bottom: undefined,
                      right: undefined,
                  }
                : {
                      top: stroke,
                      left: stroke,
                      bottom: stroke,
                      right: stroke,
                  };

        try {
            await cmd.setCellShading(props.tablePath, props.row, props.col, shading);
            await cmd.setCellBorders(props.tablePath, props.row, props.col, borders);
            props.onClose();
        } catch (e) {
            setError(String(e));
        }
    };

    return (
        <Dialog
            open={props.open}
            title="Cell Properties"
            description={
                props.tablePath
                    ? `Row ${props.row + 1}, Column ${props.col + 1}`
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

                <div class="nge-form__row">
                    <label class="nge-form__label" for="nge-cell-border-style">
                        Border style
                    </label>
                    <select
                        id="nge-cell-border-style"
                        class="nge-form__select"
                        value={borderStyle()}
                        onChange={(e) =>
                            setBorderStyle(e.currentTarget.value as BridgeBorderStyle)
                        }
                    >
                        {BORDER_STYLES.map((s) => (
                            <option value={s}>{s}</option>
                        ))}
                    </select>
                </div>

                <div class="nge-form__row">
                    <label class="nge-form__label" for="nge-cell-border-width">
                        Width
                    </label>
                    <div class="nge-form__inline">
                        <input
                            id="nge-cell-border-width"
                            class="nge-form__input"
                            type="number"
                            min={1}
                            max={96}
                            step={1}
                            value={borderWidthEighth()}
                            onInput={(e) =>
                                setBorderWidthEighth(
                                    Math.max(
                                        1,
                                        parseInt(e.currentTarget.value, 10) || 4,
                                    ),
                                )
                            }
                            style={{ width: '80px' }}
                        />
                        <span class="nge-form__hint">
                            ⅛ pt — {(borderWidthEighth() / 8).toFixed(2)} pt
                        </span>
                    </div>
                </div>

                <div class="nge-form__row">
                    <label class="nge-form__label" for="nge-cell-border-color">
                        Border colour
                    </label>
                    <div class="nge-form__inline">
                        <input
                            id="nge-cell-border-color-picker"
                            class="nge-form__input nge-form__input--color"
                            type="color"
                            value={borderColorHex()}
                            onInput={(e) => setBorderColorHex(e.currentTarget.value)}
                            aria-label="Border colour picker"
                        />
                        <input
                            id="nge-cell-border-color"
                            class="nge-form__input"
                            type="text"
                            value={borderColorHex()}
                            onInput={(e) => setBorderColorHex(e.currentTarget.value)}
                        />
                    </div>
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
