//! Compile-fail acceptance tests for the explicit borrow contract.
//!
//! Run with: cargo test -p borrow-contract-fixture --test ui

#[test]
fn ui() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/*.rs");
}
