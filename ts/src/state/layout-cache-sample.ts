/* Issue #500 (item 3) - the telemetry `PAINT_TIMING` sample's
 * `layout_caches`: `bridge::LayoutCacheTelemetry::from_stats`, mirrored.
 * Pure (no DOM, no engine) so it is unit-testable on its own. */

/** The `EngineStats` counters the sample reads (a structural subset: an
 *  engine build that predates #500 omits them all). */
export interface LayoutCacheCounters {
    paragraph_cache_bytes?: number | undefined;
    paragraph_cache_entries?: number | undefined;
    paragraph_cache_hits?: number | undefined;
    paragraph_cache_misses?: number | undefined;
    table_cache_bytes?: number | undefined;
    table_cache_entries?: number | undefined;
    table_cache_hits?: number | undefined;
    table_cache_misses?: number | undefined;
}

const COUNTER_KEYS = [
    'paragraph_cache_bytes',
    'paragraph_cache_entries',
    'paragraph_cache_hits',
    'paragraph_cache_misses',
    'table_cache_bytes',
    'table_cache_entries',
    'table_cache_hits',
    'table_cache_misses',
] as const satisfies readonly (keyof LayoutCacheCounters)[];

/** Issue #500 - mirrors `bridge::LayoutCacheTelemetry`. A hit ratio is
 *  `hits / (hits + misses)` over the engine's lifetime, omitted while the
 *  cache has seen no lookup (like the Rust `skip_serializing_if`). Numbers
 *  only - no document content. */
export interface LayoutCachesSample {
    paragraph_bytes: number;
    paragraph_entries: number;
    paragraph_hit_ratio?: number;
    table_bytes: number;
    table_entries: number;
    table_hit_ratio?: number;
}

/** `bridge::cache_hit_ratio`, to four decimals (a telemetry batch is not
 *  the place for 17 digits). */
function hitRatio(hits: number, misses: number): number | undefined {
    const total = hits + misses;
    return total > 0 ? Math.round((hits / total) * 10_000) / 10_000 : undefined;
}

/** The sample a stats reply stands for; `undefined` when the reply carries
 *  no cache counter at all (an older engine), so the field is omitted. */
export function layoutCachesSample(stats: LayoutCacheCounters): LayoutCachesSample | undefined {
    const carried = COUNTER_KEYS.some((k) => typeof stats[k] === 'number');
    if (!carried) return undefined;
    const paragraphRatio = hitRatio(stats.paragraph_cache_hits ?? 0, stats.paragraph_cache_misses ?? 0);
    const tableRatio = hitRatio(stats.table_cache_hits ?? 0, stats.table_cache_misses ?? 0);
    return {
        paragraph_bytes: stats.paragraph_cache_bytes ?? 0,
        paragraph_entries: stats.paragraph_cache_entries ?? 0,
        ...(paragraphRatio === undefined ? {} : { paragraph_hit_ratio: paragraphRatio }),
        table_bytes: stats.table_cache_bytes ?? 0,
        table_entries: stats.table_cache_entries ?? 0,
        ...(tableRatio === undefined ? {} : { table_hit_ratio: tableRatio }),
    };
}
