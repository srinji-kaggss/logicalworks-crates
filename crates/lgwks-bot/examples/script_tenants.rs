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
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use lgwks_bot::rt::task::join_all_bounded;
use lgwks_bot::rt::time::{sleep, timeout};
use lgwks_bot::script::{FlowError, Scope, StepKey, Tenant};

/// Pages per tenant.
const PAGES: u32 = 10_000;

/// Pages in flight per tenant.
const IN_FLIGHT: usize = 256;

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
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if page.is_multiple_of(97) && state.failed_once.insert(page) {
            state.transient = state.transient.saturating_add(1);
            return Err(FlowError::transient("the site was busy"));
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
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        (state.fetched, state.duplicates, state.transient)
    }

    /// Every key this site served.
    fn keys(&self) -> HashSet<StepKey> {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.served.clone()
    }
}

lgwks_bot::script! {
    /// Fetch every page, 256 at a time; each attempt gets one second and a
    /// busy site is retried up to three times under the same key.
    pub flow crawl(site: &Site, pages: Vec<u32>) -> u64:
        let sizes = each page in pages, at most 256 at once:
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
            let mut last = None;
            for _attempt in 0..3_u32 {
                match timeout(Duration::from_secs(1), site.fetch(key, page)).await {
                    Ok(Ok(size)) => return Ok(size),
                    Ok(Err(error)) if error.is_retryable() => last = Some(error),
                    Ok(Err(error)) => return Err(error),
                    Err(_elapsed) => last = Some(FlowError::transient("timed out")),
                }
                sleep(Duration::from_millis(5)).await;
            }
            Err(last.unwrap_or_else(|| FlowError::failed("no attempt ran")))
        }
    });
    let mut total: u64 = 0;
    for outcome in join_all_bounded(IN_FLIGHT, futures).await {
        total = total.saturating_add(outcome?);
    }
    Ok(total)
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
            return Err(format!("{name}: the crawl did not fetch every page exactly once").into());
        }
    }
    let shared = acme_site.keys().intersection(&globex_site.keys()).count();
    writeln!(out, " shared: {shared} keys shared across tenants")?;
    if shared != 0 {
        return Err("tenants shared a key".into());
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
    let rate = |elapsed: Duration| {
        u128::from(PAGES.saturating_mul(2))
            .saturating_mul(1_000_000)
            .checked_div(elapsed.as_micros().max(1))
            .unwrap_or(0)
    };
    writeln!(
        out,
        "   time: script! {scripted:.2?} ({} pages/s), by hand {by_hand:.2?} ({} pages/s), \
         {IN_FLIGHT} in flight per tenant",
        rate(scripted),
        rate(by_hand),
    )?;
    writeln!(out, "\n{ARCHITECTURE}")?;
    Ok(())
}
