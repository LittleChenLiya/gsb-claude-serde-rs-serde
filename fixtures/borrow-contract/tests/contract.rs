//! Runtime acceptance tests for the explicit borrow contract.
//!
//! Run with: cargo test -p borrow-contract-fixture --test contract

use borrow_contract_fixture::{BorrowedRecord, Envelope, OwnedRecord};
use std::borrow::Cow;

/// Whether `ptr` points into the `input` buffer: proves the deserialized
/// data was borrowed from this exact source, not copied.
fn points_into(input: &str, ptr: *const u8) -> bool {
    let start = input.as_ptr() as usize;
    (start..start + input.len()).contains(&(ptr as usize))
}

#[test]
fn cow_marked_borrow_borrows_from_the_input() {
    let json = String::from(r#"{"name":"ferris","alias":"crab"}"#);
    let record: BorrowedRecord = serde_json::from_str(&json).unwrap();

    // The Cow marked #[serde(borrow)] borrowed from the appropriate source.
    match &record.name {
        Cow::Borrowed(s) => {
            assert_eq!(*s, "ferris");
            assert!(points_into(&json, s.as_ptr()));
        }
        Cow::Owned(s) => panic!("expected zero-copy borrow, got owned {:?}", s),
    }

    // Option<&str> borrows implicitly through the Option nesting.
    match record.alias {
        Some(s) => {
            assert_eq!(s, "crab");
            assert!(points_into(&json, s.as_ptr()));
        }
        None => panic!("expected alias to borrow from the input"),
    }
}

#[test]
fn owned_record_does_not_claim_zero_copy() {
    let json = String::from(r#"{"name":"ferris"}"#);
    let owned: OwnedRecord = serde_json::from_str(&json).unwrap();

    // An owned String never points into the input buffer.
    assert_eq!(owned.name, "ferris");
    assert!(!points_into(&json, owned.name.as_ptr()));
}

#[test]
fn flatten_member_keeps_its_borrow_source() {
    let json = String::from(r#"{"id":7,"name":"ferris","alias":"crab"}"#);
    let envelope: Envelope = serde_json::from_str(&json).unwrap();

    assert_eq!(envelope.id, 7);

    // The flattened member's Cow still borrows from the outer input: the
    // flatten buffering does not lose the source.
    match &envelope.record.name {
        Cow::Borrowed(s) => {
            assert_eq!(*s, "ferris");
            assert!(points_into(&json, s.as_ptr()));
        }
        Cow::Owned(s) => panic!("flatten lost the borrow source, got owned {:?}", s),
    }
    match envelope.record.alias {
        Some(s) => assert!(points_into(&json, s.as_ptr())),
        None => panic!("expected flattened alias to borrow from the input"),
    }
}
