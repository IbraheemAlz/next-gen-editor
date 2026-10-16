/* Issue #500 (item 3) - the layout-cache view of `EngineStats` and the Dev
 * HUD's "Layout caches" line. Pure functions: no engine, no DOM. */
import { describe, expect, it } from 'vitest';
import {
    cacheHitRatio,
    formatCacheBytes,
    formatHitRatio,
    formatLayoutCaches,
    layoutCacheStatsFrom,
} from './layoutCaches';
import type { EngineStats } from './types';

const MIB = 1024 * 1024;

const stats = (over: Partial<EngineStats> = {}): EngineStats => ({
    wasm_heap_bytes: 0,
    document_tree_bytes: 0,
    glyph_cache_entries: 0,
    undo_stack_depth: 0,
    fonts_resident: 0,
    last_paint_ms: 0,
    last_command_ms: 0,
    paragraph_count: 0,
    word_count: 0,
    character_count: 0,
    paragraph_cache_bytes: 0,
    paragraph_cache_entries: 0,
    paragraph_cache_hits: 0,
    paragraph_cache_misses: 0,
    table_cache_bytes: 0,
    table_cache_entries: 0,
    table_cache_hits: 0,
    table_cache_misses: 0,
    ...over,
});

describe('cacheHitRatio (#500)', () => {
    it('is undefined before the first lookup, a fraction after', () => {
        expect(cacheHitRatio(0, 0)).toBeUndefined();
        expect(cacheHitRatio(0, 4)).toBe(0);
        expect(cacheHitRatio(3, 1)).toBe(0.75);
        expect(cacheHitRatio(7, 0)).toBe(1);
    });
});

describe('layoutCacheStatsFrom (#500)', () => {
    it('is undefined until the first STATS reply', () => {
        expect(layoutCacheStatsFrom(undefined)).toBeUndefined();
    });

    it('reads both caches off the reply, with their hit ratios', () => {
        const view = layoutCacheStatsFrom(
            stats({
                paragraph_cache_bytes: 1000,
                paragraph_cache_entries: 4,
                paragraph_cache_hits: 97,
                paragraph_cache_misses: 3,
                table_cache_bytes: 500,
                table_cache_entries: 1,
                table_cache_hits: 0,
                table_cache_misses: 2,
            }),
        );
        expect(view).toEqual({
            paragraph: { bytes: 1000, entries: 4, hits: 97, misses: 3, hitRatio: 0.97 },
            table: { bytes: 500, entries: 1, hits: 0, misses: 2, hitRatio: 0 },
        });
    });

    it('reads an engine build that predates #500 (fields absent) as zeros', () => {
        const old = { ...stats() } as Partial<EngineStats>;
        for (const k of Object.keys(old)) {
            if (k.includes('cache') && k !== 'glyph_cache_entries') {
                delete (old as Record<string, unknown>)[k];
            }
        }
        const view = layoutCacheStatsFrom(old as EngineStats);
        expect(view?.paragraph).toEqual({
            bytes: 0,
            entries: 0,
            hits: 0,
            misses: 0,
            hitRatio: undefined,
        });
        expect(view?.table.hitRatio).toBeUndefined();
    });
});

describe('formatCacheBytes / formatHitRatio (#500)', () => {
    it('uses binary units with one decimal', () => {
        expect(formatCacheBytes(0)).toBe('0 B');
        expect(formatCacheBytes(1023)).toBe('1023 B');
        expect(formatCacheBytes(1024)).toBe('1.0 KiB');
        expect(formatCacheBytes(320 * 1024)).toBe('320.0 KiB');
        expect(formatCacheBytes(48.2 * MIB)).toBe('48.2 MiB');
        expect(formatCacheBytes(192 * MIB)).toBe('192.0 MiB');
    });

    it('floors the ratio so a near-perfect cache does not read as perfect', () => {
        expect(formatHitRatio(undefined)).toBe('–');
        expect(formatHitRatio(0)).toBe('0 %');
        expect(formatHitRatio(0.97)).toBe('97 %');
        expect(formatHitRatio(0.996)).toBe('99 %');
        expect(formatHitRatio(1)).toBe('100 %');
    });
});

describe('formatLayoutCaches (#500)', () => {
    it('words both caches on one line', () => {
        const line = formatLayoutCaches(
            layoutCacheStatsFrom(
                stats({
                    paragraph_cache_bytes: Math.round(48.2 * MIB),
                    paragraph_cache_entries: 1203,
                    paragraph_cache_hits: 97,
                    paragraph_cache_misses: 3,
                    table_cache_bytes: Math.round(3.1 * MIB),
                    table_cache_entries: 14,
                    table_cache_hits: 9,
                    table_cache_misses: 0,
                }),
            ),
        );
        expect(line).toBe('para 48.2 MiB / 1,203 · 97 % hit · table 3.1 MiB / 14 · 100 % hit');
    });

    it('shows an untouched cache as empty with no ratio, and no stats as a dash', () => {
        expect(formatLayoutCaches(layoutCacheStatsFrom(stats()))).toBe(
            'para 0 B / 0 · – hit · table 0 B / 0 · – hit',
        );
        expect(formatLayoutCaches(undefined)).toBe('–');
    });
});
