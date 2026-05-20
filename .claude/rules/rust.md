---
description: Rust + WASM conventions for this workspace.
paths:
  - "crates/**/*.rs"
  - "tools/**/*.rs"
  - "**/Cargo.toml"
  - ".cargo/**"
  - "rust-toolchain.toml"
---

# Rust / WASM rules

## Toolchain
- Pinned to **Rust 1.95.0** in `rust-toolchain.toml` (bumped from 1.85.1 for the Phase 3 render stack — `vello` 0.9 needs ≥ 1.88). Do not bump without auditing every workspace member's MSRV.
- `wasm-pack` comes from **Homebrew** (`brew install wasm-pack`).
- **let-chains** (`if let X && let Y`) are allowed — stable since 1.88. Existing nested `if let` from the 1.85 era may stay; new code may use let-chains.

## Build profile
- `lto = "thin"` for the wasm artifact. **Never `"fat"`** — `compiler-builtins` ships precompiled object files for some intrinsics and fat LTO rejects them.
- `panic = "abort"`, `opt-level = "z"`, `codegen-units = 1`, `strip = true`.

## `.cargo/config.toml`
- **Do not** set `[build] target = "wasm32-unknown-unknown"` at the global level. Breaks `cargo check --workspace` on native tooling.
- Wasm stack size: `-z` then `stack-size=N` (two separate link-args). The old `--stack-size=N` form is rejected by modern `wasm-ld`.

## Workspace conventions
- Member crates inherit `version`, `edition`, `license`, `rust-version` via `.workspace = true`.
- Edition: **2024**.
- Workspace deps declared once at the root, consumed via `dep = { workspace = true }` in members.

## Dependency choices
- `tsify-next`, NOT `tsify` (original unmaintained since 2022). `default-features = false, features = ["js"]`.
- `unicode-bidi`, NOT `icu_bidi` (the latter isn't on crates.io at 1.5).
- `serde_bytes` + `#[tsify(type = "Uint8Array")]` for binary `Vec<u8>` fields crossing the wasm boundary.

## Code style
- Bridge enums always tagged `#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]`.
- Clippy is `-D warnings`; fix the lint, don't `#[allow]` unless documented.
- Prefer `&Path` over `&PathBuf` in fn signatures (clippy::ptr_arg).
- Use `bumpalo::Bump` for per-command scratch allocations (none yet; Phase 3+).

## Tests
- Native unit tests via `cargo test --workspace --lib`.
- WASM unit tests via `wasm-pack test --headless --chrome crates/engine-wasm`. Configure with `wasm_bindgen_test_configure!(run_in_browser)`.

## Watch out for
- `web-sys 0.3.98+`: `set_fill_style_str(&str)` is the modern API; the `JsValue` variant is deprecated.
- `init({ module_or_path: new URL(...) })` for wasm-pack `--target web`; the bare-URL form prints a deprecation warning.
- `swash::GlyphMetrics` has **no** `.bounds()` method; pull bbox from raster `Image::placement` instead.
- BiDi runs **per line**, not paragraph-wide (`crates/layout/src/paragraph.rs`). Don't reintroduce paragraph-wide visual-flatten.
