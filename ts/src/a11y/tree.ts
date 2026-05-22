/* Phase 4 §10 / Backlog #10 — accessibility mirror reconciler.
 *
 * Applies the engine's incremental `A11yPatch` stream to the visually-hidden
 * mirror DOM. Before deltas, every keystroke rebuilt the whole `<p>` list, so
 * the browser recomputed the accessibility tree for the entire document. Now a
 * keystroke patches exactly one `<p>`, and the browser only re-derives a11y
 * for that node.
 *
 * The reconciler owns `root`'s children outright — Solid renders `root` as a
 * bare `ref` host and never touches what is inside it. Patches are positional:
 * the engine's prefix/suffix diff emits Updates first, then either Inserts
 * (ascending index) or Removes, so applying them in array order is correct. */
import type { A11yParagraph, A11yPatch, A11yRun } from '../engine/types';

/** Inline CSS for a run, so a screen reader can announce its formatting. */
function runStyle(r: A11yRun): string {
    const parts: string[] = [];
    if (r.bold) parts.push('font-weight:bold');
    if (r.italic) parts.push('font-style:italic');
    if (r.underline) parts.push('text-decoration:underline');
    return parts.join(';');
}

/** Build the `<p>` element for one accessibility paragraph. */
function buildParagraph(p: A11yParagraph, index: number): HTMLParagraphElement {
    const el = document.createElement('p');
    el.dir = p.direction === 'Rtl' ? 'rtl' : 'ltr';
    /* Positional, not a stable id — a debugging aid for the DevTools a11y
       inspector; the reconciler itself addresses paragraphs by child index. */
    el.dataset.pid = String(index);
    for (const run of p.runs) {
        const span = document.createElement('span');
        const style = runStyle(run);
        if (style) span.style.cssText = style;
        span.textContent = run.text;
        el.appendChild(span);
    }
    return el;
}

export interface A11yReconciler {
    /** Apply one delta's patches to the mirror, in order. */
    apply(patches: A11yPatch[]): void;
}

/**
 * Create a reconciler bound to `root` — the `.a11y-mirror` container.
 */
export function createA11yReconciler(root: HTMLElement): A11yReconciler {
    /* `data-pid` is positional; refresh it after a structural shift so the
       inspector keeps reading true indices. */
    const reindex = (): void => {
        for (let i = 0; i < root.children.length; i++) {
            const child = root.children[i];
            if (child instanceof HTMLElement) child.dataset.pid = String(i);
        }
    };

    const apply = (patches: A11yPatch[]): void => {
        let shifted = false;
        for (const patch of patches) {
            switch (patch.type) {
                case 'REPLACE': {
                    root.replaceChildren(
                        ...patch.tree.paragraphs.map((p, i) => buildParagraph(p, i)),
                    );
                    break;
                }
                case 'UPDATE': {
                    const target = root.children[patch.index];
                    if (target) {
                        target.replaceWith(buildParagraph(patch.paragraph, patch.index));
                    }
                    break;
                }
                case 'INSERT': {
                    const before = root.children[patch.index] ?? null;
                    root.insertBefore(buildParagraph(patch.paragraph, patch.index), before);
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
        if (shifted) reindex();
    };

    return { apply };
}
