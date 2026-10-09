/* Issue #345 - what of a command may reach the durable event log.
 *
 * `OPEN_DOCUMENT.password` (the key of an encrypted `.docx`) must never be
 * persisted: the worker journals every logged command to IndexedDB, and a
 * plaintext password there would outlive the session. A replay of the
 * stripped command answers `EncryptedDocument`; recovery restores the
 * document from the snapshot pinned right after the open instead.
 *
 * A pure module (no wasm, no DOM) so the worker imports it and vitest
 * covers it. */
import type { Command } from '../../../crates/engine-wasm/pkg/engine_wasm.js';

/** `cmd` as it may be persisted: secrets stripped, everything else (the
 *  same object when nothing needed stripping) untouched. */
export function journalSafe(cmd: Command): Command {
    if (cmd.type === 'OPEN_DOCUMENT' && cmd.password !== undefined) {
        const { password: _password, ...rest } = cmd;
        return rest;
    }
    return cmd;
}
