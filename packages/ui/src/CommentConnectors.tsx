/**
 * CommentConnectors - issue #465: Word's dotted leader from each
 * comment's anchor on the page to its card in the comments rail.
 *
 * One full-window SVG (click-through), laid out in client px from DOM
 * geometry the shell already has: the page cards (`.editor-page`), the
 * rail's cards (`.nge-cm__card[data-comment-id]`) and the editor
 * viewport. The anchor is derived from `createEditorState().
 * commentHighlights` (see `commentConnector.anchorPoint`: the first
 * rect's leading edge in reading direction; a point comment's 3 pt
 * marker is its own first rect), so no engine event is involved.
 *
 * Re-laid (one coalesced frame) on any scroll, window / rail / viewport
 * resize, rail DOM change and state change (highlights, page geometry,
 * zoom, active comment). Nothing is drawn when the rail is not in the
 * DOM, and a connector is hidden while its anchor is scrolled out of the
 * editor viewport or its card out of the rail's visible band. Mount it
 * after `CommentsRail`, in the same `<Show>`.
 */
import { createEffect, createSignal, For, onCleanup, onMount, type Component } from 'solid-js';
import { createEditorState, deviceRatioFor, useEngine } from '@nge/core';
import { commentFocusFor } from './commentFocus';
import { anchorPoint, connectorPath, inBand } from './commentConnector';
import './CommentConnectors.css';

interface Connector {
    id: number;
    d: string;
    resolved: boolean;
}

export const CommentConnectors: Component = () => {
    const engine = useEngine();
    const state = createEditorState();
    const focus = commentFocusFor(engine);
    const [items, setItems] = createSignal<Connector[]>([]);
    let frame: number | undefined;
    let lastKey = '';

    const publish = (next: Connector[]): void => {
        /* Re-publish only on a real change: the <For> would otherwise
           remount every path on each scroll frame. */
        const key = next.map((c) => `${c.id}|${c.resolved}|${c.d}`).join(';');
        if (key === lastKey) return;
        lastKey = key;
        setItems(next);
    };

    const compute = (): void => {
        frame = undefined;
        const rail = document.querySelector<HTMLElement>('.nge-cm');
        const viewport = document.querySelector<HTMLElement>('.editor-viewport');
        const highlights = state.commentHighlights();
        if (!rail || !viewport || highlights.length === 0) {
            publish([]);
            return;
        }
        const railRect = rail.getBoundingClientRect();
        if (railRect.width === 0 || railRect.height === 0) {
            publish([]);
            return;
        }
        const band = (rail.closest('.nge-shell__rails') ?? rail).getBoundingClientRect();
        const view = viewport.getBoundingClientRect();
        const geo = state.pageGeometry();
        const ratio = deviceRatioFor(state.zoom(), window.devicePixelRatio || 1);
        const out: Connector[] = [];
        for (const h of highlights) {
            const first = h.rects[0];
            if (!first) continue;
            let pageIdx = 0;
            for (let i = 0; i < geo.tops.length; i++) {
                if ((geo.tops[i] ?? 0) <= first.y) pageIdx = i;
            }
            const page = document.querySelector<HTMLElement>(
                `.editor-page[data-page-index="${pageIdx}"]`,
            );
            const card = rail.querySelector<HTMLElement>(
                `.nge-cm__card[data-comment-id="${h.id}"]`,
            );
            if (!page || !card) continue;
            const pr = page.getBoundingClientRect();
            const top = geo.tops[pageIdx] ?? 0;
            const rtl = getComputedStyle(page).direction === 'rtl';
            const a = anchorPoint(
                [
                    {
                        x: first.x / ratio,
                        y: (first.y - top) / ratio,
                        w: first.w / ratio,
                        h: first.h / ratio,
                    },
                ],
                rtl,
            );
            if (!a) continue;
            const anchor = { x: pr.left + a.x, y: pr.top + a.y };
            const cr = card.getBoundingClientRect();
            const cardY = cr.top + Math.min(16, cr.height / 2);
            if (!inBand(anchor.y, view.top, view.bottom)) continue;
            if (!inBand(anchor.x, view.left, view.right)) continue;
            if (!inBand(cardY, band.top, band.bottom)) continue;
            const railOnRight = railRect.left >= anchor.x;
            out.push({
                id: h.id,
                d: connectorPath(
                    anchor,
                    railOnRight ? pr.right : pr.left,
                    railOnRight ? railRect.left : railRect.right,
                    cardY,
                ),
                resolved: h.resolved,
            });
        }
        publish(out);
    };
    const schedule = (): void => {
        if (frame !== undefined) return;
        frame = requestAnimationFrame(compute);
    };

    /* State changes. */
    createEffect(() => {
        state.commentHighlights();
        state.pageGeometry();
        state.zoom();
        focus.activeId();
        schedule();
    });

    onMount(() => {
        window.addEventListener('scroll', schedule, true);
        window.addEventListener('resize', schedule);
        const ro = new ResizeObserver(schedule);
        const mo = new MutationObserver(schedule);
        const rail = document.querySelector<HTMLElement>('.nge-cm');
        const viewport = document.querySelector<HTMLElement>('.editor-viewport');
        if (rail) {
            ro.observe(rail);
            mo.observe(rail, { childList: true, subtree: true, attributes: true });
        }
        if (viewport) ro.observe(viewport);
        schedule();
        onCleanup(() => {
            window.removeEventListener('scroll', schedule, true);
            window.removeEventListener('resize', schedule);
            ro.disconnect();
            mo.disconnect();
            if (frame !== undefined) cancelAnimationFrame(frame);
        });
    });

    return (
        <svg class="nge-comment-connectors" aria-hidden="true">
            <For each={items()}>
                {(c) => (
                    <path
                        class="nge-comment-connector"
                        classList={{
                            'nge-comment-connector--active': focus.activeId() === c.id,
                            'nge-comment-connector--resolved': c.resolved,
                        }}
                        data-comment-id={c.id}
                        d={c.d}
                    />
                )}
            </For>
        </svg>
    );
};
