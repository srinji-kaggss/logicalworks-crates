//! The authority lifetime, as an executable contract.
//!
//! Issue #41 read the crate contract as promising live revocation: "checks on
//! every call ensure a grant revoked after build cannot fire". The code has
//! never done that, and cannot: `GrantSet` is an owned `HashSet` with no revoke
//! operation, `EcsBot::assemble` stores `grants.clone()` in a private resource,
//! and `Auth` is a `Vec<Cap>` that answers one question — is this capability in
//! the list I was minted with. `#59` narrowed the prose to say so, which leaves
//! the issue's second half, and that is what this file is:
//!
//! > A contract/API test should prevent revocation claims when no operation
//! > exists.
//!
//! Three tests carry it:
//!
//! - [`a_proof_minted_from_a_grant_set_outlives_that_set`] executes the issue's
//!   own counterexample and pins the behaviour it demonstrates — an `Auth` in
//!   hand still authorizes after the set that minted it is gone. That is the
//!   snapshot boundary, tested rather than described.
//! - [`withdrawing_authority_after_build_means_building_again`] walks the
//!   supported path end to end: admission refuses an ungranted bot at build, a
//!   built bot keeps the authority it was admitted with even after the caller
//!   replaces its set, and withdrawing a capability means building again —
//!   which refuses.
//! - [`no_first_party_text_claims_revocation_this_crate_lacks`] reads every
//!   first-party text file in the repository and requires each mention of
//!   `revo*` to sit in text that denies the operation, so a claim with no
//!   operation behind it fails the build. It is the grep, left behind as a test.
//! - [`the_claim_guard_flags_the_claim_it_was_written_for`] is that guard's own
//!   control: it feeds the guard the pre-fix sentence, and both spellings of the
//!   family, and requires each to be reported.
//!
//! Nothing here asserts that an in-process SDK confines a process. The issue's
//! own evidence section says the same: this is an authority-lifetime contract,
//! not a sandbox.

use std::cell::Cell;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Barrier;

use lgwks_bot::spec::EffectScope;
use lgwks_bot::verb::{Execute, Observe};
use lgwks_bot::{Auth, Bot, BotError, Cap, DispatchCertainty, EffectLifetime, GrantSet};

use crate::effects;
use crate::poll::admit_poll;

/// What a test here reports when its precondition did not hold.
///
/// These tests cross `BotError` and `std::io`, so they return `Box<dyn Error>`
/// and propagate with `?`. A precondition that fails is then a named reason
/// rather than an unwind that reports only that something unwound. A scope that
/// cannot be built reports through the same box for the same reason: the error
/// domains it crosses do not convert into one another, and flattening them
/// would name the wrong failure.
type TestResult = Result<(), Box<dyn Error>>;

/// The same box, for the one helper that is not a test body.
type ScopeResult<T> = Result<T, Box<dyn Error>>;

// ── Test doubles ────────────────────────────────────────────────────────────

/// A scripted source: yields the next value from `values` on each poll, and
/// requires `bot.net`, so a chain on it is subject to build-time admission and
/// to the narrowing these tests are about.
struct Script {
    /// The values to yield, in poll order.
    values: Vec<u16>,
    /// How many polls have happened.
    cursor: Cell<usize>,
    /// How many polls got past the capability check.
    polls: Cell<u32>,
    /// The one capability this source requires. A field rather than a
    /// constructor-time `vec!` because [`Observe::required_caps`] borrows from
    /// the observer.
    caps: Vec<Cap>,
}

impl Script {
    /// A source that yields `values` in order and requires `bot.net`.
    fn new(values: Vec<u16>) -> Self {
        Self {
            values,
            cursor: Cell::new(0),
            polls: Cell::new(0),
            caps: vec![Cap::net()],
        }
    }
}

impl Observe for Script {
    type Output = u16;

    fn required_caps(&self) -> &[Cap] {
        &self.caps
    }

    async fn poll(&self, call: (Auth, ())) -> Result<u16, BotError> {
        admit_poll(&call.0, Observe::required_caps(self), &self.polls)?;
        let index = self.cursor.get();
        self.cursor.set(index.saturating_add(1));
        // A source that runs past its own script has nothing to report. It
        // refuses, because answering a value the test never scripted would let a
        // chain fire on movement nobody wrote.
        let Some(value) = self.values.get(index) else {
            let refusal = Err(BotError::DomainError {
                domain: "test::script".to_owned(),
                certainty: DispatchCertainty::NotDelivered,
                cause: format!("the script ran out at poll {index}"),
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "poll: returning an error to the caller");
            return refusal;
        };
        Ok(*value)
    }

    fn domain_id(&self) -> &str {
        "test::script"
    }
}

/// An action that counts its invocations: the observable effect a revocation
/// claim would have to stop, and the only evidence a test can read back.
struct Counted(Rc<Cell<usize>>);

impl Execute for Counted {
    type Input = u16;
    type Output = ();

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    fn effect_lifetime(&self) -> EffectLifetime {
        EffectLifetime::Local
    }

    async fn execute_action(&self, call: (Auth, &u16)) -> Result<(), BotError> {
        call.0.check(Execute::required_caps(self))?;
        self.0.set(self.0.get().saturating_add(1));
        Ok(())
    }

    fn domain_id(&self) -> &str {
        "test::count"
    }
}

// ── The lifetime itself ─────────────────────────────────────────────────────

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
    effects::memory_scope()
}

#[test]
fn a_proof_minted_from_a_grant_set_outlives_that_set() -> TestResult {
    // The issue's counterexample, executed as written. What it demonstrates is
    // the documented policy rather than a bug: `Auth` is a capability-membership
    // proof over the caps it was minted for, and it holds no reference to the
    // set that minted it. The two assertions after it are the controls that keep
    // this from being read as "anything goes".
    let issued = {
        let grants = GrantSet::empty().grant(Cap::net());
        grants.issue(&[Cap::net()])?
    };

    let narrowed = GrantSet::empty().grant(Cap::fs());

    assert!(
        narrowed.issue(&[Cap::net()]).is_err(),
        "a set that does not hold `bot.net` must not mint a proof for it"
    );
    assert!(
        narrowed.issue(&[Cap::fs()]).is_ok(),
        "narrowing is per-capability: the set still mints for what it does hold"
    );
    assert!(
        issued.check(&[Cap::net()]).is_ok(),
        "an issued proof keeps covering what it covered, whatever happens to the set that minted it"
    );
    assert!(
        issued.check(&[Cap::fs()]).is_err(),
        "and it must not cover what it was never minted for: a snapshot of a scope, not a blanket"
    );
    Ok(())
}

#[test]
fn withdrawing_authority_after_build_means_building_again() -> TestResult {
    let counter = Rc::new(Cell::new(0));
    let mut authority = GrantSet::empty().grant(Cap::net());

    let mut bot = Bot::builder("snapshot")
        .observe(Script::new(vec![100, 200, 503]))
        .on(|value: &u16| *value >= 500, Counted(Rc::clone(&counter)))
        .with_effects(test_effects()?)
        .build(&authority)?;

    assert_eq!(bot.tick()?, 0, "100 is below the threshold");
    assert_eq!(bot.tick()?, 0, "200 is below the threshold");
    assert_eq!(counter.get(), 0, "no action has run yet");

    // Narrowing, as far as a caller can do it: the very set that was passed to
    // `build` is replaced by one holding nothing.
    authority = GrantSet::empty();

    assert_eq!(
        bot.tick()?,
        1,
        "the built bot still holds the authority it was admitted with: the caller's set is a source, not a handle"
    );
    assert_eq!(
        counter.get(),
        1,
        "the effect ran with `bot.net` after the caller's set stopped holding it"
    );

    // Withdrawal is a rebuild, and the rebuild is refused at admission. This is
    // the whole of the supported revocation model, so it is asserted rather
    // than described.
    match Bot::builder("narrowed")
        .observe(Script::new(vec![503]))
        .on(|value: &u16| *value >= 500, Counted(Rc::new(Cell::new(0))))
        .with_effects(test_effects()?)
        .build(&authority)
    {
        Ok(_) => Err("a source requiring `bot.net` was admitted by an empty grant set".into()),
        Err(BotError::CapabilityDenied { deficit }) => {
            assert_eq!(
                deficit.first().required(),
                &Cap::net(),
                "the refusal must name the capability that was withdrawn"
            );
            Ok(())
        }
        Err(other) => Err(format!("expected CapabilityDenied, got {other:?}").into()),
    }
}

// ── Two tenants at once ─────────────────────────────────────────────────────

/// Threads per tenant, and rounds each thread repeats admission and dispatch.
const THREADS_PER_TENANT: usize = 8;
const ROUNDS: usize = 32;

/// One tenant's round: build a bot whose source requires `bot.net` under `grants`
/// and, if it is admitted, tick it once. Reports how many times the action fired,
/// or the refusal admission gave.
fn round(grants: &GrantSet) -> ScopeResult<Result<usize, BotError>> {
    let fired = Rc::new(Cell::new(0));
    let built = Bot::builder("tenant-round")
        .observe(Script::new(vec![503]))
        .on(|value: &u16| *value >= 500, Counted(Rc::clone(&fired)))
        .with_effects(test_effects()?)
        .build(grants);
    let mut bot = match built {
        Ok(bot) => bot,
        Err(refusal) => return Ok(Err(refusal)),
    };
    bot.tick()?;
    Ok(Ok(fired.get()))
}

/// Two tenants race through admission and dispatch on separate threads, released
/// together: the one whose grants lack `bot.net` is refused by name every round,
/// and the one that holds it is served every round. Neither outcome leaks across.
#[test]
fn a_tenant_without_the_grant_is_refused_while_the_other_is_served() -> TestResult {
    let serves = GrantSet::empty().grant(Cap::net());
    let lacks = GrantSet::empty().grant(Cap::fs());
    let barrier = Barrier::new(THREADS_PER_TENANT * 2);

    let outcomes = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..THREADS_PER_TENANT * 2)
            .map(|index| {
                let grants = if index % 2 == 0 { &serves } else { &lacks };
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    let results: Vec<_> = (0..ROUNDS)
                        .map(|_| round(grants).map_err(|error| error.to_string()))
                        .collect();
                    (index % 2 == 0, results)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join())
            .collect::<Vec<_>>()
    });

    let mut served_rounds = 0_usize;
    let mut refused_rounds = 0_usize;
    for joined in outcomes {
        let (is_serving, results) = joined.map_err(|_| "a tenant thread panicked")?;
        for result in results {
            match (is_serving, result?) {
                (true, Ok(1)) => served_rounds = served_rounds.saturating_add(1),
                (true, other) => {
                    return Err(format!("the granted tenant was not served once: {other:?}").into());
                }
                (false, Err(BotError::CapabilityDenied { deficit })) => {
                    assert_eq!(
                        deficit.first().required(),
                        &Cap::net(),
                        "the refusal names the capability this tenant lacks"
                    );
                    refused_rounds = refused_rounds.saturating_add(1);
                }
                (false, other) => {
                    return Err(format!("the ungranted tenant was not refused: {other:?}").into());
                }
            }
        }
    }
    let per_tenant = THREADS_PER_TENANT * ROUNDS;
    assert_eq!(served_rounds, per_tenant, "every granted round was served");
    assert_eq!(
        refused_rounds, per_tenant,
        "every ungranted round was refused"
    );
    Ok(())
}

/// A proof minted for one tenant does not cover what only the other was granted,
/// in either direction, and a set cannot mint for what it does not hold.
#[test]
fn a_proof_minted_for_one_tenant_does_not_cover_the_others_grant() -> TestResult {
    let net_tenant = GrantSet::empty().grant(Cap::net());
    let fs_tenant = GrantSet::empty().grant(Cap::fs());
    let net_proof = net_tenant.issue(&[Cap::net()])?;
    let fs_proof = fs_tenant.issue(&[Cap::fs()])?;

    assert!(net_proof.check(&[Cap::net()]).is_ok());
    assert!(fs_proof.check(&[Cap::fs()]).is_ok());
    assert!(
        net_proof.check(&[Cap::fs()]).is_err(),
        "the net tenant's proof does not cover what only the fs tenant was granted"
    );
    assert!(fs_proof.check(&[Cap::net()]).is_err(), "and the reverse");
    assert!(
        fs_tenant.issue(&[Cap::net()]).is_err(),
        "a tenant's set cannot mint a proof for what it does not hold"
    );
    Ok(())
}

// ── The API surface ─────────────────────────────────────────────────────────

/// Method names that would be a revocation or authority-generation operation.
///
/// The crate contract denies these exist, and the denial is only true while it
/// is checked: an authority generation is exactly the "shared policy handle"
/// the issue's repair section describes, so adding one is a decision that has
/// to bring the prose with it.
const REVOCATION_OPERATIONS: [&str; 9] = [
    "fn revoke",
    "fn withdraw",
    "fn invalidate",
    "fn expire",
    "fn update_grants",
    "fn set_grants",
    "fn replace_grants",
    "fn authority_generation",
    "fn grant_revision",
];

/// Whether `text` declares `operation` (`fn <name>`) as an identifier.
///
/// The name may continue only with `_`, so `fn expire` and `fn expire_now` are
/// the operation and `fn expires_at` is not: an accessor that reads a lifetime
/// fixed at the mint is a different word from an operation that ends one. A
/// plain substring test could not tell the two apart, and the first lifetime
/// accessor (#278 row 3) tripped it.
fn declares(text: &str, operation: &str) -> bool {
    text.match_indices(operation).any(|(at, _)| {
        text.get(at.saturating_add(operation.len())..)
            .and_then(|rest| rest.chars().next())
            .is_none_or(|next| !next.is_ascii_alphanumeric())
    })
}

/// Read a file inside this crate, by a path relative to its manifest.
fn read_crate_file(relative: &str) -> Result<String, Box<dyn Error>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
    fs::read_to_string(&path)
        .map_err(|error| format!("{} could not be read: {error}", path.display()).into())
}

#[test]
fn the_authority_source_offers_no_revoke_operation() -> TestResult {
    let gate = read_crate_file("src/gate.rs")?;
    let cap = read_crate_file("src/cap.rs")?;

    // Controls. Every assertion below this point is an absence, and an absence
    // passes for free against text that was never read.
    for required in ["pub fn issue", "pub fn admit", "pub fn grant"] {
        assert!(
            gate.contains(required),
            "control: `{required}` is missing from the scanned `gate.rs`, so the scan is reading the wrong text"
        );
    }
    assert!(
        cap.contains("pub fn check"),
        "control: `check` is missing from the scanned `cap.rs`"
    );

    // The matcher's own controls: it must flag the operation and its `_`
    // extensions, and must not flag a different word that shares the prefix.
    for operation in [
        "pub fn expire(&mut self)",
        "fn expire_now()",
        "fn expire<T>()",
    ] {
        assert!(
            declares(operation, "fn expire"),
            "control: `{operation}` declares the operation and must be flagged"
        );
    }
    assert!(
        !declares("pub fn expires_at(&self)", "fn expire"),
        "control: an accessor named `expires_at` is not the `expire` operation"
    );

    for (file, text) in [("src/gate.rs", gate.as_str()), ("src/cap.rs", cap.as_str())] {
        for operation in REVOCATION_OPERATIONS {
            assert!(
                !declares(text, operation),
                "{file} declares `{operation}`, which `lib.rs`, `ecs.rs`, `README.md`, \
                 `docs/security-posture.md` and `docs/general-bot-fold.md` all say does not exist. \
                 If it is real now, those claims change in the same commit as this operation"
            );
        }
    }
    Ok(())
}

// ── The prose ───────────────────────────────────────────────────────────────

/// The stem the scan looks for: `revo`.
///
/// The word family splits its spelling — `revoke`, `revoked`, `revoking` carry a
/// `k`; `revocation`, `revocable`, `RevocationFence` carry a `c` — and the two
/// earlier versions of this scan each used one of them, so each silently missed
/// half the family. `revok` could not see `RevocationFence`; `revoc` could not
/// see `revoked`, which is the word the original defect was written with. The
/// prefix is what they share, and nothing else in this repository's prose
/// contains it.
const REVOCATION_STEM: &str = "revo";

/// How much flattened text either side of a mention counts as its context.
const WINDOW: usize = 200;

/// Phrases that turn a mention of revocation into a denial that the operation
/// exists. A claim needs only one of them nearby: the point is that a reader
/// who lands on the mention is told the truth there.
const DENIALS: [&str; 7] = [
    // `GrantSet` has no `revoke`, and no method that removes a capability.
    "no revoke",
    // `the no revocation boundary` — the guide's name for the same fact.
    "no revocation",
    // `nothing revokes it` — the ECS `fire` comment.
    "nothing revokes",
    // `No revocable authority.` — the guide's non-goal.
    "no revocable",
    // The crate contract's phrase for what an `Auth` is not.
    "not a live lease",
    // `there is no in process revocation and nothing here should be described
    // as one` — the README's own sentence, the one #59 added.
    "no in process revocation",
    // `the name is wrong for what exists` — the security-posture row.
    "wrong for what exists",
];

/// Phrases that show the mention is about a different subject.
///
/// Neither subject is authority lifetime: a lexicon **alias row**
/// (`language.rs`, `general-bot-fold.md`), which is deleted rather than revoked,
/// and a **sibling repository's** revocation machinery — `Rocco`'s
/// `rocco-runtime::RevocationFence`, which `docs/estate-asset-inventory.md`
/// inventories as an asset to port and `docs/guidance-runner-spec.md` cites as
/// one of the shapes a future runner would draw on. Neither is a claim about
/// `lgwks_bot`.
const OTHER_SUBJECTS: [&str; 5] = [
    // The alias table: "revoking one is deleting a row".
    "deleting a row",
    // The `learn` method's doc: "a row is readable and revocable which a
    // fitted weight is not".
    "readable and revocable",
    // The test that pins the alias behaviour.
    "learned alias",
    // `Rocco`'s type, named as an asset rather than as this crate's.
    "revocationfence",
    // The runner spec's sentence about where the estate's assets stop: "the
    // estate assets inform the shape ... while the bot keeps its own seams".
    "estate assets inform the shape",
];

/// Files the scan does not read, each with the reason it is not read.
///
/// One file, and the cost is stated: nothing in it is checked. It is the
/// state-of-the-art survey — macaroons, leases, revocation fences, published
/// work and design targets — so every mention in it is a claim about the field
/// rather than about this crate's shipped surface, and the phrases that would
/// qualify them are not denials of a shipped operation.
const NOT_AUTHORITY_CLAIMS: [(&str, &str); 1] = [(
    "docs/frontier.md",
    "the state-of-the-art survey: its mentions are published research and design \
     targets, not statements about this crate's shipped authority",
)];

/// File extensions the scan reads. A revocation claim is prose or code; a
/// lockfile or an image is neither.
const TEXT_EXTENSIONS: [&str; 8] = ["md", "rs", "toml", "txt", "json", "yml", "yaml", "sh"];

/// Directory names the walk never descends into.
///
/// `vendor` is third-party, `target` is derived, `.git` is history, and
/// `graphify-out`, `.codegraph` and `.lgwks` are derived artifacts — the same
/// set the repository's artifact law refuses to commit. Without them the
/// scan's verdict would depend on whether the machine happened to hold a
/// knowledge-graph export whose node labels quote this file's own test names,
/// which is how a guard green in CI fails on a developer's machine.
const SKIPPED_DIRECTORIES: [&str; 6] = [
    "target",
    "vendor",
    ".git",
    "graphify-out",
    ".codegraph",
    ".lgwks",
];

/// How far below the repository root the walk descends.
const MAX_DEPTH: u8 = 6;

/// `text` with every run of non-alphanumeric characters collapsed to a single
/// space and ASCII letters lowercased.
///
/// The window rule has to survive a line break inside the phrase it is looking
/// for — `GrantSet` has no\n`revoke` is a denial whose marker a wrap splits —
/// so the scan runs on flattened text rather than on lines. The output is pure
/// ASCII, which is what makes byte offsets into it safe to slice.
fn flatten(text: &str) -> String {
    let mut flat = String::with_capacity(text.len());
    for character in text.chars() {
        if character.is_ascii_alphanumeric() {
            flat.push(character.to_ascii_lowercase());
        } else if !flat.ends_with(' ') {
            flat.push(' ');
        }
    }
    flat
}

/// Every mention of revocation in `text` whose context carries no denial and
/// puts it in no other subject.
///
/// Returns the context of each unqualified mention, for the failure message.
/// An empty result is the property the issue asks for: nothing in this file
/// claims an operation that does not exist.
fn unqualified_mentions(file: &str, text: &str) -> Vec<String> {
    let flat = flatten(text);
    let mut flagged = Vec::new();
    let mut cursor = 0usize;

    while let Some(found) = flat
        .get(cursor..)
        .and_then(|rest| rest.find(REVOCATION_STEM))
    {
        let at = cursor.saturating_add(found);
        // The context is taken in whole characters from boundaries `char_indices`
        // reports, and both ends are clamped to the text, so the slice is total:
        // a mention at either edge still gets its window rather than a
        // substitute for one.
        let start = flat[..at]
            .char_indices()
            .nth_back(WINDOW)
            .map_or(0, |(offset, _)| offset);
        let end =
            flat[at..]
                .char_indices()
                .nth(WINDOW)
                .map_or(flat.len(), |(offset, character)| {
                    at.saturating_add(offset)
                        .saturating_add(character.len_utf8())
                });
        let window = &flat[start..end];
        let qualified = DENIALS
            .iter()
            .chain(OTHER_SUBJECTS.iter())
            .any(|phrase| window.contains(phrase));
        if !qualified {
            flagged.push(format!("{file}: {window}"));
        }
        cursor = at.saturating_add(REVOCATION_STEM.len());
    }

    flagged
}

/// Whether `path` is this file, `tests/it/authority.rs`: a module of the
/// crate's one integration binary (#272).
///
/// The scan skips it, and it is the only file that is skipped. This file is
/// *about* revocation, so it names the operation on nearly every line and the
/// scan would report the guard itself as a claim. Everything else in the
/// repository is read.
fn is_this_file(path: &Path) -> bool {
    let file = path.file_name().and_then(|name| name.to_str());
    let directory = path
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str());
    file == Some("authority.rs") && directory == Some("it")
}

/// Every first-party text file at or under `root`, sorted, with the walk's
/// skips applied.
fn first_party_files(root: &Path) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    let mut found = Vec::new();
    let mut pending = vec![(root.to_path_buf(), 0u8)];

    while let Some((directory, depth)) = pending.pop() {
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                let name = entry.file_name();
                let skip = SKIPPED_DIRECTORIES
                    .iter()
                    .any(|skipped| name.to_string_lossy().eq_ignore_ascii_case(skipped));
                if !skip && depth < MAX_DEPTH {
                    pending.push((path, depth.saturating_add(1)));
                }
                continue;
            }
            let readable = path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| TEXT_EXTENSIONS.contains(&extension));
            if readable && path.file_name().is_some_and(|name| name != "Cargo.lock") {
                found.push(path);
            }
        }
    }

    found.sort();
    Ok(found)
}

#[test]
fn the_claim_guard_flags_the_claim_it_was_written_for() -> TestResult {
    // The sentence this guard exists for, quoted from `docs/general-bot-fold.md`
    // as it stood when #41 was filed: a live-revocation guarantee with no
    // operation behind it. A guard that does not flag this one is a no-op, and
    // the run of [`no_first_party_text_claims_revocation_this_crate_lacks`]
    // before the fix is what it looked like in the repository.
    let claim = "| REST gateway with JWT and scopes | `Cap` / `GrantSet`, proof-carrying | \
                 Strictly stronger: a scope checked once at a gateway is ambient for the rest \
                 of the request; `Auth` is re-proved on every call, so a grant revoked after \
                 build cannot fire (`lib.rs`) |";

    let flagged = unqualified_mentions("synthetic.md", claim);
    assert_eq!(
        flagged.len(),
        1,
        "the guard must flag the claim it was written for, and flag it once: {flagged:#?}"
    );

    // The control for the control: the same claim with the denial the fix added
    // is not flagged, so the guard is not simply reporting every mention.
    let denied = format!("{claim} `GrantSet` has no revoke operation");
    assert!(
        unqualified_mentions("synthetic.md", &denied).is_empty(),
        "a claim that carries the denial must pass: {:#?}",
        unqualified_mentions("synthetic.md", &denied)
    );

    // Both spellings of the family, because each earlier version of this scan
    // used one stem and could not see the other: a claim written with the verb
    // (`revoke`, the word the original defect used) and one written with the
    // noun (`revocable`, which is what `RevocationFence` is built from).
    for probe in [
        "the host can revoke a capability at runtime and the next call is denied",
        "authority is revocable through whatever grant set the caller holds",
    ] {
        assert_eq!(
            unqualified_mentions("synthetic.md", probe).len(),
            1,
            "the guard must report a claim written with either spelling: {probe}"
        );
    }
    Ok(())
}

#[test]
fn no_first_party_text_claims_revocation_this_crate_lacks() -> TestResult {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let files = first_party_files(&root)?;

    // The walk is the evidence, so its reach is asserted rather than assumed:
    // a walk that reaches nothing would report every file as claim-free.
    assert!(
        files.len() > 50,
        "the walk found only {} text files under {}; too few to be the repository",
        files.len(),
        root.display()
    );
    for expected in [
        "docs/security-posture.md",
        "docs/general-bot-fold.md",
        "crates/lgwks-bot/src/lib.rs",
        "crates/lgwks-bot/README.md",
    ] {
        assert!(
            root.join(expected).is_file(),
            "the walk's roots do not reach {expected}: the corpus is not the repository"
        );
    }

    let mut flagged = Vec::new();
    let mut mentions = 0usize;
    let mut checked_files = 0usize;
    let mut exempt_seen = vec![false; NOT_AUTHORITY_CLAIMS.len()];
    for path in &files {
        if is_this_file(path) {
            continue;
        }
        let relative = path
            .strip_prefix(&root)
            .map_err(|error| format!("{} is outside the repository root: {error}", path.display()))?
            .to_string_lossy()
            .into_owned();
        if let Some(index) = NOT_AUTHORITY_CLAIMS
            .iter()
            .position(|entry| entry.0 == relative)
        {
            if let Some(seen) = exempt_seen.get_mut(index) {
                *seen = true;
            }
            continue;
        }
        let text = fs::read_to_string(path)
            .map_err(|error| format!("{} could not be read: {error}", path.display()))?;
        checked_files = checked_files.saturating_add(1);
        mentions = mentions.saturating_add(text.to_lowercase().matches(REVOCATION_STEM).count());
        flagged.extend(unqualified_mentions(&relative, &text));
    }

    // A stale exemption is an unchecked file nobody noticed: every name in the
    // list has to be a file the walk actually reaches.
    for (entry, seen) in NOT_AUTHORITY_CLAIMS.iter().zip(exempt_seen.iter()) {
        assert!(
            *seen,
            "{} is exempt from this scan ({}) and the walk did not reach it, \
             so the exemption is protecting nothing",
            entry.0, entry.1
        );
    }

    // The scan's own control: at least one mention must have been found, or
    // the assertion below is passing on a corpus with nothing in it.
    assert!(
        mentions > 0,
        "no mention of `{REVOCATION_STEM}` was found in {checked_files} files: the scan is not looking where the claims are"
    );
    assert!(
        flagged.is_empty(),
        "these mentions read as revocation claims, and no operation backs them:\n{flagged:#?}\n\
         Each mention must sit in text that denies the operation (`{DENIALS:?}`), or in one that \
         shows it is about another subject (`{OTHER_SUBJECTS:?}`), or the file must be listed in \
         `NOT_AUTHORITY_CLAIMS` with the reason it is not a claim about this crate"
    );
    Ok(())
}
