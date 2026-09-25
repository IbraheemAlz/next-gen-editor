//! Issue #229 — rewrites the `rpc_command` corpus's #186/#187 regression
//! seeds from `command_gen::Scenario`'s explicit builder, instead of
//! hand-edited raw bytes that silently desync when an unrelated generator
//! arm's byte consumption changes (see `Scenario`'s doc comment in
//! `src/command_gen.rs` for the full story).
//!
//! Run this after touching anything upstream of `Scenario::seed_bytes`'s
//! encoding — `gen_seed_text`'s pool selection, `gen_command_sequence`'s
//! bucket dispatch, or the scenario fast path itself:
//!
//!   cargo run --manifest-path fuzz/Cargo.toml --example regen-seeds
//!
//! `command_gen::tests::committed_seed_bytes_match_scenario_builder` pins
//! the committed files to this output, so a decoding-contract change fails
//! that test loudly until this is re-run.

use engine_fuzz::command_gen::Scenario;
use std::path::Path;

fn main() {
    let corpus_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus/rpc_command");
    for scenario in Scenario::ALL {
        let path = corpus_dir.join(scenario.corpus_file());
        let bytes = scenario.seed_bytes();
        std::fs::write(&path, &bytes)
            .unwrap_or_else(|e| panic!("failed to write {}: {e}", path.display()));
        println!(
            "wrote {} ({} bytes): {:02x?}",
            path.display(),
            bytes.len(),
            bytes
        );
    }
    println!(
        "regen-seeds: done — {} scenario seed(s) rewritten",
        Scenario::ALL.len()
    );
}
