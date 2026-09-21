//! Identify a file's language and parse it under the crate's bounds.
//!
//! ```sh
//! cargo add lgwks_ast
//! ```
//!
//! Grammars are cargo features: the default build parses Rust, Python,
//! TypeScript, JavaScript, Go, Java, and Swift. Run with
//! `cargo run -p lgwks_ast --example parse`.

//! The output line is written through `std::io::Write` rather than `println!`
//! because `clippy::print_stdout` is forbidden workspace-wide, with no
//! test/example carve-out; the lint is what is enforced, and an example that
//! writes explicitly says the
//! same thing fallibly.

use std::io::Write;

use lgwks_ast::{Language, inspect_ast, try_parse};

/// `Result` from `main` is how the example reports a failure: the refusal's own
/// `Debug` reaches the caller, and no `expect` sits in the tree.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let language = Language::of_path("src/lib.rs")
        .ok_or("the default build compiles the rust grammar, so this must resolve")?;
    assert_eq!(language, Language::Rust, "src/lib.rs is a rust path");

    let parsed = try_parse("fn f() {}", language)?;
    let metrics = inspect_ast(&parsed.root(), None);
    assert!(
        metrics.nodes > 1,
        "a parsed function has more than its root node, got {}",
        metrics.nodes
    );

    let mut stdout = std::io::stdout();
    writeln!(stdout, "parsed rust: {} nodes", metrics.nodes)?;
    Ok(())
}
