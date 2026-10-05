use serde_derive::{Deserialize, Serialize};

// Two variants sharing an alias must be rejected.
#[derive(Deserialize)]
enum SharedAlias {
    #[serde(alias = "dup")]
    A,
    #[serde(alias = "dup")]
    B,
}

// Rename and alias share a single deserialization namespace.
#[derive(Deserialize)]
enum RenameVsAlias {
    #[serde(rename = "dup")]
    A,
    #[serde(alias = "dup")]
    B,
}

// Serialization names are a namespace of their own.
#[derive(Serialize)]
enum SharedSerName {
    #[serde(rename = "dup")]
    A,
    #[serde(rename = "dup")]
    B,
}

// skip_serializing excludes a variant from the serialization check only;
// the deserialization conflict is still rejected.
#[derive(Deserialize)]
enum SkipSerStillChecked {
    #[serde(rename = "dup")]
    A,
    #[serde(rename = "dup", skip_serializing)]
    B,
}

// ...and vice versa.
#[derive(Serialize)]
enum SkipDeStillChecked {
    #[serde(rename = "dup")]
    A,
    #[serde(rename = "dup", skip_deserializing)]
    B,
}

// Internally tagged and adjacently tagged representations are checked too.
#[derive(Deserialize)]
#[serde(tag = "t")]
enum Internal {
    #[serde(alias = "dup")]
    A,
    #[serde(alias = "dup")]
    B,
}

#[derive(Deserialize)]
#[serde(tag = "t", content = "c")]
enum Adjacent {
    #[serde(alias = "dup")]
    A,
    #[serde(alias = "dup")]
    B,
}

// Every conflict is reported, not just the first one.
#[derive(Deserialize)]
enum ReportAll {
    #[serde(rename = "one")]
    A,
    #[serde(rename = "one")]
    B,
    #[serde(rename = "two")]
    C,
    #[serde(rename = "two")]
    D,
}

fn main() {}
