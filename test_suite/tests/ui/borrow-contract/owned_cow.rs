use serde_derive::Deserialize;
use std::borrow::Cow;

#[derive(Deserialize)]
struct Test<'a> {
    #[serde(borrow)]
    cow: Cow<'a, String>,
}

fn main() {}
