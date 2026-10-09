/* Issue #345 - the event log never sees an encrypted document's password. */
import { describe, expect, it } from 'vitest';
import type { Command } from '../../../crates/engine-wasm/pkg/engine_wasm.js';
import { journalSafe } from './journal-safe';

describe('journalSafe (#345)', () => {
    it('strips OPEN_DOCUMENT.password and keeps every other field', () => {
        const bytes = new Uint8Array([1, 2, 3]);
        const open = {
            type: 'OPEN_DOCUMENT',
            bytes,
            format: 'docx',
            name: 'locked.docx',
            password: 'pass',
        } as unknown as Command;
        const safe = journalSafe(open) as Record<string, unknown>;
        expect('password' in safe).toBe(false);
        expect(safe.type).toBe('OPEN_DOCUMENT');
        expect(safe.bytes).toBe(bytes);
        expect(safe.name).toBe('locked.docx');
        expect(safe.format).toBe('docx');
        /* The caller's command is not mutated. */
        expect((open as unknown as { password: string }).password).toBe('pass');
    });

    it('returns a command without secrets unchanged (same object)', () => {
        const plain = {
            type: 'OPEN_DOCUMENT',
            bytes: new Uint8Array(),
            format: 'docx',
        } as unknown as Command;
        expect(journalSafe(plain)).toBe(plain);
        const typing = { type: 'INSERT_TEXT', text: 'pass' } as unknown as Command;
        expect(journalSafe(typing)).toBe(typing);
    });
});
