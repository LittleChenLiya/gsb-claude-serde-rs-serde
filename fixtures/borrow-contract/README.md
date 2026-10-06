# Borrow contract fixture

Minimal fixture for the explicit `Deserialize` borrow contract checks in
`serde_derive` (`serde_derive/src/internals/borrow.rs`).

The derive classifies every field by the **structure of its type** and the
`#[serde(borrow ...)]` attributes actually written on it — never by field
name:

| kind | examples | contract |
| --- | --- | --- |
| genuinely borrowing | `&'a str`, `Cow<'a, str>` + `#[serde(borrow)]`, `Option<&'a str>` | borrows from the input (zero-copy) |
| owned | `String`, `Vec<u8>` | no lifetimes; `#[serde(borrow)]` is a compile error — owned types cannot claim zero-copy |
| unsupported combination | `#[serde(borrow)]` on `Option<Cow<'a, str>>`, `borrow` + `skip_deserializing`, `flatten` without a declared borrow source | rejected before code is generated |

## Three-struct example

See [`src/lib.rs`](src/lib.rs):

1. `BorrowedRecord<'a>` — `#[serde(borrow)] name: Cow<'a, str>` borrows from
   the input; `Option<&'a str>` borrows implicitly through the `Option`.
2. `OwnedRecord` — owned `String`, no zero-copy claim.
3. `Envelope<'a>` — `#[serde(flatten)] #[serde(borrow)] record: BorrowedRecord<'a>`;
   the flattened member keeps its borrow source, which is queryable through
   `serde_derive_internals::borrow::container_contract` and still points into
   the outer input at runtime.

## Commands

```console
cargo test -p borrow-contract-fixture --test contract   # runtime: Cow borrows from its source, flatten keeps the source, owned stays owned
cargo test -p borrow-contract-fixture --test ui         # trybuild: field-position compile errors
cargo test -p serde_derive_internals borrow             # unit tests of the contract classification and query API
```

Regenerate the trybuild expectations after changing diagnostics:

```console
TRYBUILD=overwrite cargo test -p borrow-contract-fixture --test ui
```
