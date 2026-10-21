//! The step-key split through the public host API (issue #646).
//!
//! | Test | What it pins |
//! |---|---|
//! | `two_runs_key_their_steps_apart_through_a_store` | two runs of one flow over one stored host key their steps apart, so a receiver deduplicating on `scope.key()` answers each run once |
//! | `sim_business_key_dedups_across_runs` | work named with `Scope::business_key` deduplicates across runs and across tenants, while the same calls' step keys stay apart |
//! | `a_reattach_replays_without_rekeying_and_a_new_run_keys_apart` | a resubmission replays the recorded run without re-entering the body, and a new request keys apart |

#![cfg(all(feature = "script", feature = "ephemeral"))]

use crate::scratch::Scratch;

use std::cell::RefCell;
use std::collections::HashSet;
use std::error::Error;
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};

use lgwks_bot::script::Scope;
use lgwks_bot::task::{Host, RequestKey, Submission, task};

// The request fixtures themselves.
use crate::request_fixtures as request;

use request::stored_host;

type TestResult = Result<(), Box<dyn Error>>;

/// A site that answers each distinct key once and counts what it saw.
#[derive(Default)]
struct Site {
    state: RefCell<SiteState>,
}

#[derive(Default)]
struct SiteState {
    served: HashSet<String>,
    duplicates: AtomicU32,
}

impl Site {
    /// Answer `key`: the first sight is served, every later sight is a
    /// duplicate the receiver would have dropped.
    fn serve(&self, key: &str) {
        let mut state = self.state.borrow_mut();
        if !state.served.insert(key.to_owned()) {
            state.duplicates.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// How many distinct keys the site served.
    fn served(&self) -> usize {
        self.state.borrow().served.len()
    }

    /// How many repeat sights the site dropped.
    fn duplicates(&self) -> u32 {
        self.state.borrow().duplicates.load(Ordering::SeqCst)
    }

    /// The keys served, in no particular order.
    fn keys(&self) -> Vec<String> {
        self.state.borrow().served.iter().cloned().collect()
    }
}

/// Submit each key in turn over one host, each run sighting its step key at
/// `site`; returns the submissions in order.
fn submit_all(
    host: &Host,
    site: &Rc<Site>,
    keys: &[&str],
) -> Result<Vec<Submission<u32>>, Box<dyn Error>> {
    let work_site = Rc::clone(site);
    let work = Rc::new(task("notify", move |scope: Scope, value: u32| {
        let site = Rc::clone(&work_site);
        async move {
            site.serve(&scope.key().to_hex());
            Ok(value)
        }
    })?);
    keys.iter()
        .map(|key| -> Result<Submission<u32>, Box<dyn Error>> {
            Ok(lgwks_bot::block_on(host.submit(
                &RequestKey::new(key)?,
                &work,
                1_u32,
            ))?)
        })
        .collect()
}

/// Two runs of one flow over one stored host key their steps apart.
#[test]
fn two_runs_key_their_steps_apart_through_a_store() -> TestResult {
    let scratch = Scratch::new("sim-step-key-runs")?;
    let host = stored_host("acme", scratch.path())?;
    let site = Rc::new(Site::default());

    let submissions = submit_all(&host, &site, &["run-a", "run-b"])?;
    assert_eq!(submissions.len(), 2, "two keys, two submissions");
    assert!(
        matches!(submissions[0], Submission::Executed(_)),
        "a first submission executes"
    );
    assert!(
        matches!(submissions[1], Submission::Executed(_)),
        "a second request key is its own run, not a reattach"
    );

    assert_eq!(site.served(), 2, "each run's effect reached the site once");
    assert_eq!(site.duplicates(), 0, "no run's key matched another run's");
    let keys = site.keys();
    assert_ne!(keys[0], keys[1], "the two runs key their steps apart");
    Ok(())
}

/// Work named with a business key deduplicates across runs and tenants.
#[test]
fn sim_business_key_dedups_across_runs() -> TestResult {
    let scratch = Scratch::new("sim-business-key")?;
    let acme = stored_host("acme", scratch.path())?;
    let globex = stored_host("globex", scratch.path())?;
    let site = Rc::new(Site::default());

    let work_site = Rc::clone(&site);
    let charge = Rc::new(task("charge", move |scope: Scope, value: u32| {
        let site = Rc::clone(&work_site);
        async move {
            let key = scope.business_key("charge-card", &value)?;
            site.serve(&key.to_hex());
            Ok(value)
        }
    })?);

    lgwks_bot::block_on(acme.submit(&RequestKey::new("charge-1")?, &charge, 9001_u32))?;
    lgwks_bot::block_on(acme.submit(&RequestKey::new("charge-2")?, &charge, 9001_u32))?;
    lgwks_bot::block_on(globex.submit(&RequestKey::new("charge-1")?, &charge, 9001_u32))?;

    assert_eq!(
        site.served(),
        2,
        "one charge per tenant: acme's two runs share one business key, globex's run is its own"
    );
    assert_eq!(
        site.duplicates(),
        1,
        "acme's second run is a duplicate under the business key"
    );
    let keys = site.keys();
    assert_ne!(keys[0], keys[1], "the two tenants' business keys differ");
    Ok(())
}

/// A resubmission replays under the same step key; a new request keys apart.
#[test]
fn a_reattach_replays_without_rekeying_and_a_new_run_keys_apart() -> TestResult {
    let scratch = Scratch::new("sim-reattach-replay")?;
    let host = stored_host("acme", scratch.path())?;
    let site = Rc::new(Site::default());

    let submissions = submit_all(&host, &site, &["order-42", "order-42", "order-43"])?;
    assert_eq!(submissions.len(), 3, "three keys, three submissions");
    assert!(
        matches!(submissions[0], Submission::Executed(_)),
        "a first submission executes"
    );
    assert!(
        matches!(submissions[1], Submission::Reattached(_)),
        "a repeat under one key reattaches"
    );
    assert!(
        matches!(submissions[2], Submission::Executed(_)),
        "a new request key is its own run"
    );

    assert_eq!(
        site.served(),
        2,
        "the replay reattaches without re-entering the body; the new request runs"
    );
    assert_eq!(
        site.duplicates(),
        0,
        "the replay is not a duplicate sight: the body never re-entered"
    );
    let keys = site.keys();
    assert_ne!(
        keys[0], keys[1],
        "the replay's run and the new request's run key apart"
    );
    Ok(())
}
