import { describe, expect, it } from 'vitest';
import {
    ERROR_KIND_PRESENTATION,
    ERROR_KINDS_WITH_OWN_PRESENTATION,
    ERROR_TOAST_COPY,
    toastMessageFor,
} from './ErrorToast';
import { openErrorMessage, stripCommandPrefix } from './openErrorCopy';

/* Issue #491 - a damaged package is refused with the typed
 * `DocumentUnreadable`; the File menu's open banner shows it (the same
 * surface as `PackageTooLarge`), never the toast. */

describe('openErrorMessage', () => {
    it('leads a DocumentUnreadable refusal with the damaged-document sentence and keeps part + offset', () => {
        const text = openErrorMessage(
            'DocumentUnreadable',
            'OpenDocument: word/document.xml: a second root element at byte 1234 (no faithful repair)',
        );
        expect(text).toBe(
            'This document is damaged and cannot be opened: word/document.xml: a second root element at byte 1234 (no faithful repair)',
        );
    });

    it('keeps the pre-#491 copy for the other typed open refusals', () => {
        expect(openErrorMessage('PackageTooLarge', 'm')).toBe(
            'This document is too large or too deeply nested to open safely. m',
        );
        expect(openErrorMessage('WrongPassword', 'm')).toBe(
            'The password does not open this document. m',
        );
        expect(openErrorMessage('EncryptedDocument', 'm')).toContain('encrypted with a password');
        /* An untyped or legacy-format refusal shows the engine's own message. */
        expect(openErrorMessage(undefined, 'OpenDocument: x')).toBe('OpenDocument: x');
        expect(openErrorMessage('InvalidDocument', 'OpenDocument: not a .docx')).toBe(
            'OpenDocument: not a .docx',
        );
    });

    it('strips only a leading command prefix', () => {
        expect(stripCommandPrefix('LoadDocx: word/document.xml: x')).toBe('word/document.xml: x');
        expect(stripCommandPrefix('word/document.xml: x')).toBe('word/document.xml: x');
    });
});

describe('DocumentUnreadable presentation', () => {
    it('is the File menu\'s own presentation, not a toast', () => {
        expect(ERROR_KIND_PRESENTATION.DocumentUnreadable).toBe('own');
        expect(ERROR_KINDS_WITH_OWN_PRESENTATION).toContain('DocumentUnreadable');
        /* tools/parity: a kind never has toast copy AND an own presentation. */
        expect(ERROR_TOAST_COPY.DocumentUnreadable).toBeUndefined();
        expect(
            toastMessageFor({
                kind: 'DocumentUnreadable',
                command: 'OPEN_DOCUMENT',
                message: 'OpenDocument: word/document.xml: x',
                count: 1,
                at: 0,
            }),
        ).toBeUndefined();
    });
});
