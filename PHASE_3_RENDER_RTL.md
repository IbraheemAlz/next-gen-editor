# Phase 3 — Canvas Rendering & Native RTL/Arabic Pipeline

> **Parent:** [`MASTER_PLAN.md`](./MASTER_PLAN.md) §4.
> **Owning tracks:** A (Engine), C (Text/i18n), D (Renderer).
> **Calendar:** Months 6–14.
> **Exit gate:** §12.

---

## 1. Objective

Deliver the engine's full text + layout + render stack:

1. **Text pipeline** — Unicode normalization → BiDi → script/style segmentation → shaping → line breaking → justification → visual reorder.
2. **Layout engine** — paragraph, table, page, headers/footers, columns (deferred to v1.1 if needed).
3. **Renderer** — Vello/WebGPU primary; Canvas2D fallback. Tiled raster, dirty tracking.
4. **PDF export** — PDF/A-1b with font subsetting.
5. **Visual-diff golden suite** — 200-document corpus, three-tier tolerance.

Phase 3 is where "Arabic looks right" becomes a binary CI pass/fail.

---

## 2. Deliverables

| ID | Deliverable | Acceptance signal |
| --- | --- | --- |
| D3.1 | Text pipeline end-to-end | Mixed-direction paragraph renders with correct shaping + reorder |
| D3.2 | Layout engine | Paragraph + table + page boxes |
| D3.3 | Vello backend | Renders display list via WebGPU |
| D3.4 | Canvas2D backend | Visual parity with Vello at tolerance 0.5 % |
| D3.5 | Kashida justify | Matches typeset reference (signed off by Arabic typographer) |
| D3.6 | 200-doc visual-diff | Tier-A 100 % pass; Tier-B ≥80 % pass |
| D3.7 | PDF export | veraPDF reports PDF/A-1b conformant |
| D3.8 | Tiled raster + dirty tracking | Scroll 50-page doc ≥60 fps on M2 |

---

## 3. Text pipeline data flow

```
   Logical text + style runs + base direction
                 │
                 ▼
   ┌────────────────────────────┐
   │ NFC normalization          │  icu_normalizer::ComposingNormalizer::new_nfc()
   └─────────────┬──────────────┘
                 ▼
   ┌────────────────────────────┐
   │ BiDi analysis              │  icu_bidi::BidiInfo::new
   │  - paragraph direction     │
   │  - resolved levels per cp  │
   └─────────────┬──────────────┘
                 ▼
   ┌────────────────────────────┐
   │ Run segmentation           │  custom: split on script + style + level
   └─────────────┬──────────────┘
                 ▼
   ┌────────────────────────────┐
   │ Shaping (per run)          │  rustybuzz::shape
   │  - direction / script      │
   │  - language tag            │
   │  - OpenType features       │
   └─────────────┬──────────────┘
                 ▼
   ┌────────────────────────────┐
   │ Line break opportunities   │  icu_segmenter::LineSegmenter
   └─────────────┬──────────────┘
                 ▼
   ┌────────────────────────────┐
   │ Line composition           │  greedy MVP; Knuth-Plass post-MVP
   └─────────────┬──────────────┘
                 ▼
   ┌────────────────────────────┐
   │ Justification              │  Kashida (Arabic) + space (Latin)
   └─────────────┬──────────────┘
                 ▼
   ┌────────────────────────────┐
   │ Visual reorder per line    │  by BiDi level
   └─────────────┬──────────────┘
                 ▼
   ┌────────────────────────────┐
   │ Glyph positions → render   │
   └────────────────────────────┘
```

---

## 4. `lay_out_paragraph` reference implementation

`crates/text-pipeline/src/paragraph.rs`:

```rust
use icu_bidi::{BidiInfo, Level, Direction as BidiDir};
use icu_normalizer::ComposingNormalizer;
use icu_segmenter::LineSegmenter;
use rustybuzz::UnicodeBuffer;

pub fn lay_out_paragraph(
    text: &str,
    style_runs: &[StyleRun],
    base_dir: Direction,
    width: f32,
    fonts: &FontStack,
) -> Vec<LineBox> {
    /* 1. Normalize */
    let nfc = ComposingNormalizer::new_nfc();
    let text = nfc.normalize(text);

    /* 2. BiDi */
    let bdir = match base_dir {
        Direction::Ltr => Some(BidiDir::Ltr),
        Direction::Rtl => Some(BidiDir::Rtl),
        Direction::Auto => None,
    };
    let bidi = BidiInfo::new(&text, bdir);
    let para = &bidi.paragraphs[0];
    let levels: Vec<Level> = bidi.reordered_levels(para, para.range.clone());

    /* 3. Segment into shaping runs (script + style + level) */
    let shaping_runs = segment_runs(&text, &levels, style_runs);

    /* 4. Shape each run */
    let mut shaped: Vec<ShapedRun> = Vec::with_capacity(shaping_runs.len());
    for run in &shaping_runs {
        let face = fonts.resolve(run.script, &run.attrs);
        let mut buf = UnicodeBuffer::new();
        buf.push_str(&text[run.range.clone()]);
        buf.set_direction(run.direction());
        buf.set_script(run.script);
        if let Some(lang) = &run.language { buf.set_language(*lang); }
        let glyphs = rustybuzz::shape(face.as_face(), run.features.as_slice(), buf);
        shaped.push(ShapedRun {
            glyphs,
            font: face.id,
            range: run.range.clone(),
            level: run.level,
            attrs: run.attrs.clone(),
        });
    }

    /* 5. Line break opportunities */
    let segmenter = LineSegmenter::new_auto();
    let breaks: Vec<usize> = segmenter.segment_str(&text).collect();

    /* 6. Greedy composition */
    let mut lines = greedy_compose(&shaped, &breaks, width);

    /* 7. Justification */
    for line in &mut lines {
        match (line.alignment, line.has_arabic()) {
            (Alignment::Justify, true)  => apply_kashida(line),
            (Alignment::Justify, false) => apply_space_justify(line),
            _ => {}
        }
    }

    /* 8. Visual reorder per line by BiDi level */
    for line in &mut lines {
        line.reorder_visual(&levels);
    }

    lines
}
```

---

## 5. Box model (`crates/layout/src/boxes.rs`)

```rust
pub struct ParagraphBox {
    pub origin: Point,
    pub size: Size,
    pub lines: Vec<LineBox>,
    pub direction: Direction,
    pub style_id: ParagraphStyleId,
}

pub struct LineBox {
    pub origin: Point,        /* relative to paragraph */
    pub baseline: f32,        /* offset from line origin to baseline */
    pub height: f32,
    pub width: f32,
    pub runs: Vec<VisualRun>, /* visual (after reorder) */
    pub justify: JustifyInfo,
    pub alignment: Alignment,
}

pub struct VisualRun {
    pub glyphs: Vec<PositionedGlyph>,
    pub font: FontId,
    pub direction: Direction,
    pub script: Script,
    pub source_range: LogicalRange,
    pub attrs: TextAttrs,
}

pub struct PositionedGlyph {
    pub id: u16,
    pub cluster: u32,         /* offset into source_range */
    pub x_advance: f32,
    pub y_advance: f32,
    pub x_offset: f32,
    pub y_offset: f32,
}

pub struct PageBox {
    pub size: Size,           /* e.g. A4 */
    pub margins: Margins,
    pub header: Option<HeaderFooterBox>,
    pub footer: Option<HeaderFooterBox>,
    pub paragraphs: Vec<ParagraphBox>,
}
```

---

## 6. Kashida justification algorithm

`crates/text-pipeline/src/justify_kashida.rs`:

```rust
/// Apply Kashida (Arabic letter elongation) justification to a line.
/// Reference: ISO/IEC TR 14652; Microsoft "Arabic Typography Guide".
pub fn apply_kashida(line: &mut LineBox) {
    let target_w = line.target_width;
    let natural_w = line.natural_width();
    let extra = target_w - natural_w;
    if extra <= 0.0 { return; }

    /* Step 1. Identify candidate positions, prioritized by Arabic shaping rules. */
    let candidates = identify_kashida_candidates(&line.runs);

    /* Priority bands:
       P1: after seen/sheen (س/ش) before a final form
       P2: after sad/dad (ص/ض)
       P3: after ta/tha (ط/ث)
       P4: after lam/non-final (ل with joining)
       P5: anywhere in cursive joining group not already prioritized
       Lowest: after the first character of the word as last resort */

    /* Step 2. Distribute extra width across top-priority candidates first.
       Each Kashida insertion stretches the joining stroke; metrics:
       - For glyph with Kashida anchor: stretch by inserting U+0640 (tatweel)
       - OR use OpenType `cv` features that stretch (font-dependent)
       Recommend: insert tatweel glyph runs for portability across fonts. */
    let mut remaining = extra;
    for band in [Priority::P1, Priority::P2, Priority::P3, Priority::P4, Priority::P5] {
        let positions: Vec<_> = candidates.iter().filter(|c| c.priority == band).collect();
        if positions.is_empty() { continue; }
        let per = remaining / positions.len() as f32;
        for pos in &positions {
            insert_kashida_at(&mut line.runs, pos, per);
        }
        remaining = 0.0;
        break;
    }

    /* Step 3. If still under-filled (no Arabic on the line, fully Latin), apply space justify. */
    if remaining > 0.001 {
        apply_space_justify(line);
    }
}
```

Acceptance: signed off by Arabic typographer against a 30-line corpus of book-quality Naskh typesetting reference (Amiri).

---

## 7. BiDi cursor/selection mapping

Critical for editing on mixed-direction text.

```rust
pub fn logical_to_visual(line: &LineBox, logical_pos: u32) -> Vec<Rect> {
    /* A single logical position may map to two visual rectangles
       (at the boundary of an RTL/LTR run). Return both for caret;
       caller picks based on direction policy. */
    let mut out = vec![];
    for run in &line.runs {
        if run.source_range.contains(logical_pos) {
            out.push(run.caret_rect_at(logical_pos));
        }
        if run.source_range.start == logical_pos || run.source_range.end == logical_pos {
            out.push(run.caret_rect_at_boundary(logical_pos));
        }
    }
    dedup_adjacent(out)
}

pub fn selection_rects(line: &LineBox, range: LogicalRange) -> Vec<Rect> {
    /* For RTL-spanning selection, multiple discontinuous rectangles
       may appear on the same visual line. Standard Word/Pages behavior. */
    let mut rects = vec![];
    for run in &line.runs {
        if let Some(intersect) = run.source_range.intersect(range) {
            rects.push(run.bounding_rect_for(intersect));
        }
    }
    merge_collinear(rects)
}
```

---

## 8. Run segmentation (script + style + level)

`crates/text-pipeline/src/segment.rs`:

```rust
pub fn segment_runs(
    text: &str,
    levels: &[Level],
    style_runs: &[StyleRun],
) -> Vec<ShapingRun> {
    let mut runs = vec![];
    let mut i = 0;
    while i < text.len() {
        let level = levels[i];
        let style = style_at(style_runs, i);
        let script = script_at(text, i);
        let language = language_at(style_runs, i);

        let mut j = i + char_len(text, i);
        while j < text.len() {
            if levels[j] != level { break; }
            if script_at(text, j) != script { break; }
            if style_at(style_runs, j) != style { break; }
            j += char_len(text, j);
        }

        runs.push(ShapingRun {
            range: i..j,
            level,
            script,
            attrs: style.clone(),
            language,
            features: features_for(script, &style),
        });
        i = j;
    }
    runs
}

fn features_for(script: Script, attrs: &TextAttrs) -> Vec<rustybuzz::Feature> {
    let mut f = vec![];
    /* Always-on for Arabic: 'init', 'medi', 'fina', 'isol', 'rlig', 'calt' */
    if script == Script::Arabic {
        f.push(rustybuzz::Feature::from_bytes(b"init", 1, ..));
        /* … */
    }
    if attrs.small_caps { f.push(rustybuzz::Feature::from_bytes(b"smcp", 1, ..)); }
    if attrs.tabular_nums { f.push(rustybuzz::Feature::from_bytes(b"tnum", 1, ..)); }
    f
}
```

---

## 9. Renderer architecture

### 9.1 Display list (backend-agnostic)

```rust
pub enum DisplayCmd {
    FillRect { rect: Rect, color: Color },
    StrokeRect { rect: Rect, color: Color, width: f32 },
    DrawGlyphRun { run: GlyphRun, transform: Affine, paint: Paint },
    DrawPath { path: Path, paint: Paint, fill_rule: FillRule },
    DrawImage { id: ImageId, dest: Rect, src: Rect, opacity: f32 },
    PushClip { rect: Rect },
    PopClip,
    PushTransform(Affine),
    PopTransform,
    PushLayer { opacity: f32, blend: BlendMode },
    PopLayer,
}

pub struct GlyphRun {
    pub font: FontId,
    pub px_size: f32,
    pub glyphs: Vec<PositionedGlyph>,
    pub baseline: Point,
}
```

Layout emits `Vec<DisplayCmd>`. Renderer interprets.

### 9.2 Vello backend (primary, WebGPU)

```rust
pub fn render_vello(scene: &mut vello::Scene, cmds: &[DisplayCmd], fonts: &FontStack) {
    for cmd in cmds {
        match cmd {
            DisplayCmd::DrawGlyphRun { run, transform, paint } => {
                let face = fonts.get(run.font);
                scene.draw_glyphs(&face.vello_font())
                    .font_size(run.px_size)
                    .transform(*transform)
                    .brush(paint.into_brush())
                    .draw(vello::peniko::Fill::NonZero,
                          run.glyphs.iter().map(|g| vello::Glyph {
                              id: g.id as u32,
                              x: g.x_offset,
                              y: g.y_offset,
                          }));
            }
            DisplayCmd::FillRect { rect, color } => {
                scene.fill(vello::peniko::Fill::NonZero,
                           vello::kurbo::Affine::IDENTITY,
                           &color.into_brush(),
                           None,
                           &rect.to_kurbo());
            }
            /* … */
        }
    }
}
```

Detection at engine init:

```rust
let has_gpu = web_sys::window().unwrap().navigator().gpu().is_some();
self.backend = if has_gpu { RendererBackend::Vello } else { RendererBackend::Canvas2d };
```

### 9.3 Canvas2D backend (fallback)

```rust
pub fn render_canvas2d(
    ctx: &OffscreenCanvasRenderingContext2d,
    cmds: &[DisplayCmd],
    atlas: &mut GlyphAtlas,
    fonts: &FontStack,
) {
    for cmd in cmds {
        match cmd {
            DisplayCmd::DrawGlyphRun { run, transform, paint } => {
                ctx.save();
                apply_transform(ctx, transform);
                for g in &run.glyphs {
                    let key = GlyphKey {
                        font_id: run.font,
                        glyph_id: g.id,
                        px_size: (run.px_size * 100.0) as u16,
                        subpixel_x: ((g.x_offset.fract() * 4.0) as u8) & 3,
                        subpixel_y: ((g.y_offset.fract() * 4.0) as u8) & 3,
                    };
                    let entry = atlas.get_or_insert(key, || rasterize(&fonts.get(run.font), &key));
                    ctx.draw_image_with_image_bitmap_and_dw_and_dh(
                        &entry.bitmap, g.x_offset.into(), g.y_offset.into(),
                        entry.bitmap.width().into(), entry.bitmap.height().into(),
                    ).unwrap();
                }
                ctx.restore();
            }
            /* … */
        }
    }
}
```

### 9.4 Tiled raster + dirty tracking

```rust
pub struct DirtyTracker {
    tiles: BTreeSet<TileId>,
    tile_size: u32,
}

impl DirtyTracker {
    pub fn invalidate(&mut self, rect: Rect) {
        for tile in tiles_intersecting(rect, self.tile_size) {
            self.tiles.insert(tile);
        }
    }
    pub fn drain(&mut self) -> Vec<TileId> { self.tiles.drain(..).collect() }
}
```

Layout emits dirty rects per command. Renderer re-rasterizes only touched tiles. Target: scroll p95 ≤16 ms on M2.

---

## 10. PDF export pipeline

`crates/format-docx/src/pdf_export.rs`:

```rust
pub fn export_pdf(
    doc: &Document,
    out: &mut Vec<u8>,
    conformance: PdfConformance,
) -> Result<(), PdfError> {
    use pdf_writer::{Pdf, Ref, Name, Filter};

    let mut pdf = Pdf::new();
    pdf.set_conformance(conformance);
    let catalog_id = Ref::new(1);
    let pages_id = Ref::new(2);

    /* Subset + embed fonts */
    let mut font_refs = HashMap::new();
    for face in doc.fonts.iter_used() {
        let subset = subset_font(face, doc.collected_glyphs(face))?;
        font_refs.insert(face.id, embed_font(&mut pdf, &subset)?);
    }

    /* Render each page at 300 DPI */
    let mut page_refs = vec![];
    for page in &doc.pages {
        let cmds = layout_page(page);
        let page_ref = emit_page(&mut pdf, &cmds, &font_refs)?;
        page_refs.push(page_ref);
    }

    pdf.catalog(catalog_id).pages(pages_id);
    pdf.pages(pages_id).kids(page_refs.iter().copied()).count(page_refs.len() as i32);
    pdf.xmp_metadata(make_xmp(doc, conformance))?;
    out.extend_from_slice(&pdf.finish());
    Ok(())
}
```

Validation in CI:

```bash
veraPDF --format text --profile 1b out/exported.pdf
```

---

## 11. Visual diff suite

200-document corpus split by feature:

| Bucket | Count | Focus |
| --- | --- | --- |
| Plain prose (en/ar/mixed) | 40 | Baseline shaping + BiDi |
| Tables | 30 | Cell layout, borders, RTL tables |
| Lists | 20 | Nested, numbered (Arabic-Indic numerals) |
| Images / floats | 20 | Inline, anchored, wrap |
| Headers / footers / page nums | 20 | Page master + Arabic numerals |
| Footnotes / endnotes | 15 | Note reference + RTL footnote area |
| Comments + tracked changes | 15 | Margin notes, visual indicators |
| Equations (OMML) | 10 | Math rendering |
| SmartArt / DrawingML | 10 | Vector shapes |
| Edge cases | 20 | 1000-char paragraph, deeply nested lists, mixed scripts |

Per-doc manifest entry:

```json
{
    "id": "ar-table-005",
    "tier": "A",
    "features": ["bidi", "table", "rtl-cell-order"],
    "lang": ["ar", "en"],
    "pages": 3,
    "source": "synthetic",
    "kashida_required": true
}
```

Visual diff CI step:

```bash
node tools/visual-diff/run.mjs --tier A --tol 0.005     # MVP: 100% pass
node tools/visual-diff/run.mjs --tier B --tol 0.020 --threshold 0.80
```

---

## 12. Exit gate (Phase 3)

```bash
# 1. HarfBuzz shape parity (full corpus)
cargo run --release --bin shape-regression -- tests/corpus/shape/

# 2. BiDi conformance (UCD BidiTest)
cargo run --release --bin bidi-regression -- tests/corpus/bidi/

# 3. Visual diff Tier-A 100%
node tools/visual-diff/run.mjs --tier A --threshold 1.00

# 4. Visual diff Tier-B ≥80%
node tools/visual-diff/run.mjs --tier B --threshold 0.80

# 5. PDF/A-1b across Tier-A
node tools/pdf-validate/run.mjs --corpus tier-a --profile 1b

# 6. Kashida acceptance (manual + automated)
node tools/visual-diff/run.mjs --case-glob kashida-*

# 7. Scroll perf 50-page Arabic doc ≥60 fps
node tools/perf/scroll.mjs tests/corpus/ar-50p.docx --min-fps 60

# 8. Memory still under 256 MB on 50p
node tools/memory-profile/run.mjs --doc 50p
```

---

## 13. Risk register (Phase 3 specific)

| # | Risk | Likelihood | Detection | Mitigation |
| --- | --- | --- | --- | --- |
| 1 | `rustybuzz` lacks GSUB feature; Arabic word renders wrong | Med | Shape regression in CI | Escape to HarfBuzz-WASM module loaded on demand |
| 2 | Vello not production stable; visual regressions | Med | Tier-A diff | Pin known-good Vello tag; switch to Canvas2D primary if needed |
| 3 | WebGPU unavailable on Safari at MVP date | Med | Browser matrix | Canvas2D backend is at parity with Vello on Tier-A |
| 4 | Kashida policy disagrees with native typographer | High | Phase-3 review | Iterate with consultant in months 9–12; ship configurable Kashida strategy |
| 5 | Table layout corner cases blow timeline | High | Burndown | Defer non-trivial table features (vertical text, multi-col headers) to v1.1 |
| 6 | PDF font subsetting bugs | Med | veraPDF | Use battle-tested subsetter (`subsetter` crate); embed full font if subset fails |

---

## 14. Hand-off into Phase 4

End of Phase 3 the engine paints; Phase 4 wires the UI. Phase 3 must deliver:

- Engine emits `SelectionChanged`, `Painted`, `AccessibilityTreeChanged`, `FormattingChanged` events.
- Engine accepts `SetSelection`, `InsertText`, `ApplyFormatting`, `SetViewport` from any caller.
- Hit-testing implemented: pointer coords + caret rect round-trip exact within 1 px.
- Display list emission is observable from outside (debug Snapshot via `Command::RequestStats` + dev tool).
