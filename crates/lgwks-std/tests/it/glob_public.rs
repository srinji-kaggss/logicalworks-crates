//! Consumer-level checks for the compiled and one-call glob APIs.

use lgwks_std::glob::{
    GlobDialect, GlobPattern, GlobScratch, PatternError, PatternErrorKind, matches,
};
use std::error::Error;
use std::fs;
use std::process::Command;

#[test]
fn public_compile_reuse_and_legacy_call_share_text_semantics() -> Result<(), PatternError> {
    let compiled = GlobPattern::compile("src/**/[a-z]?.rs")?;
    let mut scratch = GlobScratch::new();
    for (path, expected) in [
        ("src/a1.rs", true),
        ("src/nested/a1.rs", true),
        ("src/nested/ab1.rs", false),
    ] {
        assert_eq!(
            compiled.is_match_with(path, &mut scratch),
            expected,
            "compiled matcher returns the declared result"
        );
        assert_eq!(
            compiled.is_match(path),
            expected,
            "compiled one-call matcher agrees"
        );
    }
    assert!(
        matches("a**b", "a/x/b"),
        "legacy one-call entry keeps embedded double-star migration behavior"
    );
    let legacy = GlobPattern::compile_with_dialect("a**b", GlobDialect::Legacy)?;
    assert!(
        legacy.is_match_with("a/x/b", &mut scratch),
        "named legacy policy preserves embedded double-star"
    );
    Ok(())
}

#[test]
fn public_checked_compile_distinguishes_malformed_input_from_no_match() {
    let malformed = GlobPattern::compile("[unterminated")
        .err()
        .map(|error| (error.kind, error.byte_offset));
    assert_eq!(
        malformed,
        Some((PatternErrorKind::UnclosedClass, 0)),
        "malformed syntax has a typed error and byte offset"
    );
    assert_eq!(
        GlobPattern::compile("[a-z]").map(|pattern| pattern.is_match("7")),
        Ok(false),
        "valid non-matching input is an ordinary false result"
    );
}

#[test]
fn public_wildcards_and_classes_count_unicode_scalars() -> Result<(), PatternError> {
    let one = GlobPattern::compile("?")?;
    let two = GlobPattern::compile("??")?;
    let bengali = GlobPattern::compile("[অ-ঊ]")?;
    let supplementary = GlobPattern::compile("[😀-🙏]")?;
    let decomposed = GlobPattern::compile("??")?;
    assert!(
        one.is_match("é"),
        "one question consumes one multibyte scalar"
    );
    assert!(
        !two.is_match("é"),
        "two questions do not consume one scalar"
    );
    assert!(
        bengali.is_match("ঈ"),
        "Bengali scalar range is ordered by Unicode scalar value"
    );
    assert!(
        supplementary.is_match("😃"),
        "supplementary scalar ranges are supported"
    );
    assert!(
        decomposed.is_match("e\u{301}"),
        "decomposed text contains two scalars without normalization"
    );
    assert!(
        !GlobPattern::compile("[!a]")?.is_match("/"),
        "negated classes exclude slash"
    );
    Ok(())
}

#[test]
fn shared_subset_matches_the_pinned_vendored_upstream() -> Result<(), Box<dyn Error>> {
    let cases = [
        ("literal", "literal"),
        ("a?c", "abc"),
        ("a?c", "ac"),
        ("a*b", "ab"),
        ("a*b", "axyb"),
        ("a*b", "axyc"),
        ("[a-c]", "b"),
        ("[a-c]", "z"),
        ("[!a-c]", "z"),
        ("[!a-c]", "b"),
    ];
    let ours = cases
        .iter()
        .map(|&(pattern, path)| matches(pattern, path))
        .collect::<Vec<_>>();

    let scratch = tempfile::tempdir()?;
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let upstream = manifest_dir.join("../../vendor/glob/src/lib.rs");
    let library = scratch.path().join("libglob.rlib");
    let library_build = Command::new("rustc")
        .arg("--crate-name")
        .arg("glob")
        .arg("--crate-type")
        .arg("lib")
        .arg("--edition=2021")
        .arg(&upstream)
        .arg("--out-dir")
        .arg(scratch.path())
        .output()?;
    assert!(
        library_build.status.success(),
        "pinned upstream library compile failed: {}",
        String::from_utf8_lossy(&library_build.stderr)
    );
    assert!(library.exists(), "pinned upstream rlib was produced");

    let consumer_source = scratch.path().join("consumer.rs");
    fs::write(
        &consumer_source,
        r#"
use std::error::Error;
use std::io::Write;

fn main() -> Result<(), Box<dyn Error>> {
    let cases = [
        ("literal", "literal"),
        ("a?c", "abc"),
        ("a?c", "ac"),
        ("a*b", "ab"),
        ("a*b", "axyb"),
        ("a*b", "axyc"),
        ("[a-c]", "b"),
        ("[a-c]", "z"),
        ("[!a-c]", "z"),
        ("[!a-c]", "b"),
    ];
    let mut output = std::io::stdout().lock();
    for (source, path) in cases {
        writeln!(output, "{}", glob::Pattern::new(source)?.matches(path))?;
    }
    Ok(())
}
"#,
    )?;
    let executable = scratch.path().join(if cfg!(windows) {
        "glob-reference.exe"
    } else {
        "glob-reference"
    });
    let external = format!("glob={}", library.display());
    let consumer_build = Command::new("rustc")
        .arg("--edition=2021")
        .arg(&consumer_source)
        .arg("--extern")
        .arg(external)
        .arg("-o")
        .arg(&executable)
        .output()?;
    assert!(
        consumer_build.status.success(),
        "pinned upstream consumer compile failed: {}",
        String::from_utf8_lossy(&consumer_build.stderr)
    );
    let reference = Command::new(executable).output()?;
    assert!(
        reference.status.success(),
        "pinned upstream consumer failed: {}",
        String::from_utf8_lossy(&reference.stderr)
    );
    let reference = String::from_utf8(reference.stdout)?
        .lines()
        .map(str::parse::<bool>)
        .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(
        ours, reference,
        "shared syntax subset agrees with vendored glob 0.3.4"
    );
    Ok(())
}
