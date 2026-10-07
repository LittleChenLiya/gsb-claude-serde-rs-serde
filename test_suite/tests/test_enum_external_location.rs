//! Plumbing of externally tagged enum error locations through the existing
//! `EnumAccess`/`VariantAccess` data flow.
//!
//! This file ships a tiny dependency-free token data format (the "test
//! format") whose enum adapter helper extends the ordinary Serde data flow:
//!
//! * the container path accumulated so far (`outer[0].kind`) is visible to
//!   every error raised while an enum is being deserialized;
//! * the actual tag of an unrecognized variant is captured by the adapter
//!   instead of being lost;
//! * the original Serde error category is preserved by overriding
//!   [`serde::de::Error::unknown_variant`] (rather than funneling everything
//!   through `Error::custom`), so the expected candidate list survives;
//! * successful tag matching keeps using a borrowed tag, so the borrow
//!   lifetime of the success path is unchanged (see
//!   [`test_nested_ok_borrowed_payload`]).
//!
//! No external data format crate is involved: the format is built directly
//! on top of the public `serde::de` interface.

#![allow(clippy::needless_lifetimes)]

use std::cell::RefCell;
use std::error::Error as StdError;
use std::fmt::{self, Display};

use serde::de::{
    self, Deserialize, DeserializeSeed, EnumAccess, MapAccess, SeqAccess, VariantAccess, Visitor,
};
use serde_derive::Deserialize;

// ===========================================================================
// Container path tracker
// ===========================================================================

thread_local! {
    static PATH: RefCell<Vec<Segment>> = const { RefCell::new(Vec::new()) };
    // The most recently deserialized string map key, used to extend the
    // container path when the following value is deserialized.
    static LAST_MAP_KEY: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// One step of a container path.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Segment {
    /// Element of a sequence at the given zero-based index.
    Index(usize),
    /// Value of a map with a string key.
    Field(String),
    /// Payload of an externally tagged variant with the given tag.
    Variant(String),
}

impl Segment {
    fn render(&self, first: bool, out: &mut String) {
        match self {
            Segment::Index(i) => {
                out.push('[');
                out.push_str(&i.to_string());
                out.push(']');
            }
            Segment::Field(key) | Segment::Variant(key) => {
                if !first {
                    out.push('.');
                }
                out.push_str(key);
            }
        }
    }
}

/// RAII guard that keeps one path segment pushed for its lifetime.
struct SegmentGuard {
    segment: Segment,
}

impl SegmentGuard {
    fn new(segment: Segment) -> Self {
        PATH.with(|path| path.borrow_mut().push(segment.clone()));
        SegmentGuard { segment }
    }
}

impl Drop for SegmentGuard {
    fn drop(&mut self) {
        PATH.with(|path| {
            let popped = path.borrow_mut().pop();
            debug_assert_eq!(popped.as_ref(), Some(&self.segment));
        });
    }
}

/// Renders the current container path, e.g. `outer[0].kind.Nope`.
fn current_path() -> String {
    PATH.with(|path| {
        let segments = path.borrow();
        let mut out = String::new();
        for (i, segment) in segments.iter().enumerate() {
            segment.render(i == 0, &mut out);
        }
        out
    })
}

// ===========================================================================
// Error type of the test format
// ===========================================================================

/// Structured error category of the test format. Mirrors the category that
/// Serde distinguishes on `serde::de::Error`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ErrorKind {
    /// `Error::custom`: generic/format errors, including invalid type and
    /// invalid value errors raised while decoding a variant payload.
    Custom,
    /// `Error::unknown_variant`: an unrecognized enum tag.
    UnknownVariant,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LocatedError {
    kind: ErrorKind,
    /// Error message exactly as produced by Serde's default trait methods.
    message: String,
    /// Container path at the failure point.
    path: String,
    /// The actual tag seen, for `unknown_variant` errors.
    tag: Option<String>,
    /// The expected candidate list, for `unknown_variant` errors.
    expected: Option<Vec<&'static str>>,
}

impl LocatedError {
    fn custom<T>(msg: T) -> Self
    where
        T: Display,
    {
        LocatedError {
            kind: ErrorKind::Custom,
            message: msg.to_string(),
            path: current_path(),
            tag: None,
            expected: None,
        }
    }
}

impl Display for LocatedError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if self.path.is_empty() {
            f.write_str(&self.message)
        } else {
            write!(f, "{}: {}", self.path, self.message)
        }
    }
}

impl StdError for LocatedError {}

/// Formats the candidate list exactly like Serde's internal `OneOf`.
fn format_expected(expected: &'static [&'static str]) -> String {
    match expected.len() {
        0 => String::new(),
        1 => format!("`{}`", expected[0]),
        2 => format!("`{}` or `{}`", expected[0], expected[1]),
        _ => {
            let mut out = String::from("one of ");
            for (i, name) in expected.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push('`');
                out.push_str(name);
                out.push('`');
            }
            out
        }
    }
}

impl de::Error for LocatedError {
    fn custom<T>(msg: T) -> Self
    where
        T: Display,
    {
        LocatedError::custom(msg)
    }

    // The enum adapter keeps this category intact instead of downgrading it to
    // a generic `custom` error: the actual tag and the expected candidate
    // slice are preserved, and the container path is attached.
    fn unknown_variant(tag: &str, expected: &'static [&'static str]) -> Self {
        let message = if expected.is_empty() {
            format!("unknown variant `{}`, there are no variants", tag)
        } else {
            format!(
                "unknown variant `{}`, expected {}",
                tag,
                format_expected(expected)
            )
        };
        LocatedError {
            kind: ErrorKind::UnknownVariant,
            message,
            path: current_path(),
            tag: Some(tag.to_owned()),
            expected: Some(expected.to_vec()),
        }
    }
}

// ===========================================================================
// Tokens of the test format
// ===========================================================================

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Bool(bool),
    I32(i32),
    Str(&'static str),
    BorrowedStr(&'static str),
    Seq { len: Option<usize> },
    SeqEnd,
    Map { len: Option<usize> },
    MapEnd,
    UnitVariant { variant: &'static str },
    NewtypeVariant { variant: &'static str },
    TupleVariant { variant: &'static str, len: usize },
    TupleVariantEnd,
    StructVariant { variant: &'static str, len: usize },
    StructVariantEnd,
}

// ===========================================================================
// Deserializer of the test format
// ===========================================================================

struct Deserializer<'de> {
    tokens: &'de [Token],
}

impl<'de> Deserializer<'de> {
    fn new(tokens: &'de [Token]) -> Self {
        Deserializer { tokens }
    }

    fn next(&mut self) -> Result<Token, LocatedError> {
        if let Some((first, rest)) = self.tokens.split_first() {
            self.tokens = rest;
            Ok(first.clone())
        } else {
            Err(LocatedError::custom("ran out of tokens to deserialize"))
        }
    }

    fn peek(&self) -> Result<Token, LocatedError> {
        self.tokens
            .first()
            .cloned()
            .ok_or_else(|| LocatedError::custom("ran out of tokens to deserialize"))
    }

    fn unexpected(token: Token) -> LocatedError {
        LocatedError::custom(format_args!(
            "deserialization did not expect this token: {:?}",
            token
        ))
    }

    /// Builds the enum adapter for the upcoming externally tagged variant
    /// token. The tag token itself is consumed later, inside `variant_seed`.
    fn enum_reader(&mut self) -> Result<EnumReader<'_, 'de>, LocatedError> {
        match self.peek()? {
            Token::UnitVariant { .. }
            | Token::NewtypeVariant { .. }
            | Token::TupleVariant { .. }
            | Token::StructVariant { .. } => Ok(EnumReader {
                de: self,
                tag: None,
            }),
            other => Err(Deserializer::unexpected(other)),
        }
    }

    fn visit_seq_content<V>(
        &mut self,
        end: Token,
        visitor: V,
    ) -> Result<V::Value, LocatedError>
    where
        V: Visitor<'de>,
    {
        let value = visitor.visit_seq(SeqReader {
            de: &mut *self,
            end: end.clone(),
            index: 0,
        })?;
        let closing = self.next()?;
        if closing != end {
            return Err(LocatedError::custom(format_args!(
                "expected {:?} token, found {:?}",
                end, closing
            )));
        }
        Ok(value)
    }

    fn visit_map_content<V>(
        &mut self,
        end: Token,
        visitor: V,
    ) -> Result<V::Value, LocatedError>
    where
        V: Visitor<'de>,
    {
        let value = visitor.visit_map(MapReader {
            de: &mut *self,
            end: end.clone(),
        })?;
        let closing = self.next()?;
        if closing != end {
            return Err(LocatedError::custom(format_args!(
                "expected {:?} token, found {:?}",
                end, closing
            )));
        }
        Ok(value)
    }
}

impl<'de> de::Deserializer<'de> for &mut Deserializer<'de> {
    type Error = LocatedError;

    fn deserialize_any<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self.next()? {
            Token::Bool(v) => visitor.visit_bool(v),
            Token::I32(v) => visitor.visit_i32(v),
            Token::Str(v) => {
                LAST_MAP_KEY.with(|key| *key.borrow_mut() = Some(v.to_owned()));
                visitor.visit_str(v)
            }
            Token::BorrowedStr(v) => {
                LAST_MAP_KEY.with(|key| *key.borrow_mut() = Some(v.to_owned()));
                visitor.visit_borrowed_str(v)
            }
            Token::Seq { .. } => self.visit_seq_content(Token::SeqEnd, visitor),
            Token::Map { .. } => self.visit_map_content(Token::MapEnd, visitor),
            Token::UnitVariant { .. }
            | Token::NewtypeVariant { .. }
            | Token::TupleVariant { .. }
            | Token::StructVariant { .. } => {
                let reader = self.enum_reader()?;
                visitor.visit_enum(reader)
            }
            other => Err(Deserializer::unexpected(other)),
        }
    }

    fn deserialize_enum<V>(
        self,
        _name: &'static str,
        _variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        visitor.visit_enum(self.enum_reader()?)
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
        bytes byte_buf option unit unit_struct newtype_struct seq tuple
        tuple_struct map struct identifier ignored_any
    }
}

// ===========================================================================
// Sequence / map readers: they extend the container path
// ===========================================================================

struct SeqReader<'a, 'de: 'a> {
    de: &'a mut Deserializer<'de>,
    end: Token,
    index: usize,
}

impl<'de> SeqAccess<'de> for SeqReader<'_, 'de> {
    type Error = LocatedError;

    fn next_element_seed<T>(&mut self, seed: T) -> Result<Option<T::Value>, Self::Error>
    where
        T: DeserializeSeed<'de>,
    {
        if self.de.peek()? == self.end {
            return Ok(None);
        }
        let index = self.index;
        self.index += 1;
        // The `[i]` segment is live for the whole lifetime of the element.
        let _guard = SegmentGuard::new(Segment::Index(index));
        seed.deserialize(&mut *self.de).map(Some)
    }
}

struct MapReader<'a, 'de: 'a> {
    de: &'a mut Deserializer<'de>,
    end: Token,
}

impl<'de> MapAccess<'de> for MapReader<'_, 'de> {
    type Error = LocatedError;

    fn next_key_seed<K>(&mut self, seed: K) -> Result<Option<K::Value>, Self::Error>
    where
        K: DeserializeSeed<'de>,
    {
        if self.de.peek()? == self.end {
            return Ok(None);
        }
        // Deserializing the key records it in `LAST_MAP_KEY` (keys in the
        // fixtures are strings), so `next_value_seed` can extend the path.
        seed.deserialize(&mut *self.de).map(Some)
    }

    fn next_value_seed<V>(&mut self, seed: V) -> Result<V::Value, Self::Error>
    where
        V: DeserializeSeed<'de>,
    {
        let key = LAST_MAP_KEY.with(|key| key.borrow().clone());
        let _guard = key.map(|key| SegmentGuard::new(Segment::Field(key)));
        seed.deserialize(&mut *self.de)
    }
}

// ===========================================================================
// Enum adapter helper: external tag + error location
// ===========================================================================

struct EnumReader<'a, 'de: 'a> {
    de: &'a mut Deserializer<'de>,
    /// The actual tag of the variant selected in `variant_seed`. Kept around
    /// so the payload methods can extend the container path.
    tag: Option<&'static str>,
}

impl<'de> EnumAccess<'de> for EnumReader<'_, 'de> {
    type Error = LocatedError;
    type Variant = Self;

    fn variant_seed<V>(mut self, seed: V) -> Result<(V::Value, Self), Self::Error>
    where
        V: DeserializeSeed<'de>,
    {
        let tag = match self.de.next()? {
            Token::UnitVariant { variant }
            | Token::NewtypeVariant { variant }
            | Token::TupleVariant { variant, .. }
            | Token::StructVariant { variant, .. } => variant,
            other => return Err(Deserializer::unexpected(other)),
        };
        self.tag = Some(tag);
        // The tag is handed over as a *borrowed* string. On the success path
        // no allocation takes place, so the zero-copy lifetime is unchanged.
        let value = seed.deserialize(BorrowedTagDeserializer { tag })?;
        Ok((value, self))
    }
}

impl<'de> VariantAccess<'de> for EnumReader<'_, 'de> {
    type Error = LocatedError;

    fn unit_variant(self) -> Result<(), Self::Error> {
        // The unit variant token was consumed by `variant_seed`; nothing
        // remains in the input for a payload.
        Ok(())
    }

    fn newtype_variant_seed<T>(self, seed: T) -> Result<T::Value, Self::Error>
    where
        T: DeserializeSeed<'de>,
    {
        // The externally attached tag is part of the container path while the
        // payload is being deserialized.
        let _guard = SegmentGuard::new(Segment::Variant(
            self.tag.unwrap_or_default().to_owned(),
        ));
        seed.deserialize(self.de)
    }

    fn tuple_variant<V>(self, _len: usize, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        let _guard = SegmentGuard::new(Segment::Variant(
            self.tag.unwrap_or_default().to_owned(),
        ));
        self.de
            .visit_seq_content(Token::TupleVariantEnd, visitor)
    }

    fn struct_variant<V>(
        self,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        let _guard = SegmentGuard::new(Segment::Variant(
            self.tag.unwrap_or_default().to_owned(),
        ));
        self.de
            .visit_map_content(Token::StructVariantEnd, visitor)
    }
}

/// Deserializer for the externally attached tag. Always yields the borrowed
/// tag, so on success no allocation is performed and `'de` is preserved.
struct BorrowedTagDeserializer {
    tag: &'static str,
}

impl<'de> de::Deserializer<'de> for BorrowedTagDeserializer {
    type Error = LocatedError;

    fn deserialize_any<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        visitor.visit_borrowed_str(self.tag)
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
        bytes byte_buf option unit unit_struct newtype_struct seq tuple
        tuple_struct map struct enum identifier ignored_any
    }
}

// ===========================================================================
// Fixtures and entry points
// ===========================================================================

#[derive(PartialEq, Debug, Deserialize)]
enum Inner {
    Unit,
    Newtype(i32),
    Tuple(i32, i32),
    Struct { n: i32 },
}

#[derive(PartialEq, Debug, Deserialize)]
struct Outer {
    kind: Inner,
}

fn from_tokens<'de, T>(tokens: &'de [Token]) -> Result<T, LocatedError>
where
    T: Deserialize<'de>,
{
    PATH.with(|path| path.borrow_mut().clear());
    LAST_MAP_KEY.with(|key| *key.borrow_mut() = None);
    let mut de = Deserializer::new(tokens);
    T::deserialize(&mut de)
}

const INNER_VARIANTS: &[&str] = &["Unit", "Newtype", "Tuple", "Struct"];

// -- Case 1: normal deserialization through seq and map nesting -------------

#[test]
fn test_nested_ok_seq() {
    let tokens = [
        Token::Seq { len: Some(2) },
        Token::UnitVariant { variant: "Unit" },
        Token::TupleVariant {
            variant: "Tuple",
            len: 2,
        },
        Token::I32(3),
        Token::I32(4),
        Token::TupleVariantEnd,
        Token::SeqEnd,
    ];
    let value: Vec<Inner> = from_tokens(&tokens).unwrap();
    assert_eq!(value, vec![Inner::Unit, Inner::Tuple(3, 4)]);
}

#[test]
fn test_nested_ok_map() {
    let tokens = [
        Token::Map { len: Some(1) },
        Token::BorrowedStr("kind"),
        Token::StructVariant {
            variant: "Struct",
            len: 1,
        },
        Token::BorrowedStr("n"),
        Token::I32(7),
        Token::StructVariantEnd,
        Token::MapEnd,
    ];
    let value: Outer = from_tokens(&tokens).unwrap();
    assert_eq!(
        value,
        Outer {
            kind: Inner::Struct { n: 7 }
        }
    );
}

/// Successful tag/payload matching stays zero-copy: the externally attached
/// tag is forwarded as a borrowed string and the `&str` payload borrows
/// straight from the input tokens, so the deserializer's input lifetime is
/// unchanged.
#[test]
fn test_nested_ok_borrowed_payload() {
    #[derive(PartialEq, Debug, Deserialize)]
    enum Borrowing<'a> {
        Newtype(&'a str),
    }

    let tokens = [
        Token::NewtypeVariant { variant: "Newtype" },
        Token::BorrowedStr("payload"),
    ];
    let value: Borrowing<'_> = from_tokens(&tokens).unwrap();
    assert_eq!(value, Borrowing::Newtype("payload"));
}

// -- Case 2: unknown tag through nested seq and map paths -------------------

#[test]
fn test_nested_unknown_tag_seq() {
    let tokens = [
        Token::Seq { len: Some(2) },
        Token::I32(1),
        Token::UnitVariant { variant: "Nope" },
        Token::SeqEnd,
    ];
    let error = from_tokens::<(i32, Inner)>(&tokens).unwrap_err();

    assert_eq!(error.kind, ErrorKind::UnknownVariant);
    assert_eq!(error.tag.as_deref(), Some("Nope"));
    assert_eq!(error.path, "[1]");
    assert_eq!(error.expected.as_deref(), Some(INNER_VARIANTS));
    assert_eq!(
        error.message,
        "unknown variant `Nope`, expected one of `Unit`, `Newtype`, `Tuple`, `Struct`",
    );
    assert_eq!(
        error.to_string(),
        "[1]: unknown variant `Nope`, expected one of `Unit`, `Newtype`, `Tuple`, `Struct`",
    );
}

#[test]
fn test_nested_unknown_tag_map() {
    #[derive(PartialEq, Debug, Deserialize)]
    struct Wrap {
        outer: Outer,
        tail: bool,
    }

    let tokens = [
        Token::Map { len: Some(2) },
        Token::BorrowedStr("outer"),
        Token::Map { len: Some(1) },
        Token::BorrowedStr("kind"),
        Token::UnitVariant { variant: "Bogus" },
        Token::MapEnd,
        Token::BorrowedStr("tail"),
        Token::Bool(true),
        Token::MapEnd,
    ];
    let error = from_tokens::<Wrap>(&tokens).unwrap_err();

    assert_eq!(error.kind, ErrorKind::UnknownVariant);
    assert_eq!(error.tag.as_deref(), Some("Bogus"));
    assert_eq!(error.path, "outer.kind");
    assert_eq!(error.expected.as_deref(), Some(INNER_VARIANTS));
    assert_eq!(
        error.to_string(),
        "outer.kind: unknown variant `Bogus`, expected one of `Unit`, `Newtype`, `Tuple`, `Struct`",
    );
}

// -- Case 3: bad payload: category stays `custom`, path includes the tag -----

#[test]
fn test_nested_bad_payload_newtype() {
    let tokens = [
        Token::Map { len: Some(1) },
        Token::BorrowedStr("kind"),
        Token::NewtypeVariant { variant: "Newtype" },
        Token::Bool(false),
        Token::MapEnd,
    ];
    let error = from_tokens::<Outer>(&tokens).unwrap_err();

    assert_eq!(error.kind, ErrorKind::Custom);
    assert_eq!(error.tag, None);
    assert_eq!(error.expected, None);
    assert_eq!(error.path, "kind.Newtype");
    assert_eq!(error.message, "invalid type: boolean `false`, expected i32");
    assert_eq!(
        error.to_string(),
        "kind.Newtype: invalid type: boolean `false`, expected i32",
    );
}

#[test]
fn test_nested_bad_payload_tuple_seq() {
    let tokens = [
        Token::Seq { len: Some(1) },
        Token::TupleVariant {
            variant: "Tuple",
            len: 2,
        },
        Token::I32(1),
        Token::Bool(false),
        Token::TupleVariantEnd,
        Token::SeqEnd,
    ];
    let error = from_tokens::<Vec<Inner>>(&tokens).unwrap_err();

    assert_eq!(error.kind, ErrorKind::Custom);
    assert_eq!(error.path, "[0].Tuple[1]");
    assert_eq!(
        error.to_string(),
        "[0].Tuple[1]: invalid type: boolean `false`, expected i32",
    );
}

#[test]
fn test_nested_bad_payload_struct_map() {
    let tokens = [
        Token::Map { len: Some(1) },
        Token::BorrowedStr("kind"),
        Token::StructVariant {
            variant: "Struct",
            len: 1,
        },
        Token::BorrowedStr("n"),
        Token::Str("not an int"),
        Token::StructVariantEnd,
        Token::MapEnd,
    ];
    let error = from_tokens::<Outer>(&tokens).unwrap_err();

    assert_eq!(error.kind, ErrorKind::Custom);
    assert_eq!(error.path, "kind.Struct.n");
    assert_eq!(
        error.to_string(),
        "kind.Struct.n: invalid type: string \"not an int\", expected i32",
    );
}

// Prints the structured error location data for a quick human/CI inspection.
#[test]
fn test_demo_locations() {
    let unknown_seq = [
        Token::Seq { len: Some(2) },
        Token::I32(1),
        Token::UnitVariant { variant: "Nope" },
        Token::SeqEnd,
    ];
    let err = from_tokens::<(i32, Inner)>(&unknown_seq).unwrap_err();
    println!(
        "unknown/seq: kind={:?} path={:?} tag={:?} expected={:?}\n             display=\"{}\"",
        err.kind, err.path, err.tag, err.expected, err,
    );

    let unknown_map = [
        Token::Map { len: Some(1) },
        Token::BorrowedStr("kind"),
        Token::UnitVariant { variant: "Bogus" },
        Token::MapEnd,
    ];
    let err = from_tokens::<Outer>(&unknown_map).unwrap_err();
    println!(
        "unknown/map: kind={:?} path={:?} tag={:?} expected={:?}\n             display=\"{}\"",
        err.kind, err.path, err.tag, err.expected, err,
    );

    let bad_newtype = [
        Token::NewtypeVariant { variant: "Newtype" },
        Token::Bool(false),
    ];
    let err = from_tokens::<Inner>(&bad_newtype).unwrap_err();
    println!(
        "bad payload: kind={:?} path={:?} tag={:?} expected={:?}\n             display=\"{}\"",
        err.kind, err.path, err.tag, err.expected, err,
    );
}

