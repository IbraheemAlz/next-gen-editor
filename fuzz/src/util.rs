//! Small shared helpers for the D5.5 (issue #90) generators.

use arbitrary::Unstructured;

/// Pick one element from `pool`. Falls back to the first entry on
/// exhausted entropy (`Unstructured::choose` only errors on an empty
/// slice, which no call site ever passes).
pub fn pick<'a, T>(u: &mut Unstructured, pool: &'a [T]) -> &'a T {
    u.choose(pool).unwrap_or(&pool[0])
}
