//! Issue #498 (item 2) - `RenderConfig::shell_default` IS the editor's boot
//! layout config, and the canvas-less engine every native harness uses
//! (`Engine::new_headless`: the fuzz crate, `tools/corpus-native`, the
//! native tests) lays out with it.
//!
//! The shell's side of the contract lives in TypeScript (`ts/src/App.tsx`'s
//! boot `RENDER_PAGE`, `ts/src/state/engine-store.ts` `SCREEN_DPI_SCALE`,
//! `ts/public/fonts.json` `defaults_by_script`). The first test reads those
//! three files at compile time and fails when either side moves without the
//! other, so the two can never drift silently again (before #498 the
//! headless engine laid out at 16 px / 26 px while the shell boots at
//! 24 px / 36 px, and a page count from a harness meant nothing next to the
//! shell's).

use super::*;

/// The first number after `key` in `src`, e.g. `px_size: 24,` -> `24.0`.
fn number_after(src: &str, key: &str) -> f64 {
    let at = src
        .find(key)
        .unwrap_or_else(|| panic!("`{key}` not found in the TS source"));
    let rest = src[at + key.len()..].trim_start();
    let end = rest
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(rest.len());
    rest[..end]
        .parse()
        .unwrap_or_else(|_| panic!("no number after `{key}`: {:?}", &rest[..rest.len().min(24)]))
}

/// The quoted string after `key`, e.g. `align: 'START'` -> `START`.
fn quoted_after<'a>(src: &'a str, key: &str) -> &'a str {
    let at = src
        .find(key)
        .unwrap_or_else(|| panic!("`{key}` not found in the TS source"));
    let rest = src[at + key.len()..].trim_start();
    let quote = rest.chars().next().expect("a quoted value");
    assert!(quote == '\'' || quote == '"', "`{key}` is not a string");
    let body = &rest[1..];
    &body[..body.find(quote).expect("closing quote")]
}

/// The shell's boot `RENDER_PAGE` block (the only `type: 'RENDER_PAGE'`
/// literal in `App.tsx`), cut at its closing `});`.
fn boot_render_page(app: &str) -> &str {
    let at = app
        .find("type: 'RENDER_PAGE'")
        .expect("App.tsx dispatches the boot RENDER_PAGE");
    let rest = &app[at..];
    &rest[..rest.find("});").expect("end of the boot dispatch")]
}

#[test]
fn shell_default_is_the_ts_boot_config() {
    let app = include_str!("../../../ts/src/App.tsx");
    let store = include_str!("../../../ts/src/state/engine-store.ts");
    let fonts = include_str!("../../../ts/public/fonts.json");
    let boot = boot_render_page(app);

    let cfg = RenderConfig::shell_default();

    assert_eq!(f64::from(cfg.px_size), number_after(boot, "px_size:"));
    assert_eq!(
        f64::from(cfg.line_height),
        number_after(boot, "line_height:")
    );
    let align = match cfg.alignment {
        Alignment::Start => "START",
        Alignment::End => "END",
        Alignment::Center => "CENTER",
        Alignment::Justify => "JUSTIFY",
    };
    assert_eq!(align, quoted_after(boot, "align:"));
    let direction = match cfg.base_direction {
        ShapingDirection::Rtl => "RTL",
        ShapingDirection::Ltr => "LTR",
    };
    assert_eq!(direction, quoted_after(boot, "base_direction:"));

    /* The scale the shell sends is `devicePixelRatio * SCREEN_DPI_SCALE`;
    a headless harness has a devicePixelRatio of 1. */
    assert!(
        boot.contains("device_pixel_ratio: window.devicePixelRatio * SCREEN_DPI_SCALE"),
        "the boot scale is no longer dpr x SCREEN_DPI_SCALE: {boot}"
    );
    assert!(
        store.contains("export const SCREEN_DPI_SCALE = 4 / 3;"),
        "SCREEN_DPI_SCALE moved (the engine's SHELL_BASE_SCALE is 4 / 3)"
    );
    assert_eq!(cfg.base_scale, (4.0_f64 / 3.0) as f32);
    assert_eq!(cfg.scale, cfg.base_scale, "zoom 1.0: scale == base_scale");
    assert_eq!(cfg.zoom, 1.0);
    assert_eq!(
        cfg.scale,
        compose_paint_scale(cfg.base_scale, cfg.zoom),
        "the config is internally consistent"
    );

    /* The seed face: the first Latin entry of `defaults_by_script`. */
    let latin = fonts
        .split("\"defaults_by_script\"")
        .nth(1)
        .and_then(|rest| rest.split("\"latin\"").nth(1))
        .and_then(|rest| rest.split('"').nth(1))
        .expect("fonts.json defaults_by_script.latin");
    assert_eq!(cfg.font_id, latin);
}

#[cfg(feature = "fuzz-native")]
#[test]
fn new_headless_lays_out_with_the_shell_default() {
    let engine = Engine::new_headless(DocumentTree::new());
    let got = engine.layout_cfg.as_ref().expect("a cached layout config");
    let shell = RenderConfig::shell_default();
    /* Only the face id differs: the headless engine holds one Liberation
    Sans face, registered as `fuzz-latin`. */
    assert_eq!(got.font_id, "fuzz-latin");
    assert_eq!(got.base_direction, shell.base_direction);
    assert_eq!(got.px_size, shell.px_size);
    assert_eq!(got.line_height, shell.line_height);
    assert_eq!(got.alignment, shell.alignment);
    assert_eq!(got.scale, shell.scale);
    assert_eq!(got.base_scale, shell.base_scale);
    assert_eq!(got.zoom, shell.zoom);
}

/// The purpose of the change: a harness page count is the shell's page
/// count. `tests/perf/Np.docx` are calibrated to lay out to N pages in the
/// shell (#476); `tools/perf --strict` allows +-10 %. The headless engine
/// has only the Liberation Sans face the shell seeds, so the fixtures (which
/// name no other font) must land inside the same band.
#[cfg(feature = "fuzz-native")]
#[test]
fn the_perf_fixtures_lay_out_to_their_nominal_page_counts_headless() {
    let fixtures: [(&str, &[u8], usize); 2] = [
        ("50p", include_bytes!("../../../tests/perf/50p.docx"), 50),
        ("100p", include_bytes!("../../../tests/perf/100p.docx"), 100),
    ];
    for (name, bytes, nominal) in fixtures {
        let mut engine = Engine::new_headless(DocumentTree::new());
        let evt = engine.apply_sync(Command::OpenDocument {
            bytes: bytes.to_vec(),
            format: DocFormat::Docx,
            name: None,
            defaults: None,
            limits: None,
            password: None,
        });
        assert!(
            matches!(evt, Event::DocumentLoaded { .. }),
            "{name}: expected DocumentLoaded, got {evt:?}"
        );
        engine.ensure_layout_for_fuzzing().expect("layout");
        let pages = engine.layout_page_count_for_fuzzing();
        let (lo, hi) = (nominal * 9 / 10, nominal * 11 / 10);
        assert!(
            (lo..=hi).contains(&pages),
            "{name}.docx lays out to {pages} pages headless, nominal {nominal} (+-10 %: {lo}..={hi})"
        );
    }
}
