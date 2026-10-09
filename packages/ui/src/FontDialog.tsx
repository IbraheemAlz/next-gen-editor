/**
 * FontDialog — Word's Font dialog for the two OOXML script slots
 * (issue #420): a "Latin text" section (`<w:sz>`, `<w:b>`, `<w:i>`,
 * `w:ascii` / `w:hAnsi`) and a "Complex scripts" section (`<w:szCs>`,
 * `<w:bCs>`, `<w:iCs>`, `w:cs` — Arabic, Hebrew, Thai, …), each with its
 * own family, size, bold and italic.
 *
 * The toolbar (`FontPickers`, `TextFormatButtons`) keeps writing BOTH
 * slots, as Word's ribbon does; this dialog is how an Arabic document
 * gets "14 pt Arabic next to 11 pt Latin" in one run.
 *
 * Seed: every time the dialog opens it reads the caret run's RESOLVED
 * values per slot from `createEditorState()` — `slotFormats()` (family
 * id / size / bold / italic, issue #420) and `resolvedFontLatin()` /
 * `resolvedFontCs()` + `fontSource()` (the display name and whether a
 * theme supplied it, issue #423). A family the font registry does not
 * ship (a theme's Calibri) shows by name as a disabled placeholder; it
 * is kept unless the user picks a registry font.
 *
 * Apply: per slot, only the fields the user changed go out, as ONE
 * `APPLY_FORMATTING` with that slot (`cmd.setSlotFormat`) — at most two
 * undo steps, and a mixed selection keeps every attribute nobody
 * touched. A changed family is loaded just in time first
 * (`registry.ensureFont`). The range is the engine's live selection; a
 * collapsed caret arms the formatting for the next keystroke. An
 * `Event::Error` reply is shown inline and the dialog stays open.
 *
 * Modal via the shared `<Dialog>` primitive (solid-js/web `<Portal>`).
 * Every control carries `data-nge-command="APPLY_FORMATTING"` (the
 * issue #342 parity matrix) and `data-nge-font-slot` for tests.
 */
import { createEffect, createSignal, For, Show, untrack, type Component } from 'solid-js';
import { createStore } from 'solid-js/store';
import {
    createEditorCommands,
    createEditorState,
    useFontRegistry,
    type FontSlot,
    type SlotFormatPatch,
} from '@nge/core';
import { Dialog } from './Dialog';
import './FontDialog.css';

type SlotKey = 'latin' | 'complex_script';

interface SlotDraft {
    /** Font-registry / resolution id of the family. */
    family: string;
    /** Display name of the resolved family (the placeholder's label). */
    name: string;
    /** The resolved family comes from the document theme. */
    fromTheme: boolean;
    /** Points. */
    size: number;
    bold: boolean;
    italic: boolean;
}

const SLOTS: ReadonlyArray<{
    key: SlotKey;
    slot: FontSlot;
    legend: string;
    hint: string;
    /** Element-id stem. */
    id: string;
}> = [
    {
        key: 'latin',
        slot: 'Latin',
        legend: 'Latin text',
        hint: 'Latin, Greek, Cyrillic and other non-complex scripts.',
        id: 'nge-font-dialog-latin',
    },
    {
        key: 'complex_script',
        slot: 'ComplexScript',
        legend: 'Complex scripts',
        hint: 'Arabic, Hebrew, Thai and other complex scripts.',
        id: 'nge-font-dialog-cs',
    },
];

const EMPTY: SlotDraft = {
    family: '',
    name: '',
    fromTheme: false,
    size: 12,
    bold: false,
    italic: false,
};

/** Word's size range (pt). */
const MIN_PT = 1;
const MAX_PT = 1638;

/** f32 → f64 noise (`10.5000001`) must not count as an edit. */
function roundPt(pt: number): number {
    return Math.round(pt * 100) / 100;
}

export interface FontDialogProps {
    open: boolean;
    onClose: () => void;
}

export const FontDialog: Component<FontDialogProps> = (props) => {
    const cmd = createEditorCommands();
    const state = createEditorState();
    const registry = useFontRegistry();

    const [drafts, setDrafts] = createStore<Record<SlotKey, SlotDraft>>({
        latin: { ...EMPTY },
        complex_script: { ...EMPTY },
    });
    /* What the dialog opened with — the baseline "changed" is measured
       against, and the placeholder / theme marker's source. */
    const [seeds, setSeeds] = createSignal<Record<SlotKey, SlotDraft>>({
        latin: { ...EMPTY },
        complex_script: { ...EMPTY },
    });
    const [busy, setBusy] = createSignal(false);
    const [error, setError] = createSignal<string | null>(null);

    const seedOf = (key: SlotKey): SlotDraft => {
        const format = state.slotFormats()?.[key];
        const attrs = state.attrsAtCaret();
        const name = key === 'latin' ? state.resolvedFontLatin() : state.resolvedFontCs();
        const family = format?.font_family || attrs?.font_family || '';
        return {
            family,
            name: name ?? family,
            fromTheme: state.fontSource()?.[key] === 'Theme',
            size: roundPt(format?.font_size ?? attrs?.font_size ?? 12),
            bold: format?.bold ?? false,
            italic: format?.italic ?? false,
        };
    };

    /* Re-seed on every OPEN only: a SELECTION_CHANGED arriving while the
       dialog is up must not clobber the user's edits. */
    createEffect(() => {
        if (!props.open) return;
        untrack(() => {
            const next = { latin: seedOf('latin'), complex_script: seedOf('complex_script') };
            setSeeds(next);
            setDrafts('latin', { ...next.latin });
            setDrafts('complex_script', { ...next.complex_script });
            setError(null);
        });
    });

    const inRegistry = (id: string) => registry.fonts().some((f) => f.id === id);

    const patchFor = (key: SlotKey): SlotFormatPatch => {
        const d = drafts[key];
        const s = seeds()[key];
        const patch: SlotFormatPatch = {};
        if (d.family !== s.family && d.family !== '') patch.fontFamily = d.family;
        if (d.size !== s.size) patch.fontSize = d.size;
        if (d.bold !== s.bold) patch.bold = d.bold;
        if (d.italic !== s.italic) patch.italic = d.italic;
        return patch;
    };

    const apply = async () => {
        for (const { key, legend } of SLOTS) {
            const pt = drafts[key].size;
            if (!Number.isFinite(pt) || pt < MIN_PT || pt > MAX_PT) {
                setError(`${legend}: size must be between ${MIN_PT} and ${MAX_PT} pt.`);
                return;
            }
        }
        setBusy(true);
        setError(null);
        try {
            for (const { key, slot } of SLOTS) {
                const patch = patchFor(key);
                if (Object.keys(patch).length === 0) continue;
                if (patch.fontFamily !== undefined) await registry.ensureFont(patch.fontFamily);
                const evt = await cmd.setSlotFormat(slot, patch);
                if (evt.type === 'ERROR') throw new Error(evt.message);
            }
            props.onClose();
        } catch (e) {
            setError(e instanceof Error ? e.message : String(e));
        } finally {
            setBusy(false);
        }
    };

    return (
        <Dialog
            open={props.open}
            title="Font"
            description="Format Latin text and complex-script text (Arabic, Hebrew, …) separately. The toolbar changes both."
            onClose={props.onClose}
            size="lg"
            footer={
                <>
                    <button class="nge-btn" type="button" onClick={props.onClose}>
                        Cancel
                    </button>
                    <button
                        class="nge-btn nge-btn--primary"
                        type="button"
                        disabled={busy()}
                        data-nge-command="APPLY_FORMATTING"
                        onClick={() => void apply()}
                    >
                        Apply
                    </button>
                </>
            }
        >
            <form
                class="nge-form nge-font-dialog"
                onSubmit={(e) => {
                    e.preventDefault();
                    void apply();
                }}
            >
                <div class="nge-font-dialog__slots">
                    <For each={SLOTS}>
                        {(s) => (
                            <SlotSection
                                legend={s.legend}
                                hint={s.hint}
                                id={s.id}
                                slot={s.slot}
                                draft={drafts[s.key]}
                                fonts={registry.fonts()}
                                placeholder={!inRegistry(seeds()[s.key].family)}
                                seed={seeds()[s.key]}
                                onFamily={(v) => setDrafts(s.key, 'family', v)}
                                onSize={(v) => setDrafts(s.key, 'size', v)}
                                onBold={(v) => setDrafts(s.key, 'bold', v)}
                                onItalic={(v) => setDrafts(s.key, 'italic', v)}
                            />
                        )}
                    </For>
                </div>
                <Show when={error()}>
                    <div class="nge-form__hint nge-font-dialog__error" role="alert">
                        {error()}
                    </div>
                </Show>
            </form>
        </Dialog>
    );
};

const SlotSection: Component<{
    legend: string;
    hint: string;
    id: string;
    slot: FontSlot;
    draft: SlotDraft;
    seed: SlotDraft;
    fonts: ReadonlyArray<{ id: string; label: string }>;
    /** The seed family is not a registry font: offer it as a disabled
     *  placeholder option so the select can show it. */
    placeholder: boolean;
    onFamily: (id: string) => void;
    onSize: (pt: number) => void;
    onBold: (v: boolean) => void;
    onItalic: (v: boolean) => void;
}> = (props) => {
    /* The select's value is synced in a user effect, after the option
       list rendered (a `value` binding could run first). */
    let familyEl: HTMLSelectElement | undefined;
    createEffect(() => {
        const id = props.draft.family;
        void props.fonts.length;
        void props.placeholder;
        void props.seed.family;
        if (familyEl && familyEl.value !== id) familyEl.value = id;
    });
    const themeMarker = () => props.draft.family === props.seed.family && props.seed.fromTheme;
    return (
        <fieldset class="nge-font-dialog__slot" data-nge-font-slot={props.slot}>
            <legend class="nge-font-dialog__legend">{props.legend}</legend>
            <p class="nge-form__hint nge-font-dialog__hint">{props.hint}</p>
            <div class="nge-font-dialog__field">
                <label class="nge-form__label" for={`${props.id}-family`}>
                    Font
                </label>
                <div class="nge-font-dialog__family">
                    <select
                        ref={familyEl}
                        id={`${props.id}-family`}
                        class="nge-form__select"
                        data-nge-command="APPLY_FORMATTING"
                        data-nge-font-slot={props.slot}
                        data-nge-field="family"
                        onChange={(e) => props.onFamily(e.currentTarget.value)}
                    >
                        <Show when={props.placeholder && props.seed.family !== ''}>
                            <option value={props.seed.family} disabled>
                                {props.seed.name}
                            </option>
                        </Show>
                        <For each={props.fonts}>
                            {(f) => <option value={f.id}>{f.label}</option>}
                        </For>
                    </select>
                    <Show when={themeMarker()}>
                        <span
                            class="nge-font-dialog__source"
                            title="This font comes from the document theme"
                        >
                            (theme)
                        </span>
                    </Show>
                </div>
            </div>
            <div class="nge-font-dialog__field">
                <label class="nge-form__label" for={`${props.id}-size`}>
                    Size (pt)
                </label>
                <input
                    id={`${props.id}-size`}
                    class="nge-form__input nge-font-dialog__size"
                    type="number"
                    min={MIN_PT}
                    max={MAX_PT}
                    step="0.5"
                    value={props.draft.size}
                    data-nge-command="APPLY_FORMATTING"
                    data-nge-font-slot={props.slot}
                    data-nge-field="size"
                    onInput={(e) => props.onSize(roundPt(parseFloat(e.currentTarget.value)))}
                />
            </div>
            <div class="nge-font-dialog__styles" role="group" aria-label={`${props.legend} style`}>
                <label class="nge-font-dialog__check">
                    <input
                        type="checkbox"
                        checked={props.draft.bold}
                        data-nge-command="APPLY_FORMATTING"
                        data-nge-font-slot={props.slot}
                        data-nge-field="bold"
                        onChange={(e) => props.onBold(e.currentTarget.checked)}
                    />
                    <span class="nge-font-dialog__check-label nge-font-dialog__check-label--bold">
                        Bold
                    </span>
                </label>
                <label class="nge-font-dialog__check">
                    <input
                        type="checkbox"
                        checked={props.draft.italic}
                        data-nge-command="APPLY_FORMATTING"
                        data-nge-font-slot={props.slot}
                        data-nge-field="italic"
                        onChange={(e) => props.onItalic(e.currentTarget.checked)}
                    />
                    <span class="nge-font-dialog__check-label nge-font-dialog__check-label--italic">
                        Italic
                    </span>
                </label>
            </div>
        </fieldset>
    );
};
