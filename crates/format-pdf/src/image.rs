//! Issue #121 — image XObject preparation.
//!
//! The browser decodes images for the canvas (`createImageBitmap` in the
//! worker); PDF export never sees those pixels, so this module turns the raw
//! media bytes carried on the `DocumentTree` into PDF image streams in pure
//! Rust:
//!
//! - **JPEG** passes through untouched as `/DCTDecode`. Only the frame
//!   header (`SOF0`/`SOF1`/`SOF2`) is parsed, for width, height, component
//!   count and sample precision — no pixel decode, no re-encode, no quality
//!   loss, and the stream is exactly the file's bytes.
//! - **PNG** is decoded with the `png` crate (already in the wasm graph via
//!   `vello`), normalized to 8-bit Gray / RGB (palette expanded, 16-bit
//!   stripped), and re-deflated as `/FlateDecode` samples. Alpha becomes a
//!   separate 8-bit `/SMask` stream **or** is flattened onto white — see
//!   [`AlphaMode`].
//! - **GIF / WebP / EMF / WMF / BMP / TIFF / SVG / unknown** are typed skips
//!   ([`ImageSkipReason::UnsupportedFormat`]) — never a panic.
//!
//! Every failure is an [`ImageSkipReason`]; the exporter turns it into a
//! [`crate::PdfWarning`] and leaves the image's rect empty.

use std::io::Cursor;

/// Decoded-size cap: an image whose pixel count exceeds this is skipped
/// ([`ImageSkipReason::TooLarge`]) rather than allocating an unbounded RGBA
/// buffer inside the worker's wasm heap. 40 MP ≈ 160 MiB of RGBA scratch.
pub const MAX_IMAGE_PIXELS: u64 = 40_000_000;

/// How an image's alpha channel reaches the PDF.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlphaMode {
    /// Emit the alpha channel as an 8-bit `DeviceGray` `/SMask` image —
    /// PDF 1.4 transparency (plain output and PDF/A-2u).
    SoftMask,
    /// Composite every pixel onto opaque white and drop the alpha channel.
    /// PDF/A-1b (ISO 19005-1 §6.4 forbids `/SMask`) and PDF/X-3:2003 (ISO
    /// 15930-6 forbids transparency) both need this.
    FlattenOnWhite,
}

/// Why an image was not embedded. The exporter reports it as a
/// [`crate::PdfWarning::ImageSkipped`]; the image's rect stays blank.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageSkipReason {
    /// The layout references a relationship id the media map lacks.
    MissingMedia,
    /// A format the pure-Rust pipeline does not handle (Tier 3): GIF, WebP,
    /// EMF, WMF, BMP, TIFF, SVG or unrecognized bytes.
    UnsupportedFormat { format: &'static str },
    /// A JPEG variant `/DCTDecode` cannot carry (lossless, arithmetic-coded,
    /// 12-bit) or a colour model the profile forbids without a full decode
    /// (CMYK under an sRGB output intent).
    UnsupportedEncoding { detail: String },
    /// The bytes are truncated or corrupt.
    Malformed { detail: String },
    /// The decoded image would exceed [`MAX_IMAGE_PIXELS`].
    TooLarge { width: u32, height: u32 },
}

/// PDF colour space of a prepared image's samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageColor {
    Gray,
    Rgb,
    Cmyk,
}

/// Stream encoding of a prepared image's samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageEncoding {
    /// `data` is the original JPEG file (`/DCTDecode`).
    Dct,
    /// `data` is raw 8-bit samples, row-major, uncompressed — the exporter
    /// deflates them (`/FlateDecode`).
    Raw,
}

/// One image ready to become a PDF image XObject.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedImage {
    pub width: u32,
    pub height: u32,
    pub color: ImageColor,
    pub encoding: ImageEncoding,
    pub data: Vec<u8>,
    /// Adobe-inverted CMYK JPEG — the XObject needs `/Decode [1 0 …]`.
    pub invert_cmyk: bool,
    /// Raw 8-bit alpha samples (one byte per pixel) for an `/SMask`, only
    /// under [`AlphaMode::SoftMask`] and only when some pixel is not opaque.
    pub alpha: Option<Vec<u8>>,
}

/// Sniffed container format of a media part.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaFormat {
    Png,
    Jpeg,
    Other(&'static str),
}

/// Identify a media part by its magic bytes, falling back to the
/// relationship's content type for the formats without a reliable magic
/// (WMF without a placeable header, SVG).
pub fn sniff(data: &[u8], content_type: &str) -> MediaFormat {
    if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        return MediaFormat::Png;
    }
    if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return MediaFormat::Jpeg;
    }
    if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        return MediaFormat::Other("GIF");
    }
    if data.len() >= 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        return MediaFormat::Other("WebP");
    }
    if data.len() >= 44 && data[0..4] == [1, 0, 0, 0] && &data[40..44] == b" EMF" {
        return MediaFormat::Other("EMF");
    }
    if data.starts_with(&[0xD7, 0xCD, 0xC6, 0x9A]) {
        return MediaFormat::Other("WMF");
    }
    if data.starts_with(b"BM") {
        return MediaFormat::Other("BMP");
    }
    if data.starts_with(b"II*\0") || data.starts_with(b"MM\0*") {
        return MediaFormat::Other("TIFF");
    }
    let ct = content_type.to_ascii_lowercase();
    let by_type = match ct.as_str() {
        "image/x-wmf" | "image/wmf" => "WMF",
        "image/x-emf" | "image/emf" => "EMF",
        "image/svg+xml" => "SVG",
        "image/gif" => "GIF",
        "image/webp" => "WebP",
        "image/bmp" => "BMP",
        "image/tiff" => "TIFF",
        _ => "unknown",
    };
    MediaFormat::Other(by_type)
}

/// Turn one media part into a [`PreparedImage`]. `allow_cmyk` is false under
/// every conformant profile: their output intent is sRGB, so `DeviceCMYK`
/// samples would be uncharacterized (and converting would need a full JPEG
/// decoder).
pub fn prepare_image(
    data: &[u8],
    content_type: &str,
    alpha: AlphaMode,
    allow_cmyk: bool,
) -> Result<PreparedImage, ImageSkipReason> {
    match sniff(data, content_type) {
        MediaFormat::Jpeg => prepare_jpeg(data, allow_cmyk),
        MediaFormat::Png => prepare_png(data, alpha),
        MediaFormat::Other(format) => Err(ImageSkipReason::UnsupportedFormat { format }),
    }
}

/// Frame parameters read from a JPEG's start-of-frame marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JpegFrame {
    pub width: u32,
    pub height: u32,
    pub components: u8,
    pub precision: u8,
    /// The SOF marker's low byte (`0xC0` baseline, `0xC2` progressive, …).
    pub marker: u8,
    /// An `APP14 Adobe` segment was present (inverted CMYK convention).
    pub adobe: bool,
}

fn malformed(detail: &str) -> ImageSkipReason {
    ImageSkipReason::Malformed {
        detail: detail.to_string(),
    }
}

/// Walk the JPEG marker segments up to the first start-of-frame and read
/// its header. Bounds-checked throughout: truncated or corrupt input is a
/// [`ImageSkipReason::Malformed`], never a panic.
pub fn parse_jpeg_frame(data: &[u8]) -> Result<JpegFrame, ImageSkipReason> {
    if !data.starts_with(&[0xFF, 0xD8]) {
        return Err(malformed("JPEG: missing SOI"));
    }
    let mut adobe = false;
    let mut i = 2usize;
    loop {
        /* Markers are 0xFF + code, optionally preceded by 0xFF fill. */
        if data.get(i) != Some(&0xFF) {
            return Err(malformed("JPEG: expected marker"));
        }
        while data.get(i) == Some(&0xFF) {
            i += 1;
        }
        let Some(&marker) = data.get(i) else {
            return Err(malformed("JPEG: truncated before SOF"));
        };
        i += 1;
        /* Standalone markers carry no length. */
        if marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            continue;
        }
        if marker == 0xD9 || marker == 0xDA {
            return Err(malformed("JPEG: no frame header before scan data"));
        }
        let (Some(&hi), Some(&lo)) = (data.get(i), data.get(i + 1)) else {
            return Err(malformed("JPEG: truncated segment length"));
        };
        let len = usize::from(u16::from_be_bytes([hi, lo]));
        if len < 2 || i + len > data.len() {
            return Err(malformed("JPEG: segment overruns the file"));
        }
        let seg = &data[i + 2..i + len];
        if marker == 0xEE && seg.starts_with(b"Adobe") {
            adobe = true;
        }
        /* SOF0..SOF15 minus DHT (C4), JPG (C8) and DAC (CC). */
        if (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
            if seg.len() < 6 {
                return Err(malformed("JPEG: short frame header"));
            }
            let precision = seg[0];
            let height = u32::from(u16::from_be_bytes([seg[1], seg[2]]));
            let width = u32::from(u16::from_be_bytes([seg[3], seg[4]]));
            let components = seg[5];
            if width == 0 || height == 0 {
                return Err(malformed("JPEG: zero frame dimension (DNL unsupported)"));
            }
            return Ok(JpegFrame {
                width,
                height,
                components,
                precision,
                marker,
                adobe,
            });
        }
        i += len;
    }
}

fn prepare_jpeg(data: &[u8], allow_cmyk: bool) -> Result<PreparedImage, ImageSkipReason> {
    let frame = parse_jpeg_frame(data)?;
    /* `/DCTDecode` is Huffman baseline / extended / progressive, 8-bit. */
    if !matches!(frame.marker, 0xC0..=0xC2) {
        return Err(ImageSkipReason::UnsupportedEncoding {
            detail: format!(
                "JPEG SOF{} (lossless / arithmetic) has no DCTDecode form",
                frame.marker - 0xC0
            ),
        });
    }
    if frame.precision != 8 {
        return Err(ImageSkipReason::UnsupportedEncoding {
            detail: format!("JPEG {}-bit samples", frame.precision),
        });
    }
    let color = match frame.components {
        1 => ImageColor::Gray,
        3 => ImageColor::Rgb,
        4 if allow_cmyk => ImageColor::Cmyk,
        4 => {
            return Err(ImageSkipReason::UnsupportedEncoding {
                detail: "CMYK JPEG under an sRGB output intent".to_string(),
            });
        }
        n => {
            return Err(ImageSkipReason::UnsupportedEncoding {
                detail: format!("JPEG with {n} components"),
            });
        }
    };
    Ok(PreparedImage {
        width: frame.width,
        height: frame.height,
        color,
        encoding: ImageEncoding::Dct,
        data: data.to_vec(),
        invert_cmyk: color == ImageColor::Cmyk && frame.adobe,
        alpha: None,
    })
}

fn prepare_png(data: &[u8], alpha_mode: AlphaMode) -> Result<PreparedImage, ImageSkipReason> {
    let mut decoder = png::Decoder::new(Cursor::new(data));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder
        .read_info()
        .map_err(|e| malformed(&format!("PNG header: {e}")))?;
    let (width, height) = reader.info().size();
    if width == 0 || height == 0 {
        return Err(malformed("PNG: zero dimension"));
    }
    if u64::from(width) * u64::from(height) > MAX_IMAGE_PIXELS {
        return Err(ImageSkipReason::TooLarge { width, height });
    }
    let size = reader
        .output_buffer_size()
        .ok_or(ImageSkipReason::TooLarge { width, height })?;
    let mut buf = vec![0u8; size];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| malformed(&format!("PNG data: {e}")))?;
    let (color_channels, has_alpha, color) = match info.color_type {
        png::ColorType::Grayscale => (1, false, ImageColor::Gray),
        png::ColorType::GrayscaleAlpha => (1, true, ImageColor::Gray),
        png::ColorType::Rgb => (3, false, ImageColor::Rgb),
        png::ColorType::Rgba => (3, true, ImageColor::Rgb),
        /* EXPAND turns palettes into RGB(A); unreachable in practice. */
        png::ColorType::Indexed => {
            return Err(ImageSkipReason::UnsupportedEncoding {
                detail: "PNG palette not expanded".to_string(),
            });
        }
    };
    if info.bit_depth != png::BitDepth::Eight {
        return Err(ImageSkipReason::UnsupportedEncoding {
            detail: format!("PNG bit depth {:?} after normalization", info.bit_depth),
        });
    }
    let pixels = width as usize * height as usize;
    let stride = color_channels + usize::from(has_alpha);
    /* Rows are tightly packed at 8 bits (`line_size` = width × stride). */
    let row = info.line_size;
    let mut samples = Vec::with_capacity(pixels * color_channels);
    let mut alpha = if has_alpha {
        Some(Vec::with_capacity(pixels))
    } else {
        None
    };
    for y in 0..height as usize {
        let line = &buf[y * row..y * row + width as usize * stride];
        for px in line.chunks_exact(stride) {
            let (c, a) = px.split_at(color_channels);
            match (alpha.as_mut(), a.first().copied()) {
                (Some(al), Some(a)) => {
                    if alpha_mode == AlphaMode::FlattenOnWhite {
                        samples.extend(c.iter().map(|&v| flatten_on_white(v, a)));
                    } else {
                        samples.extend_from_slice(c);
                    }
                    al.push(a);
                }
                _ => samples.extend_from_slice(c),
            }
        }
    }
    /* No soft mask when flattening, or when every pixel is opaque. */
    let alpha = match alpha {
        Some(a) if alpha_mode == AlphaMode::SoftMask && a.iter().any(|&v| v != 255) => Some(a),
        _ => None,
    };
    Ok(PreparedImage {
        width,
        height,
        color,
        encoding: ImageEncoding::Raw,
        data: samples,
        invert_cmyk: false,
        alpha,
    })
}

/// Source-over composite of one 8-bit channel onto opaque white, rounded:
/// `c·a/255 + 255·(1 − a/255)`.
pub fn flatten_on_white(c: u8, a: u8) -> u8 {
    let (c, a) = (u32::from(c), u32::from(a));
    ((c * a + 255 * (255 - a) + 127) / 255) as u8
}

#[doc(hidden)]
pub mod test_images {
    //! Fixture images synthesized at test time — no binary blobs in the
    //! tree. Public (hidden) only so downstream crates' tests (engine-wasm's
    //! end-to-end `ExportPdf` test) share them; nothing in the export path
    //! calls these, so the linker drops them from the wasm artifact.

    /// Encode an 8-bit PNG with the `png` crate's encoder.
    pub fn png(width: u32, height: u32, color: png::ColorType, pixels: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, width, height);
            enc.set_color(color);
            enc.set_depth(png::BitDepth::Eight);
            let mut w = enc.write_header().expect("png header");
            w.write_image_data(pixels).expect("png data");
        }
        out
    }

    /// [`png`] for 8-bit RGBA pixels.
    pub fn png_rgba(width: u32, height: u32, pixels: &[u8]) -> Vec<u8> {
        png(width, height, ::png::ColorType::Rgba, pixels)
    }

    /// A structurally valid baseline JPEG: SOI, APP0 JFIF, one DQT, an SOF0
    /// frame of `width`×`height`×`components`, the two standard DC/AC
    /// Huffman tables, SOS, a scan of all-zero coefficients (mid-grey) and
    /// EOI. Every MCU codes DC diff 0 (`00`) + EOB (`1010`), so the scan is a
    /// repeating bit pattern; decoders render it as uniform grey.
    pub fn jpeg(width: u16, height: u16, components: u8) -> Vec<u8> {
        let mut j = vec![0xFF, 0xD8];
        /* APP0 JFIF. */
        j.extend_from_slice(&[
            0xFF, 0xE0, 0x00, 0x10, b'J', b'F', b'I', b'F', 0x00, 0x01, 0x01, 0x00, 0x00, 0x01,
            0x00, 0x01, 0x00, 0x00,
        ]);
        /* DQT — table 0, all ones. */
        j.extend_from_slice(&[0xFF, 0xDB, 0x00, 0x43, 0x00]);
        j.extend(std::iter::repeat_n(1u8, 64));
        /* SOF0. */
        let sof_len = 8 + 3 * u16::from(components);
        j.extend_from_slice(&[0xFF, 0xC0]);
        j.extend_from_slice(&sof_len.to_be_bytes());
        j.push(8);
        j.extend_from_slice(&height.to_be_bytes());
        j.extend_from_slice(&width.to_be_bytes());
        j.push(components);
        for c in 0..components {
            j.extend_from_slice(&[c + 1, 0x11, 0x00]);
        }
        /* DHT — standard luminance DC (class 0 id 0) and AC (class 1 id 0). */
        let dc_bits = [0u8, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0];
        let dc_vals: Vec<u8> = (0..12).collect();
        let ac_bits = [0u8, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 0x7d];
        let ac_vals: [u8; 162] = [
            0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51,
            0x61, 0x07, 0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xa1, 0x08, 0x23, 0x42, 0xb1, 0xc1,
            0x15, 0x52, 0xd1, 0xf0, 0x24, 0x33, 0x62, 0x72, 0x82, 0x09, 0x0a, 0x16, 0x17, 0x18,
            0x19, 0x1a, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39,
            0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57,
            0x58, 0x59, 0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x73, 0x74, 0x75,
            0x76, 0x77, 0x78, 0x79, 0x7a, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x92,
            0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7,
            0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3,
            0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8,
            0xd9, 0xda, 0xe1, 0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf1, 0xf2,
            0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa,
        ];
        for (class_id, bits, vals) in [
            (0x00u8, &dc_bits[..], &dc_vals[..]),
            (0x10u8, &ac_bits[..], &ac_vals[..]),
        ] {
            let len = (2 + 1 + 16 + vals.len()) as u16;
            j.extend_from_slice(&[0xFF, 0xC4]);
            j.extend_from_slice(&len.to_be_bytes());
            j.push(class_id);
            j.extend_from_slice(bits);
            j.extend_from_slice(vals);
        }
        /* SOS. */
        let sos_len = 6 + 2 * u16::from(components);
        j.extend_from_slice(&[0xFF, 0xDA]);
        j.extend_from_slice(&sos_len.to_be_bytes());
        j.push(components);
        for c in 0..components {
            j.extend_from_slice(&[c + 1, 0x00]);
        }
        j.extend_from_slice(&[0x00, 0x3F, 0x00]);
        /* Scan: per block `00` (DC 0) + `1010` (EOB) = 6 bits. */
        let blocks = usize::from(width.div_ceil(8))
            * usize::from(height.div_ceil(8))
            * usize::from(components);
        let mut bits: Vec<bool> = Vec::with_capacity(blocks * 6);
        for _ in 0..blocks {
            bits.extend_from_slice(&[false, false, true, false, true, false]);
        }
        while bits.len() % 8 != 0 {
            bits.push(true);
        }
        for byte in bits.chunks(8) {
            let b = byte
                .iter()
                .fold(0u8, |acc, &bit| (acc << 1) | u8::from(bit));
            j.push(b);
            if b == 0xFF {
                j.push(0x00);
            }
        }
        j.extend_from_slice(&[0xFF, 0xD9]);
        j
    }
}

#[cfg(test)]
mod tests {
    use super::test_images::{jpeg, png as make_png};
    use super::*;

    #[test]
    fn sniffs_formats_by_magic_then_content_type() {
        assert_eq!(
            sniff(&make_png(1, 1, png::ColorType::Grayscale, &[0]), ""),
            MediaFormat::Png
        );
        assert_eq!(sniff(&jpeg(8, 8, 1), "image/png"), MediaFormat::Jpeg);
        assert_eq!(sniff(b"GIF89a....", ""), MediaFormat::Other("GIF"));
        assert_eq!(
            sniff(b"RIFF\0\0\0\0WEBPVP8 ", ""),
            MediaFormat::Other("WebP")
        );
        assert_eq!(
            sniff(&[0xD7, 0xCD, 0xC6, 0x9A, 0, 0], ""),
            MediaFormat::Other("WMF")
        );
        let mut emf = vec![1u8, 0, 0, 0];
        emf.resize(40, 0);
        emf.extend_from_slice(b" EMF");
        assert_eq!(sniff(&emf, ""), MediaFormat::Other("EMF"));
        assert_eq!(sniff(b"\0\0\0\0", "image/x-wmf"), MediaFormat::Other("WMF"));
        assert_eq!(sniff(b"<svg", "image/svg+xml"), MediaFormat::Other("SVG"));
        assert_eq!(sniff(b"", ""), MediaFormat::Other("unknown"));
    }

    #[test]
    fn jpeg_frame_header_is_read_without_decoding() {
        let j = jpeg(37, 21, 3);
        let f = parse_jpeg_frame(&j).expect("frame");
        assert_eq!(
            (f.width, f.height, f.components, f.precision),
            (37, 21, 3, 8)
        );
        assert_eq!(f.marker, 0xC0);
        let p = prepare_image(&j, "image/jpeg", AlphaMode::FlattenOnWhite, false).unwrap();
        assert_eq!(p.encoding, ImageEncoding::Dct);
        assert_eq!(p.color, ImageColor::Rgb);
        assert_eq!(p.data, j, "DCT passthrough is the file's exact bytes");
        assert!(p.alpha.is_none());
    }

    #[test]
    fn jpeg_cmyk_is_profile_gated() {
        let j = jpeg(8, 8, 4);
        let plain = prepare_image(&j, "", AlphaMode::SoftMask, true).unwrap();
        assert_eq!(plain.color, ImageColor::Cmyk);
        assert!(!plain.invert_cmyk, "no APP14 Adobe segment");
        assert!(matches!(
            prepare_image(&j, "", AlphaMode::FlattenOnWhite, false),
            Err(ImageSkipReason::UnsupportedEncoding { .. })
        ));
    }

    #[test]
    fn corrupt_and_truncated_jpegs_are_typed_skips() {
        let j = jpeg(16, 16, 3);
        for cut in [2usize, 3, 5, 20, 30] {
            assert!(
                matches!(
                    parse_jpeg_frame(&j[..cut]),
                    Err(ImageSkipReason::Malformed { .. })
                ),
                "cut at {cut}"
            );
        }
        /* Progressive-arith (SOF10) is not DCTDecode-able. */
        let mut arith = j.clone();
        let sof = arith.windows(2).position(|w| w == [0xFF, 0xC0]).unwrap();
        arith[sof + 1] = 0xCA;
        assert!(matches!(
            prepare_image(&arith, "", AlphaMode::SoftMask, true),
            Err(ImageSkipReason::UnsupportedEncoding { .. })
        ));
    }

    #[test]
    fn png_rgba_keeps_alpha_as_soft_mask_or_flattens_on_white() {
        /* 2×1: opaque red, half-transparent blue. */
        let px = [255, 0, 0, 255, 0, 0, 255, 128];
        let data = make_png(2, 1, png::ColorType::Rgba, &px);
        let soft = prepare_image(&data, "image/png", AlphaMode::SoftMask, false).unwrap();
        assert_eq!(
            (soft.width, soft.height, soft.color),
            (2, 1, ImageColor::Rgb)
        );
        assert_eq!(soft.encoding, ImageEncoding::Raw);
        assert_eq!(soft.data, vec![255, 0, 0, 0, 0, 255]);
        assert_eq!(soft.alpha, Some(vec![255, 128]));
        let flat = prepare_image(&data, "image/png", AlphaMode::FlattenOnWhite, false).unwrap();
        assert!(flat.alpha.is_none());
        /* 0·128/255 + 255·127/255 ≈ 127; 255 stays 255. */
        assert_eq!(flat.data, vec![255, 0, 0, 127, 127, 255]);
    }

    #[test]
    fn png_opaque_alpha_drops_the_soft_mask() {
        let data = make_png(1, 1, png::ColorType::Rgba, &[1, 2, 3, 255]);
        let p = prepare_image(&data, "", AlphaMode::SoftMask, false).unwrap();
        assert!(p.alpha.is_none());
        assert_eq!(p.data, vec![1, 2, 3]);
    }

    #[test]
    fn png_gray_alpha_and_palette_normalize() {
        let ga = make_png(1, 1, png::ColorType::GrayscaleAlpha, &[10, 0]);
        let p = prepare_image(&ga, "", AlphaMode::SoftMask, false).unwrap();
        assert_eq!(p.color, ImageColor::Gray);
        assert_eq!(p.data, vec![10]);
        assert_eq!(p.alpha, Some(vec![0]));
        let flat = prepare_image(&ga, "", AlphaMode::FlattenOnWhite, false).unwrap();
        assert_eq!(flat.data, vec![255], "fully transparent → white");

        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, 2, 1);
            enc.set_color(png::ColorType::Indexed);
            enc.set_depth(png::BitDepth::Eight);
            enc.set_palette(vec![9, 8, 7, 1, 2, 3]);
            let mut w = enc.write_header().unwrap();
            w.write_image_data(&[1, 0]).unwrap();
        }
        let p = prepare_image(&out, "", AlphaMode::SoftMask, false).unwrap();
        assert_eq!(p.color, ImageColor::Rgb);
        assert_eq!(p.data, vec![1, 2, 3, 9, 8, 7]);
    }

    #[test]
    fn corrupt_png_is_a_typed_skip() {
        let data = make_png(4, 4, png::ColorType::Rgb, &[7u8; 48]);
        for cut in [8usize, 20, data.len() - 14] {
            assert!(
                matches!(
                    prepare_image(&data[..cut], "", AlphaMode::SoftMask, false),
                    Err(ImageSkipReason::Malformed { .. })
                ),
                "cut at {cut}"
            );
        }
    }

    #[test]
    fn tier3_formats_are_typed_skips() {
        for (bytes, fmt) in [
            (&b"GIF89a\x01\x00\x01\x00"[..], "GIF"),
            (&b"RIFF\0\0\0\0WEBPVP8 "[..], "WebP"),
            (&[0xD7, 0xCD, 0xC6, 0x9A, 0, 0][..], "WMF"),
        ] {
            assert_eq!(
                prepare_image(bytes, "", AlphaMode::SoftMask, true),
                Err(ImageSkipReason::UnsupportedFormat { format: fmt })
            );
        }
    }

    #[test]
    fn flatten_on_white_endpoints() {
        assert_eq!(flatten_on_white(0, 0), 255);
        assert_eq!(flatten_on_white(0, 255), 0);
        assert_eq!(flatten_on_white(200, 255), 200);
    }
}
