//! The detectors' verdicts, one fixture at a time.
//!
//! These are the cases the rules were written from: the `Option` and literal
//! false positives of #48, the shadowed fallback of #49, the commented log macro
//! of #46, and one passing and one failing example of each remaining rule. Each
//! fixture asserts the finding's evidence as well as its rule, and the controls
//! assert the absence of a finding. They moved with the detectors from
//! `lgwks_deps` (#408) and read through the same public entry point.

#![cfg(feature = "scan")]

use lgwks_ast::scan::{Hit, ScanError, scan_source};

/// The workspace forbids `unwrap`/`expect` outright, with no test
/// exemption, so a test propagates its failure with `?` rather than
/// aborting the run. The message is carried in the error rather than in an
/// added `.expect`.
type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Scans one fixture string.
///
/// Returns the `ScanError` rather than asserting it away: a fixture that
/// stops parsing must fail the test that uses it loudly, and a `Vec`
/// returned on the error path would make every `!contains` assertion below
/// pass for the wrong reason.
fn scan(text: &str) -> Result<Vec<Hit>, ScanError> {
    scan_source(text, "test.rs")
}

fn rules(hits: &[Hit]) -> Vec<&'static str> {
    hits.iter().map(|hit| hit.rule()).collect()
}

// ERROR-SWALLOW: the report is a claim about a lost *error*, so every
// fixture below asserts the finding's evidence as well as its rule, and
// the controls assert the absence of a finding — the direction issue #48
// was filed from. The two are one contract: `swallow` and `no_swallow`
// are the same predicate read at both ends.

/// The one `ERROR-SWALLOW` finding, with the evidence it carries.
fn swallow(hits: &[Hit]) -> Result<&Hit, Box<dyn std::error::Error>> {
    let mut found = hits.iter().filter(|hit| hit.rule() == "ERROR-SWALLOW");
    let hit = found
        .next()
        .ok_or_else(|| format!("expected an ERROR-SWALLOW finding, got {hits:?}"))?;
    assert!(found.next().is_none(), "expected one finding, got {hits:?}");
    Ok(hit)
}

/// Asserts a fixture produces no lost-error finding.
fn no_swallow(hits: &[Hit]) {
    let found: Vec<&Hit> = hits
        .iter()
        .filter(|hit| hit.rule() == "ERROR-SWALLOW")
        .collect();
    assert!(found.is_empty(), "unexpected ERROR-SWALLOW: {found:?}");
}

/// Issue #48, literal false-positive source 1. `Option` has an
/// `unwrap_or_default` of its own, where `None` means "use zero" and no
/// error was ever produced.
const OPTION_DEFAULT: &str = r#"pub fn default_count(value: Option<usize>) -> usize {
value.unwrap_or_default()
}
"#;

/// Issue #48, literal false-positive source 2. An integer literal cannot
/// carry an error, so binding it to `_` discards nothing.
const INFALLIBLE_WILDCARD: &str = r#"pub fn discard_number() {
let _ = 7usize;
}
"#;

/// A `Result` swallowed through the same spelling as source 1.
const RESULT_DEFAULT: &str = r#"pub fn read_count() -> Result<usize, E> { Ok(1) }

pub fn count() -> usize {
read_count().unwrap_or_default()
}
"#;

/// The same `Result` reached through an alias this file declares.
const ALIASED_RESULT_DEFAULT: &str = r#"pub type Outcome<T> = Result<T, E>;
pub fn read_count() -> Outcome<usize> { Ok(1) }

pub fn count() -> usize {
read_count().unwrap_or_default()
}
"#;

/// A `Result` bound to `_` and never inspected.
const DISCARDED_RESULT_VALUE: &str = r#"pub fn read_count() -> Result<usize, E> { Ok(1) }

pub fn warm() {
let _ = read_count();
}
"#;

/// A user-defined method with the same spelling as the standard one. Its
/// receiver resolves here to a type that is not a `Result`, which is
/// evidence *against* a lost error, not merely missing evidence.
const SAME_NAME_USER_METHOD: &str = r#"pub struct Counter { hits: usize }

impl Counter {
pub fn unwrap_or_default(&self) -> usize { self.hits }
}

pub fn count() -> usize {
let counter: Counter = Counter { hits: 0 };
counter.unwrap_or_default()
}
"#;

#[test]
fn option_default_is_not_a_lost_error() -> TestResult {
    no_swallow(&scan(OPTION_DEFAULT)?);
    Ok(())
}

#[test]
fn infallible_wildcard_binding_is_not_a_lost_error() -> TestResult {
    no_swallow(&scan(INFALLIBLE_WILDCARD)?);
    Ok(())
}

#[test]
fn option_binding_is_not_a_lost_error() -> TestResult {
    no_swallow(&scan(
        "pub fn f() -> Option<u8> {\n    let value: Option<u8> = None;\n    let _ = value;\n    value\n}\n",
    )?);
    Ok(())
}

#[test]
fn user_defined_same_name_method_is_not_a_lost_error() -> TestResult {
    no_swallow(&scan(SAME_NAME_USER_METHOD)?);
    Ok(())
}

#[test]
fn result_default_is_a_lost_error() -> TestResult {
    let hits = scan(RESULT_DEFAULT)?;
    let hit = swallow(&hits)?;
    assert_eq!(hit.line(), 4, "the finding lands on the call");
    assert!(
        hit.snippet()
            .contains("the `Result` returned by fn `read_count`"),
        "{hit:?}"
    );
    Ok(())
}

#[test]
fn aliased_result_default_is_a_lost_error() -> TestResult {
    let hits = scan(ALIASED_RESULT_DEFAULT)?;
    let hit = swallow(&hits)?;
    assert!(
        hit.snippet()
            .contains("the `Result` returned by fn `read_count`"),
        "an alias declared in this file resolves: {hit:?}"
    );
    Ok(())
}

#[test]
fn discarded_result_value_is_a_lost_error() -> TestResult {
    let hits = scan(DISCARDED_RESULT_VALUE)?;
    let hit = swallow(&hits)?;
    assert_eq!(hit.line(), 4);
    assert!(
        hit.snippet()
            .contains("binds the `Result` returned by fn `read_count`"),
        "{hit:?}"
    );
    Ok(())
}

#[test]
fn resolved_result_binding_is_a_lost_error() -> TestResult {
    let hits = scan(
        "pub fn read() -> Result<u8, E> { Ok(1) }\n\npub fn f() {\n    let outcome: Result<u8, E> = read();\n    let _ = outcome;\n}\n",
    )?;
    let hit = swallow(&hits)?;
    assert!(hit.snippet().contains("`let outcome: ..`"), "{hit:?}");
    Ok(())
}

#[test]
fn unresolved_receiver_is_reported_as_shape_evidence_only() -> TestResult {
    // The receiver's type is not stated in this file, so the only thing
    // the scanner can honestly say is that the shape is not one `Option`
    // has. That is not a proved defect, and the evidence says so rather
    // than claiming an error was found.
    let hits = scan("pub fn f(buffer: &Buffer) -> u8 { buffer.map_err(|_| 0) }\n")?;
    let hit = swallow(&hits)?;
    assert!(hit.snippet().contains("the call shape alone"), "{hit:?}");
    Ok(())
}

#[test]
fn resolved_non_result_receiver_is_never_reported() -> TestResult {
    // The same call with a receiver whose type *is* stated here: the
    // evidence now resolves, and it resolves against a lost error.
    let hits = scan(
        "pub struct Buffer { len: u8 }\n\nimpl Buffer {\n    pub fn map_err<F: Fn(u8) -> u8>(&self, f: F) -> u8 { f(self.len) }\n}\n\npub fn f() -> u8 {\n    let buffer: Buffer = Buffer { len: 0 };\n    buffer.map_err(|_| 0)\n}\n",
    )?;
    no_swallow(&hits);
    Ok(())
}

#[test]
fn wildcard_map_err_is_swallow() -> TestResult {
    let hits = scan(
        "pub fn read() -> Result<u8, E> { Ok(1) }\n\npub fn f() -> Result<(), E> {\n    read().map_err(|_| E::Flat)?;\n    Ok(())\n}\n",
    )?;
    let hit = swallow(&hits)?;
    assert!(
        hit.snippet().contains("the `Result` returned by fn `read`"),
        "{hit:?}"
    );
    Ok(())
}

#[test]
fn named_but_unread_map_err_is_swallow() -> TestResult {
    let hits = scan(
        "pub fn read() -> Result<u8, E> { Ok(1) }\n\npub fn f() -> Result<(), E> {\n    read().map_err(|cause| E::Flat)?;\n    Ok(())\n}\n",
    )?;
    assert!(swallow(&hits)?.snippet().contains("map_err("), "{hits:?}");
    Ok(())
}

#[test]
fn named_and_formatted_map_err_is_clean() -> TestResult {
    no_swallow(&scan(
        "pub fn read() -> Result<u8, E> { Ok(1) }\n\npub fn f() -> Result<(), E> {\n    read().map_err(|cause| E::Msg(format!(\"{cause}\")))?;\n    Ok(())\n}\n",
    )?);
    Ok(())
}

#[test]
fn discarded_ok_is_swallow() -> TestResult {
    let hits =
        scan("pub fn read() -> Result<u8, E> { Ok(1) }\n\npub fn f() {\n    read().ok();\n}\n")?;
    assert!(
        swallow(&hits)?
            .snippet()
            .contains("the `Result` returned by fn `read`"),
        "{hits:?}"
    );
    Ok(())
}

#[test]
fn option_or_else_with_no_argument_is_clean() -> TestResult {
    no_swallow(&scan(
        "pub fn f(value: Option<u8>) -> u8 {\n    value.unwrap_or_else(|| 0)\n}\n",
    )?);
    Ok(())
}

#[test]
fn tested_ok_is_clean() -> TestResult {
    no_swallow(&scan(
        "pub fn read() -> Result<u8, E> { Ok(1) }\n\npub fn f() -> bool {\n    read().is_ok()\n}\n",
    )?);
    Ok(())
}

#[test]
fn swallow_inside_test_fn_is_exempt() -> TestResult {
    let hits = scan("#[test]\nfn f() {\n    read().map_err(|_| E::Flat).unwrap();\n}\n")?;
    no_swallow(&hits);
    Ok(())
}

// Issue #49: a fallback closure's parameter counts as used only where the
// body reads that binding. Shadowing it, naming a same-spelled field, or
// mentioning it in a macro's text are not reads of the error.

/// Issue #49, literal missed-error source: the returned `Vec` is a *new*
/// binding, and the filesystem error is discarded unread.
const SHADOWED_FALLBACK: &str = r#"pub fn load(path: &str) -> Vec<u8> {
std::fs::read(path).unwrap_or_else(|_error| {
    let _error = Vec::new();
    _error
})
}
"#;

/// The control: identical handling, no shadowing.
const UNSHADOWED_FALLBACK: &str = r#"pub fn load(path: &str) -> Vec<u8> {
std::fs::read(path).unwrap_or_else(|_error| Vec::new())
}
"#;

#[test]
fn shadowed_error_binding_does_not_satisfy_the_use_check() -> TestResult {
    let hits = scan(SHADOWED_FALLBACK)?;
    let hit = swallow(&hits)?;
    assert_eq!(hit.line(), 2, "the finding lands on the fallback call");
    assert!(hit.snippet().contains("unwrap_or_else("), "{hit:?}");
    Ok(())
}

#[test]
fn unshadowed_named_fallback_is_reported_too() -> TestResult {
    let hits = scan(UNSHADOWED_FALLBACK)?;
    assert_eq!(swallow(&hits)?.line(), 2);
    Ok(())
}

#[test]
fn renamed_unrelated_local_does_not_clear_the_finding() -> TestResult {
    let hits = scan(
        "pub fn load(path: &str) -> Vec<u8> {\n    std::fs::read(path).unwrap_or_else(|_error| {\n        let scratch = Vec::new();\n        scratch\n    })\n}\n",
    )?;
    assert_eq!(swallow(&hits)?.line(), 2);
    Ok(())
}

#[test]
fn genuine_propagation_is_clean() -> TestResult {
    no_swallow(&scan(
        "pub fn load(path: &str) -> std::io::Result<Vec<u8>> {\n    let bytes = std::fs::read(path)?;\n    Ok(bytes)\n}\n",
    )?);
    Ok(())
}

#[test]
fn structured_diagnostic_use_is_clean() -> TestResult {
    no_swallow(&scan(
        "pub fn load(path: &str) -> Result<Vec<u8>, E> {\n    std::fs::read(path).map_err(|error| E::Read { source: error })\n}\n",
    )?);
    Ok(())
}

#[test]
fn nested_closure_capture_is_clean() -> TestResult {
    no_swallow(&scan(
        "pub fn load(path: &str) -> Result<Vec<u8>, E> {\n    std::fs::read(path).map_err(|error| {\n        let render = || format!(\"{error}\");\n        E::Msg(render())\n    })\n}\n",
    )?);
    Ok(())
}

#[test]
fn nested_closure_that_rebinds_the_name_is_a_swallow() -> TestResult {
    let hits = scan(
        "pub fn load(path: &str) -> E {\n    std::fs::read(path).map_err(|error| {\n        let render = |error: u8| error.to_string();\n        E::Flat(render(0))\n    })\n}\n",
    )?;
    assert_eq!(swallow(&hits)?.line(), 2);
    Ok(())
}

#[test]
fn same_spelled_field_is_not_a_read() -> TestResult {
    let hits = scan(
        "pub struct Failure { pub error: u8 }\n\npub fn load(path: &str, other: &Failure) -> E {\n    std::fs::read(path).map_err(|error| E::Flat(other.error))\n}\n",
    )?;
    assert_eq!(swallow(&hits)?.line(), 4);
    Ok(())
}

#[test]
fn unrelated_macro_string_is_not_a_read() -> TestResult {
    let hits = scan(
        "pub fn load(path: &str) -> E {\n    std::fs::read(path).map_err(|_error| E::flat(concat!(\"_error\")))\n}\n",
    )?;
    assert_eq!(swallow(&hits)?.line(), 2);
    Ok(())
}

#[test]
fn inline_format_capture_is_a_read() -> TestResult {
    no_swallow(&scan(
        "pub fn load(path: &str) -> E {\n    std::fs::read(path).map_err(|error| E::flat(format!(\"{error}\")))\n}\n",
    )?);
    Ok(())
}

#[test]
fn comment_mentioning_the_binding_is_not_a_read() -> TestResult {
    let hits = scan(
        "pub fn load(path: &str) -> E {\n    std::fs::read(path).map_err(|error| {\n        // the error binding is mentioned here and nowhere else\n        E::flat(\"error\")\n    })\n}\n",
    )?;
    assert_eq!(swallow(&hits)?.line(), 2);
    Ok(())
}

#[test]
fn explicit_drop_is_a_swallow() -> TestResult {
    let hits = scan(
        "pub fn load(path: &str) -> E {\n    std::fs::read(path).map_err(|error| {\n        drop(error);\n        E::Flat\n    })\n}\n",
    )?;
    assert_eq!(swallow(&hits)?.line(), 2);
    Ok(())
}

#[test]
fn stored_then_dropped_binding_is_a_swallow() -> TestResult {
    let hits = scan(
        "pub fn load(path: &str) -> E {\n    std::fs::read(path).map_err(|error| {\n        let _ = error;\n        E::Flat\n    })\n}\n",
    )?;
    assert_eq!(swallow(&hits)?.line(), 2);
    Ok(())
}

// unlogged-err-return: RED then GREEN.

#[test]
fn bare_err_return_without_log_is_flagged() -> TestResult {
    let hits = scan(
        "fn f(x: bool) -> Result<(), E> {\n    if x {\n        return Err(E::Flat);\n    }\n    Ok(())\n}\n",
    )?;
    assert!(rules(&hits).contains(&"unlogged-err-return"), "{hits:?}");
    Ok(())
}

#[test]
fn err_return_after_log_in_block_is_clean() -> TestResult {
    let hits = scan(
        "fn f(x: bool) -> Result<(), E> {\n    if x {\n        log::warn!(\"bad\");\n        return Err(E::Flat);\n    }\n    Ok(())\n}\n",
    )?;
    assert!(!rules(&hits).contains(&"unlogged-err-return"), "{hits:?}");
    Ok(())
}

#[test]
fn err_return_after_eprintln_is_clean() -> TestResult {
    let hits = scan(
        "fn f(x: bool) -> Result<(), E> {\n    if x {\n        eprintln!(\"bad\");\n        return Err(E::Flat);\n    }\n    Ok(())\n}\n",
    )?;
    assert!(!rules(&hits).contains(&"unlogged-err-return"), "{hits:?}");
    Ok(())
}

/// Lines the unlogged-error rule reports, ascending.
///
/// The line is asserted rather than the rule alone because the defect this
/// rule had was about *which* return it excused: a verdict that names the
/// wrong line, or names two where the program has one unobserved return,
/// is as wrong as a missing finding.
fn unlogged_lines(hits: &[Hit]) -> Vec<usize> {
    hits.iter()
        .filter(|hit| hit.rule() == "unlogged-err-return")
        .map(|hit| hit.line())
        .collect()
}

// The issue-46 counterexample, verbatim: a comment that names a log macro
// on the error path. The program emits nothing, so the return is
// unobserved. Prose is not an emitted diagnostic and cannot excuse it.
const COMMENTED_MARKER: &str = "pub fn validate(reject: bool) -> Result<(), &'static str> {\n    if reject {\n        // tracing::warn!(\"this is a comment, not an emitted event\");\n        return Err(\"denied\");\n    }\n    Ok(())\n}\n";

// The same program with only the comment removed. Only the comment line
// number differs between the two, so a verdict that changes between them
// changed it because prose was doing the work.
const UNCOMMENTED_MARKER: &str = "pub fn validate(reject: bool) -> Result<(), &'static str> {\n    if reject {\n        return Err(\"denied\");\n    }\n    Ok(())\n}\n";

#[test]
fn a_comment_naming_a_log_macro_is_not_evidence() -> TestResult {
    let commented = scan(COMMENTED_MARKER)?;
    assert_eq!(
        unlogged_lines(&commented),
        vec![4],
        "the return on line 4 emits nothing; a comment above it must not excuse it: {commented:?}"
    );
    let uncommented = scan(UNCOMMENTED_MARKER)?;
    assert_eq!(
        unlogged_lines(&uncommented),
        vec![3],
        "removing only a comment must not change the verdict, beyond the line shift: {uncommented:?}"
    );
    Ok(())
}

#[test]
fn a_string_literal_spelling_a_log_macro_is_not_evidence() -> TestResult {
    let hits = scan(
        "fn f(x: bool) -> Result<(), E> {\n    if x {\n        let hint = \"tracing::warn!(\";\n        return Err(E::Flat);\n    }\n    Ok(())\n}\n",
    )?;
    assert_eq!(
        unlogged_lines(&hits),
        vec![4],
        "a string that spells a log macro emits nothing: {hits:?}"
    );
    Ok(())
}

#[test]
fn a_log_on_the_untaken_branch_is_not_evidence() -> TestResult {
    let hits = scan(
        "fn f(x: bool) -> Result<(), E> {\n    if x {\n        tracing::warn!(\"the other arm ran\");\n    }\n    return Err(E::Flat);\n}\n",
    )?;
    assert_eq!(
        unlogged_lines(&hits),
        vec![5],
        "a log inside a branch the return is not on does not run before it: {hits:?}"
    );
    Ok(())
}

#[test]
fn a_log_statement_after_the_return_is_not_evidence() -> TestResult {
    let hits = scan(
        "fn f(x: bool) -> Result<(), E> {\n    if x {\n        return Err(E::Flat);\n        tracing::warn!(\"unreachable\");\n    }\n    Ok(())\n}\n",
    )?;
    assert_eq!(
        unlogged_lines(&hits),
        vec![3],
        "a log reached only after the return is not evidence for it: {hits:?}"
    );
    Ok(())
}

#[test]
fn a_log_before_a_return_nested_in_a_match_is_evidence() -> TestResult {
    let hits = scan(
        "fn f(x: Option<u32>) -> Result<(), E> {\n    tracing::warn!(\"adding context\");\n    match x {\n        Some(_) => return Err(E::Flat),\n        None => {}\n    }\n    Ok(())\n}\n",
    )?;
    assert!(
        unlogged_lines(&hits).is_empty(),
        "a log that runs unconditionally before the match runs before a return inside it: {hits:?}"
    );
    Ok(())
}

#[test]
fn a_log_inside_a_closure_body_is_evidence_for_that_body() -> TestResult {
    let hits = scan(
        "fn f() -> Result<(), E> {\n    let check = || {\n        tracing::warn!(\"bad\");\n        return Err(E::Flat);\n    };\n    check()\n}\n",
    )?;
    assert!(
        unlogged_lines(&hits).is_empty(),
        "the log is a statement of the closure's own block: {hits:?}"
    );
    Ok(())
}

/// Formatting is not evidence in either direction: the same two programs
/// written three ways must produce the same verdicts.
#[test]
fn reflowing_a_program_does_not_change_its_verdict() -> TestResult {
    let flagged = [
        "fn f(x: bool) -> Result<(), E> {\n    if x {\n        return Err(E::Flat);\n    }\n    Ok(())\n}\n",
        "fn f(x: bool) -> Result<(), E> {\n    if x\n    {\n        return\n            Err(E::Flat);\n    }\n    Ok(())\n}\n",
        "fn f(x: bool) -> Result<(), E> { if x { return Err(E::Flat); } Ok(()) }\n",
    ];
    for source in flagged {
        let hits = scan(source)?;
        assert_eq!(
            unlogged_lines(&hits).len(),
            1,
            "every wrapping of an unobserved return reports it exactly once: {source:?} -> {hits:?}"
        );
    }
    let clean = [
        "fn f(x: bool) -> Result<(), E> {\n    tracing::warn!(\"x\");\n    return Err(E::Flat);\n}\n",
        "fn f(x: bool) -> Result<(), E> {\n    tracing::warn!(\n        \"x\"\n    );\n    return Err(E::Flat);\n}\n",
        "fn f(x: bool) -> Result<(), E> { tracing::warn!(\"x\"); return Err(E::Flat); }\n",
    ];
    for source in clean {
        let hits = scan(source)?;
        assert!(
            unlogged_lines(&hits).is_empty(),
            "a wrapped log macro is still one statement: {source:?} -> {hits:?}"
        );
    }
    Ok(())
}

/// A `#[test]`-annotated function is exempt from this rule; the fixture
/// above is not, so the exemption cannot be what produced the finding.
#[test]
fn the_counterexample_is_reported_because_it_is_not_a_test_fn() -> TestResult {
    let source = format!("#[test]\n{COMMENTED_MARKER}");
    let hits = scan(&source)?;
    assert!(
        unlogged_lines(&hits).is_empty(),
        "the same body under #[test] is exempt: {hits:?}"
    );
    Ok(())
}

// ALLOW-SILENCE: RED then GREEN.

#[test]
fn clippy_allow_is_silence() -> TestResult {
    let hits = scan("#[allow(clippy::too_many_arguments)]\npub fn f() {}\n")?;
    assert_eq!(rules(&hits), vec!["ALLOW-SILENCE"]);
    Ok(())
}

/// A rustc lint silences exactly as a clippy lint does: the rule is about the
/// act of silencing, not about which tool owns the lint (#699).
#[test]
fn rustc_allow_is_silence() -> TestResult {
    let hits = scan("#[allow(dead_code)]\nstruct Unused;\n")?;
    assert_eq!(rules(&hits), vec!["ALLOW-SILENCE"], "{hits:?}");
    assert!(
        hits.first()
            .is_some_and(|hit| hit.snippet() == "#[allow(dead_code)]"),
        "the evidence names what was written: {hits:?}"
    );
    Ok(())
}

/// `expect` suppresses the emission the way `allow` does, and the evidence says
/// which level was written.
#[test]
fn expect_is_silence() -> TestResult {
    let hits = scan("#[expect(clippy::needless_return)]\nfn f() {}\n")?;
    assert_eq!(rules(&hits), vec!["ALLOW-SILENCE"], "{hits:?}");
    assert!(
        hits.first()
            .is_some_and(|hit| hit.snippet() == "#[expect(clippy::needless_return)]"),
        "{hits:?}"
    );
    Ok(())
}

/// A `reason` explains a silence; it does not stop it being one.
#[test]
fn a_reason_does_not_hide_the_lint() -> TestResult {
    let hits =
        scan("#[allow(unused_mut, reason = \"only moved on some platforms\")]\nfn f() {}\n")?;
    assert_eq!(rules(&hits), vec!["ALLOW-SILENCE"], "{hits:?}");
    assert!(
        hits.first()
            .is_some_and(|hit| hit.snippet() == "#[allow(unused_mut)]"),
        "the lint is named without the reason: {hits:?}"
    );
    Ok(())
}

/// A condition does not make a silence something else: `cfg_attr` applies an
/// attribute, and this one lowers a lint.
#[test]
fn a_cfg_attr_allow_is_silence() -> TestResult {
    let hits = scan("#[cfg_attr(target_os = \"linux\", allow(dead_code))]\nfn f() {}\n")?;
    assert_eq!(rules(&hits), vec!["ALLOW-SILENCE"], "{hits:?}");
    assert!(
        hits.first()
            .is_some_and(|hit| hit.snippet() == "#[cfg_attr(.., allow(dead_code))]"),
        "{hits:?}"
    );
    Ok(())
}

/// A tool lint and a group name are silences too, and a raised level is not.
#[test]
fn every_lint_path_silences_and_a_raised_level_does_not() -> TestResult {
    let silenced = scan(
        "#[allow(rustdoc::broken_intra_doc_links)]\nfn a() {}\n\n#[expect(warnings)]\nfn b() {}\n",
    )?;
    assert_eq!(
        rules(&silenced),
        vec!["ALLOW-SILENCE", "ALLOW-SILENCE"],
        "{silenced:?}"
    );
    let raised = scan("#[deny(dead_code)]\n#[warn(unused_variables)]\nfn c() {}\n")?;
    assert!(
        raised.is_empty(),
        "a raised lint is not a silence: {raised:?}"
    );
    Ok(())
}

/// The exemption is a scope, not a spelling: a helper inside a `#[cfg(test)]`
/// module is test code even when nothing marks the helper itself (#699).
#[test]
fn a_helper_inside_a_cfg_test_module_is_exempt() -> TestResult {
    let hits = scan(
        "#[cfg(test)]\nmod tests {\n    fn helper() -> Result<(), String> {\n        return Err(String::new());\n    }\n}\n",
    )?;
    assert!(
        !rules(&hits).contains(&"unlogged-err-return"),
        "an enclosing test module is test code: {hits:?}"
    );
    Ok(())
}

/// A module under a `cfg` that can hold outside a test is production code, so
/// its returns are judged.
#[test]
fn a_module_under_another_cfg_is_not_exempt() -> TestResult {
    let hits = scan(
        "#[cfg(unix)]\nmod platform {\n    fn helper() -> Result<(), String> {\n        return Err(String::new());\n    }\n}\n",
    )?;
    assert!(
        rules(&hits).contains(&"unlogged-err-return"),
        "code that ships is judged: {hits:?}"
    );
    Ok(())
}

// long-try-chain: RED then GREEN.

#[test]
fn four_try_in_one_statement_is_a_chain() -> TestResult {
    let hits = scan("fn f() -> Result<(), E> {\n    g()?.h()?.i()?.j()?;\n    Ok(())\n}\n")?;
    assert!(rules(&hits).contains(&"long-try-chain"), "{hits:?}");
    Ok(())
}

#[test]
fn three_try_in_one_statement_is_clean() -> TestResult {
    let hits = scan("fn f() -> Result<(), E> {\n    g()?.h()?.i()?;\n    Ok(())\n}\n")?;
    assert!(!rules(&hits).contains(&"long-try-chain"), "{hits:?}");
    Ok(())
}

#[test]
fn a_loop_is_not_charged_with_the_statements_in_its_body() -> TestResult {
    let hits = scan(
        "fn f() -> Result<(), E> {\n    for x in g()? {\n        let a = x.h()?;\n        let b = a.i()?;\n        b.j()?;\n        b.k()?;\n    }\n    Ok(())\n}\n",
    )?;
    assert!(!rules(&hits).contains(&"long-try-chain"), "{hits:?}");
    Ok(())
}

#[test]
fn a_chain_inside_a_loop_body_is_still_a_chain() -> TestResult {
    let hits =
        scan("fn f() -> Result<(), E> {\n    loop {\n        g()?.h()?.i()?.j()?;\n    }\n}\n")?;
    let hit = hits
        .iter()
        .find(|hit| hit.rule() == "long-try-chain")
        .ok_or("a four-`?` statement inside a loop is a chain")?;
    assert_eq!(
        hit.line(),
        3,
        "the chain is named at its own line, not the loop's"
    );
    Ok(())
}

#[test]
fn a_loop_header_still_counts_toward_its_own_statement() -> TestResult {
    let hits = scan(
        "fn f() -> Result<(), E> {\n    while g()?.h()?.i()?.j()? {\n        let a = k()?;\n    }\n    Ok(())\n}\n",
    )?;
    assert!(rules(&hits).contains(&"long-try-chain"), "{hits:?}");
    Ok(())
}

#[test]
fn try_in_let_is_not_counted() -> TestResult {
    let hits = scan(
        "fn f() -> Result<(), E> {\n    let a = g()?;\n    let b = a.h()?;\n    let c = b.i()?;\n    let d = c.j()?;\n    Ok(())\n}\n",
    )?;
    assert!(!rules(&hits).contains(&"long-try-chain"), "{hits:?}");
    Ok(())
}

// tautological-doc: RED then GREEN.

#[test]
fn paraphrase_doc_is_tautological() -> TestResult {
    let hits = scan("/// User\npub fn user() {}\n")?;
    assert!(rules(&hits).contains(&"tautological-doc"), "{hits:?}");
    Ok(())
}

#[test]
fn behavioral_doc_is_clean() -> TestResult {
    let hits = scan("/// Opens the vault, creating it when `create` is set.\npub fn user() {}\n")?;
    assert!(!rules(&hits).contains(&"tautological-doc"), "{hits:?}");
    Ok(())
}

// unlogged-err-return evidence: the log has to carry the error (#699).

/// What makes a log evidence for a return, and the lines that stay reported.
///
/// The issue's own example leads: `eprintln!("zero"); return Err(e);` emits a
/// word and says nothing about the error, so it is not evidence (#699). A log
/// that names the error — a format capture, a field, an argument — is, and so is
/// a log carrying context of its own beyond a bare message, which is how a
/// refusal is announced with the frame it happened at. An error carrying no
/// value has nothing to name, so the log on the path is the whole signal. A name
/// the error does not carry is not the error, and a mention inside a longer word
/// (`errors`) is not a mention of `e`.
const EVIDENCE_CASES: [(&str, &[usize]); 7] = [
    (
        "fn f(x: bool) -> Result<(), E> {\n    if x {\n        let e = E::Flat;\n        eprintln!(\"zero\");\n        return Err(e);\n    }\n    Ok(())\n}\n",
        &[5],
    ),
    (
        "fn f(x: bool) -> Result<(), E> {\n    if x {\n        let e = E::Flat;\n        tracing::error!(\"failed: {e}\");\n        return Err(e);\n    }\n    Ok(())\n}\n",
        &[],
    ),
    (
        "fn f(x: bool) -> Result<(), E> {\n    if x {\n        let e = E::Flat;\n        tracing::error!(%e, \"failed\");\n        return Err(e);\n    }\n    Ok(())\n}\n",
        &[],
    ),
    (
        "fn f() -> Result<(), E> {\n    eprintln!(\"zero\");\n    return Err(E::Flat);\n}\n",
        &[],
    ),
    (
        "fn f() -> Result<(), E> {\n    let other = 1_u32;\n    eprintln!(\"saw {other}\");\n    let e = E::Flat;\n    return Err(e);\n}\n",
        &[5],
    ),
    (
        "fn f() -> Result<(), E> {\n    let e = E::Flat;\n    eprintln!(\"no errors today\");\n    return Err(e);\n}\n",
        &[4],
    ),
    (
        "fn f(index: u64) -> Result<(), E> {\n    let cause = E::Flat;\n    tracing::debug!(index, \"the frame did not decode\");\n    return Err(cause);\n}\n",
        &[],
    ),
];

#[test]
fn the_log_must_carry_the_error() -> TestResult {
    for (source, expected) in EVIDENCE_CASES {
        let hits = scan(source)?;
        assert_eq!(unlogged_lines(&hits), expected, "{source:?} -> {hits:?}");
    }
    Ok(())
}

// Span lines land on real lines.

#[test]
fn finding_lines_match_source_lines() -> TestResult {
    let hits = scan(
        "fn a() {}\nfn b() -> Result<(), E> {\n    g().map_err(|_| E::Flat)?;\n    Ok(())\n}\n",
    )?;
    let hit = hits
        .iter()
        .find(|hit| hit.rule() == "ERROR-SWALLOW")
        .ok_or("a discarded-error closure must be reported")?;
    assert_eq!(hit.line(), 3);
    Ok(())
}
