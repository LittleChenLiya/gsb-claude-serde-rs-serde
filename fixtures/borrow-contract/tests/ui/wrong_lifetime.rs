// A declared borrow lifetime that the field does not have is reported at
// the field position.
use serde::Deserialize;

#[derive(Deserialize)]
struct Wrong<'a> {
    #[serde(borrow = "'b")]
    name: &'a str,
}

fn main() {}
