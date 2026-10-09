/* Issue #388 - "does the document in the engine equal what the user last
 * saved (or opened / seeded)?"
 *
 * ONE pure transition, shared by the worker (which persists the answer in
 * the event log's `meta` store, so a later boot can offer the unsaved
 * session back) and the main-thread `EngineClient` (which answers the
 * synchronous `beforeunload` question). Both fold the same
 * (command, reply) stream through it, so they cannot drift.
 *
 * - a command that starts a new document (`new_document`: the boot seed,
 *   an opened / loaded file, a closed document) leaves the engine equal
 *   to its source: clean;
 * - any other command that may change the document model (`mutates_doc`,
 *   from `bridge::meta`) and was accepted makes it dirty;
 * - a `SAVE_DOCX` that answered `DOCUMENT_SAVED` makes it clean again;
 * - selection / view / read-back commands and refused commands (a reply
 *   of `ERROR`) change nothing. */
import { commandMeta } from '@nge/core/command-meta';
import type { Command, Event } from './types';

export function nextCleanState(clean: boolean, cmd: Command, evt: Event): boolean {
    if (evt.type === 'ERROR') return clean;
    if (cmd.type === 'SAVE_DOCX') return evt.type === 'DOCUMENT_SAVED' ? true : clean;
    const meta = commandMeta(cmd.type);
    if (meta.new_document) return true;
    if (meta.mutates_doc) return false;
    return clean;
}
