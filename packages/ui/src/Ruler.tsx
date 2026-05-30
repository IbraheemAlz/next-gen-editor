/**
 * Ruler — horizontal page ruler for indentation + tab stops.
 *
 * Sprint 11 (#13). Reads the active section's geometry through
 * `createEditorState().sectionGeometry()`, so the 0-mark aligns to
 * the content-area leading edge of the section under the caret.
 * Falls back to A4 + 1-inch margins when the engine has not emitted
 * geometry yet (fresh, empty document).
 *
 * **Layout sync (Sprint 15).** The ruler row spans the full shell
 * width, but the strip is absolutely pinned to the *measured* bounds
 * of the real `.editor-page` card (`measurePage()` + a ResizeObserver
 * / scroll listener). That inherits the viewport's centering,
 * scrollbar gutter, and live horizontal scroll, so the 0-mark always
 * sits over the paper's leading edge — like Word / Google Docs. Every
 * `getBoundingClientRect` runs after `onMount` behind a null guard, so
 * the component never measures an unmounted node.
 *
 * **Indent read-back (Sprint 15).** The active paragraph's `<w:ind>`
 * rides on `SELECTION_CHANGED` as `state.paragraphIndent()`. The
 * handles jump to those values when the caret moves, and a drag of one
 * handle preserves the other two (no more zero-clobber).
 *
 * Drag discipline:
 *  - First-line handle (▽) edits `Paragraph.props.indent.first_line`
 *    (positive) or `hanging` (negative).
 *  - Left-margin handle (△) edits `Paragraph.props.indent.start`.
 *  - Tab-stop markers (`L` triangle) — click on an empty ruler slot
 *    adds a Left tab; click on an existing marker cycles
 *    `Left → Center → Right → Decimal → removed`; drag to move;
 *    drag below the ruler-strip to remove.
 *
 * **Bridge discipline (load-bearing).** All drag motion updates a
 * pure local Solid signal — NEVER a `cmd.*` dispatch. We commit
 * exactly once, on `pointerup`. The engine receives one
 * `Command::SetParagraphIndent` / `Command::SetTabStops` per gesture,
 * which the undo stack records as a single entry.
 *
 * **RTL.** When `paragraphDirection() === 'Rtl'`, the leading edge is
 * the right side of the content area, so handles + tick math flip
 * horizontally (Word's ruler does the same).
 */
import {
    createMemo,
    createSignal,
    onCleanup,
    onMount,
    Show,
    For,
    type Component,
} from 'solid-js';
import {
    createEditorCommands,
    createEditorState,
    type BridgeTabKind,
    type BridgeTabStop,
} from '@nge/core';
import './Ruler.css';

const PT_PER_INCH = 72;
const PX_PER_PT = 96 / 72; /* CSS px per layout pt (1 in = 96 px / 72 pt). */
const MAJOR_PER_INCH = 1;
const MINOR_PER_INCH = 8; /* 0.125-inch tick spacing. */

interface DragState {
    kind: 'first-line' | 'left-indent' | 'end-indent' | 'tab-move' | 'tab-add';
    /** Tab-stop index in the local copy, when kind is `tab-move`. */
    tabIndex?: number;
    /** Live position in pt relative to content-area leading edge. */
    livePt: number;
    /** Whether the drag is below the ruler strip — triggers remove on release. */
    discard: boolean;
}

const HANDLE_HIT_PX = 14;

/* Cycle order for tab kinds when clicking an existing marker. */
const TAB_CYCLE: BridgeTabKind[] = ['Left', 'Center', 'Right', 'Decimal'];

function nextKind(k: BridgeTabKind): BridgeTabKind {
    const i = TAB_CYCLE.indexOf(k);
    /* Beyond `Decimal` wraps to `Left`; `Clear` is engine-side and
    not part of the UI cycle. */
    return TAB_CYCLE[(i + 1) % TAB_CYCLE.length] ?? 'Left';
}

function tabLabel(k: BridgeTabKind): string {
    switch (k) {
        case 'Left':
            return 'L';
        case 'Center':
            return 'C';
        case 'Right':
            return 'R';
        case 'Decimal':
            return 'D';
        case 'Clear':
            return '–';
        default:
            return 'L';
    }
}

export interface RulerProps {
    /**
     * Optional explicit unit toggle. Defaults to `in` (inches) — the
     * Word default. Units affect only label rendering; engine commits
     * always use pt.
     */
    unit?: 'in' | 'pt';
}

/* A4-portrait fallback in pt (matches `engine::PageGeometry::a4`). */
const A4_FALLBACK = {
    width_pt: 595.3,
    margin_left_pt: 72,
    margin_right_pt: 72,
};

export const Ruler: Component<RulerProps> = (props) => {
    const cmd = createEditorCommands();
    const state = createEditorState();
    const [drag, setDrag] = createSignal<DragState | null>(null);
    /* Local copy of tab stops the user is editing this gesture — bound
     * to the engine's authoritative list on every SELECTION_CHANGED
     * but mutated locally during a drag for instant visual feedback.
     * Committed back via `cmd.setTabStops` on drag release. */
    const [localStops, setLocalStops] = createSignal<BridgeTabStop[] | null>(null);
    /* Ref to the ruler strip — bound by the JSX `ref={stripEl}` attribute.
     * Used by the window-level pointermove handler, which sees
     * `e.currentTarget === window` (no `.getBoundingClientRect`); reading
     * the strip directly via this ref is the only safe path. */
    let stripEl: HTMLDivElement | undefined;
    /* Ref to the ruler root — the measurement frame the strip is pinned
     * inside. `getBoundingClientRect` on either ref is ONLY ever called
     * after `onMount` (and re-guarded with a null check), so the
     * component never measures an unmounted node (failure #2). */
    let rulerEl: HTMLDivElement | undefined;

    /* Sprint 15 (#13) — measured position of the real `.editor-page` card,
     * expressed in CSS px RELATIVE TO the ruler root. The strip is pinned
     * to this so the 0-mark sits exactly over the paper's left edge,
     * inheriting the viewport's centering, scrollbar gutter, and live
     * horizontal scroll for free (failure #1 — alignment). `null` until
     * the first measurement lands; the strip then falls back to the
     * left edge for the single boot frame. */
    const [pageLeftPx, setPageLeftPx] = createSignal<number | null>(null);

    /* Re-measure the paper relative to the ruler. Guarded end-to-end:
     * bails if either node is missing (pre-mount, crash remount) or the
     * page has no layout box yet (width 0 before first paint). */
    const measurePage = () => {
        if (!rulerEl) return;
        const page = document.querySelector<HTMLElement>(
            '.editor-page[data-page-index="0"]',
        );
        if (!page) return;
        const pageRect = page.getBoundingClientRect();
        if (pageRect.width <= 0) return;
        const rulerRect = rulerEl.getBoundingClientRect();
        setPageLeftPx(pageRect.left - rulerRect.left);
    };

    /* Wire the measurement to every input that can move the paper:
     * window resize + rail/toolbar reflow (ResizeObserver on the ruler
     * itself), viewport resize, and live scroll. All teardown is
     * registered through onCleanup so a crash remount leaves no dangling
     * listeners. The scroll listener is `passive` — the ruler never
     * cancels the scroll, it only follows it. */
    onMount(() => {
        const viewport = document.querySelector<HTMLElement>('.editor-viewport');

        /* Initial measure now, plus one on the next frame in case the
         * page card has not taken its final centered position yet (fonts,
         * scrollbar appearing on first paint). */
        measurePage();
        const raf = requestAnimationFrame(measurePage);
        onCleanup(() => cancelAnimationFrame(raf));

        const onScroll = () => measurePage();
        viewport?.addEventListener('scroll', onScroll, { passive: true });
        onCleanup(() => viewport?.removeEventListener('scroll', onScroll));

        if (typeof ResizeObserver !== 'undefined') {
            const ro = new ResizeObserver(() => measurePage());
            if (rulerEl) ro.observe(rulerEl);
            if (viewport) ro.observe(viewport);
            onCleanup(() => ro.disconnect());
        }

        /* Window resize re-centres the page even when neither observed box
         * changes size (e.g. devtools dock toggling the inner width). */
        window.addEventListener('resize', onScroll);
        onCleanup(() => window.removeEventListener('resize', onScroll));
    });

    const geom = () => state.sectionGeometry();
    const isRtl = () => state.paragraphDirection() === 'Rtl';

    /* Direction-aware marker tooltips. The handles edit LOGICAL indents
     * (`<w:start>` / `<w:firstLine>`), so the label names the logical
     * property and the physical edge it currently sits on — the start
     * handle is on the RIGHT for an RTL paragraph. */
    const firstLineLabel = () => 'First-line indent';
    const startIndentLabel = () =>
        isRtl() ? 'Start indent (leading — right edge)' : 'Left (start) indent';
    const endIndentLabel = () => 'End indent (trailing edge)';

    /** Content-area width in pt. */
    const contentWidthPt = () => {
        const g = geom();
        if (!g) return A4_FALLBACK.width_pt - A4_FALLBACK.margin_left_pt - A4_FALLBACK.margin_right_pt;
        return g.width_pt - g.margin_left_pt - g.margin_right_pt;
    };

    /** Leading-edge margin (the 0-mark offset from the ruler's left in pt). */
    const leadingMarginPt = () => {
        const g = geom();
        if (!g) return A4_FALLBACK.margin_left_pt;
        return isRtl() ? g.margin_right_pt : g.margin_left_pt;
    };

    /** Trailing margin in pt — used to draw the trailing margin band. */
    const trailingMarginPt = () => {
        const g = geom();
        if (!g) return A4_FALLBACK.margin_right_pt;
        return isRtl() ? g.margin_left_pt : g.margin_right_pt;
    };

    const totalWidthPt = () => {
        const g = geom();
        return g?.width_pt ?? A4_FALLBACK.width_pt;
    };

    /** pt → CSS px (for absolute positioning inside the ruler). */
    const ptToPx = (pt: number) => pt * PX_PER_PT;

    /** Map a pointer client-x to a logical pt offset from the 0-mark
     * (i.e. content-area leading edge). RTL flips the axis. */
    const clientXToContentPt = (clientX: number, rect: DOMRect): number => {
        const rawPxFromLeft = clientX - rect.left;
        const rawPt = rawPxFromLeft / PX_PER_PT;
        if (isRtl()) {
            /* Leading edge is the RIGHT side of the content area. */
            const leadingPxFromLeft = leadingMarginPt() + contentWidthPt();
            const leadingPt = leadingPxFromLeft;
            return Math.max(0, leadingPt - rawPt);
        }
        return Math.max(0, rawPt - leadingMarginPt());
    };

    /** CSS `left` (px) for a content-pt offset measured from the
     * leading edge. RTL: the further out the offset, the closer to
     * the left of the content area visually. */
    const contentPtToLeftPx = (contentPt: number): number => {
        if (isRtl()) {
            const trailingEdgePt = leadingMarginPt() + contentWidthPt();
            return ptToPx(trailingEdgePt - contentPt);
        }
        return ptToPx(leadingMarginPt() + contentPt);
    };

    /* ---- pointer drag plumbing ------------------------------------- */

    const onPointerMove = (e: PointerEvent) => {
        const d = drag();
        if (!d) return;
        /* `e.currentTarget` is `window` for the window-level listener and
         * has no `.getBoundingClientRect`. Read the strip via the bound
         * ref instead; guard in case Solid disposed the node mid-drag. */
        if (!stripEl) return;
        const rect = stripEl.getBoundingClientRect();
        const pt = clientXToContentPt(e.clientX, rect);
        /* Discard when the pointer leaves the ruler strip vertically by
         * more than a tolerance — Word's "drag off to remove" behaviour. */
        const discard = e.clientY - rect.bottom > HANDLE_HIT_PX;
        setDrag({ ...d, livePt: pt, discard });
        if (d.kind === 'tab-move' && d.tabIndex !== undefined) {
            const stops = localStops();
            if (stops) {
                const next = stops.slice();
                const existing = next[d.tabIndex];
                if (existing) {
                    next[d.tabIndex] = { ...existing, position_pt: pt };
                }
                setLocalStops(next);
            }
        }
    };

    const commitDrag = () => {
        const d = drag();
        if (!d) return;
        setDrag(null);
        const range = state.selection();
        if (!range) return;
        switch (d.kind) {
            case 'first-line': {
                /* First-line indent: positive pt = first-line indent,
                 * negative pt = hanging indent (engine bakes both into
                 * the `<w:ind>` block atomically). The other two fields
                 * (start, end) stay at zero because the ruler does not
                 * own them — they're driven by the left-indent handle. */
                const startPt = currentStartPt();
                /* The pointer's content-pt is relative to the leading
                 * edge; the first-line delta is (pointer pt - start). */
                const firstLinePt = d.livePt - startPt;
                void cmd.setParagraphIndent(startPt, currentEndPt(), firstLinePt);
                break;
            }
            case 'left-indent': {
                const firstLinePt = currentFirstLinePt();
                void cmd.setParagraphIndent(d.livePt, currentEndPt(), firstLinePt);
                break;
            }
            case 'end-indent': {
                /* `d.livePt` is the pointer's content-pt measured from the
                 * LEADING edge; the end indent is the inset from the
                 * TRAILING edge, so `end = contentWidth - livePt`. Preserve
                 * the start + first-line fields (read-back). */
                const endPt = Math.max(0, contentWidthPt() - d.livePt);
                void cmd.setParagraphIndent(currentStartPt(), endPt, currentFirstLinePt());
                break;
            }
            case 'tab-add': {
                const stops = (localStops() ?? state.tabStops()).slice();
                stops.push({ position_pt: d.livePt, kind: 'Left' });
                stops.sort((a, b) => a.position_pt - b.position_pt);
                void cmd.setTabStops(stops);
                setLocalStops(null);
                break;
            }
            case 'tab-move': {
                if (d.tabIndex === undefined) return;
                const stops = (localStops() ?? state.tabStops()).slice();
                if (d.discard) {
                    stops.splice(d.tabIndex, 1);
                } else {
                    const existing = stops[d.tabIndex];
                    if (existing) {
                        stops[d.tabIndex] = { ...existing, position_pt: d.livePt };
                    }
                }
                stops.sort((a, b) => a.position_pt - b.position_pt);
                void cmd.setTabStops(stops);
                setLocalStops(null);
                break;
            }
        }
    };

    /* The drag state retains its handle identity until pointerup; we
     * attach window listeners so a drag that leaves the ruler still
     * tracks. Cleaned up in onCleanup. */
    const onWindowMove = (e: PointerEvent) => onPointerMove(e);
    const onWindowUp = () => commitDrag();
    window.addEventListener('pointermove', onWindowMove);
    window.addEventListener('pointerup', onWindowUp);
    onCleanup(() => {
        window.removeEventListener('pointermove', onWindowMove);
        window.removeEventListener('pointerup', onWindowUp);
    });

    /* ---- indent value accessors (in pt) ---------------------------- */

    /* Sprint 15 (#13) — read-back of the active paragraph's `<w:ind>`,
     * surfaced on every `SELECTION_CHANGED` via
     * `state.paragraphIndent()`. This is what makes the handles JUMP to
     * a paragraph's real indent when the caret lands there (failure #3),
     * and what lets a drag of one handle PRESERVE the others instead of
     * zeroing them — `commitDrag` reads the two it isn't editing from
     * here. `first_line_pt` is SIGNED + relative to `start` (positive →
     * first-line, negative → hanging), matching the engine. */
    const currentStartPt = () => state.paragraphIndent().start_pt;
    const currentEndPt = () => state.paragraphIndent().end_pt;
    const currentFirstLinePt = () => state.paragraphIndent().first_line_pt;

    /* ---- tab-stop accessors ---------------------------------------- */

    const effectiveStops = createMemo<BridgeTabStop[]>(() => localStops() ?? state.tabStops());

    /* ---- click handlers -------------------------------------------- */

    const onStripPointerDown = (e: PointerEvent) => {
        if (e.button !== 0) return;
        const target = e.target as HTMLElement;
        /* Clicks on a handle / marker have their own onpointerdown. */
        if (target.closest('.nge-ruler__handle, .nge-ruler__tab')) return;
        /* Prefer the bound ref; fall back to `currentTarget` so the
         * listener still works if it ever moves off the strip. */
        const strip = stripEl ?? (e.currentTarget as HTMLElement | null);
        if (!strip) return;
        const rect = strip.getBoundingClientRect();
        const pt = clientXToContentPt(e.clientX, rect);
        /* Outside content area = no-op. */
        if (pt < 0 || pt > contentWidthPt()) return;
        setDrag({ kind: 'tab-add', livePt: pt, discard: false });
    };

    const onHandlePointerDown = (
        e: PointerEvent,
        kind: 'first-line' | 'left-indent' | 'end-indent',
    ) => {
        e.stopPropagation();
        e.preventDefault();
        setDrag({ kind, livePt: 0, discard: false });
    };

    const onTabPointerDown = (e: PointerEvent, index: number, stop: BridgeTabStop) => {
        e.stopPropagation();
        e.preventDefault();
        /* Cache the current stops list as the "local" baseline so a
         * drag mutates this copy until release. A plain click (no
         * meaningful move) commits as a kind-cycle. */
        setLocalStops(state.tabStops().slice());
        setDrag({
            kind: 'tab-move',
            tabIndex: index,
            livePt: stop.position_pt,
            discard: false,
        });
    };

    /* Click vs drag — onpointerup synthesises a click when the marker
     * hasn't moved enough; cycle the kind instead. We piggyback on
     * `commitDrag`'s pointerup hook by checking the live pt against
     * the original. Cleaner: do it here, BEFORE commitDrag fires
     * `cmd.setTabStops`. */
    const onTabPointerUp = (e: PointerEvent, index: number, stop: BridgeTabStop) => {
        const d = drag();
        if (!d || d.kind !== 'tab-move') return;
        const moved = Math.abs(d.livePt - stop.position_pt) * PX_PER_PT > 4;
        if (moved) return; /* drag-commit path will handle it */
        /* Pure click — cycle kind. Replace the drag intent with an
         * in-place kind change; cancel commitDrag's tab-move branch
         * by short-circuiting via the local stops list. */
        e.stopPropagation();
        const stops = (localStops() ?? state.tabStops()).slice();
        const existing = stops[index];
        if (existing) {
            stops[index] = { ...existing, kind: nextKind(existing.kind) };
            void cmd.setTabStops(stops);
        }
        setLocalStops(null);
        setDrag(null);
    };

    /* ---- ruler geometry / ticks ------------------------------------ */

    const totalPx = () => ptToPx(totalWidthPt());

    const ticks = createMemo(() => {
        const tot = totalWidthPt();
        const out: { x: number; major: boolean; label: string }[] = [];
        const ticksPerPt = MINOR_PER_INCH / PT_PER_INCH;
        const totalTicks = Math.floor(tot * ticksPerPt);
        for (let i = 0; i <= totalTicks; i++) {
            const pt = i / ticksPerPt;
            const major = i % MINOR_PER_INCH === 0;
            /* Label inches relative to the content area's leading
             * edge — that's the canonical Word ruler convention. */
            const contentPt = isRtl() ? leadingMarginPt() + contentWidthPt() - pt : pt - leadingMarginPt();
            let label = '';
            if (major && contentPt >= 0 && contentPt <= contentWidthPt() + 0.1) {
                const inches = contentPt / PT_PER_INCH;
                if (Math.abs(inches) > 0.01) {
                    label = Math.round(inches).toString();
                }
            }
            out.push({ x: ptToPx(pt), major, label });
        }
        return out;
    });

    /* ---- handle positions ------------------------------------------ */

    /* Effective indent values during a live drag fall back to the
     * committed engine state (zero shims today; the read-back path
     * lives on a separate issue — see comment near currentStartPt). */
    const liveFirstLinePt = () => {
        const d = drag();
        if (d?.kind === 'first-line') return d.livePt;
        /* Dragging the left indent carries the first-line marker with it
         * (the engine stores first-line RELATIVE to start), so preview it
         * at the dragged start + the preserved relative offset. */
        if (d?.kind === 'left-indent') return d.livePt + currentFirstLinePt();
        return currentStartPt() + currentFirstLinePt();
    };
    const liveLeftIndentPt = () => {
        const d = drag();
        if (d?.kind === 'left-indent') return d.livePt;
        return currentStartPt();
    };
    /** End (trailing-edge) indent in pt. During a drag the pointer's
     * leading-relative content-pt converts to a trailing inset
     * (`contentWidth - livePt`); otherwise the committed engine value. */
    const liveEndIndentPt = () => {
        const d = drag();
        if (d?.kind === 'end-indent') {
            return Math.max(0, contentWidthPt() - d.livePt);
        }
        return currentEndPt();
    };

    /* Reset focus to whatever owned it before the drag — the canvas
     * input must keep focus so typing continues into the document. */
    const restoreFocus = () => {
        const ta = document.querySelector<HTMLTextAreaElement>(
            '#nge-hidden-input, textarea[data-nge-hidden-input]',
        );
        ta?.focus();
    };

    return (
        <div
            ref={rulerEl}
            class="nge-ruler"
            data-rtl={isRtl() ? 'true' : 'false'}
            role="region"
            aria-label="Page ruler — drag to set indents and tab stops"
        >
            <div
                ref={stripEl}
                class="nge-ruler__strip"
                style={{
                    width: `${totalPx()}px`,
                    /* Pin the strip's left edge to the measured paper card
                     * so the 0-mark sits over the page's left edge. Falls
                     * back to 0 for the single pre-measurement boot frame. */
                    left: `${pageLeftPx() ?? 0}px`,
                }}
                onPointerDown={onStripPointerDown}
                onPointerUp={restoreFocus}
            >
                {/* Margin bands (leading + trailing). */}
                <div
                    class="nge-ruler__margin nge-ruler__margin--leading"
                    style={
                        isRtl()
                            ? { right: '0', width: `${ptToPx(trailingMarginPt())}px` }
                            : { left: '0', width: `${ptToPx(leadingMarginPt())}px` }
                    }
                />
                <div
                    class="nge-ruler__margin nge-ruler__margin--trailing"
                    style={
                        isRtl()
                            ? { left: '0', width: `${ptToPx(leadingMarginPt())}px` }
                            : { right: '0', width: `${ptToPx(trailingMarginPt())}px` }
                    }
                />

                {/* Tick marks. */}
                <For each={ticks()}>
                    {(t) => (
                        <div
                            class="nge-ruler__tick"
                            data-major={t.major ? 'true' : 'false'}
                            style={{ left: `${t.x}px` }}
                        >
                            <Show when={t.label !== ''}>
                                <span class="nge-ruler__tick-label">{t.label}</span>
                            </Show>
                        </div>
                    )}
                </For>

                {/* Tab-stop markers. */}
                <For each={effectiveStops()}>
                    {(stop, i) => (
                        <button
                            type="button"
                            class="nge-ruler__tab"
                            data-kind={stop.kind}
                            style={{ left: `${contentPtToLeftPx(stop.position_pt)}px` }}
                            title={`Tab stop (${stop.kind})`}
                            onPointerDown={(e) => onTabPointerDown(e, i(), stop)}
                            onPointerUp={(e) => onTabPointerUp(e, i(), stop)}
                        >
                            {tabLabel(stop.kind)}
                        </button>
                    )}
                </For>

                {/* Indent handles. Always at the leading edge so the
                 * ruler looks like Word's even before any indent value
                 * is wired through SELECTION_CHANGED. Tooltips use the
                 * LOGICAL name (`start`, not `left`) and call out the
                 * physical edge per direction — the start handle sits on
                 * the RIGHT for an RTL paragraph (`<w:start>` is the
                 * leading edge), so a hard-coded "Left indent" label
                 * would mislead. Standard `title` attributes, matching
                 * the toolbar's tooltip convention (no Tooltip component
                 * exists in @nge/ui). */}
                <div
                    class="nge-ruler__handle nge-ruler__handle--first-line"
                    role="slider"
                    aria-label={firstLineLabel()}
                    title={firstLineLabel()}
                    aria-valuenow={liveFirstLinePt()}
                    style={{ left: `${contentPtToLeftPx(liveFirstLinePt())}px` }}
                    onPointerDown={(e) => onHandlePointerDown(e, 'first-line')}
                >
                    ▽
                </div>
                <div
                    class="nge-ruler__handle nge-ruler__handle--left-indent"
                    role="slider"
                    aria-label={startIndentLabel()}
                    title={startIndentLabel()}
                    aria-valuenow={liveLeftIndentPt()}
                    style={{ left: `${contentPtToLeftPx(liveLeftIndentPt())}px` }}
                    onPointerDown={(e) => onHandlePointerDown(e, 'left-indent')}
                >
                    △
                </div>
                {/* End (trailing-edge) indent — physical RIGHT for LTR,
                 * physical LEFT for RTL. Position is logically driven:
                 * `contentWidth - end` from the leading edge, fed through
                 * the same direction-aware `contentPtToLeftPx` as every
                 * other marker, so the physical side flips for free. */}
                <div
                    class="nge-ruler__handle nge-ruler__handle--end-indent"
                    role="slider"
                    aria-label={endIndentLabel()}
                    title={endIndentLabel()}
                    aria-valuenow={liveEndIndentPt()}
                    style={{
                        left: `${contentPtToLeftPx(contentWidthPt() - liveEndIndentPt())}px`,
                    }}
                    onPointerDown={(e) => onHandlePointerDown(e, 'end-indent')}
                >
                    △
                </div>

                {/* Live drag preview marker — only visible during a tab-add. */}
                <Show when={drag()?.kind === 'tab-add'}>
                    <div
                        class="nge-ruler__ghost"
                        style={{ left: `${contentPtToLeftPx(drag()!.livePt)}px` }}
                    >
                        L
                    </div>
                </Show>
            </div>
            <Show when={props.unit === 'pt'}>
                <span class="nge-ruler__unit-badge">pt</span>
            </Show>
        </div>
    );
};
