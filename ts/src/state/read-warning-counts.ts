/* Issue #406 — the telemetry `DOC_OPEN` sample's per-kind reader-warning
 * counts: `bridge::read_warning_counts`, mirrored. Pure (no DOM, no
 * engine) so it is unit-testable on its own. */
import type { ReadWarning, ReadWarningKind } from '../engine/types';

/** Issue #406 — mirrors `bridge::ReadWarningCount`. */
export interface ReadWarningCount {
    kind: ReadWarningKind;
    count: number;
}

/** `ReadWarningKind` declaration order (the Rust `Ord`): a `Record`, so a
 *  new bridge kind fails `tsc` until it is ranked here. */
const KIND_RANK: Record<ReadWarningKind, number> = {
    TableNestingTooDeep: 0,
    InvalidMeasure: 1,
    MeasureClamped: 2,
    UnclosedField: 3,
    StrayFieldChar: 4,
    FieldNestingTooDeep: 5,
    NonCanonicalNamespaces: 6,
    NotWordprocessingMl: 7,
    MainPartFallback: 8,
    UnsafeRelationshipTarget: 9,
    MalformedPart: 10,
};

/** Issue #406 — `bridge::read_warning_counts`: fold an open's coalesced
 *  warnings by kind (an entry counts `count` times), kinds in ascending
 *  declaration order, each kind once. */
export function readWarningCounts(warnings: readonly ReadWarning[]): ReadWarningCount[] {
    const byKind = new Map<ReadWarningKind, number>();
    for (const w of warnings) {
        byKind.set(w.kind, (byKind.get(w.kind) ?? 0) + Math.max(1, w.count ?? 1));
    }
    return [...byKind]
        .sort(([a], [b]) => (KIND_RANK[a] ?? 99) - (KIND_RANK[b] ?? 99))
        .map(([kind, count]) => ({ kind, count }));
}
