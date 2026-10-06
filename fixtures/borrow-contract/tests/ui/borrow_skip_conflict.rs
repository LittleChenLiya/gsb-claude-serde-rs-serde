// borrow conflicts with skip_deserializing: a skipped field is never
// deserialized so there is nothing to borrow.
use serde::Deserialize;
use std::borrow::Cow;

#[derive(Deserialize)]
struct Conflict<'a> {
    #[serde(borrow, skip_deserializing)]
    name: Cow<'a, str>,
}

fn main() {}
