/* Issue #449 - "no control may be left enabled that only produces a refusal
 * toast". A static audit of the `@nge/ui` shelf: every facade member a
 * component calls (`cmd.<member>(`) whose commands the engine's protection
 * gate does not always admit (`protection` class other than `exempt`) must
 * be named in a `protectionGate(state, ...)` / `protectionRefusal(...)`
 * call of the same component - or the component is a dialog whose OPENER
 * names it. A new control that dispatches a gated command without asking
 * the one protection query fails here, not in a user's toast.
 *
 * Source scan on purpose (the vitest harness has no DOM): the shelf's JSX
 * is the thing under audit. */
import { readdirSync, readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';
import { commandMeta } from '@nge/core/command-meta';
/* The facade map is types-only at runtime (no Solid), so it loads under the
   node-environment harness; `@nge/core`'s index would pull the JSX in. */
import { FACADE_METHOD_COMMANDS } from '../../packages/core/src/facadeMap';

const UI_DIR = fileURLToPath(new URL('../../packages/ui/src/', import.meta.url));

const read = (file: string): string => readFileSync(`${UI_DIR}${file}`, 'utf8');

/** Components whose controls are reached only through an OPENER that is
 *  itself gated: the dialog is the opener's, so the opener must name every
 *  member the dialog calls. */
const OPENED_BY: Record<string, string> = {
    'CellPropertiesDialog.tsx': 'TableContextMenu.tsx',
    'FontDialog.tsx': 'FontPickers.tsx',
    'PageSetupDialog.tsx': 'LayoutControls.tsx',
    'ParagraphBordersDialog.tsx': 'LayoutControls.tsx',
    'StyleEditDialog.tsx': 'StylesDropdown.tsx',
};

/** The facade members a component calls through `cmd.<member>(`. */
function calledMembers(source: string): string[] {
    const found = new Set<string>();
    for (const m of source.matchAll(/\bcmd\.([A-Za-z]+)\(/g)) {
        if (m[1] !== undefined) found.add(m[1]);
    }
    return [...found].sort();
}

/** Does `source` name `member` as an argument of a protection query? */
function asksAbout(source: string, member: string): boolean {
    for (const call of source.matchAll(/protection(?:Gate|Refusal)\(([^)]*)\)/g)) {
        if (new RegExp(`['"]${member}['"]`).test(call[1] ?? '')) return true;
    }
    return false;
}

/** A member is gated when ANY of its commands is not always admitted. */
function isGated(member: string): boolean {
    const target = (FACADE_METHOD_COMMANDS as Record<string, unknown>)[member];
    if (!Array.isArray(target)) return false;
    return target.some((type: unknown) => commandMeta(String(type)).protection !== 'exempt');
}

const files = readdirSync(UI_DIR).filter((f) => f.endsWith('.tsx'));

describe('the @nge/ui shelf asks the protection query for every gated command (#449)', () => {
    it('finds the shelf', () => {
        expect(files.length).toBeGreaterThan(30);
        expect(files).toContain('TextFormatButtons.tsx');
    });

    it('every cmd.<member>() a component calls is exempt, or named in its protection query', () => {
        const missing: string[] = [];
        for (const file of files) {
            const source = read(file);
            const owner = OPENED_BY[file];
            const asker = owner === undefined ? source : read(owner);
            for (const member of calledMembers(source)) {
                if (!(member in FACADE_METHOD_COMMANDS)) continue;
                if (!isGated(member)) continue;
                if (!asksAbout(asker, member)) {
                    missing.push(
                        `${file}: cmd.${member}() is refused by some protection modes but ` +
                            `${owner ?? file} never passes '${member}' to protectionGate / protectionRefusal`,
                    );
                }
            }
        }
        expect(missing).toEqual([]);
    });

    it('knows which members are gated (the audit is not vacuous)', () => {
        for (const member of [
            'toggleFormatting',
            'setFontSize',
            'insertTableAtCaret',
            'insertImageAtCaret',
            'insertComment',
            'setParagraphAlign',
        ]) {
            expect(isGated(member), member).toBe(true);
        }
        for (const member of ['undo', 'setZoom', 'saveDocument', 'setSelection', 'exitHeaderFooter']) {
            expect(isGated(member), member).toBe(false);
        }
        /* The scan sees the controls the issue names. */
        expect(calledMembers(read('TextFormatButtons.tsx'))).toContain('toggleFormatting');
        expect(calledMembers(read('FontPickers.tsx'))).toContain('setFontSize');
        expect(calledMembers(read('InsertTableButton.tsx'))).toContain('insertTableAtCaret');
        expect(calledMembers(read('InsertImageButton.tsx'))).toContain('insertImageAtCaret');
    });

    it('every dialog opener is itself a gated component', () => {
        for (const [dialog, opener] of Object.entries(OPENED_BY)) {
            expect(files, dialog).toContain(dialog);
            expect(read(opener), `${opener} opens ${dialog}`).toMatch(/protectionGate\(/);
        }
    });
});
