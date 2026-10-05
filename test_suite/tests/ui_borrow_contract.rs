#![allow(unused_attributes)]

// Targeted runner for the borrow-contract UI cases only, so acceptance does
// not need to build the whole ui suite. The same cases are also picked up by
// the full suite in compiletest.rs (tests/ui/**/*.rs) on nightly.
#[test]
fn ui_borrow_contract() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/borrow-contract/*.rs");
}
