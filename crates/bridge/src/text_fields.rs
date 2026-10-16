//! Issue #485 - which `Command` fields carry text, and how the engine's
//! input boundary treats each one.
//!
//! #467 keeps characters XML cannot carry out of the model by cleaning every
//! text-bearing command argument in the dispatcher (`engine-wasm`
//! `text_input_guard`). That guard used to be a hand list of variants with
//! a wildcard `Ok(())` fallback, so a NEW text field escaped the boundary
//! until the writer's last line or the save gate caught it. The list is now
//! DERIVED from [`command_meta!`](crate::meta): every entry there ends in
//! `; (field: class, ...)` - `()` for a command with no text - and the one
//! macro generates
//!
//! - the per-variant declaration [`crate::CommandMeta::text`] (what the
//!   classification says), and
//! - [`crate::Command::visit_text_mut`] (an exhaustive `match`, no
//!   wildcard), which hands every declared field to a visitor through
//!   [`TextSlots`].
//!
//! So a new variant fails to compile until it is classified, a declared
//! field that does not exist fails to compile, and the guard has no list of
//! its own. A new `String` field on an EXISTING variant is caught by the
//! `every_command_string_field_is_classified` test, which reads the bridge
//! sources and compares them with the declarations.
//!
//! Nested payloads (`TextAttrsPatch`, `BridgeStyleProperties`,
//! `ImageBlob`) implement [`TextSlots`] below with an exhaustive
//! destructure - no `..` - so a new field on one of them fails to compile
//! until it is treated.

use std::ops::ControlFlow;

use crate::{BridgeSpanStylePatch, BridgeStyleProperties, Command, ImageBlob, TextAttrsPatch};

/// How a text-bearing field is treated at the input boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextClass {
    /// Document body text: C0 controls dropped, a vertical tab a line
    /// break, a form feed a page break, `U+FFFE` / `U+FFFF` refused
    /// (`engine::text_input::sanitize_text_in_place`).
    Body,
    /// A one-line value that ends up in the package (an author, a date, a
    /// style id, a font family): as [`TextClass::Body`] but a line break
    /// is refused too (`sanitize_line_in_place`).
    Line,
    /// A structured payload: its own [`TextSlots`] impl visits each leaf
    /// with the leaf's class.
    Nested,
    /// Never stored in the document or the package (a registry key, a
    /// password, a harness input) or cleaned elsewhere. The reason is the
    /// documentation; the guard does not touch it.
    Exempt(&'static str),
}

/// One declared text field of a command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextField {
    /// The field's name on the `Command` variant (`"text"`).
    pub name: &'static str,
    pub class: TextClass,
}

/// What a visitor is handed: the dotted path of the string
/// (`"attrs.font_family"`), its class, and the string itself.
pub type TextVisitor<'a> = dyn FnMut(&'static str, TextClass, &mut String) -> ControlFlow<()> + 'a;

/// A command field that can hand its strings to a [`TextVisitor`].
pub trait TextSlots {
    /// Visit every string in the field. `path` and `class` are the
    /// DECLARATION's (the field name and class in `command_meta!`); a
    /// nested payload ignores both and reports its own leaves.
    fn visit_text(
        &mut self,
        path: &'static str,
        class: TextClass,
        f: &mut TextVisitor<'_>,
    ) -> ControlFlow<()>;
}

impl TextSlots for String {
    fn visit_text(
        &mut self,
        path: &'static str,
        class: TextClass,
        f: &mut TextVisitor<'_>,
    ) -> ControlFlow<()> {
        f(path, class, self)
    }
}

impl TextSlots for Option<String> {
    fn visit_text(
        &mut self,
        path: &'static str,
        class: TextClass,
        f: &mut TextVisitor<'_>,
    ) -> ControlFlow<()> {
        match self {
            Some(s) => f(path, class, s),
            None => ControlFlow::Continue(()),
        }
    }
}

impl TextSlots for Vec<String> {
    fn visit_text(
        &mut self,
        path: &'static str,
        class: TextClass,
        f: &mut TextVisitor<'_>,
    ) -> ControlFlow<()> {
        for s in self {
            f(path, class, s)?;
        }
        ControlFlow::Continue(())
    }
}

/// `Recover.log_tail`: every replayed command meets the guard itself when
/// `apply` runs it, so the field is declared `Exempt` and never visited.
impl TextSlots for Vec<Command> {
    fn visit_text(
        &mut self,
        _: &'static str,
        _: TextClass,
        _: &mut TextVisitor<'_>,
    ) -> ControlFlow<()> {
        ControlFlow::Continue(())
    }
}

/// `Recover.renderer_downgrade`: backend names echoed on
/// `Event::Recovered`, never stored.
impl TextSlots for Option<crate::RendererDowngrade> {
    fn visit_text(
        &mut self,
        _: &'static str,
        _: TextClass,
        _: &mut TextVisitor<'_>,
    ) -> ControlFlow<()> {
        ControlFlow::Continue(())
    }
}

/// `SetFontPriority.priorities`: font registry ids, looked up and never
/// stored in the document.
impl TextSlots for Vec<crate::FontPriority> {
    fn visit_text(
        &mut self,
        _: &'static str,
        _: TextClass,
        _: &mut TextVisitor<'_>,
    ) -> ControlFlow<()> {
        ControlFlow::Continue(())
    }
}

impl TextSlots for TextAttrsPatch {
    fn visit_text(
        &mut self,
        _: &'static str,
        _: TextClass,
        f: &mut TextVisitor<'_>,
    ) -> ControlFlow<()> {
        let TextAttrsPatch {
            bold: _,
            italic: _,
            underline: _,
            strike: _,
            font_family,
            font_size: _,
            color: _,
            bg_color: _,
            script: _,
            language,
            caps: _,
            small_caps: _,
            font_slot: _,
        } = self;
        font_family.visit_text("attrs.font_family", TextClass::Line, f)?;
        language.visit_text("attrs.language", TextClass::Line, f)
    }
}

impl TextSlots for BridgeSpanStylePatch {
    fn visit_text(
        &mut self,
        _: &'static str,
        _: TextClass,
        f: &mut TextVisitor<'_>,
    ) -> ControlFlow<()> {
        let BridgeSpanStylePatch {
            bold: _,
            italic: _,
            underline: _,
            strike: _,
            font_size: _,
            color: _,
            bg_color: _,
            font_family,
            caps: _,
            small_caps: _,
        } = self;
        font_family.visit_text("properties.run_props.font_family", TextClass::Line, f)
    }
}

impl TextSlots for BridgeStyleProperties {
    fn visit_text(
        &mut self,
        _: &'static str,
        _: TextClass,
        f: &mut TextVisitor<'_>,
    ) -> ControlFlow<()> {
        let BridgeStyleProperties {
            // Paragraph properties carry no strings (the source-scan test
            // fails if that changes).
            para_props: _,
            run_props,
            based_on,
            clear_based_on: _,
            display_name,
        } = self;
        if let Some(run) = run_props {
            run.visit_text("properties.run_props", TextClass::Nested, f)?;
        }
        based_on.visit_text("properties.based_on", TextClass::Line, f)?;
        display_name.visit_text("properties.display_name", TextClass::Line, f)
    }
}

/// `InsertImage.image`: the MIME type is stored as the media's content
/// type and reaches `[Content_Types].xml`; the pixel bytes are binary.
impl TextSlots for ImageBlob {
    fn visit_text(
        &mut self,
        _: &'static str,
        _: TextClass,
        f: &mut TextVisitor<'_>,
    ) -> ControlFlow<()> {
        let ImageBlob {
            bytes: _,
            mime,
            width: _,
            height: _,
        } = self;
        mime.visit_text("image.mime", TextClass::Line, f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CommandKind, TextAttrsPatch};

    /// Every `(path, class)` the visitor sees for `cmd`, in order.
    fn visited(cmd: &mut Command) -> Vec<(&'static str, TextClass)> {
        let mut out = Vec::new();
        let _ = cmd.visit_text_mut(&mut |path, class, _| {
            out.push((path, class));
            ControlFlow::Continue(())
        });
        out
    }

    fn json(v: serde_json::Value) -> Command {
        serde_json::from_value(v).expect("decodes")
    }

    #[test]
    fn the_visitor_reports_each_declared_field_with_its_class() {
        use TextClass::{Body, Line};
        let mut c = json(serde_json::json!({ "type": "INSERT_TEXT", "text": "x" }));
        assert_eq!(visited(&mut c), vec![("text", Body)]);
        let mut c = json(serde_json::json!({
            "type": "REPLY_TO_COMMENT", "parent_id": 1, "text": "t", "author": "a"
        }));
        assert_eq!(visited(&mut c), vec![("text", Body), ("author", Line)]);
        let mut c = json(serde_json::json!({ "type": "UNDO" }));
        assert!(visited(&mut c).is_empty());
        let mut c = json(serde_json::json!({
            "type": "MODIFY_STYLE",
            "style_id": "S",
            "properties": {
                "run_props": { "font_family": "F" },
                "based_on": "B",
                "display_name": "D",
            },
        }));
        assert_eq!(
            visited(&mut c),
            vec![
                ("style_id", Line),
                ("properties.run_props.font_family", Line),
                ("properties.based_on", Line),
                ("properties.display_name", Line),
            ]
        );
        let mut c = json(serde_json::json!({
            "type": "APPLY_FORMATTING",
            "range": {
                "start": { "path": { "steps": [{ "kind": "BLOCK", "idx": 0 }] }, "offset": 0 },
                "end": { "path": { "steps": [{ "kind": "BLOCK", "idx": 0 }] }, "offset": 1 },
            },
            "attrs": { "font_family": "Arial" },
        }));
        // An absent `Option<String>` is not visited.
        assert_eq!(visited(&mut c), vec![("attrs.font_family", Line)]);
    }

    #[test]
    fn a_visitor_can_edit_in_place_and_stop_the_walk() {
        let mut c = json(serde_json::json!({
            "type": "SET_REVIEW_IDENTITY", "author": "a\u{1}", "date": "d"
        }));
        let mut seen = 0;
        let flow = c.visit_text_mut(&mut |path, _, s| {
            seen += 1;
            if path == "author" {
                s.retain(|ch| ch != '\u{1}');
                return ControlFlow::Break(());
            }
            ControlFlow::Continue(())
        });
        assert!(flow.is_break());
        assert_eq!(seen, 1, "the walk stopped at the first Break");
        let Command::SetReviewIdentity { author, .. } = c else {
            panic!("variant");
        };
        assert_eq!(author, "a");
        // An absent field visits nothing; nested payloads report leaves.
        let mut patch = TextAttrsPatch {
            bold: None,
            italic: None,
            underline: None,
            strike: None,
            font_family: Some("F".into()),
            font_size: None,
            color: None,
            bg_color: None,
            script: None,
            language: None,
            caps: None,
            small_caps: None,
            font_slot: None,
        };
        let mut paths = Vec::new();
        let _ = patch.visit_text("attrs", TextClass::Nested, &mut |p, _, _| {
            paths.push(p);
            ControlFlow::Continue(())
        });
        assert_eq!(paths, vec!["attrs.font_family"]);
    }

    /// Every classified command is a real variant with matching names - the
    /// macro would not have compiled otherwise - and each declared field
    /// has a distinct name.
    #[test]
    fn declared_fields_are_distinct_per_command() {
        for kind in CommandKind::ALL {
            let fields = kind.meta().text;
            let mut names: Vec<&str> = fields.iter().map(|f| f.name).collect();
            names.sort_unstable();
            names.dedup();
            assert_eq!(names.len(), fields.len(), "{kind:?} declares a field twice");
        }
    }

    // ---- source scan: every String-bearing Command field is classified ----

    /// Drop `//` and `/* */` comments and `#[...]` attributes (balanced).
    fn strip(src: &str) -> String {
        let b: Vec<char> = src.chars().collect();
        let mut out = String::new();
        let mut i = 0;
        while i < b.len() {
            if b[i] == '/' && b.get(i + 1) == Some(&'/') {
                while i < b.len() && b[i] != '\n' {
                    i += 1;
                }
            } else if b[i] == '/' && b.get(i + 1) == Some(&'*') {
                i += 2;
                while i + 1 < b.len() && !(b[i] == '*' && b[i + 1] == '/') {
                    i += 1;
                }
                i += 2;
            } else if b[i] == '#' && b.get(i + 1) == Some(&'[') {
                let mut depth = 0;
                while i < b.len() {
                    if b[i] == '[' {
                        depth += 1;
                    } else if b[i] == ']' {
                        depth -= 1;
                        if depth == 0 {
                            i += 1;
                            break;
                        }
                    }
                    i += 1;
                }
            } else {
                out.push(b[i]);
                i += 1;
            }
        }
        out
    }

    /// The text between the `{` that follows `header` and its matching `}`.
    fn body_after<'a>(src: &'a str, header: &str) -> Option<&'a str> {
        let at = src.find(header)? + header.len();
        let open = at + src[at..].find('{')?;
        let mut depth = 0;
        for (i, ch) in src[open..].char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(&src[open + 1..open + i]);
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// Split `s` at top-level commas (outside `<>`, `()`, `[]`, `{}`).
    fn split_top(s: &str) -> Vec<&str> {
        let mut out = Vec::new();
        let mut depth = 0i32;
        let mut start = 0;
        for (i, ch) in s.char_indices() {
            match ch {
                '<' | '(' | '[' | '{' => depth += 1,
                '>' if !s[..i].ends_with('-') => depth -= 1,
                ')' | ']' | '}' => depth -= 1,
                ',' if depth == 0 => {
                    out.push(&s[start..i]);
                    start = i + 1;
                }
                _ => {}
            }
        }
        out.push(&s[start..]);
        out.into_iter()
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .collect()
    }

    /// `name: Type` pairs of a struct body or a struct-variant body.
    fn fields_of(body: &str) -> Vec<(String, String)> {
        split_top(body)
            .into_iter()
            .filter_map(|p| {
                let (n, t) = p.split_once(':')?;
                let n = n.trim().trim_start_matches("pub ").trim();
                n.chars()
                    .all(|c| c.is_alphanumeric() || c == '_')
                    .then(|| (n.to_string(), t.trim().to_string()))
            })
            .collect()
    }

    /// The variants of `enum Command` with their `(field, type)` pairs.
    fn command_variants(src: &str) -> Vec<(String, Vec<(String, String)>)> {
        let body = body_after(src, "pub enum Command").expect("enum Command");
        split_top(body)
            .into_iter()
            .map(|v| match v.find('{') {
                Some(open) => {
                    let name = v[..open].trim().to_string();
                    let inner = &v[open + 1..v.rfind('}').expect("closing brace")];
                    (name, fields_of(inner))
                }
                None => (v.trim().to_string(), Vec::new()),
            })
            .collect()
    }

    fn type_names(ty: &str) -> Vec<&str> {
        ty.split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .filter(|w| w.chars().next().is_some_and(char::is_uppercase))
            .collect()
    }

    /// Whether `ty` is, or (transitively) holds, a `String`. Types are
    /// looked up in `defs` (struct name -> field types).
    fn holds_string(
        ty: &str,
        defs: &std::collections::BTreeMap<String, Vec<(String, String)>>,
        seen: &mut Vec<String>,
    ) -> bool {
        for w in type_names(ty) {
            if w == "String" {
                return true;
            }
            if seen.iter().any(|s| s == w) {
                continue;
            }
            seen.push(w.to_string());
            if let Some(fields) = defs.get(w)
                && fields.iter().any(|(_, t)| holds_string(t, defs, seen))
            {
                return true;
            }
        }
        false
    }

    /// Struct names (and their fields) defined in the bridge sources.
    fn struct_defs(srcs: &[String]) -> std::collections::BTreeMap<String, Vec<(String, String)>> {
        let mut defs = std::collections::BTreeMap::new();
        for src in srcs {
            let mut rest = src.as_str();
            while let Some(p) = rest.find("pub struct ") {
                rest = &rest[p + "pub struct ".len()..];
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                let after = &rest[name.len()..];
                // Only braced structs (a tuple / unit struct has no named fields).
                let head = after.trim_start();
                if head.starts_with('{')
                    && let Some(body) = body_after(
                        &format!("pub struct {name}{after}"),
                        &format!("pub struct {name}"),
                    )
                {
                    defs.insert(name, fields_of(body));
                }
            }
        }
        defs
    }

    /// Issue #485 - the guard's field set is the DECLARED one, so the
    /// declarations must cover every command field that is (or holds) a
    /// `String`, and declare nothing else. A new `String` field on an
    /// existing variant fails here until it is classified in
    /// `command_meta!`; a field renamed or removed fails until the stale
    /// declaration is fixed.
    #[test]
    fn every_command_string_field_is_classified() {
        let srcs: Vec<String> = [include_str!("command.rs"), include_str!("common.rs")]
            .iter()
            .map(|s| strip(s))
            .collect();
        let defs = struct_defs(&srcs);
        let variants = command_variants(&srcs[0]);
        assert_eq!(
            variants.len(),
            CommandKind::ALL.len(),
            "the scan found every Command variant"
        );
        let mut problems = Vec::new();
        for (name, fields) in &variants {
            let kind = CommandKind::ALL
                .iter()
                .find(|k| k.name() == name)
                .unwrap_or_else(|| panic!("{name} is not in command_meta!"));
            let declared: Vec<&str> = kind.meta().text.iter().map(|f| f.name).collect();
            // `Vec<Command>` holds strings only through the commands it
            // replays; it is declared (Exempt) rather than recursed into.
            let mut holders: Vec<&str> = fields
                .iter()
                .filter(|(_, t)| {
                    t.contains("Vec<Command>") || holds_string(t, &defs, &mut Vec::new())
                })
                .map(|(f, _)| f.as_str())
                .collect();
            holders.sort_unstable();
            let mut declared_sorted = declared.clone();
            declared_sorted.sort_unstable();
            if holders != declared_sorted {
                problems.push(format!(
                    "{name}: string-bearing fields {holders:?} vs declared {declared_sorted:?}"
                ));
            }
        }
        assert!(
            problems.is_empty(),
            "classify in command_meta!:\n{}",
            problems.join("\n")
        );
    }

    /// Every struct a `Nested` field points at (and every struct below it
    /// that holds a string) has a `TextSlots` impl in this file, so its
    /// leaves are reached.
    #[test]
    fn every_nested_string_struct_has_a_text_slots_impl() {
        let srcs: Vec<String> = [include_str!("command.rs"), include_str!("common.rs")]
            .iter()
            .map(|s| strip(s))
            .collect();
        let defs = struct_defs(&srcs);
        let this = include_str!("text_fields.rs");
        let variants = command_variants(&srcs[0]);
        for (name, fields) in &variants {
            let kind = CommandKind::ALL.iter().find(|k| k.name() == name).unwrap();
            for tf in kind.meta().text {
                if tf.class != TextClass::Nested {
                    continue;
                }
                let ty = &fields
                    .iter()
                    .find(|(f, _)| f == tf.name)
                    .expect("declared field exists")
                    .1;
                for w in type_names(ty) {
                    if defs.contains_key(w) && holds_string(w, &defs, &mut Vec::new()) {
                        assert!(
                            this.contains(&format!("impl TextSlots for {w}")),
                            "{name}.{} holds strings through {w}: add `impl TextSlots for {w}`",
                            tf.name
                        );
                    }
                }
            }
        }
        // And the leaf structs those impls reach into: any struct below a
        // nested one that directly holds a String needs its own impl too.
        for (sname, sfields) in &defs {
            let direct = sfields
                .iter()
                .any(|(_, t)| type_names(t).contains(&"String"));
            let reached = [
                "TextAttrsPatch",
                "BridgeStyleProperties",
                "BridgeSpanStylePatch",
                "ImageBlob",
            ]
            .contains(&sname.as_str());
            let reachable_from_command =
                variants.iter().any(|(_, fs)| {
                    fs.iter()
                        .any(|(_, t)| type_names(t).contains(&sname.as_str()))
                }) || ["BridgeSpanStylePatch", "BridgeParaPropertiesPatch"]
                    .contains(&sname.as_str());
            if direct && reachable_from_command && !reached {
                // A struct reachable from a command that holds a String
                // directly but is not one of the four with an impl: it must
                // be Exempt-declared where it is used, or gain an impl.
                assert!(
                    this.contains(&format!("impl TextSlots for {sname}"))
                        || this.contains(&format!("impl TextSlots for Option<crate::{sname}>"))
                        || this.contains(&format!("impl TextSlots for Vec<crate::{sname}>")),
                    "{sname} holds a String and is reachable from a Command: classify it"
                );
            }
        }
    }
}
