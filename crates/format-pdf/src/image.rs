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
//! - **GIF** (issue #189, `gif` feature) is decoded with the `gif` crate:
//!   the *first frame* only (what a static print shows; later animation
//!   frames are ignored), palette expanded to RGB, the transparent index
//!   becoming alpha 0, composited at its frame offset onto a transparent
//!   logical-screen canvas — the same picture `createImageBitmap` paints.
//! - **WebP** (issue #189, `webp` feature) is decoded with `image-webp`:
//!   lossy (VP8, with an optional `ALPH` plane) and lossless (VP8L); for an
//!   animated file, the first frame.
//! - **BMP** (issue #207, always on — hand-written, no crate) decodes
//!   uncompressed 24-bit (BGR) and 32-bit (BGRX, the 4th byte ignored —
//!   `BI_RGB` has no real alpha there), 8-bit palette (`BI_RGB` or the
//!   classic `BI_RLE8`). Every other DIB variant (OS/2 core headers, 1/2/4/
//!   16-bit depths, `BI_BITFIELDS`/`BI_RLE4`/JPEG/PNG-in-BMP compression) is
//!   a typed [`ImageSkipReason::UnsupportedEncoding`].
//! - **TIFF** (issue #207, `tiff` feature) decodes 8-bit Gray/GrayA/RGB/
//!   RGBA via the `tiff` crate (whatever strip compression it used —
//!   uncompressed, LZW, PackBits, Deflate — the crate handles internally),
//!   plus CMYK under the same `allow_cmyk` profile gate as JPEG. Palette,
//!   YCbCr and non-8-bit samples are a typed `UnsupportedEncoding` (rare in
//!   Word media; the crate's decoder gives no ready path to expand a
//!   palette without hand-rolling that ourselves, like the BMP one).
//! - Decoded GIF / WebP / BMP / TIFF pixels take exactly the PNG path from
//!   there: raw RGB `/FlateDecode` samples plus an `/SMask` or a flatten
//!   onto white ([`AlphaMode`]). With a decoder feature off, that format
//!   falls back to the typed [`ImageSkipReason::UnsupportedFormat`] skip.
//! - **EMF / WMF / SVG / unknown** are typed skips
//!   ([`ImageSkipReason::UnsupportedFormat`]) — never a panic.
//!
//! Every failure is an [`ImageSkipReason`]; the exporter turns it into a
//! [`crate::PdfWarning`] and leaves the image's rect empty.
//!
//! ## Allocation bounds (issue #208)
//!
//! [`MAX_IMAGE_PIXELS`] (40 MP ≈ 160 MiB of RGBA) is checked against the
//! *declared* dimensions before any pixel buffer is allocated, for every
//! format. That is enough on its own for PNG, GIF and BMP, but not always
//! for WebP — a source audit of each vendored decoder found:
//!
//! - **PNG** (`png` 0.18): the crate's own [`png::Limits`] (default 64 MiB)
//!   bounds its *internal* scratch (the unfiltering buffer, at most a
//!   couple of rows wide) independently of our dimension check — pinned
//!   explicitly in [`prepare_png`] rather than left as an implicit default.
//!   The *output* buffer we allocate ourselves is sized from
//!   `reader.output_buffer_size()`, itself a function of the
//!   already-`MAX_IMAGE_PIXELS`-checked width/height.
//! - **GIF** (`gif` 0.14): `gif::MemoryLimit::Bytes` is a real, verified
//!   bound — every internal buffer grows through the crate's own
//!   `try_reserve`, which checks the limit before each reservation (source:
//!   `reader/mod.rs`). We set it to `MAX_IMAGE_PIXELS * 4` bytes. The LZW
//!   dictionary (`weezl`) is separately bounded by the GIF format itself
//!   (a hard 12-bit code / 4096-entry ceiling), not adversarially growable.
//! - **WebP** (`image-webp` 0.2, issue #208's original prompt): a source
//!   audit found `WebPDecoder::set_memory_limit` is **only** consulted for
//!   the optional ICC/EXIF/XMP metadata chunk reads (`decoder.rs`) — it is
//!   *never* checked before the pixel-buffer allocations (`rgb_frame` /
//!   `rgba_frame` / `canvas`), so our call to it below is inert for pixel
//!   data. The real bound for those buffers is transitively our own
//!   `check_dimensions` pre-check: `read_image` requires `buf.len() ==
//!   output_buffer_size()`, itself `width * height * {3,4}`, and every
//!   animation frame is bounds-checked against that same already-capped
//!   canvas. Two internal allocations remain **unbounded by anything we can
//!   reach from this crate** (confirmed by reading `vp8.rs` / `lossless.rs`
//!   — both permissively licensed, not the clean-room-walled reference
//!   trees):
//!   - `vp8::init_partitions` reads a 3-byte (≤ 16 MiB) partition size
//!     *before* validating the reader has that many bytes left — a lying
//!     header can force one ~16 MiB transient allocation from a tiny file
//!     before the subsequent `read_exact` fails and unwinds it. Bounded
//!     (≤ 8 partitions × ~16 MiB ≈ 128 MiB structurally, since the size
//!     field is only 3 bytes wide) but not zero.
//!   - VP8L's meta-Huffman path can set `num_huff_groups` up to 65536 from
//!     a *tiny* meta-image, then allocates one `HuffmanCodeGroup` (5 trees)
//!     per group — but building each group consumes real, non-trivial
//!     bitstream bits (a `read_huffman_code` call per tree), so reaching
//!     the max is not free; it costs the attacker roughly proportional
//!     input, not proportional output.
//!
//!   Patching either needs a fork of `image-webp`, out of scope for this
//!   task (a discovered gap, reported rather than fixed here — see the
//!   task's final report). Both are well inside the 2 GiB wasm heap
//!   ceiling, and the worker's crash-recovery path (issue #85) makes a
//!   trap non-destructive, so this is a documented residual risk, not a
//!   blocking one.
//! - **BMP** (hand-written): the only buffers are ones we allocate
//!   ourselves, all sized from the already-`check_dimensions`-checked
//!   width/height (`MAX_IMAGE_PIXELS * 3` bytes for the RGB output,
//!   `MAX_IMAGE_PIXELS` bytes for the RLE8 index scratch) — no vendored
//!   code, no hidden amplification.
//! - **TIFF** (`tiff` 0.9): [`tiff::decoder::Limits`] defaults are looser
//!   than we need (256 MiB decode buffer, 128 MiB intermediate, 1 MiB per
//!   IFD tag value) — [`prepare_tiff`] tightens `decoding_buffer_size` /
//!   `intermediate_buffer_size` to `MAX_IMAGE_PIXELS * 4` (the widest
//!   sample layout we accept, RGBA8/CMYK8) and `ifd_value_size` to 64 KiB
//!   (bounds a single forged tag's value array, e.g. `StripByteCounts`).

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
    /// A format the pure-Rust pipeline does not handle (Tier 3): EMF, WMF,
    /// BMP, TIFF, SVG or unrecognized bytes — and GIF / WebP when the
    /// crate's `gif` / `webp` decoder feature is off.
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
    Gif,
    WebP,
    Bmp,
    Tiff,
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
        return MediaFormat::Gif;
    }
    if data.len() >= 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        return MediaFormat::WebP;
    }
    if data.len() >= 44 && data[0..4] == [1, 0, 0, 0] && &data[40..44] == b" EMF" {
        return MediaFormat::Other("EMF");
    }
    if data.starts_with(&[0xD7, 0xCD, 0xC6, 0x9A]) {
        return MediaFormat::Other("WMF");
    }
    if data.starts_with(b"BM") {
        return MediaFormat::Bmp;
    }
    if data.starts_with(b"II*\0") || data.starts_with(b"MM\0*") {
        return MediaFormat::Tiff;
    }
    let ct = content_type.to_ascii_lowercase();
    let by_type = match ct.as_str() {
        "image/x-wmf" | "image/wmf" => "WMF",
        "image/x-emf" | "image/emf" => "EMF",
        "image/svg+xml" => "SVG",
        /* Declared GIF / WebP / BMP / TIFF without the magic: the decoder
        reports the corruption as `Malformed`, matching the format the
        relationship claims rather than a generic "unknown". */
        "image/gif" => return MediaFormat::Gif,
        "image/webp" => return MediaFormat::WebP,
        "image/bmp" | "image/x-bmp" | "image/x-ms-bmp" => return MediaFormat::Bmp,
        "image/tiff" | "image/tiff-fx" => return MediaFormat::Tiff,
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
        #[cfg(feature = "gif")]
        MediaFormat::Gif => prepare_gif(data, alpha),
        #[cfg(not(feature = "gif"))]
        MediaFormat::Gif => Err(ImageSkipReason::UnsupportedFormat { format: "GIF" }),
        #[cfg(feature = "webp")]
        MediaFormat::WebP => prepare_webp(data, alpha),
        #[cfg(not(feature = "webp"))]
        MediaFormat::WebP => Err(ImageSkipReason::UnsupportedFormat { format: "WebP" }),
        MediaFormat::Bmp => prepare_bmp(data, alpha),
        #[cfg(feature = "tiff")]
        MediaFormat::Tiff => prepare_tiff(data, alpha, allow_cmyk),
        #[cfg(not(feature = "tiff"))]
        MediaFormat::Tiff => Err(ImageSkipReason::UnsupportedFormat { format: "TIFF" }),
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
    /* Issue #208 — pin the crate's own internal-scratch budget explicitly
    rather than rely on its (currently identical) 64 MiB default: bounds the
    unfiltering buffer independently of the `MAX_IMAGE_PIXELS` check below,
    which only gates the *output* buffer we allocate ourselves. */
    decoder.set_limits(png::Limits {
        bytes: 64 * 1024 * 1024,
    });
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
    let layout = PixelLayout {
        width,
        height,
        color,
        color_channels,
        has_alpha,
        /* Rows are tightly packed at 8 bits (`line_size` = width × stride). */
        row_bytes: info.line_size,
    };
    split_samples(layout, &buf, alpha_mode)
}

/// Shape of a decoded 8-bit, row-major, interleaved pixel buffer.
#[derive(Debug, Clone, Copy)]
struct PixelLayout {
    width: u32,
    height: u32,
    color: ImageColor,
    /// 1 (gray) or 3 (RGB) colour samples per pixel.
    color_channels: usize,
    /// One trailing alpha sample per pixel.
    has_alpha: bool,
    /// Bytes from one row's start to the next (≥ width × stride).
    row_bytes: usize,
}

/// Split a decoded interleaved buffer into PDF colour samples plus an
/// optional `/SMask` plane, or flatten the alpha onto white — shared by the
/// PNG, GIF and WebP paths. A buffer shorter than the layout claims is a
/// [`ImageSkipReason::Malformed`], never an out-of-bounds panic.
fn split_samples(
    layout: PixelLayout,
    buf: &[u8],
    alpha_mode: AlphaMode,
) -> Result<PreparedImage, ImageSkipReason> {
    let PixelLayout {
        width,
        height,
        color,
        color_channels,
        has_alpha,
        row_bytes,
    } = layout;
    let pixels = width as usize * height as usize;
    let stride = color_channels + usize::from(has_alpha);
    let line_len = width as usize * stride;
    let mut samples = Vec::with_capacity(pixels * color_channels);
    let mut alpha = if has_alpha {
        Some(Vec::with_capacity(pixels))
    } else {
        None
    };
    for y in 0..height as usize {
        let line = buf
            .get(y * row_bytes..y * row_bytes + line_len)
            .ok_or_else(|| malformed("decoded pixel buffer shorter than the image"))?;
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

/// Reject a zero or over-budget canvas before anything is allocated.
fn check_dimensions(format: &str, width: u32, height: u32) -> Result<(), ImageSkipReason> {
    if width == 0 || height == 0 {
        return Err(malformed(&format!("{format}: zero dimension")));
    }
    if u64::from(width) * u64::from(height) > MAX_IMAGE_PIXELS {
        return Err(ImageSkipReason::TooLarge { width, height });
    }
    Ok(())
}

/// GIF → first frame, RGBA. The frame is composited at its `(left, top)`
/// offset onto a fully transparent canvas the size of the logical screen
/// (or of the frame's extent when the screen descriptor says 0×0), clipped
/// to it; the transparent colour index decodes to alpha 0.
#[cfg(feature = "gif")]
fn prepare_gif(data: &[u8], alpha_mode: AlphaMode) -> Result<PreparedImage, ImageSkipReason> {
    let mut opts = gif::DecodeOptions::new();
    opts.set_color_output(gif::ColorOutput::RGBA);
    /* Bounds the frame buffer the decoder allocates (RGBA = 4 B/px). */
    if let Some(limit) = std::num::NonZeroU64::new(MAX_IMAGE_PIXELS * 4) {
        opts.set_memory_limit(gif::MemoryLimit::Bytes(limit));
    }
    let mut decoder = opts
        .read_info(Cursor::new(data))
        .map_err(|e| malformed(&format!("GIF header: {e}")))?;
    let (screen_w, screen_h) = (u32::from(decoder.width()), u32::from(decoder.height()));
    if screen_w != 0 && screen_h != 0 {
        check_dimensions("GIF", screen_w, screen_h)?;
    }
    let frame = decoder
        .read_next_frame()
        .map_err(|e| malformed(&format!("GIF data: {e}")))?
        .ok_or_else(|| malformed("GIF: no image frame"))?;
    let (left, top) = (usize::from(frame.left), usize::from(frame.top));
    let (fw, fh) = (usize::from(frame.width), usize::from(frame.height));
    let (width, height) = if screen_w == 0 || screen_h == 0 {
        ((left + fw) as u32, (top + fh) as u32)
    } else {
        (screen_w, screen_h)
    };
    check_dimensions("GIF", width, height)?;
    let (cw, ch) = (width as usize, height as usize);
    let mut canvas = vec![0u8; cw * ch * 4];
    if fw > 0 && left < cw {
        let n = fw.min(cw - left) * 4;
        for (y, row) in frame.buffer.chunks_exact(fw * 4).take(fh).enumerate() {
            let cy = top + y;
            if cy >= ch {
                break;
            }
            let dst = (cy * cw + left) * 4;
            canvas[dst..dst + n].copy_from_slice(&row[..n]);
        }
    }
    let layout = PixelLayout {
        width,
        height,
        color: ImageColor::Rgb,
        color_channels: 3,
        has_alpha: true,
        row_bytes: cw * 4,
    };
    split_samples(layout, &canvas, alpha_mode)
}

/// WebP → RGB or RGBA via `image-webp` (lossy VP8 + `ALPH`, lossless VP8L,
/// first frame of an animation).
#[cfg(feature = "webp")]
fn prepare_webp(data: &[u8], alpha_mode: AlphaMode) -> Result<PreparedImage, ImageSkipReason> {
    let mut decoder = image_webp::WebPDecoder::new(Cursor::new(data))
        .map_err(|e| malformed(&format!("WebP header: {e}")))?;
    let (width, height) = decoder.dimensions();
    /* Issue #208 — `check_dimensions` above is the bound that actually
    matters for the pixel buffers below: a source audit found
    `set_memory_limit` is only consulted for the optional ICC/EXIF/XMP
    metadata chunk reads in this crate version, never before `rgb_frame` /
    `rgba_frame` / `canvas`. Set anyway (harmless, and correct if a future
    `image-webp` release wires it up) — see the module doc's allocation-
    bounds section for the two internal allocations this crate does not let
    us bound at all (VP8 partition sizes, VP8L meta-Huffman groups). */
    check_dimensions("WebP", width, height)?;
    decoder.set_memory_limit((MAX_IMAGE_PIXELS * 4) as usize);
    let has_alpha = decoder.has_alpha();
    let size = decoder
        .output_buffer_size()
        .ok_or(ImageSkipReason::TooLarge { width, height })?;
    let mut buf = vec![0u8; size];
    decoder
        .read_image(&mut buf)
        .map_err(|e| malformed(&format!("WebP data: {e}")))?;
    let stride = if has_alpha { 4 } else { 3 };
    let layout = PixelLayout {
        width,
        height,
        color: ImageColor::Rgb,
        color_channels: 3,
        has_alpha,
        row_bytes: width as usize * stride,
    };
    split_samples(layout, &buf, alpha_mode)
}

/* ---- Issue #207: BMP (hand-written, no crate) ------------------------ */

fn read_u16_le(data: &[u8], at: usize) -> Option<u16> {
    data.get(at..at + 2)
        .map(|s| u16::from_le_bytes([s[0], s[1]]))
}

fn read_u32_le(data: &[u8], at: usize) -> Option<u32> {
    data.get(at..at + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn read_i32_le(data: &[u8], at: usize) -> Option<i32> {
    read_u32_le(data, at).map(|v| v as i32)
}

/// Bytes from one row's start to the next: bit-packed rows are padded up to
/// a 4-byte boundary (the BMP spec's `DWORD` alignment).
fn bmp_row_stride(width: u32, bits_per_pixel: u32) -> Result<usize, ImageSkipReason> {
    let bits = u64::from(width)
        .checked_mul(u64::from(bits_per_pixel))
        .ok_or_else(|| malformed("BMP: row width overflow"))?;
    let stride = bits.div_ceil(8).div_ceil(4).saturating_mul(4);
    usize::try_from(stride).map_err(|_| malformed("BMP: row stride too large"))
}

/// Read `colors_used` (or `1 << bpp` when it's the BMP convention's 0-means-
/// "all of them") palette entries starting at `start`: 4 bytes each, Blue-
/// Green-Red-Reserved, per the DIB `RGBQUAD` layout.
fn read_bmp_palette(
    data: &[u8],
    start: usize,
    colors_used: u32,
    bpp: u32,
) -> Result<Vec<[u8; 3]>, ImageSkipReason> {
    let max_colors = 1u32 << bpp;
    let count = if colors_used == 0 || colors_used > max_colors {
        max_colors
    } else {
        colors_used
    } as usize;
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let off = start
            .checked_add(i * 4)
            .ok_or_else(|| malformed("BMP: palette offset overflow"))?;
        let entry = data
            .get(off..off + 4)
            .ok_or_else(|| malformed("BMP: truncated palette"))?;
        out.push([entry[2], entry[1], entry[0]]);
    }
    Ok(out)
}

/// Uncompressed `BI_RGB`: 24bpp (BGR), 32bpp (BGRX, 4th byte ignored) or
/// 8bpp indexed (a palette entry per pixel). Rows are read bottom-up unless
/// `top_down` (a negative `biHeight`), and padded to the DWORD boundary.
#[allow(clippy::too_many_arguments)] // one row-decode path for all three BI_RGB depths
fn decode_bmp_uncompressed(
    data: &[u8],
    width: u32,
    height: u32,
    top_down: bool,
    pixel_data_start: usize,
    bytes_per_pixel: usize,
    palette: Option<&[[u8; 3]]>,
    alpha_mode: AlphaMode,
) -> Result<PreparedImage, ImageSkipReason> {
    let row_stride = bmp_row_stride(width, (bytes_per_pixel * 8) as u32)?;
    let (w, h) = (width as usize, height as usize);
    let mut out = vec![0u8; w * h * 3];
    for y_out in 0..h {
        let src_row = if top_down { y_out } else { h - 1 - y_out };
        let row_start = pixel_data_start
            .checked_add(
                src_row
                    .checked_mul(row_stride)
                    .ok_or_else(|| malformed("BMP: row offset overflow"))?,
            )
            .ok_or_else(|| malformed("BMP: row offset overflow"))?;
        let row = data
            .get(row_start..row_start + row_stride)
            .ok_or_else(|| malformed("BMP: pixel data truncated"))?;
        let dst = &mut out[y_out * w * 3..(y_out + 1) * w * 3];
        for x in 0..w {
            let px = row
                .get(x * bytes_per_pixel..x * bytes_per_pixel + bytes_per_pixel)
                .ok_or_else(|| malformed("BMP: row shorter than its declared width"))?;
            let rgb = match palette {
                Some(pal) => pal.get(px[0] as usize).copied().unwrap_or([0, 0, 0]),
                None => [px[2], px[1], px[0]],
            };
            dst[x * 3..x * 3 + 3].copy_from_slice(&rgb);
        }
    }
    split_samples(
        PixelLayout {
            width,
            height,
            color: ImageColor::Rgb,
            color_channels: 3,
            has_alpha: false,
            row_bytes: w * 3,
        },
        &out,
        alpha_mode,
    )
}

/// The classic 8-bit RLE (`BI_RLE8`): pairs of (count, index) runs plus the
/// escape codes (0,0) end-of-line, (0,1) end-of-bitmap, (0,2) delta, (0,N≥3)
/// an N-byte literal run padded to a word. Always bottom-up per the spec
/// (the `top_down` / negative-height combination is invalid for RLE and not
/// specially handled — this just decodes bottom-up regardless). Every
/// branch of the loop advances the input cursor by at least one byte, and a
/// `max_steps` bound (proportional to the input length) is kept anyway as
/// defense in depth — never trust "this can't loop forever" alone.
fn decode_bmp_rle8(
    data: &[u8],
    width: u32,
    height: u32,
    pixel_data_start: usize,
    palette: &[[u8; 3]],
    alpha_mode: AlphaMode,
) -> Result<PreparedImage, ImageSkipReason> {
    let (w, h) = (width as usize, height as usize);
    let input = data
        .get(pixel_data_start..)
        .ok_or_else(|| malformed("BMP: RLE data offset beyond the file"))?;
    let mut idx = vec![0u8; w * h];
    let mut pos = 0usize;
    let mut x = 0usize;
    let mut row = 0usize; // 0 = bottom row (the RLE stream's origin).
    let max_steps = input.len().saturating_mul(2).saturating_add(64);
    let mut steps = 0usize;
    loop {
        steps += 1;
        if steps > max_steps {
            return Err(malformed("BMP: RLE stream did not terminate"));
        }
        let (Some(&b0), Some(&b1)) = (input.get(pos), input.get(pos + 1)) else {
            // Truncated mid-stream with no explicit end-of-bitmap marker:
            // keep whatever full rows were already decoded rather than
            // fail the whole image over a missing terminator.
            break;
        };
        pos += 2;
        if b0 > 0 {
            if row < h {
                let out_row = h - 1 - row;
                let n = (b0 as usize).min(w.saturating_sub(x));
                let dst = out_row * w + x;
                if let Some(s) = idx.get_mut(dst..dst + n) {
                    s.fill(b1);
                }
            }
            x += b0 as usize;
        } else {
            match b1 {
                0 => {
                    row += 1;
                    x = 0;
                }
                1 => break,
                2 => {
                    let (Some(&dx), Some(&dy)) = (input.get(pos), input.get(pos + 1)) else {
                        break;
                    };
                    pos += 2;
                    x += dx as usize;
                    row += dy as usize;
                }
                n => {
                    let count = n as usize;
                    let Some(bytes) = input.get(pos..pos + count) else {
                        break;
                    };
                    if row < h {
                        let out_row = h - 1 - row;
                        let take = count.min(w.saturating_sub(x));
                        let dst = out_row * w + x;
                        if let Some(d) = idx.get_mut(dst..dst + take) {
                            d.copy_from_slice(&bytes[..take]);
                        }
                    }
                    pos += count;
                    if count % 2 == 1 {
                        pos += 1;
                    }
                    x += count;
                }
            }
        }
    }
    let mut rgb = vec![0u8; w * h * 3];
    for (i, &pi) in idx.iter().enumerate() {
        let c = palette.get(pi as usize).copied().unwrap_or([0, 0, 0]);
        rgb[i * 3..i * 3 + 3].copy_from_slice(&c);
    }
    split_samples(
        PixelLayout {
            width,
            height,
            color: ImageColor::Rgb,
            color_channels: 3,
            has_alpha: false,
            row_bytes: w * 3,
        },
        &rgb,
        alpha_mode,
    )
}

/// BMP → RGB. Uncompressed 24/32-bit and 8-bit palette (`BI_RGB`), plus the
/// classic 8-bit RLE (`BI_RLE8`). Every other DIB header variant (OS/2 core
/// headers < the 40-byte `BITMAPINFOHEADER`, 1/2/4/16-bit depths,
/// `BI_BITFIELDS`/`BI_RLE4`/JPEG/PNG-in-BMP compression) is a typed
/// [`ImageSkipReason::UnsupportedEncoding`] — legacy or rare in Word media,
/// not worth a general bit-field decoder. Every read is bounds-checked
/// (`.get()`, checked arithmetic); truncated or corrupt input is
/// [`ImageSkipReason::Malformed`], never a panic.
fn prepare_bmp(data: &[u8], alpha_mode: AlphaMode) -> Result<PreparedImage, ImageSkipReason> {
    if data.len() < 18 || &data[0..2] != b"BM" {
        return Err(malformed("BMP: missing 'BM' magic"));
    }
    let pixel_data_start =
        read_u32_le(data, 10).ok_or_else(|| malformed("BMP: truncated file header"))? as usize;
    let header_size =
        read_u32_le(data, 14).ok_or_else(|| malformed("BMP: truncated DIB header size"))?;
    if header_size < 40 {
        return Err(ImageSkipReason::UnsupportedEncoding {
            detail: format!(
                "BMP DIB header size {header_size} (only BITMAPINFOHEADER-family ≥ 40 supported)"
            ),
        });
    }
    let width_i = read_i32_le(data, 18).ok_or_else(|| malformed("BMP: truncated width"))?;
    let height_i = read_i32_le(data, 22).ok_or_else(|| malformed("BMP: truncated height"))?;
    let bpp = read_u16_le(data, 28).ok_or_else(|| malformed("BMP: truncated bit depth"))?;
    let compression =
        read_u32_le(data, 30).ok_or_else(|| malformed("BMP: truncated compression"))?;
    let colors_used =
        read_u32_le(data, 46).ok_or_else(|| malformed("BMP: truncated colors-used"))?;

    if width_i <= 0 || height_i == 0 || height_i == i32::MIN {
        return Err(malformed(
            "BMP: non-positive width or unrepresentable height",
        ));
    }
    let top_down = height_i < 0;
    let width = width_i as u32;
    let height = height_i.unsigned_abs();
    check_dimensions("BMP", width, height)?;

    let palette_start = 14usize
        .checked_add(header_size as usize)
        .ok_or_else(|| malformed("BMP: header size overflow"))?;

    match (bpp, compression) {
        (24, 0) => decode_bmp_uncompressed(
            data,
            width,
            height,
            top_down,
            pixel_data_start,
            3,
            None,
            alpha_mode,
        ),
        (32, 0) => decode_bmp_uncompressed(
            data,
            width,
            height,
            top_down,
            pixel_data_start,
            4,
            None,
            alpha_mode,
        ),
        (8, 0) => {
            let palette = read_bmp_palette(data, palette_start, colors_used, 8)?;
            decode_bmp_uncompressed(
                data,
                width,
                height,
                top_down,
                pixel_data_start,
                1,
                Some(&palette),
                alpha_mode,
            )
        }
        (8, 1) => {
            let palette = read_bmp_palette(data, palette_start, colors_used, 8)?;
            decode_bmp_rle8(data, width, height, pixel_data_start, &palette, alpha_mode)
        }
        (bpp, comp) => Err(ImageSkipReason::UnsupportedEncoding {
            detail: format!("BMP {bpp}bpp compression {comp}"),
        }),
    }
}

/* ---- Issue #207: TIFF (`tiff` crate) ---------------------------------- */

/// TIFF → 8-bit Gray/GrayA/RGB/RGBA/CMYK via the `tiff` crate, whatever
/// strip compression it used internally (uncompressed, LZW, PackBits,
/// Deflate). Palette, YCbCr and non-8-bit samples are a typed
/// `UnsupportedEncoding` (see the module doc). CMYK is gated by
/// `allow_cmyk`, matching the JPEG path.
#[cfg(feature = "tiff")]
fn prepare_tiff(
    data: &[u8],
    alpha_mode: AlphaMode,
    allow_cmyk: bool,
) -> Result<PreparedImage, ImageSkipReason> {
    let decoder = tiff::decoder::Decoder::new(Cursor::new(data))
        .map_err(|e| malformed(&format!("TIFF header: {e}")))?;
    /* Issue #208 — tighten the crate's own internal-allocation ceilings
    (defaults: 256 MiB decode buffer / 128 MiB intermediate / 1 MiB per IFD
    tag value) to our own budget, independent of the `check_dimensions`
    call below: a lying header (bogus samples-per-pixel or bit depth we'd
    reject afterward anyway) can't force more than our own worst case
    before we ever see a pixel. */
    let mut limits = tiff::decoder::Limits::default();
    limits.decoding_buffer_size = (MAX_IMAGE_PIXELS * 4) as usize;
    limits.intermediate_buffer_size = limits.decoding_buffer_size;
    limits.ifd_value_size = 1 << 16;
    let mut decoder = decoder.with_limits(limits);

    let (width, height) = decoder
        .dimensions()
        .map_err(|e| malformed(&format!("TIFF dimensions: {e}")))?;
    check_dimensions("TIFF", width, height)?;
    let color = decoder
        .colortype()
        .map_err(|e| malformed(&format!("TIFF color type: {e}")))?;
    let (color_channels, has_alpha, out_color) = match color {
        tiff::ColorType::Gray(8) => (1, false, ImageColor::Gray),
        tiff::ColorType::GrayA(8) => (1, true, ImageColor::Gray),
        tiff::ColorType::RGB(8) => (3, false, ImageColor::Rgb),
        tiff::ColorType::RGBA(8) => (3, true, ImageColor::Rgb),
        tiff::ColorType::CMYK(8) if allow_cmyk => (4, false, ImageColor::Cmyk),
        tiff::ColorType::CMYK(8) => {
            return Err(ImageSkipReason::UnsupportedEncoding {
                detail: "CMYK TIFF under an sRGB output intent".to_string(),
            });
        }
        other => {
            return Err(ImageSkipReason::UnsupportedEncoding {
                detail: format!(
                    "TIFF color type {other:?} (only 8-bit Gray/GrayA/RGB/RGBA/CMYK decode)"
                ),
            });
        }
    };
    let result = decoder
        .read_image()
        .map_err(|e| malformed(&format!("TIFF data: {e}")))?;
    let tiff::decoder::DecodingResult::U8(buf) = result else {
        return Err(ImageSkipReason::UnsupportedEncoding {
            detail: "TIFF sample width other than 8 bits".to_string(),
        });
    };
    let stride = color_channels + usize::from(has_alpha);
    let layout = PixelLayout {
        width,
        height,
        color: out_color,
        color_channels,
        has_alpha,
        row_bytes: width as usize * stride,
    };
    split_samples(layout, &buf, alpha_mode)
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

    /// A GIF89a with one image frame of `indices` (row-major colour-table
    /// indices, `frame_w`×`frame_h`) at `(left, top)` on a
    /// `screen_w`×`screen_h` logical screen, `palette` (RGB triples) as the
    /// global colour table and `transparent` as the frame's transparent
    /// index. Encoded with the `gif` crate's own encoder (LZW) — tiny,
    /// deterministic, no blob in the tree.
    #[cfg(feature = "gif")]
    #[allow(clippy::too_many_arguments)]
    pub fn gif(
        screen_w: u16,
        screen_h: u16,
        palette: &[u8],
        left: u16,
        top: u16,
        frame_w: u16,
        frame_h: u16,
        indices: &[u8],
        transparent: Option<u8>,
    ) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc =
                ::gif::Encoder::new(&mut out, screen_w, screen_h, palette).expect("gif header");
            let frame = ::gif::Frame {
                left,
                top,
                width: frame_w,
                height: frame_h,
                transparent,
                buffer: std::borrow::Cow::Borrowed(indices),
                ..::gif::Frame::default()
            };
            enc.write_frame(&frame).expect("gif frame");
        }
        out
    }

    /// A lossless (VP8L) WebP of 8-bit RGBA pixels, encoded with
    /// `image-webp`'s own encoder.
    #[cfg(feature = "webp")]
    pub fn webp_lossless_rgba(width: u32, height: u32, pixels: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        image_webp::WebPEncoder::new(&mut out)
            .encode(pixels, width, height, image_webp::ColorType::Rgba8)
            .expect("webp encode");
        out
    }

    /// A 4×4 lossy (simple-format `VP8 `) WebP of a flat (200, 40, 40) —
    /// 72 bytes. `image-webp` has no lossy encoder, so these bytes were
    /// produced once by libwebp (`quality=100`) and are spelled out here;
    /// decoders reproduce the colour to within YUV 4:2:0 rounding.
    pub const WEBP_LOSSY_4X4_RED: [u8; 72] = [
        0x52, 0x49, 0x46, 0x46, 0x40, 0x00, 0x00, 0x00, 0x57, 0x45, 0x42, 0x50, 0x56, 0x50, 0x38,
        0x20, 0x34, 0x00, 0x00, 0x00, 0x10, 0x02, 0x00, 0x9d, 0x01, 0x2a, 0x04, 0x00, 0x04, 0x00,
        0x00, 0x00, 0x00, 0x25, 0xa0, 0x02, 0x74, 0xba, 0x01, 0xf8, 0x01, 0xfa, 0x00, 0x03, 0xc8,
        0x00, 0xfe, 0xfe, 0xeb, 0xbc, 0xbf, 0xfa, 0x8d, 0x5f, 0xaa, 0x03, 0x7f, 0xfd, 0x46, 0xcf,
        0xff, 0xd8, 0x6c, 0x3c, 0x1f, 0x89, 0x03, 0xff, 0xec, 0x1f, 0x00, 0x00,
    ];

    /// A 4×4 lossy WebP with an alpha plane (extended format: `VP8X` +
    /// `ALPH` + `VP8 `) — flat (40, 40, 200); rows 0–1 opaque, rows 2–3
    /// fully transparent. 112 bytes, produced once by libwebp
    /// (`quality=100`, `alpha_quality=100`, `exact`).
    pub const WEBP_LOSSY_ALPHA_4X4_BLUE: [u8; 112] = [
        0x52, 0x49, 0x46, 0x46, 0x68, 0x00, 0x00, 0x00, 0x57, 0x45, 0x42, 0x50, 0x56, 0x50, 0x38,
        0x58, 0x0a, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x03, 0x00, 0x00,
        0x41, 0x4c, 0x50, 0x48, 0x0c, 0x00, 0x00, 0x00, 0x01, 0x10, 0x0b, 0x26, 0xf9, 0x4b, 0x77,
        0xcd, 0x21, 0x22, 0x72, 0x02, 0x56, 0x50, 0x38, 0x20, 0x36, 0x00, 0x00, 0x00, 0x10, 0x02,
        0x00, 0x9d, 0x01, 0x2a, 0x04, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x25, 0xa0, 0x02, 0x74,
        0xba, 0x01, 0xf8, 0x01, 0xf8, 0x00, 0x03, 0xc8, 0x00, 0xfe, 0xff, 0xb9, 0x03, 0x2f, 0xff,
        0xd8, 0x6c, 0x7f, 0xb0, 0xd8, 0xff, 0x61, 0xb1, 0xff, 0xec, 0x36, 0x3f, 0xfa, 0xca, 0xaf,
        0x92, 0xa3, 0xf6, 0xcc, 0x00, 0x00, 0x00,
    ];

    /// A minimal uncompressed BMP: `BITMAPFILEHEADER` + 40-byte
    /// `BITMAPINFOHEADER` (+ a palette, for `bytes_per_pixel == 1`) + row-
    /// padded pixel data. `pixels` is row-major, top-to-bottom, tightly
    /// packed at `bytes_per_pixel` bytes/pixel (BGR for 3, BGRX for 4, a
    /// palette index for 1); this builder adds the DWORD row padding and
    /// writes rows bottom-up in the file (the BMP convention) unless
    /// `top_down`, matching what `prepare_bmp` expects to invert.
    #[allow(clippy::too_many_arguments)]
    pub fn bmp(
        width: u32,
        height: u32,
        bpp: u16,
        bytes_per_pixel: usize,
        palette: Option<&[[u8; 3]]>,
        pixels: &[u8],
        top_down: bool,
    ) -> Vec<u8> {
        let row_stride = (u64::from(width) * u64::from(bpp)).div_ceil(8).div_ceil(4) as usize * 4;
        let palette_bytes = palette.map_or(0, |p| p.len() * 4);
        let pixel_data_start = 14 + 40 + palette_bytes;
        let file_size = pixel_data_start + row_stride * height as usize;
        let mut out = Vec::with_capacity(file_size);
        out.extend_from_slice(b"BM");
        out.extend_from_slice(&(file_size as u32).to_le_bytes());
        out.extend_from_slice(&[0, 0, 0, 0]);
        out.extend_from_slice(&(pixel_data_start as u32).to_le_bytes());
        out.extend_from_slice(&40u32.to_le_bytes());
        out.extend_from_slice(&(width as i32).to_le_bytes());
        let signed_height: i32 = if top_down {
            -(height as i32)
        } else {
            height as i32
        };
        out.extend_from_slice(&signed_height.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&bpp.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0i32.to_le_bytes());
        out.extend_from_slice(&0i32.to_le_bytes());
        let colors_used = palette.map_or(0, |p| p.len() as u32);
        out.extend_from_slice(&colors_used.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        if let Some(pal) = palette {
            for c in pal {
                out.extend_from_slice(&[c[2], c[1], c[0], 0]);
            }
        }
        for y in 0..height as usize {
            let src_row = if top_down { y } else { height as usize - 1 - y };
            let start = src_row * width as usize * bytes_per_pixel;
            let row = &pixels[start..start + width as usize * bytes_per_pixel];
            out.extend_from_slice(row);
            let pad = out.len() + (row_stride - row.len());
            out.resize(pad, 0);
        }
        out
    }

    /// A BMP using the classic 8-bit RLE (`BI_RLE8`): `rle_data` is the
    /// already-encoded opcode stream, written verbatim as the pixel data.
    pub fn bmp_rle8(width: u32, height: u32, palette: &[[u8; 3]], rle_data: &[u8]) -> Vec<u8> {
        let pixel_data_start = 14 + 40 + palette.len() * 4;
        let file_size = pixel_data_start + rle_data.len();
        let mut out = Vec::with_capacity(file_size);
        out.extend_from_slice(b"BM");
        out.extend_from_slice(&(file_size as u32).to_le_bytes());
        out.extend_from_slice(&[0, 0, 0, 0]);
        out.extend_from_slice(&(pixel_data_start as u32).to_le_bytes());
        out.extend_from_slice(&40u32.to_le_bytes());
        out.extend_from_slice(&(width as i32).to_le_bytes());
        out.extend_from_slice(&(height as i32).to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&8u16.to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes()); // BI_RLE8
        out.extend_from_slice(&(rle_data.len() as u32).to_le_bytes());
        out.extend_from_slice(&0i32.to_le_bytes());
        out.extend_from_slice(&0i32.to_le_bytes());
        out.extend_from_slice(&(palette.len() as u32).to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        for c in palette {
            out.extend_from_slice(&[c[2], c[1], c[0], 0]);
        }
        out.extend_from_slice(rle_data);
        out
    }

    /// A minimal single-IFD TIFF via the `tiff` crate's own encoder —
    /// uncompressed 8-bit Gray/RGB/RGBA, little-endian.
    #[cfg(feature = "tiff")]
    pub fn tiff_rgb8(width: u32, height: u32, pixels: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut cursor = std::io::Cursor::new(&mut out);
            let mut enc = ::tiff::encoder::TiffEncoder::new(&mut cursor).expect("tiff header");
            enc.write_image::<::tiff::encoder::colortype::RGB8>(width, height, pixels)
                .expect("tiff image");
        }
        out
    }

    /// [`tiff_rgb8`] for 8-bit RGBA pixels.
    #[cfg(feature = "tiff")]
    pub fn tiff_rgba8(width: u32, height: u32, pixels: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut cursor = std::io::Cursor::new(&mut out);
            let mut enc = ::tiff::encoder::TiffEncoder::new(&mut cursor).expect("tiff header");
            enc.write_image::<::tiff::encoder::colortype::RGBA8>(width, height, pixels)
                .expect("tiff image");
        }
        out
    }

    /// [`tiff_rgb8`] for 8-bit grayscale pixels.
    #[cfg(feature = "tiff")]
    pub fn tiff_gray8(width: u32, height: u32, pixels: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut cursor = std::io::Cursor::new(&mut out);
            let mut enc = ::tiff::encoder::TiffEncoder::new(&mut cursor).expect("tiff header");
            enc.write_image::<::tiff::encoder::colortype::Gray8>(width, height, pixels)
                .expect("tiff image");
        }
        out
    }

    /// [`tiff_rgb8`] for 8-bit CMYK pixels.
    #[cfg(feature = "tiff")]
    pub fn tiff_cmyk8(width: u32, height: u32, pixels: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut cursor = std::io::Cursor::new(&mut out);
            let mut enc = ::tiff::encoder::TiffEncoder::new(&mut cursor).expect("tiff header");
            enc.write_image::<::tiff::encoder::colortype::CMYK8>(width, height, pixels)
                .expect("tiff image");
        }
        out
    }

    /// 16-bit grayscale — outside `prepare_tiff`'s supported (8-bit-only)
    /// subset, used to test that skip path.
    #[cfg(feature = "tiff")]
    pub fn tiff_gray16(width: u32, height: u32, pixels: &[u16]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut cursor = std::io::Cursor::new(&mut out);
            let mut enc = ::tiff::encoder::TiffEncoder::new(&mut cursor).expect("tiff header");
            enc.write_image::<::tiff::encoder::colortype::Gray16>(width, height, pixels)
                .expect("tiff image");
        }
        out
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
        assert_eq!(sniff(b"GIF89a....", ""), MediaFormat::Gif);
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 ", ""), MediaFormat::WebP);
        assert_eq!(sniff(b"????", "image/gif"), MediaFormat::Gif);
        assert_eq!(sniff(b"????", "image/webp"), MediaFormat::WebP);
        assert_eq!(sniff(b"BM\0\0\0\0", ""), MediaFormat::Bmp);
        assert_eq!(sniff(b"II*\0", ""), MediaFormat::Tiff);
        assert_eq!(sniff(b"MM\0*", ""), MediaFormat::Tiff);
        assert_eq!(sniff(b"????", "image/bmp"), MediaFormat::Bmp);
        assert_eq!(sniff(b"????", "image/tiff"), MediaFormat::Tiff);
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
        for (bytes, content_type, fmt) in [
            (&[0xD7, 0xCD, 0xC6, 0x9A, 0, 0][..], "", "WMF"),
            (&b"<svg"[..], "image/svg+xml", "SVG"),
        ] {
            assert_eq!(
                prepare_image(bytes, content_type, AlphaMode::SoftMask, true),
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

    /* ---- Issue #189: GIF + WebP ------------------------------------- */

    /// Red, green, blue, white.
    #[cfg(feature = "gif")]
    const GIF_PALETTE: [u8; 12] = [255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255];

    /// 3×2, indices `0 1 2 / 3 2 0`, index 2 (blue) transparent.
    #[cfg(feature = "gif")]
    fn gif_3x2_transparent() -> Vec<u8> {
        test_images::gif(3, 2, &GIF_PALETTE, 0, 0, 3, 2, &[0, 1, 2, 3, 2, 0], Some(2))
    }

    #[cfg(feature = "gif")]
    #[test]
    fn gif_palette_expands_and_transparent_index_becomes_alpha() {
        let data = gif_3x2_transparent();
        let soft = prepare_image(&data, "image/gif", AlphaMode::SoftMask, false).unwrap();
        assert_eq!(
            (soft.width, soft.height, soft.color, soft.encoding),
            (3, 2, ImageColor::Rgb, ImageEncoding::Raw)
        );
        assert_eq!(soft.alpha, Some(vec![255, 255, 0, 255, 0, 255]));
        /* Opaque pixels carry their palette colour. */
        assert_eq!(&soft.data[0..6], &[255, 0, 0, 0, 255, 0]);
        assert_eq!(&soft.data[9..12], &[255, 255, 255]);
        assert_eq!(&soft.data[15..18], &[255, 0, 0]);

        let flat = prepare_image(&data, "image/gif", AlphaMode::FlattenOnWhite, false).unwrap();
        assert!(flat.alpha.is_none());
        assert_eq!(
            flat.data,
            vec![
                255, 0, 0, 0, 255, 0, 255, 255, 255, //
                255, 255, 255, 255, 255, 255, 255, 0, 0,
            ],
            "transparent index → white under PDF/A-1b / X-3"
        );
    }

    #[cfg(feature = "gif")]
    #[test]
    fn gif_opaque_first_frame_has_no_soft_mask() {
        let data = test_images::gif(2, 1, &GIF_PALETTE, 0, 0, 2, 1, &[2, 3], None);
        let p = prepare_image(&data, "", AlphaMode::SoftMask, false).unwrap();
        assert!(p.alpha.is_none());
        assert_eq!(p.data, vec![0, 0, 255, 255, 255, 255]);
    }

    #[cfg(feature = "gif")]
    #[test]
    fn gif_frame_is_composited_at_its_offset_on_a_transparent_screen() {
        /* 4×3 screen, a 2×1 red/green frame at (1, 1). */
        let data = test_images::gif(4, 3, &GIF_PALETTE, 1, 1, 2, 1, &[0, 1], None);
        let soft = prepare_image(&data, "", AlphaMode::SoftMask, false).unwrap();
        assert_eq!((soft.width, soft.height), (4, 3));
        let mut alpha = vec![0u8; 12];
        alpha[5] = 255;
        alpha[6] = 255;
        assert_eq!(soft.alpha, Some(alpha));
        assert_eq!(&soft.data[15..21], &[255, 0, 0, 0, 255, 0]);
        let flat = prepare_image(&data, "", AlphaMode::FlattenOnWhite, false).unwrap();
        assert_eq!(&flat.data[0..3], &[255, 255, 255]);
        assert_eq!(&flat.data[15..21], &[255, 0, 0, 0, 255, 0]);

        /* A frame hanging off the screen is clipped, not a panic. */
        let data = test_images::gif(2, 2, &GIF_PALETTE, 1, 1, 3, 3, &[1; 9], None);
        let p = prepare_image(&data, "", AlphaMode::SoftMask, false).unwrap();
        assert_eq!((p.width, p.height), (2, 2));
        assert_eq!(p.alpha, Some(vec![0, 0, 0, 255]));

        /* A 0×0 logical screen falls back to the frame's extent. */
        let data = test_images::gif(0, 0, &GIF_PALETTE, 1, 0, 1, 1, &[3], None);
        let p = prepare_image(&data, "", AlphaMode::SoftMask, false).unwrap();
        assert_eq!((p.width, p.height), (2, 1));
        assert_eq!(p.alpha, Some(vec![0, 255]));
    }

    #[cfg(feature = "gif")]
    #[test]
    fn gif_oversize_screen_is_skipped_before_decoding() {
        let mut data = gif_3x2_transparent();
        /* Logical screen descriptor: width, height (LE u16) at bytes 6..10. */
        data[6..10].copy_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]);
        assert_eq!(
            prepare_image(&data, "", AlphaMode::SoftMask, false),
            Err(ImageSkipReason::TooLarge {
                width: 65535,
                height: 65535
            })
        );
    }

    /// 3×2 RGBA with every alpha level class: opaque, partial, clear.
    #[cfg(feature = "webp")]
    const WEBP_PX: [u8; 24] = [
        255, 0, 0, 255, 0, 255, 0, 128, 0, 0, 255, 0, //
        10, 20, 30, 255, 40, 50, 60, 255, 70, 80, 90, 64,
    ];

    #[cfg(feature = "webp")]
    #[test]
    fn webp_lossless_round_trips_exactly_with_alpha() {
        let data = test_images::webp_lossless_rgba(3, 2, &WEBP_PX);
        assert_eq!(sniff(&data, ""), MediaFormat::WebP);
        let soft = prepare_image(&data, "image/webp", AlphaMode::SoftMask, false).unwrap();
        assert_eq!(
            (soft.width, soft.height, soft.color, soft.encoding),
            (3, 2, ImageColor::Rgb, ImageEncoding::Raw)
        );
        assert_eq!(soft.alpha, Some(vec![255, 128, 0, 255, 255, 64]));
        /* Opaque / partial pixels are bit-exact (lossless). */
        assert_eq!(&soft.data[0..6], &[255, 0, 0, 0, 255, 0]);
        assert_eq!(&soft.data[9..18], &[10, 20, 30, 40, 50, 60, 70, 80, 90]);

        let flat = prepare_image(&data, "", AlphaMode::FlattenOnWhite, false).unwrap();
        assert!(flat.alpha.is_none());
        assert_eq!(&flat.data[0..3], &[255, 0, 0]);
        assert_eq!(&flat.data[6..9], &[255, 255, 255], "alpha 0 → white");
        let expect: Vec<u8> = [70u8, 80, 90]
            .iter()
            .map(|&c| flatten_on_white(c, 64))
            .collect();
        assert_eq!(&flat.data[15..18], expect.as_slice());
    }

    #[cfg(feature = "webp")]
    #[test]
    fn webp_lossless_opaque_has_no_soft_mask() {
        let px: Vec<u8> = [9u8, 8, 7, 255].repeat(4);
        let data = test_images::webp_lossless_rgba(2, 2, &px);
        let p = prepare_image(&data, "", AlphaMode::SoftMask, false).unwrap();
        assert!(p.alpha.is_none());
        assert_eq!(p.data, [9u8, 8, 7].repeat(4));
    }

    /// Lossy samples land within YUV 4:2:0 rounding of the source colour.
    #[cfg(feature = "webp")]
    fn near(got: &[u8], want: [u8; 3]) -> bool {
        got.iter()
            .zip(want)
            .all(|(&g, w)| (i16::from(g) - i16::from(w)).abs() <= 8)
    }

    #[cfg(feature = "webp")]
    #[test]
    fn webp_lossy_decodes_to_rgb() {
        let data = test_images::WEBP_LOSSY_4X4_RED;
        let p = prepare_image(&data, "image/webp", AlphaMode::SoftMask, false).unwrap();
        assert_eq!((p.width, p.height, p.color), (4, 4, ImageColor::Rgb));
        assert!(p.alpha.is_none());
        assert_eq!(p.data.len(), 4 * 4 * 3);
        for px in p.data.chunks_exact(3) {
            assert!(near(px, [200, 40, 40]), "{px:?}");
        }
    }

    #[cfg(feature = "webp")]
    #[test]
    fn webp_lossy_alpha_plane_is_a_soft_mask_or_flattened() {
        let data = test_images::WEBP_LOSSY_ALPHA_4X4_BLUE;
        let soft = prepare_image(&data, "", AlphaMode::SoftMask, false).unwrap();
        assert_eq!((soft.width, soft.height), (4, 4));
        let mut alpha = vec![255u8; 8];
        alpha.extend([0u8; 8]);
        assert_eq!(soft.alpha, Some(alpha));
        assert!(
            near(&soft.data[0..3], [40, 40, 200]),
            "{:?}",
            &soft.data[0..3]
        );

        let flat = prepare_image(&data, "", AlphaMode::FlattenOnWhite, false).unwrap();
        assert!(flat.alpha.is_none());
        assert!(near(&flat.data[0..3], [40, 40, 200]));
        assert!(
            flat.data[24..].iter().all(|&v| v == 255),
            "clear rows → white"
        );
    }

    /// Fuzz-style robustness: every truncation, plus deterministic byte
    /// corruption, of every GIF / WebP fixture is `Ok` or a typed skip —
    /// never a panic (the wasm build is `panic = "abort"`, so a decoder
    /// panic would kill the worker mid-export).
    #[test]
    fn truncated_and_corrupt_gif_webp_never_panic() {
        /* Only mutated when a decoder feature adds its encoder fixtures. */
        #[cfg_attr(not(any(feature = "gif", feature = "webp")), allow(unused_mut))]
        let mut fixtures: Vec<Vec<u8>> = vec![
            b"GIF89a".to_vec(),
            b"RIFF\0\0\0\0WEBP".to_vec(),
            test_images::WEBP_LOSSY_4X4_RED.to_vec(),
            test_images::WEBP_LOSSY_ALPHA_4X4_BLUE.to_vec(),
        ];
        #[cfg(feature = "gif")]
        {
            fixtures.push(gif_3x2_transparent());
            fixtures.push(test_images::gif(
                4,
                3,
                &GIF_PALETTE,
                1,
                1,
                2,
                1,
                &[0, 1],
                None,
            ));
        }
        #[cfg(feature = "webp")]
        fixtures.push(test_images::webp_lossless_rgba(3, 2, &WEBP_PX));

        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for fixture in &fixtures {
            for cut in 0..fixture.len() {
                for mode in [AlphaMode::SoftMask, AlphaMode::FlattenOnWhite] {
                    let r = prepare_image(&fixture[..cut], "image/gif", mode, false);
                    if cut < 13 {
                        assert!(r.is_err(), "cut {cut} of {fixture:?}");
                    }
                    let _ = prepare_image(&fixture[..cut], "image/webp", mode, false);
                }
            }
            for _ in 0..400 {
                let mut m = fixture.clone();
                /* Keep the magic so the decoder (not the sniffer) sees it. */
                for _ in 0..1 + next() % 4 {
                    let at = 4 + (next() as usize) % (m.len() - 4);
                    m[at] = next() as u8;
                }
                if let Ok(p) = prepare_image(&m, "", AlphaMode::SoftMask, false) {
                    assert_eq!(
                        p.data.len(),
                        p.width as usize * p.height as usize * 3,
                        "decoded size matches the dimensions"
                    );
                }
            }
        }
    }

    /* ---- Issue #207: BMP + TIFF --------------------------------------- */

    const BMP_PALETTE: [[u8; 3]; 4] = [[10, 20, 30], [40, 50, 60], [70, 80, 90], [100, 110, 120]];

    #[test]
    fn bmp_24bpp_bgr_becomes_rgb_bottom_up_and_top_down() {
        /* 2×2, top-to-bottom RGB rows: (255,0,0),(0,255,0) / (0,0,255),(255,255,0). */
        let bgr: [u8; 12] = [
            0, 0, 255, 0, 255, 0, //
            255, 0, 0, 0, 255, 255,
        ];
        let expect_rgb = vec![255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 0];
        for top_down in [false, true] {
            let data = test_images::bmp(2, 2, 24, 3, None, &bgr, top_down);
            let p = prepare_image(&data, "image/bmp", AlphaMode::SoftMask, false).unwrap();
            assert_eq!((p.width, p.height, p.color), (2, 2, ImageColor::Rgb));
            assert!(p.alpha.is_none());
            assert_eq!(p.data, expect_rgb, "top_down={top_down}");
        }
    }

    #[test]
    fn bmp_32bpp_ignores_the_fourth_byte() {
        let bgrx: [u8; 4] = [10, 20, 30, 200]; // B, G, R, junk/reserved
        let data = test_images::bmp(1, 1, 32, 4, None, &bgrx, false);
        let p = prepare_image(&data, "", AlphaMode::SoftMask, false).unwrap();
        assert!(p.alpha.is_none());
        assert_eq!(p.data, vec![30, 20, 10]);
    }

    #[test]
    fn bmp_8bpp_palette_expands_to_rgb() {
        let indices: [u8; 2] = [1, 3];
        let data = test_images::bmp(2, 1, 8, 1, Some(&BMP_PALETTE), &indices, false);
        let p = prepare_image(&data, "", AlphaMode::SoftMask, false).unwrap();
        assert_eq!(p.data, vec![40, 50, 60, 100, 110, 120]);
    }

    #[test]
    fn bmp_rle8_decodes_encoded_runs_end_of_line_and_absolute_mode() {
        /* 4×2. Bottom row: run of 4×index0. Top row: absolute-mode [1,1,2]
        (padded odd count) then an encoded run of 1×index3, then end of
        bitmap. */
        let rle: Vec<u8> = vec![4, 0, 0, 0, 0, 3, 1, 1, 2, 0, 1, 3, 0, 1];
        let data = test_images::bmp_rle8(4, 2, &BMP_PALETTE, &rle);
        let p = prepare_image(&data, "image/bmp", AlphaMode::SoftMask, false).unwrap();
        assert_eq!((p.width, p.height), (4, 2));
        let mut expect = Vec::new();
        expect.extend_from_slice(&BMP_PALETTE[1]);
        expect.extend_from_slice(&BMP_PALETTE[1]);
        expect.extend_from_slice(&BMP_PALETTE[2]);
        expect.extend_from_slice(&BMP_PALETTE[3]);
        for _ in 0..4 {
            expect.extend_from_slice(&BMP_PALETTE[0]);
        }
        assert_eq!(p.data, expect);
    }

    #[test]
    fn bmp_rle8_delta_leaves_the_default_index_behind() {
        /* 3×1: index 2 at x=0, delta (+1, +0) to x=2, index 1 at x=2 — x=1
        is never written and stays the default index 0. */
        let rle: Vec<u8> = vec![1, 2, 0, 2, 1, 0, 1, 1, 0, 1];
        let data = test_images::bmp_rle8(3, 1, &BMP_PALETTE, &rle);
        let p = prepare_image(&data, "", AlphaMode::SoftMask, false).unwrap();
        let mut expect = Vec::new();
        expect.extend_from_slice(&BMP_PALETTE[2]);
        expect.extend_from_slice(&BMP_PALETTE[0]);
        expect.extend_from_slice(&BMP_PALETTE[1]);
        assert_eq!(p.data, expect);
    }

    #[test]
    fn bmp_unsupported_dib_variants_are_typed_skips() {
        /* OS/2 core header (size 12). */
        let mut core = vec![b'B', b'M'];
        core.extend_from_slice(&26u32.to_le_bytes()); // file size (unchecked)
        core.extend_from_slice(&[0, 0, 0, 0]);
        core.extend_from_slice(&26u32.to_le_bytes()); // pixel data offset
        core.extend_from_slice(&12u32.to_le_bytes()); // BITMAPCOREHEADER size
        core.extend_from_slice(&[1, 0, 1, 0, 1, 0, 24, 0]);
        assert!(matches!(
            prepare_image(&core, "", AlphaMode::SoftMask, false),
            Err(ImageSkipReason::UnsupportedEncoding { .. })
        ));

        /* 16bpp BI_RGB — not in the supported set. */
        let px16 = [0u8; 4];
        let data = test_images::bmp(1, 1, 16, 2, None, &px16, false);
        assert!(matches!(
            prepare_image(&data, "", AlphaMode::SoftMask, false),
            Err(ImageSkipReason::UnsupportedEncoding { .. })
        ));
    }

    /// Fuzz-style robustness: every truncation, plus deterministic byte
    /// corruption (the "BM" magic kept intact so the decoder, not the
    /// sniffer, sees it), of every BMP fixture is `Ok` or a typed skip —
    /// never a panic.
    #[test]
    fn truncated_and_corrupt_bmp_never_panic() {
        let fixtures: Vec<Vec<u8>> = vec![
            test_images::bmp(2, 2, 24, 3, None, &[0u8; 12], false),
            test_images::bmp(2, 2, 32, 4, None, &[0u8; 16], false),
            test_images::bmp(2, 1, 8, 1, Some(&BMP_PALETTE), &[1, 3], false),
            test_images::bmp_rle8(
                4,
                2,
                &BMP_PALETTE,
                &[4, 0, 0, 0, 0, 3, 1, 1, 2, 0, 1, 3, 0, 1],
            ),
        ];
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for fixture in &fixtures {
            for cut in 0..=fixture.len() {
                for mode in [AlphaMode::SoftMask, AlphaMode::FlattenOnWhite] {
                    let r = prepare_image(&fixture[..cut], "image/bmp", mode, false);
                    if cut < 18 {
                        assert!(r.is_err(), "cut {cut} of {fixture:?}");
                    }
                }
            }
            for _ in 0..400 {
                let mut m = fixture.clone();
                /* Keep the "BM" magic so the decoder (not the sniffer) sees it. */
                for _ in 0..1 + next() % 4 {
                    let at = 2 + (next() as usize) % (m.len() - 2);
                    m[at] = next() as u8;
                }
                if let Ok(p) = prepare_image(&m, "", AlphaMode::SoftMask, false) {
                    assert_eq!(
                        p.data.len(),
                        p.width as usize * p.height as usize * 3,
                        "decoded size matches the dimensions"
                    );
                }
            }
        }
    }

    /// Issue #208's adversarial "large declared dimensions" corpus seeds
    /// (`fuzz/corpus/image_decode/`, staged for a future dedicated
    /// `format_pdf_image_decode` fuzz target — see the task's final
    /// report): a tiny file whose header claims a canvas far past
    /// [`MAX_IMAGE_PIXELS`] must be rejected before any pixel buffer is
    /// allocated, never panic, for every format this module handles.
    #[test]
    fn adversarial_oversized_dimension_seeds_are_rejected_before_allocating() {
        let cases: &[(&[u8], &str)] = &[
            (
                include_bytes!(
                    "../../../fuzz/corpus/image_decode/gif_oversized_dims_65535x65535.bin"
                ),
                "image/gif",
            ),
            (
                include_bytes!("../../../fuzz/corpus/image_decode/webp_oversized_dims_vp8x.bin"),
                "image/webp",
            ),
            (
                include_bytes!("../../../fuzz/corpus/image_decode/png_oversized_dims_ihdr.bin"),
                "image/png",
            ),
            (
                include_bytes!(
                    "../../../fuzz/corpus/image_decode/bmp_oversized_dims_65535x65535.bin"
                ),
                "image/bmp",
            ),
            (
                include_bytes!("../../../fuzz/corpus/image_decode/tiff_oversized_dims_ifd.bin"),
                "image/tiff",
            ),
        ];
        for (data, content_type) in cases {
            for mode in [AlphaMode::SoftMask, AlphaMode::FlattenOnWhite] {
                let r = prepare_image(data, content_type, mode, false);
                assert!(
                    matches!(
                        r,
                        Err(ImageSkipReason::TooLarge { .. })
                            | Err(ImageSkipReason::Malformed { .. })
                            | Err(ImageSkipReason::UnsupportedFormat { .. })
                    ),
                    "{content_type}: expected a typed skip, got {r:?}"
                );
            }
        }
    }

    #[cfg(feature = "tiff")]
    #[test]
    fn tiff_rgb8_and_gray8_round_trip() {
        let rgb_px: [u8; 12] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
        let data = test_images::tiff_rgb8(2, 2, &rgb_px);
        let p = prepare_image(&data, "image/tiff", AlphaMode::SoftMask, false).unwrap();
        assert_eq!((p.width, p.height, p.color), (2, 2, ImageColor::Rgb));
        assert!(p.alpha.is_none());
        assert_eq!(p.data, rgb_px);

        let gray_px: [u8; 6] = [10, 20, 30, 40, 50, 60];
        let data = test_images::tiff_gray8(3, 2, &gray_px);
        let p = prepare_image(&data, "", AlphaMode::SoftMask, false).unwrap();
        assert_eq!(p.color, ImageColor::Gray);
        assert_eq!(p.data, gray_px);
    }

    #[cfg(feature = "tiff")]
    #[test]
    fn tiff_rgba8_keeps_alpha_as_soft_mask_or_flattens_on_white() {
        /* 2×1: opaque red, half-transparent blue. */
        let px = [255, 0, 0, 255, 0, 0, 255, 128];
        let data = test_images::tiff_rgba8(2, 1, &px);
        let soft = prepare_image(&data, "image/tiff", AlphaMode::SoftMask, false).unwrap();
        assert_eq!(soft.data, vec![255, 0, 0, 0, 0, 255]);
        assert_eq!(soft.alpha, Some(vec![255, 128]));
        let flat = prepare_image(&data, "", AlphaMode::FlattenOnWhite, false).unwrap();
        assert!(flat.alpha.is_none());
        assert_eq!(flat.data, vec![255, 0, 0, 127, 127, 255]);
    }

    #[cfg(feature = "tiff")]
    #[test]
    fn tiff_cmyk_is_profile_gated() {
        let px: [u8; 4] = [10, 20, 30, 40];
        let data = test_images::tiff_cmyk8(1, 1, &px);
        let plain = prepare_image(&data, "", AlphaMode::SoftMask, true).unwrap();
        assert_eq!(plain.color, ImageColor::Cmyk);
        assert_eq!(plain.data, px);
        assert!(matches!(
            prepare_image(&data, "", AlphaMode::SoftMask, false),
            Err(ImageSkipReason::UnsupportedEncoding { .. })
        ));
    }

    #[cfg(feature = "tiff")]
    #[test]
    fn tiff_16bit_samples_are_a_typed_skip() {
        let px16: [u16; 4] = [0, 0, 0, 0];
        let data = test_images::tiff_gray16(2, 2, &px16);
        assert!(matches!(
            prepare_image(&data, "", AlphaMode::SoftMask, false),
            Err(ImageSkipReason::UnsupportedEncoding { .. })
        ));
    }

    /// Fuzz-style robustness: every truncation, plus deterministic byte
    /// corruption, of every TIFF fixture is `Ok` or a typed skip — never a
    /// panic.
    #[cfg(feature = "tiff")]
    #[test]
    fn truncated_and_corrupt_tiff_never_panic() {
        let fixtures: Vec<Vec<u8>> = vec![
            test_images::tiff_rgb8(2, 2, &[0u8; 12]),
            test_images::tiff_rgba8(2, 2, &[0u8; 16]),
            test_images::tiff_gray8(3, 2, &[0u8; 6]),
        ];
        let mut seed = 0xD1B5_4A32_D192_ED03u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for fixture in &fixtures {
            for cut in 0..=fixture.len() {
                for mode in [AlphaMode::SoftMask, AlphaMode::FlattenOnWhite] {
                    let _ = prepare_image(&fixture[..cut], "image/tiff", mode, false);
                }
            }
            for _ in 0..400 {
                let mut m = fixture.clone();
                for _ in 0..1 + next() % 4 {
                    let at = (next() as usize) % m.len();
                    m[at] = next() as u8;
                }
                /* Keep the byte-order magic so the sniffer still routes to
                TIFF (content-type would too, but this exercises both). */
                m[0..4].copy_from_slice(&fixture[0..4]);
                if let Ok(p) = prepare_image(&m, "", AlphaMode::SoftMask, false) {
                    let channels = match p.color {
                        ImageColor::Gray => 1,
                        ImageColor::Rgb => 3,
                        ImageColor::Cmyk => 4,
                    };
                    assert_eq!(
                        p.data.len(),
                        p.width as usize * p.height as usize * channels,
                        "decoded size matches the dimensions"
                    );
                }
            }
        }
    }

    #[cfg(not(feature = "tiff"))]
    #[test]
    fn tiff_without_the_feature_is_an_unsupported_format() {
        assert_eq!(
            prepare_image(b"II*\0\x08\0\0\0\0\0", "", AlphaMode::SoftMask, false),
            Err(ImageSkipReason::UnsupportedFormat { format: "TIFF" })
        );
    }

    #[cfg(not(feature = "gif"))]
    #[test]
    fn gif_without_the_feature_is_an_unsupported_format() {
        assert_eq!(
            prepare_image(b"GIF89a\x01\x00\x01\x00", "", AlphaMode::SoftMask, false),
            Err(ImageSkipReason::UnsupportedFormat { format: "GIF" })
        );
    }

    #[cfg(not(feature = "webp"))]
    #[test]
    fn webp_without_the_feature_is_an_unsupported_format() {
        assert_eq!(
            prepare_image(
                &test_images::WEBP_LOSSY_4X4_RED,
                "",
                AlphaMode::SoftMask,
                false
            ),
            Err(ImageSkipReason::UnsupportedFormat { format: "WebP" })
        );
    }
}
