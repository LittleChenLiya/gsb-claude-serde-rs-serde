//! Runtime acceptance tests for the enum variant name pre-check in the
//! derive: legal rename/alias/skip combinations must keep working exactly as
//! before (the derive defaults stay compatible), in each tag/content mode.
#![deny(trivial_numeric_casts)]
#![allow(clippy::derive_partial_eq_without_eq)]

use serde_derive::{Deserialize, Serialize};
use serde_test::{assert_de_tokens, assert_ser_tokens, assert_tokens, Token};

// Legal rename roundtrip: serialize and deserialize names are checked
// independently, so a serialize name may freely coincide with another
// variant's deserialize name.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
enum LegalRenameRoundtrip {
    #[serde(rename(serialize = "ser_a", deserialize = "de_a"))]
    A,
    #[serde(rename(serialize = "ser_b", deserialize = "de_b"))]
    #[serde(alias = "alias_b")]
    B,
}

#[test]
fn legal_rename_roundtrip() {
    assert_ser_tokens(
        &LegalRenameRoundtrip::A,
        &[Token::UnitVariant {
            name: "LegalRenameRoundtrip",
            variant: "ser_a",
        }],
    );
    assert_de_tokens(
        &LegalRenameRoundtrip::A,
        &[Token::UnitVariant {
            name: "LegalRenameRoundtrip",
            variant: "de_a",
        }],
    );
    assert_de_tokens(
        &LegalRenameRoundtrip::B,
        &[Token::UnitVariant {
            name: "LegalRenameRoundtrip",
            variant: "alias_b",
        }],
    );
    assert_ser_tokens(
        &LegalRenameRoundtrip::B,
        &[Token::UnitVariant {
            name: "LegalRenameRoundtrip",
            variant: "ser_b",
        }],
    );
}

// A serialize-only skip removes the variant from the serialize namespace
// only; identical serialize names are then fine.
#[derive(Debug, PartialEq, Serialize)]
enum SkipSerializingDirection {
    #[serde(rename = "dup")]
    A,
    #[serde(rename = "dup", skip_serializing)]
    B,
    C,
}

#[test]
fn skip_serializing_only_exempts_serialize_direction() {
    assert_ser_tokens(
        &SkipSerializingDirection::A,
        &[Token::UnitVariant {
            name: "SkipSerializingDirection",
            variant: "dup",
        }],
    );
    assert_ser_tokens(
        &SkipSerializingDirection::C,
        &[Token::UnitVariant {
            name: "SkipSerializingDirection",
            variant: "C",
        }],
    );
}

// A deserialize-only skip removes the variant from the deserialize namespace
// only; identical deserialize names are then fine.
#[derive(Debug, PartialEq, Deserialize)]
enum SkipDeserializingDirection {
    #[serde(rename = "dup")]
    A,
    #[serde(rename = "dup", skip_deserializing)]
    B,
}

#[test]
fn skip_deserializing_only_exempts_deserialize_direction() {
    assert_de_tokens(
        &SkipDeserializingDirection::A,
        &[Token::UnitVariant {
            name: "SkipDeserializingDirection",
            variant: "dup",
        }],
    );
}

// Internally tagged enums: the reserved tag field and legal renames keep
// working together.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
enum InternallyTagged {
    #[serde(rename = "renamed")]
    Struct {
        value: u8,
    },
    Other,
}

#[test]
fn internally_tagged_reserved_tag_roundtrip() {
    assert_tokens(
        &InternallyTagged::Struct { value: 1 },
        &[
            Token::Struct {
                name: "InternallyTagged",
                len: 2,
            },
            Token::Str("type"),
            Token::Str("renamed"),
            Token::Str("value"),
            Token::U8(1),
            Token::StructEnd,
        ],
    );
    assert_tokens(
        &InternallyTagged::Other,
        &[
            Token::Struct {
                name: "InternallyTagged",
                len: 1,
            },
            Token::Str("type"),
            Token::Str("Other"),
            Token::StructEnd,
        ],
    );
}

// Adjacently tagged enums: tag/content keys plus variant renames.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", content = "c")]
enum AdjacentlyTagged {
    #[serde(rename = "renamed")]
    A(u8),
}

#[test]
fn adjacently_tagged_rename_roundtrip() {
    assert_tokens(
        &AdjacentlyTagged::A(1),
        &[
            Token::Struct {
                name: "AdjacentlyTagged",
                len: 2,
            },
            Token::Str("t"),
            Token::UnitVariant {
                name: "AdjacentlyTagged",
                variant: "renamed",
            },
            Token::Str("c"),
            Token::U8(1),
            Token::StructEnd,
        ],
    );
}
