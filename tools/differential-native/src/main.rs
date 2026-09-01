//! `differential-native` — issue #89's "our engine" side of the
//! differential oracle harness.
//!
//! `read_docx` → native layout (`crates/layout` + `crates/text-pipeline`,
//! no wasm, no browser) → `crates/format-pdf::export_pdf`. The orchestrating
//! comparison against LibreOffice / Word lives in `tools/differential/`
//! (Node, drives `soffice`, `pdftotext`, `pdftoppm`, and this binary) — see
//! `tools/differential/README.md`.
//!
//! CLEAN-ROOM: this binary and its `pipeline` module were written entirely
//! from this repo's own `layout` / `text-pipeline` / `engine` / `format-pdf`
//! public APIs (the same ones `crates/engine-wasm` calls) plus the ECMA-376
//! spec. Nothing here was read from, or informed by,
//! `/data/code/reference/` — see `plans/cleanroom/PROTOCOL.md`.
//!
//! ```sh
//! cargo run -p differential-native --release -- <input.docx> <output.pdf>
//! ```

mod fixtures;
mod pipeline;

use anyhow::{Context, Result};
use format_docx::read_docx;
use format_pdf::{PdfProfile, export_pdf};
use std::collections::HashMap;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use text_pipeline::{FontStack, LoadedFont};

/// The three fonts committed under `ts/fonts/` — Latin (Liberation Sans) and
/// two Arabic faces (Amiri for justified/kashida body text, Noto Naskh as a
/// second covering face). `FontStack::from_faces` classifies each by script
/// automatically (`fonts.rs::from_faces`), so no per-script wiring is needed
/// here.
fn build_font_stack() -> Result<FontStack> {
    let mut faces: HashMap<String, Arc<LoadedFont>> = HashMap::new();
    let seeds: &[(&str, &[u8])] = &[
        (
            "liberation",
            include_bytes!("../../../ts/fonts/LiberationSans-Regular.ttf"),
        ),
        (
            "amiri",
            include_bytes!("../../../ts/fonts/Amiri-Regular.ttf"),
        ),
        (
            "noto-naskh",
            include_bytes!("../../../ts/fonts/NotoNaskhArabic-Regular.ttf"),
        ),
    ];
    for (id, bytes) in seeds {
        let face = LoadedFont::parse((*id).to_string(), bytes.to_vec())
            .map_err(|e| anyhow::anyhow!("parse font `{id}`: {e:?}"))?;
        faces.insert((*id).to_string(), Arc::new(face));
    }
    Ok(FontStack::from_faces(faces, "liberation"))
}

fn run(input: &Path, output: &Path) -> Result<usize> {
    let bytes = std::fs::read(input).with_context(|| format!("read {}", input.display()))?;
    let archive = read_docx(&bytes).with_context(|| format!("read_docx {}", input.display()))?;
    let mut doc = archive.document;

    let fonts = build_font_stack()?;
    let built = pipeline::build_pages(&mut doc, &fonts);
    let para_texts: Vec<&str> = built.para_texts.iter().map(String::as_str).collect();

    let mut pdf_bytes = Vec::new();
    export_pdf(
        &built.pages,
        &fonts,
        &para_texts,
        PdfProfile::Plain,
        &mut pdf_bytes,
    )
    .map_err(|e| anyhow::anyhow!("export_pdf: {e}"))?;

    if let Some(parent) = output.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    }
    std::fs::write(output, &pdf_bytes).with_context(|| format!("write {}", output.display()))?;

    Ok(built.pages.len())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.first().map(String::as_str) == Some("--gen-fixtures") {
        let dir = args
            .get(1)
            .map(Path::new)
            .unwrap_or_else(|| Path::new(fixtures::DEFAULT_FIXTURES_DIR));
        return match fixtures::generate(dir) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("FAIL: {e:#}");
                ExitCode::FAILURE
            }
        };
    }

    let (Some(input), Some(output)) = (args.first(), args.get(1)) else {
        eprintln!(
            "usage: differential-native <input.docx> <output.pdf>\n       differential-native --gen-fixtures [dir]"
        );
        return ExitCode::FAILURE;
    };
    match run(Path::new(input), Path::new(output)) {
        Ok(page_count) => {
            println!("[differential-native] {input} -> {output} ({page_count} page(s))");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("FAIL: {e:#}");
            ExitCode::FAILURE
        }
    }
}
