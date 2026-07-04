/**
 * BorderEditor — a reusable per-edge border editor shared by the cell
 * (issue #45) and paragraph (issue #41) border pickers.
 *
 * Model: a "pen" (style + width + colour) plus a 4-edge diagram. Picking
 * the pen sets the stroke that clicking an edge paints onto THAT edge
 * only, so each of top/right/bottom/left holds an independent stroke —
 * genuinely distinct per-edge styles, not one value fanned to all four.
 * "All" paints every edge with the current pen; "None" clears them.
 *
 * Controlled: renders `props.value` (a `BridgeCellBorders`, reused as the
 * paragraph-border wire type) and reports edits via `props.onChange`.
 */
import { createSignal, type Component } from 'solid-js';
import {
    type BridgeBorderStroke,
    type BridgeBorderStyle,
    type BridgeCellBorders,
    type Color,
} from '@nge/core';
import './BorderEditor.css';

const BORDER_STYLES: BridgeBorderStyle[] = [
    'Single',
    'Double',
    'Dotted',
    'Dashed',
];

type Edge = 'top' | 'right' | 'bottom' | 'left';
const EDGES: Edge[] = ['top', 'right', 'bottom', 'left'];

export function hexToColor(hex: string): Color | undefined {
    const m = /^#?([0-9a-f]{6})$/i.exec(hex.trim());
    if (!m) return undefined;
    const v = parseInt(m[1]!, 16);
    return { r: (v >> 16) & 0xff, g: (v >> 8) & 0xff, b: v & 0xff, a: 255 };
}

export function colorToHex(c: Color | undefined): string {
    if (!c) return '';
    const hh = (n: number) => n.toString(16).padStart(2, '0');
    return `#${hh(c.r)}${hh(c.g)}${hh(c.b)}`;
}

export interface BorderEditorProps {
    value: BridgeCellBorders;
    onChange: (next: BridgeCellBorders) => void;
    /** Unique prefix for input ids so two editors can coexist. */
    idPrefix: string;
}

export const BorderEditor: Component<BorderEditorProps> = (props) => {
    /* The "pen": what a click paints onto an edge. Independent of the
       stored per-edge strokes so different edges can carry different
       styles (pick pen → click top; change pen → click bottom). */
    const [penStyle, setPenStyle] = createSignal<BridgeBorderStyle>('Single');
    const [penWidthEighth, setPenWidthEighth] = createSignal(4); // 0.5pt
    const [penColorHex, setPenColorHex] = createSignal('#000000');

    const pen = (): BridgeBorderStroke => ({
        style: penStyle(),
        size_eighth_pt: penWidthEighth(),
        color: hexToColor(penColorHex()),
    });

    const edgeOn = (edge: Edge): boolean => props.value[edge] !== undefined;

    const setEdge = (edge: Edge, stroke: BridgeBorderStroke | undefined) => {
        props.onChange({ ...props.value, [edge]: stroke });
    };

    const toggleEdge = (edge: Edge) => {
        if (edgeOn(edge)) {
            setEdge(edge, undefined);
        } else {
            /* Painting an edge also adopts it as the pen so a subsequent
               style tweak targets the edge the user just enabled. */
            setEdge(edge, pen());
        }
    };

    const applyAll = () => {
        const stroke = pen();
        props.onChange({
            top: stroke,
            right: stroke,
            bottom: stroke,
            left: stroke,
        });
    };

    const clearAll = () => {
        props.onChange({
            top: undefined,
            right: undefined,
            bottom: undefined,
            left: undefined,
        });
    };

    const edgeLabel = (edge: Edge): string =>
        edge.charAt(0).toUpperCase() + edge.slice(1);

    return (
        <div class="nge-borders">
            <div class="nge-borders__pen">
                <div class="nge-form__row">
                    <label
                        class="nge-form__label"
                        for={`${props.idPrefix}-border-style`}
                    >
                        Style
                    </label>
                    <select
                        id={`${props.idPrefix}-border-style`}
                        class="nge-form__select"
                        value={penStyle()}
                        onChange={(e) =>
                            setPenStyle(e.currentTarget.value as BridgeBorderStyle)
                        }
                    >
                        {BORDER_STYLES.map((s) => (
                            <option value={s}>{s}</option>
                        ))}
                    </select>
                </div>

                <div class="nge-form__row">
                    <label
                        class="nge-form__label"
                        for={`${props.idPrefix}-border-width`}
                    >
                        Width
                    </label>
                    <div class="nge-form__inline">
                        <input
                            id={`${props.idPrefix}-border-width`}
                            class="nge-form__input"
                            type="number"
                            min={1}
                            max={96}
                            step={1}
                            value={penWidthEighth()}
                            onInput={(e) =>
                                setPenWidthEighth(
                                    Math.max(1, parseInt(e.currentTarget.value, 10) || 4),
                                )
                            }
                            style={{ width: '72px' }}
                        />
                        <span class="nge-form__hint">
                            ⅛ pt — {(penWidthEighth() / 8).toFixed(2)} pt
                        </span>
                    </div>
                </div>

                <div class="nge-form__row">
                    <label
                        class="nge-form__label"
                        for={`${props.idPrefix}-border-color`}
                    >
                        Colour
                    </label>
                    <div class="nge-form__inline">
                        <input
                            id={`${props.idPrefix}-border-color-picker`}
                            class="nge-form__input nge-form__input--color"
                            type="color"
                            value={penColorHex()}
                            onInput={(e) => setPenColorHex(e.currentTarget.value)}
                            aria-label="Border colour picker"
                        />
                        <input
                            id={`${props.idPrefix}-border-color`}
                            class="nge-form__input"
                            type="text"
                            value={penColorHex()}
                            onInput={(e) => setPenColorHex(e.currentTarget.value)}
                        />
                    </div>
                </div>
            </div>

            <div class="nge-borders__diagram" role="group" aria-label="Edges">
                <div class="nge-borders__presets">
                    <button
                        class="nge-btn nge-btn--sm"
                        type="button"
                        onClick={applyAll}
                    >
                        All
                    </button>
                    <button
                        class="nge-btn nge-btn--sm"
                        type="button"
                        onClick={clearAll}
                    >
                        None
                    </button>
                </div>
                <div class="nge-borders__grid">
                    {EDGES.map((edge) => (
                        <button
                            class={`nge-borders__edge nge-borders__edge--${edge}${
                                edgeOn(edge) ? ' nge-borders__edge--on' : ''
                            }`}
                            type="button"
                            aria-pressed={edgeOn(edge)}
                            aria-label={`${edgeLabel(edge)} border`}
                            title={`${edgeLabel(edge)} border`}
                            onClick={() => toggleEdge(edge)}
                        >
                            {edgeLabel(edge).charAt(0)}
                        </button>
                    ))}
                    <span class="nge-borders__center" aria-hidden="true" />
                </div>
            </div>
        </div>
    );
};
