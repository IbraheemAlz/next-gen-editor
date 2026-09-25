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
 * `aria-current="true"` (`setActiveStory`, fed by `editing_story`).
 *
 * Issue #203: footnote / endnote stories are `NOTE` nodes. A footnote is an
 * `<aside role="doc-footnote">` right after the paragraph of its first
 * reference; endnotes are the contiguous TAIL of the node list, and the
 * reconciler wraps that tail in ONE `<section role="doc-endnotes">` (an
 * `<ol>` of `<li>` notes — DPUB-ARIA 1.1's replacement for the deprecated
 * `doc-endnote` role). A reference mark is an `<a role="doc-noteref">`
 * linking (`href`) to its note region's DOM id. Because the endnote section
 * nests nodes one level below the mirror root, patch indices address a
 * LOGICAL slot list (`slots`), not `root.children` directly.
 *
 * Issue #215: an inline image / text box's `U+FFFC` placeholder is an
 * `A11yRun` carrying `object` instead of raw placeholder text (`text` is
 * empty). An image run becomes `<img role="img" alt="…">` (alt from the
 * picture's `descr`/`name`); a text box run becomes a plain `<a>` linking
 * to its `TEXT_BOX` region's DOM id (`textBoxDomId`), the same way a note
 * reference links to its note region.
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
import type {
    A11yCell,
    A11yNode,
    A11yNoteKind,
    A11yObjectRef,
    A11yParagraph,
    A11yPatch,
    A11yRun,
} from '../engine/types';

/** Inline CSS for a run, so a screen reader can announce its formatting. */
function runStyle(r: A11yRun): string {
    const parts: string[] = [];
    if (r.bold) parts.push('font-weight:bold');
    if (r.italic) parts.push('font-style:italic');
    if (r.underline) parts.push('text-decoration:underline');
    return parts.join(';');
}

/** Issue #203 — the DOM id of a note region (`A11yNote.id` is the engine's
 *  stable `footnote-<w:id>` / `endnote-<w:id>`). */
export function noteDomId(id: string): string {
    return `nge-a11y-${id}`;
}

/** Issue #215 — the DOM id of a text-box region (`A11yTextBox.id` is the
 *  engine's stable `<path>@<at>` story address). */
export function textBoxDomId(id: string): string {
    return `nge-a11y-tb-${id}`;
}

/** Issue #203 — "Footnote 3" / "Endnote iv" (bare kind for a custom mark). */
function noteLabel(kind: A11yNoteKind, marker: string): string {
    const word = kind === 'Endnote' ? 'Endnote' : 'Footnote';
    return marker ? `${word} ${marker}` : word;
}

/** Issue #215 — build the element for an inline object run: an
 *  `<img role="img">` for a picture (alt from the engine's `descr`/`name`,
 *  falling back to a generic label so an unlabeled picture is still
 *  discoverable rather than treated as decorative), or a plain `<a>`
 *  referencing the `TEXT_BOX` region for a text box — the object IS the
 *  run's content, so `run.text` is empty and never rendered here. */
function buildObjectRun(object: A11yObjectRef): HTMLElement {
    if (object.kind === 'IMAGE') {
        const img = document.createElement('img');
        img.setAttribute('role', 'img');
        img.alt = object.alt ?? 'Image';
        return img;
    }
    const a = document.createElement('a');
    a.href = `#${textBoxDomId(object.id)}`;
    a.tabIndex = -1;
    a.setAttribute('aria-label', object.alt ? `Text box: ${object.alt}` : 'Text box');
    a.dataset.objectRef = object.id;
    return a;
}

/** Build the element for one run — a `<span>`, (issue #203) an
 *  `<a role="doc-noteref">` for a footnote / endnote reference mark, or
 *  (issue #215) an inline image / text box object. Links are kept out of
 *  the tab order: the hidden textarea owns focus. */
function buildRun(run: A11yRun): HTMLElement {
    let el: HTMLElement;
    if (run.object !== undefined) {
        el = buildObjectRun(run.object);
    } else if (run.note_ref !== undefined) {
        const a = document.createElement('a');
        a.setAttribute('role', 'doc-noteref');
        a.href = `#${noteDomId(run.note_ref.id)}`;
        a.tabIndex = -1;
        a.setAttribute('aria-label', noteLabel(run.note_ref.kind, run.text));
        a.dataset.noteRef = run.note_ref.id;
        el = a;
    } else {
        el = document.createElement('span');
    }
    const style = runStyle(run);
    if (style) el.style.cssText = style;
    el.textContent = run.text;
    return el;
}

/** Build the `<p>` element for one accessibility paragraph.
 *
 *  Issue #195 — `dir` is the paragraph's OWN resolved base direction
 *  (explicit bidi → first-strong → document base, the engine's layout
 *  resolution), never the document-wide `direction`: an RTL paragraph in
 *  an LTR document must run UAX #9 with an RTL base for the screen reader.
 *  Every `<p>` sets it, so container regions (text box, header / footer,
 *  note) carry no `dir` and nothing is inherited. */
function buildParagraph(p: A11yParagraph): HTMLParagraphElement {
    const el = document.createElement('p');
    el.dir = p.resolved_direction === 'Rtl' ? 'rtl' : 'ltr';
    for (const run of p.runs) el.appendChild(buildRun(run));
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
 *  `editing_story.rid`, so the active region can be marked. Issue #215 —
 *  the element's real DOM `id` (`textBoxDomId`) is the link target of
 *  its anchor paragraph's `object` run. */
function buildTextBox(node: Extract<A11yNode, { kind: 'TEXT_BOX' }>): HTMLElement {
    const el = document.createElement('div');
    el.setAttribute('role', 'group');
    el.setAttribute('aria-label', node.name ?? 'Text box');
    if (node.description !== undefined) el.setAttribute('aria-description', node.description);
    el.id = textBoxDomId(node.id);
    el.dataset.storyId = node.id;
    for (const child of node.nodes) el.appendChild(buildNode(child));
    return el;
}

/** Issue #203 — one note story: a footnote is an `<aside
 *  role="doc-footnote">`; an endnote an `<li>` the reconciler files into
 *  the `doc-endnotes` section's list. `id` is the reference marks' link
 *  target; `data-story-id` is what `setActiveStory` matches. */
function buildNote(node: Extract<A11yNode, { kind: 'NOTE' }>): HTMLElement {
    const endnote = node.note_kind === 'Endnote';
    const el = document.createElement(endnote ? 'li' : 'aside');
    if (!endnote) el.setAttribute('role', 'doc-footnote');
    el.id = noteDomId(node.id);
    el.setAttribute('aria-label', noteLabel(node.note_kind, node.marker));
    el.dataset.storyId = node.id;
    el.dataset.noteKind = endnote ? 'endnote' : 'footnote';
    for (const child of node.nodes) el.appendChild(buildNode(child));
    return el;
}

/** Dispatch on `A11yNode.kind` — single entry point for builders + recursion. */
function buildNode(node: A11yNode): HTMLElement {
    if (node.kind === 'TABLE') return buildTable(node);
    if (node.kind === 'STORY') return buildStory(node);
    if (node.kind === 'TEXT_BOX') return buildTextBox(node);
    if (node.kind === 'NOTE') return buildNote(node);
    return buildParagraph(node);
}

/** Stamp `data-pid` (the logical node index) for the DevTools inspector. */
function stampPid(node: HTMLElement, index: number): HTMLElement {
    node.dataset.pid = String(index);
    return node;
}

/** Issue #203 — a top-level endnote lives in the `doc-endnotes` section. */
function isEndnote(el: HTMLElement): boolean {
    return el.dataset.noteKind === 'endnote';
}

export interface A11yReconciler {
    /** Apply one delta's patches to the mirror, in order. */
    apply(patches: A11yPatch[]): void;
    /** Issue #165 / #203 — mark the region whose `data-story-id` is `id`
     *  (a text box address, or a note's `footnote-N` / `endnote-N`) as
     *  the active story (`aria-current="true"`); `null` clears it.
     *  Survives later patches (a rebuilt region is re-marked). */
    setActiveStory(id: string | null): void;
}

/**
 * Create a reconciler bound to `root` — the `.a11y-mirror` container.
 */
export function createA11yReconciler(root: HTMLElement): A11yReconciler {
    let activeStory: string | null = null;
    /* The top-level node elements in engine order — what patch indices
       address. Every slot is a child of `root`, except endnotes, which
       are children of the section's list. */
    const slots: HTMLElement[] = [];
    let endnotes: { section: HTMLElement; list: HTMLOListElement } | null = null;

    const endnoteList = (): HTMLOListElement => {
        if (endnotes === null) {
            const section = document.createElement('section');
            section.setAttribute('role', 'doc-endnotes');
            section.setAttribute('aria-label', 'Endnotes');
            const list = document.createElement('ol');
            section.appendChild(list);
            root.appendChild(section);
            endnotes = { section, list };
        }
        return endnotes.list;
    };

    const dropEmptySection = (): void => {
        if (endnotes !== null && endnotes.list.childElementCount === 0) {
            endnotes.section.remove();
            endnotes = null;
        }
    };

    /** Put `slots[i]` into its container, before the next slot that
     *  shares that container (the section always stays last in `root`). */
    const place = (i: number): void => {
        const el = slots[i];
        if (!el) return;
        const container: HTMLElement = isEndnote(el) ? endnoteList() : root;
        for (let j = i + 1; j < slots.length; j++) {
            const next = slots[j];
            if (next && next.parentElement === container) {
                container.insertBefore(el, next);
                return;
            }
        }
        const section = endnotes?.section ?? null;
        if (container === root && section !== null) root.insertBefore(el, section);
        else container.appendChild(el);
    };

    const reindex = (): void => {
        slots.forEach((el, i) => stampPid(el, i));
    };

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
                    root.replaceChildren();
                    endnotes = null;
                    slots.length = 0;
                    patch.tree.nodes.forEach((n, i) => {
                        slots.push(stampPid(buildNode(n), i));
                        place(i);
                    });
                    break;
                }
                case 'UPDATE': {
                    const old = slots[patch.index];
                    if (!old) break;
                    const el = stampPid(buildNode(patch.node), patch.index);
                    slots[patch.index] = el;
                    if (isEndnote(el) === isEndnote(old)) {
                        old.replaceWith(el);
                    } else {
                        old.remove();
                        place(patch.index);
                        dropEmptySection();
                    }
                    break;
                }
                case 'INSERT': {
                    const index = Math.min(patch.index, slots.length);
                    slots.splice(index, 0, stampPid(buildNode(patch.node), index));
                    place(index);
                    shifted = true;
                    break;
                }
                case 'REMOVE': {
                    const [gone] = slots.splice(patch.index, 1);
                    gone?.remove();
                    dropEmptySection();
                    shifted = true;
                    break;
                }
            }
        }
        if (shifted) reindex();
        if (activeStory !== null) markActive();
    };

    return { apply, setActiveStory };
}
