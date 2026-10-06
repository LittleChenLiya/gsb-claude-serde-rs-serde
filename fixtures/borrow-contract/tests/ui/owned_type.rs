// An owned field must not claim zero-copy: #[serde(borrow)] on an owned
// type is rejected at the field.
use serde::Deserialize;

#[derive(Deserialize)]
struct Owned {
    #[serde(borrow)]
    name: String,
}

fn main() {}
