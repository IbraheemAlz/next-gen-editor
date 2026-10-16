/**
 * Issue #491 - the File menu's banner text for a refused open, as a pure
 * function (no DOM, no Solid) so a unit test can pin the copy.
 *
 * These kinds are `'own'` in `ERROR_KIND_PRESENTATION` (ErrorToast.tsx): the
 * File menu's `.nge-fm__error` banner shows them, never the toast.
 */
import type { ErrorKind } from '@nge/core';

/** `OpenDocument: word/document.xml: …` -> `word/document.xml: …`. The
 *  engine prefixes every refusal with the command name. */
export function stripCommandPrefix(message: string): string {
    return message.replace(/^[A-Za-z][A-Za-z0-9]*: /, '');
}

/** The banner text for an engine `ERROR`: typed refusals (issues #348 /
 *  #345 / #491) get a plain-language lead, everything else shows the
 *  engine's own message.
 *
 *  `DocumentUnreadable` (#491): the engine's message already names the part
 *  and the offset and carries no document bytes (`engine-wasm`
 *  `open_error::scrub_detail`), so the banner shows it after the lead. */
export function openErrorMessage(kind: ErrorKind | undefined, message: string): string {
    switch (kind) {
        case 'PackageTooLarge':
            return `This document is too large or too deeply nested to open safely. ${message}`;
        case 'EncryptedDocument':
            return (
                'This document is encrypted with a password. Open it from the File menu to ' +
                `enter the password. ${message}`
            );
        case 'WrongPassword':
            return `The password does not open this document. ${message}`;
        case 'DocumentUnreadable':
            return `This document is damaged and cannot be opened: ${stripCommandPrefix(message)}`;
        default:
            return message;
    }
}
