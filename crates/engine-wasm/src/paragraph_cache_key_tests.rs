//! Issue #456 — the paragraph layout cache key sees every layout input
//! (and only those). The compile-time half is the exhaustive destructure
//! in `paragraph_cache_key.rs`; this is the behavioural half: changing a
//! hashed field changes the key, changing a layout-irrelevant field does
//! not, and the verified-hit backstop still catches a bad entry.

use super::*;

fn sctx() -> StyleContext<'static> {
    StyleContext {
        styles: Box::leak(Box::default()),
        run_defaults: Box::leak(Box::default()),
        note_markers: None,
        note_self_mark: None,
        theme: None,
        theme_key: 0,
        settings: None,
    }
}

fn cfg() -> RenderConfig {
    RenderConfig {
        font_id: "Amiri".to_string(),
        base_direction: ShapingDirection::Ltr,
        px_size: 16.0,
        line_height: 1.4,
        alignment: Alignment::Start,
        scale: 1.0,
        base_scale: 1.0,
        zoom: 1.0,
    }
}

fn revision(kind: engine::RevisionKind) -> engine::Revision {
    engine::Revision {
        start: 0,
        end: 4,
        kind,
        author: "a".into(),
        date: "2026-01-01T00:00:00Z".into(),
        id: Some(1),
        prev_attrs: None,
        move_name: None,
    }
}

fn run(start: u32, end: u32, style: engine::SpanStyle) -> engine::StyleRun {
    engine::StyleRun { start, end, style }
}

/// A paragraph exercising every collection the key walks.
fn base() -> engine::Paragraph {
    let mut p = engine::Paragraph {
        text: "tab\there \u{FFFC} linked words".to_string(),
        ..Default::default()
    };
    p.spans = vec![run(
        0,
        3,
        engine::SpanStyle {
            bold: Some(true),
            ..Default::default()
        },
    )];
    p.props.tab_stops = vec![engine::TabStop {
        position_pt: 72.0,
        kind: engine::TabKind::Left,
        leader: engine::TabLeader::None,
    }];
    p.hyperlinks = vec![engine::Hyperlink {
        start: 9,
        end: 15,
        target: "https://example.com".into(),
        attrs: Vec::new(),
    }];
    p.revisions = vec![revision(engine::RevisionKind::Delete)];
    p.inline_objects = vec![engine::InlineObject {
        at: 9,
        kind: engine::InlineKind::Image {
            rel_id: "rId1".into(),
            width_emu: 914_400,
            height_emu: 914_400,
            media_key: None,
        },
        anchor: Some(Box::new(engine::FloatAnchor::default())),
        source_xml: None,
    }];
    p
}

fn key(p: &engine::Paragraph) -> u64 {
    paragraph_layout_key(p, &cfg(), 1.0, 451.0, sctx())
}

fn anchor_mut(p: &mut engine::Paragraph) -> &mut engine::FloatAnchor {
    p.inline_objects[0].anchor.as_deref_mut().expect("anchor")
}

fn span_style_mut(p: &mut engine::Paragraph) -> &mut engine::SpanStyle {
    &mut p.spans[0].style
}

type Mutation = (&'static str, fn(&mut engine::Paragraph));

/// Every hashed field: changing it must change the key.
#[test]
fn every_hashed_paragraph_field_changes_the_key() {
    let hashed: &[Mutation] = &[
        ("text", |p| p.text.push('x')),
        ("props.alignment", |p| {
            p.props.alignment = Some(EngineAlignment::Center)
        }),
        ("props.direction", |p| {
            p.props.direction = Some(engine::TextDirection::Rtl)
        }),
        ("props.indent.start", |p| p.props.indent.start_twips = 240),
        ("props.indent.end", |p| p.props.indent.end_twips = 240),
        ("props.indent.first_line", |p| {
            p.props.indent.first_line_twips = 240
        }),
        ("props.indent.hanging", |p| {
            p.props.indent.hanging_twips = 240
        }),
        ("props.line_height", |p| {
            p.props.line_height = Some(engine::LineHeight::Exact { twips: 300 })
        }),
        ("props.suppress_auto_hyphens", |p| {
            p.props.suppress_auto_hyphens = Some(true)
        }),
        ("props.tab_stops.position", |p| {
            p.props.tab_stops[0].position_pt = 96.0
        }),
        // Issue #346.
        ("props.tab_stops.kind", |p| {
            p.props.tab_stops[0].kind = engine::TabKind::Right
        }),
        ("props.tab_stops.leader", |p| {
            p.props.tab_stops[0].leader = engine::TabLeader::Dot
        }),
        ("props.tab_stops.len", |p| {
            p.props.tab_stops.push(engine::TabStop::default())
        }),
        ("resolved_marker", |p| p.resolved_marker = Some("1.".into())),
        ("resolved_list_indent", |p| {
            p.resolved_list_indent = Some(engine::Indent {
                start_twips: 720,
                ..Default::default()
            })
        }),
        ("spans.len", |p| p.spans.push(run(4, 5, Default::default()))),
        ("spans.start", |p| p.spans[0].start = 1),
        ("spans.end", |p| p.spans[0].end = 2),
        ("style.font_size", |p| {
            span_style_mut(p).font_size = Some(20.0)
        }),
        ("style.font_size_cs", |p| {
            span_style_mut(p).font_size_cs = Some(20.0)
        }),
        ("style.color", |p| {
            span_style_mut(p).color = Some([1, 2, 3, 255])
        }),
        ("style.bold", |p| span_style_mut(p).bold = Some(false)),
        ("style.bold_cs", |p| span_style_mut(p).bold_cs = Some(true)),
        ("style.italic", |p| span_style_mut(p).italic = Some(true)),
        ("style.italic_cs", |p| {
            span_style_mut(p).italic_cs = Some(true)
        }),
        ("style.underline", |p| {
            span_style_mut(p).underline = Some(engine::UnderlineStyle::Single)
        }),
        ("style.strike", |p| span_style_mut(p).strike = Some(true)),
        ("style.bg_color", |p| {
            span_style_mut(p).bg_color = Some([9, 9, 9, 255])
        }),
        ("style.font_family", |p| {
            span_style_mut(p).font_family = Some(engine::FontFamily::Amiri)
        }),
        ("style.font_family_cs", |p| {
            span_style_mut(p).font_family_cs = Some(engine::FontFamily::Amiri)
        }),
        ("style.caps", |p| span_style_mut(p).caps = Some(true)),
        ("style.small_caps", |p| {
            span_style_mut(p).small_caps = Some(true)
        }),
        ("style.vert_align", |p| {
            span_style_mut(p).vert_align = Some(engine::VertAlign::Superscript)
        }),
        ("style.raw_font_family", |p| {
            span_style_mut(p).raw_font_family = Some("Fancy".into())
        }),
        ("style.font_theme", |p| {
            span_style_mut(p).font_theme = Some("minorHAnsi".into())
        }),
        ("style.font_bindings", |p| {
            span_style_mut(p).font_bindings = Some(Box::default())
        }),
        ("style.color_theme", |p| {
            span_style_mut(p).color_theme = Some(Box::default())
        }),
        ("style.forces_complex_script", |p| {
            engine::GrabBag::push_into(&mut span_style_mut(p).grab_bag, b"<w:rtl/>".to_vec())
        }),
        ("inline_objects.len", |p| {
            p.inline_objects.push(p.inline_objects[0].clone())
        }),
        ("inline_objects.at", |p| p.inline_objects[0].at = 10),
        ("inline_objects.image.width", |p| {
            p.inline_objects[0].kind = engine::InlineKind::Image {
                rel_id: "rId1".into(),
                width_emu: 1,
                height_emu: 914_400,
                media_key: None,
            }
        }),
        ("inline_objects.kind", |p| {
            p.inline_objects[0].kind = engine::InlineKind::Symbol {
                font: "Wingdings".into(),
                char: "a".into(),
            }
        }),
        ("inline_objects.anchor.none", |p| {
            p.inline_objects[0].anchor = None
        }),
        ("anchor.position_h", |p| {
            anchor_mut(p).position_h.relative_from = engine::HRelativeFrom::Character
        }),
        ("anchor.simple_pos", |p| anchor_mut(p).simple_pos = true),
        ("anchor.simple_pos_x", |p| {
            anchor_mut(p).simple_pos_x_emu = 5
        }),
        ("anchor.simple_pos_y", |p| {
            anchor_mut(p).simple_pos_y_emu = 5
        }),
        ("anchor.relative_height", |p| {
            anchor_mut(p).relative_height = 1
        }),
        ("anchor.behind_doc", |p| anchor_mut(p).behind_doc = true),
        ("anchor.hidden", |p| anchor_mut(p).hidden = true),
        ("anchor.dist_top", |p| anchor_mut(p).dist_top_emu = 9),
        ("anchor.dist_bottom", |p| anchor_mut(p).dist_bottom_emu = 9),
        ("anchor.dist_left", |p| anchor_mut(p).dist_left_emu = 9),
        ("anchor.dist_right", |p| anchor_mut(p).dist_right_emu = 9),
        ("anchor.wrap", |p| {
            anchor_mut(p).wrap = engine::WrapKind::Square
        }),
        ("anchor.wrap_text", |p| {
            anchor_mut(p).wrap_text = engine::WrapText::Left
        }),
        ("anchor.wrap_polygon", |p| {
            anchor_mut(p).wrap_polygon = Some(vec![(0, 0), (1, 1)])
        }),
        ("hyperlinks.len", |p| p.hyperlinks.clear()),
        ("hyperlinks.start", |p| p.hyperlinks[0].start = 10),
        ("hyperlinks.end", |p| p.hyperlinks[0].end = 16),
        // An internal anchor link renders in the run's own look.
        ("hyperlinks.internal_target", |p| {
            p.hyperlinks[0].target = "#bookmark".into()
        }),
        ("revisions.len", |p| p.revisions.clear()),
        ("revisions.start", |p| p.revisions[0].start = 1),
        ("revisions.end", |p| p.revisions[0].end = 3),
        // Issue #307 — Delete (strike) vs FormatChange (no visual) vs Insert.
        ("revisions.kind delete->format", |p| {
            p.revisions[0].kind = engine::RevisionKind::FormatChange
        }),
        ("revisions.kind delete->insert", |p| {
            p.revisions[0].kind = engine::RevisionKind::Insert
        }),
        ("revisions.kind delete->move_from", |p| {
            p.revisions[0].kind = engine::RevisionKind::MoveFrom
        }),
        ("revisions.kind delete->move_to", |p| {
            p.revisions[0].kind = engine::RevisionKind::MoveTo
        }),
    ];
    let before = key(&base());
    assert_eq!(before, key(&base()), "the key is deterministic");
    for (name, mutate) in hashed {
        let mut p = base();
        mutate(&mut p);
        assert_ne!(
            before,
            key(&p),
            "hashed field `{name}` did not move the key"
        );
    }
}

/// Every layout-irrelevant field: changing it must NOT change the key
/// (the reason for each is in `paragraph_cache_key.rs`).
#[test]
fn layout_irrelevant_fields_do_not_change_the_key() {
    let irrelevant: &[Mutation] = &[
        ("props.spacing", |p| p.props.spacing.before_twips = 240),
        ("props.keep_next", |p| p.props.keep_next = Some(true)),
        ("props.keep_lines", |p| p.props.keep_lines = Some(true)),
        ("props.page_break_before", |p| {
            p.props.page_break_before = true
        }),
        ("props.borders", |p| {
            p.props.borders = Some(engine::CellBorders::default())
        }),
        ("props.shading", |p| p.props.shading = Some([1, 2, 3, 255])),
        ("props.outline_level", |p| p.props.outline_level = Some(1)),
        ("props.widow_control", |p| {
            p.props.widow_control = Some(false)
        }),
        ("props.list_item", |p| {
            p.props.list_item = Some(engine::ListItem { num_id: 1, ilvl: 0 })
        }),
        ("list_item", |p| {
            p.list_item = Some(engine::ListItem { num_id: 1, ilvl: 0 })
        }),
        ("dirty", |p| p.dirty = true),
        ("source_xml", |p| p.source_xml = Some(b"<w:p/>".to_vec())),
        ("direct_overrides", |p| {
            p.direct_overrides.indent.start_twips = 99
        }),
        ("section_end", |p| p.section_end = Some(Box::default())),
        ("bookmarks", |p| {
            p.bookmarks.push(engine::Bookmark::default())
        }),
        ("mark_revisions", |p| {
            p.mark_revisions
                .push(revision(engine::RevisionKind::Insert))
        }),
        // Only an EMPTY paragraph is sized by its mark (issue #370).
        ("mark_style (paragraph with text)", |p| {
            p.mark_style = Some(Box::new(engine::SpanStyle {
                font_size: Some(40.0),
                ..Default::default()
            }))
        }),
        ("revisions.author", |p| p.revisions[0].author = "bob".into()),
        ("revisions.date", |p| p.revisions[0].date = "x".into()),
        ("revisions.id", |p| p.revisions[0].id = Some(77)),
        ("hyperlinks.external_target", |p| {
            p.hyperlinks[0].target = "https://other.example".into()
        }),
        ("hyperlinks.attrs", |p| p.hyperlinks[0].attrs.clear()),
        ("style.char_style", |p| {
            span_style_mut(p).char_style = Some("Emph".into())
        }),
        ("style.lang", |p| {
            span_style_mut(p).lang = Some(Box::new(engine::Lang {
                val: Some("en-US".into()),
                ..Default::default()
            }))
        }),
        ("anchor.locked", |p| anchor_mut(p).locked = true),
        ("anchor.layout_in_cell", |p| {
            anchor_mut(p).layout_in_cell = false
        }),
        ("anchor.allow_overlap", |p| {
            anchor_mut(p).allow_overlap = false
        }),
        ("anchor.wrap_xml", |p| {
            anchor_mut(p).wrap_xml = Some("<x/>".into())
        }),
        ("anchor.doc_pr_xml", |p| {
            anchor_mut(p).doc_pr_xml = Some("<x/>".into())
        }),
        ("inline_objects.source_xml", |p| {
            p.inline_objects[0].source_xml = Some(b"<w:drawing/>".to_vec())
        }),
    ];
    let before = key(&base());
    for (name, mutate) in irrelevant {
        let mut p = base();
        mutate(&mut p);
        assert_eq!(before, key(&p), "irrelevant field `{name}` moved the key");
    }
}

/// An EMPTY paragraph is sized by its mark (issue #370), so there the
/// mark style is an input.
#[test]
fn empty_paragraph_mark_style_is_hashed() {
    let empty = engine::Paragraph::default();
    let mut marked = empty.clone();
    marked.mark_style = Some(Box::new(engine::SpanStyle {
        font_size: Some(40.0),
        ..Default::default()
    }));
    assert_ne!(key(&empty), key(&marked));
    let mut bold = marked.clone();
    bold.mark_style.as_mut().expect("mark").bold = Some(true);
    assert_ne!(key(&marked), key(&bold));
}

/// The config / width / scale half of the key.
#[test]
fn render_config_width_and_scale_change_the_key() {
    let p = base();
    let k = key(&p);
    let with = |f: fn(&mut RenderConfig)| {
        let mut c = cfg();
        f(&mut c);
        paragraph_layout_key(&p, &c, 1.0, 451.0, sctx())
    };
    assert_ne!(k, with(|c| c.font_id = "Other".into()));
    assert_ne!(k, with(|c| c.base_direction = ShapingDirection::Rtl));
    assert_ne!(k, with(|c| c.px_size = 20.0));
    assert_ne!(k, with(|c| c.line_height = 2.0));
    assert_ne!(k, with(|c| c.alignment = Alignment::Center));
    assert_ne!(k, paragraph_layout_key(&p, &cfg(), 2.0, 451.0, sctx()));
    assert_ne!(k, paragraph_layout_key(&p, &cfg(), 1.0, 220.0, sctx()));
    // Paint-side scale / zoom are not layout inputs.
    assert_eq!(k, with(|c| c.scale = 3.0));
    assert_eq!(k, with(|c| c.base_scale = 3.0));
    assert_eq!(k, with(|c| c.zoom = 2.0));
}

/// Issue #346 — the stale-undo scenario at the key level: the same stop
/// position with a different kind / leader must not share a cache entry.
#[test]
fn tab_kind_and_leader_at_the_same_position_do_not_collide() {
    let mut left = base();
    left.props.tab_stops[0].kind = engine::TabKind::Left;
    let mut right = left.clone();
    right.props.tab_stops[0].kind = engine::TabKind::Right;
    let mut dotted = left.clone();
    dotted.props.tab_stops[0].leader = engine::TabLeader::Dot;
    assert_ne!(key(&left), key(&right));
    assert_ne!(key(&left), key(&dotted));
}

/// `cached_paragraph_is_consistent` stays the runtime backstop: a cached
/// box of the wrong width under a (forced) matching key is dropped,
/// re-laid and reported as a `CacheMismatch`.
#[test]
fn verified_hit_backstop_still_catches_a_bad_entry() {
    let engine = tests::test_engine_with_doc(DocumentTree::from_text("hello wide world"));
    let cfg = engine.layout_cfg.clone().expect("cfg");
    let doc = engine.undo.current().clone();
    let sctx = StyleContext::of(&doc);
    let fonts = FontStack::from_faces(engine.fonts.clone(), &cfg.font_id);
    let para = doc
        .blocks
        .get(0)
        .and_then(engine::Block::as_paragraph)
        .expect("paragraph")
        .clone();
    let mut cache = LruCache::new(NonZeroUsize::new(8).expect("cap"));
    let good = layout_paragraph_cached(&para, &fonts, &cfg, 1.0, 400.0, sctx, &mut cache);
    let k = paragraph_layout_key(&para, &cfg, 1.0, 400.0, sctx);
    let mut bad = good.clone();
    bad.size.width = 123.0;
    cache.put(k, bad);
    let healed = layout_paragraph_cached(&para, &fonts, &cfg, 1.0, 400.0, sctx, &mut cache);
    assert_eq!(healed.size.width.to_bits(), good.size.width.to_bits());
}
