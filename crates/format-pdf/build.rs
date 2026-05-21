//! Build script — synthesizes the sRGB ICC profile the PDF/A-1b `OutputIntent`
//! embeds.
//!
//! CLAUDE.md forbids vendoring binary blobs into the source tree. PDF/A-1b
//! nonetheless requires an embedded ICC profile in the output intent. The
//! resolution: do not check a `.icc` file in — *generate* one here from plain,
//! auditable Rust. The synthesis is hermetic (no network, no system files), so
//! the build stays reproducible and CI-friendly, and the only artifact is a
//! transient file under `OUT_DIR` that `lib.rs` pulls in with `include_bytes!`.
//!
//! The profile is a minimal valid ICC v2.1.0 RGB display profile: the sRGB
//! primaries and white point (Bradford-adapted to the D50 PCS) with a gamma-2.2
//! tone curve approximating the sRGB transfer function. That is sufficient for
//! a PDF/A-1b RGB output intent — veraPDF validates the PDF structure around
//! the profile and the profile's own header/tag integrity, not photometric
//! fidelity to the full piecewise sRGB curve.

use std::env;
use std::fs;
use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let icc = build_srgb_icc();
    let out =
        Path::new(&env::var("OUT_DIR").expect("OUT_DIR set by cargo")).join("srgb-v2-micro.icc");
    fs::write(&out, &icc).expect("write synthesized ICC profile");
}

/// ICC `s15Fixed16Number` — a fixed-point real, `value * 65536`, big-endian.
fn s15f16(v: f64) -> [u8; 4] {
    ((v * 65536.0).round() as i32).to_be_bytes()
}

/// `XYZType` tag data: signature, reserved, one `XYZNumber` (3 × s15Fixed16).
fn xyz_tag(x: f64, y: f64, z: f64) -> Vec<u8> {
    let mut t = Vec::with_capacity(20);
    t.extend_from_slice(b"XYZ ");
    t.extend_from_slice(&[0; 4]);
    t.extend_from_slice(&s15f16(x));
    t.extend_from_slice(&s15f16(y));
    t.extend_from_slice(&s15f16(z));
    t
}

/// `curveType` tag data with a single entry — an ICC gamma curve. The lone
/// `u16` is a `u8Fixed8Number` (`gamma * 256`).
fn curve_gamma(gamma: f64) -> Vec<u8> {
    let mut t = Vec::with_capacity(14);
    t.extend_from_slice(b"curv");
    t.extend_from_slice(&[0; 4]);
    t.extend_from_slice(&1u32.to_be_bytes());
    t.extend_from_slice(&((gamma * 256.0).round() as u16).to_be_bytes());
    t
}

/// `textType` tag data (ICC v2): signature, reserved, NUL-terminated ASCII.
fn text_tag(s: &str) -> Vec<u8> {
    let mut t = Vec::new();
    t.extend_from_slice(b"text");
    t.extend_from_slice(&[0; 4]);
    t.extend_from_slice(s.as_bytes());
    t.push(0);
    t
}

/// `textDescriptionType` tag data (ICC v2): an ASCII block followed by the
/// (here empty) Unicode and Macintosh ScriptCode blocks the type mandates.
fn desc_tag(s: &str) -> Vec<u8> {
    let mut t = Vec::new();
    t.extend_from_slice(b"desc");
    t.extend_from_slice(&[0; 4]);
    t.extend_from_slice(&((s.len() + 1) as u32).to_be_bytes()); // ASCII count incl. NUL
    t.extend_from_slice(s.as_bytes());
    t.push(0);
    t.extend_from_slice(&0u32.to_be_bytes()); // Unicode language code
    t.extend_from_slice(&0u32.to_be_bytes()); // Unicode count
    t.extend_from_slice(&0u16.to_be_bytes()); // ScriptCode code
    t.push(0); // ScriptCode count
    t.extend_from_slice(&[0u8; 67]); // ScriptCode (Macintosh) description
    t
}

/// Synthesize a minimal valid ICC v2.1.0 RGB display profile for sRGB.
fn build_srgb_icc() -> Vec<u8> {
    /* 128-byte profile header. Everything not set stays zero — that covers the
    optional CMM, platform, manufacturer, model, attribute and creator
    fields, plus the reserved tail. */
    let mut header = vec![0u8; 128];
    header[8..12].copy_from_slice(&0x0210_0000u32.to_be_bytes()); // version 2.1.0
    header[12..16].copy_from_slice(b"mntr"); // device class: display
    header[16..20].copy_from_slice(b"RGB "); // data colour space
    header[20..24].copy_from_slice(b"XYZ "); // profile connection space
    for (i, v) in [2026u16, 5, 22, 0, 0, 0].iter().enumerate() {
        header[24 + i * 2..26 + i * 2].copy_from_slice(&v.to_be_bytes()); // creation date
    }
    header[36..40].copy_from_slice(b"acsp"); // mandatory file signature
    // PCS illuminant — the ICC-fixed D50 white point.
    header[68..72].copy_from_slice(&s15f16(0.964_20));
    header[72..76].copy_from_slice(&s15f16(1.000_00));
    header[76..80].copy_from_slice(&s15f16(0.824_91));

    /* The nine tags mandatory for an RGB matrix/TRC display profile. The
    colorant XYZ values are the sRGB primaries Bradford-adapted to D50. */
    let tags: [(&[u8], Vec<u8>); 9] = [
        (b"desc", desc_tag("sRGB IEC61966-2.1")),
        (b"wtpt", xyz_tag(0.964_20, 1.000_00, 0.824_91)),
        (b"rXYZ", xyz_tag(0.436_07, 0.222_49, 0.013_92)),
        (b"gXYZ", xyz_tag(0.385_15, 0.716_87, 0.097_08)),
        (b"bXYZ", xyz_tag(0.143_07, 0.060_61, 0.714_10)),
        (b"rTRC", curve_gamma(2.2)),
        (b"gTRC", curve_gamma(2.2)),
        (b"bTRC", curve_gamma(2.2)),
        (
            b"cprt",
            text_tag("Synthesized sRGB-approximate profile - no rights reserved."),
        ),
    ];

    /* Tag table: a count followed by 12-byte (signature, offset, size) rows.
    Tag data is concatenated after the table, each block aligned to 4. */
    let mut table = Vec::new();
    table.extend_from_slice(&(tags.len() as u32).to_be_bytes());
    let mut body = Vec::new();
    let data_start = 128 + 4 + tags.len() * 12;
    for (sig, data) in &tags {
        table.extend_from_slice(sig);
        table.extend_from_slice(&((data_start + body.len()) as u32).to_be_bytes());
        table.extend_from_slice(&(data.len() as u32).to_be_bytes());
        body.extend_from_slice(data);
        while body.len() % 4 != 0 {
            body.push(0);
        }
    }

    let mut profile = Vec::with_capacity(128 + table.len() + body.len());
    profile.extend_from_slice(&header);
    profile.extend_from_slice(&table);
    profile.extend_from_slice(&body);
    let total = profile.len() as u32;
    profile[0..4].copy_from_slice(&total.to_be_bytes()); // profile size
    profile
}
