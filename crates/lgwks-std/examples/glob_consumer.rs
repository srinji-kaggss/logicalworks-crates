//! Minimal executable consumer of the checked and migration glob APIs.

use std::error::Error;
use std::io::Write;

use lgwks_std::glob::{GlobDialect, GlobPattern, GlobScratch, PatternErrorKind};

fn main() -> Result<(), Box<dyn Error>> {
    let pattern = GlobPattern::compile("src/**/[a-z]?.rs")?;
    let mut scratch = GlobScratch::new();
    let root_match = pattern.is_match_with("src/a1.rs", &mut scratch);
    let nested_match = pattern.is_match_with("src/sub/a1.rs", &mut scratch);
    let malformed = GlobPattern::compile("[unclosed");
    let malformed_kind = malformed.err().map(|error| error.kind);
    let legacy = GlobPattern::compile_with_dialect("a**b", GlobDialect::Legacy)?;
    let legacy_match = legacy.is_match_with("a/x/b", &mut scratch);

    if !root_match
        || !nested_match
        || malformed_kind != Some(PatternErrorKind::UnclosedClass)
        || !legacy_match
    {
        return Err("glob public journey produced an unexpected result".into());
    }

    let mut output = std::io::stdout().lock();
    writeln!(
        output,
        "glob consumer: strict root/nested matches=true/true; malformed_class=rejected; legacy a**b match=true"
    )?;
    Ok(())
}
