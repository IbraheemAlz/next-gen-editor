#![no_main]
//! New target (D5.5, issue #227): raw bytes -> `format_pdf::prepare_image`
//! for every enabled decoder (PNG/JPEG/GIF/WebP/BMP/TIFF; sniffing decides).
//! Asserts the typed-skip contract: no panic, no allocation past
//! `MAX_IMAGE_PIXELS`, and a successful decode is XObject-ready. See
//! `engine_fuzz::run_format_pdf_image_decode`.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    engine_fuzz::run_format_pdf_image_decode(data);
});
