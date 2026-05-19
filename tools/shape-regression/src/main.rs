//! Shape-regression harness.
//!
//! Reads `corpus.json`, shapes each string via `text-pipeline::shape_text`,
//! and compares the result against committed `expected` snapshots.
//!
//! Usage:
//!     cargo run -p shape-regression                    # verify against snapshots
//!     UPDATE=1 cargo run -p shape-regression           # write actual back as expected
//!
//! No HarfBuzz CLI dependency at this stage; future weeks can bind to `hb-shape`
//! for independent verification once the runtime is available.

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use text_pipeline::{LoadedFont, ShapingDirection, shape_text};

#[derive(Deserialize, Serialize, Debug)]
struct Corpus {
    cases: Vec<TestCase>,
}

#[derive(Deserialize, Serialize, Debug)]
struct TestCase {
    id: String,
    text: String,
    font: String,
    direction: String,
    px_size: f32,
    #[serde(default)]
    expected: Option<Expected>,
}

#[derive(Deserialize, Serialize, Debug, PartialEq, Clone)]
struct Expected {
    glyph_count: u32,
    glyph_ids: Vec<u32>,
    total_advance_approx: f32,
}

fn project_root() -> PathBuf {
    // tools/shape-regression -> ../.. = project root
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .expect("project root parent")
}

fn run_case(case: &TestCase, root: &Path) -> Result<Expected> {
    let font_path = root.join(&case.font);
    let bytes =
        fs::read(&font_path).with_context(|| format!("read font {}", font_path.display()))?;
    let font =
        LoadedFont::parse(case.id.clone(), bytes).map_err(|e| anyhow!("LoadedFont::parse: {e}"))?;
    let dir = match case.direction.as_str() {
        "RTL" | "rtl" => ShapingDirection::Rtl,
        _ => ShapingDirection::Ltr,
    };
    let shaped = shape_text(&font, &case.text, dir, case.px_size);
    let glyph_ids: Vec<u32> = shaped.glyphs.iter().map(|g| g.glyph_id).collect();
    Ok(Expected {
        glyph_count: glyph_ids.len() as u32,
        glyph_ids,
        total_advance_approx: shaped.total_advance,
    })
}

fn diff(actual: &Expected, expected: &Expected) -> Vec<String> {
    let mut errs = vec![];
    if actual.glyph_count != expected.glyph_count {
        errs.push(format!(
            "glyph_count: expected {}, got {}",
            expected.glyph_count, actual.glyph_count
        ));
    }
    if actual.glyph_ids != expected.glyph_ids {
        errs.push(format!(
            "glyph_ids:\n    expected {:?}\n    actual   {:?}",
            expected.glyph_ids, actual.glyph_ids
        ));
    }
    if (actual.total_advance_approx - expected.total_advance_approx).abs() > 0.5 {
        errs.push(format!(
            "total_advance: expected ~{:.2}, got {:.2}",
            expected.total_advance_approx, actual.total_advance_approx
        ));
    }
    errs
}

fn main() -> ExitCode {
    let update = env::var("UPDATE").map(|v| v == "1").unwrap_or(false);
    let root = project_root();
    let corpus_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("corpus.json");
    let raw = match fs::read(&corpus_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("read {}: {e}", corpus_path.display());
            return ExitCode::FAILURE;
        }
    };
    let mut corpus: Corpus = match serde_json::from_slice(&raw) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("parse corpus.json: {e}");
            return ExitCode::FAILURE;
        }
    };

    let mut pass = 0;
    let mut fail = 0;

    for case in &mut corpus.cases {
        let actual = match run_case(case, &root) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("[ERROR] {}: {e}", case.id);
                fail += 1;
                continue;
            }
        };

        if update || case.expected.is_none() {
            println!(
                "[UPDATE] {}: {} glyphs, advance={:.2}, ids={:?}",
                case.id, actual.glyph_count, actual.total_advance_approx, actual.glyph_ids
            );
            case.expected = Some(actual);
            pass += 1;
            continue;
        }

        let expected = case.expected.as_ref().unwrap();
        let errs = diff(&actual, expected);
        if errs.is_empty() {
            println!(
                "[PASS] {}: {} glyphs, advance={:.2}",
                case.id, actual.glyph_count, actual.total_advance_approx
            );
            pass += 1;
        } else {
            println!("[FAIL] {}:", case.id);
            for e in errs {
                println!("    {e}");
            }
            fail += 1;
        }
    }

    if update {
        let pretty = serde_json::to_string_pretty(&corpus).expect("serialize");
        fs::write(&corpus_path, pretty).expect("write corpus.json");
        println!("\n[UPDATE] corpus.json refreshed");
    }

    println!("\n{pass} passed, {fail} failed");

    if fail > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
