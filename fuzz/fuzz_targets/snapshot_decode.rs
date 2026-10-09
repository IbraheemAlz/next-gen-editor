#![no_main]
//! Issue #341: `engine::snapshot` envelopes — valid and structure-aware
//! mutated — through `Engine::restore` / `Command::Recover`; an `Ok` or a
//! typed error, never a panic, then one `apply` round plus the engine's
//! invariant check. See `engine_fuzz::run_snapshot_decode` /
//! `engine_fuzz::snapshot_gen`.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    engine_fuzz::run_snapshot_decode(data);
});
