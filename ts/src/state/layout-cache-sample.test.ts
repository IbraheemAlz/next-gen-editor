/* Issue #500 (item 3) - the PAINT_TIMING sample's `layout_caches`:
 * `bridge::LayoutCacheTelemetry::from_stats`, mirrored. Pure. */
import { describe, expect, it } from 'vitest';
import { layoutCachesSample } from './layout-cache-sample';

describe('layoutCachesSample (#500)', () => {
    it('carries bytes, entries and a hit ratio per cache', () => {
        expect(
            layoutCachesSample({
                paragraph_cache_bytes: 1000,
                paragraph_cache_entries: 4,
                paragraph_cache_hits: 97,
                paragraph_cache_misses: 3,
                table_cache_bytes: 500,
                table_cache_entries: 1,
                table_cache_hits: 1,
                table_cache_misses: 2,
            }),
        ).toEqual({
            paragraph_bytes: 1000,
            paragraph_entries: 4,
            paragraph_hit_ratio: 0.97,
            table_bytes: 500,
            table_entries: 1,
            table_hit_ratio: 0.3333,
        });
    });

    it('omits a ratio while the cache has seen no lookup', () => {
        const sample = layoutCachesSample({
            paragraph_cache_bytes: 0,
            paragraph_cache_entries: 0,
            paragraph_cache_hits: 0,
            paragraph_cache_misses: 0,
            table_cache_hits: 5,
            table_cache_misses: 0,
        });
        expect(sample).toEqual({
            paragraph_bytes: 0,
            paragraph_entries: 0,
            table_bytes: 0,
            table_entries: 0,
            table_hit_ratio: 1,
        });
        expect(sample).not.toHaveProperty('paragraph_hit_ratio');
    });

    it('is undefined for a stats reply from an engine that predates the counters', () => {
        expect(layoutCachesSample({})).toBeUndefined();
        /* Other numeric stats fields do not count as cache counters. */
        expect(layoutCachesSample({ wasm_heap_bytes: 100 } as never)).toBeUndefined();
    });
});
