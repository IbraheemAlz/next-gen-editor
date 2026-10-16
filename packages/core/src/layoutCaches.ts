/**
 * Issue #500 (item 3) - the engine's layout caches, read off `EngineStats`.
 *
 * The engine keeps two caches that outlive a paint: the byte-budgeted
 * paragraph layout cache (#457) and the content-keyed table box cache
 * (#379). Before #500 their size and hit rate existed only under
 * `cfg(test)`, so a regression such as #497 (a cache that stops hitting)
 * was visible only as a slow paint. `EngineStats` now carries the bytes,
 * the entry count and the lookup counters; this module turns them into
 * the typed view `createEditorState().layoutCacheStats()` exposes and the
 * one line the Dev HUD shows.
 *
 * Headless and pure (no DOM, no engine, no Solid), so it is unit-tested
 * under vitest and shared by the HUD formatting.
 */
import type { EngineStats } from './types';

/** One cache's figures. `hitRatio` is `hits / (hits + misses)` over the
 *  engine's lifetime (a recovery's fresh engine starts again at zero),
 *  `undefined` until the cache has seen a lookup. */
export interface LayoutCacheFigures {
    /** Estimated bytes of the cached boxes. */
    bytes: number;
    /** Boxes currently cached. */
    entries: number;
    hits: number;
    misses: number;
    hitRatio: number | undefined;
}

/** Both layout caches. */
export interface LayoutCacheStats {
    paragraph: LayoutCacheFigures;
    table: LayoutCacheFigures;
}

/** `hits / (hits + misses)`; `undefined` while there has been no lookup. */
export function cacheHitRatio(hits: number, misses: number): number | undefined {
    const total = hits + misses;
    return total > 0 ? hits / total : undefined;
}

function figures(
    bytes: number | undefined,
    entries: number | undefined,
    hits: number | undefined,
    misses: number | undefined,
): LayoutCacheFigures {
    /* An engine build that predates #500 omits the fields: read as zeros. */
    const h = hits ?? 0;
    const m = misses ?? 0;
    return {
        bytes: bytes ?? 0,
        entries: entries ?? 0,
        hits: h,
        misses: m,
        hitRatio: cacheHitRatio(h, m),
    };
}

/** The layout-cache view of a stats reply (`undefined` before the first one). */
export function layoutCacheStatsFrom(stats: EngineStats | undefined): LayoutCacheStats | undefined {
    if (stats === undefined) return undefined;
    return {
        paragraph: figures(
            stats.paragraph_cache_bytes,
            stats.paragraph_cache_entries,
            stats.paragraph_cache_hits,
            stats.paragraph_cache_misses,
        ),
        table: figures(
            stats.table_cache_bytes,
            stats.table_cache_entries,
            stats.table_cache_hits,
            stats.table_cache_misses,
        ),
    };
}

/** `48.2 MiB`, `312.5 KiB`, `96 B` - one decimal, binary units (the
 *  caches' budgets are 192 MiB and 32 MiB). */
export function formatCacheBytes(n: number): string {
    if (n < 1024) return `${n} B`;
    if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KiB`;
    return `${(n / (1024 * 1024)).toFixed(1)} MiB`;
}

/** `97 %`. Floored: 99.6 % must not read as a perfect `100 %`. `–` while
 *  the cache has seen no lookup. */
export function formatHitRatio(ratio: number | undefined): string {
    if (ratio === undefined) return '–';
    return `${Math.floor(ratio * 100)} %`;
}

function figuresLabel(label: string, f: LayoutCacheFigures): string {
    return (
        `${label} ${formatCacheBytes(f.bytes)} / ${f.entries.toLocaleString('en-US')}` +
        ` · ${formatHitRatio(f.hitRatio)} hit`
    );
}

/** The Dev HUD's "Layout caches" row:
 *  `para 48.2 MiB / 1,203 · 97 % hit · table 3.1 MiB / 14 · 100 % hit`. */
export function formatLayoutCaches(stats: LayoutCacheStats | undefined): string {
    if (stats === undefined) return '–';
    return `${figuresLabel('para', stats.paragraph)} · ${figuresLabel('table', stats.table)}`;
}
