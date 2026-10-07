//! Example `Deserializer` adapter that observes *missing optional fields*
//! while deserializing derived structs, distinguishing three situations for
//! every declared field:
//!
//! * `Missing` -- the key never appeared in the input,
//! * `Null` -- the key appeared with an explicit `null` / `None` value,
//! * `Present` -- the key appeared with a real value.
//!
//! Properties of the adapter:
//!
//! * The deserialized value is exactly what the derived `Deserialize` impl
//!   produces; the adapter only observes, it never changes the result.
//! * Only fields declared by the struct (the `FIELDS` list serde passes to
//!   `deserialize_struct`) are recorded. Unknown keys are forwarded
//!   untouched and their content is never inspected.
//! * Nested structs keep their path: a missing field `name` inside field
//!   `inner` is recorded as `inner.name`.
//! * A recorder failure never swallows the original deserialization error:
//!   if both fail, the deserialization error is returned.
//! * The whole thing is opt-in. Code that does not call
//!   `observe_missing_fields` uses `Deserialize::deserialize` exactly as
//!   before, and passing the `()` recorder disables observation entirely.
//!
//! Known limitations of this example: enum internals and map entries
//! (dynamic keys) are forwarded without observation, and `deserialize_any`
//! based content (e.g. `#[serde(flatten)]`) is not tracked.

use serde::de::Deserialize;
use serde_derive::Deserialize as DeriveDeserialize;


mod observe {
    use serde::de::{
        self, Deserialize, DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor,
    };
    use std::cell::Cell;
    use std::fmt::{self, Display};

    /// What the observer recorded about one declared field.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub enum FieldState {
        /// The key never appeared in the input.
        Missing,
        /// The key appeared and the value was an explicit `null` / `None`.
        Null,
        /// The key appeared with a non-null value.
        Present,
    }

    /// One recorded observation: dotted path plus state.
    #[derive(Clone, PartialEq, Eq, Debug)]
    pub struct Observation {
        pub path: String,
        pub state: FieldState,
    }

    /// Sink for field observations.
    pub trait Recorder {
        type Error;
        fn record(&mut self, path: &str, state: FieldState) -> Result<(), Self::Error>;
    }

    /// Collecting recorder used by most callers.
    impl Recorder for Vec<Observation> {
        type Error = std::convert::Infallible;
        fn record(&mut self, path: &str, state: FieldState) -> Result<(), Self::Error> {
            self.push(Observation {
                path: path.to_owned(),
                state,
            });
            Ok(())
        }
    }

    /// No-op recorder: observation disabled, pure pass-through.
    impl Recorder for () {
        type Error = std::convert::Infallible;
        fn record(&mut self, _path: &str, _state: FieldState) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    impl<F, E> Recorder for F
    where
        F: FnMut(&str, FieldState) -> Result<(), E>,
    {
        type Error = E;
        fn record(&mut self, path: &str, state: FieldState) -> Result<(), E> {
            self(path, state)
        }
    }

    /// Error returned by [`observe_missing_fields`].
    ///
    /// The original deserialization error always wins: `Recorder` is only
    /// returned when deserialization itself succeeded but recording failed.
    #[derive(Debug)]
    pub enum ObserveError<De, Re> {
        Deserialize(De),
        Recorder(Re),
    }

    impl<De: Display, Re: Display> Display for ObserveError<De, Re> {
        fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            match self {
                ObserveError::Deserialize(err) => Display::fmt(err, formatter),
                ObserveError::Recorder(err) => {
                    write!(formatter, "field observation recorder failed: {err}")
                }
            }
        }
    }

    impl<De, Re> std::error::Error for ObserveError<De, Re>
    where
        De: Display + fmt::Debug,
        Re: Display + fmt::Debug,
    {
    }

    /// Deserialize `T` from `deserializer`, recording missing / explicit-null
    /// / present states of declared struct fields into `recorder`.
    ///
    /// The returned value (or error) is exactly what
    /// `T::deserialize(deserializer)` would have produced. Recorder failures
    /// are reported only when deserialization itself succeeded.
    pub fn observe_missing_fields<'de, T, D, R>(
        deserializer: D,
        recorder: &mut R,
    ) -> Result<T, ObserveError<D::Error, R::Error>>
    where
        T: Deserialize<'de>,
        D: Deserializer<'de>,
        R: Recorder,
    {
        let mut shared = Shared {
            recorder,
            err: None,
        };
        let result = T::deserialize(Observer {
            de: deserializer,
            shared: &mut shared,
            path: String::new(),
            classified: None,
        });
        match result {
            // The original deserialization error is never swallowed, even if
            // the recorder failed as well.
            Err(err) => Err(ObserveError::Deserialize(err)),
            Ok(value) => match shared.err {
                Some(err) => Err(ObserveError::Recorder(err)),
                None => Ok(value),
            },
        }
    }

    //////////////////////////////////////////////////////////////////////

    struct Shared<'r, R: Recorder> {
        recorder: &'r mut R,
        err: Option<R::Error>,
    }

    impl<'r, R: Recorder> Shared<'r, R> {
        /// Record one observation. A recorder failure is stashed, never
        /// raised here, so it cannot interrupt or replace the real
        /// deserialization result.
        fn record(&mut self, path: &str, state: FieldState) {
            if self.err.is_none() {
                if let Err(err) = self.recorder.record(path, state) {
                    self.err = Some(err);
                }
            }
        }
    }

    fn child_path(parent: &str, segment: &str) -> String {
        if parent.is_empty() {
            segment.to_owned()
        } else {
            let mut path = String::with_capacity(parent.len() + 1 + segment.len());
            path.push_str(parent);
            path.push('.');
            path.push_str(segment);
            path
        }
    }

    macro_rules! forward_to_inner {
        ($($method:ident)*) => {$(
            fn $method<V>(self, visitor: V) -> Result<V::Value, Self::Error>
            where
                V: Visitor<'de>,
            {
                self.de.$method(visitor)
            }
        )*};
    }

    //////////////////////////////////////////////////////////////////////
    // The observing Deserializer wrapper.

    struct Observer<'c, 's, 'r, D, R: Recorder> {
        de: D,
        shared: &'s mut Shared<'r, R>,
        path: String,
        /// Set only while deserializing the value of a *declared* struct
        /// field; `deserialize_option` flips it so the caller knows the
        /// field was already classified as `Null`/`Present`.
        classified: Option<&'c Cell<bool>>,
    }

    impl<'de, 'c, 's, 'r, D, R> Deserializer<'de> for Observer<'c, 's, 'r, D, R>
    where
        D: Deserializer<'de>,
        R: Recorder,
    {
        type Error = D::Error;

        forward_to_inner! {
            deserialize_any
            deserialize_bool
            deserialize_i8 deserialize_i16 deserialize_i32 deserialize_i64 deserialize_i128
            deserialize_u8 deserialize_u16 deserialize_u32 deserialize_u64 deserialize_u128
            deserialize_f32 deserialize_f64
            deserialize_char deserialize_str deserialize_string
            deserialize_bytes deserialize_byte_buf
            deserialize_unit
            deserialize_identifier
            deserialize_ignored_any
        }

        fn deserialize_option<V>(self, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            let Observer {
                de,
                shared,
                path,
                classified,
            } = self;
            let record = classified.is_some();
            if let Some(cell) = classified {
                cell.set(true);
            }
            de.deserialize_option(OptionVisitor {
                inner: visitor,
                shared,
                path,
                record,
            })
        }

        fn deserialize_unit_struct<V>(
            self,
            name: &'static str,
            visitor: V,
        ) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
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
            let Observer {
                de,
                shared,
                path,
                classified,
            } = self;
            de.deserialize_newtype_struct(
                name,
                NewtypeVisitor {
                    inner: visitor,
                    shared,
                    path,
                    classified,
                },
            )
        }

        fn deserialize_seq<V>(self, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            let Observer {
                de, shared, path, ..
            } = self;
            de.deserialize_seq(ListVisitor {
                inner: visitor,
                shared,
                path,
            })
        }

        fn deserialize_tuple<V>(self, len: usize, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            let Observer {
                de, shared, path, ..
            } = self;
            de.deserialize_tuple(
                len,
                ListVisitor {
                    inner: visitor,
                    shared,
                    path,
                },
            )
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
            let Observer {
                de, shared, path, ..
            } = self;
            de.deserialize_tuple_struct(
                name,
                len,
                ListVisitor {
                    inner: visitor,
                    shared,
                    path,
                },
            )
        }

        fn deserialize_map<V>(self, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            // Dynamic keys: map entries are not declared fields, so maps are
            // passed through without observation.
            self.de.deserialize_map(visitor)
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
            let Observer {
                de, shared, path, ..
            } = self;
            de.deserialize_struct(
                name,
                fields,
                StructVisitor {
                    inner: visitor,
                    shared,
                    path,
                    fields,
                },
            )
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
            // Enum internals are out of scope for this example.
            self.de.deserialize_enum(name, variants, visitor)
        }

        fn is_human_readable(&self) -> bool {
            self.de.is_human_readable()
        }
    }

    //////////////////////////////////////////////////////////////////////
    // Option values: distinguish explicit null from a present value.

    struct OptionVisitor<'s, 'r, V, R: Recorder> {
        inner: V,
        shared: &'s mut Shared<'r, R>,
        path: String,
        record: bool,
    }

    impl<'de, 's, 'r, V, R> Visitor<'de> for OptionVisitor<'s, 'r, V, R>
    where
        V: Visitor<'de>,
        R: Recorder,
    {
        type Value = V::Value;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            self.inner.expecting(formatter)
        }

        fn visit_none<E>(self) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            if self.record {
                self.shared.record(&self.path, FieldState::Null);
            }
            self.inner.visit_none()
        }

        fn visit_unit<E>(self) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            if self.record {
                self.shared.record(&self.path, FieldState::Null);
            }
            self.inner.visit_unit()
        }

        fn visit_some<D2>(self, deserializer: D2) -> Result<Self::Value, D2::Error>
        where
            D2: Deserializer<'de>,
        {
            if self.record {
                self.shared.record(&self.path, FieldState::Present);
            }
            self.inner.visit_some(Observer {
                de: deserializer,
                shared: self.shared,
                path: self.path,
                classified: None,
            })
        }
    }

    //////////////////////////////////////////////////////////////////////
    // Newtype structs: transparent, keep the field classification alive.

    struct NewtypeVisitor<'c, 's, 'r, V, R: Recorder> {
        inner: V,
        shared: &'s mut Shared<'r, R>,
        path: String,
        classified: Option<&'c Cell<bool>>,
    }

    impl<'de, 'c, 's, 'r, V, R> Visitor<'de> for NewtypeVisitor<'c, 's, 'r, V, R>
    where
        V: Visitor<'de>,
        R: Recorder,
    {
        type Value = V::Value;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            self.inner.expecting(formatter)
        }

        fn visit_newtype_struct<D2>(self, deserializer: D2) -> Result<Self::Value, D2::Error>
        where
            D2: Deserializer<'de>,
        {
            self.inner.visit_newtype_struct(Observer {
                de: deserializer,
                shared: self.shared,
                path: self.path,
                classified: self.classified,
            })
        }
    }

    //////////////////////////////////////////////////////////////////////
    // Structs: track which declared fields appeared, record the rest as
    // missing once the input map/sequence is exhausted.

    struct StructVisitor<'s, 'r, V, R: Recorder> {
        inner: V,
        shared: &'s mut Shared<'r, R>,
        path: String,
        fields: &'static [&'static str],
    }

    impl<'de, 's, 'r, V, R> Visitor<'de> for StructVisitor<'s, 'r, V, R>
    where
        V: Visitor<'de>,
        R: Recorder,
    {
        type Value = V::Value;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            self.inner.expecting(formatter)
        }

        fn visit_map<A>(self, map: A) -> Result<Self::Value, A::Error>
        where
            A: MapAccess<'de>,
        {
            self.inner.visit_map(ObservedMap {
                inner: map,
                shared: self.shared,
                path: self.path,
                fields: self.fields,
                seen: vec![false; self.fields.len()],
                missing_recorded: false,
                current_field: None,
            })
        }

        fn visit_seq<A>(self, seq: A) -> Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            self.inner.visit_seq(ObservedStructSeq {
                inner: seq,
                shared: self.shared,
                path: self.path,
                fields: self.fields,
                index: 0,
                missing_recorded: false,
            })
        }
    }

    struct ObservedMap<'s, 'r, M, R: Recorder> {
        inner: M,
        shared: &'s mut Shared<'r, R>,
        path: String,
        fields: &'static [&'static str],
        seen: Vec<bool>,
        missing_recorded: bool,
        /// Index into `fields` of the key most recently yielded, if it is a
        /// declared field. Unknown keys leave this `None`.
        current_field: Option<usize>,
    }

    impl<'s, 'r, M, R: Recorder> ObservedMap<'s, 'r, M, R> {
        fn record_missing(&mut self) {
            if self.missing_recorded {
                return;
            }
            self.missing_recorded = true;
            for (index, field) in self.fields.iter().enumerate() {
                if !self.seen[index] {
                    let path = child_path(&self.path, field);
                    self.shared.record(&path, FieldState::Missing);
                }
            }
        }
    }

    impl<'de, 's, 'r, M, R> MapAccess<'de> for ObservedMap<'s, 'r, M, R>
    where
        M: MapAccess<'de>,
        R: Recorder,
    {
        type Error = M::Error;

        fn next_key_seed<K>(&mut self, seed: K) -> Result<Option<K::Value>, Self::Error>
        where
            K: DeserializeSeed<'de>,
        {
            self.current_field = None;
            let mut captured = None;
            let key = self.inner.next_key_seed(KeySeed {
                inner: seed,
                fields: self.fields,
                captured: &mut captured,
            })?;
            match key {
                Some(value) => {
                    if let Some(name) = captured {
                        if let Some(index) = self.fields.iter().position(|field| *field == name) {
                            self.seen[index] = true;
                            self.current_field = Some(index);
                        }
                    }
                    Ok(Some(value))
                }
                None => {
                    self.record_missing();
                    Ok(None)
                }
            }
        }

        fn next_value_seed<V>(&mut self, seed: V) -> Result<V::Value, Self::Error>
        where
            V: DeserializeSeed<'de>,
        {
            let (path, track) = match self.current_field {
                Some(index) => (child_path(&self.path, self.fields[index]), true),
                // Unknown key: forwarded untouched, its content is neither
                // recorded nor inspected.
                None => (self.path.clone(), false),
            };
            self.inner.next_value_seed(ValueSeed {
                inner: seed,
                shared: self.shared,
                path,
                track,
            })
        }

        fn size_hint(&self) -> Option<usize> {
            self.inner.size_hint()
        }
    }

    /// Positional (sequence) representation of a struct.
    struct ObservedStructSeq<'s, 'r, S, R: Recorder> {
        inner: S,
        shared: &'s mut Shared<'r, R>,
        path: String,
        fields: &'static [&'static str],
        index: usize,
        missing_recorded: bool,
    }

    impl<'de, 's, 'r, S, R> SeqAccess<'de> for ObservedStructSeq<'s, 'r, S, R>
    where
        S: SeqAccess<'de>,
        R: Recorder,
    {
        type Error = S::Error;

        fn next_element_seed<T>(&mut self, seed: T) -> Result<Option<T::Value>, Self::Error>
        where
            T: DeserializeSeed<'de>,
        {
            let (path, track) = match self.fields.get(self.index) {
                Some(field) => (child_path(&self.path, field), true),
                None => (self.path.clone(), false),
            };
            let element = self.inner.next_element_seed(ValueSeed {
                inner: seed,
                shared: &mut *self.shared,
                path,
                track,
            })?;
            match element {
                Some(value) => {
                    self.index += 1;
                    Ok(Some(value))
                }
                None => {
                    if !self.missing_recorded {
                        self.missing_recorded = true;
                        let start = self.index.min(self.fields.len());
                        for field in &self.fields[start..] {
                            let path = child_path(&self.path, field);
                            self.shared.record(&path, FieldState::Missing);
                        }
                    }
                    Ok(None)
                }
            }
        }

        fn size_hint(&self) -> Option<usize> {
            self.inner.size_hint()
        }
    }

    /// Seed for the value of a declared struct field. Records `Present` when
    /// the value deserialized successfully and was not already classified by
    /// `deserialize_option`.
    struct ValueSeed<'s, 'r, S, R: Recorder> {
        inner: S,
        shared: &'s mut Shared<'r, R>,
        path: String,
        track: bool,
    }

    impl<'de, 's, 'r, S, R> DeserializeSeed<'de> for ValueSeed<'s, 'r, S, R>
    where
        S: DeserializeSeed<'de>,
        R: Recorder,
    {
        type Value = S::Value;

        fn deserialize<D2>(self, deserializer: D2) -> Result<Self::Value, D2::Error>
        where
            D2: Deserializer<'de>,
        {
            let classified = Cell::new(false);
            let result = self.inner.deserialize(Observer {
                de: deserializer,
                shared: &mut *self.shared,
                path: self.path.clone(),
                classified: if self.track { Some(&classified) } else { None },
            });
            if self.track && result.is_ok() && !classified.get() {
                self.shared.record(&self.path, FieldState::Present);
            }
            result
        }
    }

    //////////////////////////////////////////////////////////////////////
    // Key capture: learn *which* declared field a key names without
    // disturbing the real key deserialization.

    struct KeySeed<'c2, S> {
        inner: S,
        fields: &'static [&'static str],
        captured: &'c2 mut Option<String>,
    }

    impl<'de, 'c2, S> DeserializeSeed<'de> for KeySeed<'c2, S>
    where
        S: DeserializeSeed<'de>,
    {
        type Value = S::Value;

        fn deserialize<D2>(self, deserializer: D2) -> Result<Self::Value, D2::Error>
        where
            D2: Deserializer<'de>,
        {
            self.inner.deserialize(KeyCapture {
                de: deserializer,
                fields: self.fields,
                captured: self.captured,
            })
        }
    }

    struct KeyCapture<'c2, D> {
        de: D,
        fields: &'static [&'static str],
        captured: &'c2 mut Option<String>,
    }

    impl<'de, 'c2, D> Deserializer<'de> for KeyCapture<'c2, D>
    where
        D: Deserializer<'de>,
    {
        type Error = D::Error;

        fn deserialize_identifier<V>(self, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            self.de.deserialize_identifier(CaptureVisitor {
                inner: visitor,
                fields: self.fields,
                captured: self.captured,
            })
        }

        fn deserialize_str<V>(self, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            self.de.deserialize_str(CaptureVisitor {
                inner: visitor,
                fields: self.fields,
                captured: self.captured,
            })
        }

        fn deserialize_string<V>(self, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            self.de.deserialize_string(CaptureVisitor {
                inner: visitor,
                fields: self.fields,
                captured: self.captured,
            })
        }

        fn deserialize_bytes<V>(self, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            self.de.deserialize_bytes(CaptureVisitor {
                inner: visitor,
                fields: self.fields,
                captured: self.captured,
            })
        }

        fn deserialize_byte_buf<V>(self, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            self.de.deserialize_byte_buf(CaptureVisitor {
                inner: visitor,
                fields: self.fields,
                captured: self.captured,
            })
        }

        fn deserialize_any<V>(self, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            self.de.deserialize_any(CaptureVisitor {
                inner: visitor,
                fields: self.fields,
                captured: self.captured,
            })
        }

        forward_to_inner! {
            deserialize_bool
            deserialize_i8 deserialize_i16 deserialize_i32 deserialize_i64 deserialize_i128
            deserialize_u8 deserialize_u16 deserialize_u32 deserialize_u64 deserialize_u128
            deserialize_f32 deserialize_f64
            deserialize_char
            deserialize_option
            deserialize_unit
            deserialize_seq
            deserialize_map
            deserialize_ignored_any
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

        fn deserialize_enum<V>(
            self,
            name: &'static str,
            variants: &'static [&'static str],
            visitor: V,
        ) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            self.de.deserialize_enum(name, variants, visitor)
        }

        fn deserialize_unit_struct<V>(
            self,
            name: &'static str,
            visitor: V,
        ) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
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
            self.de.deserialize_newtype_struct(name, visitor)
        }

        fn deserialize_tuple<V>(self, len: usize, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
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
            self.de.deserialize_tuple_struct(name, len, visitor)
        }

        fn is_human_readable(&self) -> bool {
            self.de.is_human_readable()
        }
    }

    struct CaptureVisitor<'c2, V> {
        inner: V,
        fields: &'static [&'static str],
        captured: &'c2 mut Option<String>,
    }

    impl<'c2, V> CaptureVisitor<'c2, V> {
        fn capture_index(&mut self, index: usize) {
            if let Some(field) = self.fields.get(index) {
                *self.captured = Some((*field).to_owned());
            }
        }
    }

    macro_rules! capture_visit_str {
        ($($visit:ident($ty:ty);)*) => {$(
            fn $visit<E>(self, v: $ty) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                *self.captured = Some(v.to_owned());
                self.inner.$visit(v)
            }
        )*};
    }

    macro_rules! capture_visit_bytes {
        ($($visit:ident($ty:ty);)*) => {$(
            fn $visit<E>(self, v: $ty) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                *self.captured = Some(String::from_utf8_lossy(v.as_ref()).into_owned());
                self.inner.$visit(v)
            }
        )*};
    }

    macro_rules! capture_visit_index {
        ($($visit:ident($ty:ty);)*) => {$(
            fn $visit<E>(mut self, v: $ty) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                self.capture_index(v as usize);
                self.inner.$visit(v)
            }
        )*};
    }

    macro_rules! forward_visit {
        ($($visit:ident($ty:ty);)*) => {$(
            fn $visit<E>(self, v: $ty) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                self.inner.$visit(v)
            }
        )*};
    }

    impl<'de, 'c2, V> Visitor<'de> for CaptureVisitor<'c2, V>
    where
        V: Visitor<'de>,
    {
        type Value = V::Value;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            self.inner.expecting(formatter)
        }

        capture_visit_str! {
            visit_str(&str);
            visit_borrowed_str(&'de str);
            visit_string(String);
        }

        capture_visit_bytes! {
            visit_bytes(&[u8]);
            visit_borrowed_bytes(&'de [u8]);
            visit_byte_buf(Vec<u8>);
        }

        capture_visit_index! {
            visit_u8(u8);
            visit_u16(u16);
            visit_u32(u32);
            visit_u64(u64);
        }

        forward_visit! {
            visit_bool(bool);
            visit_i8(i8);
            visit_i16(i16);
            visit_i32(i32);
            visit_i64(i64);
            visit_i128(i128);
            visit_u128(u128);
            visit_f32(f32);
            visit_f64(f64);
            visit_char(char);
        }

        fn visit_none<E>(self) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            self.inner.visit_none()
        }

        fn visit_unit<E>(self) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            self.inner.visit_unit()
        }
    }

    //////////////////////////////////////////////////////////////////////
    // Sequences of non-struct elements: keep nested structs observable
    // under indexed paths like `items.0.name`.

    struct ListVisitor<'s, 'r, V, R: Recorder> {
        inner: V,
        shared: &'s mut Shared<'r, R>,
        path: String,
    }

    impl<'de, 's, 'r, V, R> Visitor<'de> for ListVisitor<'s, 'r, V, R>
    where
        V: Visitor<'de>,
        R: Recorder,
    {
        type Value = V::Value;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            self.inner.expecting(formatter)
        }

        fn visit_seq<A>(self, seq: A) -> Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            self.inner.visit_seq(ListSeq {
                inner: seq,
                shared: self.shared,
                path: self.path,
                index: 0,
            })
        }
    }

    struct ListSeq<'s, 'r, S, R: Recorder> {
        inner: S,
        shared: &'s mut Shared<'r, R>,
        path: String,
        index: usize,
    }

    impl<'de, 's, 'r, S, R> SeqAccess<'de> for ListSeq<'s, 'r, S, R>
    where
        S: SeqAccess<'de>,
        R: Recorder,
    {
        type Error = S::Error;

        fn next_element_seed<T>(&mut self, seed: T) -> Result<Option<T::Value>, Self::Error>
        where
            T: DeserializeSeed<'de>,
        {
            let path = child_path(&self.path, &self.index.to_string());
            let element = self.inner.next_element_seed(ListElementSeed {
                inner: seed,
                shared: &mut *self.shared,
                path,
            })?;
            if element.is_some() {
                self.index += 1;
            }
            Ok(element)
        }

        fn size_hint(&self) -> Option<usize> {
            self.inner.size_hint()
        }
    }

    struct ListElementSeed<'s, 'r, S, R: Recorder> {
        inner: S,
        shared: &'s mut Shared<'r, R>,
        path: String,
    }

    impl<'de, 's, 'r, S, R> DeserializeSeed<'de> for ListElementSeed<'s, 'r, S, R>
    where
        S: DeserializeSeed<'de>,
        R: Recorder,
    {
        type Value = S::Value;

        fn deserialize<D2>(self, deserializer: D2) -> Result<Self::Value, D2::Error>
        where
            D2: Deserializer<'de>,
        {
            self.inner.deserialize(Observer {
                de: deserializer,
                shared: self.shared,
                path: self.path,
                classified: None,
            })
        }
    }
}


//////////////////////////////////////////////////////////////////////////
// A small custom Deserializer used as the test format: an in-memory
// JSON-like value tree.

mod mini {
    use serde::de::value::{Error, StrDeserializer};
    use serde::de::{
        self, DeserializeSeed, Deserializer, MapAccess, SeqAccess, Unexpected, Visitor,
    };

    #[derive(Debug)]
    pub enum Data {
        Null,
        Some(Box<Data>),
        I64(i64),
        U64(u64),
        Str(&'static str),
        Seq(Vec<Data>),
        Struct(&'static str, Vec<(&'static str, Data)>),
    }

    impl Data {
        pub fn some(data: Data) -> Data {
            Data::Some(Box::new(data))
        }

        pub fn strukt(name: &'static str, fields: Vec<(&'static str, Data)>) -> Data {
            Data::Struct(name, fields)
        }
    }

    fn unexpected(data: &Data) -> Unexpected<'_> {
        match data {
            Data::Null => Unexpected::Unit,
            Data::Some(_) => Unexpected::Other("option"),
            Data::I64(v) => Unexpected::Signed(*v),
            Data::U64(v) => Unexpected::Unsigned(*v),
            Data::Str(s) => Unexpected::Str(s),
            Data::Seq(_) => Unexpected::Seq,
            Data::Struct(..) => Unexpected::Map,
        }
    }

    macro_rules! forward_to_any {
        ($($method:ident)*) => {$(
            fn $method<V>(self, visitor: V) -> Result<V::Value, Self::Error>
            where
                V: Visitor<'de>,
            {
                self.deserialize_any(visitor)
            }
        )*};
    }

    impl<'de> Deserializer<'de> for &'de Data {
        type Error = Error;

        fn deserialize_any<V>(self, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            match self {
                Data::Null => visitor.visit_unit(),
                Data::Some(inner) => visitor.visit_some(&**inner),
                Data::I64(v) => visitor.visit_i64(*v),
                Data::U64(v) => visitor.visit_u64(*v),
                Data::Str(s) => visitor.visit_borrowed_str(s),
                Data::Seq(items) => visitor.visit_seq(Items {
                    iter: items.iter(),
                }),
                Data::Struct(_, fields) => visitor.visit_map(Pairs {
                    iter: fields.iter(),
                    value: None,
                }),
            }
        }

        fn deserialize_option<V>(self, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            match self {
                Data::Null => visitor.visit_none(),
                Data::Some(inner) => visitor.visit_some(&**inner),
                other => visitor.visit_some(other),
            }
        }

        fn deserialize_struct<V>(
            self,
            _name: &'static str,
            _fields: &'static [&'static str],
            visitor: V,
        ) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            match self {
                Data::Struct(_, fields) => visitor.visit_map(Pairs {
                    iter: fields.iter(),
                    value: None,
                }),
                other => Err(de::Error::invalid_type(unexpected(other), &visitor)),
            }
        }

        forward_to_any! {
            deserialize_bool
            deserialize_i8 deserialize_i16 deserialize_i32 deserialize_i64 deserialize_i128
            deserialize_u8 deserialize_u16 deserialize_u32 deserialize_u64 deserialize_u128
            deserialize_f32 deserialize_f64
            deserialize_char deserialize_str deserialize_string
            deserialize_bytes deserialize_byte_buf
            deserialize_unit
            deserialize_seq
            deserialize_map
            deserialize_identifier deserialize_ignored_any
        }

        fn deserialize_unit_struct<V>(
            self,
            _name: &'static str,
            visitor: V,
        ) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            self.deserialize_any(visitor)
        }

        fn deserialize_newtype_struct<V>(
            self,
            _name: &'static str,
            visitor: V,
        ) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            self.deserialize_any(visitor)
        }

        fn deserialize_tuple<V>(
            self,
            _len: usize,
            visitor: V,
        ) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            self.deserialize_any(visitor)
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
            self.deserialize_any(visitor)
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
            self.deserialize_any(visitor)
        }

        fn is_human_readable(&self) -> bool {
            true
        }
    }

    struct Pairs<'de> {
        iter: std::slice::Iter<'de, (&'static str, Data)>,
        value: Option<&'de Data>,
    }

    impl<'de> MapAccess<'de> for Pairs<'de> {
        type Error = Error;

        fn next_key_seed<K>(&mut self, seed: K) -> Result<Option<K::Value>, Self::Error>
        where
            K: DeserializeSeed<'de>,
        {
            match self.iter.next() {
                Some((key, value)) => {
                    self.value = Some(value);
                    seed.deserialize(StrDeserializer::new(key)).map(Some)
                }
                None => Ok(None),
            }
        }

        fn next_value_seed<V>(&mut self, seed: V) -> Result<V::Value, Self::Error>
        where
            V: DeserializeSeed<'de>,
        {
            let value = self.value.take().expect("next_value before next_key");
            seed.deserialize(value)
        }

        fn size_hint(&self) -> Option<usize> {
            Some(self.iter.len())
        }
    }

    struct Items<'de> {
        iter: std::slice::Iter<'de, Data>,
    }

    impl<'de> SeqAccess<'de> for Items<'de> {
        type Error = Error;

        fn next_element_seed<T>(&mut self, seed: T) -> Result<Option<T::Value>, Self::Error>
        where
            T: DeserializeSeed<'de>,
        {
            match self.iter.next() {
                Some(item) => seed.deserialize(item).map(Some),
                None => Ok(None),
            }
        }

        fn size_hint(&self) -> Option<usize> {
            Some(self.iter.len())
        }
    }
}

use mini::Data;
use observe::{observe_missing_fields, FieldState, Observation, ObserveError};

//////////////////////////////////////////////////////////////////////////

#[derive(PartialEq, Debug, DeriveDeserialize)]
struct Config {
    host: Option<String>,
    port: Option<u16>,
}

#[derive(PartialEq, Debug, DeriveDeserialize)]
struct Inner {
    name: Option<String>,
}

#[derive(PartialEq, Debug, DeriveDeserialize)]
struct Outer {
    inner: Inner,
    opt: Option<i32>,
}

#[derive(PartialEq, Debug, DeriveDeserialize)]
struct Batch {
    items: Vec<Inner>,
}

#[derive(PartialEq, Debug, DeriveDeserialize)]
struct Required {
    id: i32,
    opt: Option<i32>,
}

fn observation(path: &str, state: FieldState) -> Observation {
    Observation {
        path: path.to_owned(),
        state,
    }
}

/// The three canonical objects: key absent, explicit null, real value.
#[test]
fn missing_null_and_value_are_distinguished() {
    // 1. Both keys absent.
    let data = Data::strukt("Config", vec![]);
    let mut log = Vec::new();
    let value: Config = observe_missing_fields(&data, &mut log).unwrap();
    assert_eq!(value, Config {
        host: None,
        port: None
    });
    // Same value the plain interface produces.
    let plain: Config = Deserialize::deserialize(&data).unwrap();
    assert_eq!(value, plain);
    assert_eq!(
        log,
        vec![
            observation("host", FieldState::Missing),
            observation("port", FieldState::Missing),
        ]
    );

    // 2. Both keys present with explicit null. The derived result is
    // identical to case 1, but the observations are different.
    let data = Data::strukt(
        "Config",
        vec![("host", Data::Null), ("port", Data::Null)],
    );
    let mut log = Vec::new();
    let value: Config = observe_missing_fields(&data, &mut log).unwrap();
    assert_eq!(value, Config {
        host: None,
        port: None
    });
    assert_eq!(
        log,
        vec![
            observation("host", FieldState::Null),
            observation("port", FieldState::Null),
        ]
    );

    // 3. Both keys present with real values.
    let data = Data::strukt(
        "Config",
        vec![
            ("host", Data::some(Data::Str("example.com"))),
            ("port", Data::some(Data::U64(8080))),
        ],
    );
    let mut log = Vec::new();
    let value: Config = observe_missing_fields(&data, &mut log).unwrap();
    assert_eq!(value, Config {
        host: Some("example.com".to_owned()),
        port: Some(8080),
    });
    assert_eq!(
        log,
        vec![
            observation("host", FieldState::Present),
            observation("port", FieldState::Present),
        ]
    );
}

#[test]
fn nested_struct_paths_are_preserved() {
    let data = Data::strukt(
        "Outer",
        vec![(
            "inner",
            Data::strukt("Inner", vec![("name", Data::Null)]),
        )],
    );
    let mut log = Vec::new();
    let value: Outer = observe_missing_fields(&data, &mut log).unwrap();
    assert_eq!(value, Outer {
        inner: Inner { name: None },
        opt: None,
    });
    assert_eq!(
        log,
        vec![
            observation("inner.name", FieldState::Null),
            observation("inner", FieldState::Present),
            observation("opt", FieldState::Missing),
        ]
    );
}

#[test]
fn nested_paths_inside_sequences_are_indexed() {
    let data = Data::strukt(
        "Batch",
        vec![(
            "items",
            Data::Seq(vec![Data::strukt("Inner", vec![])]),
        )],
    );
    let mut log = Vec::new();
    let value: Batch = observe_missing_fields(&data, &mut log).unwrap();
    assert_eq!(value, Batch {
        items: vec![Inner { name: None }],
    });
    assert_eq!(
        log,
        vec![
            observation("items.0.name", FieldState::Missing),
            observation("items", FieldState::Present),
        ]
    );
}

#[test]
fn unknown_fields_are_not_recorded_or_inspected() {
    // `extra` carries a value and `weird` carries an explicit null; neither
    // is a declared field, so neither may appear in the log.
    let data = Data::strukt(
        "Config",
        vec![
            ("host", Data::some(Data::Str("example.com"))),
            ("extra", Data::I64(99)),
            ("weird", Data::Null),
        ],
    );
    let mut log = Vec::new();
    let value: Config = observe_missing_fields(&data, &mut log).unwrap();
    assert_eq!(value, Config {
        host: Some("example.com".to_owned()),
        port: None,
    });
    assert_eq!(
        log,
        vec![
            observation("host", FieldState::Present),
            observation("port", FieldState::Missing),
        ]
    );
}

#[test]
fn real_type_errors_are_returned_unchanged() {
    let data = Data::strukt(
        "Config",
        vec![("port", Data::some(Data::Str("not-a-number")))],
    );
    let mut log = Vec::new();
    let err = observe_missing_fields::<Config, _, _>(&data, &mut log).unwrap_err();
    let plain_err = Config::deserialize(&data).unwrap_err();
    match err {
        ObserveError::Deserialize(err) => {
            // The genuine type error from the derived impl, byte for byte.
            assert_eq!(err.to_string(), plain_err.to_string());
            assert!(err.to_string().contains("invalid type"));
        }
        ObserveError::Recorder(err) => panic!("unexpected recorder error: {err}"),
    }
}

#[test]
fn missing_required_field_still_errors_and_is_recorded() {
    let data = Data::strukt("Required", vec![]);
    let mut log = Vec::new();
    let err = observe_missing_fields::<Required, _, _>(&data, &mut log).unwrap_err();
    match err {
        ObserveError::Deserialize(err) => {
            assert_eq!(err.to_string(), "missing field `id`");
        }
        ObserveError::Recorder(err) => panic!("unexpected recorder error: {err}"),
    }
    // The observation explaining the failure was still recorded.
    assert_eq!(
        log,
        vec![
            observation("id", FieldState::Missing),
            observation("opt", FieldState::Missing),
        ]
    );
}

#[test]
fn recorder_failure_never_swallows_deserialization_error() {
    let mut fail = |_path: &str, _state: FieldState| -> Result<(), &'static str> {
        Err("recorder is full")
    };

    // Deserialization succeeds, recorder fails: recorder error surfaces.
    let data = Data::strukt("Config", vec![]);
    let err = observe_missing_fields::<Config, _, _>(&data, &mut fail).unwrap_err();
    match err {
        ObserveError::Recorder("recorder is full") => {}
        other => panic!("expected recorder error, got {other:?}"),
    }

    // Both fail: the original deserialization error wins.
    let data = Data::strukt(
        "Config",
        vec![("port", Data::some(Data::Str("not-a-number")))],
    );
    let err = observe_missing_fields::<Config, _, _>(&data, &mut fail).unwrap_err();
    match err {
        ObserveError::Deserialize(err) => {
            assert!(err.to_string().contains("invalid type"));
        }
        ObserveError::Recorder(err) => panic!("deserialization error was swallowed: {err}"),
    }
}

#[test]
fn disabled_observation_matches_the_plain_interface() {
    // With the `()` recorder the option is off: results and errors are
    // exactly those of `Deserialize::deserialize`.
    let data = Data::strukt(
        "Config",
        vec![
            ("host", Data::some(Data::Str("example.com"))),
            ("port", Data::Null),
        ],
    );
    let observed: Config = observe_missing_fields(&data, &mut ()).unwrap();
    let plain: Config = Deserialize::deserialize(&data).unwrap();
    assert_eq!(observed, plain);

    let data = Data::strukt(
        "Config",
        vec![("port", Data::some(Data::Str("not-a-number")))],
    );
    let observed_err = observe_missing_fields::<Config, _, _>(&data, &mut ()).unwrap_err();
    let plain_err = Config::deserialize(&data).unwrap_err();
    match observed_err {
        ObserveError::Deserialize(err) => assert_eq!(err.to_string(), plain_err.to_string()),
        ObserveError::Recorder(err) => panic!("unexpected recorder error: {err}"),
    }
}

/// Demonstration with a small temporary-directory fixture: the observation
/// log of a real run is written to a report file.
#[test]
fn demo_report_in_temp_dir() {
    let data = Data::strukt(
        "Outer",
        vec![
            ("inner", Data::strukt("Inner", vec![])),
            ("opt", Data::Null),
        ],
    );
    let mut log = Vec::new();
    let value: Outer = observe_missing_fields(&data, &mut log).unwrap();

    let mut report = format!("result: {value:?}\n");
    for obs in &log {
        report.push_str(&format!("{} -> {:?}\n", obs.path, obs.state));
    }

    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"));
    let file = dir.join("observations.log");
    std::fs::write(&file, &report).unwrap();
    let read_back = std::fs::read_to_string(&file).unwrap();
    assert_eq!(read_back, report);
    assert!(read_back.contains("inner.name -> Missing"));
    assert!(read_back.contains("opt -> Null"));
    println!("observation report written to {}", file.display());
    print!("{report}");
}
