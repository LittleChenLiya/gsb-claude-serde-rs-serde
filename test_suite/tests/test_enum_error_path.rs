//! Wires externally-tagged enum error positions (container path + actual
//! variant tag) into the token test format's data flow.
//!
//! The `TrackDe` adapter below wraps the test format's `Deserializer` and its
//! enum/seq/map access helpers so that a failing deserialization reports the
//! container path (`groups.alpha[1].meta`) and, for unknown variants, the
//! actual tag that was rejected. The original serde error keeps its
//! `unknown_variant` category, its expected candidates, and its concrete
//! error type; successful borrows keep their `'de` lifetime.
//!
//! Everything is self-contained: the token format is a small in-memory
//! fixture built on the public `serde_test::Token` type, and the default
//! (unadapted) interface is regression-tested through serde_test's public
//! assert API. No external format or path-tracking dependency is used.

#![allow(clippy::needless_lifetimes)]

use serde::de::{
    self, Deserialize, DeserializeSeed, Deserializer, EnumAccess, IntoDeserializer, MapAccess,
    SeqAccess, VariantAccess, Visitor,
};
use serde_derive::Deserialize;
use serde_test::{assert_de_tokens_error, Token};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::error::Error as StdError;
use std::fmt::{self, Display};
use std::rc::Rc;

//////////////////////////////////////////////////////////////////////////
// Minimal in-memory token test format
//////////////////////////////////////////////////////////////////////////

/// Error type of the token test format. Only `custom` is implemented by
/// hand; `unknown_variant`, `invalid_type` and friends keep serde's original
/// default implementations, so the error category and the expected
/// candidates are exactly the ones serde itself produces.
#[derive(Debug, Clone, PartialEq)]
struct TokenError {
    msg: String,
}

impl de::Error for TokenError {
    fn custom<T: Display>(msg: T) -> Self {
        TokenError {
            msg: msg.to_string(),
        }
    }
}

impl Display for TokenError {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str(&self.msg)
    }
}

impl StdError for TokenError {}

/// A minimal externally-tagged token format over `serde_test::Token`,
/// mirroring the conventions of serde_test's own deserializer.
#[derive(Debug)]
struct TokenDeserializer<'de> {
    tokens: &'de [Token],
}

impl<'de> TokenDeserializer<'de> {
    fn new(tokens: &'de [Token]) -> Self {
        TokenDeserializer { tokens }
    }

    fn remaining(&self) -> usize {
        self.tokens.len()
    }

    fn peek(&self) -> Result<Token, TokenError> {
        self.tokens
            .first()
            .copied()
            .ok_or_else(|| de::Error::custom("ran out of tokens to deserialize"))
    }

    fn next(&mut self) -> Result<Token, TokenError> {
        match self.tokens.split_first() {
            Some((&first, rest)) => {
                self.tokens = rest;
                Ok(first)
            }
            None => Err(de::Error::custom("ran out of tokens to deserialize")),
        }
    }

    fn expect(&mut self, expected: Token) -> Result<(), TokenError> {
        let token = self.next()?;
        if token == expected {
            Ok(())
        } else {
            Err(de::Error::custom(format!(
                "expected Token::{} but deserialization wants Token::{}",
                token, expected
            )))
        }
    }
}

macro_rules! forward_to_any {
    ($($method:ident;)*) => {$(
        fn $method<V>(self, visitor: V) -> Result<V::Value, TokenError>
        where
            V: Visitor<'de>,
        {
            self.deserialize_any(visitor)
        }
    )*};
}

impl<'de, 'a> Deserializer<'de> for &'a mut TokenDeserializer<'de> {
    type Error = TokenError;

    fn deserialize_any<V>(self, visitor: V) -> Result<V::Value, TokenError>
    where
        V: Visitor<'de>,
    {
        match self.next()? {
            Token::Bool(v) => visitor.visit_bool(v),
            Token::I8(v) => visitor.visit_i8(v),
            Token::I16(v) => visitor.visit_i16(v),
            Token::I32(v) => visitor.visit_i32(v),
            Token::I64(v) => visitor.visit_i64(v),
            Token::U8(v) => visitor.visit_u8(v),
            Token::U16(v) => visitor.visit_u16(v),
            Token::U32(v) => visitor.visit_u32(v),
            Token::U64(v) => visitor.visit_u64(v),
            Token::F32(v) => visitor.visit_f32(v),
            Token::F64(v) => visitor.visit_f64(v),
            Token::Char(v) => visitor.visit_char(v),
            Token::Str(v) => visitor.visit_str(v),
            Token::BorrowedStr(v) => visitor.visit_borrowed_str(v),
            Token::String(v) => visitor.visit_string(v.to_owned()),
            Token::Unit | Token::UnitStruct { .. } => visitor.visit_unit(),
            Token::None => visitor.visit_none(),
            Token::Some => visitor.visit_some(self),
            Token::Seq { len } => {
                visitor.visit_seq(TokenSeq::new(self, Token::SeqEnd, len))
            }
            Token::Tuple { len } => {
                visitor.visit_seq(TokenSeq::new(self, Token::TupleEnd, Some(len)))
            }
            Token::Map { len } => {
                visitor.visit_map(TokenMap::new(self, Token::MapEnd, len))
            }
            Token::Struct { len, .. } => {
                visitor.visit_map(TokenMap::new(self, Token::StructEnd, Some(len)))
            }
            Token::Enum { .. } => visitor.visit_enum(TokenEnum { de: self }),
            Token::UnitVariant { variant, .. } => visitor.visit_str(variant),
            token => Err(de::Error::custom(format!(
                "deserialization did not expect this token: {}",
                token
            ))),
        }
    }

    forward_to_any! {
        deserialize_bool;
        deserialize_i8;
        deserialize_i16;
        deserialize_i32;
        deserialize_i64;
        deserialize_i128;
        deserialize_u8;
        deserialize_u16;
        deserialize_u32;
        deserialize_u64;
        deserialize_u128;
        deserialize_f32;
        deserialize_f64;
        deserialize_char;
        deserialize_str;
        deserialize_string;
        deserialize_bytes;
        deserialize_byte_buf;
        deserialize_unit;
        deserialize_identifier;
        deserialize_ignored_any;
    }

    fn deserialize_option<V>(self, visitor: V) -> Result<V::Value, TokenError>
    where
        V: Visitor<'de>,
    {
        match self.peek()? {
            Token::Unit | Token::None => {
                self.next()?;
                visitor.visit_none()
            }
            Token::Some => {
                self.next()?;
                visitor.visit_some(self)
            }
            _ => self.deserialize_any(visitor),
        }
    }

    fn deserialize_unit_struct<V>(
        self,
        name: &'static str,
        visitor: V,
    ) -> Result<V::Value, TokenError>
    where
        V: Visitor<'de>,
    {
        self.expect(Token::UnitStruct { name })?;
        visitor.visit_unit()
    }

    fn deserialize_newtype_struct<V>(
        self,
        name: &'static str,
        visitor: V,
    ) -> Result<V::Value, TokenError>
    where
        V: Visitor<'de>,
    {
        self.expect(Token::NewtypeStruct { name })?;
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_seq<V>(self, visitor: V) -> Result<V::Value, TokenError>
    where
        V: Visitor<'de>,
    {
        match self.next()? {
            Token::Seq { len } => visitor.visit_seq(TokenSeq::new(self, Token::SeqEnd, len)),
            token => Err(de::Error::custom(format!(
                "expected Token::Seq but deserialization wants Token::{}",
                token
            ))),
        }
    }

    fn deserialize_tuple<V>(self, _len: usize, visitor: V) -> Result<V::Value, TokenError>
    where
        V: Visitor<'de>,
    {
        match self.next()? {
            Token::Tuple { len } => {
                visitor.visit_seq(TokenSeq::new(self, Token::TupleEnd, Some(len)))
            }
            Token::Seq { len } => visitor.visit_seq(TokenSeq::new(self, Token::SeqEnd, len)),
            token => Err(de::Error::custom(format!(
                "expected Token::Tuple but deserialization wants Token::{}",
                token
            ))),
        }
    }

    fn deserialize_tuple_struct<V>(
        self,
        _name: &'static str,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, TokenError>
    where
        V: Visitor<'de>,
    {
        self.deserialize_tuple(len, visitor)
    }

    fn deserialize_map<V>(self, visitor: V) -> Result<V::Value, TokenError>
    where
        V: Visitor<'de>,
    {
        match self.next()? {
            Token::Map { len } => visitor.visit_map(TokenMap::new(self, Token::MapEnd, len)),
            token => Err(de::Error::custom(format!(
                "expected Token::Map but deserialization wants Token::{}",
                token
            ))),
        }
    }

    fn deserialize_struct<V>(
        self,
        name: &'static str,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, TokenError>
    where
        V: Visitor<'de>,
    {
        match self.next()? {
            Token::Struct { name: n, len } if n == name => {
                visitor.visit_map(TokenMap::new(self, Token::StructEnd, Some(len)))
            }
            token => Err(de::Error::custom(format!(
                "expected Token::Struct but deserialization wants Token::{}",
                token
            ))),
        }
    }

    fn deserialize_enum<V>(
        self,
        name: &'static str,
        _variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, TokenError>
    where
        V: Visitor<'de>,
    {
        match self.peek()? {
            Token::Enum { name: n } if n == name => {
                self.next()?;
                visitor.visit_enum(TokenEnum { de: self })
            }
            Token::UnitVariant { name: n, .. } | Token::NewtypeVariant { name: n, .. }
                if n == name =>
            {
                visitor.visit_enum(TokenEnum { de: self })
            }
            token => Err(de::Error::custom(format!(
                "expected Token::Enum but deserialization wants Token::{}",
                token
            ))),
        }
    }

    fn is_human_readable(&self) -> bool {
        true
    }
}

struct TokenSeq<'a, 'de> {
    de: &'a mut TokenDeserializer<'de>,
    end: Token,
    len: Option<usize>,
}

impl<'a, 'de> TokenSeq<'a, 'de> {
    fn new(de: &'a mut TokenDeserializer<'de>, end: Token, len: Option<usize>) -> Self {
        TokenSeq { de, end, len }
    }
}

impl<'de, 'a> SeqAccess<'de> for TokenSeq<'a, 'de> {
    type Error = TokenError;

    fn next_element_seed<T>(&mut self, seed: T) -> Result<Option<T::Value>, TokenError>
    where
        T: DeserializeSeed<'de>,
    {
        if self.de.peek()? == self.end {
            self.de.next()?;
            return Ok(None);
        }
        self.len = self.len.map(|len| len.saturating_sub(1));
        seed.deserialize(&mut *self.de).map(Some)
    }

    fn size_hint(&self) -> Option<usize> {
        self.len
    }
}

struct TokenMap<'a, 'de> {
    de: &'a mut TokenDeserializer<'de>,
    end: Token,
    len: Option<usize>,
}

impl<'a, 'de> TokenMap<'a, 'de> {
    fn new(de: &'a mut TokenDeserializer<'de>, end: Token, len: Option<usize>) -> Self {
        TokenMap { de, end, len }
    }
}

impl<'de, 'a> MapAccess<'de> for TokenMap<'a, 'de> {
    type Error = TokenError;

    fn next_key_seed<K>(&mut self, seed: K) -> Result<Option<K::Value>, TokenError>
    where
        K: DeserializeSeed<'de>,
    {
        if self.de.peek()? == self.end {
            self.de.next()?;
            return Ok(None);
        }
        self.len = self.len.map(|len| len.saturating_sub(1));
        seed.deserialize(&mut *self.de).map(Some)
    }

    fn next_value_seed<V>(&mut self, seed: V) -> Result<V::Value, TokenError>
    where
        V: DeserializeSeed<'de>,
    {
        seed.deserialize(&mut *self.de)
    }

    fn size_hint(&self) -> Option<usize> {
        self.len
    }
}

/// Enum access helper of the token test format: externally tagged, variant
/// identifier first, payload after.
struct TokenEnum<'a, 'de> {
    de: &'a mut TokenDeserializer<'de>,
}

impl<'de, 'a> EnumAccess<'de> for TokenEnum<'a, 'de> {
    type Error = TokenError;
    type Variant = Self;

    fn variant_seed<V>(self, seed: V) -> Result<(V::Value, Self), TokenError>
    where
        V: DeserializeSeed<'de>,
    {
        match self.de.peek()? {
            Token::UnitVariant { variant, .. } | Token::NewtypeVariant { variant, .. } => {
                let de = variant.into_deserializer();
                let value = seed.deserialize(de)?;
                Ok((value, self))
            }
            _ => {
                let value = seed.deserialize(&mut *self.de)?;
                Ok((value, self))
            }
        }
    }
}

impl<'de, 'a> VariantAccess<'de> for TokenEnum<'a, 'de> {
    type Error = TokenError;

    fn unit_variant(self) -> Result<(), TokenError> {
        match self.de.next()? {
            Token::UnitVariant { .. } | Token::Unit => Ok(()),
            token => Err(de::Error::custom(format!(
                "expected unit variant but deserialization wants Token::{}",
                token
            ))),
        }
    }

    fn newtype_variant_seed<T>(self, seed: T) -> Result<T::Value, TokenError>
    where
        T: DeserializeSeed<'de>,
    {
        if let Token::NewtypeVariant { .. } = self.de.peek()? {
            self.de.next()?;
        }
        seed.deserialize(self.de)
    }

    fn tuple_variant<V>(self, _len: usize, _visitor: V) -> Result<V::Value, TokenError>
    where
        V: Visitor<'de>,
    {
        Err(de::Error::custom(
            "tuple variants are not supported by this token fixture",
        ))
    }

    fn struct_variant<V>(
        self,
        _fields: &'static [&'static str],
        _visitor: V,
    ) -> Result<V::Value, TokenError>
    where
        V: Visitor<'de>,
    {
        Err(de::Error::custom(
            "struct variants are not supported by this token fixture",
        ))
    }
}

//////////////////////////////////////////////////////////////////////////
// Path-tracking adapter over the test format
//////////////////////////////////////////////////////////////////////////

/// One step inside the container hierarchy.
#[derive(Debug, Clone, PartialEq)]
enum Seg {
    /// Struct field or map key.
    Key(String),
    /// Sequence or tuple index.
    Index(usize),
}

/// Side channel shared by every layer of the adapter. Segments are pushed
/// before descending into a container element and popped on success; when an
/// error aborts deserialization the remaining stack is exactly the error
/// position. `tag` holds the most recent variant identifier seen by the enum
/// adapter helper.
#[derive(Clone, Default)]
struct Track {
    segs: Rc<RefCell<Vec<Seg>>>,
    tag: Rc<RefCell<Option<String>>>,
}

impl Track {
    fn push(&self, seg: Seg) {
        self.segs.borrow_mut().push(seg);
    }

    fn pop(&self) {
        self.segs.borrow_mut().pop();
    }

    fn path(&self) -> String {
        let mut out = String::new();
        for seg in self.segs.borrow().iter() {
            match seg {
                Seg::Key(key) => {
                    if !out.is_empty() {
                        out.push('.');
                    }
                    out.push_str(key);
                }
                Seg::Index(i) => {
                    out.push('[');
                    out.push_str(&i.to_string());
                    out.push(']');
                }
            }
        }
        out
    }
}

/// Error returned by [`deserialize_tracked`]. The original serde error is
/// preserved untouched as `inner` (same type, same `unknown_variant` category,
/// same expected candidates); the adapter only adds the container `path` and
/// the actual variant `tag` around it.
#[derive(Debug)]
struct PathError {
    path: String,
    tag: Option<String>,
    inner: TokenError,
}

impl PathError {
    fn path(&self) -> &str {
        &self.path
    }

    fn variant_tag(&self) -> Option<&str> {
        self.tag.as_deref()
    }

    fn inner(&self) -> &TokenError {
        &self.inner
    }
}

impl Display for PathError {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        if self.path.is_empty() {
            Display::fmt(&self.inner, formatter)
        } else {
            write!(formatter, "{}: {}", self.path, self.inner)
        }
    }
}

impl StdError for PathError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        Some(&self.inner)
    }
}

/// Deserialize `T` from test-format tokens, tracking the container path and
/// the actual variant tag through the adapter.
fn deserialize_tracked<'de, T>(tokens: &'de [Token]) -> Result<T, PathError>
where
    T: Deserialize<'de>,
{
    let track = Track::default();
    let mut token_de = TokenDeserializer::new(tokens);
    let result = T::deserialize(TrackDe {
        de: &mut token_de,
        track: track.clone(),
    });
    match result {
        Ok(value) => {
            assert_eq!(
                token_de.remaining(),
                0,
                "tokens left over after successful deserialization"
            );
            Ok(value)
        }
        Err(inner) => Err(PathError {
            path: track.path(),
            tag: track.tag.borrow().clone(),
            inner,
        }),
    }
}

/// Deserializer wrapper. The error type is exactly `D::Error`, so serde's
/// original error category flows through unchanged.
struct TrackDe<D> {
    de: D,
    track: Track,
}

macro_rules! track_deserialize {
    () => {};
    ($method:ident; $($rest:tt)*) => {
        fn $method<V>(self, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            self.de.$method(TrackVisitor {
                visitor,
                track: self.track,
            })
        }
        track_deserialize! { $($rest)* }
    };
    ($method:ident($($arg:ident : $ty:ty),*); $($rest:tt)*) => {
        fn $method<V>(self, $($arg: $ty,)* visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            self.de.$method(
                $($arg,)*
                TrackVisitor {
                    visitor,
                    track: self.track,
                },
            )
        }
        track_deserialize! { $($rest)* }
    };
}

impl<'de, D> Deserializer<'de> for TrackDe<D>
where
    D: Deserializer<'de>,
{
    type Error = D::Error;

    track_deserialize! {
        deserialize_any;
        deserialize_bool;
        deserialize_i8;
        deserialize_i16;
        deserialize_i32;
        deserialize_i64;
        deserialize_i128;
        deserialize_u8;
        deserialize_u16;
        deserialize_u32;
        deserialize_u64;
        deserialize_u128;
        deserialize_f32;
        deserialize_f64;
        deserialize_char;
        deserialize_str;
        deserialize_string;
        deserialize_bytes;
        deserialize_byte_buf;
        deserialize_option;
        deserialize_unit;
        deserialize_unit_struct(name: &'static str);
        deserialize_newtype_struct(name: &'static str);
        deserialize_seq;
        deserialize_tuple(len: usize);
        deserialize_tuple_struct(name: &'static str, len: usize);
        deserialize_map;
        deserialize_struct(name: &'static str, fields: &'static [&'static str]);
        deserialize_enum(name: &'static str, variants: &'static [&'static str]);
        deserialize_identifier;
        deserialize_ignored_any;
    }

    fn is_human_readable(&self) -> bool {
        true
    }
}

/// Visitor wrapper: forwards every scalar untouched (borrows included) and
/// wraps only the container/enum entry points.
struct TrackVisitor<V> {
    visitor: V,
    track: Track,
}

macro_rules! forward_visit {
    () => {};
    ($method:ident(); $($rest:tt)*) => {
        fn $method<E>(self) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            self.visitor.$method()
        }
        forward_visit! { $($rest)* }
    };
    ($method:ident($ty:ty); $($rest:tt)*) => {
        fn $method<E>(self, v: $ty) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            self.visitor.$method(v)
        }
        forward_visit! { $($rest)* }
    };
}

impl<'de, V> Visitor<'de> for TrackVisitor<V>
where
    V: Visitor<'de>,
{
    type Value = V::Value;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        self.visitor.expecting(formatter)
    }

    forward_visit! {
        visit_bool(bool);
        visit_i8(i8);
        visit_i16(i16);
        visit_i32(i32);
        visit_i64(i64);
        visit_i128(i128);
        visit_u8(u8);
        visit_u16(u16);
        visit_u32(u32);
        visit_u64(u64);
        visit_u128(u128);
        visit_f32(f32);
        visit_f64(f64);
        visit_char(char);
        visit_str(&str);
        visit_borrowed_str(&'de str);
        visit_string(String);
        visit_bytes(&[u8]);
        visit_borrowed_bytes(&'de [u8]);
        visit_byte_buf(Vec<u8>);
        visit_none();
        visit_unit();
    }

    fn visit_some<D2>(self, de: D2) -> Result<Self::Value, D2::Error>
    where
        D2: Deserializer<'de>,
    {
        self.visitor.visit_some(TrackDe {
            de,
            track: self.track,
        })
    }

    fn visit_newtype_struct<D2>(self, de: D2) -> Result<Self::Value, D2::Error>
    where
        D2: Deserializer<'de>,
    {
        self.visitor.visit_newtype_struct(TrackDe {
            de,
            track: self.track,
        })
    }

    fn visit_seq<A>(self, acc: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        self.visitor.visit_seq(TrackSeq {
            acc,
            track: self.track,
            index: 0,
        })
    }

    fn visit_map<A>(self, acc: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        self.visitor.visit_map(TrackMap {
            acc,
            track: self.track,
            key: None,
        })
    }

    fn visit_enum<A>(self, acc: A) -> Result<Self::Value, A::Error>
    where
        A: EnumAccess<'de>,
    {
        self.visitor.visit_enum(TrackEnum {
            acc,
            track: self.track,
        })
    }
}

/// Seed wrapper so nested values are deserialized through [`TrackDe`].
struct TrackSeed<S> {
    seed: S,
    track: Track,
}

impl<'de, S> DeserializeSeed<'de> for TrackSeed<S>
where
    S: DeserializeSeed<'de>,
{
    type Value = S::Value;

    fn deserialize<D>(self, de: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        self.seed.deserialize(TrackDe {
            de,
            track: self.track,
        })
    }
}

struct TrackSeq<A> {
    acc: A,
    track: Track,
    index: usize,
}

impl<'de, A> SeqAccess<'de> for TrackSeq<A>
where
    A: SeqAccess<'de>,
{
    type Error = A::Error;

    fn next_element_seed<T>(&mut self, seed: T) -> Result<Option<T::Value>, Self::Error>
    where
        T: DeserializeSeed<'de>,
    {
        self.track.push(Seg::Index(self.index));
        let result = self.acc.next_element_seed(TrackSeed {
            seed,
            track: self.track.clone(),
        });
        if result.is_ok() {
            self.track.pop();
        }
        self.index += 1;
        result
    }

    fn size_hint(&self) -> Option<usize> {
        self.acc.size_hint()
    }
}

struct TrackMap<A> {
    acc: A,
    track: Track,
    key: Option<String>,
}

impl<'de, A> MapAccess<'de> for TrackMap<A>
where
    A: MapAccess<'de>,
{
    type Error = A::Error;

    fn next_key_seed<K>(&mut self, seed: K) -> Result<Option<K::Value>, Self::Error>
    where
        K: DeserializeSeed<'de>,
    {
        let sink = Rc::new(RefCell::new(None));
        let result = self.acc.next_key_seed(RecordSeed {
            seed,
            sink: sink.clone(),
        });
        if let Ok(Some(_)) = result {
            self.key = sink.borrow_mut().take();
        }
        result
    }

    fn next_value_seed<V>(&mut self, seed: V) -> Result<V::Value, Self::Error>
    where
        V: DeserializeSeed<'de>,
    {
        self.track.push(Seg::Key(self.key.take().unwrap_or_else(|| "?".to_owned())));
        let result = self.acc.next_value_seed(TrackSeed {
            seed,
            track: self.track.clone(),
        });
        if result.is_ok() {
            self.track.pop();
        }
        result
    }

    fn size_hint(&self) -> Option<usize> {
        self.acc.size_hint()
    }
}

/// Enum adapter helper: captures the actual variant tag while the variant
/// identifier is deserialized, so an `unknown_variant` error can be reported
/// together with the tag that was rejected.
struct TrackEnum<A> {
    acc: A,
    track: Track,
}

impl<'de, A> EnumAccess<'de> for TrackEnum<A>
where
    A: EnumAccess<'de>,
{
    type Error = A::Error;
    type Variant = TrackVariant<A::Variant>;

    fn variant_seed<V>(self, seed: V) -> Result<(V::Value, Self::Variant), Self::Error>
    where
        V: DeserializeSeed<'de>,
    {
        *self.track.tag.borrow_mut() = None;
        let (value, variant) = self.acc.variant_seed(RecordSeed {
            seed,
            sink: self.track.tag.clone(),
        })?;
        Ok((
            value,
            TrackVariant {
                variant,
                track: self.track,
            },
        ))
    }
}

struct TrackVariant<V> {
    variant: V,
    track: Track,
}

impl<'de, V> VariantAccess<'de> for TrackVariant<V>
where
    V: VariantAccess<'de>,
{
    type Error = V::Error;

    fn unit_variant(self) -> Result<(), Self::Error> {
        self.variant.unit_variant()
    }

    fn newtype_variant_seed<T>(self, seed: T) -> Result<T::Value, Self::Error>
    where
        T: DeserializeSeed<'de>,
    {
        self.variant.newtype_variant_seed(TrackSeed {
            seed,
            track: self.track,
        })
    }

    fn tuple_variant<V2>(self, len: usize, visitor: V2) -> Result<V2::Value, Self::Error>
    where
        V2: Visitor<'de>,
    {
        self.variant.tuple_variant(
            len,
            TrackVisitor {
                visitor,
                track: self.track,
            },
        )
    }

    fn struct_variant<V2>(
        self,
        fields: &'static [&'static str],
        visitor: V2,
    ) -> Result<V2::Value, Self::Error>
    where
        V2: Visitor<'de>,
    {
        self.variant.struct_variant(
            fields,
            TrackVisitor {
                visitor,
                track: self.track,
            },
        )
    }
}

/// Seed wrapper that records any string seen while deserializing a map key or
/// enum variant identifier.
struct RecordSeed<S> {
    seed: S,
    sink: Rc<RefCell<Option<String>>>,
}

impl<'de, S> DeserializeSeed<'de> for RecordSeed<S>
where
    S: DeserializeSeed<'de>,
{
    type Value = S::Value;

    fn deserialize<D>(self, de: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        self.seed.deserialize(RecordDe {
            de,
            sink: self.sink,
        })
    }
}

struct RecordDe<D> {
    de: D,
    sink: Rc<RefCell<Option<String>>>,
}

macro_rules! record_wrap {
    ($($method:ident;)*) => {$(
        fn $method<V>(self, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            self.de.$method(RecordVisitor {
                visitor,
                sink: self.sink,
            })
        }
    )*};
}

macro_rules! record_passthrough {
    () => {};
    ($method:ident; $($rest:tt)*) => {
        fn $method<V>(self, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            self.de.$method(visitor)
        }
        record_passthrough! { $($rest)* }
    };
    ($method:ident($($arg:ident : $ty:ty),*); $($rest:tt)*) => {
        fn $method<V>(self, $($arg: $ty,)* visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            self.de.$method($($arg,)* visitor)
        }
        record_passthrough! { $($rest)* }
    };
}

impl<'de, D> Deserializer<'de> for RecordDe<D>
where
    D: Deserializer<'de>,
{
    type Error = D::Error;

    record_wrap! {
        deserialize_any;
        deserialize_identifier;
        deserialize_str;
        deserialize_string;
    }

    record_passthrough! {
        deserialize_bool;
        deserialize_i8;
        deserialize_i16;
        deserialize_i32;
        deserialize_i64;
        deserialize_i128;
        deserialize_u8;
        deserialize_u16;
        deserialize_u32;
        deserialize_u64;
        deserialize_u128;
        deserialize_f32;
        deserialize_f64;
        deserialize_char;
        deserialize_bytes;
        deserialize_byte_buf;
        deserialize_option;
        deserialize_unit;
        deserialize_unit_struct(name: &'static str);
        deserialize_newtype_struct(name: &'static str);
        deserialize_seq;
        deserialize_tuple(len: usize);
        deserialize_tuple_struct(name: &'static str, len: usize);
        deserialize_map;
        deserialize_struct(name: &'static str, fields: &'static [&'static str]);
        deserialize_enum(name: &'static str, variants: &'static [&'static str]);
        deserialize_ignored_any;
    }

    fn is_human_readable(&self) -> bool {
        self.de.is_human_readable()
    }
}

struct RecordVisitor<V> {
    visitor: V,
    sink: Rc<RefCell<Option<String>>>,
}

impl<V> RecordVisitor<V> {
    fn record(&self, v: &str) {
        *self.sink.borrow_mut() = Some(v.to_owned());
    }
}

impl<'de, V> Visitor<'de> for RecordVisitor<V>
where
    V: Visitor<'de>,
{
    type Value = V::Value;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        self.visitor.expecting(formatter)
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        self.record(v);
        self.visitor.visit_str(v)
    }

    fn visit_borrowed_str<E>(self, v: &'de str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        self.record(v);
        self.visitor.visit_borrowed_str(v)
    }

    fn visit_string<E>(self, v: String) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        self.record(&v);
        self.visitor.visit_string(v)
    }

    forward_visit! {
        visit_bool(bool);
        visit_i8(i8);
        visit_i16(i16);
        visit_i32(i32);
        visit_i64(i64);
        visit_i128(i128);
        visit_u8(u8);
        visit_u16(u16);
        visit_u32(u32);
        visit_u64(u64);
        visit_u128(u128);
        visit_f32(f32);
        visit_f64(f64);
        visit_char(char);
        visit_bytes(&[u8]);
        visit_borrowed_bytes(&'de [u8]);
        visit_byte_buf(Vec<u8>);
        visit_none();
        visit_unit();
    }

    fn visit_some<D2>(self, de: D2) -> Result<Self::Value, D2::Error>
    where
        D2: Deserializer<'de>,
    {
        self.visitor.visit_some(de)
    }

    fn visit_newtype_struct<D2>(self, de: D2) -> Result<Self::Value, D2::Error>
    where
        D2: Deserializer<'de>,
    {
        self.visitor.visit_newtype_struct(de)
    }

    fn visit_seq<A>(self, acc: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        self.visitor.visit_seq(acc)
    }

    fn visit_map<A>(self, acc: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        self.visitor.visit_map(acc)
    }

    fn visit_enum<A>(self, acc: A) -> Result<Self::Value, A::Error>
    where
        A: EnumAccess<'de>,
    {
        self.visitor.visit_enum(acc)
    }
}

//////////////////////////////////////////////////////////////////////////
// Fixtures: externally-tagged enum nested in struct -> map -> seq -> struct
//////////////////////////////////////////////////////////////////////////

#[derive(Debug, PartialEq, Deserialize)]
enum Payload {
    A,
    B(i32),
}

#[derive(Debug, PartialEq, Deserialize)]
struct Item {
    meta: Payload,
}

#[derive(Debug, PartialEq, Deserialize)]
struct Outer {
    groups: BTreeMap<String, Vec<Item>>,
}

/// Tokens for `Outer { groups: { "alpha": [Item { meta: A }, Item { meta:
/// <second> }] } }` where the second item's meta tokens are supplied by the
/// caller.
fn nested_tokens(second_meta: &[Token]) -> Vec<Token> {
    let mut tokens = vec![
        Token::Struct {
            name: "Outer",
            len: 1,
        },
        Token::Str("groups"),
        Token::Map { len: Some(1) },
        Token::Str("alpha"),
        Token::Seq { len: Some(2) },
        Token::Struct {
            name: "Item",
            len: 1,
        },
        Token::Str("meta"),
        Token::UnitVariant {
            name: "Payload",
            variant: "A",
        },
        Token::StructEnd,
        Token::Struct {
            name: "Item",
            len: 1,
        },
        Token::Str("meta"),
    ];
    tokens.extend_from_slice(second_meta);
    tokens.extend_from_slice(&[
        Token::StructEnd,
        Token::SeqEnd,
        Token::MapEnd,
        Token::StructEnd,
    ]);
    tokens
}

fn expected_outer() -> Outer {
    let mut groups = BTreeMap::new();
    groups.insert(
        "alpha".to_owned(),
        vec![Item { meta: Payload::A }, Item { meta: Payload::B(7) }],
    );
    Outer { groups }
}

/// The message serde itself produces for this unknown variant, built through
/// the original `de::Error::unknown_variant` interface.
fn unknown_variant_message() -> String {
    let err: TokenError = de::Error::unknown_variant("Nope", &["A", "B"]);
    err.to_string()
}

//////////////////////////////////////////////////////////////////////////
// Tests
//////////////////////////////////////////////////////////////////////////

#[test]
fn test_nested_enum_ok() {
    let tokens = nested_tokens(&[
        Token::Enum { name: "Payload" },
        Token::Str("B"),
        Token::I32(7),
    ]);
    let value: Outer = deserialize_tracked(&tokens).unwrap();
    assert_eq!(value, expected_outer());
}

#[test]
fn test_nested_enum_unknown_variant() {
    let tokens = nested_tokens(&[Token::Enum { name: "Payload" }, Token::Str("Nope")]);
    let err = deserialize_tracked::<Outer>(&tokens).unwrap_err();

    // Container path and actual tag are attached by the adapter.
    assert_eq!(err.path(), "groups.alpha[1].meta");
    assert_eq!(err.variant_tag(), Some("Nope"));

    // The original serde error is preserved: same `unknown_variant` category,
    // same expected candidates, same concrete TokenError type.
    assert_eq!(err.inner().to_string(), unknown_variant_message());
    assert_eq!(
        err.inner().to_string(),
        "unknown variant `Nope`, expected `A` or `B`"
    );
    assert_eq!(
        err.to_string(),
        "groups.alpha[1].meta: unknown variant `Nope`, expected `A` or `B`"
    );
}

#[test]
fn test_nested_enum_bad_payload() {
    let tokens = nested_tokens(&[
        Token::Enum { name: "Payload" },
        Token::Str("B"),
        Token::Bool(true),
    ]);
    let err = deserialize_tracked::<Outer>(&tokens).unwrap_err();

    // Path points at the enum whose payload failed; the adapter also reports
    // which variant's payload was being deserialized.
    assert_eq!(err.path(), "groups.alpha[1].meta");
    assert_eq!(err.variant_tag(), Some("B"));

    // Original invalid-type error is preserved untouched.
    assert_eq!(
        err.inner().to_string(),
        "invalid type: boolean `true`, expected i32"
    );
    assert_eq!(
        err.to_string(),
        "groups.alpha[1].meta: invalid type: boolean `true`, expected i32"
    );
}

#[test]
fn test_successful_borrow_lifetimes_unchanged() {
    // A plain borrowed str flows through the adapter with its 'de lifetime.
    let str_tokens = [Token::BorrowedStr("hello")];
    let text: &str = deserialize_tracked(&str_tokens).unwrap();
    assert_eq!(text, "hello");

    // A borrowed enum payload keeps borrowing after a successful deserialize.
    #[derive(Debug, PartialEq, Deserialize)]
    enum Borrowed<'a> {
        #[serde(borrow)]
        Text(&'a str),
    }

    let enum_tokens = [
        Token::Enum { name: "Borrowed" },
        Token::Str("Text"),
        Token::BorrowedStr("world"),
    ];
    let value: Borrowed<'_> = deserialize_tracked(&enum_tokens).unwrap();
    assert_eq!(value, Borrowed::Text("world"));
}

/// Short regression for the original (unadapted) interface: serde_test still
/// reports serde's `unknown_variant` error with its expected candidates, and
/// the adapter's preserved inner error matches it exactly.
#[test]
fn test_default_interface_unknown_variant_regression() {
    assert_de_tokens_error::<Payload>(
        &[Token::Enum { name: "Payload" }, Token::Str("Nope")],
        "unknown variant `Nope`, expected `A` or `B`",
    );

    // The default interface message and the adapter's preserved inner error
    // are the same string, so results line up between the two interfaces.
    assert_eq!(
        unknown_variant_message(),
        "unknown variant `Nope`, expected `A` or `B`"
    );
}
