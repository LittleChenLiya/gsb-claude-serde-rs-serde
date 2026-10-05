use serde_derive::Deserialize;
use std::borrow::Cow;

#[derive(Deserialize)]
struct Test<'a> {
    #[serde(borrow = "'b")]
    cow: Cow<'a, str>,
}

fn main() {}
