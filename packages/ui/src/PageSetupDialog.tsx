/**
 * PageSetupDialog — modal editor for the section containing the
 * caret. Mutates `<w:pgMar>` (margins) and `<w:pgSz>` orientation
 * via the Sprint 4 (UI Edition) Rust additions:
 *   - `Command::SetPageMargins { at, top_pt, right_pt, bottom_pt, left_pt }`
 *   - `Command::SetPageOrientation { at, orientation }`
 *
 * Units: input field accepts inches (`in`) or points (`pt`); the
 * dialog normalises to points before dispatch. Default is inches —
 * Word's preset (1 inch = 72 pt margins on all four edges).
 *
 * No engine read-back: the form opens with sensible defaults (the
 * `defaultMarginInches` prop or 1.0). A future enhancement could
 * resolve the current section geometry from a SelectionChanged
 * extension and prefill — out of scope for Sprint 4.
 */
import { createSignal, createEffect, type Component } from 'solid-js';
import {
    createEditorCommands,
    type PageOrientation,
} from '@nge/core';
import { Dialog } from './Dialog';

type Unit = 'in' | 'pt';

const PT_PER_INCH = 72;

function toPt(value: number, unit: Unit): number {
    return unit === 'in' ? value * PT_PER_INCH : value;
}

export interface PageSetupDialogProps {
    open: boolean;
    onClose: () => void;
    /** Default margin width (inches) when the dialog opens. */
    defaultMarginInches?: number;
}

export const PageSetupDialog: Component<PageSetupDialogProps> = (props) => {
    const cmd = createEditorCommands();
    const [unit, setUnit] = createSignal<Unit>('in');
    const [top, setTop] = createSignal(1);
    const [right, setRight] = createSignal(1);
    const [bottom, setBottom] = createSignal(1);
    const [left, setLeft] = createSignal(1);
    const [orientation, setOrientation] = createSignal<PageOrientation>('Portrait');
    const [error, setError] = createSignal<string | null>(null);

    /* Re-seed defaults each time the dialog reopens. */
    createEffect(() => {
        if (props.open) {
            const d = props.defaultMarginInches ?? 1;
            setTop(d);
            setRight(d);
            setBottom(d);
            setLeft(d);
            setUnit('in');
            setOrientation('Portrait');
            setError(null);
        }
    });

    const apply = async () => {
        const u = unit();
        const t = toPt(top(), u);
        const r = toPt(right(), u);
        const b = toPt(bottom(), u);
        const l = toPt(left(), u);
        if ([t, r, b, l].some((v) => !Number.isFinite(v) || v < 0)) {
            setError('Margins must be non-negative numbers.');
            return;
        }
        try {
            await cmd.setPageOrientationAtCaret(orientation());
            await cmd.setPageMarginsAtCaret(t, r, b, l);
            props.onClose();
        } catch (e) {
            setError(String(e));
        }
    };

    const setAll = (v: number) => {
        setTop(v);
        setRight(v);
        setBottom(v);
        setLeft(v);
    };

    return (
        <Dialog
            open={props.open}
            title="Page Setup"
            description="Margins + orientation for the section containing the caret."
            onClose={props.onClose}
            size="md"
            footer={
                <>
                    <button class="nge-btn" type="button" onClick={() => setAll(unit() === 'in' ? 1 : 72)}>
                        Reset to default
                    </button>
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
                    <span class="nge-form__label">Orientation</span>
                    <div class="nge-form__radiogroup" role="radiogroup" aria-label="Orientation">
                        <button
                            type="button"
                            role="radio"
                            class="nge-form__radio"
                            aria-checked={orientation() === 'Portrait'}
                            data-selected={orientation() === 'Portrait'}
                            onClick={() => setOrientation('Portrait')}
                        >
                            <span aria-hidden="true">▯</span>
                            <span>Portrait</span>
                        </button>
                        <button
                            type="button"
                            role="radio"
                            class="nge-form__radio"
                            aria-checked={orientation() === 'Landscape'}
                            data-selected={orientation() === 'Landscape'}
                            onClick={() => setOrientation('Landscape')}
                        >
                            <span aria-hidden="true">▭</span>
                            <span>Landscape</span>
                        </button>
                    </div>
                </div>

                <div class="nge-form__row">
                    <label class="nge-form__label" for="nge-page-unit">Units</label>
                    <select
                        id="nge-page-unit"
                        class="nge-form__select"
                        value={unit()}
                        onChange={(e) => setUnit(e.currentTarget.value as Unit)}
                    >
                        <option value="in">Inches</option>
                        <option value="pt">Points</option>
                    </select>
                </div>

                <div>
                    <div class="nge-form__sublabel">Margins ({unit()})</div>
                    <div class="nge-form__grid-4">
                        <div class="nge-form__inline">
                            <label class="nge-form__label" for="nge-margin-top">Top</label>
                            <input
                                id="nge-margin-top"
                                class="nge-form__input"
                                type="number"
                                min={0}
                                step={unit() === 'in' ? 0.1 : 1}
                                value={top()}
                                onInput={(e) => setTop(parseFloat(e.currentTarget.value) || 0)}
                            />
                        </div>
                        <div class="nge-form__inline">
                            <label class="nge-form__label" for="nge-margin-bottom">Bottom</label>
                            <input
                                id="nge-margin-bottom"
                                class="nge-form__input"
                                type="number"
                                min={0}
                                step={unit() === 'in' ? 0.1 : 1}
                                value={bottom()}
                                onInput={(e) => setBottom(parseFloat(e.currentTarget.value) || 0)}
                            />
                        </div>
                        <div class="nge-form__inline">
                            <label class="nge-form__label" for="nge-margin-left">Left</label>
                            <input
                                id="nge-margin-left"
                                class="nge-form__input"
                                type="number"
                                min={0}
                                step={unit() === 'in' ? 0.1 : 1}
                                value={left()}
                                onInput={(e) => setLeft(parseFloat(e.currentTarget.value) || 0)}
                            />
                        </div>
                        <div class="nge-form__inline">
                            <label class="nge-form__label" for="nge-margin-right">Right</label>
                            <input
                                id="nge-margin-right"
                                class="nge-form__input"
                                type="number"
                                min={0}
                                step={unit() === 'in' ? 0.1 : 1}
                                value={right()}
                                onInput={(e) => setRight(parseFloat(e.currentTarget.value) || 0)}
                            />
                        </div>
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
