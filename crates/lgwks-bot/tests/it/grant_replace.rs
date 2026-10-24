//! Live revocation: `EcsBot::replace_grants` as an executable contract (#716).
//!
//! Withdrawing the *caller's* set never narrows a built bot (`authority.rs`
//! pins that). Narrowing the bot itself is `Bot::replace_grants`, which swaps
//! the retained set the framework mints every per-call proof from. Both the
//! swap and the ticks it separates take `&mut self`, so no tick observes a
//! half-swapped set, and the swap reports what it withdrew and added for the
//! audit trail. Proofs already minted keep covering what they were minted for
//! until their lifetimes lapse; only subsequent mints read the new set.
//!
//! Five tests carry the journey:
//!
//! - [`replacing_grants_narrows_a_built_bot_on_its_next_tick`] is the issue's
//!   journey: a firing bot is narrowed mid-run and its very next tick is
//!   denied naming the withdrawn capability, with the effect never running
//!   without it.
//! - [`replacing_grants_with_an_identical_set_changes_nothing`] is the no-op
//!   control: an identical swap reports empty lists and the bot fires on.
//! - [`replacing_grants_reports_added_capabilities`] pins the added half of
//!   the report, sorted, with the bot still polling under the wider set.
//! - [`replacing_grants_on_one_bot_leaves_another_bot_untouched`] is the
//!   isolation half: authority is per bot, never a shared handle.
//! - [`a_proof_minted_before_replacement_keeps_covering`] pins the credential
//!   boundary: a proof in hand survives the swap while subsequent mints do
//!   not.
//!
//! Split from `authority.rs`, whose size ceiling this journey would break. The
//! doubles are owned there — [`Script`], [`Counted`] and [`test_effects`] are
//! `pub(crate)` for exactly this file — so the two files share one meaning of
//! "a bot that needs `bot.net`".

use std::cell::Cell;
use std::error::Error;
use std::rc::Rc;

use lgwks_bot::{Bot, BotError, Cap, GrantSet};

use crate::authority::{Counted, Script, test_effects};

/// What a test here reports when its precondition did not hold.
type TestResult = Result<(), Box<dyn Error>>;

#[test]
fn replacing_grants_narrows_a_built_bot_on_its_next_tick() -> TestResult {
    let counter = Rc::new(Cell::new(0));
    let mut bot = Bot::builder("narrowed")
        .observe(Script::new(vec![100, 200, 503]))
        .on(|value: &u16| *value >= 500, Counted(Rc::clone(&counter)))
        .with_effects(test_effects()?)
        .build(&GrantSet::empty().grant(Cap::net()))?;

    assert_eq!(bot.tick()?, 0, "100 is below the threshold");
    assert_eq!(bot.tick()?, 0, "200 is below the threshold");

    let report = bot.replace_grants(&GrantSet::empty());
    assert_eq!(
        report.withdrawn().to_vec(),
        vec![Cap::net()],
        "the swap reports the capability it withdrew"
    );
    assert!(
        report.added().is_empty(),
        "nothing was granted: {:?}",
        report.added()
    );

    match bot.tick() {
        Err(BotError::CapabilityDenied { deficit }) => {
            assert_eq!(
                deficit.first().required(),
                &Cap::net(),
                "the denial names the withdrawn capability"
            );
        }
        other => {
            return Err(format!(
                "a tick after the grant was withdrawn must be denied, got {other:?}"
            )
            .into());
        }
    }
    assert_eq!(
        counter.get(),
        0,
        "the effect never ran without its capability"
    );
    Ok(())
}

#[test]
fn replacing_grants_with_an_identical_set_changes_nothing() -> TestResult {
    let counter = Rc::new(Cell::new(0));
    let grants = GrantSet::empty().grant(Cap::net());
    let mut bot = Bot::builder("steady")
        .observe(Script::new(vec![503]))
        .on(|value: &u16| *value >= 500, Counted(Rc::clone(&counter)))
        .with_effects(test_effects()?)
        .build(&grants)?;

    let report = bot.replace_grants(&grants);
    assert!(
        report.withdrawn().is_empty() && report.added().is_empty(),
        "an identical swap reports no change: {:?}",
        report
    );
    assert_eq!(bot.tick()?, 1, "the bot still fires under the same grants");
    assert_eq!(counter.get(), 1, "the effect ran");
    Ok(())
}

#[test]
fn replacing_grants_reports_added_capabilities() -> TestResult {
    let mut bot = Bot::builder("widened")
        .observe(Script::new(vec![100]))
        .on(|value: &u16| *value >= 500, Counted(Rc::new(Cell::new(0))))
        .with_effects(test_effects()?)
        .build(&GrantSet::empty().grant(Cap::net()))?;

    let report = bot.replace_grants(&GrantSet::all_shipped());
    assert!(
        report.withdrawn().is_empty(),
        "nothing was withdrawn: {:?}",
        report.withdrawn()
    );
    assert_eq!(
        report.added().to_vec(),
        vec![Cap::fs(), Cap::notify(), Cap::sys()],
        "the swap reports every added capability, sorted"
    );
    assert_eq!(bot.tick()?, 0, "the bot still polls under the wider set");
    Ok(())
}

#[test]
fn replacing_grants_on_one_bot_leaves_another_bot_untouched() -> TestResult {
    let mut first = Bot::builder("first")
        .observe(Script::new(vec![503]))
        .on(|value: &u16| *value >= 500, Counted(Rc::new(Cell::new(0))))
        .with_effects(test_effects()?)
        .build(&GrantSet::empty().grant(Cap::net()))?;
    let mut second = Bot::builder("second")
        .observe(Script::new(vec![503]))
        .on(|value: &u16| *value >= 500, Counted(Rc::new(Cell::new(0))))
        .with_effects(test_effects()?)
        .build(&GrantSet::empty().grant(Cap::net()))?;

    let report = first.replace_grants(&GrantSet::empty());
    assert_eq!(
        report.withdrawn().to_vec(),
        vec![Cap::net()],
        "the first bot lost its capability"
    );
    assert!(
        second
            .replace_grants(&GrantSet::empty().grant(Cap::net()))
            .withdrawn()
            .is_empty(),
        "the second bot holds its own set: an identical swap reports nothing"
    );
    assert!(
        first.tick().is_err(),
        "the narrowed bot is denied on its next tick"
    );
    assert_eq!(
        second.tick()?,
        1,
        "the untouched bot still fires: authority is per bot, never shared"
    );
    Ok(())
}

#[test]
fn a_proof_minted_before_replacement_keeps_covering() -> TestResult {
    let grants = GrantSet::empty().grant(Cap::net());
    let issued = grants.issue(&[Cap::net()])?;
    let mut bot = Bot::builder("credential")
        .observe(Script::new(vec![100]))
        .on(|value: &u16| *value >= 500, Counted(Rc::new(Cell::new(0))))
        .with_effects(test_effects()?)
        .build(&grants)?;

    let report = bot.replace_grants(&GrantSet::empty());
    assert_eq!(
        report.withdrawn().to_vec(),
        vec![Cap::net()],
        "the swap withdrew the capability"
    );
    assert!(
        issued.check(&[Cap::net()]).is_ok(),
        "a proof in hand is judged by what it carries, not by the set that minted it"
    );
    assert!(
        bot.tick().is_err(),
        "while subsequent mints read the narrowed set and are denied"
    );
    Ok(())
}
