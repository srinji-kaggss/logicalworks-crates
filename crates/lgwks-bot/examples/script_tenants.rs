//! `script!` as a user runs it: two tenants, one flow, a real fan-out.
//!
//! Crawls the same 10,000 pages for two tenants at once. Every page takes a
//! millisecond, one page in 97 fails transiently on its first attempt, and the
//! site deduplicates by the step key it is handed. The run prints what a user
//! needs to believe it worked: pages fetched per tenant, retries absorbed,
//! duplicates the keys prevented, keys shared across tenants (must be zero),
//! and wall time. It then runs the same workload hand-written against
//! `join_all_bounded`, the credible alternative, and prints both timings.
//! Last, it prints the architecture map the script emitted.
//!
//! ```text
//! cargo run -p lgwks_bot --example script_tenants --release
//! ```
//!
//! Output goes through `std::io::Write` because `clippy::print_stdout` is
//! forbidden workspace-wide, examples included.

use std::collections::HashSet;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lgwks_bot::rt::task::join_all_bounded;
use lgwks_bot::rt::time::{sleep, timeout};
use lgwks_bot::script::{FlowError, Scope, StepKey, Tenant};

#[path = "../tests/support/lock.rs"]
mod lock;

use lock::take_unpoisoned;

/// Pages per tenant.
const PAGES: u32 = 10_000;

/// The bound `each` chooses when none is written: 64 waits per available
/// core. The hand-written crawl is given the same number so the comparison is
/// of the orchestration, not of the bound.
fn machine_bound() -> usize {
    std::thread::available_parallelism()
        .map_or(1, std::num::NonZeroUsize::get)
        .saturating_mul(64)
        .min(lgwks_bot::script::MAX_IN_FLIGHT)
}

/// A remote site that deduplicates requests by idempotency key.
#[derive(Default)]
struct Site {
    /// What the site has seen, behind one lock never held across an await.
    state: Mutex<SiteState>,
}

/// The site's ledger.
#[derive(Default)]
struct SiteState {
    /// Keys already served: a repeat is answered from here, not redone.
    served: HashSet<StepKey>,
    /// Pages whose first attempt has already failed.
    failed_once: HashSet<u32>,
    /// Requests that did the work.
    fetched: u64,
    /// Requests answered as duplicates.
    duplicates: u64,
    /// Transient failures handed out.
    transient: u64,
}

impl Site {
    /// Fetch `page` under `key`: one millisecond, a transient failure on the
    /// first attempt of every 97th page, and a key seen before is a duplicate.
    async fn fetch(&self, key: StepKey, page: u32) -> Result<u64, FlowError> {
        sleep(Duration::from_millis(1)).await;
        let mut state = take_unpoisoned(&self.state);
        if page.is_multiple_of(97) && state.failed_once.insert(page) {
            state.transient = state.transient.saturating_add(1);
            let refusal = Err(FlowError::transient("the site was busy"));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "fetch: returning an error to the caller");
            return refusal;
        }
        if !state.served.insert(key) {
            state.duplicates = state.duplicates.saturating_add(1);
        } else {
            state.fetched = state.fetched.saturating_add(1);
        }
        Ok(u64::from(page))
    }

    /// `(fetched, duplicates, transient)`.
    fn counts(&self) -> (u64, u64, u64) {
        let state = take_unpoisoned(&self.state);
        (state.fetched, state.duplicates, state.transient)
    }

    /// Every key this site served.
    fn keys(&self) -> HashSet<StepKey> {
        let state = take_unpoisoned(&self.state);
        state.served.clone()
    }
}

lgwks_bot::script! {
    /// Fetch every page, as many at once as this machine sustains; each
    /// attempt gets one second and a busy site is retried up to three times
    /// under the same key.
    pub flow crawl(site: &Site, pages: Vec<u32>) -> u64:
        let sizes = each page in pages:
            retry up to 3 times, waiting 5ms:
                within 1s:
                    site.fetch(scope.key(), page).await?
        give back sizes.iter().sum()
}

/// The same crawl written by hand on the bounded fan-out primitive: the shape
/// a careful author reaches for without `script!`. It must own everything it
/// touches (`Arc`, `'static`), spell out the retry and deadline, and invent
/// its own key.
async fn crawl_by_hand(site: Arc<Site>, tenant: &str, pages: Vec<u32>) -> Result<u64, FlowError> {
    let tenant = Arc::<str>::from(tenant);
    let futures = pages.into_iter().map(|page| {
        let site = Arc::clone(&site);
        let tenant = Arc::clone(&tenant);
        async move {
            let key = Scope::root(Tenant::new(&tenant)?)
                .enter(&format!("page#{page}"))?
                .key();
            // Three attempts, spelled as a ceiling inside the loop rather than as
            // a `for` with an arm after it: every arm either returns or counts
            // towards the ceiling, so there is no outcome left to invent for the
            // path out of the loop and no stand-in error standing in for a
            // refusal nobody produced.
            let mut attempt = 0_u32;
            loop {
                let refusal = match timeout(Duration::from_secs(1), site.fetch(key, page)).await {
                    Ok(Ok(size)) => return Ok(size),
                    Ok(Err(error)) => error,
                    Err(_elapsed) => FlowError::transient("timed out"),
                };
                attempt = attempt.saturating_add(1);
                if !refusal.is_retryable() || attempt >= 3 {
                    let refusal = Err(refusal);
                    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "crawl_by_hand: returning an error to the caller");
                    return refusal;
                }
                sleep(Duration::from_millis(5)).await;
            }
        }
    });
    let mut total: u64 = 0;
    for outcome in join_all_bounded(machine_bound(), futures).await {
        total = total.saturating_add(outcome?);
    }
    Ok(total)
}

/// Pages per second over `elapsed`, or `None` when the window is not a number.
///
/// `None` is the elapsed window rounding to zero at the microsecond resolution
/// the report prints in: a clock below the run rather than a page count, and a
/// printed zero would read as a rate the site achieved.
fn rate(elapsed: Duration) -> Option<u128> {
    let micros = elapsed.as_micros();
    if micros == 0 {
        return None;
    }
    u128::from(PAGES.saturating_mul(2))
        .saturating_mul(1_000_000)
        .checked_div(micros)
}

/// The throughput clause of the timing line, or the explicit absence.
fn rate_field(elapsed: Duration) -> String {
    match rate(elapsed) {
        Some(pages) => format!("{pages} pages/s"),
        None => String::from("pages/s unmeasured at this clock's resolution"),
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = lgwks_bot::Runtime::new()?;
    let mut out = std::io::stdout().lock();
    let pages: Vec<u32> = (0..PAGES).collect();
    let expected: u64 = pages.iter().map(|&page| u64::from(page)).sum();

    // Two tenants, one flow, concurrently, on one task.
    let acme_site = Site::default();
    let globex_site = Site::default();
    let acme = Scope::root(Tenant::new("acme")?);
    let globex = Scope::root(Tenant::new("globex")?);
    let started = Instant::now();
    let (acme_total, globex_total) = runtime.block_on(async {
        lgwks_bot::try_join!(
            crawl(&acme, &acme_site, pages.clone()),
            crawl(&globex, &globex_site, pages.clone()),
        )
    })?;
    let scripted = started.elapsed();

    for (name, site, total) in [
        ("acme", &acme_site, acme_total),
        ("globex", &globex_site, globex_total),
    ] {
        let (fetched, duplicates, transient) = site.counts();
        writeln!(
            out,
            "{name:>7}: {fetched} pages fetched, {transient} transient failures retried, \
             {duplicates} duplicates, total {total} (expected {expected})"
        )?;
        if total != expected || fetched != u64::from(PAGES) {
            let refusal =
                Err(format!("{name}: the crawl did not fetch every page exactly once").into());
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "main: returning an error to the caller");
            return refusal;
        }
    }
    let shared = acme_site.keys().intersection(&globex_site.keys()).count();
    writeln!(out, " shared: {shared} keys shared across tenants")?;
    if shared != 0 {
        let refusal = Err("tenants shared a key".into());
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "main: returning an error to the caller");
        return refusal;
    }

    // The same two crawls, written by hand on the bounded fan-out primitive.
    let started = Instant::now();
    runtime.block_on(async {
        lgwks_bot::try_join!(
            crawl_by_hand(Arc::new(Site::default()), "acme", pages.clone()),
            crawl_by_hand(Arc::new(Site::default()), "globex", pages.clone()),
        )
    })?;
    let by_hand = started.elapsed();
    writeln!(
        out,
        "   time: script! {scripted:.2?} ({}), by hand {by_hand:.2?} ({}), \
         {} in flight per tenant (chosen by the machine)",
        rate_field(scripted),
        rate_field(by_hand),
        machine_bound(),
    )?;
    writeln!(out, "\n{ARCHITECTURE}")?;
    Ok(())
}
