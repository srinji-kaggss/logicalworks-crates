//! The complete first-run demo: every guarantee the README names, firing.
//!
//! `examples/quickstart.rs` is the README-pinned snippet, so it stays small:
//! it shows the retry guarantee alone. This example runs that guarantee
//! again on different content and adds the rest of the first-run story,
//! each demo printed and asserted, deterministically, with no real wait:
//! the deadline overrun on a virtual clock, the capability refusal naming
//! the missing grant, and the kill-and-resume across a modelled crash.
//!
//! The run id is not a caller-supplied literal: a host with a run store mints
//! one for `run()` — that is the `ephemeral` feature, which is why this
//! example declares `required-features = ["script", "ephemeral"]` — and
//! `Host::resume` adopts the minted id from the report. Resume a run id the
//! store has never recorded and the store refuses it (`StoreError::UnknownRun`).
//!
//! ```sh
//! cargo run --locked -p lgwks_bot --features ephemeral --example guarantees
//! ```

use std::error::Error;
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use lgwks_bot::script::{Clock, FlowError, Scope, Tenant, remember};
use lgwks_bot::task::{Disposition, Host, task};
use lgwks_bot::{Action, Auth, BotError, Cap, EffectLifetime, Execute, GrantSet, domains};

/// A stand-in for a record store having a bad minute: the first two reads
/// answer "busy", later reads succeed. The read count lives on the store,
/// so the flow reaches it through a reference, the way a real client holds
/// a handle.
struct Store {
    /// The number of reads attempted so far.
    reads: AtomicU8,
}

impl Store {
    /// Read one record: two refusals, then the value.
    async fn fetch(&self, key: u64) -> Result<u64, FlowError> {
        let attempt = self.reads.fetch_add(1, Ordering::Relaxed);
        match attempt {
            0 | 1 => Err(FlowError::transient("the record store was busy")),
            _ => Ok(key.saturating_mul(10)),
        }
    }
}

/// The `demo::publish` action. It needs the `demo.publish` capability, which
/// the demo host never grants, so the run is blocked at admission — before
/// any step executes — and the refusal names the capability.
struct Publish {
    /// The capabilities this action requires.
    needs: Vec<Cap>,
}

impl Publish {
    /// Build the action the `demo::publish` registry entry resolves to.
    fn from_target(_target: &str) -> Result<Action, BotError> {
        Ok(Action::new(Self {
            needs: vec![Cap::new("demo.publish")],
        }))
    }
}

impl Execute for Publish {
    type Input = &'static str;
    type Output = ();

    fn required_caps(&self) -> &[Cap] {
        &self.needs
    }

    fn effect_lifetime(&self) -> EffectLifetime {
        EffectLifetime::External
    }

    async fn execute_action(&self, call: (Auth, &&'static str)) -> Result<(), BotError> {
        call.0.check(&self.needs)?;
        Ok(())
    }

    fn domain_id(&self) -> &str {
        "demo::publish"
    }
}

domains! {
    /// The demo registry: one execute action, no observe sources.
    DEMO_DOMAINS {
        observe {}
        execute {
            "demo::publish" => Publish::from_target,
        }
    }
}

lgwks_bot::script! {
    /// Read one record by key. A busy store is worth retrying: the same
    /// read again, each attempt inside its own one-second budget.
    flow fetch(store: &Store, key: u64) -> u64:
        retry up to 3 times, waiting 5ms:
            within 1s:
                store.fetch(key).await?

    /// A body that spends more than its `within` budget: the virtual clock
    /// jumps two seconds inside a one-second budget, so the run is cancelled
    /// at the `slow_square/within` site and reports `TimedOut`.
    flow slow_square(number: u64) -> u64:
        within 1s:
            scope.clock().advance(Duration::from_secs(2)).or_fail()?
            number.saturating_mul(number)

    /// Publish through the gated action. The host grants nothing, so the run
    /// is blocked before any step executes and the refusal names the
    /// capability.
    flow publish(text: &'static str):
        act demo::publish on "demo" with text
}

/// Demo one: the retry guarantee. Two transient refusals, then success —
/// three attempts under one key, about 10 ms of real backoff.
fn retry_demo() -> Result<(), Box<dyn Error>> {
    let mut out = std::io::stdout().lock();
    let store = Store {
        reads: AtomicU8::new(0),
    };
    let scope = Scope::root(Tenant::new("demo")?);
    let total = lgwks_bot::block_on(fetch(&scope, &store, 3))?;
    writeln!(out, "fetched record: {total}")?;
    assert_eq!(total, 30, "key 3 answers 30 after two transient refusals");
    assert_eq!(
        store.reads.load(Ordering::Relaxed),
        3,
        "three reads, not more"
    );
    Ok(())
}

/// Demo two: the deadline overrun. The virtual clock jumps past the `within`
/// budget, so the run reports `TimedOut` naming the site, with no real wait.
fn deadline_demo() -> Result<(), Box<dyn Error>> {
    let mut out = std::io::stdout().lock();
    let scope = Scope::with_clock(Tenant::new("demo")?, Clock::virtual_at(Duration::ZERO));
    match lgwks_bot::block_on(slow_square(&scope, 7)) {
        Err(error @ FlowError::TimedOut { .. }) => {
            writeln!(out, "overrun: {error}")?;
            assert!(
                error.to_string().contains("slow_square/within"),
                "the overrun names the site that timed out"
            );
        }
        ref other => {
            lgwks_std::trace::debug!(?other, "expected a timed-out run");
            return Err(format!("expected a timed-out run, got {other:?}").into());
        }
    }
    Ok(())
}

/// The demo host: one tenant, no grants, a run store in a fresh temporary
/// directory named by a random nonce — an OS process id is reused, a CSPRNG
/// draw is not — so concurrent demo runs never share one. Built once and
/// shared by the refusal and resume demos.
fn demo_host() -> Result<Host, Box<dyn Error>> {
    let nonce = lgwks_std::random::bytes::<8>()?;
    let store = std::env::temp_dir().join(format!(
        "lgwks-quickstart-store-{}",
        u64::from_le_bytes(nonce)
    ));
    Ok(Host::builder("demo")?
        .domains(&DEMO_DOMAINS)
        .grants(GrantSet::empty())
        .run_store(&store)?
        .build()?)
}

/// Demo three: the capability refusal. The run needs `demo.publish`, the host
/// grants nothing, and the block happens at admission — before any step
/// executes — so the refusal names the capability and nothing ran.
fn refusal_demo(host: &Host) -> Result<(), Box<dyn Error>> {
    let mut out = std::io::stdout().lock();
    let publishing = task("publish", |scope: Scope, text: &'static str| async move {
        publish(&scope, text).await
    })?;
    let report = lgwks_bot::block_on(host.run(&publishing, "demo"));
    let refusal = match report.result() {
        Err(refusal) => Ok(refusal),
        Ok(_) => Err(format!(
            "expected a blocked run, got {:?}",
            report.disposition()
        )),
    };
    let need = match *refusal? {
        FlowError::Blocked { ref deficit, .. } => deficit
            .shortages()
            .map(|short| short.required().clone())
            .collect::<Vec<Cap>>(),
        ref other => {
            lgwks_std::trace::debug!(?other, "expected a blocked run");
            return Err(format!("expected a blocked run, got {other:?}").into());
        }
    };
    assert_eq!(
        need,
        vec![Cap::new("demo.publish")],
        "the refusal names the capability"
    );
    let named = need
        .iter()
        .map(|cap| cap.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    writeln!(out, "refused: the run needs {named}")?;
    Ok(())
}

/// Demo four: the kill-and-resume. The body records two durable steps and
/// models a crash between them; the run ends `Cancelled` with one recorded
/// step, the report carries the minted run id, and resuming that id replays
/// the recorded step and finishes the run.
fn resume_demo(host: &Host) -> Result<(), Box<dyn Error>> {
    let mut out = std::io::stdout().lock();
    let resuming = task("resuming", |scope: Scope, crash: bool| async move {
        let one = remember(&scope, "step-one", || async { Ok::<u64, FlowError>(1) }).await?;
        if crash {
            lgwks_std::trace::debug!("modelled crash between the two recorded steps");
            return Err(FlowError::Cancelled {
                at: Arc::from("step-two"),
            });
        }
        let two = remember(&scope, "step-two", || async { Ok::<u64, FlowError>(2) }).await?;
        one.checked_add(two)
            .ok_or_else(|| FlowError::failed("step sum overflow"))
    })?;

    let crashed = lgwks_bot::block_on(host.run(&resuming, true));
    assert!(
        matches!(crashed.disposition(), Disposition::Cancelled),
        "the modelled crash cancels the run"
    );
    assert!(crashed.output().is_none(), "a cancelled run has no output");
    let run = crashed.run_id().ok_or("a stored run must name a run id")?;
    let store = host
        .run_store()
        .ok_or("a host built with a store installed keeps one")?;
    assert_eq!(
        store.record_count(run),
        1,
        "only the first step was recorded"
    );
    writeln!(out, "killed run: {:?}", crashed.disposition())?;

    let resumed = lgwks_bot::block_on(host.resume(run, &resuming, false));
    assert!(
        matches!(resumed.disposition(), Disposition::Succeeded),
        "the resumed run finishes"
    );
    assert_eq!(
        resumed.output(),
        Some(&3),
        "the resumed run sums both steps"
    );
    writeln!(
        out,
        "resumed run: {:?} output: {:?}",
        resumed.disposition(),
        resumed.output()
    )?;
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    retry_demo()?;
    deadline_demo()?;
    let host = demo_host()?;
    refusal_demo(&host)?;
    resume_demo(&host)?;
    Ok(())
}
