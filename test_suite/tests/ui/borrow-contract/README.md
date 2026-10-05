# Explicit borrow contract for `#[derive(Deserialize)]`

The derive classifies every field structurally — from its **type** and its
**attributes**, never from the field's name — into one of three categories:

1. **Truly borrowed fields** — the generated `Deserialize` impl gets a
   `'de: 'a` bound per borrowed lifetime and the field is deserialized
   zero-copy from the deserializer input. This covers `&'a str`, `&'a [u8]`
   (implicit), and `Cow<'a, str>` / `Cow<'a, [u8]>` under an explicit
   `#[serde(borrow)]`, including lifetimes nested inside `Option`, `Vec`,
   tuples and references, which are bound according to the attribute.
2. **Owned fields** — no borrowed lifetimes, no zero-copy claim. An owned
   type never reports itself as zero-copy; `#[serde(borrow)]` on a type with
   no lifetimes, or on a `Cow` over an owned element type such as
   `Cow<'a, String>`, is rejected at the field.
3. **Unsupported combinations** — conflicting attributes such as
   `#[serde(borrow)]` + `#[serde(skip_deserializing)]` are rejected with a
   compile error before any code is generated.

The source of every borrow — including borrows that pass through a
`#[serde(flatten)]` member — is queryable at
`serde_derive_internals::borrowck::field_contracts`.

## Three-struct example

```rust
use serde_derive::Deserialize;
use std::borrow::Cow;

// 1. Truly borrowed: `cow` borrows from the deserializer input; `s` and
//    `opt` borrow implicitly.
#[derive(Deserialize)]
struct Borrowed<'a> {
    #[serde(borrow)]
    cow: Cow<'a, str>,
    s: &'a str,
    opt: Option<&'a [u8]>,
}

// 2. Fully owned: no zero-copy claim, no `'de` bounds.
#[derive(Deserialize)]
struct Owned {
    cow: Cow<'static, str>,
    s: String,
}

// 3. Flatten: the member declares that it sources borrowed data, so the
//    source is propagated (`'de: 'a`) instead of lost.
#[derive(Deserialize)]
struct Inner<'a> {
    #[serde(borrow)]
    cow: Cow<'a, str>,
}

#[derive(Deserialize)]
struct Outer<'a> {
    #[serde(flatten, borrow)]
    inner: Inner<'a>,
}
```

## Compile-fail cases in this directory

| Case | Rejected because |
| ---- | ---------------- |
| `owned_cow.rs` | `Cow<'a, String>` owns its data; `borrow` would be a false zero-copy claim |
| `nested_owned_cow.rs` | same, nested inside `Option` |
| `wrong_lifetime.rs` | `borrow = "'b"` names a lifetime the field does not have; error points at the field |
| `borrow_skip.rs` | `borrow` conflicts with `skip_deserializing` |

## Cargo commands

```console
# Queryable contract API + error cases (three-struct example, no derive output)
cargo test -p serde_derive_internals --test borrow_contract

# Runtime behavior of the three structs (Cow::Borrowed vs Cow::Owned, flatten)
cargo test -p serde_test_suite --test test_borrow_contract

# Compile-fail UI cases in this directory (targeted trybuild run)
cargo test -p serde_test_suite --test ui_borrow_contract
```
