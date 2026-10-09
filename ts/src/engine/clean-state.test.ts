/* Issue #332 - the #388 "does the engine equal what the user last saved?"
 * transition, shared by the worker and the main-thread EngineClient. */
import { describe, expect, it } from 'vitest';
import type { Command, Event } from '../../../crates/engine-wasm/pkg/engine_wasm.js';
import { nextCleanState } from './clean-state';

const cmd = (type: string): Command => ({ type }) as unknown as Command;
const evt = (type: string): Event => ({ type }) as unknown as Event;
const ok = evt('PAINTED');
const refused = evt('ERROR');

describe('nextCleanState (#388)', () => {
    it('an accepted document-mutating command makes a clean document dirty', () => {
        expect(nextCleanState(true, cmd('INSERT_TEXT'), ok)).toBe(false);
        expect(nextCleanState(true, cmd('DELETE_AT_CARET'), ok)).toBe(false);
    });

    it('a dirty document stays dirty on further edits', () => {
        expect(nextCleanState(false, cmd('INSERT_TEXT'), ok)).toBe(false);
    });

    it('commands that start a new document leave it clean, even from dirty', () => {
        for (const type of ['OPEN_DOCUMENT', 'LOAD_DOCX', 'RENDER_PAGE', 'CLOSE_DOCUMENT']) {
            expect(nextCleanState(false, cmd(type), ok), type).toBe(true);
            expect(nextCleanState(true, cmd(type), ok), type).toBe(true);
        }
    });

    it('SAVE_DOCX cleans only when the reply is DOCUMENT_SAVED', () => {
        expect(nextCleanState(false, cmd('SAVE_DOCX'), evt('DOCUMENT_SAVED'))).toBe(true);
        expect(nextCleanState(false, cmd('SAVE_DOCX'), ok)).toBe(false);
        expect(nextCleanState(true, cmd('SAVE_DOCX'), ok)).toBe(true);
    });

    it('a refused command (ERROR reply) changes nothing, whatever it was', () => {
        expect(nextCleanState(true, cmd('INSERT_TEXT'), refused)).toBe(true);
        expect(nextCleanState(false, cmd('INSERT_TEXT'), refused)).toBe(false);
        expect(nextCleanState(false, cmd('SAVE_DOCX'), refused)).toBe(false);
        expect(nextCleanState(false, cmd('OPEN_DOCUMENT'), refused)).toBe(false);
    });

    it('read-only / selection / view commands change nothing', () => {
        for (const type of ['HIT_TEST', 'GET_SELECTION_AS_CLIPBOARD']) {
            expect(nextCleanState(true, cmd(type), ok), type).toBe(true);
            expect(nextCleanState(false, cmd(type), ok), type).toBe(false);
        }
    });

    it('an unknown command type is treated conservatively, as a mutation', () => {
        // a newer wire message the generated table does not know: better a
        // spurious "unsaved changes" prompt than a silently lost edit
        expect(nextCleanState(true, cmd('NOT_A_COMMAND'), ok)).toBe(false);
        expect(nextCleanState(false, cmd('NOT_A_COMMAND'), ok)).toBe(false);
    });
});
