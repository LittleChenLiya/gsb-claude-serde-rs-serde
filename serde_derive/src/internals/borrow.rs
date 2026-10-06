//! Explicit borrow-contract analysis for `#[derive(Deserialize)]`.
//!
//! The borrow contract of a field is decided by the *structure of its type*
//! together with the `#[serde(borrow ...)]` attributes actually written on
//! it, never by guessing from the field's name:
//!
//! - genuinely borrowing fields: `&'a str`, `Cow<'a, str>`, `Option<&'a str>`
//! - owned fields: `String`, `Vec<u8>`, ... which must not claim zero-copy
//! - unsupported combinations, which are rejected before any code is
//!   generated instead of silently deserializing owned data
//!
//! The contract of every field, including members reached through
//! `#[serde(flatten)]`, can be inspected with [`container_contract`].

use crate::internals::ast::{Container, Field};
use crate::internals::{attr, ungroup, Ctxt, Derive};
use quote::ToTokens;
use std::collections::BTreeSet;

/// Where the data of a deserialized field comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BorrowSource {
    /// Borrows directly from the deserializer input (zero-copy).
    Input,
    /// Owned by the deserialized value. Cannot borrow from the input, so any
    /// zero-copy claim on this field would be false.
    Owned,
    /// Not decidable from the type's structure alone (generic type
    /// parameters, type aliases, ...). The field's own `Deserialize`
    /// implementation decides where the data comes from.
    Opaque,
}

/// The borrow contract of a single field.
#[derive(Clone)]
pub struct FieldContract {
    /// Field name (or tuple index) as written in the struct.
    pub member: String,
    /// Where the field's data comes from, classified by type structure.
    pub source: BorrowSource,
    /// Lifetimes this field borrows from the deserializer input, as declared
    /// by `#[serde(borrow ...)]` or implied by `&str` / `&[u8]` field types.
    pub lifetimes: BTreeSet<syn::Lifetime>,
    /// Whether this field is reached through `#[serde(flatten)]`. A flattened
    /// member keeps its borrow source: the lifetimes listed here are borrowed
    /// from the outer input, not from an intermediate owned buffer.
    pub via_flatten: bool,
}

/// Classify a field type by its structure, without looking at any names.
///
/// Nested `Cow`, `Option` and references are resolved recursively, so
/// `Option<Cow<'a, str>>` and `Option<&'a str>` classify the same as their
/// inner type.
pub fn classify(ty: &syn::Type) -> BorrowSource {
    let ty = ungroup(ty);

    // &'a str, &'a [u8], &'a T
    if let syn::Type::Reference(reference) = ty {
        if reference.mutability.is_none() {
            return BorrowSource::Input;
        }
    }

    // Cow<'a, str> and Cow<'a, [u8]> borrow with #[serde(borrow)].
    if attr::is_cow(ty, attr::is_str) || attr::is_cow(ty, attr::is_slice_u8) {
        return BorrowSource::Input;
    }

    // Option<T> borrows exactly when T does.
    if let Some(inner) = option_inner(ty) {
        return classify(inner);
    }

    // A type without lifetimes cannot borrow from the input.
    let mut lifetimes = BTreeSet::new();
    attr::collect_lifetimes(ty, &mut lifetimes);
    if lifetimes.is_empty() {
        BorrowSource::Owned
    } else {
        BorrowSource::Opaque
    }
}

/// The borrow contract of one field.
pub fn field_contract(field: &Field) -> FieldContract {
    FieldContract {
        member: member_name(&field.member),
        source: classify(field.ty),
        lifetimes: field.attrs.borrowed_lifetimes().clone(),
        via_flatten: field.attrs.flatten(),
    }
}

/// The borrow contract of every field of a container, including members
/// reached through `#[serde(flatten)]`.
pub fn container_contract(cont: &Container) -> Vec<FieldContract> {
    cont.data.all_fields().map(field_contract).collect()
}

/// Cross-checks that enforce the explicit borrow contract before any code is
/// generated. Runs for `#[derive(Deserialize)]` only; the generated code of
/// containers that pass these checks is unchanged.
pub fn check_borrow_contract(cx: &Ctxt, cont: &Container, derive: Derive) {
    if let Derive::Serialize = derive {
        return;
    }
    let contracts = container_contract(cont);
    for (field, contract) in cont.data.all_fields().zip(&contracts) {
        check_conflicting_attributes(cx, field);
        check_nested_cow(cx, field, contract);
        check_flatten_borrow_source(cx, field, contract);
    }
}

/// `#[serde(borrow)]` on a field that is never deserialized is a conflict:
/// there is nothing to borrow.
fn check_conflicting_attributes(cx: &Ctxt, field: &Field) {
    if field.attrs.explicit_borrow() && field.attrs.skip_deserializing() {
        cx.error_spanned_by(
            field.original,
            "#[serde(borrow)] cannot be combined with #[serde(skip_deserializing)]; \
             a skipped field is never deserialized so there is nothing to borrow",
        );
    }
}

/// A `Cow` borrows from the input only when it is the field's top-level type
/// and the derive substitutes `borrow_cow_str` / `borrow_cow_bytes`. A `Cow`
/// nested inside `Option`, `Vec`, a map or a tuple is deserialized through
/// the generic `Deserialize` impl of `Cow`, which always produces owned
/// data. Reject the combination instead of silently dropping zero-copy.
fn check_nested_cow(cx: &Ctxt, field: &Field, contract: &FieldContract) {
    if !field.attrs.explicit_borrow() || field.attrs.deserialize_with().is_some() {
        return;
    }
    // An owned type contains no Cow that could pretend to borrow.
    if contract.source == BorrowSource::Owned {
        return;
    }
    if contains_nested_cow(field.ty, true) {
        let msg = format!(
            "#[serde(borrow)] cannot borrow through `{}`; a Cow only borrows from the input \
             when it is the field's top-level type, a nested Cow is always deserialized as \
             owned (not zero-copy)",
            field.ty.to_token_stream(),
        );
        cx.error_spanned_by(field.original, msg);
    }
}

/// A `#[serde(flatten)]` member with lifetimes must declare where they are
/// borrowed from. Without an explicit `#[serde(borrow ...)]` the lifetimes
/// would not be tied to the deserializer input and the borrow source of the
/// flattened member is lost.
fn check_flatten_borrow_source(cx: &Ctxt, field: &Field, contract: &FieldContract) {
    if !contract.via_flatten
        || field.attrs.skip_deserializing()
        || field.attrs.deserialize_with().is_some()
    {
        return;
    }

    let mut undeclared = BTreeSet::new();
    attr::collect_lifetimes(field.ty, &mut undeclared);
    for declared in &contract.lifetimes {
        undeclared.remove(declared);
    }
    if undeclared.is_empty() {
        return;
    }

    let lifetimes = undeclared
        .iter()
        .map(ToTokens::to_token_stream)
        .map(|tokens| tokens.to_string())
        .collect::<Vec<_>>()
        .join(" + ");
    let msg = format!(
        "#[serde(flatten)] field `{}` has lifetime {} with no declared borrow source; \
         add #[serde(borrow)] so the flattened member can borrow it from the input",
        contract.member, lifetimes,
    );
    cx.error_spanned_by(field.original, msg);
}

/// Whether a `Cow<'_, str>` or `Cow<'_, [u8]>` appears anywhere in the type
/// except at the top level. Purely structural: unknown constructs such as
/// macro types are opaque and are not descended into.
fn contains_nested_cow(ty: &syn::Type, top: bool) -> bool {
    let ty = ungroup(ty);
    if !top && (attr::is_cow(ty, attr::is_str) || attr::is_cow(ty, attr::is_slice_u8)) {
        return true;
    }
    match ty {
        syn::Type::Path(ty) => {
            if ty.qself.is_some() {
                return false;
            }
            ty.path.segments.iter().any(|seg| match &seg.arguments {
                syn::PathArguments::AngleBracketed(bracketed) => {
                    bracketed.args.iter().any(|arg| match arg {
                        syn::GenericArgument::Type(ty) => contains_nested_cow(ty, false),
                        _ => false,
                    })
                }
                _ => false,
            })
        }
        syn::Type::Tuple(ty) => ty.elems.iter().any(|elem| contains_nested_cow(elem, false)),
        syn::Type::Array(ty) => contains_nested_cow(&ty.elem, false),
        syn::Type::Slice(ty) => contains_nested_cow(&ty.elem, false),
        // serde does not look through parentheses either, so a parenthesized
        // top-level Cow would not get the borrowing deserialize_with.
        syn::Type::Paren(ty) => contains_nested_cow(&ty.elem, false),
        _ => false,
    }
}

/// The inner type `T` of `Option<T>`, structurally.
fn option_inner(ty: &syn::Type) -> Option<&syn::Type> {
    let path = match ungroup(ty) {
        syn::Type::Path(ty) => &ty.path,
        _ => return None,
    };
    let seg = path.segments.last()?;
    if seg.ident != "Option" {
        return None;
    }
    let args = match &seg.arguments {
        syn::PathArguments::AngleBracketed(bracketed) => &bracketed.args,
        _ => return None,
    };
    if args.len() != 1 {
        return None;
    }
    match &args[0] {
        syn::GenericArgument::Type(inner) => Some(inner),
        _ => None,
    }
}

fn member_name(member: &syn::Member) -> String {
    match member {
        syn::Member::Named(ident) => ident.to_string(),
        syn::Member::Unnamed(index) => index.index.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::internals::ast::Container;
    use proc_macro2::Span;
    use syn::Ident;

    fn container(item: &syn::DeriveInput) -> (Ctxt, Option<Container>) {
        let cx = Ctxt::new();
        let cont = Container::from_ast(
            &cx,
            item,
            Derive::Deserialize,
            &Ident::new("private", Span::call_site()),
        );
        (cx, cont)
    }

    fn contract_of(item: &syn::DeriveInput) -> Vec<FieldContract> {
        let (cx, cont) = container(item);
        cx.check().unwrap();
        container_contract(&cont.unwrap())
    }

    fn errors_of(item: &syn::DeriveInput) -> String {
        let (cx, _cont) = container(item);
        cx.check().unwrap_err().to_string()
    }

    fn lifetimes(contract: &FieldContract) -> Vec<String> {
        contract
            .lifetimes
            .iter()
            .map(ToTokens::to_token_stream)
            .map(|tokens| tokens.to_string())
            .collect()
    }

    #[test]
    fn classify_by_structure_not_name() {
        // A field named `owned` can still borrow, and a field named
        // `borrowed` can still be owned: only the type structure matters.
        let item: syn::DeriveInput = syn::parse_quote! {
            struct S<'a> {
                #[serde(borrow)]
                owned: Cow<'a, str>,
                borrowed: String,
            }
        };
        let contracts = contract_of(&item);
        assert_eq!(contracts[0].member, "owned");
        assert_eq!(contracts[0].source, BorrowSource::Input);
        assert_eq!(contracts[1].member, "borrowed");
        assert_eq!(contracts[1].source, BorrowSource::Owned);
        assert_eq!(contracts[1].lifetimes.len(), 0);
    }

    #[test]
    fn classify_nested_option_and_references() {
        assert_eq!(
            classify(&syn::parse_quote!(Cow<'a, str>)),
            BorrowSource::Input
        );
        assert_eq!(
            classify(&syn::parse_quote!(Option<Cow<'a, str>>)),
            BorrowSource::Input
        );
        assert_eq!(
            classify(&syn::parse_quote!(Option<Option<&'a str>>)),
            BorrowSource::Input
        );
        assert_eq!(classify(&syn::parse_quote!(&'a [u8])), BorrowSource::Input);
        assert_eq!(classify(&syn::parse_quote!(String)), BorrowSource::Owned);
        assert_eq!(classify(&syn::parse_quote!(Vec<u8>)), BorrowSource::Owned);
        assert_eq!(
            classify(&syn::parse_quote!(std::collections::HashMap<String, u64>)),
            BorrowSource::Owned
        );
        assert_eq!(
            classify(&syn::parse_quote!(Vec<Cow<'a, str>>)),
            BorrowSource::Opaque
        );
        assert_eq!(classify(&syn::parse_quote!(T)), BorrowSource::Owned);
        assert_eq!(
            classify(&syn::parse_quote!(Wrapper<'a>)),
            BorrowSource::Opaque
        );
    }

    #[test]
    fn flatten_member_source_is_queryable() {
        let item: syn::DeriveInput = syn::parse_quote! {
            struct Envelope<'a> {
                id: u32,
                #[serde(flatten)]
                #[serde(borrow)]
                extra: Inner<'a>,
            }
        };
        let contracts = contract_of(&item);
        assert_eq!(contracts.len(), 2);

        let flattened = &contracts[1];
        assert_eq!(flattened.member, "extra");
        assert!(flattened.via_flatten);
        assert_eq!(lifetimes(flattened), vec!["'a".to_owned()]);

        let plain = &contracts[0];
        assert_eq!(plain.member, "id");
        assert!(!plain.via_flatten);
        assert_eq!(plain.source, BorrowSource::Owned);
    }

    #[test]
    fn owned_type_cannot_claim_zero_copy() {
        let item: syn::DeriveInput = syn::parse_quote! {
            struct S {
                #[serde(borrow)]
                name: String,
            }
        };
        let errors = errors_of(&item);
        assert!(errors.contains("owned type"), "{}", errors);
        assert!(errors.contains("zero-copy"), "{}", errors);
    }

    #[test]
    fn nested_option_cow_is_rejected() {
        let item: syn::DeriveInput = syn::parse_quote! {
            struct S<'a> {
                #[serde(borrow)]
                name: Option<Cow<'a, str>>,
            }
        };
        let errors = errors_of(&item);
        assert!(errors.contains("cannot borrow through"), "{}", errors);
        assert!(errors.contains("not zero-copy"), "{}", errors);
    }

    #[test]
    fn nested_cow_allowed_with_custom_deserialize_with() {
        let item: syn::DeriveInput = syn::parse_quote! {
            struct S<'a> {
                #[serde(borrow, deserialize_with = "deserialize_option_cow")]
                name: Option<Cow<'a, str>>,
            }
        };
        let (cx, _cont) = container(&item);
        cx.check().unwrap();
    }

    #[test]
    fn flatten_without_borrow_source_is_rejected() {
        let item: syn::DeriveInput = syn::parse_quote! {
            struct Outer<'a> {
                id: u32,
                #[serde(flatten)]
                extra: Inner<'a>,
            }
        };
        let errors = errors_of(&item);
        assert!(errors.contains("no declared borrow source"), "{}", errors);
        assert!(errors.contains("`extra`"), "{}", errors);
        assert!(errors.contains("'a"), "{}", errors);
    }

    #[test]
    fn borrow_conflicts_with_skip_deserializing() {
        let item: syn::DeriveInput = syn::parse_quote! {
            struct S<'a> {
                #[serde(borrow, skip_deserializing)]
                name: Cow<'a, str>,
            }
        };
        let errors = errors_of(&item);
        assert!(errors.contains("cannot be combined"), "{}", errors);
    }

    #[test]
    fn wrong_declared_lifetime_is_rejected() {
        let item: syn::DeriveInput = syn::parse_quote! {
            struct S<'a> {
                #[serde(borrow = "'b")]
                name: &'a str,
            }
        };
        let errors = errors_of(&item);
        assert!(errors.contains("does not have lifetime 'b"), "{}", errors);
    }

    #[test]
    fn implicit_option_ref_borrows_without_attribute() {
        let item: syn::DeriveInput = syn::parse_quote! {
            struct S<'a> {
                alias: Option<&'a str>,
            }
        };
        let contracts = contract_of(&item);
        assert_eq!(contracts[0].source, BorrowSource::Input);
        assert_eq!(lifetimes(&contracts[0]), vec!["'a".to_owned()]);
    }
}
