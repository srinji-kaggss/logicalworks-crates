//! Sync-surface acceptance without the async engine.
//!
//! Compiled only under `--no-default-features`: proves the serializable spec
//! surface (`BotSpec` JSON round-trip) and the synchronous tick path survive
//! with no `tokio` in the tree.
//!
//! Passing this file does **not** exercise reactor integration, and saying so
//! is the point of keeping it separate: with no `rt` feature there is no timer
//! driver, no socket driver, and no runtime for `Bot::tick` to detect. What it
//! covers is the other half of the contract — a bot whose verbs await
//! reactor-free futures still runs a whole tick on the thread-parking executor,
//! and the crate's four verbs are not secretly runtime-dependent.
#![cfg(not(feature = "rt"))]

use std::cell::RefCell;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll};

use lgwks_bot::broker::Broker;
use lgwks_bot::effect::{EnvironmentId, FlowRevision, RunId};
use lgwks_bot::journal::MemoryJournal;
use lgwks_bot::spec::{ActionSpec, ChainSpec, EffectIdentity, EffectScope};
use lgwks_bot::{Auth, Bot, BotError, BotSpec, Cap, EffectLifetime, Execute, GrantSet, Observe};

/// The tests here cross `json::Error` and `BotError`, so they report
/// `Box<dyn Error>` and propagate each with `?`. A scope that cannot be built
/// reports through the same box for the same reason: the error domains it
/// crosses do not convert into one another, and flattening them would name the
/// wrong failure.
type TestResult = Result<(), Box<dyn std::error::Error>>;

/// The same box, for the one helper that is not a test body.
type ScopeResult<T> = Result<T, Box<dyn std::error::Error>>;

/// The value the source and the action both carry.
const VALUE: u32 = 1;

/// A future that yields once and then resolves, without a reactor.
///
/// This is the only shape available in a no-`rt` build: the crate ships no
/// executor of its own, so a verb future must complete on whatever executor
/// drives it, and this one does so by waking its own waker.
struct YieldOnce {
    /// Whether this future has returned `Pending` once already.
    yielded: bool,
    /// The value it resolves to.
    value: u32,
}

impl Future for YieldOnce {
    type Output = u32;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<u32> {
        let this = self.get_mut();
        if this.yielded {
            return Poll::Ready(this.value);
        }
        this.yielded = true;
        context.waker().wake_by_ref();
        Poll::Pending
    }
}

/// A source that awaits one reactor-free yield before yielding its value.
struct YieldSource;

impl Observe for YieldSource {
    type Output = u32;

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    async fn poll(&self, call: (Auth, ())) -> Result<u32, BotError> {
        call.0.check(Observe::required_caps(self))?;
        Ok(YieldOnce {
            yielded: false,
            value: VALUE,
        }
        .await)
    }

    fn domain_id(&self) -> &str {
        "test::yield_source"
    }
}

/// An action that awaits one reactor-free yield before recording its label.
struct YieldAction {
    /// The effects recorded so far, in the order they ran.
    log: Rc<RefCell<Vec<u32>>>,
}

impl Execute for YieldAction {
    type Input = u32;
    type Output = ();

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    fn effect_lifetime(&self) -> EffectLifetime {
        EffectLifetime::Local
    }

    async fn execute_action(&self, call: (Auth, &u32)) -> Result<(), BotError> {
        call.0.check(Execute::required_caps(self))?;
        let seen = YieldOnce {
            yielded: false,
            value: VALUE,
        }
        .await;
        self.log.borrow_mut().push(seen);
        Ok(())
    }

    fn domain_id(&self) -> &str {
        "test::yield_action"
    }
}

/// The effect scope a test's bot runs under.
///
/// A bot has no dispatch path without one: the scope names the run, the
/// environment the run acts on, and the journal a dispatch is written to before
/// it leaves the process. A fresh identity per call, so no test inherits
/// another's held attempts, and an in-memory journal, because these tests are
/// about a different boundary than durability is.
///
/// Returns `ScopeResult` because building a scope crosses `IdError` and
/// `BrokerError` as well as `BotError`, and none of the three converts into
/// another; the tests below report through `Box<dyn Error>` for the same reason.
fn test_effects() -> ScopeResult<EffectScope> {
    let environment = EnvironmentId::from_hex("2122232425262728292a2b2c2d2e2f30")?;
    let mut broker = Broker::new();
    broker.register(environment)?;
    Ok(EffectScope::new(
        EffectIdentity::new(
            RunId::from_hex("0102030405060708090a0b0c0d0e0f10")?,
            environment,
            FlowRevision::from_tagged(
                "blake3_256",
                "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
            )?,
        ),
        broker,
        Box::new(MemoryJournal::new()),
    ))
}

#[test]
fn no_default_features_preserve_the_sync_spec_surface() -> TestResult {
    // The specs are `#[non_exhaustive]`, so this consumer builds them through
    // `new` rather than a struct literal — the same path a downstream crate
    // must take.
    let spec = BotSpec::new(
        "minimal",
        vec![ChainSpec::new(
            "source::event",
            "owner/repo",
            vec![("changed".into(), ActionSpec::new("notify::log", "stdout"))],
        )],
    );

    let json = spec.to_json()?;
    let decoded = BotSpec::from_json(&json)?;
    assert_eq!(decoded.name, "minimal");
    assert_eq!(decoded.chains[0].on[0].0, "changed");
    Ok(())
}

#[test]
fn no_default_features_run_a_synchronous_tick() -> TestResult {
    let log = Rc::new(RefCell::new(Vec::new()));
    let mut bot = Bot::builder("no-default")
        .observe(YieldSource)
        .on(
            |seen: &u32| *seen == VALUE,
            YieldAction {
                log: Rc::clone(&log),
            },
        )
        .with_effects(test_effects()?)
        .build(&GrantSet::empty())?;

    let fired = bot.tick()?;
    assert_eq!(
        fired, 1,
        "the only chain's condition holds on the first tick"
    );
    assert_eq!(
        log.borrow().as_slice(),
        [VALUE],
        "the effect must have run exactly once, with the observed value"
    );

    let unchanged = bot.tick()?;
    assert_eq!(
        unchanged, 0,
        "the source's value did not move, so change detection must select nothing"
    );
    Ok(())
}
