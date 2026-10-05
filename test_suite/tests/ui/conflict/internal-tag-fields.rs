use serde_derive::{Deserialize, Serialize};

// The deserialize-only name of a field is checked against the tag...
#[derive(Deserialize)]
#[serde(tag = "conflict")]
enum DeName {
    A {
        #[serde(rename(deserialize = "conflict"))]
        x: (),
    },
}

// ...and every conflicting field is reported, not just the first one.
#[derive(Serialize)]
#[serde(tag = "conflict")]
enum ReportAll {
    A {
        #[serde(rename = "conflict")]
        x: (),
    },
    B {
        #[serde(rename = "conflict")]
        y: (),
    },
}

fn main() {}
