//! The edges of the detectors' verdicts: the shapes that tree-sitter spells
//! differently from the reader that scanned before it (#408), each pinned to the
//! verdict that reader gave the same file.
//!
//! Every fixture was scanned by both readers over the repository at the commit
//! the port started from, and the lines below are what both answered. The
//! vendored crates under `vendor/` and a sample of the crates.io registry were
//! scanned by both too, and the two agreed finding for finding.

#![cfg(feature = "scan")]

use std::error::Error;

use lgwks_ast::scan::scan_source;

type TestResult = Result<(), Box<dyn Error>>;

/// The lines `rule` reports for `source`, ascending.
fn lines(source: &str, rule: &str) -> Result<Vec<usize>, Box<dyn Error>> {
    Ok(scan_source(source, "case.rs")?
        .iter()
        .filter(|hit| hit.rule() == rule)
        .map(|hit| hit.line())
        .collect())
}

/// An attribute is part of the expression it is written on, so `#[cfg(..)] return Err(..);` is reported at the attribute's line, as the reader that scanned before the tree-sitter port counted it (vendored `syn` 2.0.119 `item.rs` carries this exact shape).
const ATTR_RETURN: &str = r#"fn f(x: bool) -> Result<(), E> {
    if x {
        #[cfg(feature = "printing")]
        return Err(E::Flat);
        #[cfg(not(feature = "printing"))]
        return Err(E::Other);
    }
    Ok(())
}
"#;

/// `cfg(test)` and `cfg(all(test, ..))` items are test code and skipped; `cfg(not(test))` and `cfg(any(test, unix))` can hold in a non-test build, so they are scanned (Rust reference, conditional compilation).
const CFG_TEST_MOD: &str = r#"pub fn read() -> Result<u8, E> { Ok(1) }

#[cfg(test)]
mod tests {
    fn helper() {
        super::read().ok();
    }
}

#[cfg(not(test))]
mod live {
    fn helper() {
        super::read().ok();
        let _ = read();
    }
}

#[cfg(all(test, feature = "extra"))]
mod gated {
    fn helper() {
        let _ = read();
    }
}

#[cfg(any(test, unix))]
fn maybe() {
    let _ = read();
}
"#;

/// Four `?` in one statement are a chain, including under an attribute (reported at the attribute) and as a block's tail expression; a `match` arm written without braces counts, a braced one is judged on its own statements.
const CHAIN: &str = r#"fn f() -> Result<(), E> {
    #[inline]
    a()?.b()?.c()?.d()?;
    let x = match a()? { 1 => b()?.c()?.d()?, _ => 0 };
    match a()?.b()? { 1 => 1, _ => 2 };
    if a()?.b()?.c()?.d()? { }
    loop { a()?.b()?.c()? ; }
    a()?.b()?.c()?.d()?
}
"#;

/// A doc is `///` lines, `/** */`, or `#[doc = ".."]`, in source order around other attributes; `pub(crate)` and trait items are not judged; seven content tokens are past the ceiling.
const DOC: &str = r#"/** user records */
pub fn user_records() {}

#[doc = "load config"]
pub fn load_config() {}

/// load config
#[inline]
pub fn load_config_again() {}

/// Load
/// config
pub fn load_config_two() {}

/// Load config
pub(crate) fn load_config_three() {}
/// load
/// load
/// load
/// load
/// load
/// load
/// load
pub fn load_seven() {}
impl X {
    /// build frame
    pub fn build_frame() {}
}
trait Y {
    /// build frame
    fn build_frame();
}
"#;

/// Reading the binding through a field, a path or a shadow is not reading the error (#49); a `ref` or `mut` parameter is a named one; a struct-literal shorthand and a bare argument are reads.
const MACROS: &str = r#"pub fn a(path: &str) -> E {
    std::fs::read(path).map_err(|error| { log!("{}", error.kind); E::Flat })
}
pub fn b(path: &str) -> E {
    std::fs::read(path).map_err(|error| { log!("{}", x::error); E::Flat })
}
pub fn c(path: &str) -> E {
    std::fs::read(path).map_err(|error| { log!("{:?}", &[error]); E::Flat })
}
pub fn d(path: &str) -> E {
    std::fs::read(path).map_err(|error| { log!("{error:?}"); E::Flat })
}
pub fn e(path: &str) -> E {
    std::fs::read(path).map_err(|error| { let error = 1; log!("{}", error); E::Flat })
}
pub fn f(path: &str) -> E {
    std::fs::read(path).map_err(|error| { match 1 { error => error } ; E::Flat })
}
pub fn g(path: &str) -> E {
    std::fs::read(path).map_err(|error| { if let Some(error) = Some(1) { error } else { 0 }; E::Flat })
}
pub fn h(path: &str) -> E {
    std::fs::read(path).map_err(|error| { for error in 0..1 { error; } E::Flat })
}
pub fn i(path: &str) -> E {
    std::fs::read(path).map_err(|error| { let _ = error; E::Flat })
}
pub fn j(path: &str) -> E {
    std::fs::read(path).map_err(|error| E::Wrap(error))
}
pub fn k(path: &str) -> E {
    std::fs::read(path).map_err(|ref error| E::Flat)
}
pub fn l(path: &str) -> E {
    std::fs::read(path).map_err(|mut error| E::Flat)
}
pub fn m(path: &str) -> E {
    std::fs::read(path).map_err(|error| E::Flat { error })
}
pub fn n(path: &str) -> E {
    std::fs::read(path).map_err(|error| error::wrap())
}
"#;

/// `Self::f` and `self::f` are the free function `f`; `crate::f` is a path through a module and does not resolve, while a standard routine resolves by its path even with a leading `::`.
const SELF_CALL: &str = r#"pub struct S;
fn read() -> Result<u8, E> { Ok(1) }
impl S {
    fn load(&self) -> u8 {
        Self::read().unwrap_or_default()
    }
    fn read() -> Result<u8, E> { Ok(1) }
}
pub fn free() {
    let _ = self::read();
    let _ = crate::read();
    let _ = ::std::fs::read("a");
}
"#;

/// A trait's default method is production code unless it is a test; a log in the function around a nested trait does not reach into it.
const TRAIT_DEFAULT: &str = r#"trait T {
    fn run(&self) -> Result<(), E> {
        return Err(E::Flat);
    }
    #[test]
    fn exempt(&self) -> Result<(), E> {
        return Err(E::Flat);
    }
}
fn outer() -> Result<(), E> {
    tracing::warn!("x");
    trait Inner {
        fn run(&self) -> Result<(), E> {
            return Err(E::Flat);
        }
    }
    Ok(())
}
"#;

/// `.map_err::<E, _>(|_| ..)` is the same call as without the turbofish; a typed or tuple parameter is a shape the detector does not accuse.
const TURBOFISH: &str = r#"pub fn read() -> Result<u8, E> { Ok(1) }

pub fn f() -> Result<u8, E> {
    read().map_err::<E, _>(|_| E::Flat)
}

pub fn g() -> u8 {
    read().unwrap_or_default()
}

pub fn h(path: &str) -> E {
    std::fs::read(path).map_err(|error: std::io::Error| E::Flat)
}

pub fn k(path: &str) -> E {
    std::fs::read(path).map_err(|(first, second)| E::Flat)
}
"#;

#[test]
fn a_return_that_carries_attributes_starts_at_its_first_attribute() -> TestResult {
    assert_eq!(
        lines(ATTR_RETURN, "unlogged-err-return")?,
        vec![3, 5],
        "attr return"
    );
    Ok(())
}

#[test]
fn cfg_test_modules_are_skipped_and_other_cfgs_are_not() -> TestResult {
    assert_eq!(
        lines(CFG_TEST_MOD, "ERROR-SWALLOW")?,
        vec![13, 14, 27],
        "cfg test mod"
    );
    Ok(())
}

#[test]
fn try_chains_count_headers_and_tails_and_not_blocks_or_lets() -> TestResult {
    assert_eq!(lines(CHAIN, "long-try-chain")?, vec![2, 6, 8], "chain");
    Ok(())
}

#[test]
fn paraphrase_docs_are_read_from_every_spelling_a_doc_takes() -> TestResult {
    assert_eq!(
        lines(DOC, "tautological-doc")?,
        vec![2, 5, 9, 13, 27],
        "doc"
    );
    Ok(())
}

#[test]
fn a_macro_reads_the_error_only_by_a_value_token_or_a_format_capture() -> TestResult {
    assert_eq!(
        lines(MACROS, "ERROR-SWALLOW")?,
        vec![5, 14, 17, 20, 23, 26, 32, 35, 41],
        "macros"
    );
    Ok(())
}

#[test]
fn self_qualified_calls_resolve_and_module_paths_do_not() -> TestResult {
    assert_eq!(
        lines(SELF_CALL, "ERROR-SWALLOW")?,
        vec![5, 10, 12],
        "self call"
    );
    Ok(())
}

#[test]
fn a_trait_default_method_is_scanned_and_a_test_helper_is_not() -> TestResult {
    assert_eq!(
        lines(TRAIT_DEFAULT, "unlogged-err-return")?,
        vec![3, 7],
        "trait default"
    );
    Ok(())
}

#[test]
fn a_turbofish_method_call_is_a_method_call() -> TestResult {
    assert_eq!(lines(TURBOFISH, "ERROR-SWALLOW")?, vec![4, 8], "turbofish");
    Ok(())
}
