/* Issue #449 - which shelf controls an enforced document protection would
 * refuse. The pure half pins `commandRefusal` / `methodRefusal` against the
 * engine's `protection_gate` classes (the generated `PROTECTION_ADMITS`
 * table); the reactive half drives `createEditorState().protectionRefusal`
 * through `SELECTION_CHANGED` against a scripted engine handle (`useEngine`
 * mocked: `EngineProvider.tsx` is JSX the node-side vitest does not
 * compile). */
import { createRoot } from 'solid-js';
import { describe, expect, it, vi } from 'vitest';
import { COMMAND_META, PROTECTION_ADMITS, protectionAdmits } from './commandMeta.generated';
import type { EditorCommands } from './createEditorCommands';
import { FACADE_METHOD_COMMANDS } from './facadeMap';
import {
    PROTECTION_MODE_PHRASE,
    commandRefusal,
    methodRefusal,
    type ProtectionContext,
} from './protection';
import type { Event, ProtectionMode } from './types';

const MODES: readonly ProtectionMode[] = ['readOnly', 'comments', 'trackedChanges', 'forms'];

const open: ProtectionContext = { mode: undefined, inStory: false };
const mode = (m: ProtectionMode, inStory = false): ProtectionContext => ({ mode: m, inStory });

/** The facade members the shelf controls call, by what they do. */
const FORMATTING: (keyof EditorCommands)[] = [
    'toggleFormatting',
    'applyAttrs',
    'setFontSize',
    'setFontFamily',
    'setColor',
    'setHighlight',
    'setUnderline',
];
const STRUCTURAL: (keyof EditorCommands)[] = [
    'insertTableAtCaret',
    'insertImageAtCaret',
    'insertPageBreak',
    'toggleList',
    'setParagraphAlign',
    'setLineSpacing',
    'applyStyle',
    'insertFootnote',
];
const COMMENTS: (keyof EditorCommands)[] = [
    'insertComment',
    'deleteComment',
    'resolveComment',
    'replyToComment',
];
/** Dispatch only exempt commands (or none): never locked by protection. */
const ALWAYS_OPEN: (keyof EditorCommands)[] = [
    'undo',
    'redo',
    'setZoom',
    'setSelection',
    'selectAll',
    'moveCaret',
    'saveDocument',
    'saveDocx',
    'exportPdf',
    'openDocument',
    'closeDocument',
    'enterHeaderFooter',
    'exitHeaderFooter',
    'setReviewIdentity',
    'setFieldCodeView',
    'requestStats',
    'getSelectionAsClipboard',
    'setRenderer',
    'raw',
];

describe('commandRefusal (#449)', () => {
    it('never refuses anything in an unprotected document', () => {
        for (const type of Object.keys(COMMAND_META)) {
            expect(commandRefusal(open, type)).toBeUndefined();
        }
    });

    it('refuses exactly the commands the generated PROTECTION_ADMITS table does not admit', () => {
        for (const m of MODES) {
            for (const [type, meta] of Object.entries(COMMAND_META)) {
                const admitted = PROTECTION_ADMITS[m]?.includes(meta.protection) ?? false;
                expect(protectionAdmits(m, type)).toBe(admitted);
                const refusal = commandRefusal(mode(m), type);
                expect(refusal === undefined, `${m} / ${type}`).toBe(admitted);
                if (refusal !== undefined) expect(refusal.kind).toBe('mode');
            }
        }
    });

    it('never refuses an exempt command, in any mode, inside or outside a story', () => {
        for (const m of MODES) {
            for (const [type, meta] of Object.entries(COMMAND_META)) {
                if (meta.protection !== 'exempt') continue;
                expect(commandRefusal(mode(m), type), `${m} / ${type}`).toBeUndefined();
                expect(commandRefusal(mode(m, true), type), `${m} / ${type}`).toBeUndefined();
            }
        }
    });

    it('read-only refuses formatting, structure and comments alike', () => {
        for (const type of ['APPLY_FORMATTING', 'TOGGLE_FORMATTING', 'INSERT_TABLE', 'INSERT_IMAGE', 'INSERT_COMMENT']) {
            const refusal = commandRefusal(mode('readOnly'), type);
            expect(refusal?.kind, type).toBe('mode');
            expect(refusal?.mode).toBe('readOnly');
            expect(refusal?.command).toBe(type);
            expect(refusal?.title).toBe('Not available: this document is protected (read-only)');
        }
    });

    it('comments-only admits comments and nothing that changes the document', () => {
        expect(commandRefusal(mode('comments'), 'INSERT_COMMENT')).toBeUndefined();
        expect(commandRefusal(mode('comments'), 'REPLY_TO_COMMENT')).toBeUndefined();
        expect(commandRefusal(mode('comments'), 'TOGGLE_FORMATTING')?.kind).toBe('mode');
        expect(commandRefusal(mode('comments'), 'INSERT_TABLE')?.kind).toBe('mode');
        expect(commandRefusal(mode('comments'), 'INSERT_TEXT')?.kind).toBe('mode');
    });

    it('tracked-changes admits formatting, text and comments but not structure', () => {
        expect(commandRefusal(mode('trackedChanges'), 'TOGGLE_FORMATTING')).toBeUndefined();
        expect(commandRefusal(mode('trackedChanges'), 'INSERT_TEXT')).toBeUndefined();
        expect(commandRefusal(mode('trackedChanges'), 'INSERT_COMMENT')).toBeUndefined();
        expect(commandRefusal(mode('trackedChanges'), 'INSERT_TABLE')?.kind).toBe('mode');
        expect(commandRefusal(mode('trackedChanges'), 'ACCEPT_ALL_REVISIONS')?.kind).toBe('mode');
    });

    it('forms admits the text class (the engine narrows it to form content) and nothing else', () => {
        expect(commandRefusal(mode('forms'), 'INSERT_TEXT')).toBeUndefined();
        expect(commandRefusal(mode('forms'), 'DELETE_AT_CARET')).toBeUndefined();
        expect(commandRefusal(mode('forms'), 'TOGGLE_FORMATTING')?.kind).toBe('mode');
        expect(commandRefusal(mode('forms'), 'INSERT_COMMENT')?.kind).toBe('mode');
        expect(commandRefusal(mode('forms'), 'INSERT_IMAGE')?.kind).toBe('mode');
    });

    it('a story under a mode that admits the class is refused as a story, naming the mode', () => {
        const refusal = commandRefusal(mode('trackedChanges', true), 'TOGGLE_FORMATTING');
        expect(refusal?.kind).toBe('story');
        expect(refusal?.title).toBe(
            'Not available: headers, footers, notes and text boxes cannot be edited in a protected document (tracked changes only)',
        );
        expect(commandRefusal(mode('forms', true), 'INSERT_TEXT')?.kind).toBe('story');
        expect(commandRefusal(mode('comments', true), 'INSERT_COMMENT')?.kind).toBe('story');
    });

    it('the mode check comes first: a story under read-only is a mode refusal (the gate order)', () => {
        expect(commandRefusal(mode('readOnly', true), 'TOGGLE_FORMATTING')?.kind).toBe('mode');
    });

    it('a story in an UNPROTECTED document refuses nothing', () => {
        expect(commandRefusal({ mode: undefined, inStory: true }, 'TOGGLE_FORMATTING')).toBeUndefined();
    });

    it('an unknown command type is refused like an unclassified one (the table default)', () => {
        expect(commandRefusal(mode('trackedChanges'), 'SOME_FUTURE_COMMAND')?.kind).toBe('mode');
        expect(commandRefusal(open, 'SOME_FUTURE_COMMAND')).toBeUndefined();
    });

    it('words every mode', () => {
        for (const m of MODES) {
            expect(commandRefusal(mode(m), 'INSERT_TABLE_UNKNOWN')?.title).toContain(
                `(${PROTECTION_MODE_PHRASE[m]})`,
            );
        }
    });
});

describe('methodRefusal (#449)', () => {
    it('locks the formatting, structure and comment controls under read-only', () => {
        for (const method of [...FORMATTING, ...STRUCTURAL, ...COMMENTS]) {
            expect(methodRefusal(mode('readOnly'), method)?.kind, method).toBe('mode');
        }
    });

    it('leaves the exempt facade members enabled under every mode', () => {
        for (const m of MODES) {
            for (const method of ALWAYS_OPEN) {
                expect(methodRefusal(mode(m), method), `${m} / ${method}`).toBeUndefined();
                expect(methodRefusal(mode(m, true), method), `${m} / ${method} in a story`).toBeUndefined();
            }
        }
    });

    it('comments-only leaves the comment controls enabled and locks the rest', () => {
        for (const method of COMMENTS) {
            expect(methodRefusal(mode('comments'), method), method).toBeUndefined();
        }
        for (const method of [...FORMATTING, ...STRUCTURAL]) {
            expect(methodRefusal(mode('comments'), method)?.kind, method).toBe('mode');
        }
    });

    it('tracked-changes leaves formatting and comments enabled, locks structure', () => {
        for (const method of [...FORMATTING, ...COMMENTS]) {
            expect(methodRefusal(mode('trackedChanges'), method), method).toBeUndefined();
        }
        for (const method of STRUCTURAL) {
            expect(methodRefusal(mode('trackedChanges'), method)?.kind, method).toBe('mode');
        }
    });

    it('forms locks formatting, structure and comments (only text enters)', () => {
        for (const method of [...FORMATTING, ...STRUCTURAL, ...COMMENTS]) {
            expect(methodRefusal(mode('forms'), method)?.kind, method).toBe('mode');
        }
        expect(methodRefusal(mode('forms'), 'insertText')).toBeUndefined();
        expect(methodRefusal(mode('forms'), 'splitParagraph')).toBeUndefined();
    });

    it('inside a story every non-exempt member is locked, even where the mode admits it', () => {
        for (const method of [...FORMATTING, ...COMMENTS]) {
            expect(methodRefusal(mode('trackedChanges', true), method)?.kind, method).toBe('story');
        }
    });

    it('reports the first refusal among several members, and none when all are admitted', () => {
        expect(methodRefusal(mode('comments'), 'insertComment', 'toggleFormatting')?.command).toBe(
            'TOGGLE_FORMATTING',
        );
        expect(methodRefusal(mode('comments'), 'insertComment', 'replyToComment')).toBeUndefined();
        expect(methodRefusal(mode('readOnly'))).toBeUndefined();
    });

    it('a member that dispatches several commands is locked when ANY is refused', () => {
        /* `openDocument` sends SET_ZOOM + OPEN_DOCUMENT, both exempt. */
        expect(FACADE_METHOD_COMMANDS.openDocument).toEqual(['SET_ZOOM', 'OPEN_DOCUMENT']);
        expect(methodRefusal(mode('readOnly'), 'openDocument')).toBeUndefined();
    });

    it('host capabilities (no engine command) and the raw escape hatch are never refused', () => {
        expect(FACADE_METHOD_COMMANDS.setRenderer).toBe('host');
        expect(FACADE_METHOD_COMMANDS.raw).toBe('raw');
        expect(methodRefusal(mode('readOnly'), 'setRenderer', 'raw')).toBeUndefined();
    });

    it('is open for an unprotected document', () => {
        for (const method of [...FORMATTING, ...STRUCTURAL, ...COMMENTS]) {
            expect(methodRefusal(open, method), method).toBeUndefined();
        }
    });
});

/* ---- createEditorState().protectionRefusal ----------------------------- */

const listeners: Array<(e: Event) => void> = [];
const fakeEngine = {
    renderer: 'canvas2d',
    subscribe(fn: (e: Event) => void): () => void {
        listeners.push(fn);
        return () => {};
    },
};
vi.mock('./EngineProvider', () => ({ useEngine: () => fakeEngine }));

const emit = (e: unknown): void => {
    for (const l of listeners) l(e as Event);
};

const POS = { path: { steps: [{ kind: 'BLOCK', idx: 0 }] }, offset: 0 };
const selectionChanged = (extra: Record<string, unknown>) => ({
    type: 'SELECTION_CHANGED',
    range: { start: POS, end: POS },
    caret: { x: 0, y: 0, w: 1, h: 10 },
    rects: [],
    tab_stops: [],
    can_undo: false,
    can_redo: false,
    is_tracking_changes: false,
    field_code_view: false,
    document_revision: 1,
    ...extra,
});

describe('createEditorState().protectionRefusal (#449)', () => {
    it('follows SELECTION_CHANGED.protection and editing_story, reactively', async () => {
        const { createEditorState } = await import('./createEditorState');
        const dispose = createRoot((d) => {
            const state = createEditorState();
            /* Nothing known yet: nothing is locked. */
            expect(state.protectionRefusal('toggleFormatting')).toBeUndefined();

            emit(selectionChanged({ protection: 'readOnly' }));
            expect(state.protection()).toBe('readOnly');
            expect(state.protectionRefusal('toggleFormatting')?.kind).toBe('mode');
            expect(state.protectionRefusal('insertTableAtCaret', 'insertImageAtCaret')?.kind).toBe('mode');
            expect(state.protectionRefusal('undo', 'setZoom')).toBeUndefined();

            emit(selectionChanged({ protection: 'comments' }));
            expect(state.protectionRefusal('insertComment')).toBeUndefined();
            expect(state.protectionRefusal('toggleFormatting')?.kind).toBe('mode');

            emit(selectionChanged({ protection: 'trackedChanges' }));
            expect(state.protectionRefusal('toggleFormatting')).toBeUndefined();
            emit(
                selectionChanged({
                    protection: 'trackedChanges',
                    editing_story: { kind: 'HEADER', section: 0, slot: 'DEFAULT' },
                }),
            );
            expect(state.protectionRefusal('toggleFormatting')?.kind).toBe('story');
            expect(state.protectionRefusal('exitHeaderFooter')).toBeUndefined();

            /* A document without protection (a new document, or the
               restriction gone) unlocks everything. */
            emit(selectionChanged({ protection: undefined }));
            expect(state.protectionRefusal('toggleFormatting', 'insertTableAtCaret')).toBeUndefined();
            return d;
        });
        dispose();
    });
});
