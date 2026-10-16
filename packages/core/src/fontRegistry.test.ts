/* Issue #472 - the font registry's per-script defaults: the manifest's
 * `defaults_by_script` is normalised, mapped onto the bridge `Script`
 * names and sent as ONE `SET_FONT_PRIORITY`; the engine's effective answer
 * comes back, and a refusal never rejects. A stubbed `fetch` serves the
 * manifest, a scripted engine answers - no DOM, no worker. */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { createFontRegistry, normalizeScriptDefaults, type FontManifest } from './createFontRegistry';
import type { Command, Event } from './types';

const manifest = (extra: Partial<FontManifest>): FontManifest => ({
    defaults: ['amiri', 'liberation'],
    fonts: [],
    ...extra,
});

function registryFor(m: FontManifest, answer: (cmd: Command) => Event | Promise<Event>) {
    vi.stubGlobal(
        'fetch',
        vi.fn(async () => ({ ok: true, status: 200, json: async () => m })),
    );
    const sent: Command[] = [];
    const registry = createFontRegistry(
        {
            dispatch: async (cmd: Command) => {
                sent.push(cmd);
                return answer(cmd);
            },
        },
        { manifestUrl: 'http://localhost/fonts.json' },
    );
    return { registry, sent };
}

afterEach(() => {
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
});

describe('normalizeScriptDefaults (#472)', () => {
    it('keeps ordered lists, wraps a bare id and drops junk', () => {
        expect(
            normalizeScriptDefaults({
                Latin: 'liberation',
                arabic: ['amiri', '', 'noto-naskh'],
                common: [],
            }),
        ).toEqual({ latin: ['liberation'], arabic: ['amiri', 'noto-naskh'] });
        expect(normalizeScriptDefaults(undefined)).toEqual({});
    });
});

describe('FontRegistry.applyScriptDefaults (#472)', () => {
    it('sends the manifest table as one SET_FONT_PRIORITY and returns the effective lists', async () => {
        const effective = [{ script: 'Latin' as const, ids: ['liberation'] }];
        const { registry, sent } = registryFor(
            manifest({
                defaults_by_script: {
                    latin: ['liberation'],
                    arabic: ['amiri', 'noto-naskh'],
                    common: ['liberation'],
                },
            }),
            () => ({ type: 'FONT_PRIORITY_APPLIED', priorities: effective }),
        );
        expect(await registry.applyScriptDefaults()).toEqual(effective);
        expect(sent).toEqual([
            {
                type: 'SET_FONT_PRIORITY',
                priorities: [
                    { script: 'Latin', ids: ['liberation'] },
                    { script: 'Arabic', ids: ['amiri', 'noto-naskh'] },
                    /* Issue #490 - the explicit script-neutral default
                     * (digits, punctuation, empty paragraphs, marker glyphs). */
                    { script: 'Common', ids: ['liberation'] },
                ],
            },
        ]);
        expect(registry.scriptDefaults()).toEqual({
            latin: ['liberation'],
            arabic: ['amiri', 'noto-naskh'],
            common: ['liberation'],
        });
    });

    it('sends nothing for a manifest that declares no defaults', async () => {
        const { registry, sent } = registryFor(manifest({}), () => {
            throw new Error('must not be asked');
        });
        expect(await registry.applyScriptDefaults()).toEqual([]);
        expect(sent).toEqual([]);
    });

    it('warns about an unknown script key and still sends the known ones', async () => {
        const warn = vi.spyOn(console, 'warn').mockImplementation(() => undefined);
        const { registry, sent } = registryFor(
            manifest({ defaults_by_script: { klingon: ['x'], latin: 'liberation' } }),
            () => ({ type: 'FONT_PRIORITY_APPLIED', priorities: [] }),
        );
        await registry.applyScriptDefaults();
        expect(warn).toHaveBeenCalledWith(expect.stringContaining("unknown script 'klingon'"));
        expect(sent).toEqual([
            { type: 'SET_FONT_PRIORITY', priorities: [{ script: 'Latin', ids: ['liberation'] }] },
        ]);
    });

    it('never rejects: an engine error or a throw leaves the engine fallback and warns', async () => {
        const warn = vi.spyOn(console, 'warn').mockImplementation(() => undefined);
        const failing = registryFor(manifest({ defaults_by_script: { latin: 'liberation' } }), () => ({
            type: 'ERROR',
            message: 'nope',
        }));
        expect(await failing.registry.applyScriptDefaults()).toEqual([]);
        const throwing = registryFor(manifest({ defaults_by_script: { latin: 'liberation' } }), () => {
            throw new Error('worker gone');
        });
        expect(await throwing.registry.applyScriptDefaults()).toEqual([]);
        expect(warn).toHaveBeenCalledTimes(2);
    });
});
