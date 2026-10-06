// A Cow nested inside Option cannot borrow through the Option; only a
// top-level Cow borrows from the input.
use serde::Deserialize;
use std::borrow::Cow;

#[derive(Deserialize)]
struct Nested<'a> {
    #[serde(borrow)]
    name: Option<Cow<'a, str>>,
}

fn main() {}
