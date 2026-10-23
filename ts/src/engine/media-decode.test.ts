/* Issue #506 - which commands make the worker decode pictures, and the
 * decode loop. */
import { describe, expect, it } from 'vitest';
import { commandMeta } from '@nge/core/command-meta';
import { MediaDecoder, mediaDecodePlan, type MediaEntry } from './media-decode';

const plan = (cmd: Parameters<typeof commandMeta>[0], reply = 'SELECTION_CHANGED') =>
    mediaDecodePlan(commandMeta(cmd), reply);

describe('mediaDecodePlan (#506)', () => {
    it('decodes and repaints after INSERT_IMAGE - the picture the engine minted itself', () => {
        expect(plan('INSERT_IMAGE')).toEqual({ decode: true, repaint: true });
    });

    it('decodes after an undo / redo / paste, which can bring a picture blob back', () => {
        for (const type of ['UNDO', 'REDO', 'PASTE_PLAIN'] as const) {
            expect(plan(type).decode, type).toBe(true);
        }
    });

    it('decodes after OPEN_DOCUMENT but leaves the paint to the page canvases', () => {
        expect(plan('OPEN_DOCUMENT', 'DOCUMENT_OPENED')).toEqual({ decode: true, repaint: false });
    });

    it('does nothing for read-only commands (HIT_TEST fires on every pointermove)', () => {
        for (const type of ['HIT_TEST', 'GET_IMAGE_RECTS', 'REQUEST_PAINT', 'PING'] as const) {
            expect(plan(type), type).toEqual({ decode: false, repaint: false });
        }
    });

    it('does nothing when the engine refused the command', () => {
        expect(plan('INSERT_IMAGE', 'ERROR')).toEqual({ decode: false, repaint: false });
        expect(plan('OPEN_DOCUMENT', 'ERROR')).toEqual({ decode: false, repaint: false });
    });

    it('every command that mutates the document gets a decode pass (the rule has no list of its own)', () => {
        const meta = { mutates_doc: true, new_document: false };
        expect(mediaDecodePlan(meta, 'X')).toEqual({ decode: true, repaint: true });
        expect(mediaDecodePlan({ mutates_doc: false, new_document: false }, 'X').decode).toBe(false);
    });
});

const entry = (rel_id: string, size = 4): MediaEntry => ({
    rel_id,
    mime: 'image/png',
    bytes: new Uint8Array(size),
});

function decoder(opts: { failing?: Set<string> } = {}) {
    const registered: string[] = [];
    const warnings: string[] = [];
    let decodes = 0;
    const d = new MediaDecoder<{ id: number }>({
        decode: async (blob) => {
            decodes++;
            if (opts.failing && blob.size === 13) throw new Error('bad image');
            return { id: decodes };
        },
        register: (relId) => registered.push(relId),
        warn: (m) => warnings.push(m),
    });
    return { d, registered, warnings, decodes: () => decodes };
}

describe('MediaDecoder (#506)', () => {
    it('registers every entry it is given, in order, and counts them', async () => {
        const { d, registered } = decoder();
        expect(await d.run([entry('nge_img_1'), entry('nge_img_2')])).toBe(2);
        expect(registered).toEqual(['nge_img_1', 'nge_img_2']);
    });

    it('an empty pending list is a no-op', async () => {
        const { d, registered, decodes } = decoder();
        expect(await d.run([])).toBe(0);
        expect(registered).toEqual([]);
        expect(decodes()).toBe(0);
    });

    it('a blob that cannot be decoded is skipped and reported once, not on every later command', async () => {
        const { d, registered, warnings, decodes } = decoder({ failing: new Set(['bad']) });
        const bad = entry('bad', 13);
        const good = entry('good', 4);
        expect(await d.run([bad, good])).toBe(1);
        expect(registered).toEqual(['good']);
        expect(warnings).toHaveLength(1);
        /* The engine still lists `bad` as pending (it has no bitmap). */
        expect(await d.run([bad])).toBe(0);
        expect(await d.run([bad])).toBe(0);
        expect(warnings).toHaveLength(1);
        expect(decodes()).toBe(2);
    });

    it('reset() forgets past failures (a new document has different blobs)', async () => {
        const { d, warnings } = decoder({ failing: new Set(['bad']) });
        const bad = entry('bad', 13);
        await d.run([bad]);
        d.reset();
        await d.run([bad]);
        expect(warnings).toHaveLength(2);
    });

    it('a key re-minted with different bytes is not mistaken for the failed one', async () => {
        const { d, registered } = decoder({ failing: new Set(['k']) });
        expect(await d.run([entry('k', 13)])).toBe(0);
        expect(await d.run([entry('k', 4)])).toBe(1);
        expect(registered).toEqual(['k']);
    });
});
