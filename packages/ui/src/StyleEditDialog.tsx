/**
 * StyleEditDialog — modal editor for a paragraph style DEFINITION
 * (issue #21 §5). Where `StylesDropdown` assigns a style to the caret
 * paragraph (`Command::ApplyStyle`), this dialog mutates the
 * `<w:style>` entry itself via `Command::ModifyStyle` — the engine
 * re-cascades every dependent paragraph immediately and regenerates
 * `styles.xml` on the next save.
 *
 * Patch semantics: every field starts BLANK ("Leave unchanged") and
 * only touched fields enter the payload. tsify-next renders
 * `Option<T>` as `T | undefined` with REQUIRED keys, so under
 * `exactOptionalPropertyTypes` the builders below set EVERY key of
 * `BridgeSpanStylePatch` / `BridgeParaPropertiesPatch` /
 * `BridgeStyleProperties` explicitly — untouched fields to
 * `undefined` (the same discipline as `emptyPatch()` in @nge/core).
 * A half with no touched field is sent as `undefined` wholesale.
 *
 * Failure path: the engine resolves `Event::Error` (e.g. unknown
 * style id) as a normal RPC reply, not a rejection — the message is
 * rendered inline and the dialog stays open.
 */
import { createSignal, createEffect, For, Show, type Component } from 'solid-js';
import {
    createEditorCommands,
    type Alignment,
    type BridgeParaPropertiesPatch,
    type BridgeSpanStylePatch,
    type BridgeStyleProperties,
    type Color,
} from '@nge/core';
import { Dialog } from './Dialog';
import './StyleEditDialog.css';

/** Small fixed swatch row — the discrete-palette discipline from
 *  ColorPickers.tsx (one dispatch per commit, no continuous wheel). */
const TEXT_SWATCHES: string[] = [
    '#000000', '#666666', '#980000', '#ff0000',
    '#ff9900', '#38761d', '#1155cc', '#9900ff',
];

function hexToColor(hex: string): Color | undefined {
    const m = /^#?([0-9a-f]{6})$/i.exec(hex.trim());
    if (!m) return undefined;
    const v = parseInt(m[1]!, 16);
    return { r: (v >> 16) & 0xff, g: (v >> 8) & 0xff, b: v & 0xff, a: 255 };
}

/** Tri-state checkbox: `undefined` renders as the native indeterminate
 *  dash ("Leave unchanged"); a click resolves it to true/false. */
const TriCheckbox: Component<{
    id: string;
    value: boolean | undefined;
    onChange: (v: boolean) => void;
}> = (props) => {
    let el: HTMLInputElement | undefined;
    createEffect(() => {
        const v = props.value;
        if (el) el.indeterminate = v === undefined;
    });
    return (
        <input
            ref={el}
            id={props.id}
            class="nge-style-edit__check"
            type="checkbox"
            checked={props.value === true}
            onChange={(e) => props.onChange(e.currentTarget.checked)}
        />
    );
};

/** Per-field "back to Leave unchanged" affordance — only rendered
 *  once the field is touched, so blank state stays visually quiet. */
const ResetButton: Component<{
    when: boolean;
    field: string;
    onReset: () => void;
}> = (props) => (
    <Show when={props.when}>
        <button
            class="nge-style-edit__reset"
            type="button"
            aria-label={`Reset ${props.field} to "Leave unchanged"`}
            title={'Reset to "Leave unchanged"'}
            onClick={props.onReset}
        >
            <span aria-hidden="true">↺</span>
        </button>
    </Show>
);

const triLabel = (v: boolean | undefined): string =>
    v === undefined ? 'Leave unchanged' : v ? 'On' : 'Off';

export interface StyleEditDialogProps {
    open: boolean;
    /** The `<w:style>` id to mutate (e.g. `Heading1`). */
    styleId: string;
    /** Human label for the title bar; falls back to the id. */
    styleLabel?: string | undefined;
    onClose: () => void;
}

export const StyleEditDialog: Component<StyleEditDialogProps> = (props) => {
    const cmd = createEditorCommands();

    /* Every field's zero state means "leave untouched". */
    const [displayName, setDisplayName] = createSignal('');
    const [bold, setBold] = createSignal<boolean | undefined>(undefined);
    const [italic, setItalic] = createSignal<boolean | undefined>(undefined);
    const [fontSize, setFontSize] = createSignal<number | undefined>(undefined);
    const [colorHex, setColorHex] = createSignal<string | undefined>(undefined);
    const [alignment, setAlignment] = createSignal<Alignment | ''>('');
    const [lineSpacing, setLineSpacing] = createSignal<'' | '1' | '1.15' | '1.5' | '2'>('');
    const [error, setError] = createSignal<string | null>(null);

    /* Re-blank every field each time the dialog reopens — the patch
     * always starts from "change nothing". */
    createEffect(() => {
        if (!props.open) return;
        setDisplayName('');
        setBold(undefined);
        setItalic(undefined);
        setFontSize(undefined);
        setColorHex(undefined);
        setAlignment('');
        setLineSpacing('');
        setError(null);
    });

    const runTouched = () =>
        bold() !== undefined ||
        italic() !== undefined ||
        fontSize() !== undefined ||
        colorHex() !== undefined;

    const paraTouched = () => alignment() !== '' || lineSpacing() !== '';

    const touched = () =>
        runTouched() || paraTouched() || displayName().trim() !== '';

    /** Assemble the sparse payload — only touched halves materialize;
     *  inside a touched half every key is set explicitly (untouched
     *  fields to `undefined`) per exactOptionalPropertyTypes. */
    const buildProperties = (): BridgeStyleProperties => {
        const hex = colorHex();
        const run_props: BridgeSpanStylePatch | undefined = runTouched()
            ? {
                  bold: bold(),
                  italic: italic(),
                  underline: undefined,
                  strike: undefined,
                  font_size: fontSize(),
                  color: hex !== undefined ? hexToColor(hex) : undefined,
                  bg_color: undefined,
                  font_family: undefined,
                  caps: undefined,
                  small_caps: undefined,
              }
            : undefined;

        const align = alignment();
        const spacing = lineSpacing();
        const para_props: BridgeParaPropertiesPatch | undefined = paraTouched()
            ? {
                  alignment: align === '' ? undefined : align,
                  direction: undefined,
                  line_spacing_multiplier:
                      spacing === '' ? undefined : parseFloat(spacing),
                  indent_start_pt: undefined,
                  indent_end_pt: undefined,
                  first_line_pt: undefined,
                  shading: undefined,
              }
            : undefined;

        const name = displayName().trim();
        return {
            para_props,
            run_props,
            based_on: undefined,
            clear_based_on: undefined,
            display_name: name === '' ? undefined : name,
        };
    };

    const apply = async () => {
        setError(null);
        const fs = fontSize();
        if (fs !== undefined && (!Number.isFinite(fs) || fs < 1)) {
            setError('Font size must be a number ≥ 1 pt.');
            return;
        }
        try {
            const evt = await cmd.modifyStyle(props.styleId, buildProperties());
            if (evt.type === 'ERROR') {
                /* Engine refusal (unknown style id, …) resolves as a
                 * normal Event::Error reply — surface it inline. */
                setError(evt.message);
                return;
            }
            props.onClose();
        } catch (e) {
            setError(String(e));
        }
    };

    return (
        <Dialog
            open={props.open}
            title={`Edit style — ${props.styleLabel ?? props.styleId}`}
            description={
                'Blank fields are left unchanged. Applied changes re-cascade ' +
                'every paragraph using this style.'
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
                        disabled={!touched()}
                        title={
                            touched()
                                ? `Modify the "${props.styleId}" definition`
                                : 'Every field is "Leave unchanged" — nothing to apply'
                        }
                        onClick={() => void apply()}
                    >
                        Apply
                    </button>
                </>
            }
        >
            <form class="nge-form" onSubmit={(e) => e.preventDefault()}>
                <div class="nge-form__row">
                    <label class="nge-form__label" for="nge-style-edit-name">
                        Display name
                    </label>
                    <input
                        id="nge-style-edit-name"
                        class="nge-form__input"
                        type="text"
                        autofocus
                        placeholder="Leave unchanged"
                        value={displayName()}
                        onInput={(e) => setDisplayName(e.currentTarget.value)}
                    />
                </div>

                <div class="nge-form__row">
                    <label class="nge-form__label" for="nge-style-edit-bold">
                        Bold
                    </label>
                    <div class="nge-form__inline">
                        <TriCheckbox
                            id="nge-style-edit-bold"
                            value={bold()}
                            onChange={setBold}
                        />
                        <span class="nge-style-edit__state">{triLabel(bold())}</span>
                        <ResetButton
                            when={bold() !== undefined}
                            field="bold"
                            onReset={() => setBold(undefined)}
                        />
                    </div>
                </div>

                <div class="nge-form__row">
                    <label class="nge-form__label" for="nge-style-edit-italic">
                        Italic
                    </label>
                    <div class="nge-form__inline">
                        <TriCheckbox
                            id="nge-style-edit-italic"
                            value={italic()}
                            onChange={setItalic}
                        />
                        <span class="nge-style-edit__state">{triLabel(italic())}</span>
                        <ResetButton
                            when={italic() !== undefined}
                            field="italic"
                            onReset={() => setItalic(undefined)}
                        />
                    </div>
                </div>

                <div class="nge-form__row">
                    <label class="nge-form__label" for="nge-style-edit-size">
                        Font size (pt)
                    </label>
                    <div class="nge-form__inline">
                        <input
                            id="nge-style-edit-size"
                            class="nge-form__input nge-style-edit__size"
                            type="number"
                            min={1}
                            step={0.5}
                            placeholder="Leave unchanged"
                            value={fontSize() ?? ''}
                            onInput={(e) => {
                                const raw = e.currentTarget.value.trim();
                                if (raw === '') {
                                    setFontSize(undefined);
                                    return;
                                }
                                const v = parseFloat(raw);
                                setFontSize(Number.isFinite(v) ? v : undefined);
                            }}
                        />
                        <ResetButton
                            when={fontSize() !== undefined}
                            field="font size"
                            onReset={() => setFontSize(undefined)}
                        />
                    </div>
                </div>

                <div class="nge-form__row">
                    <span class="nge-form__label" id="nge-style-edit-color-label">
                        Text colour
                    </span>
                    <div class="nge-form__inline">
                        <div
                            class="nge-style-edit__swatches"
                            role="group"
                            aria-labelledby="nge-style-edit-color-label"
                        >
                            <For each={TEXT_SWATCHES}>
                                {(sw) => (
                                    <button
                                        class="nge-style-edit__swatch"
                                        type="button"
                                        aria-label={sw}
                                        aria-pressed={colorHex() === sw}
                                        title={sw}
                                        style={{ background: sw }}
                                        data-active={colorHex() === sw}
                                        onClick={() =>
                                            setColorHex(
                                                colorHex() === sw ? undefined : sw,
                                            )
                                        }
                                    />
                                )}
                            </For>
                        </div>
                        <Show when={colorHex() === undefined}>
                            <span class="nge-style-edit__state">Leave unchanged</span>
                        </Show>
                        <ResetButton
                            when={colorHex() !== undefined}
                            field="text colour"
                            onReset={() => setColorHex(undefined)}
                        />
                    </div>
                </div>

                <div class="nge-form__row">
                    <label class="nge-form__label" for="nge-style-edit-align">
                        Alignment
                    </label>
                    <select
                        id="nge-style-edit-align"
                        class="nge-form__select"
                        value={alignment()}
                        onChange={(e) =>
                            setAlignment(e.currentTarget.value as Alignment | '')
                        }
                    >
                        <option value="">Leave unchanged</option>
                        <option value="Start">Start</option>
                        <option value="Center">Center</option>
                        <option value="End">End</option>
                        <option value="Justify">Justify</option>
                    </select>
                </div>

                <div class="nge-form__row">
                    <label class="nge-form__label" for="nge-style-edit-spacing">
                        Line spacing
                    </label>
                    <select
                        id="nge-style-edit-spacing"
                        class="nge-form__select"
                        value={lineSpacing()}
                        onChange={(e) =>
                            setLineSpacing(
                                e.currentTarget.value as
                                    | ''
                                    | '1'
                                    | '1.15'
                                    | '1.5'
                                    | '2',
                            )
                        }
                    >
                        <option value="">Leave unchanged</option>
                        <option value="1">Single (1.0)</option>
                        <option value="1.15">1.15</option>
                        <option value="1.5">1.5</option>
                        <option value="2">Double (2.0)</option>
                    </select>
                </div>

                <Show when={error()}>
                    <div class="nge-form__hint nge-style-edit__error" role="alert">
                        {error()}
                    </div>
                </Show>
            </form>
        </Dialog>
    );
};
