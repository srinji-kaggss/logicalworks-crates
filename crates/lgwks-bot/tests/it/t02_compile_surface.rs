//! T02's compile surface: a mismatched typed chain and a cross-schema result
//! binding must be refused by the compiler, and the equivalent local
//! borrowed/non-`Send` program must compile *and* run.
//!
//! | Test | What it pins |
//! |---|---|
//! | `a_correct_local_borrowed_non_send_consumer_compiles` | the probe harness can compile a valid consumer, so a negative probe cannot be a missing-dependency failure in disguise |
//! | `a_source_condition_action_mismatch_is_refused` | the typed chain builder refuses a condition/action that do not read the source's output |
//! | `a_cross_schema_result_binding_is_refused` | binding a `Report<T>` from one schema into a `Report<U>` is a type error |
//! | `a_local_borrowed_non_send_body_runs_on_the_host` | an `Rc<RefCell<_>>`-holding body over a borrowed `&[u8]`, with an await in the middle, compiles and runs |
//!
//! # Why the probes are separate crates
//!
//! A compile-fail assertion cannot live in this test binary: this file must keep
//! compiling. So each negative case is type-checked as a real downstream
//! consumer by [`compile_probe`], against the workspace lockfile, and paired
//! with a positive probe that must compile cleanly — otherwise a probe that
//! failed for a missing dependency would read as the intended refusal.

#![cfg(feature = "script")]

use std::cell::RefCell;
use std::rc::Rc;

use lgwks_bot::script::{FlowError, Scope};
use lgwks_bot::task::{Disposition, Host, Report, task};

// The compile-probe harness, shared with `t22_process_surface`.
use crate::compile;

use compile::{assert_compiles, assert_refused_for, compile_probe};

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// The positive control: a consumer whose source, condition and action all read
/// the same type, holding non-`Send` state, must compile.
const POSITIVE_CONSUMER: &str = r#"
use std::cell::RefCell;
use std::rc::Rc;
use lgwks_bot::script::{FlowError, Scope};
use lgwks_bot::task::{Host, task};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::builder("probe")?.build()?;
    let seen: Rc<RefCell<u32>> = Rc::new(RefCell::new(0));
    let body_seen = Rc::clone(&seen);
    let work = task("count", move |_scope: Scope, bytes: &[u8]| {
        let len = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
        let seen = Rc::clone(&body_seen);
        async move {
            *seen.borrow_mut() = len;
            lgwks_bot::rt::task::yield_now().await;
            Ok::<u32, FlowError>(len)
        }
    })?;
    let report = lgwks_bot::block_on(host.run(&work, b"abc".as_slice()));
    assert!(report.disposition().is_success());
    Ok(())
}
"#;

/// A typed chain whose condition reads `String` against a source that yields
/// `u32`: the `.on` bound cannot hold.
const MISMATCHED_CHAIN: &str = r#"
use lgwks_bot::{Auth, Bot, BotError, Cap, EffectLifetime, Evaluate, Execute, Observe};

struct Source;
impl Observe for Source {
    type Output = u32;
    fn required_caps(&self) -> &[Cap] { &[] }
    async fn poll(&self, call: (Auth, ())) -> Result<u32, BotError> { call.0.check(&[])?; Ok(0) }
    fn domain_id(&self) -> &str { "probe::source" }
}

struct ReadsText;
impl Evaluate<String> for ReadsText {
    fn check(&self, _value: &String) -> Result<bool, BotError> { Ok(true) }
    fn condition_id(&self) -> &str { "probe::reads_text" }
}

struct WritesText;
impl Execute for WritesText {
    type Input = String;
    type Output = ();
    fn required_caps(&self) -> &[Cap] { &[] }
    fn effect_lifetime(&self) -> EffectLifetime { EffectLifetime::Local }
    async fn execute_action(&self, call: (Auth, &String)) -> Result<(), BotError> {
        call.0.check(&[])?;
        Ok(())
    }
    fn domain_id(&self) -> &str { "probe::writes_text" }
}

fn main() {
    let _builder = Bot::builder("probe").observe(Source).on(ReadsText, WritesText);
}
"#;

/// A task that yields a `String` bound into a `Report<u32>`: the two schemas do
/// not line up.
const CROSS_SCHEMA_BINDING: &str = r#"
use lgwks_bot::script::{FlowError, Scope};
use lgwks_bot::task::{Host, Report, task};

fn host() -> Result<Host, Box<dyn std::error::Error>> {
    Ok(Host::builder("probe")?.build()?)
}

async fn bind(host: &Host) -> Result<(), Box<dyn std::error::Error>> {
    let text = task("text", |_scope: Scope, _: ()| async {
        Ok::<_, FlowError>(String::from("x"))
    })?;
    let _bound: Report<u32> = host.run(&text, ()).await;
    Ok(())
}

fn main() {
    let _ = host;
    let _ = bind;
}
"#;

/// A valid consumer must compile, so a neighbouring negative probe that failed
/// for a missing dependency cannot be mistaken for the intended refusal.
#[test]
fn a_correct_local_borrowed_non_send_consumer_compiles() -> TestResult {
    let output = compile_probe("t02-positive", "", POSITIVE_CONSUMER)?;
    assert_compiles(&output);
    Ok(())
}

/// A source/condition/action type mismatch in the typed chain builder is a
/// compile error, not a downcast miss at tick time.
#[test]
fn a_source_condition_action_mismatch_is_refused() -> TestResult {
    let output = compile_probe("t02-chain-mismatch", "", MISMATCHED_CHAIN)?;
    // The condition reads `String` while the source yields `u32`, so the
    // `Evaluate<S::Output>` bound is unsatisfied: E0277 naming the trait.
    assert_refused_for(&output, "E0277", "Evaluate");
    Ok(())
}

/// Binding a result from one schema into another is a type error.
#[test]
fn a_cross_schema_result_binding_is_refused() -> TestResult {
    let output = compile_probe("t02-cross-schema", "", CROSS_SCHEMA_BINDING)?;
    // `host.run` yields `Report<String>`; the annotation asks for `Report<u32>`.
    assert_refused_for(&output, "E0308", "Report");
    Ok(())
}

/// The equivalent correct program — an `Rc<RefCell<_>>` body over a borrowed
/// `&[u8]`, with a real await crossing the `Rc` — compiles and runs on the host.
#[test]
fn a_local_borrowed_non_send_body_runs_on_the_host() -> TestResult {
    let host = Host::builder("acme")?.build()?;
    let seen: Rc<RefCell<Vec<u32>>> = Rc::new(RefCell::new(Vec::new()));
    let body_seen = Rc::clone(&seen);

    let work = task("sum-bytes", move |_scope: Scope, bytes: &[u8]| {
        // Read the borrow synchronously; the future below owns what it needs and
        // keeps the `Rc` alive across its await, which makes it non-`Send`.
        let values: Vec<u32> = bytes.iter().map(|byte| u32::from(*byte)).collect();
        let seen = Rc::clone(&body_seen);
        async move {
            seen.borrow_mut().push(
                u32::try_from(values.len())
                    .map_err(|_| FlowError::failed("more bytes than a u32 can name"))?,
            );
            lgwks_bot::rt::task::yield_now().await;
            let sum = values.iter().copied().sum::<u32>();
            seen.borrow_mut().push(sum);
            Ok::<u32, FlowError>(sum)
        }
    })?;

    let input: Vec<u8> = vec![1, 2, 3];
    let report: Report<u32> = lgwks_bot::block_on(host.run(&work, input.as_slice()));
    assert_eq!(
        report.disposition(),
        Disposition::Succeeded,
        "a non-Send body over a borrowed input runs: {:?}",
        report.error()
    );
    assert_eq!(report.output().copied(), Some(6), "the bytes were summed");
    assert_eq!(
        *seen.borrow(),
        vec![3, 6],
        "the `Rc` the body kept across its await is the caller's own"
    );
    Ok(())
}
