//! Identify a file's language and parse it under the crate's bounds.
//!
//! ```sh
//! cargo add lgwks_ast
//! ```
//!
//! Grammars are cargo features: the default build parses Rust, Python,
//! TypeScript, JavaScript, Go, Java, and Swift. Run with
//! `cargo run -p lgwks_ast --example parse`.

use lgwks_ast::{Language, inspect_ast, try_parse};

fn main() {
    let language = Language::of_path("src/lib.rs").expect("rust grammar is compiled in");
    assert_eq!(language, Language::Rust);

    let parsed = try_parse("fn f() {}", language).expect("valid rust parses");
    let metrics = inspect_ast(&parsed.root(), None);
    assert!(metrics.nodes > 1);

    println!("parsed rust: {} nodes", metrics.nodes);
}
