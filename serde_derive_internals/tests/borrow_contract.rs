//! Query-side acceptance tests for the explicit borrow contract.
//!
//! Three structs are exercised throughout:
//!
//! ```ignore
//! // 1. truly borrowed fields
//! struct Borrowed<'a> {
//!     #[serde(borrow)]
//!     cow: Cow<'a, str>,
//!     s: &'a str,
//!     opt: Option<&'a [u8]>,
//! }
//!
//! // 2. fully owned fields: no zero-copy claim
//! struct Owned {
//!     s: String,
//!     n: u64,
//!     cow: Cow<'static, str>,
//! }
//!
//! // 3. a flattened member keeps its borrow source
//! struct Outer<'a> {
//!     id: u64,
//!     #[serde(flatten)]
//!     inner: Inner<'a>,
//!     #[serde(borrow)]
//!     cow: Cow<'a, str>,
//! }
//! ```

use serde_derive_internals::borrowck::{field_contracts, BorrowSource, FieldContract};
use serde_derive_internals::{ast, Ctxt, Derive};
use std::collections::BTreeSet;
use syn::DeriveInput;

fn contracts_of(input: &DeriveInput) -> Result<Vec<FieldContract>, syn::Error> {
    let cx = Ctxt::new();
    let private = syn::Ident::new("private", proc_macro2::Span::call_site());
    let cont = ast::Container::from_ast(&cx, input, Derive::Deserialize, &private).unwrap();
    cx.check()?;
    Ok(field_contracts(&cont))
}

fn borrowed(lifetimes: &[&str]) -> BorrowSource {
    BorrowSource::Borrowed {
        lifetimes: lifetimes.iter().map(|lt| lt.to_string()).collect(),
    }
}

// Struct 1: a Cow marked #[serde(borrow)] borrows from the deserializer
// input, and so do the implicitly borrowed &str / Option<&[u8]> fields.
#[test]
fn borrowed_cow_borrows_from_input() {
    let input: DeriveInput = syn::parse_quote! {
        struct Borrowed<'a> {
            #[serde(borrow)]
            cow: Cow<'a, str>,
            s: &'a str,
            opt: Option<&'a [u8]>,
        }
    };
    let contracts = contracts_of(&input).unwrap();
    assert_eq!(contracts.len(), 3);

    let cow = &contracts[0];
    assert_eq!(cow.member, "cow");
    assert!(cow.explicit_borrow);
    assert_eq!(cow.source, borrowed(&["a"]));
    assert!(cow.is_zero_copy());

    let s = &contracts[1];
    assert_eq!(s.member, "s");
    assert!(!s.explicit_borrow);
    assert_eq!(s.source, borrowed(&["a"]));

    let opt = &contracts[2];
    assert_eq!(opt.member, "opt");
    assert_eq!(opt.source, borrowed(&["a"]));
}

// Struct 2: a fully owned struct never claims to be zero-copy.
#[test]
fn owned_fields_do_not_claim_zero_copy() {
    let input: DeriveInput = syn::parse_quote! {
        struct Owned {
            s: String,
            n: u64,
            cow: Cow<'static, str>,
        }
    };
    let contracts = contracts_of(&input).unwrap();
    assert_eq!(contracts.len(), 3);
    for contract in &contracts {
        assert!(!contract.explicit_borrow);
        assert_eq!(contract.source, BorrowSource::Owned);
        assert!(!contract.is_zero_copy());
    }
}

// Struct 3: the source of a borrow that passes through a #[serde(flatten)]
// member stays queryable, keyed by the member's own name.
#[test]
fn flatten_member_source_is_queryable() {
    let input: DeriveInput = syn::parse_quote! {
        struct Outer<'a> {
            id: u64,
            #[serde(flatten)]
            inner: Inner<'a>,
            #[serde(borrow)]
            cow: Cow<'a, str>,
        }
    };
    let contracts = contracts_of(&input).unwrap();
    assert_eq!(contracts.len(), 3);

    let inner = contracts
        .iter()
        .find(|contract| contract.member == "inner")
        .unwrap();
    assert_eq!(
        inner.source,
        BorrowSource::Flatten {
            lifetimes: BTreeSet::new(),
        }
    );
    assert!(!inner.is_zero_copy());

    let cow = contracts
        .iter()
        .find(|contract| contract.member == "cow")
        .unwrap();
    assert_eq!(cow.source, borrowed(&["a"]));
    assert!(cow.is_zero_copy());
}

// A borrowed flatten member reports the lifetimes it carries instead of
// dropping them.
#[test]
fn flatten_borrowed_member_keeps_lifetimes() {
    let input: DeriveInput = syn::parse_quote! {
        struct Outer<'a> {
            #[serde(flatten, borrow)]
            map: std::collections::HashMap<&'a str, u32>,
        }
    };
    let contracts = contracts_of(&input).unwrap();
    assert_eq!(contracts.len(), 1);
    assert_eq!(
        contracts[0].source,
        BorrowSource::Flatten {
            lifetimes: ["a".to_string()].into_iter().collect(),
        }
    );
}

// An owned field must not borrow: #[serde(borrow)] on a type without
// lifetimes is rejected at the field.
#[test]
fn rejects_borrow_of_owned_field() {
    let input: DeriveInput = syn::parse_quote! {
        struct S {
            #[serde(borrow)]
            s: String,
        }
    };
    let err = contracts_of(&input).unwrap_err();
    assert!(err.to_string().contains("has no lifetimes to borrow"));
}

// A lifetime declaration that does not exist in the field type is rejected.
#[test]
fn rejects_wrong_lifetime_declaration() {
    let input: DeriveInput = syn::parse_quote! {
        struct S<'a> {
            #[serde(borrow = "'b")]
            cow: Cow<'a, str>,
        }
    };
    let err = contracts_of(&input).unwrap_err();
    assert!(err.to_string().contains("does not have lifetime 'b"));
}

// A Cow over an owned element type cannot borrow, even nested.
#[test]
fn rejects_unborrowable_cow() {
    for input in [
        syn::parse_quote! {
            struct S<'a> {
                #[serde(borrow)]
                cow: Cow<'a, String>,
            }
        },
        syn::parse_quote! {
            struct S<'a> {
                #[serde(borrow)]
                cow: Option<Cow<'a, Vec<u8>>>,
            }
        },
    ] {
        let err = contracts_of(&input).unwrap_err();
        assert!(
            err.to_string().contains("cannot borrow from the deserializer"),
            "unexpected error: {}",
            err,
        );
    }
}

// A Cow over a generic type parameter may be instantiated with str, so it is
// not rejected.
#[test]
fn allows_generic_cow_element() {
    let input: DeriveInput = syn::parse_quote! {
        struct S<'a, T: ?Sized + ToOwned> {
            #[serde(borrow)]
            cow: Cow<'a, T>,
        }
    };
    let contracts = contracts_of(&input).unwrap();
    assert_eq!(contracts[0].source, borrowed(&["a"]));
}

// #[serde(borrow)] combined with #[serde(skip_deserializing)] is a
// conflicting attribute pair and is rejected before compile.
#[test]
fn rejects_borrow_skip_conflict() {
    let input: DeriveInput = syn::parse_quote! {
        struct S<'a> {
            #[serde(borrow, skip_deserializing)]
            s: &'a str,
        }
    };
    let err = contracts_of(&input).unwrap_err();
    assert!(err.to_string().contains("cannot be combined"));
}
