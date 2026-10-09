//! Issue #407 — the command-boundary finiteness guard.
//!
//! A `Command` crosses the worker boundary from TypeScript through
//! `serde-wasm-bindgen`, which hands a JS `NaN` / `Infinity` to an `f32` /
//! `f64` field unchanged (an integer field rejects them at decode time). A
//! non-finite number then reaches a handler that clamps it — `f32::clamp`
//! passes NaN straight through — and on into layout, where it poisons every
//! geometry it touches. Issue #186 guarded the zoom pair by hand; this
//! module guards **every** numeric field of every command, present and
//! future, without a hand-kept list: [`first_non_finite`] walks a value
//! through its own `Serialize` impl (a no-output serializer that looks only
//! at numbers), so a new `f32` field anywhere in the bridge schema is
//! covered the day it is added.
//!
//! The engine runs [`Command::first_non_finite`] first thing in its
//! dispatcher and answers a typed `Event::Error { kind: InvalidArgument }`
//! naming the field (`ApplyFormatting: attrs.font_size is NaN …`) instead
//! of applying anything. The walk is allocation-free until it finds
//! something; strings and byte buffers are skipped in O(1).

use std::fmt::{self, Display, Write as _};

use serde::Serialize;
use serde::ser;

use crate::command::Command;

/// A non-finite number found in a command (or any serializable value).
#[derive(Clone, Debug, PartialEq)]
pub struct NonFiniteField {
    /// Where the number sits, from the value's root: struct fields joined
    /// with `.`, sequence elements as `[i]` — e.g. `attrs.font_size`,
    /// `viewport.w`, `stops[2].position_pt`. `<value>` for a bare number.
    pub field: String,
    /// The offending number (`NaN`, `inf` or `-inf`).
    pub value: f64,
}

impl Display for NonFiniteField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} is {} (every numeric argument must be a finite number)",
            self.field, self.value
        )
    }
}

/// The first non-finite `f32` / `f64` inside `value`, in serialization
/// order, or `None` when every number is finite.
pub fn first_non_finite<T: Serialize + ?Sized>(value: &T) -> Option<NonFiniteField> {
    probe(value, &[])
}

impl Command {
    /// Issue #407 — this command's first non-finite numeric field, if any.
    ///
    /// `Recover.log_tail` is not descended into: every replayed command
    /// goes back through the engine's dispatcher, where it meets this guard
    /// on its own — one bad logged command must not refuse the whole
    /// recovery.
    pub fn first_non_finite(&self) -> Option<NonFiniteField> {
        probe(self, &["log_tail"])
    }
}

fn probe<T: Serialize + ?Sized>(
    value: &T,
    skip_top_level: &'static [&'static str],
) -> Option<NonFiniteField> {
    let mut p = Probe {
        path: Vec::new(),
        skip_top_level,
    };
    match value.serialize(&mut p) {
        Err(Stop::Found(found)) => Some(found),
        /* A `Serialize` impl that refuses to serialize (none in the
        bridge today) holds no number this walk could judge. */
        Ok(()) | Err(Stop::Custom) => None,
    }
}

/// One step of the path to the number being looked at.
enum Seg {
    Field(&'static str),
    Index(usize),
}

struct Probe {
    path: Vec<Seg>,
    /// Top-level struct fields the walk skips (see
    /// [`Command::first_non_finite`]).
    skip_top_level: &'static [&'static str],
}

impl Probe {
    fn check(&self, v: f64) -> Result<(), Stop> {
        if v.is_finite() {
            return Ok(());
        }
        let mut field = String::new();
        for seg in &self.path {
            match seg {
                Seg::Field(name) => {
                    if !field.is_empty() {
                        field.push('.');
                    }
                    field.push_str(name);
                }
                Seg::Index(i) => {
                    let _ = write!(field, "[{i}]");
                }
            }
        }
        if field.is_empty() {
            field.push_str("<value>");
        }
        Err(Stop::Found(NonFiniteField { field, value: v }))
    }
}

/// The walk's short-circuit: `Found` stops at the first offender.
#[derive(Debug)]
enum Stop {
    Found(NonFiniteField),
    Custom,
}

impl Display for Stop {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Stop::Found(found) => found.fmt(f),
            Stop::Custom => f.write_str("value refused to serialize"),
        }
    }
}

impl std::error::Error for Stop {}

impl ser::Error for Stop {
    fn custom<T: Display>(_msg: T) -> Self {
        Stop::Custom
    }
}

/// Every compound shape (seq / tuple / map / struct / variant) walks its
/// children through this; `pop` is set when the compound pushed a variant
/// name segment that its `end` must remove.
struct Compound<'a> {
    probe: &'a mut Probe,
    index: usize,
    pop: bool,
}

impl Compound<'_> {
    fn element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Stop> {
        self.probe.path.push(Seg::Index(self.index));
        self.index += 1;
        let r = value.serialize(&mut *self.probe);
        self.probe.path.pop();
        r
    }

    fn field<T: Serialize + ?Sized>(&mut self, key: &'static str, value: &T) -> Result<(), Stop> {
        let top_level = self.probe.path.len() == usize::from(self.pop);
        if top_level && self.probe.skip_top_level.contains(&key) {
            return Ok(());
        }
        self.probe.path.push(Seg::Field(key));
        let r = value.serialize(&mut *self.probe);
        self.probe.path.pop();
        r
    }

    fn finish(self) -> Result<(), Stop> {
        if self.pop {
            self.probe.path.pop();
        }
        Ok(())
    }
}

impl<'a> ser::Serializer for &'a mut Probe {
    type Ok = ();
    type Error = Stop;
    type SerializeSeq = Compound<'a>;
    type SerializeTuple = Compound<'a>;
    type SerializeTupleStruct = Compound<'a>;
    type SerializeTupleVariant = Compound<'a>;
    type SerializeMap = Compound<'a>;
    type SerializeStruct = Compound<'a>;
    type SerializeStructVariant = Compound<'a>;

    fn serialize_bool(self, _: bool) -> Result<(), Stop> {
        Ok(())
    }
    fn serialize_i8(self, _: i8) -> Result<(), Stop> {
        Ok(())
    }
    fn serialize_i16(self, _: i16) -> Result<(), Stop> {
        Ok(())
    }
    fn serialize_i32(self, _: i32) -> Result<(), Stop> {
        Ok(())
    }
    fn serialize_i64(self, _: i64) -> Result<(), Stop> {
        Ok(())
    }
    fn serialize_i128(self, _: i128) -> Result<(), Stop> {
        Ok(())
    }
    fn serialize_u8(self, _: u8) -> Result<(), Stop> {
        Ok(())
    }
    fn serialize_u16(self, _: u16) -> Result<(), Stop> {
        Ok(())
    }
    fn serialize_u32(self, _: u32) -> Result<(), Stop> {
        Ok(())
    }
    fn serialize_u64(self, _: u64) -> Result<(), Stop> {
        Ok(())
    }
    fn serialize_u128(self, _: u128) -> Result<(), Stop> {
        Ok(())
    }
    fn serialize_f32(self, v: f32) -> Result<(), Stop> {
        self.check(f64::from(v))
    }
    fn serialize_f64(self, v: f64) -> Result<(), Stop> {
        self.check(v)
    }
    fn serialize_char(self, _: char) -> Result<(), Stop> {
        Ok(())
    }
    fn serialize_str(self, _: &str) -> Result<(), Stop> {
        Ok(())
    }
    fn serialize_bytes(self, _: &[u8]) -> Result<(), Stop> {
        Ok(())
    }
    fn collect_str<T: Display + ?Sized>(self, _: &T) -> Result<(), Stop> {
        Ok(())
    }
    fn serialize_none(self) -> Result<(), Stop> {
        Ok(())
    }
    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<(), Stop> {
        value.serialize(self)
    }
    fn serialize_unit(self) -> Result<(), Stop> {
        Ok(())
    }
    fn serialize_unit_struct(self, _: &'static str) -> Result<(), Stop> {
        Ok(())
    }
    fn serialize_unit_variant(self, _: &'static str, _: u32, _: &'static str) -> Result<(), Stop> {
        Ok(())
    }
    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        value: &T,
    ) -> Result<(), Stop> {
        value.serialize(self)
    }
    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<(), Stop> {
        self.path.push(Seg::Field(variant));
        let r = value.serialize(&mut *self);
        self.path.pop();
        r
    }
    fn serialize_seq(self, _: Option<usize>) -> Result<Compound<'a>, Stop> {
        Ok(Compound {
            probe: self,
            index: 0,
            pop: false,
        })
    }
    fn serialize_tuple(self, _: usize) -> Result<Compound<'a>, Stop> {
        self.serialize_seq(None)
    }
    fn serialize_tuple_struct(self, _: &'static str, _: usize) -> Result<Compound<'a>, Stop> {
        self.serialize_seq(None)
    }
    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        _: usize,
    ) -> Result<Compound<'a>, Stop> {
        self.path.push(Seg::Field(variant));
        Ok(Compound {
            probe: self,
            index: 0,
            pop: true,
        })
    }
    fn serialize_map(self, _: Option<usize>) -> Result<Compound<'a>, Stop> {
        self.serialize_seq(None)
    }
    fn serialize_struct(self, _: &'static str, _: usize) -> Result<Compound<'a>, Stop> {
        self.serialize_seq(None)
    }
    fn serialize_struct_variant(
        self,
        name: &'static str,
        index: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<Compound<'a>, Stop> {
        self.serialize_tuple_variant(name, index, variant, len)
    }
}

impl ser::SerializeSeq for Compound<'_> {
    type Ok = ();
    type Error = Stop;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Stop> {
        self.element(value)
    }
    fn end(self) -> Result<(), Stop> {
        self.finish()
    }
}

impl ser::SerializeTuple for Compound<'_> {
    type Ok = ();
    type Error = Stop;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Stop> {
        self.element(value)
    }
    fn end(self) -> Result<(), Stop> {
        self.finish()
    }
}

impl ser::SerializeTupleStruct for Compound<'_> {
    type Ok = ();
    type Error = Stop;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Stop> {
        self.element(value)
    }
    fn end(self) -> Result<(), Stop> {
        self.finish()
    }
}

impl ser::SerializeTupleVariant for Compound<'_> {
    type Ok = ();
    type Error = Stop;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Stop> {
        self.element(value)
    }
    fn end(self) -> Result<(), Stop> {
        self.finish()
    }
}

/// A map's keys and values are both walked, as the entry's `[i]`: a
/// `NaN` map key is as unusable as a `NaN` value.
impl ser::SerializeMap for Compound<'_> {
    type Ok = ();
    type Error = Stop;
    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result<(), Stop> {
        self.probe.path.push(Seg::Index(self.index));
        let r = key.serialize(&mut *self.probe);
        self.probe.path.pop();
        r
    }
    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Stop> {
        self.element(value)
    }
    fn end(self) -> Result<(), Stop> {
        self.finish()
    }
}

impl ser::SerializeStruct for Compound<'_> {
    type Ok = ();
    type Error = Stop;
    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), Stop> {
        self.field(key, value)
    }
    fn end(self) -> Result<(), Stop> {
        self.finish()
    }
}

impl ser::SerializeStructVariant for Compound<'_> {
    type Ok = ();
    type Error = Stop;
    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), Stop> {
        self.field(key, value)
    }
    fn end(self) -> Result<(), Stop> {
        self.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BlockPath, Command, LogicalPos, LogicalRange, Point, Rect, TextAttrsPatch};

    fn pos() -> LogicalPos {
        LogicalPos {
            path: BlockPath::top(0),
            offset: 0,
        }
    }

    #[test]
    fn finite_commands_pass() {
        for cmd in [
            Command::Ping,
            Command::SetZoom { scale: 1.5 },
            Command::ExpandLayout { target_y: 0.0 },
            Command::HitTest {
                at: Point { x: -3.0, y: 1e30 },
            },
            Command::InsertText {
                at: Some(pos()),
                text: "NaN inf".into(),
            },
        ] {
            assert_eq!(cmd.first_non_finite(), None, "{cmd:?}");
        }
    }

    #[test]
    fn a_top_level_float_is_named() {
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let found = Command::SetZoom { scale: bad }.first_non_finite().unwrap();
            assert_eq!(found.field, "scale");
            assert_eq!(found.value.to_string(), f64::from(bad).to_string());
        }
        let found = Command::Tick { now_ms: f64::NAN }
            .first_non_finite()
            .unwrap();
        assert_eq!(found.field, "now_ms");
        assert!(found.value.is_nan());
        assert_eq!(
            found.to_string(),
            "now_ms is NaN (every numeric argument must be a finite number)"
        );
    }

    #[test]
    fn nested_and_optional_floats_are_found_with_their_path() {
        let found = Command::ApplyFormatting {
            range: Some(LogicalRange {
                start: pos(),
                end: pos(),
            }),
            attrs: TextAttrsPatch {
                bold: Some(true),
                italic: None,
                underline: None,
                strike: None,
                font_family: Some("Amiri".into()),
                font_size: Some(f32::INFINITY),
                color: None,
                bg_color: None,
                script: None,
                language: None,
                caps: None,
                small_caps: None,
                font_slot: None,
            },
        }
        .first_non_finite()
        .unwrap();
        assert_eq!(found.field, "attrs.font_size");
        assert_eq!(found.value, f64::INFINITY);

        let found = Command::RequestPaint {
            viewport: Rect {
                x: 0.0,
                y: 0.0,
                w: 10.0,
                h: 10.0,
            },
            dirty: Some(Rect {
                x: 0.0,
                y: f32::NAN,
                w: 1.0,
                h: 1.0,
            }),
        }
        .first_non_finite()
        .unwrap();
        assert_eq!(found.field, "dirty.y");
    }

    /// `Recover.log_tail` is replayed command by command through the
    /// guarded dispatcher, so the guard does not refuse the recovery.
    #[test]
    fn recover_does_not_judge_its_log_tail() {
        let cmd = Command::Recover {
            snapshot: Vec::new(),
            log_tail: vec![Command::SetZoom { scale: f32::NAN }],
            renderer_downgrade: None,
            package: None,
        };
        assert_eq!(cmd.first_non_finite(), None);
        /* … while the generic walk does see it. */
        let found = first_non_finite(&cmd).unwrap();
        assert_eq!(found.field, "log_tail[0].scale");
    }

    #[test]
    fn bare_values_and_sequences() {
        assert_eq!(first_non_finite(&1.0f32), None);
        assert_eq!(first_non_finite(&f64::NAN).unwrap().field, "<value>");
        assert_eq!(
            first_non_finite(&vec![1.0f32, 2.0, f32::NAN])
                .unwrap()
                .field,
            "[2]"
        );
        assert_eq!(first_non_finite(&("x", 3u8, [0.5f64])), None);
    }
}
