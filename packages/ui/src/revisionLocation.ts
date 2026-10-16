import type { RevisionSnapshot } from '@nge/core';

/** Issue #440 — where a review-pane row's change sits: the top-level
 *  block, the table nesting depth when it is inside a table cell (or is a
 *  row change), then the row or the byte range. */
export function locationLabel(rev: RevisionSnapshot): string {
    const parts = [`block ${rev.block}`];
    const depth = rev.depth ?? 0;
    if (depth > 0) {
        parts.push(depth === 1 ? 'table' : `table depth ${depth}`);
    }
    parts.push(rev.row !== undefined ? `row ${rev.row + 1}` : `${rev.start}–${rev.end}`);
    return parts.join(' · ');
}
