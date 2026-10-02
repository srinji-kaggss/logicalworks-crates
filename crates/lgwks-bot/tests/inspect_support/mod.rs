//! Shared fixtures for the inspection wiring tests.
//!
//! Not a test target: `tests/inspect_support/mod.rs` is a module both
//! `inspect_wiring.rs` and `sim_inspect_wiring.rs` include with `mod
//! inspect_support;`. Keeping the registry, the capture action and the scope
//! factory here means the estate's inspection fixtures exist once rather than
//! being copied between the two targets.

// Each target uses a different subset of these fixtures.
#![allow(
    dead_code,
    reason = "each inspection wiring target uses a different subset of these fixtures"
)]

/// The deterministic simulation substrate, re-included so this module can build
/// a real effect scope without a second copy of the construction.
#[path = "../sim/mod.rs"]
pub mod sim;

use std::cell::RefCell;
use std::error::Error;
use std::rc::Rc;

use lgwks_bot::domain::inspect::Subject;
use lgwks_bot::inspect::Inspection;
use lgwks_bot::journal::MemoryJournal;
use lgwks_bot::spec::{Bot, BotSpec, EffectScope};
use lgwks_bot::{Action, Auth, BotError, Cap, EffectLifetime, Execute, GrantSet, domains};

/// A test result that may carry a `BotError`, an id error or an I/O error.
pub type TestResult<T> = Result<T, Box<dyn Error>>;

thread_local! {
    /// The last report an action received on this thread.
    pub static CAPTURED: RefCell<Option<Inspection>> = const { RefCell::new(None) };
}

/// Read and clear the captured report.
pub fn take_captured() -> TestResult<Inspection> {
    CAPTURED
        .with(|slot| slot.borrow_mut().take())
        .ok_or_else(|| "the capture action did not run".into())
}

/// Clear the capture before a run.
pub fn clear_captured() {
    CAPTURED.with(|slot| *slot.borrow_mut() = None);
}

/// An action that records the inspection it was handed.
pub struct Capture;

impl Capture {
    /// Build one from the `target` its spec names (ignored).
    pub fn from_target(target: &str) -> Result<Action, BotError> {
        let _ = target;
        Ok(Action::new(Self))
    }
}

impl Execute for Capture {
    type Input = Inspection;
    type Output = ();

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    fn effect_lifetime(&self) -> EffectLifetime {
        EffectLifetime::Local
    }

    async fn execute_action(&self, call: (Auth, &Inspection)) -> Result<(), BotError> {
        call.0.check(&[])?;
        CAPTURED.with(|slot| *slot.borrow_mut() = Some(call.1.clone()));
        Ok(())
    }

    fn domain_id(&self) -> &str {
        "test::capture"
    }
}

domains! {
    /// The registry these tests run: the shipped inspection source and a
    /// capture action.
    pub INSPECT_DOMAINS {
        observe {
            "inspect::subject" => Subject::from_target,
        }
        execute {
            "test::capture" => Capture::from_target,
        }
    }
}

/// A fresh effect scope over an in-memory journal.
///
/// The run identity and the probe journal come from the shared simulation rig,
/// so this adds no second copy of the estate's bot-scope construction.
pub fn scope() -> TestResult<EffectScope> {
    let store = Rc::new(RefCell::new(MemoryJournal::new()));
    sim::rig::scope(sim::rig::identity()?, store)
}

/// Clear the capture, tick `bot` once, and return the report it captured.
///
/// The one place a test ticks an inspection bot, so the "fire once, then read
/// what the action saw" sequence is not copied between targets.
pub fn tick_once(bot: &mut Bot) -> TestResult<Inspection> {
    clear_captured();
    assert_eq!(bot.tick()?, 1, "the one inspection chain fires exactly once");
    take_captured()
}

/// Build a native bot observing `artifact`, tick it once, and return the report
/// its action captured.
pub fn observe_once(artifact: &str, name: &str) -> TestResult<Inspection> {
    let mut bot = Bot::builder(name)
        .observe(Subject::at(artifact))
        .on(|_: &Inspection| true, Capture)
        .with_effects(scope()?)
        .build(&GrantSet::empty().grant(Cap::fs()))?;
    tick_once(&mut bot)
}

/// Escape `text` as a JSON string body.
pub fn json_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len().saturating_add(2));
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// A one-chain spec naming the inspection source and the capture action.
pub fn spec_for(artifact: &str) -> TestResult<BotSpec> {
    let json = format!(
        r#"{{"version":1,"name":"wired","chains":[
            {{"source":"inspect::subject","target":{target},
             "on":[["always",{{"domain":"test::capture","target":""}}]]}}]}}"#,
        target = json_string(artifact)
    );
    Ok(BotSpec::from_json(&json)?)
}

/// Write `subject` to a unique temp artifact and return its path string.
pub fn artifact_for(label: &str, subject: &str) -> TestResult<String> {
    let path = std::env::temp_dir().join(format!(
        "lgwks-inspect-wiring-{label}-{}.rs",
        std::process::id()
    ));
    std::fs::write(&path, subject)?;
    Ok(path.to_string_lossy().into_owned())
}
