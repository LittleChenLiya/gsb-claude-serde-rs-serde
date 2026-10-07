//! Deserializer adapter example: observe *declared* struct fields during
//! deserialization and record whether each one was missing from the input,
//! explicitly null, or held a real value, while reusing the result produced
//! by the derived `Deserialize` implementation unchanged.
//!
//! Properties of the adapter:
//!
//! * Only declared fields are recorded. Unknown fields are skipped by the
//!   derived implementation as usual and their content is never inspected.
//! * Nested structs keep their path (`inner.b`).
//! * A failing recorder never masks the original deserialization result or
//!   its error; recorder failures are collected on the side.
//! * Observation is opt-in: when the option is not enabled the original
//!   `Deserializer` interface keeps working untouched.

use serde::de::{self, DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
use serde::Deserialize;
use serde_derive::Deserialize as DeriveDeserialize;
use std::cell::{Cell, RefCell};
use std::fmt;
use std::fs;
use std::path::Path;
use std::rc::Rc;

//////////////////////////////////////////////////////////////////////////
// Mini test format
//////////////////////////////////////////////////////////////////////////

mod mini {
    use serde::de::{self, DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
    use std::fmt;

    /// Tokens of a minimal test format: just enough to drive derived structs
    /// made of `Option` fields, plain scalars and nested structs.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Tok {
        Struct { name: &'static str },
        StructEnd,
        Seq { name: &'static str },
        SeqEnd,
        Key(&'static str),
        Str(&'static str),
        U32(u32),
        Some,
        Null,
    }

    fn kind(tok: Tok) -> &'static str {
        match tok {
            Tok::Struct { .. } => "struct",
            Tok::StructEnd => "struct end",
            Tok::Seq { .. } => "sequence",
            Tok::SeqEnd => "sequence end",
            Tok::Key(_) => "key",
            Tok::Str(_) => "string",
            Tok::U32(_) => "u32",
            Tok::Some => "some",
            Tok::Null => "null",
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    pub struct MiniError(pub String);

    impl fmt::Display for MiniError {
        fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str(&self.0)
        }
    }

    impl std::error::Error for MiniError {}

    impl de::Error for MiniError {
        fn custom<T>(msg: T) -> Self
        where
            T: fmt::Display,
        {
            MiniError(msg.to_string())
        }
    }

    /// A minimal token-stream deserializer used as the test format.
    pub struct MiniDeserializer {
        tokens: &'static [Tok],
        pos: usize,
    }

    impl MiniDeserializer {
        pub fn new(tokens: &'static [Tok]) -> Self {
            MiniDeserializer { tokens, pos: 0 }
        }

        fn peek(&self) -> Result<Tok, MiniError> {
            self.tokens
                .get(self.pos)
                .copied()
                .ok_or_else(|| MiniError("unexpected end of input".to_owned()))
        }

        fn next(&mut self) -> Result<Tok, MiniError> {
            let tok = self.peek()?;
            self.pos += 1;
            Ok(tok)
        }

        /// Skip one whole value without looking at its content.
        fn skip_value(&mut self) -> Result<(), MiniError> {
            match self.next()? {
                Tok::Struct { .. } | Tok::Seq { .. } => {
                    let mut depth = 1;
                    while depth > 0 {
                        match self.next()? {
                            Tok::Struct { .. } | Tok::Seq { .. } => depth += 1,
                            Tok::StructEnd | Tok::SeqEnd => depth -= 1,
                            _ => {}
                        }
                    }
                    Ok(())
                }
                Tok::StructEnd | Tok::SeqEnd => {
                    Err(MiniError("unexpected end of compound value".to_owned()))
                }
                _ => Ok(()),
            }
        }
    }

    macro_rules! unsupported {
        ($($method:ident)*) => {
            $(
                fn $method<V>(self, visitor: V) -> Result<V::Value, Self::Error>
                where
                    V: Visitor<'de>,
                {
                    let _ = visitor;
                    Err(MiniError(
                        concat!("mini deserializer does not support ", stringify!($method))
                            .to_owned(),
                    ))
                }
            )*
        };
    }

    impl<'de> Deserializer<'de> for &mut MiniDeserializer {
        type Error = MiniError;

        unsupported! {
            deserialize_bool
            deserialize_i8
            deserialize_i16
            deserialize_i32
            deserialize_i64
            deserialize_i128
            deserialize_u8
            deserialize_u16
            deserialize_u64
            deserialize_u128
            deserialize_f32
            deserialize_f64
            deserialize_char
            deserialize_bytes
            deserialize_byte_buf
            deserialize_unit
            deserialize_seq
            deserialize_map
        }

        fn deserialize_any<V>(self, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            match self.peek()? {
                Tok::Str(_) => self.deserialize_str(visitor),
                Tok::U32(_) => self.deserialize_u32(visitor),
                Tok::Struct { name } | Tok::Seq { name } => {
                    self.deserialize_struct(name, &[], visitor)
                }
                Tok::Some | Tok::Null => self.deserialize_option(visitor),
                tok => Err(MiniError(format!(
                    "deserialize_any not supported for {}",
                    kind(tok)
                ))),
            }
        }

        fn deserialize_str<V>(self, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            match self.next()? {
                Tok::Str(value) => visitor.visit_str(value),
                tok => Err(MiniError(format!(
                    "invalid type: {}, expected a string",
                    kind(tok)
                ))),
            }
        }

        fn deserialize_string<V>(self, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            self.deserialize_str(visitor)
        }

        fn deserialize_u32<V>(self, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            match self.next()? {
                Tok::U32(value) => visitor.visit_u32(value),
                tok => Err(MiniError(format!(
                    "invalid type: {}, expected u32",
                    kind(tok)
                ))),
            }
        }

        fn deserialize_option<V>(self, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            match self.next()? {
                Tok::Null => visitor.visit_none(),
                Tok::Some => visitor.visit_some(self),
                tok => Err(MiniError(format!(
                    "invalid type: {}, expected an option",
                    kind(tok)
                ))),
            }
        }

        fn deserialize_struct<V>(
            self,
            name: &'static str,
            _fields: &'static [&'static str],
            visitor: V,
        ) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            match self.next()? {
                Tok::Struct { name: found } if found == name => {
                    visitor.visit_map(MiniMap { de: self })
                }
                Tok::Seq { name: found } if found == name => {
                    visitor.visit_seq(MiniSeq { de: self })
                }
                Tok::Struct { name: found } | Tok::Seq { name: found } => Err(MiniError(
                    format!("expected struct `{name}`, found `{found}`"),
                )),
                tok => Err(MiniError(format!(
                    "invalid type: {}, expected struct `{name}`",
                    kind(tok)
                ))),
            }
        }

        fn deserialize_identifier<V>(self, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            match self.next()? {
                Tok::Key(key) => visitor.visit_str(key),
                tok => Err(MiniError(format!(
                    "invalid type: {}, expected a field key",
                    kind(tok)
                ))),
            }
        }

        fn deserialize_ignored_any<V>(self, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            self.skip_value()?;
            visitor.visit_unit()
        }

        fn deserialize_unit_struct<V>(
            self,
            _name: &'static str,
            visitor: V,
        ) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            let _ = visitor;
            Err(MiniError("unsupported".to_owned()))
        }

        fn deserialize_newtype_struct<V>(
            self,
            _name: &'static str,
            visitor: V,
        ) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            let _ = visitor;
            Err(MiniError("unsupported".to_owned()))
        }

        fn deserialize_tuple<V>(self, _len: usize, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            let _ = visitor;
            Err(MiniError("unsupported".to_owned()))
        }

        fn deserialize_tuple_struct<V>(
            self,
            _name: &'static str,
            _len: usize,
            visitor: V,
        ) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            let _ = visitor;
            Err(MiniError("unsupported".to_owned()))
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
            let _ = visitor;
            Err(MiniError("unsupported".to_owned()))
        }

        fn is_human_readable(&self) -> bool {
            true
        }
    }

    struct MiniMap<'a> {
        de: &'a mut MiniDeserializer,
    }

    impl<'de> MapAccess<'de> for MiniMap<'_> {
        type Error = MiniError;

        fn next_key_seed<K>(&mut self, seed: K) -> Result<Option<K::Value>, Self::Error>
        where
            K: DeserializeSeed<'de>,
        {
            if self.de.peek()? == Tok::StructEnd {
                self.de.next()?;
                return Ok(None);
            }
            seed.deserialize(&mut *self.de).map(Some)
        }

        fn next_value_seed<V>(&mut self, seed: V) -> Result<V::Value, Self::Error>
        where
            V: DeserializeSeed<'de>,
        {
            seed.deserialize(&mut *self.de)
        }
    }

    struct MiniSeq<'a> {
        de: &'a mut MiniDeserializer,
    }

    impl<'de> SeqAccess<'de> for MiniSeq<'_> {
        type Error = MiniError;

        fn next_element_seed<T>(&mut self, seed: T) -> Result<Option<T::Value>, Self::Error>
        where
            T: DeserializeSeed<'de>,
        {
            if self.de.peek()? == Tok::SeqEnd {
                self.de.next()?;
                return Ok(None);
            }
            seed.deserialize(&mut *self.de).map(Some)
        }
    }
}

use mini::{MiniDeserializer, MiniError, Tok};

//////////////////////////////////////////////////////////////////////////
// Missing-field observer (Deserializer adapter)
//////////////////////////////////////////////////////////////////////////

/// Whether a declared field was seen in the input, and in which shape.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Presence {
    /// The field key never appeared in the input.
    Missing,
    /// The field key appeared with an explicit null.
    Null,
    /// The field key appeared with a real (non-null) value.
    Value,
}

impl Presence {
    fn as_str(self) -> &'static str {
        match self {
            Presence::Missing => "missing",
            Presence::Null => "null",
            Presence::Value => "value",
        }
    }
}

/// One observation about a declared field.
#[derive(Clone, PartialEq, Eq, Debug)]
struct FieldRecord {
    /// Dotted path from the observed root, e.g. `inner.b`.
    path: String,
    presence: Presence,
}

impl fmt::Display for FieldRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        write!(formatter, "{}={}", self.path, self.presence.as_str())
    }
}

struct State {
    enabled: bool,
    path: Vec<&'static str>,
    records: Vec<FieldRecord>,
    recorder_errors: Vec<String>,
    recorder: Option<Box<dyn FnMut(&FieldRecord) -> Result<(), String>>>,
}

impl State {
    fn emit(&mut self, record: FieldRecord) {
        if let Some(recorder) = &mut self.recorder {
            // A recorder failure is collected on the side; it must never
            // replace or mask the deserialization result or its error.
            if let Err(err) = recorder(&record) {
                self.recorder_errors.push(err);
            }
        }
        self.records.push(record);
    }
}

/// Handle used to configure observation and to collect the records
/// afterwards. Clone it to observe several documents with the same recorder.
#[derive(Clone)]
struct MissingFieldObserver {
    state: Rc<RefCell<State>>,
}

impl MissingFieldObserver {
    /// Observation enabled.
    fn new() -> Self {
        MissingFieldObserver {
            state: Rc::new(RefCell::new(State {
                enabled: true,
                path: Vec::new(),
                records: Vec::new(),
                recorder_errors: Vec::new(),
                recorder: None,
            })),
        }
    }

    /// Observation disabled: `wrap` still works but forwards the original
    /// deserializer untouched and records nothing.
    fn disabled() -> Self {
        let observer = MissingFieldObserver::new();
        observer.state.borrow_mut().enabled = false;
        observer
    }

    /// Sink for records. Its errors are collected, never propagated as
    /// deserialization errors.
    fn with_recorder(
        self,
        recorder: impl FnMut(&FieldRecord) -> Result<(), String> + 'static,
    ) -> Self {
        self.state.borrow_mut().recorder = Some(Box::new(recorder));
        self
    }

    /// Adapt any deserializer. The returned value is consumed by
    /// `Deserialize::deserialize`.
    fn wrap<D>(&self, de: D) -> Observer<D> {
        Observer {
            de,
            state: Rc::clone(&self.state),
        }
    }

    fn records(&self) -> Vec<FieldRecord> {
        self.state.borrow().records.clone()
    }

    fn recorder_errors(&self) -> Vec<String> {
        self.state.borrow().recorder_errors.clone()
    }
}

impl Default for MissingFieldObserver {
    fn default() -> Self {
        MissingFieldObserver::new()
    }
}

fn mark(seen: &RefCell<Vec<Option<Presence>>>, idx: usize, presence: Presence) {
    seen.borrow_mut()[idx] = Some(presence);
}

/// Forward the scalar and container methods that the observer does not care
/// about to the wrapped deserializer. Each method starts with `self.before()`,
/// an inherent method that is a no-op for plain wrappers and records a real
/// value for the current field in the field-value wrapper.
macro_rules! delegate_scalars {
    () => {
        fn deserialize_bool<V>(self, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            self.before();
            self.de.deserialize_bool(visitor)
        }

        fn deserialize_i8<V>(self, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            self.before();
            self.de.deserialize_i8(visitor)
        }

        fn deserialize_i16<V>(self, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            self.before();
            self.de.deserialize_i16(visitor)
        }

        fn deserialize_i32<V>(self, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            self.before();
            self.de.deserialize_i32(visitor)
        }

        fn deserialize_i64<V>(self, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            self.before();
            self.de.deserialize_i64(visitor)
        }

        fn deserialize_i128<V>(self, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            self.before();
            self.de.deserialize_i128(visitor)
        }

        fn deserialize_u8<V>(self, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            self.before();
            self.de.deserialize_u8(visitor)
        }

        fn deserialize_u16<V>(self, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            self.before();
            self.de.deserialize_u16(visitor)
        }

        fn deserialize_u32<V>(self, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            self.before();
            self.de.deserialize_u32(visitor)
        }

        fn deserialize_u64<V>(self, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            self.before();
            self.de.deserialize_u64(visitor)
        }

        fn deserialize_u128<V>(self, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            self.before();
            self.de.deserialize_u128(visitor)
        }

        fn deserialize_f32<V>(self, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            self.before();
            self.de.deserialize_f32(visitor)
        }

        fn deserialize_f64<V>(self, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            self.before();
            self.de.deserialize_f64(visitor)
        }

        fn deserialize_char<V>(self, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            self.before();
            self.de.deserialize_char(visitor)
        }

        fn deserialize_unit<V>(self, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            self.before();
            self.de.deserialize_unit(visitor)
        }

        fn deserialize_unit_struct<V>(
            self,
            name: &'static str,
            visitor: V,
        ) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            self.before();
            self.de.deserialize_unit_struct(name, visitor)
        }

        fn deserialize_newtype_struct<V>(
            self,
            name: &'static str,
            visitor: V,
        ) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            self.before();
            self.de.deserialize_newtype_struct(name, visitor)
        }

        fn deserialize_seq<V>(self, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            self.before();
            self.de.deserialize_seq(visitor)
        }

        fn deserialize_tuple<V>(self, len: usize, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            self.before();
            self.de.deserialize_tuple(len, visitor)
        }

        fn deserialize_tuple_struct<V>(
            self,
            name: &'static str,
            len: usize,
            visitor: V,
        ) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            self.before();
            self.de.deserialize_tuple_struct(name, len, visitor)
        }

        fn deserialize_map<V>(self, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            self.before();
            self.de.deserialize_map(visitor)
        }

        fn deserialize_enum<V>(
            self,
            name: &'static str,
            variants: &'static [&'static str],
            visitor: V,
        ) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            self.before();
            self.de.deserialize_enum(name, variants, visitor)
        }

        fn deserialize_ignored_any<V>(self, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            self.before();
            self.de.deserialize_ignored_any(visitor)
        }

        fn is_human_readable(&self) -> bool {
            self.de.is_human_readable()
        }
    };
}

/// Forward the remaining methods without inspecting them.
macro_rules! delegate_transparent {
    () => {
        delegate_scalars!();

        fn deserialize_any<V>(self, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            self.de.deserialize_any(visitor)
        }

        fn deserialize_str<V>(self, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            self.de.deserialize_str(visitor)
        }

        fn deserialize_string<V>(self, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            self.de.deserialize_string(visitor)
        }

        fn deserialize_bytes<V>(self, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            self.de.deserialize_bytes(visitor)
        }

        fn deserialize_byte_buf<V>(self, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            self.de.deserialize_byte_buf(visitor)
        }

        fn deserialize_option<V>(self, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            self.de.deserialize_option(visitor)
        }

        fn deserialize_identifier<V>(self, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            self.de.deserialize_identifier(visitor)
        }
    };
}

/// The top-level adapter produced by [`MissingFieldObserver::wrap`].
struct Observer<D> {
    de: D,
    state: Rc<RefCell<State>>,
}

impl<D> Observer<D> {
    fn before(&self) {}
}

impl<'de, D> Deserializer<'de> for Observer<D>
where
    D: Deserializer<'de>,
{
    type Error = D::Error;

    delegate_transparent!();

    fn deserialize_struct<V>(
        self,
        name: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        if self.state.borrow().enabled {
            track_struct(self.de, name, fields, visitor, &self.state, None)
        } else {
            // Option not enabled: the original interface is used unchanged.
            self.de.deserialize_struct(name, fields, visitor)
        }
    }
}

/// Deserializer wrapper for the content of `Some(..)`: keeps tracking nested
/// structs under the current path.
struct StructTracker<D> {
    de: D,
    state: Rc<RefCell<State>>,
    segment: Option<&'static str>,
}

impl<D> StructTracker<D> {
    fn before(&self) {}
}

impl<'de, D> Deserializer<'de> for StructTracker<D>
where
    D: Deserializer<'de>,
{
    type Error = D::Error;

    delegate_transparent!();

    fn deserialize_struct<V>(
        self,
        name: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        track_struct(self.de, name, fields, visitor, &self.state, self.segment)
    }
}

/// Deserializer wrapper for the value of one declared field. Distinguishes
/// an explicit null from a real value and recurses into nested structs.
struct FieldValueDeserializer<D> {
    de: D,
    state: Rc<RefCell<State>>,
    seen: Rc<RefCell<Vec<Option<Presence>>>>,
    idx: usize,
    segment: &'static str,
}

impl<D> FieldValueDeserializer<D> {
    fn before(&self) {
        mark(&self.seen, self.idx, Presence::Value);
    }
}

impl<'de, D> Deserializer<'de> for FieldValueDeserializer<D>
where
    D: Deserializer<'de>,
{
    type Error = D::Error;

    delegate_scalars!();

    fn deserialize_any<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        mark(&self.seen, self.idx, Presence::Value);
        self.de.deserialize_any(visitor)
    }

    fn deserialize_str<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        mark(&self.seen, self.idx, Presence::Value);
        self.de.deserialize_str(visitor)
    }

    fn deserialize_string<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        mark(&self.seen, self.idx, Presence::Value);
        self.de.deserialize_string(visitor)
    }

    fn deserialize_bytes<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        mark(&self.seen, self.idx, Presence::Value);
        self.de.deserialize_bytes(visitor)
    }

    fn deserialize_byte_buf<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        mark(&self.seen, self.idx, Presence::Value);
        self.de.deserialize_byte_buf(visitor)
    }

    fn deserialize_identifier<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        mark(&self.seen, self.idx, Presence::Value);
        self.de.deserialize_identifier(visitor)
    }

    fn deserialize_option<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.de.deserialize_option(OptionVisitor {
            inner: visitor,
            state: self.state,
            seen: self.seen,
            idx: self.idx,
            segment: self.segment,
        })
    }

    fn deserialize_struct<V>(
        self,
        name: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        mark(&self.seen, self.idx, Presence::Value);
        track_struct(self.de, name, fields, visitor, &self.state, Some(self.segment))
    }
}

/// Visitor that tells an explicit null apart from a real value.
struct OptionVisitor<V> {
    inner: V,
    state: Rc<RefCell<State>>,
    seen: Rc<RefCell<Vec<Option<Presence>>>>,
    idx: usize,
    segment: &'static str,
}

impl<'de, V> Visitor<'de> for OptionVisitor<V>
where
    V: Visitor<'de>,
{
    type Value = V::Value;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        self.inner.expecting(formatter)
    }

    fn visit_none<E>(self) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        mark(&self.seen, self.idx, Presence::Null);
        self.inner.visit_none()
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        mark(&self.seen, self.idx, Presence::Null);
        self.inner.visit_unit()
    }

    fn visit_some<D>(self, de: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        mark(&self.seen, self.idx, Presence::Value);
        self.inner.visit_some(StructTracker {
            de,
            state: self.state,
            segment: Some(self.segment),
        })
    }
}

/// Run `deserialize_struct` on the wrapped deserializer while tracking which
/// declared fields were seen. Emits one record per declared field, in
/// declaration order, once the struct has been deserialized successfully.
fn track_struct<'de, D, V>(
    de: D,
    name: &'static str,
    fields: &'static [&'static str],
    visitor: V,
    state: &Rc<RefCell<State>>,
    segment: Option<&'static str>,
) -> Result<V::Value, D::Error>
where
    D: Deserializer<'de>,
    V: Visitor<'de>,
{
    if let Some(segment) = segment {
        state.borrow_mut().path.push(segment);
    }
    let seen = Rc::new(RefCell::new(vec![None; fields.len()]));
    let visitor = StructVisitor {
        inner: visitor,
        fields,
        seen: Rc::clone(&seen),
        state: Rc::clone(state),
    };
    let result = de.deserialize_struct(name, fields, visitor);
    // The result of the derived implementation is reused unchanged; an
    // original error propagates as-is.
    let value = match result {
        Ok(value) => value,
        Err(err) => {
            if segment.is_some() {
                state.borrow_mut().path.pop();
            }
            return Err(err);
        }
    };
    {
        // Emit while this struct's own segment is still on the path, so
        // nested records keep their dotted prefix.
        let mut state = state.borrow_mut();
        let prefix = state.path.join(".");
        for (idx, field) in fields.iter().enumerate() {
            let presence = seen.borrow()[idx].unwrap_or(Presence::Missing);
            let path = if prefix.is_empty() {
                (*field).to_owned()
            } else {
                format!("{prefix}.{field}")
            };
            state.emit(FieldRecord { path, presence });
        }
    }
    if segment.is_some() {
        state.borrow_mut().path.pop();
    }
    Ok(value)
}

/// Visitor that installs the tracking map/seq access around the derived one.
struct StructVisitor<V> {
    inner: V,
    fields: &'static [&'static str],
    seen: Rc<RefCell<Vec<Option<Presence>>>>,
    state: Rc<RefCell<State>>,
}

impl<'de, V> Visitor<'de> for StructVisitor<V>
where
    V: Visitor<'de>,
{
    type Value = V::Value;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        self.inner.expecting(formatter)
    }

    fn visit_map<A>(self, map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        self.inner.visit_map(TrackingMap {
            inner: map,
            fields: self.fields,
            seen: self.seen,
            state: self.state,
            current: Rc::new(Cell::new(None)),
        })
    }

    fn visit_seq<A>(self, seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        self.inner.visit_seq(TrackingSeq {
            inner: seq,
            fields: self.fields,
            seen: self.seen,
            state: self.state,
            index: 0,
        })
    }
}

/// Map access that matches each key against the declared fields. Unknown
/// keys are forwarded untouched and their content is never inspected.
struct TrackingMap<A> {
    inner: A,
    fields: &'static [&'static str],
    seen: Rc<RefCell<Vec<Option<Presence>>>>,
    state: Rc<RefCell<State>>,
    current: Rc<Cell<Option<usize>>>,
}

impl<'de, A> MapAccess<'de> for TrackingMap<A>
where
    A: MapAccess<'de>,
{
    type Error = A::Error;

    fn next_key_seed<K>(&mut self, seed: K) -> Result<Option<K::Value>, Self::Error>
    where
        K: DeserializeSeed<'de>,
    {
        self.current.set(None);
        self.inner.next_key_seed(KeySeed {
            inner: seed,
            fields: self.fields,
            seen: Rc::clone(&self.seen),
            current: Rc::clone(&self.current),
        })
    }

    fn next_value_seed<V>(&mut self, seed: V) -> Result<V::Value, Self::Error>
    where
        V: DeserializeSeed<'de>,
    {
        match self.current.take() {
            Some(idx) => self.inner.next_value_seed(FieldValueSeed {
                inner: seed,
                state: Rc::clone(&self.state),
                seen: Rc::clone(&self.seen),
                idx,
                segment: self.fields[idx],
            }),
            // Unknown field: forward untouched, do not inspect its content.
            None => self.inner.next_value_seed(seed),
        }
    }

    fn size_hint(&self) -> Option<usize> {
        self.inner.size_hint()
    }
}

/// Seq access for formats that drive structs positionally.
struct TrackingSeq<A> {
    inner: A,
    fields: &'static [&'static str],
    seen: Rc<RefCell<Vec<Option<Presence>>>>,
    state: Rc<RefCell<State>>,
    index: usize,
}

impl<'de, A> SeqAccess<'de> for TrackingSeq<A>
where
    A: SeqAccess<'de>,
{
    type Error = A::Error;

    fn next_element_seed<T>(&mut self, seed: T) -> Result<Option<T::Value>, Self::Error>
    where
        T: DeserializeSeed<'de>,
    {
        let index = self.index;
        self.index += 1;
        if index < self.fields.len() {
            self.inner.next_element_seed(FieldValueSeed {
                inner: seed,
                state: Rc::clone(&self.state),
                seen: Rc::clone(&self.seen),
                idx: index,
                segment: self.fields[index],
            })
        } else {
            self.inner.next_element_seed(seed)
        }
    }

    fn size_hint(&self) -> Option<usize> {
        self.inner.size_hint()
    }
}

struct FieldValueSeed<V> {
    inner: V,
    state: Rc<RefCell<State>>,
    seen: Rc<RefCell<Vec<Option<Presence>>>>,
    idx: usize,
    segment: &'static str,
}

impl<'de, V> DeserializeSeed<'de> for FieldValueSeed<V>
where
    V: DeserializeSeed<'de>,
{
    type Value = V::Value;

    fn deserialize<D>(self, de: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        self.inner.deserialize(FieldValueDeserializer {
            de,
            state: self.state,
            seen: self.seen,
            idx: self.idx,
            segment: self.segment,
        })
    }
}

struct KeySeed<K> {
    inner: K,
    fields: &'static [&'static str],
    seen: Rc<RefCell<Vec<Option<Presence>>>>,
    current: Rc<Cell<Option<usize>>>,
}

impl<'de, K> DeserializeSeed<'de> for KeySeed<K>
where
    K: DeserializeSeed<'de>,
{
    type Value = K::Value;

    fn deserialize<D>(self, de: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        self.inner.deserialize(KeyDeserializer {
            de,
            fields: self.fields,
            seen: self.seen,
            current: self.current,
        })
    }
}

/// Deserializer wrapper that captures the text of a field key while it is
/// being deserialized by the derived field-identifier visitor.
struct KeyDeserializer<D> {
    de: D,
    fields: &'static [&'static str],
    seen: Rc<RefCell<Vec<Option<Presence>>>>,
    current: Rc<Cell<Option<usize>>>,
}

impl<D> KeyDeserializer<D> {
    fn before(&self) {}

    fn key_visitor<V>(&self, inner: V) -> KeyVisitor<V> {
        KeyVisitor {
            inner,
            fields: self.fields,
            seen: Rc::clone(&self.seen),
            current: Rc::clone(&self.current),
        }
    }
}

impl<'de, D> Deserializer<'de> for KeyDeserializer<D>
where
    D: Deserializer<'de>,
{
    type Error = D::Error;

    delegate_scalars!();

    fn deserialize_any<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        let visitor = self.key_visitor(visitor);
        self.de.deserialize_any(visitor)
    }

    fn deserialize_str<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        let visitor = self.key_visitor(visitor);
        self.de.deserialize_str(visitor)
    }

    fn deserialize_string<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        let visitor = self.key_visitor(visitor);
        self.de.deserialize_string(visitor)
    }

    fn deserialize_bytes<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        let visitor = self.key_visitor(visitor);
        self.de.deserialize_bytes(visitor)
    }

    fn deserialize_byte_buf<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        let visitor = self.key_visitor(visitor);
        self.de.deserialize_byte_buf(visitor)
    }

    fn deserialize_identifier<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        let visitor = self.key_visitor(visitor);
        self.de.deserialize_identifier(visitor)
    }

    fn deserialize_option<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.de.deserialize_option(visitor)
    }

    fn deserialize_struct<V>(
        self,
        name: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.de.deserialize_struct(name, fields, visitor)
    }
}

/// Visitor that records a declared field key and forwards everything to the
/// derived field-identifier visitor.
struct KeyVisitor<V> {
    inner: V,
    fields: &'static [&'static str],
    seen: Rc<RefCell<Vec<Option<Presence>>>>,
    current: Rc<Cell<Option<usize>>>,
}

impl<V> KeyVisitor<V> {
    fn record(&self, key: &str) {
        // Only declared fields are recorded; unknown keys are left alone.
        if let Some(idx) = self.fields.iter().position(|field| field == &key) {
            self.seen.borrow_mut()[idx] = Some(Presence::Value);
            self.current.set(Some(idx));
        }
    }
}

impl<'de, V> Visitor<'de> for KeyVisitor<V>
where
    V: Visitor<'de>,
{
    type Value = V::Value;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        self.inner.expecting(formatter)
    }

    fn visit_str<E>(self, key: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        self.record(key);
        self.inner.visit_str(key)
    }

    fn visit_borrowed_str<E>(self, key: &'de str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        self.record(key);
        self.inner.visit_borrowed_str(key)
    }

    fn visit_string<E>(self, key: String) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        self.record(&key);
        self.inner.visit_string(key)
    }

    fn visit_bytes<E>(self, key: &[u8]) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        if let Ok(key) = std::str::from_utf8(key) {
            self.record(key);
        }
        self.inner.visit_bytes(key)
    }

    fn visit_borrowed_bytes<E>(self, key: &'de [u8]) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        if let Ok(key) = std::str::from_utf8(key) {
            self.record(key);
        }
        self.inner.visit_borrowed_bytes(key)
    }

    fn visit_byte_buf<E>(self, key: Vec<u8>) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        if let Ok(key) = std::str::from_utf8(&key) {
            self.record(key);
        }
        self.inner.visit_byte_buf(key)
    }

    fn visit_u64<E>(self, index: u64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        self.inner.visit_u64(index)
    }

    fn visit_i64<E>(self, index: i64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        self.inner.visit_i64(index)
    }
}

//////////////////////////////////////////////////////////////////////////
// Fixtures
//////////////////////////////////////////////////////////////////////////

#[derive(DeriveDeserialize, PartialEq, Debug)]
struct Inner {
    b: Option<u32>,
}

#[derive(DeriveDeserialize, PartialEq, Debug)]
struct Outer {
    a: Option<u32>,
    inner: Option<Inner>,
    keep: String,
}

fn record(path: &str, presence: Presence) -> FieldRecord {
    FieldRecord {
        path: path.to_owned(),
        presence,
    }
}

fn observe(tokens: &'static [Tok]) -> (Result<Outer, MiniError>, MissingFieldObserver) {
    let observer = MissingFieldObserver::new();
    let mut de = MiniDeserializer::new(tokens);
    let result = Outer::deserialize(observer.wrap(&mut de));
    (result, observer)
}

const MISSING_TOKENS: &[Tok] = &[
    Tok::Struct { name: "Outer" },
    Tok::Key("keep"),
    Tok::Str("x"),
    Tok::StructEnd,
];

const NULL_TOKENS: &[Tok] = &[
    Tok::Struct { name: "Outer" },
    Tok::Key("a"),
    Tok::Null,
    Tok::Key("inner"),
    Tok::Null,
    Tok::Key("keep"),
    Tok::Str("x"),
    Tok::StructEnd,
];

const VALUE_TOKENS: &[Tok] = &[
    Tok::Struct { name: "Outer" },
    Tok::Key("a"),
    Tok::Some,
    Tok::U32(1),
    Tok::Key("inner"),
    Tok::Some,
    Tok::Struct { name: "Inner" },
    Tok::Key("b"),
    Tok::Some,
    Tok::U32(2),
    Tok::StructEnd,
    Tok::Key("keep"),
    Tok::Str("x"),
    // Unknown field with nested content: skipped by the derived impl, never
    // recorded nor inspected by the observer.
    Tok::Key("mystery"),
    Tok::Struct { name: "Whatever" },
    Tok::Key("deep"),
    Tok::U32(9),
    Tok::StructEnd,
    Tok::StructEnd,
];

//////////////////////////////////////////////////////////////////////////
// Tests
//////////////////////////////////////////////////////////////////////////

#[test]
fn missing_fields_are_recorded() {
    let (result, observer) = observe(MISSING_TOKENS);
    assert_eq!(
        result.unwrap(),
        Outer {
            a: None,
            inner: None,
            keep: "x".to_owned(),
        }
    );
    assert_eq!(
        observer.records(),
        vec![
            record("a", Presence::Missing),
            record("inner", Presence::Missing),
            record("keep", Presence::Value),
        ]
    );
    assert!(observer.recorder_errors().is_empty());
}

#[test]
fn explicit_null_is_distinguished_from_missing() {
    let (result, observer) = observe(NULL_TOKENS);
    // The derived result is reused unchanged: explicit null and missing both
    // deserialize to `None`, but the records tell them apart.
    assert_eq!(
        result.unwrap(),
        Outer {
            a: None,
            inner: None,
            keep: "x".to_owned(),
        }
    );
    assert_eq!(
        observer.records(),
        vec![
            record("a", Presence::Null),
            record("inner", Presence::Null),
            record("keep", Presence::Value),
        ]
    );
}

#[test]
fn present_fields_and_nested_paths_are_recorded() {
    let (result, observer) = observe(VALUE_TOKENS);
    assert_eq!(
        result.unwrap(),
        Outer {
            a: Some(1),
            inner: Some(Inner { b: Some(2) }),
            keep: "x".to_owned(),
        }
    );
    assert_eq!(
        observer.records(),
        vec![
            // Nested records keep their dotted path and are emitted when the
            // nested struct finishes, before the parent records.
            record("inner.b", Presence::Value),
            record("a", Presence::Value),
            record("inner", Presence::Value),
            record("keep", Presence::Value),
        ]
    );
}

#[test]
fn nested_missing_field_keeps_its_path() {
    let tokens: &[Tok] = &[
        Tok::Struct { name: "Outer" },
        Tok::Key("inner"),
        Tok::Some,
        Tok::Struct { name: "Inner" },
        Tok::StructEnd,
        Tok::Key("keep"),
        Tok::Str("x"),
        Tok::StructEnd,
    ];
    let (result, observer) = observe(tokens);
    assert_eq!(
        result.unwrap(),
        Outer {
            a: None,
            inner: Some(Inner { b: None }),
            keep: "x".to_owned(),
        }
    );
    assert_eq!(
        observer.records(),
        vec![
            record("inner.b", Presence::Missing),
            record("a", Presence::Missing),
            record("inner", Presence::Value),
            record("keep", Presence::Value),
        ]
    );
}

#[test]
fn seq_form_structs_are_tracked_too() {
    let tokens: &[Tok] = &[
        Tok::Seq { name: "Outer" },
        Tok::Some,
        Tok::U32(1),
        Tok::Null,
        Tok::Str("x"),
        Tok::SeqEnd,
    ];
    let (result, observer) = observe(tokens);
    assert_eq!(
        result.unwrap(),
        Outer {
            a: Some(1),
            inner: None,
            keep: "x".to_owned(),
        }
    );
    assert_eq!(
        observer.records(),
        vec![
            record("a", Presence::Value),
            record("inner", Presence::Null),
            record("keep", Presence::Value),
        ]
    );
}

#[test]
fn real_type_error_propagates_despite_failing_recorder() {
    let tokens: &[Tok] = &[
        Tok::Struct { name: "Outer" },
        Tok::Key("a"),
        Tok::Some,
        Tok::Str("oops"),
        Tok::StructEnd,
    ];
    let observer =
        MissingFieldObserver::new().with_recorder(|_| Err("recorder is broken".to_owned()));
    let mut de = MiniDeserializer::new(tokens);
    let result = Outer::deserialize(observer.wrap(&mut de));
    // The original deserialization error is surfaced, not a recorder error.
    assert_eq!(
        result.unwrap_err(),
        MiniError("invalid type: string, expected u32".to_owned())
    );
}

#[test]
fn recorder_failure_does_not_change_the_result() {
    let observer =
        MissingFieldObserver::new().with_recorder(|_| Err("recorder is broken".to_owned()));
    let mut de = MiniDeserializer::new(MISSING_TOKENS);
    let result = Outer::deserialize(observer.wrap(&mut de));
    assert_eq!(
        result.unwrap(),
        Outer {
            a: None,
            inner: None,
            keep: "x".to_owned(),
        }
    );
    // Records are still collected and recorder failures are reported on the
    // side instead of being swallowed or masking the result.
    assert_eq!(observer.records().len(), 3);
    assert_eq!(observer.recorder_errors().len(), 3);
    assert!(observer
        .recorder_errors()
        .iter()
        .all(|err| err == "recorder is broken"));
}

#[test]
fn disabled_option_uses_the_original_interface() {
    // Option not enabled: the adapter forwards everything and records nothing.
    let observer = MissingFieldObserver::disabled();
    let mut de = MiniDeserializer::new(VALUE_TOKENS);
    let result = Outer::deserialize(observer.wrap(&mut de));
    assert_eq!(
        result.unwrap(),
        Outer {
            a: Some(1),
            inner: Some(Inner { b: Some(2) }),
            keep: "x".to_owned(),
        }
    );
    assert!(observer.records().is_empty());

    // The plain original interface keeps working as before.
    let mut de = MiniDeserializer::new(VALUE_TOKENS);
    let result = Outer::deserialize(&mut de);
    assert_eq!(
        result.unwrap(),
        Outer {
            a: Some(1),
            inner: Some(Inner { b: Some(2) }),
            keep: "x".to_owned(),
        }
    );
}

#[test]
fn report_fixture_in_temp_dir() {
    let cases: [(&str, &[Tok]); 3] = [
        ("missing", MISSING_TOKENS),
        ("null", NULL_TOKENS),
        ("value", VALUE_TOKENS),
    ];
    let mut report = String::from("# missing-field observation report\n");
    for (case, tokens) in cases {
        let (result, observer) = observe(tokens);
        let value = result.unwrap();
        report.push_str(&format!("case={case} result={value:?}\n"));
        for rec in observer.records() {
            report.push_str(&format!("  {rec}\n"));
        }
    }

    let expected = concat!(
        "# missing-field observation report\n",
        "case=missing result=Outer { a: None, inner: None, keep: \"x\" }\n",
        "  a=missing\n",
        "  inner=missing\n",
        "  keep=value\n",
        "case=null result=Outer { a: None, inner: None, keep: \"x\" }\n",
        "  a=null\n",
        "  inner=null\n",
        "  keep=value\n",
        "case=value result=Outer { a: Some(1), inner: Some(Inner { b: Some(2) }), keep: \"x\" }\n",
        "  inner.b=value\n",
        "  a=value\n",
        "  inner=value\n",
        "  keep=value\n",
    );
    assert_eq!(report, expected);

    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"));
    fs::create_dir_all(dir).unwrap();
    let file = dir.join("missing_fields_report.txt");
    fs::write(&file, &report).unwrap();
    println!("report written to {}", file.display());
    assert_eq!(fs::read_to_string(&file).unwrap(), report);
}
