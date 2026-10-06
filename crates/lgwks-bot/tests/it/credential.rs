//! Row 3 of issue #278, on the real surface: a credential that expires while a
//! bot holds it.
//!
//! The proof is minted the way a host mints one — `GrantSet::issue_at` on a
//! declared clock — and then *held*, which is what an adapter does when it
//! obtains a token once and spends it over a ladder of steps. The subject is a
//! real file read through the shipped `domain::data::JsonStore` verb path, and
//! the clock is a real wall clock, so the lifetime is spent in real time rather
//! than by advancing a counter: a test that proved only that `now` had been
//! moved would prove nothing about a proof that lapses on its own.
//!
//! The recovery case is in the same file: a fresh grant mints a working proof
//! again, which is the repair a lapsed credential calls for. A check that
//! simply refused everything would pass the first half and fail the second.

use std::error::Error;
use std::time::Duration;

use crate::scratch::Scratch;

use lgwks_bot::cap::{Cap, upstream_credential_rejection};
use lgwks_bot::clock::Clock;
use lgwks_bot::domain::data::JsonStore;
use lgwks_bot::domain::gh::GhError;
use lgwks_bot::error::BotError;
use lgwks_bot::spec::{Need, NeedSet};
use lgwks_bot::verb::Observe;
use lgwks_bot::{GrantSet, RetryClass};

type TestResult = Result<(), Box<dyn Error>>;

/// A lifetime short enough that a test spends it in real time, and long enough
/// that the first poll is not a race the test has already lost.
const LIFETIME: Duration = Duration::from_millis(120);

/// How long to park before the second poll. Longer than [`LIFETIME`] by a
/// margin that dwarfs the resolution of the host's timer, so the second poll is
/// certainly past the expiry rather than usually past it.
const ELAPSED: Duration = Duration::from_millis(220);

/// A store holding one whole document, so a poll has something to report.
fn real_store(tag: &str) -> Result<(Scratch, JsonStore), Box<dyn Error>> {
    let scratch = Scratch::new(tag)?;
    let path = scratch.path().join("store.json");
    std::fs::write(&path, b"{\"revision\":1}")?;
    Ok((scratch, JsonStore::new(&path)))
}

/// Poll a store with `auth`, keeping the error rather than its classification.
async fn poll_with(store: &JsonStore, auth: &lgwks_bot::Auth) -> Result<String, BotError> {
    Observe::poll(store, (auth.clone(), ()))
        .await
        .map(|state| state.raw().to_owned())
}

/// Park the calling thread for at least `wait`.
///
/// `park_timeout` rather than the banned blocking `sleep`: this thread has no
/// reactor, so `rt::time::sleep` cannot be awaited on it, and what is being
/// waited for is the credential's own lifetime passing in real time. Repeated
/// until the span is spent, because `park_timeout` may return early on a
/// spurious wake-up, and a lifetime the test only *usually* outwaited would make
/// the expiry assertions a race.
fn spend(wait: Duration) {
    let started = std::time::Instant::now();
    while let Some(left) = wait.checked_sub(started.elapsed()) {
        if left.is_zero() {
            break;
        }
        std::thread::park_timeout(left);
    }
}

/// The row's own journey: a proof that is minted, spent once, and then found
/// lapsed — refused as a typed `CredentialExpired` naming the capabilities it
/// covered, and classified as the permanent refusal a re-grant repairs.
#[test]
fn a_proof_that_lapses_mid_run_refuses_the_verb() -> TestResult {
    let (_scratch, store) = real_store("lapsing")?;
    let clock = Clock::wall();
    let grants = GrantSet::empty().grant_expiring(Cap::fs(), LIFETIME);
    let auth = grants.issue_at(&[Cap::fs()], &clock)?;

    let raw = lgwks_std::task::block_on(poll_with(&store, &auth))?;
    assert_eq!(
        raw, "{\"revision\":1}",
        "a fresh credential reads the store it is entitled to read"
    );
    assert!(
        !auth.is_expired(),
        "and it is live, with {:?} of its life left",
        auth.remaining()
    );

    spend(ELAPSED);

    match lgwks_std::task::block_on(poll_with(&store, &auth)) {
        Ok(raw) => Err(Box::new(std::io::Error::other(format!(
            "a credential that expired {ELAPSED:?} ago must not read the store, but it \
             returned {raw}"
        )))),
        Err(BotError::CredentialExpired {
            ref capabilities,
            expired_at,
            now,
        }) => {
            assert_eq!(
                capabilities,
                &[Cap::fs()],
                "the refusal names every capability the lapsed proof covered"
            );
            assert_eq!(
                auth.expires_at(),
                Some(expired_at),
                "and the reading it expired at, which the proof itself carries"
            );
            assert!(
                now >= expired_at,
                "the check must be reported at or after the expiry: {now:?} against {expired_at:?}"
            );
            Ok(())
        }
        Err(other) => Err(Box::new(std::io::Error::other(format!(
            "a lapsed credential must be CredentialExpired, not {other}"
        )))),
    }
}

/// The repair: a lapsed credential is closed by minting a new proof from a
/// grant that names a new lifetime, and the store is readable again.
#[test]
fn a_fresh_grant_mints_a_working_proof_again() -> TestResult {
    let (_scratch, store) = real_store("repair")?;
    let clock = Clock::wall();
    let lapsed = GrantSet::empty()
        .grant_expiring(Cap::fs(), LIFETIME)
        .issue_at(&[Cap::fs()], &clock)?;
    spend(ELAPSED);
    assert!(
        lgwks_std::task::block_on(poll_with(&store, &lapsed)).is_err(),
        "the control: the credential is lapsed before the repair is attempted"
    );

    let reissued = GrantSet::empty()
        .grant_expiring(Cap::fs(), LIFETIME)
        .issue_at(&[Cap::fs()], &clock)?;
    assert!(
        !reissued.is_expired(),
        "a fresh grant is not lapsed, whatever the old one was"
    );
    let raw = lgwks_std::task::block_on(poll_with(&store, &reissued))?;
    assert_eq!(
        raw, "{\"revision\":1}",
        "and the re-granted credential reads the store again"
    );
    Ok(())
}

/// A grant that named no lifetime is not a lapsed one — the two are different
/// facts, and collapsing them would make every ordinary proof look expired.
#[test]
fn a_grant_without_a_lifetime_never_expires() -> TestResult {
    let (_scratch, store) = real_store("perpetual")?;
    let auth = GrantSet::empty()
        .grant(Cap::fs())
        .issue_at(&[Cap::fs()], &Clock::wall())?;
    spend(ELAPSED);
    assert_eq!(
        auth.expires_at(),
        None,
        "a grant with no lifetime names no expiry, which is not an expired one"
    );
    assert!(
        !auth.is_expired(),
        "and the proof is still live however long the test waited"
    );
    let raw = lgwks_std::task::block_on(poll_with(&store, &auth))?;
    assert_eq!(raw, "{\"revision\":1}", "so the verb still reads the store");
    Ok(())
}

/// A proof covering two capabilities with different lifetimes takes the
/// shortest: a proof is presented as a whole and must not outlive its weakest
/// part. Re-granting the same capability with a longer lifetime must not widen
/// it either, which is the shape of a token its issuer cancelled quietly keeping working.
#[test]
fn a_proof_takes_the_shortest_of_its_capabilities_lifetimes() -> TestResult {
    let clock = Clock::virtual_at(Duration::ZERO);
    let grants = GrantSet::empty()
        .grant_expiring(Cap::net(), Duration::from_secs(3600))
        .grant_expiring(Cap::fs(), Duration::from_millis(1));
    let auth = grants.issue_at(&[Cap::net(), Cap::fs()], &clock)?;
    assert_eq!(
        auth.expires_at(),
        Some(Duration::from_millis(1)),
        "the proof must expire when its shortest-lived capability does"
    );

    let widened = GrantSet::empty()
        .grant_expiring(Cap::fs(), Duration::from_millis(1))
        .grant_expiring(Cap::fs(), Duration::from_secs(3600));
    let held = widened.issue_at(&[Cap::fs()], &clock)?;
    assert_eq!(
        held.expires_at(),
        Some(Duration::from_millis(1)),
        "a second, longer grant must not extend the first"
    );
    Ok(())
}

/// The upstream half: a receiver that refuses the credential is a typed
/// rejection carrying the repair, not a generic failure and not a retry.
#[test]
fn an_upstream_rejection_carries_the_repair_and_is_never_retried() -> TestResult {
    let required = [Cap::fs(), Cap::net(), Cap::fs()];
    let rejection = upstream_credential_rejection("net::probe", 401, &required);

    assert_eq!(
        rejection.retry_class(),
        RetryClass::Never,
        "the same credential against the same receiver gives the same status, so a retry is \
         a loop rather than a repair"
    );
    let BotError::CredentialRejected {
        ref domain,
        status,
        ref needs,
    } = rejection
    else {
        return Err(Box::new(std::io::Error::other(
            "an upstream refusal must be CredentialRejected",
        )));
    };
    assert_eq!(
        domain, "net::probe",
        "the refusal names the domain that presented the credential"
    );
    assert_eq!(status, 401, "and the status the receiver reported");

    let reported = needs.needs();
    assert_eq!(
        reported.len(),
        1,
        "one shortfall in, one need out: a caller must not iterate to learn what to re-grant: \
         {needs}"
    );
    let Need::CredentialExpired {
        ref domain,
        ref capabilities,
    } = reported[0]
    else {
        return Err(Box::new(std::io::Error::other(format!(
            "the repair must be a credential need, not a shortage: {needs}"
        ))));
    };
    assert_eq!(
        domain, "net::probe",
        "the need is attributed to the domain whose upstream refused"
    );
    assert_eq!(
        capabilities,
        &[Cap::fs(), Cap::net()],
        "sorted and de-duplicated: two spellings of one requirement are one requirement"
    );

    let repair = needs.proposed_grants();
    assert!(
        repair.admit(&[Cap::net(), Cap::fs()]).is_ok(),
        "and the proposed grants must close exactly what the need names"
    );
    assert!(
        rejection.to_string().contains("401"),
        "the refusal says the status in its own words: {rejection}"
    );
    Ok(())
}

/// The same repair, reached from the shipped adapter that actually sees a
/// `401`: GitHub's client. A permission loss and a transport failure are
/// different facts, and only one of them is repaired by authority.
#[test]
fn a_github_permission_loss_repairs_authority_and_a_transport_loss_does_not() -> TestResult {
    let unauthorized = GhError::Unauthorized {
        what: "read diff".into(),
        status: 401,
        reason: "HTTP 401".into(),
    };
    let Some(needs) = unauthorized.repair() else {
        return Err(Box::new(std::io::Error::other(
            "a 401 is repaired by re-granting authority, so it must carry the repair",
        )));
    };
    assert_eq!(
        needs,
        NeedSet::expired_credentials("gh", &[Cap::net()]),
        "and the repair is the one need naming the capability the domain needs"
    );
    assert!(
        !unauthorized.is_read_only_retryable(),
        "and repeating the same credential is not what fixes it"
    );

    let transport = GhError::Transport {
        what: "read diff".into(),
        exit_code: Some(1),
        stderr: "connection reset".into(),
    };
    assert!(
        transport.repair().is_none(),
        "a transport failure is repaired by a network, not by authority: {transport}"
    );
    Ok(())
}
