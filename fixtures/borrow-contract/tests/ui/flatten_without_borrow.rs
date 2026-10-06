// A flattened member with a lifetime must declare its borrow source.
use serde::Deserialize;

#[derive(Deserialize)]
struct Inner<'a> {
    name: &'a str,
}

#[derive(Deserialize)]
struct Outer<'a> {
    id: u32,
    #[serde(flatten)]
    extra: Inner<'a>,
}

fn main() {}
