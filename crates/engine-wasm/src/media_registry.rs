//! Issue #506 - which of the document's media blobs still need a decoded
//! bitmap.
//!
//! The shell decodes each `word/media/*` blob into an `ImageBitmap` (the
//! browser's own pipeline) and installs it with `Engine::register_image`;
//! the Canvas2D backend looks pictures up there and paints a grey
//! placeholder on a miss. The decode used to run only after an open and a
//! recovery, so a picture the engine itself minted (`InsertImage`) painted
//! as the placeholder for the rest of the session. The worker now asks the
//! engine after every command that can change the document which blobs are
//! *pending* ([`pending_keys`]) and decodes just those.
//!
//! A key being present in the cache is not enough to call it registered:
//! the engine mints `nge_img_<n>` from the CURRENT media map, so an undone
//! insert frees its key and the next insert (a different picture) re-mints
//! the same one while the cache still holds the old bitmap. Every
//! registration therefore records a [`fingerprint`] of the bytes it was
//! decoded from, and a blob whose current fingerprint differs is pending
//! again.

use std::collections::HashMap;

use engine::ImageBlob;

/// Bytes sampled by [`fingerprint`] (spread evenly over the blob).
const SAMPLES: usize = 256;

/// A cheap content fingerprint of a blob: its length plus FNV-1a over up
/// to [`SAMPLES`] bytes spread evenly across it. O(1) in the blob size on
/// purpose - the check runs after every mutating command, and a 500-picture
/// document must not hash every megabyte on every keystroke. Sampling the
/// MIDDLE matters: every PNG starts with the same 8 signature bytes and
/// ends with the same `IEND` chunk, so a head / tail hash would call any
/// two equally long PNGs the same.
pub(crate) fn fingerprint(data: &[u8]) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = FNV_OFFSET ^ (data.len() as u64);
    h = h.wrapping_mul(FNV_PRIME);
    if data.is_empty() {
        return h;
    }
    let step = (data.len() / SAMPLES).max(1);
    let mut i = 0;
    let mut taken = 0;
    while i < data.len() && taken < SAMPLES {
        h ^= u64::from(data[i]);
        h = h.wrapping_mul(FNV_PRIME);
        i += step;
        taken += 1;
    }
    /* The last byte always counts, whatever the stride landed on. */
    h ^= u64::from(data[data.len() - 1]);
    h.wrapping_mul(FNV_PRIME)
}

/// The media keys with no up-to-date bitmap, sorted (a deterministic decode
/// order). `registered(key)` answers the fingerprint the key's cached bitmap
/// was decoded from, `None` when nothing is cached for it.
pub(crate) fn pending_keys(
    media: &HashMap<String, ImageBlob>,
    registered: impl Fn(&str) -> Option<u64>,
) -> Vec<&str> {
    let mut out: Vec<&str> = media
        .iter()
        .filter(|(key, blob)| registered(key) != Some(fingerprint(&blob.data)))
        .map(|(key, _)| key.as_str())
        .collect();
    out.sort_unstable();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blob(bytes: &[u8]) -> ImageBlob {
        ImageBlob {
            content_type: "image/png".to_string(),
            data: bytes.to_vec(),
        }
    }

    /// A PNG-shaped blob: the shared signature, `body`, the shared `IEND`.
    fn png_like(body: &[u8]) -> Vec<u8> {
        let mut v = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        v.extend_from_slice(body);
        v.extend_from_slice(&[0, 0, 0, 0, b'I', b'E', b'N', b'D', 0xae, 0x42, 0x60, 0x82]);
        v
    }

    #[test]
    fn a_blob_with_no_cached_bitmap_is_pending() {
        let mut media = HashMap::new();
        media.insert("nge_img_1".to_string(), blob(b"abc"));
        assert_eq!(pending_keys(&media, |_| None), vec!["nge_img_1"]);
    }

    #[test]
    fn a_blob_whose_bitmap_matches_its_bytes_is_not_pending() {
        let mut media = HashMap::new();
        media.insert("nge_img_1".to_string(), blob(b"abc"));
        let fp = fingerprint(b"abc");
        assert!(pending_keys(&media, |_| Some(fp)).is_empty());
    }

    #[test]
    fn a_reminted_key_holding_different_bytes_is_pending_again() {
        /* Insert A (nge_img_1), undo, insert B: the engine mints nge_img_1
        again while the cache still holds A's bitmap. */
        let a = png_like(&[1; 4000]);
        let mut b = png_like(&[1; 4000]);
        let step = a.len() / SAMPLES;
        b[step * 100] = 9; /* equal length, equal head and tail, one sampled byte apart */
        let mut media = HashMap::new();
        media.insert("nge_img_1".to_string(), blob(&b));
        let cached_for_a = fingerprint(&a);
        assert_eq!(
            pending_keys(&media, |_| Some(cached_for_a)),
            vec!["nge_img_1"],
            "a different picture under a reused key must be decoded again"
        );
    }

    #[test]
    fn equal_length_pngs_with_different_pixels_fingerprint_differently() {
        let a = png_like(&(0..2000u32).map(|i| (i % 251) as u8).collect::<Vec<_>>());
        let b = png_like(&(0..2000u32).map(|i| (i % 241) as u8).collect::<Vec<_>>());
        assert_eq!(a.len(), b.len());
        assert_ne!(fingerprint(&a), fingerprint(&b));
    }

    #[test]
    fn pending_keys_come_back_sorted_and_only_the_stale_ones() {
        let mut media = HashMap::new();
        for (k, bytes) in [("z", b"zz" as &[u8]), ("a", b"aa"), ("m", b"mm")] {
            media.insert(k.to_string(), blob(bytes));
        }
        let fp_m = fingerprint(b"mm");
        let pending = pending_keys(&media, |k| (k == "m").then_some(fp_m));
        assert_eq!(pending, vec!["a", "z"]);
    }

    #[test]
    fn an_empty_blob_has_a_stable_fingerprint() {
        assert_eq!(fingerprint(&[]), fingerprint(&[]));
        assert_ne!(fingerprint(&[]), fingerprint(&[0]));
    }
}
