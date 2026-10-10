/**
 * CommentHighlights — issue #387: the persistent on-canvas highlight of
 * commented text (Word shades it), as a DOM overlay over ONE page card —
 * never drawn into the canvas (the overlay / a11y rule).
 *
 * Mount one per page, inside the positioned page element that hosts the
 * page's `<canvas>` (the reference shell mounts it in every
 * `.editor-page`). The rects come from `createEditorState().
 * commentHighlights` (`Event::CommentHighlights`: per-line rects through
 * the engine's body geometry, like selection rects), in absolute document
 * device px; this page's slice is mapped to page-local CSS px through the
 * paginator's page tops and the zoom-aware device ratio.
 *
 * The highlights are click-through (`pointer-events: none`), so a click
 * on commented text still places the caret. Hover and click are read
 * from the page element instead: hovering a highlight shows its author,
 * clicking one makes its thread the active comment, which the
 * `CommentsRail` selects (and scrolls to) — the same shared focus the
 * rail's "Go to comment" sets, which emphasizes the highlight here.
 */
import { createMemo, createSignal, For, onCleanup, onMount, Show, type Component } from 'solid-js';
import { createEditorState, deviceRatioFor, useEngine, type CommentHighlight } from '@nge/core';
import { commentFocusFor } from './commentFocus';
import './CommentHighlights.css';

export interface CommentHighlightsProps {
    /** 0-based index of the page this overlay covers. */
    pageIdx: number;
}

interface LocalRect {
    id: number;
    author: string;
    resolved: boolean;
    x: number;
    y: number;
    w: number;
    h: number;
}

interface Hover {
    id: number;
    author: string;
    x: number;
    y: number;
}

/** Pointer travel (CSS px) past which a press-release is a drag, not a click. */
const CLICK_SLOP_PX = 4;

export const CommentHighlights: Component<CommentHighlightsProps> = (props) => {
    const engine = useEngine();
    const state = createEditorState();
    const focus = commentFocusFor(engine);
    const [hover, setHover] = createSignal<Hover | null>(null);
    let rootEl: HTMLDivElement | undefined;

    /* This page's highlight rects in page-local CSS px. */
    const rects = createMemo<LocalRect[]>(() => {
        const all: CommentHighlight[] = state.commentHighlights();
        if (all.length === 0) return [];
        const geo = state.pageGeometry();
        const top = geo.tops[props.pageIdx] ?? (props.pageIdx === 0 ? 0 : undefined);
        if (top === undefined) return [];
        const height = geo.heights[props.pageIdx] ?? Number.POSITIVE_INFINITY;
        const ratio = deviceRatioFor(state.zoom(), window.devicePixelRatio || 1);
        const out: LocalRect[] = [];
        for (const h of all) {
            for (const r of h.rects) {
                if (r.y + r.h <= top || r.y >= top + height || r.w <= 0) continue;
                out.push({
                    id: h.id,
                    author: h.author,
                    resolved: h.resolved,
                    x: r.x / ratio,
                    y: (r.y - top) / ratio,
                    w: r.w / ratio,
                    h: r.h / ratio,
                });
            }
        }
        return out;
    });

    /* The highlight under a page-local point: the smallest one when
       comments overlap (the most specific range). */
    const hitAt = (x: number, y: number): LocalRect | undefined => {
        let best: LocalRect | undefined;
        for (const r of rects()) {
            if (x < r.x || x > r.x + r.w || y < r.y || y > r.y + r.h) continue;
            if (!best || r.w * r.h < best.w * best.h) best = r;
        }
        return best;
    };

    onMount(() => {
        const page = rootEl?.parentElement;
        if (!page) return;
        const local = (e: PointerEvent | MouseEvent): { x: number; y: number } => {
            const b = page.getBoundingClientRect();
            return { x: e.clientX - b.left, y: e.clientY - b.top };
        };
        let down: { x: number; y: number } | null = null;
        const onMove = (e: PointerEvent): void => {
            const p = local(e);
            const hit = hitAt(p.x, p.y);
            if (!hit) {
                if (hover() !== null) setHover(null);
                return;
            }
            const cur = hover();
            if (cur && cur.id === hit.id && Math.abs(cur.x - p.x) < 1) return;
            setHover({ id: hit.id, author: hit.author, x: p.x, y: hit.y });
        };
        const onLeave = (): void => {
            setHover(null);
        };
        const onDown = (e: PointerEvent): void => {
            down = local(e);
        };
        const onClick = (e: MouseEvent): void => {
            const p = local(e);
            if (down && Math.hypot(p.x - down.x, p.y - down.y) > CLICK_SLOP_PX) return;
            /* Only clicks on the page itself (canvas / overlays), never on
               a control a host mounted inside the card. */
            const t = e.target;
            if (t instanceof HTMLElement && t.closest('button, input, select, textarea')) return;
            const hit = hitAt(p.x, p.y);
            focus.setActiveId(hit ? hit.id : null);
        };
        page.addEventListener('pointermove', onMove);
        page.addEventListener('pointerleave', onLeave);
        page.addEventListener('pointerdown', onDown);
        page.addEventListener('click', onClick);
        onCleanup(() => {
            page.removeEventListener('pointermove', onMove);
            page.removeEventListener('pointerleave', onLeave);
            page.removeEventListener('pointerdown', onDown);
            page.removeEventListener('click', onClick);
        });
    });

    return (
        <div
            ref={(el) => (rootEl = el)}
            class="nge-comment-highlights"
            data-page-index={props.pageIdx}
            aria-hidden="true"
        >
            <For each={rects()}>
                {(r) => (
                    <div
                        class="nge-comment-highlight"
                        classList={{
                            'nge-comment-highlight--resolved': r.resolved,
                            'nge-comment-highlight--active': focus.activeId() === r.id,
                            'nge-comment-highlight--hover': hover()?.id === r.id,
                        }}
                        data-comment-id={r.id}
                        data-author={r.author}
                        style={{
                            left: `${r.x}px`,
                            top: `${r.y}px`,
                            width: `${r.w}px`,
                            height: `${r.h}px`,
                        }}
                    />
                )}
            </For>
            <Show when={hover()}>
                {(h) => (
                    <div
                        class="nge-comment-highlight__tip"
                        data-comment-id={h().id}
                        style={{ left: `${h().x}px`, top: `${h().y}px` }}
                    >
                        {h().author || 'Anonymous'}
                    </div>
                )}
            </Show>
        </div>
    );
};
