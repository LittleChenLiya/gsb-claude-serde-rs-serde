//! Three-struct example of the explicit `Deserialize` borrow contract.
//!
//! The derive decides from the *structure of each field's type* and the
//! `#[serde(borrow ...)]` attributes actually written on it — never from
//! field names — whether a field borrows from the input:
//!
//! 1. [`BorrowedRecord`] genuinely borrows: `Cow<'de, str>` with
//!    `#[serde(borrow)]` and `Option<&'de str>` (implicitly borrowed).
//! 2. [`OwnedRecord`] is owned: it declares no lifetimes and makes no
//!    zero-copy claim. Writing `#[serde(borrow)]` on it is a compile error.
//! 3. [`Envelope`] flattens a borrowing member: `#[serde(flatten)]` keeps the
//!    member's borrow source, which stays queryable and points into the
//!    outer input.

use serde::Deserialize;
use std::borrow::Cow;

/// Struct 1: genuinely borrowing fields.
#[derive(Debug, Deserialize)]
pub struct BorrowedRecord<'a> {
    /// Borrows from the input when the format supports it.
    #[serde(borrow)]
    pub name: Cow<'a, str>,
    /// References borrow implicitly, also through `Option`.
    pub alias: Option<&'a str>,
}

/// Struct 2: owned fields, no zero-copy claim.
#[derive(Debug, Deserialize)]
pub struct OwnedRecord {
    /// Always owned. `#[serde(borrow)]` here would be rejected:
    /// an owned type cannot pretend to be zero-copy.
    pub name: String,
}

/// Struct 3: a flattened member whose borrow source is preserved.
#[derive(Debug, Deserialize)]
pub struct Envelope<'a> {
    pub id: u32,
    /// The flattened member still borrows from the outer input; the
    /// `#[serde(borrow)]` declaration is what ties `'a` to the input.
    #[serde(flatten)]
    #[serde(borrow)]
    pub record: BorrowedRecord<'a>,
}
