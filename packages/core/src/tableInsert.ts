/**
 * Issue #563 — where Insert Table puts a table relative to the caret.
 *
 * Pure (no DOM, unit-tested). The table goes right after the paragraph
 * the caret is in, IN THAT PARAGRAPH'S OWN CONTAINER: the body (or the
 * active story) for a top-level paragraph, the table cell for a cell
 * paragraph — at any depth, so a caret in a cell nests the new table in
 * that cell. The engine resolves the position through
 * `DocumentTree::resolve_block_insert` and keeps a paragraph after the
 * table in its container (OOXML: a cell, like the body, ends with a
 * paragraph). The pre-#563 facade took the caret's FIRST block step, so a
 * caret in a cell inserted a top-level table after the outer one.
 */
import type { BlockPath } from './types';

/** The `InsertTable.at` for a caret at `caret`: the caret's path with its
 *  last `BLOCK` step advanced by one. A path that does not end in a
 *  `BLOCK` step (none should reach here) appends to the body. */
export function tableInsertPosition(caret: BlockPath): BlockPath {
    const steps = caret.steps;
    const last = steps[steps.length - 1];
    if (!last || last.kind !== 'BLOCK') {
        return { steps: [] };
    }
    return { steps: [...steps.slice(0, -1), { kind: 'BLOCK', idx: last.idx + 1 }] };
}
