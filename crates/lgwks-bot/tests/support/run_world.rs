//! A world bound to a run feed, for the run-event families (ADR 0006 R3).
//!
//! One definition of "a bot whose only work is the runs it is fed", shared by
//! `run_events` and `sim_run_events`, so the two families cannot drift into
//! testing different worlds. The bot has no chain: a tick fires only the bound
//! run system.

use std::cell::Cell;
use std::error::Error;
use std::rc::Rc;

use lgwks_bot::script::{FlowError, Scope};
use lgwks_bot::{Bot, GrantSet, RunFeed, SettledRun};

use crate::effects::memory_scope;

/// What a bound world fired, in firing order.
pub type Fired = Rc<Cell<Vec<SettledRun>>>;

/// A fresh world bound to `feed`, and the record of what it fires.
pub fn bound_world(feed: &RunFeed) -> Result<(Bot, Fired), Box<dyn Error>> {
    let mut bot = Bot::builder("run-world")
        .with_effects(memory_scope()?)
        .build(&GrantSet::empty())?;
    let fired: Fired = Rc::new(Cell::new(Vec::new()));
    let sink = Rc::clone(&fired);
    feed.bind(&mut bot, move |run| {
        let mut seen = sink.take();
        seen.push(run);
        sink.set(seen);
    });
    Ok((bot, fired))
}

/// A run body that fails when it is told to: the probe both families drive.
pub async fn fallible(_scope: Scope, fail: bool) -> Result<(), FlowError> {
    if fail {
        Err(FlowError::failed(format_args!(
            "the probe was told to fail"
        )))
    } else {
        Ok(())
    }
}

/// Bind `feed` to a fresh world, tick it once, and return what fired.
pub fn fire(feed: &RunFeed) -> Result<Vec<SettledRun>, Box<dyn Error>> {
    let (mut bot, fired) = bound_world(feed)?;
    bot.tick()?;
    Ok(fired.take())
}
