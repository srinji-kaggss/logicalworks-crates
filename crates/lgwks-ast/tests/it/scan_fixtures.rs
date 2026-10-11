//! One fixture per rule edge for the zero-gate detectors (#408), through the
//! public `lgwks_ast::scan` API.
//!
//! These are the unit tests the `syn` scan carried, kept verbatim in what they
//! assert, so the tree-sitter port is held to every verdict the old reader
//! gave: issue #46 (prose is not a log), #48 (an `Option` default is not a lost
//! error), #49 (a shadowed or unread error binding is one), and the
//! threshold, exemption and placement cases of the three syntax-only rules.

#![cfg(feature = "scan")]

use std::error::Error;

use lgwks_ast::scan::{Hit, ScanError, scan_source};

/// What a fixture reports when a precondition did not hold.
type TestResult = Result<(), Box<dyn Error>>;

/// Scan one fixture, keeping the refusal: a fixture that stops parsing must
/// fail loudly, not pass every `!contains` below for the wrong reason.
fn scan(text: &str) -> Result<Vec<Hit>, ScanError> {
    scan_source(text, "test.rs")
}

/// The rule names a scan reported, in order.
fn rules(hits: &[Hit]) -> Vec<&'static str> {
    hits.iter().map(Hit::rule).collect()
}

/// The one ERROR-SWALLOW finding, with its evidence.
fn swallow(hits: &[Hit]) -> Result<&Hit, Box<dyn Error>> {
    let mut found = hits.iter().filter(|hit| hit.rule() == "ERROR-SWALLOW");
    let hit = found
        .next()
        .ok_or_else(|| format!("expected an ERROR-SWALLOW finding, got {hits:?}"))?;
    assert!(found.next().is_none(), "expected one finding, got {hits:?}");
    Ok(hit)
}

/// Asserts a fixture reports no lost error.
fn no_swallow(hits: &[Hit]) {
    let found: Vec<&Hit> = hits
        .iter()
        .filter(|hit| hit.rule() == "ERROR-SWALLOW")
        .collect();
    assert!(found.is_empty(), "unexpected ERROR-SWALLOW: {found:?}");
}

/// The lines the unlogged-error rule reported, ascending.
fn unlogged_lines(hits: &[Hit]) -> Vec<usize> {
    hits.iter()
        .filter(|hit| hit.rule() == "unlogged-err-return")
        .map(Hit::line)
        .collect()
}

// ── ERROR-SWALLOW (#48, #49) ────────────────────────────────────────────────

#[test]
fn option_default_is_not_a_lost_error() -> TestResult {
    no_swallow(&scan(
        "pub fn default_count(value: Option<usize>) -> usize {\n    value.unwrap_or_default()\n}\n",
    )?);
    Ok(())
}

#[test]
fn infallible_wildcard_binding_is_not_a_lost_error() -> TestResult {
    no_swallow(&scan(
        "pub fn discard_number() {\n    let _ = 7usize;\n}\n",
    )?);
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
    no_swallow(&scan(
        "pub struct Counter { hits: usize }\n\nimpl Counter {\n    pub fn unwrap_or_default(&self) -> usize { self.hits }\n}\n\npub fn count() -> usize {\n    let counter: Counter = Counter { hits: 0 };\n    counter.unwrap_or_default()\n}\n",
    )?);
    Ok(())
}

#[test]
fn result_default_is_a_lost_error() -> TestResult {
    let hits = scan(
        "pub fn read_count() -> Result<usize, E> { Ok(1) }\n\npub fn count() -> usize {\n    read_count().unwrap_or_default()\n}\n",
    )?;
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
    let hits = scan(
        "pub type Outcome<T> = Result<T, E>;\npub fn read_count() -> Outcome<usize> { Ok(1) }\n\npub fn count() -> usize {\n    read_count().unwrap_or_default()\n}\n",
    )?;
    assert!(
        swallow(&hits)?
            .snippet()
            .contains("the `Result` returned by fn `read_count`"),
        "an alias declared in this file resolves: {hits:?}"
    );
    Ok(())
}

#[test]
fn discarded_result_value_is_a_lost_error() -> TestResult {
    let hits = scan(
        "pub fn read_count() -> Result<usize, E> { Ok(1) }\n\npub fn warm() {\n    let _ = read_count();\n}\n",
    )?;
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
    assert!(
        swallow(&hits)?.snippet().contains("`let outcome: ..`"),
        "{hits:?}"
    );
    Ok(())
}

#[test]
fn unresolved_receiver_is_reported_as_shape_evidence_only() -> TestResult {
    let hits = scan("pub fn f(buffer: &Buffer) -> u8 { buffer.map_err(|_| 0) }\n")?;
    assert!(
        swallow(&hits)?.snippet().contains("the call shape alone"),
        "{hits:?}"
    );
    Ok(())
}

#[test]
fn resolved_non_result_receiver_is_never_reported() -> TestResult {
    no_swallow(&scan(
        "pub struct Buffer { len: u8 }\n\nimpl Buffer {\n    pub fn map_err<F: Fn(u8) -> u8>(&self, f: F) -> u8 { f(self.len) }\n}\n\npub fn f() -> u8 {\n    let buffer: Buffer = Buffer { len: 0 };\n    buffer.map_err(|_| 0)\n}\n",
    )?);
    Ok(())
}

#[test]
fn wildcard_map_err_is_swallow() -> TestResult {
    let hits = scan(
        "pub fn read() -> Result<u8, E> { Ok(1) }\n\npub fn f() -> Result<(), E> {\n    read().map_err(|_| E::Flat)?;\n    Ok(())\n}\n",
    )?;
    assert!(
        swallow(&hits)?
            .snippet()
            .contains("the `Result` returned by fn `read`"),
        "{hits:?}"
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
    no_swallow(&scan(
        "#[test]\nfn f() {\n    read().map_err(|_| E::Flat).unwrap();\n}\n",
    )?);
    Ok(())
}

#[test]
fn swallow_inside_cfg_test_module_is_exempt() -> TestResult {
    no_swallow(&scan(
        "#[cfg(all(test, unix))]\nmod tests {\n    fn f() {\n        read().map_err(|_| E::Flat).unwrap();\n    }\n}\n",
    )?);
    let hits = scan(
        "#[cfg(any(test, unix))]\nmod tests {\n    fn f() {\n        read().map_err(|_| E::Flat).unwrap();\n    }\n}\n",
    )?;
    assert_eq!(
        swallow(&hits)?.line(),
        4,
        "`any(test, unix)` holds outside test"
    );
    Ok(())
}

#[test]
fn shadowed_error_binding_does_not_satisfy_the_use_check() -> TestResult {
    let hits = scan(
        "pub fn load(path: &str) -> Vec<u8> {\n    std::fs::read(path).unwrap_or_else(|_error| {\n        let _error = Vec::new();\n        _error\n    })\n}\n",
    )?;
    let hit = swallow(&hits)?;
    assert_eq!(hit.line(), 2, "the finding lands on the fallback call");
    assert!(hit.snippet().contains("unwrap_or_else("), "{hit:?}");
    Ok(())
}

#[test]
fn unshadowed_named_fallback_is_reported_too() -> TestResult {
    let hits = scan(
        "pub fn load(path: &str) -> Vec<u8> {\n    std::fs::read(path).unwrap_or_else(|_error| Vec::new())\n}\n",
    )?;
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
fn shorthand_field_initializer_is_a_read() -> TestResult {
    no_swallow(&scan(
        "pub fn load(path: &str) -> Result<Vec<u8>, E> {\n    std::fs::read(path).map_err(|error| E::Read { error })\n}\n",
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

// ── unlogged-err-return (#46) ───────────────────────────────────────────────

#[test]
fn bare_err_return_without_log_is_flagged() -> TestResult {
    let hits = scan(
        "fn f(x: bool) -> Result<(), E> {\n    if x {\n        return Err(E::Flat);\n    }\n    Ok(())\n}\n",
    )?;
    assert_eq!(unlogged_lines(&hits), vec![3], "{hits:?}");
    Ok(())
}

#[test]
fn err_return_after_log_in_block_is_clean() -> TestResult {
    for log in ["log::warn!(\"bad\");", "eprintln!(\"bad\");"] {
        let hits = scan(&format!(
            "fn f(x: bool) -> Result<(), E> {{\n    if x {{\n        {log}\n        return Err(E::Flat);\n    }}\n    Ok(())\n}}\n"
        ))?;
        assert!(unlogged_lines(&hits).is_empty(), "{log}: {hits:?}");
    }
    Ok(())
}

#[test]
fn a_comment_naming_a_log_macro_is_not_evidence() -> TestResult {
    let commented = scan(
        "pub fn validate(reject: bool) -> Result<(), &'static str> {\n    if reject {\n        // tracing::warn!(\"this is a comment, not an emitted event\");\n        return Err(\"denied\");\n    }\n    Ok(())\n}\n",
    )?;
    assert_eq!(unlogged_lines(&commented), vec![4], "{commented:?}");
    let uncommented = scan(
        "pub fn validate(reject: bool) -> Result<(), &'static str> {\n    if reject {\n        return Err(\"denied\");\n    }\n    Ok(())\n}\n",
    )?;
    assert_eq!(unlogged_lines(&uncommented), vec![3], "{uncommented:?}");
    Ok(())
}

#[test]
fn a_string_literal_spelling_a_log_macro_is_not_evidence() -> TestResult {
    let hits = scan(
        "fn f(x: bool) -> Result<(), E> {\n    if x {\n        let hint = \"tracing::warn!(\";\n        return Err(E::Flat);\n    }\n    Ok(())\n}\n",
    )?;
    assert_eq!(unlogged_lines(&hits), vec![4], "{hits:?}");
    Ok(())
}

#[test]
fn a_log_on_the_untaken_branch_is_not_evidence() -> TestResult {
    let hits = scan(
        "fn f(x: bool) -> Result<(), E> {\n    if x {\n        tracing::warn!(\"the other arm ran\");\n    }\n    return Err(E::Flat);\n}\n",
    )?;
    assert_eq!(unlogged_lines(&hits), vec![5], "{hits:?}");
    Ok(())
}

#[test]
fn a_log_statement_after_the_return_is_not_evidence() -> TestResult {
    let hits = scan(
        "fn f(x: bool) -> Result<(), E> {\n    if x {\n        return Err(E::Flat);\n        tracing::warn!(\"unreachable\");\n    }\n    Ok(())\n}\n",
    )?;
    assert_eq!(unlogged_lines(&hits), vec![3], "{hits:?}");
    Ok(())
}

#[test]
fn a_log_before_a_return_nested_in_a_match_is_evidence() -> TestResult {
    let hits = scan(
        "fn f(x: Option<u32>) -> Result<(), E> {\n    tracing::warn!(\"adding context\");\n    match x {\n        Some(_) => return Err(E::Flat),\n        None => {}\n    }\n    Ok(())\n}\n",
    )?;
    assert!(unlogged_lines(&hits).is_empty(), "{hits:?}");
    Ok(())
}

#[test]
fn a_log_inside_a_closure_body_is_evidence_for_that_body() -> TestResult {
    let hits = scan(
        "fn f() -> Result<(), E> {\n    let check = || {\n        tracing::warn!(\"bad\");\n        return Err(E::Flat);\n    };\n    check()\n}\n",
    )?;
    assert!(unlogged_lines(&hits).is_empty(), "{hits:?}");
    Ok(())
}

#[test]
fn reflowing_a_program_does_not_change_its_verdict() -> TestResult {
    for source in [
        "fn f(x: bool) -> Result<(), E> {\n    if x {\n        return Err(E::Flat);\n    }\n    Ok(())\n}\n",
        "fn f(x: bool) -> Result<(), E> {\n    if x\n    {\n        return\n            Err(E::Flat);\n    }\n    Ok(())\n}\n",
        "fn f(x: bool) -> Result<(), E> { if x { return Err(E::Flat); } Ok(()) }\n",
    ] {
        let hits = scan(source)?;
        assert_eq!(unlogged_lines(&hits).len(), 1, "{source:?} -> {hits:?}");
    }
    for source in [
        "fn f(x: bool) -> Result<(), E> {\n    tracing::warn!(\"x\");\n    return Err(E::Flat);\n}\n",
        "fn f(x: bool) -> Result<(), E> {\n    tracing::warn!(\n        \"x\"\n    );\n    return Err(E::Flat);\n}\n",
        "fn f(x: bool) -> Result<(), E> { tracing::warn!(\"x\"); return Err(E::Flat); }\n",
    ] {
        let hits = scan(source)?;
        assert!(unlogged_lines(&hits).is_empty(), "{source:?} -> {hits:?}");
    }
    Ok(())
}

#[test]
fn the_counterexample_is_exempt_under_test() -> TestResult {
    let hits = scan(
        "#[test]\npub fn validate(reject: bool) -> Result<(), &'static str> {\n    if reject {\n        return Err(\"denied\");\n    }\n    Ok(())\n}\n",
    )?;
    assert!(unlogged_lines(&hits).is_empty(), "{hits:?}");
    Ok(())
}

// ── ALLOW-SILENCE, long-try-chain, tautological-doc ─────────────────────────

#[test]
fn clippy_allow_is_silence_and_rustc_allow_is_not() -> TestResult {
    let hits = scan("#[allow(clippy::too_many_arguments, dead_code)]\npub fn f() {}\n")?;
    assert_eq!(rules(&hits), vec!["ALLOW-SILENCE"]);
    assert_eq!(
        hits.first().map(Hit::snippet),
        Some("#[allow(clippy::too_many_arguments)]")
    );
    assert!(scan("#[allow(dead_code)]\nstruct Unused;\n")?.is_empty());
    Ok(())
}

#[test]
fn try_chain_threshold_and_let_exemption() -> TestResult {
    let four = scan("fn f() -> Result<(), E> {\n    g()?.h()?.i()?.j()?;\n    Ok(())\n}\n")?;
    assert_eq!(rules(&four), vec!["long-try-chain"], "{four:?}");
    let three = scan("fn f() -> Result<(), E> {\n    g()?.h()?.i()?;\n    Ok(())\n}\n")?;
    assert!(three.is_empty(), "{three:?}");
    let lets = scan(
        "fn f() -> Result<(), E> {\n    let a = g()?;\n    let b = a.h()?;\n    let c = b.i()?;\n    let d = c.j()?;\n    Ok(())\n}\n",
    )?;
    assert!(lets.is_empty(), "{lets:?}");
    Ok(())
}

#[test]
fn a_loop_is_judged_by_its_header_not_its_body() -> TestResult {
    let body = scan(
        "fn f() -> Result<(), E> {\n    for x in g()? {\n        let a = x.h()?;\n        let b = a.i()?;\n        b.j()?;\n        b.k()?;\n    }\n    Ok(())\n}\n",
    )?;
    assert!(body.is_empty(), "{body:?}");
    let inner =
        scan("fn f() -> Result<(), E> {\n    loop {\n        g()?.h()?.i()?.j()?;\n    }\n}\n")?;
    assert_eq!(inner.first().map(Hit::line), Some(3), "{inner:?}");
    let header = scan(
        "fn f() -> Result<(), E> {\n    while g()?.h()?.i()?.j()? {\n        let a = k()?;\n    }\n    Ok(())\n}\n",
    )?;
    assert_eq!(rules(&header), vec!["long-try-chain"], "{header:?}");
    Ok(())
}

#[test]
fn paraphrase_doc_is_tautological_and_behavior_is_not() -> TestResult {
    assert_eq!(
        rules(&scan("/// User\npub fn user() {}\n")?),
        vec!["tautological-doc"]
    );
    assert!(
        scan("/// Opens the vault, creating it when `create` is set.\npub fn user() {}\n")?
            .is_empty()
    );
    assert!(scan("/// User\npub(crate) fn user() {}\n")?.is_empty());
    Ok(())
}

#[test]
fn finding_lines_match_source_lines() -> TestResult {
    let hits = scan(
        "fn a() {}\nfn b() -> Result<(), E> {\n    g().map_err(|_| E::Flat)?;\n    Ok(())\n}\n",
    )?;
    assert_eq!(swallow(&hits)?.line(), 3);
    Ok(())
}

// ── Refusals and bounds ─────────────────────────────────────────────────────

#[test]
fn a_source_past_the_depth_ceiling_is_refused_not_scanned() {
    // `syn` recursed without a bound and aborted the gate on this shape.
    let nested = format!(
        "fn f() {{ let x = {}0{}; }}\n",
        "(".repeat(10_000),
        ")".repeat(10_000)
    );
    assert!(
        matches!(scan(&nested), Err(ScanError::Unparseable { ref path, .. }) if path == "test.rs"),
        "a hostile nesting is a refusal with its path"
    );
}
