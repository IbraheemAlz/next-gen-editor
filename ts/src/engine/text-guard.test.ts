/* Issue #467 - the shell refuses a lone UTF-16 surrogate before the engine
 * (whose string glue would silently turn it into U+FFFD) sees it. */
import { describe, expect, it } from 'vitest';
import type { Command } from '../../../crates/engine-wasm/pkg/engine_wasm.js';
import { findIllFormedText, loneSurrogateIndex, refuseIllFormedText } from './text-guard';

const cmd = (c: Record<string, unknown>): Command => c as unknown as Command;

describe('loneSurrogateIndex', () => {
    it('finds a lone high surrogate, a lone low surrogate, a split pair', () => {
        expect(loneSurrogateIndex('a\uD800b')).toBe(1);
        expect(loneSurrogateIndex('ab\uDC00')).toBe(2);
        expect(loneSurrogateIndex('\uD83D')).toBe(0);
        expect(loneSurrogateIndex('\uDE00\uD83D')).toBe(0);
    });

    it('accepts a well-formed pair and plain text', () => {
        expect(loneSurrogateIndex('a\u{1F600}b')).toBe(-1);
        expect(loneSurrogateIndex('😀')).toBe(-1);
        expect(loneSurrogateIndex('مرحبا')).toBe(-1);
        expect(loneSurrogateIndex('')).toBe(-1);
    });
});

describe('refuseIllFormedText (#467)', () => {
    it('refuses a lone surrogate in typed or pasted text with a typed InvalidArgument', () => {
        const evt = refuseIllFormedText(cmd({ type: 'INSERT_TEXT', text: 'a\uD800b' }));
        expect(evt).toMatchObject({
            type: 'ERROR',
            kind: 'InvalidArgument',
            command: 'INSERT_TEXT',
        });
        const message = (evt as { message: string }).message;
        expect(message.startsWith('InsertText: text: ')).toBe(true);
        expect(message).toContain('index 1');
        expect(refuseIllFormedText(cmd({ type: 'PASTE_PLAIN', text: '\uDC00' }))).toMatchObject({
            command: 'PASTE_PLAIN',
        });
        expect(refuseIllFormedText(cmd({ type: 'PASTE_HTML', html: '<p>\uD83D</p>' }))).toMatchObject({
            kind: 'InvalidArgument',
        });
    });

    it('walks nested objects and arrays and names the field', () => {
        expect(
            findIllFormedText(
                cmd({ type: 'APPLY_FORMATTING', attrs: { font_family: 'Ar\uD800ial' } }),
            ),
        ).toEqual({ field: 'attrs.font_family', index: 2 });
        expect(
            findIllFormedText(cmd({ type: 'X', stops: [{ ok: 'fine' }, { name: '\uDFFF' }] })),
        ).toEqual({ field: 'stops[1].name', index: 0 });
    });

    it('lets well-formed text through untouched, emoji included', () => {
        expect(refuseIllFormedText(cmd({ type: 'INSERT_TEXT', text: 'hi \u{1F600}' }))).toBeUndefined();
        expect(refuseIllFormedText(cmd({ type: 'INSERT_TEXT', text: 'a\u0001b\u000b\u000c' }))).toBeUndefined();
        expect(refuseIllFormedText(cmd({ type: 'UNDO' }))).toBeUndefined();
    });

    it('does not read byte payloads or a RECOVER tail', () => {
        const bytes = new Uint8Array([0xed, 0xa0, 0x80]);
        expect(
            refuseIllFormedText(cmd({ type: 'OPEN_DOCUMENT', bytes, format: 'Docx', name: 'a.docx' })),
        ).toBeUndefined();
        expect(
            refuseIllFormedText(cmd({ type: 'RECOVER', log_tail: [{ type: 'INSERT_TEXT', text: '\uD800' }] })),
        ).toBeUndefined();
    });

    it('survives a cyclic structure', () => {
        const loop: Record<string, unknown> = { type: 'X' };
        loop.self = loop;
        expect(refuseIllFormedText(cmd(loop))).toBeUndefined();
    });
});
