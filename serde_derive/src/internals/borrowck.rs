//! Explicit borrow-contract analysis for `#[derive(Deserialize)]`.
//!
//! Every field of a container is classified structurally -- from its *type*
//! and its *attributes*, never from its name -- into one of three categories:
//!
//! * **truly borrowed fields** -- the generated `Deserialize` impl receives a
//!   `'de: 'a` bound for each borrowed lifetime and the field is deserialized
//!   zero-copy from the deserializer's input;
//! * **owned fields** -- the field is fully owned and must not claim to be
//!   zero-copy;
//! * **unsupported combinations** -- attribute combinations through which no
//!   borrow contract can be honored. These are rejected with an error before
//!   any code is generated.
//!
//! The classification is exposed through [`field_contracts`] so that the
//! source of every borrowed field -- including fields reached through a
//! `#[serde(flatten)]` member -- can be queried.

// The query half of this module is a public API of serde_derive_internals;
// serde_derive itself only uses the check half.
#![allow(dead_code)]

use crate::internals::ast::{Container, Field};
use crate::internals::attr::{is_slice_u8, is_str};
use crate::internals::{ungroup, Ctxt};
use proc_macro2::Ident;
use quote::ToTokens;
use std::collections::BTreeSet;
use syn::ext::IdentExt;

/// Where the bytes of a deserialized field come from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BorrowSource {
    /// The field is fully owned. The deserializer allocates or copies the
    /// data; nothing is borrowed from the input.
    Owned,
    /// The field borrows from the deserializer's input. The generated impl
    /// requires the input to outlive each of the listed lifetimes.
    Borrowed { lifetimes: BTreeSet<String> },
    /// The field is a `#[serde(flatten)]` member. Borrowed data inside it is
    /// sourced through the flattened member's own fields; the member is
    /// reported here so that the source of a borrow is never lost when it
    /// passes through a flattened member.
    Flatten { lifetimes: BTreeSet<String> },
}

/// The borrow contract of a single field of a `Deserialize` container.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldContract {
    /// Field name, or the tuple index as a string for tuple fields.
    pub member: String,
    /// Whether the field carries an explicit `#[serde(borrow)]` attribute, as
    /// opposed to lifetimes borrowed implicitly (`&str`, `&[u8]`).
    pub explicit_borrow: bool,
    /// Where the field's bytes come from.
    pub source: BorrowSource,
}

impl FieldContract {
    /// Whether the generated `Deserialize` impl deserializes this field
    /// zero-copy from the deserializer's input.
    ///
    /// Owned fields return `false`: an owned type never claims to be
    /// zero-copy.
    pub fn is_zero_copy(&self) -> bool {
        matches!(self.source, BorrowSource::Borrowed { .. })
    }
}

/// Query the borrow contract of every field of `cont`, in declaration order.
///
/// `#[serde(flatten)]` members are reported with [`BorrowSource::Flatten`]
/// keyed by the member's own name, so the source of a borrow remains
/// queryable even when it passes through a flattened member.
pub fn field_contracts(cont: &Container) -> Vec<FieldContract> {
    cont.data
        .all_fields()
        .map(|field| {
            let lifetimes = field
                .attrs
                .borrowed_lifetimes()
                .iter()
                .map(|lt| lt.ident.to_string())
                .collect::<BTreeSet<_>>();
            let source = if field.attrs.flatten() {
                BorrowSource::Flatten { lifetimes }
            } else if lifetimes.is_empty() {
                BorrowSource::Owned
            } else {
                BorrowSource::Borrowed { lifetimes }
            };
            FieldContract {
                member: member_name(field),
                explicit_borrow: field.attrs.has_explicit_borrow(),
                source,
            }
        })
        .collect()
}

/// Cross-cutting borrow-contract checks, run before code generation so that
/// conflicting attributes are rejected before anything is emitted.
///
/// Only fields with an explicit `#[serde(borrow)]` are checked; implicitly
/// borrowed `&str` / `&[u8]` fields and owned fields need no contract.
pub fn check_borrow_contracts(cx: &Ctxt, cont: &Container) {
    for field in cont.data.all_fields() {
        if !field.attrs.has_explicit_borrow() {
            continue;
        }
        check_not_skipped(cx, field);
        check_cow_borrows(cx, cont, field);
    }
}

// A field that is never deserialized cannot borrow anything. Combining
// #[serde(borrow)] with #[serde(skip_deserializing)] is a conflicting
// attribute pair and is rejected before compile.
fn check_not_skipped(cx: &Ctxt, field: &Field) {
    if field.attrs.skip_deserializing() {
        cx.error_spanned_by(
            field.original,
            "#[serde(borrow)] cannot be combined with #[serde(skip_deserializing)]: a skipped field is never deserialized and cannot borrow",
        );
    }
}

// Only Cow<'_, str> and Cow<'_, [u8]> can borrow from the deserializer (via
// serde's private borrow_cow_str / borrow_cow_bytes helpers). A Cow over any
// other element type always owns its data, so marking it #[serde(borrow)]
// would be a false zero-copy claim. This is checked structurally through
// nested Option / Vec / tuples / references; a Cow whose element mentions a
// generic type parameter of the container is left alone because the
// parameter could be instantiated with str or [u8].
fn check_cow_borrows(cx: &Ctxt, cont: &Container, field: &Field) {
    let params = cont
        .generics
        .type_params()
        .map(|param| &param.ident)
        .collect::<BTreeSet<_>>();
    if let Some(cow) = find_unborrowable_cow(field.ty, &params) {
        let msg = format!(
            "field `{}` contains {} which cannot borrow from the deserializer; #[serde(borrow)] only makes Cow<'_, str> and Cow<'_, [u8]> zero-copy",
            member_name(field),
            compact(cow.into_token_stream().to_string()),
        );
        cx.error_spanned_by(field.original, msg);
    }
}

// Render a type without the spaces that TokenStream's Display inserts
// around generic brackets: "Cow < 'a, String >" becomes "Cow<'a, String>".
fn compact(tokens: String) -> String {
    tokens.replace(" < ", "<").replace(" >", ">")
}

// Find the first path segment of the form Cow<'_, T> in which T is a
// concrete type that can never be borrowed (not str, not [u8], and not
// mentioning a generic type parameter). Nested Cow inside Option, Vec,
// tuples, arrays and references is found by walking the whole type.
fn find_unborrowable_cow<'t>(
    ty: &'t syn::Type,
    params: &BTreeSet<&Ident>,
) -> Option<&'t syn::PathSegment> {
    match ungroup(ty) {
        syn::Type::Path(ty) => {
            if let Some(qself) = &ty.qself {
                if let Some(bad) = find_unborrowable_cow(&qself.ty, params) {
                    return Some(bad);
                }
            }
            for seg in &ty.path.segments {
                if let syn::PathArguments::AngleBracketed(bracketed) = &seg.arguments {
                    if seg.ident == "Cow" && bracketed.args.len() == 2 {
                        if let (
                            syn::GenericArgument::Lifetime(_),
                            syn::GenericArgument::Type(elem),
                        ) = (&bracketed.args[0], &bracketed.args[1])
                        {
                            if !is_str(elem)
                                && !is_slice_u8(elem)
                                && !mentions_type_param(elem, params)
                            {
                                return Some(seg);
                            }
                        }
                    }
                    for arg in &bracketed.args {
                        if let syn::GenericArgument::Type(arg) = arg {
                            if let Some(bad) = find_unborrowable_cow(arg, params) {
                                return Some(bad);
                            }
                        }
                    }
                }
            }
            None
        }
        syn::Type::Reference(ty) => find_unborrowable_cow(&ty.elem, params),
        syn::Type::Slice(ty) => find_unborrowable_cow(&ty.elem, params),
        syn::Type::Array(ty) => find_unborrowable_cow(&ty.elem, params),
        syn::Type::Ptr(ty) => find_unborrowable_cow(&ty.elem, params),
        syn::Type::Paren(ty) => find_unborrowable_cow(&ty.elem, params),
        syn::Type::Tuple(ty) => ty
            .elems
            .iter()
            .find_map(|elem| find_unborrowable_cow(elem, params)),
        _ => None,
    }
}

// Whether a type mentions any generic type parameter of the container, in
// which case no structural statement about borrowability can be made.
fn mentions_type_param(ty: &syn::Type, params: &BTreeSet<&Ident>) -> bool {
    if params.is_empty() {
        return false;
    }
    match ungroup(ty) {
        syn::Type::Path(ty) => {
            if let Some(qself) = &ty.qself {
                if mentions_type_param(&qself.ty, params) {
                    return true;
                }
            }
            for seg in &ty.path.segments {
                if ty.qself.is_none()
                    && ty.path.segments.len() == 1
                    && seg.arguments.is_none()
                    && params.contains(&seg.ident)
                {
                    return true;
                }
                if let syn::PathArguments::AngleBracketed(bracketed) = &seg.arguments {
                    for arg in &bracketed.args {
                        if let syn::GenericArgument::Type(arg) = arg {
                            if mentions_type_param(arg, params) {
                                return true;
                            }
                        }
                    }
                }
            }
            false
        }
        syn::Type::Reference(ty) => mentions_type_param(&ty.elem, params),
        syn::Type::Slice(ty) => mentions_type_param(&ty.elem, params),
        syn::Type::Array(ty) => mentions_type_param(&ty.elem, params),
        syn::Type::Ptr(ty) => mentions_type_param(&ty.elem, params),
        syn::Type::Paren(ty) => mentions_type_param(&ty.elem, params),
        syn::Type::Tuple(ty) => ty
            .elems
            .iter()
            .any(|elem| mentions_type_param(elem, params)),
        _ => false,
    }
}

fn member_name(field: &Field) -> String {
    match &field.member {
        syn::Member::Named(ident) => ident.unraw().to_string(),
        syn::Member::Unnamed(index) => index.index.to_string(),
    }
}
