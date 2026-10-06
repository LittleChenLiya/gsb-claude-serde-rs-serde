use serde_derive::{Deserialize, Serialize};

// Two variants sharing the same deserialize alias must be rejected: the input
// name `common` could not be routed to a single variant.
#[derive(Deserialize)]
enum SharedAlias {
    #[serde(alias = "common")]
    A,
    #[serde(alias = "common")]
    B,
}

// Two variants renamed to the same serialize name must be rejected.
#[derive(Serialize)]
enum SharedSerializeName {
    #[serde(rename = "same")]
    A,
    #[serde(rename = "same")]
    B,
}

// rename_all rules can create collisions too.
#[derive(Deserialize)]
#[serde(rename_all = "UPPERCASE")]
enum RenameAllCollision {
    #[serde(rename = "A")]
    First,
    A,
}

// A skip only exempts its own direction: `skip_serializing` does not keep the
// deserialize names below from colliding.
#[derive(Deserialize)]
enum SkipDoesNotMixDirections {
    #[serde(rename = "same")]
    A,
    #[serde(rename = "same", skip_serializing)]
    B,
}

// Every conflict is reported before code generation, not just the first one.
#[derive(Deserialize)]
enum MultipleConflicts {
    #[serde(alias = "x")]
    A,
    #[serde(alias = "x")]
    B,
    #[serde(rename = "y")]
    C,
    #[serde(rename = "y")]
    D,
}

// Internally tagged enums: every field colliding with the reserved tag name
// is reported, not just the first one.
#[derive(Serialize)]
#[serde(tag = "type")]
enum InternalTagReservedFields {
    A {
        #[serde(rename = "type")]
        x: u8,
    },
    B {
        #[serde(rename = "type")]
        y: u8,
    },
}

// ---------------------------------------------------------------------------
// The cases below must NOT produce errors.

// Custom deserialize_with / serialize_with logic is undecided for the derive,
// so coinciding names are not rejected.
mod custom {
    use serde::{Deserializer, Serializer};

    pub fn serialize<S: Serializer>(_: &u8, _: S) -> Result<S::Ok, S::Error> {
        unimplemented!()
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(_: D) -> Result<u8, D::Error> {
        unimplemented!()
    }
}

#[derive(Serialize)]
enum CustomSerializeUndecided {
    #[serde(rename = "same", serialize_with = "custom::serialize")]
    A(u8),
    #[serde(rename = "same")]
    B(u8),
}

#[derive(Deserialize)]
enum CustomDeserializeUndecided {
    #[serde(rename = "same", deserialize_with = "custom::deserialize")]
    A(u8),
    #[serde(rename = "same")]
    B(u8),
}

// Container-level conversions replace the generated (de)serialization, so
// the effective variant names are undecided.
#[derive(Clone, Serialize)]
#[serde(into = "String")]
enum IntoConversion {
    #[serde(rename = "same")]
    A,
    #[serde(rename = "same")]
    B,
}

impl From<IntoConversion> for String {
    fn from(_: IntoConversion) -> String {
        unimplemented!()
    }
}

#[derive(Deserialize)]
#[serde(from = "String")]
enum FromConversion {
    #[serde(rename = "same")]
    A,
    #[serde(rename = "same")]
    B,
}

impl From<String> for FromConversion {
    fn from(_: String) -> Self {
        unimplemented!()
    }
}

// Untagged enums do not route input or output by variant name.
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum UntaggedNamesUnused {
    #[serde(rename = "same")]
    A(u8),
    #[serde(rename = "same")]
    B(u16),
}

// skip_serializing exempts only the serialize direction, so identical
// serialize names are fine here as long as deserialize names differ.
#[derive(Serialize)]
enum SkipSerializeDirection {
    #[serde(rename = "same")]
    A,
    #[serde(rename = "same", skip_serializing)]
    B,
}

// skip_deserializing exempts only the deserialize direction.
#[derive(Deserialize)]
enum SkipDeserializeDirection {
    #[serde(rename = "same")]
    A,
    #[serde(rename = "same", skip_deserializing)]
    B,
}

// An `other` variant is a catch-all; its name never routes input.
#[derive(Deserialize)]
enum OtherIsCatchAll {
    #[serde(alias = "same")]
    A,
    #[serde(other)]
    B,
}

fn main() {}
