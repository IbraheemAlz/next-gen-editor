//! Font loading + glyph metrics + glyph rasterization via `swash`.

use crate::script::Script;
use crate::substitution::{ScriptClass, Substitution, family_key, substitution_for};
use std::collections::HashMap;
use std::sync::Arc;
use swash::FontRef;
use swash::scale::{Render, ScaleContext, Source};
use swash::zeno::Format;
use thiserror::Error;

/// Font identifier — a key into the engine's font map.
pub type FontId = String;

#[derive(Debug, Error)]
pub enum FontError {
    #[error("font parse failed (invalid TTF/OTF/WOFF2)")]
    Parse,
    #[error("glyph missing for U+{0:04X}")]
    GlyphMissing(u32),
    #[error("rasterizer produced no image")]
    NoImage,
    /// Issue #422 — the requested pixel size is non-finite, not positive,
    /// or above [`MAX_RASTER_PX`]; nothing was rasterized.
    #[error("glyph pixel size outside the rasterizable range")]
    SizeOutOfRange,
}

/// Issue #422 — the largest pixel size [`LoadedFont::rasterize_glyph`]
/// rasterizes. The size is untrusted (a span's font size from a `.docx`
/// `w:sz`, a restored snapshot or a wire `ApplyFormatting`, times the zoom
/// and device scale): a fuzz-restored snapshot asked for a 20 720 638 px
/// glyph and `swash` reserved a 1.4 GB coverage mask for it. 4096 device
/// px keeps one mask at most ~16 MB; it is 3072 pt at 100 % zoom on a 1×
/// display (Word's own maximum font size is 1638 pt) and ~300 pt at the
/// 450 % zoom on a 3× display. A larger glyph is not rasterized — the
/// renderer skips it as it skips a glyph the font cannot draw.
pub const MAX_RASTER_PX: f32 = 4096.0;

#[derive(Debug, Clone, Copy)]
pub struct FontMetrics {
    pub units_per_em: u16,
    pub ascent: f32,
    pub descent: f32,
    pub leading: f32,
    pub cap_height: f32,
    pub x_height: f32,
}

/// Issue #329 — a face's line metrics under **Word's rule** (Windows Word,
/// the platform Arabic documents are authored on), in font units:
///
/// - `OS/2.fsSelection` bit 7 (`USE_TYPO_METRICS`) set → the typographic
///   ascender / descender / line gap;
/// - otherwise `usWinAscent` / `usWinDescent`, plus GDI's *external
///   leading* — the part of `hhea.lineGap` the win extent does not already
///   absorb: `max(0, hhea.lineGap − ((winAscent + winDescent) −
///   (hhea.ascender − hhea.descender)))`. That is what makes Word's single
///   spacing for 12 pt Times New Roman 13.8 pt rather than 13.3 pt;
/// - no usable `OS/2` → the `hhea` values.
///
/// A single line is `ascent + descent + line_gap` tall; the gap sits above
/// the ascent (GDI's external leading is leading *above* the text). Mac
/// Word uses the `hhea` values instead — see [`LoadedFont::line_metrics`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineMetrics {
    pub units_per_em: u16,
    pub ascent: i32,
    pub descent: i32,
    pub line_gap: i32,
}

impl LineMetrics {
    /// `(ascent + line gap, descent)` in px at `px_size`: the line box's
    /// split at the baseline for one line of this face.
    pub fn scaled(&self, px_size: f32) -> (f32, f32) {
        let upem = f32::from(self.units_per_em.max(1));
        let k = px_size / upem;
        (
            (self.ascent + self.line_gap) as f32 * k,
            self.descent as f32 * k,
        )
    }

    /// The single-spaced line height in em (`(ascent + descent + gap) / upem`).
    pub fn line_height_em(&self) -> f32 {
        (self.ascent + self.descent + self.line_gap) as f32 / f32::from(self.units_per_em.max(1))
    }
}

/// Issue #329 — [`LineMetrics`] for `face` (Word's rule; see there).
fn word_line_metrics(face: &rustybuzz::Face<'_>) -> LineMetrics {
    let tables = face.tables();
    let hhea = tables.hhea;
    let upem = tables.head.units_per_em;
    let hhea_metrics = LineMetrics {
        units_per_em: upem,
        ascent: i32::from(hhea.ascender),
        descent: -i32::from(hhea.descender),
        line_gap: i32::from(hhea.line_gap).max(0),
    };
    let Some(os2) = tables.os2 else {
        return hhea_metrics;
    };
    if os2.use_typographic_metrics() {
        return LineMetrics {
            units_per_em: upem,
            ascent: i32::from(os2.typographic_ascender()),
            descent: -i32::from(os2.typographic_descender()),
            line_gap: i32::from(os2.typographic_line_gap()).max(0),
        };
    }
    /* `usWinAscent` / `usWinDescent` (OS/2 bytes 74..78) are unsigned;
    ttf-parser reads them as `i16` and negates the descent, which
    overflows on a hostile 0x8000 — read the raw bytes instead. */
    let raw = face
        .raw_face()
        .table(rustybuzz::ttf_parser::Tag::from_bytes(b"OS/2"))
        .and_then(|d| {
            let u16_at = |at: usize| Some(u16::from_be_bytes([*d.get(at)?, *d.get(at + 1)?]));
            Some((u16_at(74)?, u16_at(76)?))
        });
    let Some((win_ascent, win_descent)) = raw.map(|(a, d)| (i32::from(a), i32::from(d))) else {
        return hhea_metrics;
    };
    if win_ascent + win_descent <= 0 {
        return hhea_metrics;
    }
    let hhea_extent = i32::from(hhea.ascender) - i32::from(hhea.descender);
    let external_leading =
        (i32::from(hhea.line_gap) - ((win_ascent + win_descent) - hhea_extent)).max(0);
    LineMetrics {
        units_per_em: upem,
        ascent: win_ascent,
        descent: win_descent,
        line_gap: external_leading,
    }
}

#[derive(Debug, Clone, Copy)]
pub struct GlyphMetrics {
    pub advance_width: f32,
}

#[derive(Debug, Clone)]
pub struct RasterizedGlyph {
    pub width: u32,
    pub height: u32,
    /// Horizontal offset from pen position to bitmap's left edge.
    pub left: i32,
    /// Vertical offset from baseline to bitmap's top edge (positive = above).
    pub top: i32,
    /// 8-bit alpha coverage, row-major, top-left origin. `len == width * height`.
    pub alpha: Vec<u8>,
}

/// A loaded font face. The `rustybuzz::Face` — whose construction parses and
/// table-indexes the whole font file — is built once at load and cached;
/// re-parsing it per `shape_text` call dominated multi-page layout
/// (Backlog #13). `swash::FontRef` stays rebuilt per access (genuinely cheap).
pub struct LoadedFont {
    id: String,
    /// Font bytes, leaked to `'static` so the cached `rustybuzz::Face` may
    /// borrow them. A loaded font lives for the engine's lifetime regardless,
    /// so this leak is bounded and intentional.
    data: &'static [u8],
    /// The parsed + table-indexed rustybuzz face — built once, reused for
    /// every shaping call.
    rb_face: rustybuzz::Face<'static>,
    units_per_em: u16,
    /// Issue #329 — the face's own family names from its `name` table
    /// (name ID 16, the typographic family, then name ID 1), deduplicated.
    /// [`FontStack`] matches a document's family name against these as
    /// well as against the face's id, so a face registered under any id
    /// still answers to its real name ("Liberation Sans" → `liberation`).
    family_names: Vec<String>,
    /// Issue #329 — Word's line metrics for the face ([`LineMetrics`]).
    line_metrics: LineMetrics,
}

/// Issue #329 — identifies the face (id, units per em, family names);
/// the font bytes are never dumped.
impl std::fmt::Debug for LoadedFont {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoadedFont")
            .field("id", &self.id)
            .field("units_per_em", &self.units_per_em)
            .field("family_names", &self.family_names)
            .finish_non_exhaustive()
    }
}

impl LoadedFont {
    pub fn parse(id: String, data: Vec<u8>) -> Result<Self, FontError> {
        /* Validate BEFORE leaking: a rejected font (garbage bytes from a
        hostile `LoadFont`, or the fuzzer) must not strand its buffer —
        LeakSanitizer flags it and a long session would accumulate them.
        The probe borrows `data` and is dropped before the leak. */
        if FontRef::from_index(&data, 0).is_none()
            || rustybuzz::Face::from_slice(&data, 0).is_none()
        {
            return Err(FontError::Parse);
        }
        /* Leak the bytes to `'static`: the cached `rustybuzz::Face` borrows
        them, and a loaded font is never freed during a session anyway. */
        let data: &'static [u8] = Vec::leak(data);
        let face = FontRef::from_index(data, 0).ok_or(FontError::Parse)?;
        let upem = face.metrics(&[]).units_per_em;
        let rb_face = rustybuzz::Face::from_slice(data, 0).ok_or(FontError::Parse)?;
        let family_names = name_table_families(&rb_face);
        let line_metrics = word_line_metrics(&rb_face);
        Ok(Self {
            id,
            data,
            rb_face,
            units_per_em: upem,
            family_names,
            line_metrics,
        })
    }

    /// Issue #329 — the family names the face's `name` table declares
    /// (typographic family first, then the legacy family), as written.
    pub fn family_names(&self) -> &[String] {
        &self.family_names
    }

    /// Issue #329 — the face's line metrics under Word's (Windows) rule,
    /// which a `.docx` document's font-derived line pitch is built from.
    /// Unlike [`Self::metrics`] (swash: typographic metrics when
    /// `USE_TYPO_METRICS`, else `hhea`, no line gap) this follows what
    /// Word does with the `OS/2` win extent and GDI's external leading.
    pub fn line_metrics(&self) -> LineMetrics {
        self.line_metrics
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// The raw font-file bytes — used to embed the full face in a PDF.
    pub fn data(&self) -> &[u8] {
        self.data
    }

    /// The font bytes as a `'static` slice. The bytes are leaked at parse
    /// (see [`Self::parse`]), so this hands out their real `'static` lifetime —
    /// the Vello path needs it to build a zero-copy `peniko::Blob`.
    pub fn data_static(&self) -> &'static [u8] {
        self.data
    }

    fn face(&self) -> FontRef<'_> {
        FontRef::from_index(self.data, 0).expect("validated in parse")
    }

    /// The cached rustybuzz face — parsed and table-indexed once at load.
    pub fn face_rustybuzz(&self) -> &rustybuzz::Face<'static> {
        &self.rb_face
    }

    /// Per-glyph horizontal advance widths in 1000-em units — the PDF font
    /// `/W` array convention. PDF/A-1b §6.3.5 requires the values to match
    /// the embedded font program's `hmtx` advances.
    pub fn widths_em1000(&self) -> Vec<f32> {
        let upem = self.rb_face.units_per_em() as f32;
        let n = self.rb_face.number_of_glyphs();
        let scale = if upem > 0.0 { 1000.0 / upem } else { 1.0 };
        (0..n)
            .map(|gid| {
                let adv = self
                    .rb_face
                    .glyph_hor_advance(rustybuzz::ttf_parser::GlyphId(gid))
                    .unwrap_or(0) as f32;
                (adv * scale).round()
            })
            .collect()
    }

    /// Whether the font's cmap maps `ch` to a real (non-`.notdef`) glyph.
    pub fn covers(&self, ch: char) -> bool {
        self.face().charmap().map(ch) != 0
    }

    /// Glyph id for `ch` via the font's cmap; `None` when unmapped
    /// (`.notdef`). Used to look up the Tatweel (U+0640) for Kashida ink.
    pub fn glyph_id(&self, ch: char) -> Option<u16> {
        let gid = self.face().charmap().map(ch);
        (gid != 0).then_some(gid)
    }

    /// Whether the font's own OS/2 metadata marks it a bold weight (>= 600).
    pub fn is_bold(&self) -> bool {
        self.face().attributes().weight().0 >= 600
    }

    /// Whether the font's own metadata marks it italic or oblique.
    pub fn is_italic(&self) -> bool {
        !matches!(self.face().attributes().style(), swash::Style::Normal)
    }

    pub fn metrics(&self, px_size: f32) -> FontMetrics {
        let m = self.face().metrics(&[]).scale(px_size);
        FontMetrics {
            units_per_em: self.units_per_em,
            ascent: m.ascent,
            descent: m.descent,
            leading: m.leading,
            cap_height: m.cap_height,
            x_height: m.x_height,
        }
    }

    pub fn glyph_metrics(&self, ch: char, px_size: f32) -> Result<GlyphMetrics, FontError> {
        let face = self.face();
        let gid = face.charmap().map(ch);
        if gid == 0 {
            return Err(FontError::GlyphMissing(ch as u32));
        }
        let gm = face.glyph_metrics(&[]).scale(px_size);
        Ok(GlyphMetrics {
            advance_width: gm.advance_width(gid),
        })
    }

    /// Rasterize a glyph by character (does charmap lookup internally).
    pub fn rasterize(&self, ch: char, px_size: f32) -> Result<RasterizedGlyph, FontError> {
        let gid = {
            let face = self.face();
            let gid = face.charmap().map(ch);
            if gid == 0 {
                return Err(FontError::GlyphMissing(ch as u32));
            }
            gid
        };
        self.rasterize_glyph(gid, px_size)
    }

    /// Rasterize a glyph by its glyph id (skipping the charmap lookup;
    /// used after `rustybuzz` shaping returns glyph ids directly).
    pub fn rasterize_glyph(&self, gid: u16, px_size: f32) -> Result<RasterizedGlyph, FontError> {
        if !(px_size.is_finite() && px_size > 0.0 && px_size <= MAX_RASTER_PX) {
            return Err(FontError::SizeOutOfRange);
        }
        let face = self.face();
        let mut ctx = ScaleContext::new();
        let mut scaler = ctx.builder(face).size(px_size).hint(true).build();
        let image = Render::new(&[Source::Outline])
            .format(Format::Alpha)
            .render(&mut scaler, gid)
            .ok_or(FontError::NoImage)?;
        Ok(RasterizedGlyph {
            width: image.placement.width,
            height: image.placement.height,
            left: image.placement.left,
            top: image.placement.top,
            alpha: image.data,
        })
    }
}

/// Issue #329 — decode the family names (name IDs 16 and 1) of a face's
/// `name` table: Unicode / Windows records are UTF-16BE, Macintosh Roman
/// records are kept when they are plain ASCII. Typographic family first,
/// duplicates (the same name in several platform records) dropped.
fn name_table_families(face: &rustybuzz::Face<'_>) -> Vec<String> {
    use rustybuzz::ttf_parser::PlatformId;
    use rustybuzz::ttf_parser::name_id::{FAMILY, TYPOGRAPHIC_FAMILY};
    let mut out: Vec<String> = Vec::new();
    for wanted in [TYPOGRAPHIC_FAMILY, FAMILY] {
        for name in face.names() {
            if name.name_id != wanted {
                continue;
            }
            let decoded = match name.platform_id {
                PlatformId::Unicode | PlatformId::Windows => {
                    let units: Vec<u16> = name
                        .name
                        .chunks_exact(2)
                        .map(|b| u16::from_be_bytes([b[0], b[1]]))
                        .collect();
                    String::from_utf16(&units).ok()
                }
                PlatformId::Macintosh if name.name.is_ascii() => {
                    Some(String::from_utf8_lossy(name.name).into_owned())
                }
                _ => None,
            };
            if let Some(n) = decoded.map(|n| n.trim().to_string())
                && !n.is_empty()
                && !out.iter().any(|o| family_key(o) == family_key(&n))
            {
                out.push(n);
            }
        }
    }
    out
}

/// Issue #329 — how [`FontStack::resolve_family`] matched a requested
/// family to a loaded face.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FamilyMatch {
    /// The family string is a loaded font id verbatim (Backlog #9).
    Exact,
    /// The family's [`family_key`] equals a loaded face's id key or one of
    /// its `name`-table family names ("Liberation Sans" → `liberation`).
    Name,
    /// The family is not loaded; a [`crate::SUBSTITUTIONS`] row for it
    /// (and the piece's script class) named a loaded substitute.
    Substituted(&'static Substitution),
}

/// Issue #329 — the outcome of [`FontStack::resolve_family`].
#[derive(Debug, Clone, Copy)]
pub struct FamilyResolution<'a> {
    pub id: &'a FontId,
    pub face: &'a LoadedFont,
    pub matched: FamilyMatch,
    /// The face whose line metrics stand in for the requested family's
    /// (a substitution row's [`Substitution::metrics_from`], when that
    /// face is loaded); `None` uses [`Self::face`]'s own.
    pub metrics_id: Option<&'a FontId>,
}

/// Issue #329 — [`FontStack::resolve_detailed`]: a face for one shaped
/// piece, the synthesis it needs, and how a requested family matched
/// (`None` when the piece named no family or the stack fell back).
#[derive(Debug, Clone, Copy)]
pub struct Resolved<'a> {
    pub id: &'a FontId,
    pub face: &'a LoadedFont,
    pub synthesis: Synthesis,
    pub matched: Option<FamilyMatch>,
    /// See [`FamilyResolution::metrics_id`].
    pub metrics_id: Option<&'a FontId>,
}

/// Which synthetic styles the renderer must apply because no real font face
/// covered the requested weight / slant (Backlog #1 — faux bold / italic).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Synthesis {
    pub faux_bold: bool,
    pub faux_italic: bool,
}

/// A per-script font resolver (PHASE_3_RENDER_RTL.md §13.A).
///
/// Holds every loaded face plus, for each script, the ids of the faces that
/// cover it. [`resolve`](FontStack::resolve) picks the best face for a script
/// and requested weight/slant, falling through `fallback_chain` and flagging
/// synthetic styling when no real face variant exists.
pub struct FontStack {
    faces: HashMap<FontId, Arc<LoadedFont>>,
    by_script: HashMap<Script, Vec<FontId>>,
    fallback_chain: Vec<FontId>,
    /// Issue #329 — [`family_key`] → the ids of the faces answering to it:
    /// each face under its own id's key, then under every `name`-table
    /// family name. Sorted, deduplicated.
    by_family: HashMap<String, Vec<FontId>>,
}

impl FontStack {
    /// Build a stack from loaded faces, classifying each by the scripts it
    /// covers (probed against a representative codepoint). `primary` seeds the
    /// fallback chain so a script with no dedicated face still resolves.
    pub fn from_faces(faces: HashMap<FontId, Arc<LoadedFont>>, primary: &str) -> Self {
        let mut by_script: HashMap<Script, Vec<FontId>> = HashMap::new();
        for (id, face) in &faces {
            if face.covers('\u{0628}') {
                by_script
                    .entry(Script::Arabic)
                    .or_default()
                    .push(id.clone());
            }
            if face.covers('A') {
                by_script.entry(Script::Latin).or_default().push(id.clone());
            }
        }
        /* Deterministic priority within each script. */
        for ids in by_script.values_mut() {
            ids.sort();
        }
        /* Fallback chain: the primary first, then the rest in id order. */
        let mut fallback_chain: Vec<FontId> = Vec::new();
        if faces.contains_key(primary) {
            fallback_chain.push(primary.to_string());
        }
        let mut others: Vec<FontId> = faces
            .keys()
            .filter(|k| k.as_str() != primary)
            .cloned()
            .collect();
        others.sort();
        fallback_chain.extend(others);
        /* Issue #329 — the family-name index. */
        let mut by_family: HashMap<String, Vec<FontId>> = HashMap::new();
        for (id, face) in &faces {
            let keys = std::iter::once(family_key(id))
                .chain(face.family_names().iter().map(|n| family_key(n)));
            for key in keys.filter(|k| !k.is_empty()) {
                let ids = by_family.entry(key).or_default();
                if !ids.contains(id) {
                    ids.push(id.clone());
                }
            }
        }
        for ids in by_family.values_mut() {
            ids.sort();
        }
        Self {
            faces,
            by_script,
            fallback_chain,
            by_family,
        }
    }

    /// Issue #329 — the loaded face a document's `family` name stands for
    /// when `script` text is shaped in it, before any per-script fallback:
    ///
    /// 1. **Exact** — `family` is a loaded font id (Backlog #9; wins
    ///    regardless of weight, slant or coverage, as it always has).
    /// 2. **Name** — `family`'s [`family_key`] is a loaded face's id key or
    ///    one of its `name`-table families.
    /// 3. **Substituted** — the [`crate::SUBSTITUTIONS`] row for
    ///    (`family`, the script's [`ScriptClass`]) names a loaded face that
    ///    covers the class (Carlito for Calibri, Noto Naskh Arabic for
    ///    Arabic text in Simplified Arabic or Arial, …).
    ///
    /// Among several faces answering to one name, the one whose own
    /// weight / slant matches `bold` / `italic` wins. `None` when nothing
    /// matches: the caller falls back to the per-script default.
    pub fn resolve_family(
        &self,
        family: &str,
        script: Script,
        bold: bool,
        italic: bool,
    ) -> Option<FamilyResolution<'_>> {
        if let Some((id, face)) = self.faces.get_key_value(family) {
            return Some(FamilyResolution {
                id,
                face: face.as_ref(),
                matched: FamilyMatch::Exact,
                metrics_id: None,
            });
        }
        if let Some((id, face)) = self.pick_family(&family_key(family), None, bold, italic) {
            return Some(FamilyResolution {
                id,
                face,
                matched: FamilyMatch::Name,
                metrics_id: None,
            });
        }
        let class = ScriptClass::of(script);
        let row = substitution_for(family, class)?;
        let (id, face) = row.substitutes.iter().find_map(|name| {
            self.pick_family(&family_key(name), Some(class.probe()), bold, italic)
        })?;
        let metrics_id = row
            .metrics_from
            .and_then(|m| self.pick_family(&family_key(m), None, false, false))
            .map(|(id, _)| id);
        Some(FamilyResolution {
            id,
            face,
            matched: FamilyMatch::Substituted(row),
            metrics_id,
        })
    }

    /// The face answering to family `key` (see [`Self::resolve_family`]):
    /// one covering `probe` when given, the weight / slant match first.
    fn pick_family(
        &self,
        key: &str,
        probe: Option<char>,
        bold: bool,
        italic: bool,
    ) -> Option<(&FontId, &LoadedFont)> {
        let candidates: Vec<(&FontId, &LoadedFont)> = self
            .by_family
            .get(key)?
            .iter()
            .filter_map(|id| self.faces.get_key_value(id))
            .map(|(id, face)| (id, face.as_ref()))
            .filter(|(_, face)| probe.is_none_or(|c| face.covers(c)))
            .collect();
        candidates
            .iter()
            .find(|(_, face)| face.is_bold() == bold && face.is_italic() == italic)
            .or_else(|| candidates.first())
            .copied()
    }

    /// Resolve `script` plus an optional explicit font `family` and the
    /// requested weight/slant to a font id, its loaded face, and the
    /// [`Synthesis`] the renderer must apply. An explicit `family` wins
    /// when it resolves ([`Self::resolve_family`]: a loaded id, a face's
    /// name, or — issue #329 — a metric-compatible / closest-style
    /// substitute); otherwise a real face whose own metadata matches
    /// `bold`/`italic` is used verbatim, falling back to the best covering
    /// face with the missing weight/slant flagged for synthesis (Backlog
    /// #1). `None` only when the stack holds no faces.
    pub fn resolve(
        &self,
        script: Script,
        family: Option<&str>,
        bold: bool,
        italic: bool,
    ) -> Option<(&FontId, &LoadedFont, Synthesis)> {
        self.resolve_detailed(script, family, bold, italic)
            .map(|r| (r.id, r.face, r.synthesis))
    }

    /// Issue #329 — [`Self::resolve`], also reporting how `family`
    /// matched and which face's line metrics stand in for it.
    pub fn resolve_detailed(
        &self,
        script: Script,
        family: Option<&str>,
        bold: bool,
        italic: bool,
    ) -> Option<Resolved<'_>> {
        /* An explicit font-family request wins when it resolves; faux
        styling still fills any weight/slant gap. */
        if let Some(fam) = family
            && let Some(r) = self.resolve_family(fam, script, bold, italic)
        {
            return Some(Resolved {
                id: r.id,
                face: r.face,
                synthesis: Synthesis {
                    faux_bold: bold && !r.face.is_bold(),
                    faux_italic: italic && !r.face.is_italic(),
                },
                matched: Some(r.matched),
                metrics_id: r.metrics_id,
            });
        }
        fn fallback<'a>(
            id: &'a FontId,
            face: &'a LoadedFont,
            synthesis: Synthesis,
        ) -> Resolved<'a> {
            Resolved {
                id,
                face,
                synthesis,
                matched: None,
                metrics_id: None,
            }
        }
        let preferred = self.by_script.get(&script).into_iter().flatten();
        let chain: Vec<&FontId> = preferred.chain(self.fallback_chain.iter()).collect();
        /* A real variant whose own metadata matches the request — no faux. */
        for id in &chain {
            if let Some((key, face)) = self.faces.get_key_value(*id)
                && face.is_bold() == bold
                && face.is_italic() == italic
            {
                return Some(fallback(key, face.as_ref(), Synthesis::default()));
            }
        }
        /* No matching variant — fall back to the first covering face and
        synthesize the missing weight / slant. */
        for id in &chain {
            if let Some((key, face)) = self.faces.get_key_value(*id) {
                return Some(fallback(
                    key,
                    face.as_ref(),
                    Synthesis {
                        faux_bold: bold,
                        faux_italic: italic,
                    },
                ));
            }
        }
        None
    }

    /// The loaded face for an exact font id, if the stack holds it.
    pub fn face(&self, id: &str) -> Option<&LoadedFont> {
        self.faces.get(id).map(|f| f.as_ref())
    }

    /// The first loaded face — in `fallback_chain` order — whose cmap
    /// covers `ch`. Issue #68: `resolve` picks a face by script only and
    /// never checks glyph coverage, so a direction-derived Arabic face
    /// can be selected for a universal list-marker glyph (`◦`, `▪`) it
    /// lacks, mapping it to `.notdef` (invisible). Callers use this to
    /// swap in any loaded face that actually has the glyph before shaping.
    pub fn resolve_covering(&self, ch: char) -> Option<(&FontId, &LoadedFont)> {
        self.fallback_chain
            .iter()
            .filter_map(|id| self.faces.get_key_value(id))
            .find(|(_, face)| face.covers(ch))
            .map(|(key, face)| (key, face.as_ref()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /* Nightly-fuzz regression (#323/#324 follow-up): garbage and empty
    bytes are a typed `Parse` error, and the validation happens before the
    buffer is leaked (LeakSanitizer would flag the stranded buffer). */
    #[test]
    fn parse_rejects_garbage_without_leaking() {
        for bytes in [vec![], vec![0u8; 8], b"not a font at all".to_vec()] {
            assert!(matches!(
                LoadedFont::parse("junk".to_string(), bytes),
                Err(FontError::Parse)
            ));
        }
    }

    /* Issue #422 — a hostile pixel size (the fuzz-restored snapshot asked
    for 20 720 638 px, a 1.4 GB mask) is a typed refusal, not a giant
    allocation; nominal sizes and the cap itself still rasterize. */
    #[test]
    fn rasterize_refuses_sizes_outside_the_cap() {
        let bytes = include_bytes!("../../../ts/fonts/LiberationSans-Regular.ttf").to_vec();
        let font = LoadedFont::parse("lib".to_string(), bytes).expect("font");
        for px in [
            20_720_638.0,
            MAX_RASTER_PX + 1.0,
            f32::INFINITY,
            f32::NAN,
            0.0,
            -12.0,
        ] {
            assert!(
                matches!(font.rasterize('H', px), Err(FontError::SizeOutOfRange)),
                "{px}"
            );
        }
        assert!(font.rasterize('H', 16.0).is_ok());
        let at_cap = font
            .rasterize('H', MAX_RASTER_PX)
            .expect("the cap rasterizes");
        assert!((at_cap.width as usize) * (at_cap.height as usize) <= 4096 * 4096 * 2);
    }

    /* Issue #23 — a dynamically-registered custom font (here `cairo`, the
    `fonts.json` worked example) resolves by its string id for both Latin and
    Arabic, proving the string-backed `FontFamily` reaches the layout engine's
    real glyphs rather than falling back. The bytes are the committed Cairo
    face under `ts/public/fonts/`. */
    fn cairo_stack() -> FontStack {
        let cairo = include_bytes!("../../../ts/public/fonts/Cairo-Regular.ttf").to_vec();
        let liberation = include_bytes!("../../../ts/fonts/LiberationSans-Regular.ttf").to_vec();
        let mut faces: HashMap<FontId, Arc<LoadedFont>> = HashMap::new();
        faces.insert(
            "cairo".to_string(),
            Arc::new(LoadedFont::parse("cairo".to_string(), cairo).expect("parse Cairo")),
        );
        faces.insert(
            "liberation".to_string(),
            Arc::new(
                LoadedFont::parse("liberation".to_string(), liberation).expect("parse Liberation"),
            ),
        );
        // `liberation` is the primary/fallback root — a correct resolve must
        // pick `cairo` *because the family was requested*, not by fallback.
        FontStack::from_faces(faces, "liberation")
    }

    #[test]
    fn custom_font_parses_and_covers_scripts() {
        let stack = cairo_stack();
        let cairo = stack.face("cairo").expect("cairo loaded");
        assert!(cairo.covers('A'), "Cairo should cover Latin");
        assert!(cairo.covers('\u{0628}'), "Cairo should cover Arabic (ب)");
    }

    #[test]
    fn custom_font_id_resolves_to_loaded_face() {
        let stack = cairo_stack();
        let (id, _face, synth) = stack
            .resolve(Script::Latin, Some("cairo"), false, false)
            .expect("resolve latin");
        assert_eq!(id, "cairo", "explicit custom family must win for Latin");
        assert!(!synth.faux_bold && !synth.faux_italic);

        let (id, _face, _synth) = stack
            .resolve(Script::Arabic, Some("cairo"), false, false)
            .expect("resolve arabic");
        assert_eq!(id, "cairo", "explicit custom family must win for Arabic");
    }

    #[test]
    fn unknown_family_id_falls_back_not_panics() {
        // A requested-but-unloaded id resolves through the fallback chain.
        let stack = cairo_stack();
        let (id, _f, _s) = stack
            .resolve(Script::Latin, Some("not-loaded"), false, false)
            .expect("resolve falls back");
        assert!(id == "cairo" || id == "liberation");
    }

    /* Issue #329 — the shipped faces under their `FontFamily` ids, the
    stack the editor and the theme fixtures run with. */
    fn shipped_stack(with_noto: bool) -> FontStack {
        let mut faces: HashMap<FontId, Arc<LoadedFont>> = HashMap::new();
        let mut add = |id: &str, bytes: &[u8]| {
            faces.insert(
                id.to_string(),
                Arc::new(LoadedFont::parse(id.to_string(), bytes.to_vec()).expect("parse")),
            );
        };
        add(
            "liberation",
            include_bytes!("../../../ts/fonts/LiberationSans-Regular.ttf"),
        );
        add(
            "amiri",
            include_bytes!("../../../ts/fonts/Amiri-Regular.ttf"),
        );
        if with_noto {
            add(
                "noto-naskh",
                include_bytes!("../../../ts/fonts/NotoNaskhArabic-Regular.ttf"),
            );
        }
        FontStack::from_faces(faces, "liberation")
    }

    #[test]
    fn name_table_families_are_read() {
        let stack = shipped_stack(true);
        assert_eq!(
            stack.face("liberation").unwrap().family_names(),
            ["Liberation Sans"]
        );
        assert_eq!(
            stack.face("noto-naskh").unwrap().family_names(),
            ["Noto Naskh Arabic"]
        );
        assert_eq!(stack.face("amiri").unwrap().family_names(), ["Amiri"]);
    }

    #[test]
    fn resolve_family_matches_exact_then_name_then_substitution() {
        let stack = shipped_stack(true);
        let r = |fam: &str, script| {
            stack
                .resolve_family(fam, script, false, false)
                .map(|r| (r.id.as_str(), r.matched, r.metrics_id.map(String::as_str)))
        };
        /* A loaded id, and a face's own name in any spelling. */
        assert_eq!(
            r("liberation", Script::Latin),
            Some(("liberation", FamilyMatch::Exact, None))
        );
        assert_eq!(
            r("Liberation Sans", Script::Arabic),
            Some(("liberation", FamilyMatch::Name, None))
        );
        assert_eq!(
            r("noto-naskh-arabic", Script::Arabic),
            Some(("noto-naskh", FamilyMatch::Name, None))
        );
        /* Arial's metric clone for Latin text; the Naskh face for Arabic
        text in the same family, with Arial's (= Liberation Sans's) line
        metrics. */
        let (id, m, metrics) = r("arial", Script::Latin).expect("arial latin");
        assert_eq!((id, metrics), ("liberation", None));
        assert!(matches!(m, FamilyMatch::Substituted(row) if row.metric_compatible));
        let (id, m, metrics) = r("Arial", Script::Arabic).expect("arial arabic");
        assert_eq!((id, metrics), ("noto-naskh", Some("liberation")));
        assert!(matches!(m, FamilyMatch::Substituted(row) if !row.metric_compatible));
        /* The Arabic faces Word documents name. */
        assert_eq!(
            r("simplified-arabic", Script::Arabic).unwrap().0,
            "noto-naskh"
        );
        assert_eq!(r("Sakkal Majalla", Script::Arabic).unwrap().0, "noto-naskh");
        assert_eq!(r("traditional-arabic", Script::Arabic).unwrap().0, "amiri");
        assert_eq!(r("Arabic Typesetting", Script::Arabic).unwrap().0, "amiri");
        /* An Arabic face's Latin text has no row; an unshipped substitute
        (Carlito) leaves the family to the script fallback. */
        assert_eq!(r("simplified-arabic", Script::Latin), None);
        assert_eq!(r("calibri", Script::Latin), None);
        assert_eq!(r("not-a-font", Script::Latin), None);
    }

    #[test]
    fn substitution_walks_the_preference_list_and_checks_coverage() {
        /* Without Noto Naskh, the simplified faces take Amiri (their
        second choice). */
        let stack = shipped_stack(false);
        let (id, _, _) = stack
            .resolve(Script::Arabic, Some("simplified-arabic"), false, false)
            .expect("resolve");
        assert_eq!(id, "amiri");
        /* Liberation Sans has no Arabic: never an Arabic substitute even
        though the row's metrics face is loaded. */
        let r = stack
            .resolve_detailed(Script::Arabic, Some("arial"), false, false)
            .expect("resolve");
        assert_eq!(r.id, "amiri");
        assert_eq!(r.metrics_id.map(String::as_str), Some("liberation"));
        /* `resolve` keeps faux styling on a substitute. */
        let (_, _, synth) = stack
            .resolve(Script::Latin, Some("Times New Roman"), true, false)
            .expect("resolve");
        assert!(synth.faux_bold);
    }

    /* Issue #329 — Word's line metrics, against the published vertical
    metrics of the shipped faces (see `LineMetrics`): Liberation Sans
    (no USE_TYPO_METRICS) takes its win extent plus 67 units of external
    leading — Arial's 1.15 em; Carlito the win extent (Calibri's 1.22 em);
    Amiri and Noto Naskh Arabic set USE_TYPO_METRICS and take the
    typographic values (Amiri 1.758 em, not its 2.76 em win extent). */
    #[test]
    fn line_metrics_follow_words_rule() {
        let lm = |bytes: &[u8]| {
            LoadedFont::parse("f".into(), bytes.to_vec())
                .expect("parse")
                .line_metrics()
        };
        let liberation = lm(include_bytes!(
            "../../../ts/fonts/LiberationSans-Regular.ttf"
        ));
        assert_eq!(
            liberation,
            LineMetrics {
                units_per_em: 2048,
                ascent: 1854,
                descent: 434,
                line_gap: 67
            }
        );
        assert!((liberation.line_height_em() - 1.149).abs() < 0.001);
        let carlito = lm(include_bytes!(
            "../../../ts/public/fonts/Carlito-Regular.ttf"
        ));
        assert_eq!(
            (carlito.ascent, carlito.descent, carlito.line_gap),
            (1950, 550, 0)
        );
        let amiri = lm(include_bytes!("../../../ts/fonts/Amiri-Regular.ttf"));
        assert_eq!(
            (amiri.ascent, amiri.descent, amiri.line_gap),
            (1124, 634, 0)
        );
        let naskh = lm(include_bytes!(
            "../../../ts/fonts/NotoNaskhArabic-Regular.ttf"
        ));
        assert_eq!(
            (naskh.ascent, naskh.descent, naskh.line_gap),
            (1069, 634, 0)
        );
        /* `scaled` puts the gap above the baseline. */
        let (a, d) = liberation.scaled(2048.0);
        assert_eq!((a, d), (1921.0, 434.0));
    }
}
