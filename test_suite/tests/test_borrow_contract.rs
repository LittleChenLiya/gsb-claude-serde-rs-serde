//! Runtime acceptance tests for the explicit Deserialize borrow contract.
//!
//! Three structs are exercised:
//!
//! * `Borrowed` -- a Cow marked `#[serde(borrow)]` really borrows from the
//!   deserializer input, alongside implicitly borrowed `&str` / `&[u8]`;
//! * `Owned` -- a fully owned struct never claims to be zero-copy, even when
//!   the input hands out borrowed strings;
//! * `Outer` -- a `#[serde(flatten)]` member keeps its borrow source: a Cow
//!   inside the flattened member still borrows from the input.
//!
//! The compile-time half of the contract (error positions, conflicting
//! attributes) is covered by `tests/ui/borrow-contract/*` through
//! `tests/ui_borrow_contract.rs`, and the queryable source map by
//! `serde_derive_internals/tests/borrow_contract.rs`.

#![allow(clippy::derive_partial_eq_without_eq)]

use serde::de::value::{BorrowedStrDeserializer, MapDeserializer};
use serde::de::{Deserialize, IntoDeserializer};
use serde_derive::Deserialize;
use serde_test::{assert_de_tokens, Token};
use std::borrow::Cow;

// Struct 1 of 3: truly borrowed fields.
#[derive(Deserialize, Debug, PartialEq)]
struct Borrowed<'a> {
    #[serde(borrow)]
    cow: Cow<'a, str>,
    s: &'a str,
    opt: Option<&'a [u8]>,
}

// Struct 2 of 3: fully owned; must not claim zero-copy.
#[derive(Deserialize, Debug, PartialEq)]
struct Owned {
    cow: Cow<'static, str>,
    s: String,
}

// Struct 3 of 3: a flattened member keeps its borrow source.
#[derive(Deserialize, Debug, PartialEq)]
struct Inner<'a> {
    #[serde(borrow)]
    cow: Cow<'a, str>,
    s: &'a str,
}

#[derive(Deserialize, Debug, PartialEq)]
struct Outer<'a> {
    // The flatten member declares that it sources borrowed data, so the
    // `'de: 'a` bound is propagated to Outer's Deserialize impl.
    #[serde(flatten, borrow)]
    inner: Inner<'a>,
}

struct BorrowedStr(&'static str);

impl<'de> IntoDeserializer<'de> for BorrowedStr {
    type Deserializer = BorrowedStrDeserializer<'de, serde::de::value::Error>;

    fn into_deserializer(self) -> Self::Deserializer {
        BorrowedStrDeserializer::new(self.0)
    }
}

// A Cow marked #[serde(borrow)] borrows from the deserializer input; the
// implicitly borrowed reference fields borrow too.
#[test]
fn borrowed_cow_borrows_from_input() {
    let tokens = [
        Token::Struct {
            name: "Borrowed",
            len: 3,
        },
        Token::Str("cow"),
        Token::BorrowedStr("borrowed"),
        Token::Str("s"),
        Token::BorrowedStr("borrowed"),
        Token::Str("opt"),
        Token::Some,
        Token::BorrowedBytes(b"bytes"),
        Token::StructEnd,
    ];
    assert_de_tokens(
        &Borrowed {
            cow: Cow::Borrowed("borrowed"),
            s: "borrowed",
            opt: Some(&b"bytes"[..]),
        },
        &tokens,
    );

    // Token equality cannot distinguish Cow::Borrowed from Cow::Owned, so
    // deserialize once more and inspect the Cow directly.
    #[derive(Deserialize)]
    struct JustCow<'a> {
        #[serde(borrow)]
        cow: Cow<'a, str>,
    }

    let de = MapDeserializer::new(IntoIterator::into_iter([(
        "cow",
        BorrowedStr("borrowed"),
    )]));
    let value = JustCow::deserialize(de).unwrap();
    assert!(matches!(value.cow, Cow::Borrowed("borrowed")));
}

// The same fields owned: no false zero-copy claim, even when the input
// hands out borrowed strings.
#[test]
fn owned_fields_do_not_borrow() {
    let de = MapDeserializer::new(IntoIterator::into_iter([
        ("cow", BorrowedStr("owned")),
        ("s", BorrowedStr("owned")),
    ]));
    let value = Owned::deserialize(de).unwrap();

    assert!(matches!(value.cow, Cow::Owned(ref s) if s == "owned"));
    assert_eq!(value.s, "owned");
}

// A borrow that passes through a #[serde(flatten)] member still comes from
// the deserializer input; the source is not lost.
#[test]
fn flatten_member_keeps_borrow_source() {
    let de = MapDeserializer::new(IntoIterator::into_iter([
        ("cow", BorrowedStr("borrowed")),
        ("s", BorrowedStr("borrowed")),
    ]));
    let value = Outer::deserialize(de).unwrap();

    assert!(matches!(value.inner.cow, Cow::Borrowed("borrowed")));
    assert_eq!(value.inner.s, "borrowed");
}
