use serde_derive::Deserialize;

#[derive(Deserialize)]
struct Test<'a> {
    #[serde(borrow, skip_deserializing)]
    s: &'a str,
}

fn main() {}
