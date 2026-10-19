//! The `main.rs` a consumer builds, made from a README page the way rustdoc makes
//! a doctest from a fenced block.
//!
//! Three rules decide whether a README quickstart compiles as a stranger copies
//! it, and each one was a failure of the shell check this replaces (the
//! `lgwks_ast` 2.0.0 README, #526):
//!
//! * A hidden line is `# ` (or a lone `#`), and `##` is an escaped `#`. A
//!   leading `#[..]` or `#![..]` is code, so `#[derive(..)]` keeps its `#`.
//! * `extern crate` lines and `#![..]` attributes belong to the crate root, so
//!   they stay above the `main` the block is wrapped in.
//! * A block that declares `fn main` is the whole program; any other block is
//!   the body of one.

/// The head of the `main` a doctest-style block is wrapped in.
const WRAP_HEAD: &str = "fn main() -> Result<(), Box<dyn std::error::Error>> {";

/// The fence a runnable block opens with.
const FENCE: &str = "```";

/// Whether `line` opens a Rust block: ```` ```rust ```` alone or with attributes.
fn opens_rust(line: &str) -> bool {
    match line.strip_prefix("```rust") {
        Some(rest) => rest.is_empty() || rest.starts_with(',') || rest.starts_with(' '),
        None => false,
    }
}

/// The first fenced Rust block of `page`, as written; `None` when the page has
/// none, or its first one is empty or never closed.
pub(crate) fn first_rust_block(page: &str) -> Option<String> {
    let mut inside = false;
    let mut block: Vec<&str> = Vec::new();
    for line in page.lines() {
        if !inside {
            inside = opens_rust(line);
            continue;
        }
        if line.starts_with(FENCE) {
            return if block.iter().all(|kept| kept.trim().is_empty()) {
                None
            } else {
                Some(block.join("\n"))
            };
        }
        block.push(line);
    }
    None
}

/// `line` with rustdoc's hiding marker taken off, so a hidden line compiles.
fn reveal(line: &str) -> String {
    let split = line
        .find(|glyph: char| !glyph.is_whitespace())
        .and_then(|at| line.split_at_checked(at));
    let Some((indent, body)) = split else {
        return line.to_owned();
    };
    if body == "#" {
        return indent.to_owned();
    }
    let shown = if body.starts_with("##") {
        body.strip_prefix('#')
    } else {
        body.strip_prefix("# ")
    };
    match shown {
        Some(text) => format!("{indent}{text}"),
        None => line.to_owned(),
    }
}

/// Whether `line` belongs to the crate root rather than to a `main` body.
fn is_crate_level(line: &str) -> bool {
    let body = line.trim_start();
    body.starts_with("#![") || body.starts_with("extern crate ")
}

/// The `main.rs` of a consumer built from `block`.
pub(crate) fn main_rs(block: &str) -> String {
    let lines: Vec<String> = block.lines().map(reveal).collect();
    if lines.iter().any(|line| line.contains("fn main")) {
        return format!("{}\n", lines.join("\n"));
    }
    let (mut program, body): (Vec<String>, Vec<String>) =
        lines.into_iter().partition(|line| is_crate_level(line));
    let returns = body.iter().any(|line| line.contains("Ok::<"));
    program.push(WRAP_HEAD.to_owned());
    program.extend(body);
    if !returns {
        program.push("Ok(())".to_owned());
    }
    program.push("}".to_owned());
    format!("{}\n", program.join("\n"))
}
