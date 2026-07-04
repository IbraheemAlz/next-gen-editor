/**
 * StylesDropdown — paragraph styles picker.
 *
 * **Sprint 12 (#11)** — wired through the real engine path. Each
 * entry dispatches `Command::ApplyStyle { style_id }`; the engine
 * sets `Paragraph.style_id`, folds the `<w:style>` cascade into the
 * resolved props, and preserves direct overrides via the shadow
 * approach.
 *
 * **Issue #29** — the active indicator is a truthful engine
 * read-back: it derives from `state.paragraphStyleId()` (the caret
 * paragraph's `<w:pStyle>`, carried on every SELECTION_CHANGED),
 * falling back to `Normal` when the paragraph is detached. A click
 * still sets a brief optimistic echo for snappy UX, but the next
 * engine read-back always wins — the echo is cleared as soon as
 * `paragraphStyleId` updates (or immediately if the engine refuses
 * the apply with `Event::Error`).
 *
 * **Issue #21** — each row carries a hover/focus-revealed ✎ edit
 * button that opens `StyleEditDialog` for that id instead of
 * applying it; the dialog dispatches `Command::ModifyStyle` to
 * mutate the style DEFINITION (the engine re-cascades every
 * dependent paragraph and regenerates styles.xml on save).
 */
import { createEffect, createSignal, For, Show, type Component } from 'solid-js';
import {
    createEditorCommands,
    createEditorState,
    STYLE_PRESETS,
    type ParagraphStyleId,
} from '@nge/core';
import { StyleEditDialog } from './StyleEditDialog';
import './StylesDropdown.css';

const STYLES: { id: ParagraphStyleId; label: string }[] = [
    { id: 'Normal', label: 'Normal' },
    { id: 'Title', label: 'Title' },
    { id: 'Heading1', label: 'Heading 1' },
    { id: 'Heading2', label: 'Heading 2' },
    { id: 'Heading3', label: 'Heading 3' },
];

export const StylesDropdown: Component = () => {
    const cmd = createEditorCommands();
    const state = createEditorState();
    const [open, setOpen] = createSignal(false);
    /* #29 — optimistic echo shown between the click and the engine's
     * SELECTION_CHANGED read-back. Never the source of truth. */
    const [optimistic, setOptimistic] = createSignal<ParagraphStyleId | undefined>(
        undefined,
    );
    const [editTarget, setEditTarget] = createSignal<
        { id: ParagraphStyleId; label: string } | undefined
    >(undefined);

    /* Engine read-back wins: any update to the caret paragraph's
     * `<w:pStyle>` (post-apply, caret move, undo, …) clears the echo. */
    createEffect(() => {
        void state.paragraphStyleId();
        setOptimistic(undefined);
    });

    /** Active style id — engine truth, optimistic echo only while a
     *  just-clicked apply is in flight, `Normal` when detached. */
    const active = (): ParagraphStyleId =>
        optimistic() ?? state.paragraphStyleId() ?? 'Normal';

    const ready = () => state.selection() !== undefined;
    /* Phase 3 (#39) — `Command::ApplyStyle` and `Command::ModifyStyle`
     * are both engine-gated while a header/footer story is active
     * ("style application/editing" on the story blocklist). Gate the
     * trigger, every menu row's apply action, and the row's edit (✎)
     * entry point — all three would otherwise round-trip to a silent
     * `Event::Error`. */
    const inStory = () => state.editingStory() !== undefined;
    const gatedTitle = (label: string) =>
        inStory() ? 'Not available while editing a header or footer' : label;

    const apply = async (id: ParagraphStyleId) => {
        if (inStory()) return;
        setOptimistic(id);
        setOpen(false);
        /* Sprint 12 — real engine path: writes `<w:pStyle>` + folds
         * the style cascade. `Normal` detaches by passing the same
         * id (engine treats it as a regular style; if the loaded
         * doc has no `Normal` entry, the cascade falls back to
         * `style_defaults` which gives the Word default look). */
        const evt = await cmd.applyStyle(id);
        if (evt.type === 'ERROR') {
            /* Engine refused — drop the echo so the truthful
             * read-back shows through again. */
            setOptimistic(undefined);
        }
    };

    const previewStyle = (id: ParagraphStyleId): string => {
        const p = STYLE_PRESETS[id];
        if (!p) return '';
        const parts: string[] = [];
        if (p.font_size) parts.push(`font-size: ${p.font_size}px`);
        if (p.bold) parts.push('font-weight: 700');
        return parts.join('; ');
    };

    /** Trigger label: known ids get their pretty label; a custom id
     *  from a loaded document's stylesheet displays verbatim. */
    const activeLabel = () =>
        STYLES.find((s) => s.id === active())?.label ?? active();

    return (
        <div class="nge-styles">
            <button
                class="nge-btn nge-styles__trigger"
                type="button"
                aria-haspopup="menu"
                aria-expanded={open()}
                aria-label="Paragraph style"
                disabled={!ready() || inStory()}
                title={gatedTitle('Paragraph style')}
                onClick={() => setOpen((v) => !v)}
            >
                <span>{activeLabel()}</span>
                <span class="nge-styles__chevron" aria-hidden="true">▾</span>
            </button>
            <Show when={open()}>
                <ul
                    class="nge-styles__menu"
                    role="menu"
                    onMouseLeave={() => setOpen(false)}
                >
                    <For each={STYLES}>
                        {(s) => (
                            <li role="none" class="nge-styles__row">
                                <button
                                    class="nge-styles__item"
                                    type="button"
                                    role="menuitemradio"
                                    aria-checked={active() === s.id}
                                    disabled={inStory()}
                                    title={
                                        inStory()
                                            ? 'Not available while editing a header or footer'
                                            : undefined
                                    }
                                    onClick={() => void apply(s.id)}
                                >
                                    <span
                                        class="nge-styles__preview"
                                        style={previewStyle(s.id)}
                                    >
                                        {s.label}
                                    </span>
                                </button>
                                <button
                                    class="nge-styles__edit"
                                    type="button"
                                    role="menuitem"
                                    aria-label={`Edit style ${s.label}`}
                                    title={gatedTitle(`Edit the "${s.label}" style definition`)}
                                    disabled={inStory()}
                                    onClick={() => {
                                        setOpen(false);
                                        setEditTarget({ id: s.id, label: s.label });
                                    }}
                                >
                                    <span aria-hidden="true">✎</span>
                                </button>
                            </li>
                        )}
                    </For>
                    <li role="none" class="nge-styles__note">
                        Writes <code>&lt;w:pStyle&gt;</code>; folds the style cascade.
                    </li>
                </ul>
            </Show>
            <Show when={editTarget()}>
                {(t) => (
                    <StyleEditDialog
                        open={true}
                        styleId={t().id}
                        styleLabel={t().label}
                        onClose={() => setEditTarget(undefined)}
                    />
                )}
            </Show>
        </div>
    );
};
