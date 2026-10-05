use serde_derive::Deserialize;
use std::borrow::Cow;

#[derive(Deserialize)]
struct Test<'a> {
    #[serde(borrow)]
    cow: Option<Cow<'a, Vec<u8>>>,
}

fn main() {}
