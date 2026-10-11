# The program

Part of [Getting started with lgwks_bot](../getting-started.md).


```rust
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use lgwks_bot::{Auth, Bot, BotError, Cap, Execute, GrantSet, Observe};

/// The observed source. A real one reads a socket, a file, or an API; this one
/// reads a counter the caller owns, so the example is deterministic.
struct QueueDepth {
    depth: Arc<AtomicU32>,
}

impl Observe for QueueDepth {
    /// `PartialEq` is this executor's one extra bound: the condition here *is*
    /// change detection, so the observed value has to be comparable.
    type Output = u32;

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    async fn poll(&self, call: (Auth, ())) -> Result<u32, BotError> {
        call.0.check(self.required_caps())?;
        Ok(self.depth.load(Ordering::SeqCst))
    }

    fn domain_id(&self) -> &str {
        "queue::depth"
    }
}

/// The effect. Delivering a notification is what needs `bot.notify`, so that is
/// the capability the action declares.
struct Page {
    caps: Vec<Cap>,
    pages: Arc<AtomicU32>,
}

impl Execute for Page {
    type Input = u32;
    type Output = ();

    fn required_caps(&self) -> &[Cap] {
        &self.caps
    }

    async fn execute_action(&self, call: (Auth, &u32)) -> Result<(), BotError> {
        // Checked before the effect, not after it.
        call.0.check(self.required_caps())?;
        self.pages.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn domain_id(&self) -> &str {
        "notify::page"
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let depth = Arc::new(AtomicU32::new(3));
    let pages = Arc::new(AtomicU32::new(0));

    // The grant set is required at build, not only at run: a bot declaring a
    // capability it was not granted fails here.
    let grants = GrantSet::empty().grant(Cap::notify());

    let mut bot = Bot::builder("queue-watch")
        .observe(QueueDepth {
            depth: Arc::clone(&depth),
        })
        .on(|seen: &u32| *seen > 10, Page {
            caps: vec![Cap::notify()],
            pages: Arc::clone(&pages),
        })
        .build(&grants)?;

    // Tick one: depth is 3. A source with no remembered value counts as
    // changed, so the condition is evaluated; it is false, so nothing fires.
    let first = bot.tick()?;
    assert_eq!(first, 0, "depth 3 is under the threshold");
    assert_eq!(bot.revisions(), vec![1], "the first poll is a change");

    // Tick two: depth is unchanged. `Revision` does not move, so the condition
    // is not even re-evaluated.
    let second = bot.tick()?;
    assert_eq!(second, 0);
    assert_eq!(bot.revisions(), vec![1], "a held value is not a change");

    // Tick three: depth moves past the threshold, and the effect runs once.
    depth.store(11, Ordering::SeqCst);
    let third = bot.tick()?;
    assert_eq!(third, 1, "one action fired");
    assert_eq!(pages.load(Ordering::SeqCst), 1);

    // Tick four: depth is still 11. The condition is still true and the effect
    // does not run again, because nothing moved.
    let fourth = bot.tick()?;
    assert_eq!(fourth, 0, "a held value does not re-fire");
    assert_eq!(pages.load(Ordering::SeqCst), 1);

    println!("fired: {third}, revisions: {:?}", bot.revisions());
    Ok(())
}
```

It prints `fired: 1, revisions: [2]`.
