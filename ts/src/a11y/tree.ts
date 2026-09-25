/* Phase 4 §10 / Backlog #10 / Phase 5 PR 3b — accessibility mirror reconciler.
 *
 * Applies the engine's incremental `A11yPatch` stream to the visually-hidden
 * mirror DOM. Before deltas, every keystroke rebuilt the whole `<p>` list, so
 * the browser recomputed the accessibility tree for the entire document. Now a
 * keystroke patches exactly one node, and the browser only re-derives a11y
 * for that node.
 *
 * Issue #165: text-box stories are `TEXT_BOX` nodes — `role="group"` regions
 * right after their anchor paragraph (nested boxes inside their parent's
 * region), named from the box's `docPr`, holding the body's exact `<p dir>` /
 * `<span>` structure. The region of the story being edited carries
 * `aria-current="true"` (`setActiveStory`, fed by `editing_story.rid`).
 *
 * Phase 5 PR 3b: nodes are now `A11yNode` (`Paragraph | Table`), not flat
 * paragraphs. Tables render as `<table role="table">` with `<tr role="row">`
 * and `<td role="gridcell" aria-rowspan aria-colspan>`. Continue-rows of a
 * vMerge span are not emitted — the Restart cell carries the resolved
 * `aria-rowspan`.
 *
 * The reconciler owns `root`'s children outright — Solid renders `root` as a
 * bare `ref` host and never touches what is inside it. Patches are positional:
 * the engine's prefix/suffix diff emits Updates first, then either Inserts
 * (ascending index) or Removes, so applying them in array order is correct. */
import type { A11yCell, A11yNode, A11yParagraph, A11yPatch, A11yRun } from '../engine/types';

/** Inline CSS for a run, so a screen reader can announce its formatting. */
function runStyle(r: A11yRun): string {
    const parts: string[] = [];
    if (r.bold) parts.push('font-weight:bold');
    if (r.italic) parts.push('font-style:italic');
    if (r.underline) parts.push('text-decoration:underline');
    return parts.join(';');
}

/** Build the `<p>` element for one accessibility paragraph.
 *
 *  Issue #195 — `dir` is the paragraph's OWN resolved base direction
 *  (explicit bidi → first-strong → document base, the engine's layout
 *  resolution), never the document-wide `direction`: an RTL paragraph in
 *  an LTR document must run UAX #9 with an RTL base for the screen reader.
 *  Every `<p>` sets it, so container regions (text box, header / footer)
 *  carry no `dir` and nothing is inherited. */
function buildParagraph(p: A11yParagraph): HTMLParagraphElement {
    const el = document.createElement('p');
    el.dir = p.resolved_direction === 'Rtl' ? 'rtl' : 'ltr';
    for (const run of p.runs) {
        const span = document.createElement('span');
        const style = runStyle(run);
        if (style) span.style.cssText = style;
        span.textContent = run.text;
        el.appendChild(span);
    }
    return el;
}

/** Build a `<td role="gridcell">` for one cell, recursing on nested nodes. */
function buildCell(cell: A11yCell): HTMLTableCellElement {
    const td = document.createElement('td');
    td.setAttribute('role', 'gridcell');
    if (cell.row_span > 1) td.setAttribute('aria-rowspan', String(cell.row_span));
    if (cell.col_span > 1) td.setAttribute('aria-colspan', String(cell.col_span));
    td.dataset.row = String(cell.row);
    td.dataset.col = String(cell.col);
    for (const child of cell.nodes) td.appendChild(buildNode(child));
    return td;
}

/** Build the `<table role="table">` element for one accessibility table. */
function buildTable(node: Extract<A11yNode, { kind: 'TABLE' }>): HTMLTableElement {
    const el = document.createElement('table');
    el.setAttribute('role', 'table');
    el.dataset.blockIndex = String(node.block_index);
    const tbody = document.createElement('tbody');
    for (const row of node.rows) {
        const tr = document.createElement('tr');
        tr.setAttribute('role', 'row');
        for (const cell of row.cells) tr.appendChild(buildCell(cell));
        tbody.appendChild(tr);
    }
    el.appendChild(tbody);
    return el;
}

/** Issue #73 — one referenced header/footer part mirrored as a
 *  landmark container (`role="banner"` for headers, `"contentinfo"`
 *  for footers) so screen readers can reach band text. */
function buildStory(node: Extract<A11yNode, { kind: 'STORY' }>): HTMLElement {
    const el = document.createElement('div');
    el.setAttribute('role', node.header ? 'banner' : 'contentinfo');
    el.setAttribute(
        'aria-label',
        node.header ? 'Page header' : 'Page footer',
    );
    el.dataset.rid = node.rid;
    for (const child of node.nodes) el.appendChild(buildNode(child));
    return el;
}

/** Issue #165 — one text box story mirrored as a `role="group"` region,
 *  named from the box's `docPr` name (description → `aria-description`).
 *  `data-story-id` is the box address the engine also reports as
 *  `editing_story.rid`, so the active region can be marked. */
function buildTextBox(node: Extract<A11yNode, { kind: 'TEXT_BOX' }>): HTMLElement {
    const el = document.createElement('div');
    el.setAttribute('role', 'group');
    el.setAttribute('aria-label', node.name ?? 'Text box');
    if (node.description !== undefined) el.setAttribute('aria-description', node.description);
    el.dataset.storyId = node.id;
    for (const child of node.nodes) el.appendChild(buildNode(child));
    return el;
}

/** Dispatch on `A11yNode.kind` — single entry point for builders + recursion. */
function buildNode(node: A11yNode): HTMLElement {
    if (node.kind === 'TABLE') return buildTable(node);
    if (node.kind === 'STORY') return buildStory(node);
    if (node.kind === 'TEXT_BOX') return buildTextBox(node);
    return buildParagraph(node);
}

/** Refresh top-level positional indices after a structural shift. */
function reindex(root: HTMLElement): void {
    for (let i = 0; i < root.children.length; i++) {
        const child = root.children[i];
        if (child instanceof HTMLElement) child.dataset.pid = String(i);
    }
}

/** Stamp `data-pid` on `node` for the DevTools inspector. */
function stampPid(node: HTMLElement, index: number): HTMLElement {
    node.dataset.pid = String(index);
    return node;
}

export interface A11yReconciler {
    /** Apply one delta's patches to the mirror, in order. */
    apply(patches: A11yPatch[]): void;
    /** Issue #165 — mark the text-box region whose `data-story-id` is `id`
     *  as the active story (`aria-current="true"`); `null` clears it.
     *  Survives later patches (a rebuilt region is re-marked). */
    setActiveStory(id: string | null): void;
}

/**
 * Create a reconciler bound to `root` — the `.a11y-mirror` container.
 */
export function createA11yReconciler(root: HTMLElement): A11yReconciler {
    let activeStory: string | null = null;

    const markActive = (): void => {
        for (const el of root.querySelectorAll<HTMLElement>('[aria-current]')) {
            if (el.dataset.storyId !== activeStory) el.removeAttribute('aria-current');
        }
        if (activeStory === null) return;
        for (const el of root.querySelectorAll<HTMLElement>('[data-story-id]')) {
            if (el.dataset.storyId === activeStory) el.setAttribute('aria-current', 'true');
        }
    };

    const setActiveStory = (id: string | null): void => {
        if (id === activeStory) return;
        activeStory = id;
        markActive();
    };

    const apply = (patches: A11yPatch[]): void => {
        let shifted = false;
        for (const patch of patches) {
            switch (patch.type) {
                case 'REPLACE': {
                    root.replaceChildren(
                        ...patch.tree.nodes.map((n, i) => stampPid(buildNode(n), i)),
                    );
                    break;
                }
                case 'UPDATE': {
                    const target = root.children[patch.index];
                    if (target) {
                        target.replaceWith(stampPid(buildNode(patch.node), patch.index));
                    }
                    break;
                }
                case 'INSERT': {
                    const before = root.children[patch.index] ?? null;
                    root.insertBefore(stampPid(buildNode(patch.node), patch.index), before);
                    shifted = true;
                    break;
                }
                case 'REMOVE': {
                    root.children[patch.index]?.remove();
                    shifted = true;
                    break;
                }
            }
        }
        if (shifted) reindex(root);
        if (activeStory !== null) markActive();
    };

    return { apply, setActiveStory };
}
