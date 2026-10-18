//! ── Feature-matrix acceptance ───────────────────────────────────────────────
//! Compiled unconditionally so `cargo test --no-default-features` asserts
//! something instead of passing vacuously. What a build with a grammar must
//! prove is not that one path lookup returns `Some` but that the grammar it
//! compiled reads its own language: a build with no grammar claims no path at
//! all, and every build claims exactly the extensions its compiled table
//! declares.
//!
//! The `lang-*` features are independent by design, so no assertion here may
//! name a language the build might not have compiled: `Language::Rust` does not
//! exist without `lang-rust`, and a Python-only build is non-empty while
//! correctly refusing `.rs`.

use super::language::{any_extension_matches, extension_of};
use super::*;
/// Paths every build answers from its own compiled table.
///
/// Not one path per declared extension: [`Language::of_path`] is driven by
/// the table itself, so one path per *shape* is enough here — extensions
/// from the default set, ones only a standalone `lang-*` feature declares,
/// an extension no grammar declares, and a path with no extension at all.
/// The extension-by-extension check is
/// `every_compiled_language_resolves_its_own_extensions`.
const PROBES: &[&str] = &[
    "src/lib.rs",
    "src/main.py",
    "src/app.go",
    "src/app.md",
    "src/app.c",
    "src/probe.lgwks-unclaimed",
    "noextension",
];

/// Whether any compiled language declares `extension`.
///
/// This reads the table from the test side so the assertions below compare
/// the lookup against the *compiled* set rather than against a path someone
/// assumed would be present.
fn table_claims(extension: &str) -> bool {
    Language::ALL
        .iter()
        .any(|language| any_extension_matches(language.extensions(), extension))
}

/// A path is claimed exactly when a compiled grammar declares its extension.
///
/// Its predecessor asserted `Language::of_path("src/lib.rs").is_some()`
/// whenever [`Language::ALL`] was non-empty. That implication does not hold:
/// `lang-rust` is independent of every other grammar feature, so a
/// standalone Python build is non-empty and correctly does not claim `.rs`.
///
/// Read what this loop can and cannot tell you. It compares [`Language::of_path`]
/// against the same compiled table `of_path` is built from — `extension_of`
/// and `any_extension_matches` over [`Language::ALL`] — so it is a
/// consistency check between the aggregate lookup and the per-language
/// extension lists, not independent evidence that either is right. It
/// cannot fail while those two agree, and it asserts only `is_some()`, so it
/// says nothing about *which* language a path resolves to.
///
/// The regression the report described is pinned by concrete assertions
/// instead: the two `rust_paths_resolve_*` tests below, which name
/// `Some(Language::Rust)` and `None` outright, the [`Language::ALL`]
/// emptiness block at the end of this test, and
/// `every_compiled_language_resolves_its_own_extensions`, which pairs each
/// compiled language with its own declared extension.
#[test]
fn empty_build_selects_nothing() {
    for &probe in PROBES {
        let expected = extension_of(probe).is_some_and(table_claims);
        assert_eq!(
            Language::of_path(probe).is_some(),
            expected,
            "{probe}: claimed exactly when a compiled grammar declares its extension"
        );
        assert_eq!(
            detect(probe).is_some(),
            expected,
            "{probe}: `detect` is the same lookup under a second name"
        );
    }
    if Language::ALL.is_empty() {
        assert!(
            Language::of_path("src/lib.rs").is_none(),
            "a build with no grammar compiled must not claim .rs"
        );
        assert!(
            Language::of_path("src/main.py").is_none(),
            "a build with no grammar compiled must not claim .py"
        );
    }
}

/// `.rs` resolves exactly when the Rust grammar was compiled.
///
/// [`Language::Rust`] does not exist without `lang-rust`, so the two
/// directions are two tests rather than two branches of one; both assert
/// the lookup rather than inferring it from a non-empty [`Language::ALL`].
#[cfg(feature = "lang-rust")]
#[test]
fn rust_paths_resolve_with_the_rust_grammar() {
    assert_eq!(
        Language::of_path("src/lib.rs"),
        Some(Language::Rust),
        "a build with the Rust grammar must claim .rs"
    );
    assert_eq!(
        detect("src/lib.rs"),
        Some(Language::Rust),
        "detect is of_path under a second name"
    );
}

/// The other direction: no Rust grammar, no `.rs`.
#[cfg(not(feature = "lang-rust"))]
#[test]
fn rust_paths_resolve_to_nothing_without_the_rust_grammar() {
    assert!(
        Language::of_path("src/lib.rs").is_none(),
        "a build without the Rust grammar must not claim .rs, however many other grammars it compiled"
    );
    assert!(
        detect("src/lib.rs").is_none(),
        "detect is of_path under a second name"
    );
}

/// Every compiled grammar resolves each extension it declares.
///
/// The module above asserts this too, but only for builds that compiled
/// `lang-rust`; a standalone grammar build has no counterpart there, and
/// the lookup is exactly what such a build must not get wrong.
#[test]
fn every_compiled_language_resolves_its_own_extensions() {
    for &language in Language::ALL {
        for &extension in language.extensions() {
            let path = format!("dir/probe.{extension}");
            assert_eq!(
                Language::of_path(&path),
                Some(language),
                "{}: {path} must resolve to its own language",
                language.name()
            );
        }
    }
}

/// Every compiled grammar applies the public byte bound before parsing.
///
/// The bound is checked ahead of the parser, so it holds for any grammar;
/// this pins that it is a property of the call rather than of Rust, which
/// is the only grammar the `lang-rust`-gated tests exercise it with.
#[test]
fn every_compiled_language_refuses_oversized_source() {
    let oversized = "x".repeat(MAX_SOURCE_BYTES.saturating_add(1));
    for &language in Language::ALL {
        assert!(
            matches!(
                try_parse(&oversized, language),
                Err(ParseError::SourceTooLarge { .. })
            ),
            "{}: source past MAX_SOURCE_BYTES must be refused before parsing",
            language.name()
        );
    }
}

/// How many grammars the manifest declares, one `lang-*` feature each.
///
/// The fixture table below is the acceptance matrix for these, so the count
/// is stated once here and checked against the table rather than repeated
/// per row.
const DECLARED_GRAMMARS: usize = 28;

/// One row of the fixture table: the grammar's stable name, source it must
/// accept, and source it must refuse.
///
/// A tuple rather than a struct with three named fields because the table is
/// twenty-eight rows of data and a row's shape says nothing the type does
/// not: every reader destructures it, so each use names the field it reads.
type GrammarFixture = (&'static str, &'static str, &'static str);

/// One valid and one malformed fixture per declared grammar.
///
/// The malformed half is the half that proves something. tree-sitter
/// recovers from nearly everything, so a grammar that "parses" a fixture
/// may only be demonstrating recovery; each row's malformed source is a
/// construct the grammar cannot complete — an unterminated block, string,
/// or collection — which a working grammar reports through an `ERROR` or
/// `MISSING` node and [`try_parse`] therefore refuses.
///
/// Rows are keyed on the stable [`Language::name`] rather than on a
/// variant, because a row must be nameable in a build whose feature did not
/// compile the variant: this table is complete in every configuration, and
/// `Language::ALL` decides which rows a given build runs.
///
/// ```text
/// (name, valid source, malformed source)
/// ```
const FIXTURES: &[GrammarFixture] = &[
    ("bash", "echo hello\n", "echo \"unterminated\n"),
    (
        "c",
        "int main(void) { return 0; }\n",
        "int main(void) { return 0;\n",
    ),
    (
        "cpp",
        "int main() { return 0; }\n",
        "int main() { return 0;\n",
    ),
    ("csharp", "class A { }\n", "class A {\n"),
    ("css", "a { color: red; }\n", "a { color: red;\n"),
    ("dart", "void main() {}\n", "void main() {\n"),
    (
        "elixir",
        "defmodule A do\n  def f, do: 1\nend\n",
        "defmodule A do\n  def f, do: 1\n",
    ),
    (
        "go",
        "package main\n\nfunc main() {}\n",
        "package main\n\nfunc main() {\n",
    ),
    (
        "haskell",
        "main = putStrLn \"hi\"\n",
        "main = putStrLn \"hi\n",
    ),
    (
        "hcl",
        "resource \"a\" \"b\" {\n}\n",
        "resource \"a\" \"b\" {\n",
    ),
    (
        "html",
        "<!DOCTYPE html>\n<html><body><p>hi</p></body></html>\n",
        "<html><body><p>hi\n",
    ),
    ("java", "class A {}\n", "class A {\n"),
    ("javascript", "const a = 1;\n", "function f() {\n"),
    ("json", "{\"a\": 1}\n", "{\"a\": 1\n"),
    ("kotlin", "fun main() {}\n", "fun main() {\n"),
    ("lua", "local a = 1\n", "function f(\n"),
    // Markdown's refusal surface is its table scanner:
    // `pipe_table_delimiter_row` in the block grammar requires a `|` after
    // every delimiter cell, so a header row followed by `|---` cannot
    // complete and carries a `MISSING` node. The source is otherwise valid
    // GFM, which is the point of pinning it — the fixture records what
    // *this* compiled grammar refuses, so a grammar bump that starts
    // accepting it fails this row rather than passing quietly. The valid
    // row carries a table written the way that scanner accepts it.
    (
        "markdown",
        "# Title\n\n| a | b |\n|---|---|\n| 1 | 2 |\n",
        "| a |\n|---\n| b |\n",
    ),
    ("nix", "{ pkgs }: pkgs.hello\n", "let a = 1;\n"),
    ("php", "<?php echo \"hi\";\n", "<?php function f() {\n"),
    (
        "python",
        "def f():\n    return 1\n",
        "def f(:\n    return 1\n",
    ),
    ("ruby", "def f\n  1\nend\n", "def f\n  1\n"),
    ("rust", "fn main() {}\n", "fn main( {\n"),
    (
        "scala",
        "object A { def f = 1 }\n",
        "object A { def f = 1\n",
    ),
    ("solidity", "contract A {}\n", "contract A {\n"),
    ("swift", "func f() {}\n", "func f() {\n"),
    ("tsx", "const A = () => <div />;\n", "function f() {\n"),
    ("typescript", "const a: number = 1;\n", "function f() {\n"),
    ("yaml", "a: 1\n", "a: [1, 2\n"),
];

/// Every compiled grammar reads its own language, and refuses a source that
/// is not one.
///
/// A path lookup returning `Some` only proves a table row exists; this is
/// the test that proves the grammar behind the row works. Both directions
/// go through the public checked API, so a "valid" fixture that the grammar
/// only *recovers* into a tree fails, and so does a malformed fixture the
/// grammar happens to accept. Findings are collected rather than asserted
/// one by one, so a single run reports every grammar out of step with its
/// fixture instead of stopping at the first.
#[test]
fn every_compiled_grammar_parses_its_fixture_and_refuses_a_malformed_one() {
    let mut failures: Vec<String> = Vec::new();
    for &language in Language::ALL {
        let name = language.name();
        let Some(&(_, valid, malformed)) = FIXTURES.iter().find(|row| row.0 == name) else {
            failures.push(format!("`{name}` is compiled but has no fixture row"));
            continue;
        };
        if let Err(refusal) = try_parse(valid, language) {
            failures.push(format!("`{name}` refused its valid fixture: {refusal}"));
        }
        match try_parse(malformed, language) {
            Err(ParseError::InvalidSyntax { .. }) => {}
            Ok(_) => failures.push(format!(
                "`{name}` accepted a malformed fixture: {malformed:?}"
            )),
            Err(other) => failures.push(format!(
                "`{name}` refused a malformed fixture as {other}, not InvalidSyntax"
            )),
        }
    }
    assert!(
        failures.is_empty(),
        "grammar fixtures disagree with their grammars: {}",
        failures.join("; ")
    );
}

/// The fixture table holds one row per declared grammar, with no repeats.
///
/// A row nothing matches is a row that never runs, which is how a renamed
/// grammar would drop out of the matrix silently; the count is what makes a
/// deleted row fail here rather than shrink the matrix unnoticed.
#[test]
fn fixture_table_is_one_row_per_declared_grammar() {
    let mut covered: Vec<&str> = Vec::new();
    for &(name, _, _) in FIXTURES {
        assert!(!covered.contains(&name), "two fixture rows cover `{name}`");
        covered.push(name);
    }
    assert_eq!(
        covered.len(),
        DECLARED_GRAMMARS,
        "the fixture table must cover every grammar the manifest declares"
    );
}

/// Under `full` every declared grammar is compiled, so the table and the
/// compiled set correspond exactly: a misspelled row name has nothing to
/// match and is caught here rather than skipped.
#[cfg(feature = "full")]
#[test]
fn fixture_table_names_exactly_the_grammars_full_compiles() {
    for &(name, _, _) in FIXTURES {
        assert!(
            Language::ALL.iter().any(|language| language.name() == name),
            "fixture row `{name}` names no grammar that `full` compiled"
        );
    }
    assert_eq!(
        Language::ALL.len(),
        FIXTURES.len(),
        "`full` compiles every declared grammar, so every one must have a fixture row"
    );
}
