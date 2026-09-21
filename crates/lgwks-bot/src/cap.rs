//! `cap` owns the capability token for bot domains and enforces
//! INV-BOT-CAP-DOTTED: the shipped capabilities are dotted string names drawn
//! from the bot vocabulary, in the same shape as the capability model used
//! across this project. [`Cap::new`] accepts any name by convention: custom
//! capabilities are data-driven (`Cap::new("your.domain.cap")`), so the
//! constructor cannot reject unknown names without breaking that path.
//! Enforcement is by equality at the gate (`required ⊆ granted`): an unknown or
//! misspelled name simply never matches a grant. Prefer the
//! [`Cap::NET`]/[`Cap::FS`]/[`Cap::SYS`]/[`Cap::NOTIFY`] constants and their
//! shorthand constructors for shipped capabilities.
//!
//! Names ([`Cap`]) are forgeable labels; authority is the sealed [`Auth`]
//! proof, minted only by [`GrantSet`](crate::gate::GrantSet). Every
//! side-effecting verb takes `(Auth, input)` tuples and checks coverage
//! before acting. This stops confused-deputy calls and accidental ungated
//! use; it is not a sandbox, since in-process code can always dial out
//! directly, so the guarantee is explicit, auditable authority, not
//! confinement.

use lgwks_std::json::{Deserialize, Serialize};
use std::borrow::Cow;
use std::fmt;

use super::error::BotError;
use super::gate::GrantSet;

/// A capability permission required by a bot domain.
///
/// Dotted string name: `bot.net`, `bot.fs`, `bot.sys`, `bot.notify`. Compared
/// by name equality. The gate checks `required ⊆ granted` before a bot builds.
///
/// # Why the name is a `Cow<'static, str>`
///
/// Almost every capability in a running bot is one of the shipped constants,
/// and the rest are dotted names built once from configuration. A `String`
/// would make the common case pay for the rare one: [`Cap::net`] would
/// allocate, and — because `GrantSet::issue` mints a proof by copying the
/// requirement list — every effect execution would re-allocate the same four
/// constants. `Cow::Borrowed` makes the shipped case a pointer copy, so
/// cloning a shipped capability is free and authority for it costs no
/// allocation at all.
///
/// Equality, ordering and hashing all compare the *contents*, so a `Borrowed`
/// and an `Owned` spelling of the same name are one capability — which is what
/// makes a deserialized `Cap` interchangeable with a constant rather than a
/// second, unequal one.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde")]
pub struct Cap(Cow<'static, str>);

impl Cap {
    /// Network access: HTTP, WebSocket, API calls.
    pub const NET: &str = "bot.net";
    /// Filesystem access: read, write, watch paths.
    pub const FS: &str = "bot.fs";
    /// System access: process control, environment.
    pub const SYS: &str = "bot.sys";
    /// Notification delivery: Slack, email, webhook push.
    pub const NOTIFY: &str = "bot.notify";

    /// Construct a capability from its dotted name. Any name is accepted:
    /// authority is decided by the grant set, not by this constructor, so an
    /// unknown name is a capability nothing grants rather than an error here.
    pub fn new(name: impl Into<Cow<'static, str>>) -> Self {
        Self(name.into())
    }

    /// The dotted name, which is the stable identity.
    ///
    /// Borrowed, never copied: the name lives in the capability, and every
    /// caller here wants to read it, log it, or compare it.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_ref()
    }

    /// Shorthand for `Cap::new(Cap::NET)`.
    #[must_use]
    pub fn net() -> Self {
        Self::new(Self::NET)
    }

    /// Shorthand for `Cap::new(Cap::FS)`.
    #[must_use]
    pub fn fs() -> Self {
        Self::new(Self::FS)
    }

    /// Shorthand for `Cap::new(Cap::SYS)`.
    #[must_use]
    pub fn sys() -> Self {
        Self::new(Self::SYS)
    }

    /// Shorthand for `Cap::new(Cap::NOTIFY)`.
    #[must_use]
    pub fn notify() -> Self {
        Self::new(Self::NOTIFY)
    }
}

impl fmt::Display for Cap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ── What a check found missing ───────────────────────────────────────────────

/// The domain that declared a capability requirement.
///
/// Carried so a denial names something a person can act on. `bot.net` alone
/// says what is missing; `gh::pr_status` says *who is asking*, which is the
/// question an operator actually has — a bot with twelve sources should not
/// have to be bisected to learn which one wants the network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Demand {
    /// The domain identifier that declared the requirement.
    domain: String,
}

impl Demand {
    /// Record the domain that required a capability.
    #[must_use]
    pub fn new(domain: impl Into<String>) -> Self {
        Self {
            domain: domain.into(),
        }
    }

    /// The domain identifier, as its `domain_id` reports it.
    #[must_use]
    pub fn domain(&self) -> &str {
        &self.domain
    }
}

/// One requirement that a check found ungranted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shortage {
    /// The capability that was required and not granted.
    required: Cap,
    /// Who declared the requirement, when the check site knows.
    ///
    /// `None` for a bare [`Auth::check`] and for [`GrantSet::admit`]: both are
    /// handed a capability list and no caller, and a `&[Cap]` does not say who
    /// assembled it. The two sites that *do* know — bot admission and the
    /// substrate's per-entry check — fill it in, so a denial a person must act
    /// on names the domain to act on.
    demand: Option<Demand>,
}

impl Shortage {
    /// Record one unmet requirement.
    ///
    /// `pub(crate)` rather than public: a shortage is a fact a check observed,
    /// and a caller that can mint one can assert a denial that never happened.
    pub(crate) fn new(required: Cap, demand: Option<Demand>) -> Self {
        Self { required, demand }
    }

    /// The capability that was required and not granted.
    #[must_use]
    pub fn required(&self) -> &Cap {
        &self.required
    }

    /// Who declared the requirement, when the check site knew.
    #[must_use]
    pub fn demand(&self) -> Option<&Demand> {
        self.demand.as_ref()
    }
}

/// Every requirement a check found ungranted, at once.
///
/// The unit of a denial is the whole shortfall, not its first element, and that
/// is the entire reason this type exists. A check that returns one missing
/// capability at a time makes admission a loop: the caller grants what they were
/// told, tries again, and is told the next word. Nothing in the loop is
/// *wrong* — each pass reports a true fact — but the repair is serialised
/// against the check, so a requirement list of length `n` costs `n` round trips
/// to discover, and the person on the other end experiences it as a build that
/// keeps refusing for reasons that keep changing.
///
/// The check already computed the whole answer: it is `required` minus
/// `granted`. This type is that difference reported without discarding the rest
/// of it, so one pass names every unmet requirement and the repair is written
/// once. The cost is that the type is a list rather than a scalar; the benefit
/// is that the list is complete.
///
/// **Never empty, and that is a property of the type rather than a rule about
/// it.** The first requirement is a field and the remainder is a `Vec`, so the
/// empty case is unrepresentable and [`first`](Deficit::first) is total without
/// a panic path. The obvious shape — one `Vec<Shortage>` plus a documented
/// precondition — was rejected here because this crate forbids `expect` and
/// `panic`, so the only ways to make `first()` total over a `Vec` would be an
/// unwrap that cannot be written or a returned `Option` every caller would have
/// to dismiss knowing it can never be `None`. Making the state
/// unrepresentable costs one field and removes the question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deficit {
    /// The first requirement that was not met.
    first: Shortage,
    /// The rest, in the order admission asked for them, each named once.
    rest: Vec<Shortage>,
}

impl Deficit {
    /// Build from a shortfall, or `None` when nothing was missing.
    ///
    /// `Option` rather than a precondition, because that is the honest return
    /// type of "the shortfall, if there is one": the caller is a check that has
    /// just computed a difference and needs to know which of the two it has.
    ///
    /// Crate-private because a caller free to mint a `Deficit` is a caller free
    /// to assert a denial that never happened.
    pub(crate) fn from_shortages(shortages: Vec<Shortage>) -> Option<Self> {
        let mut remaining = shortages.into_iter();
        let first = remaining.next()?;
        Some(Self::new(first, remaining.collect()))
    }

    /// Build from a first requirement and the remainder.
    ///
    /// Total, because non-emptiness is carried by the first argument's type
    /// rather than asserted about a `Vec`. [`from_shortages`](Deficit::from_shortages)
    /// is the form a check wants; this one is what a caller that already holds
    /// the head uses.
    pub(crate) fn new(first: Shortage, rest: Vec<Shortage>) -> Self {
        Self { first, rest }
    }

    /// Every unmet requirement, in the order admission asked for it.
    ///
    /// An iterator rather than a slice, because the first shortage is a field
    /// and the rest are a `Vec`: the two are not contiguous, and collecting
    /// them into one would either copy every shortage or give up the
    /// non-emptiness that makes [`first`](Deficit::first) total.
    pub fn shortages(&self) -> impl Iterator<Item = &Shortage> {
        std::iter::once(&self.first).chain(self.rest.iter())
    }

    /// How many requirements were unmet, counting a capability required by two
    /// domains twice.
    #[must_use]
    pub fn len(&self) -> usize {
        // Saturating rather than `+ 1`: the workspace forbids plain arithmetic,
        // and a shortfall of `usize::MAX` requirements is not a case worth a
        // panic. `len` is only ever compared against small numbers.
        self.rest.len().saturating_add(1)
    }

    /// Whether the shortfall is empty — always `false`, by construction.
    ///
    /// Present because clippy pairs `len` with it, and because a caller folding
    /// deficits together asks the question. The answer being constant is the
    /// point of the type, not an accident of it.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        false
    }

    /// The first unmet requirement.
    ///
    /// Total, not an `Option`: a `Deficit` that exists has at least one
    /// shortage, so there is no absence for a caller to handle. This is the
    /// shape the crate used to report for *every* denial — one capability —
    /// available here for a caller that genuinely wants only one.
    #[must_use]
    pub fn first(&self) -> &Shortage {
        &self.first
    }

    /// The grant set that repairs exactly this shortfall.
    ///
    /// The primitive that turns a diagnostic into a repair without a person in
    /// the middle: the shortfall already names every capability that would close
    /// it, so the caller hands back the set the deficit derived rather than
    /// translating a message by hand — which is the step where a repair gets
    /// written for the first shortage and the rest are missed.
    ///
    /// Idempotent under [`GrantSet::grant`], which is consuming and de-dupes, so
    /// folding it into a set the caller already holds reaches a fixed point:
    /// `held.grant(..)` per element, or `shortfall.to_grant_set()` handed
    /// straight to a builder.
    #[must_use]
    pub fn to_grant_set(&self) -> GrantSet {
        self.shortages().fold(GrantSet::empty(), |set, shortage| {
            set.grant(shortage.required.clone())
        })
    }
}

/// Every requirement in `required` that `satisfied` does not accept, in
/// declaration order, each capability named once.
///
/// The membership test is a parameter rather than a fixed call because the two
/// checks answer it differently and both answers are right: [`Auth`] holds a
/// sorted `Vec` and binary-searches it, [`GrantSet`] holds a `HashSet` and
/// hashes. Sharing this walk keeps the two from drifting into reporting
/// different shortfalls for the same hole, which is the failure that would make
/// the completeness property below a per-caller accident.
pub(crate) fn uncovered<F>(required: &[Cap], satisfied: F, demand: Option<&Demand>) -> Vec<Shortage>
where
    F: Fn(&Cap) -> bool,
{
    let mut shortages: Vec<Shortage> = Vec::new();
    for cap in required {
        if satisfied(cap) {
            continue;
        }
        // Named once. A domain that lists the same capability twice is stating
        // one requirement, and reporting it twice would inflate `len` into a
        // number that does not count anything.
        //
        // A linear scan here rather than the `HashSet` the collections guidance
        // would prefer, and the reason is what the two paths cost. This body
        // runs only for a capability that was *not* satisfied, so on the path a
        // running bot takes — every requirement covered — it does not execute
        // at all, and the scan is bounded by the size of the *shortfall*, not
        // of the requirement list. A set would have to be built before the
        // first comparison, which would allocate on the success path to
        // accelerate a failure path. Trading an allocation per check for a
        // scan that never runs is the wrong direction.
        if shortages.iter().any(|shortage| shortage.required == *cap) {
            continue;
        }
        shortages.push(Shortage::new(cap.clone(), demand.cloned()));
    }
    shortages
}

// ── Sealed proof ─────────────────────────────────────────────────────────────

/// Proof of granted authority. A tuple struct with a private payload:
/// only [`GrantSet`] can mint one, so presenting an
/// `Auth` proves the host granted every capability it covers. Deliberately
/// not serializable: authority must not round-trip through JSON.
///
/// Check coverage with [`Auth::check`] before any side effect.
///
/// The covered set is held sorted and deduplicated, which is what makes
/// [`check`](Auth::check) logarithmic rather than quadratic. Coverage is asked
/// once per required capability, so a linear scan here is a linear scan inside a
/// loop over the requirement list — `required.len() * granted.len()` string
/// comparisons, growing with the *product* of two counts. Sorting at
/// construction moves that product to `required.len() * log2(granted.len())`,
/// and costs one sort per mint rather than per check. The derivation is
/// measured in `bench/README.md`, where the unsorted form reached 12.9
/// microseconds for 128 capabilities against 3.4 nanoseconds for one.
#[derive(Debug, Clone)]
pub struct Auth(Vec<Cap>);

impl Auth {
    /// Mint a proof covering exactly `caps`. Crate-private on purpose: this is
    /// the seal. [`GrantSet::issue`](super::gate::GrantSet::issue) is the only
    /// caller, so a proof can never cover a capability the gate did not admit.
    ///
    /// Sorts and deduplicates, because both are properties of a *set* that the
    /// caller passed as a `Vec`. [`covers`](Auth::covers) therefore returns name
    /// order rather than the order the capabilities were required in — the order
    /// a requirement list has is a property of that list, and a proof does not
    /// carry one.
    pub(crate) fn new(mut caps: Vec<Cap>) -> Self {
        caps.sort_unstable();
        caps.dedup();
        Self(caps)
    }

    /// The capabilities this proof covers, in name order.
    #[must_use]
    pub fn covers(&self) -> &[Cap] {
        &self.0
    }

    /// Whether this proof covers `cap`.
    ///
    /// The single-capability question, for a caller that has one. Binary search
    /// because [`Auth`] is sorted by construction.
    #[must_use]
    pub fn covers_cap(&self, cap: &Cap) -> bool {
        self.0.binary_search(cap).is_ok()
    }

    /// The requirements in `required` that this proof does not cover, in
    /// declaration order, each named once.
    ///
    /// The total form: what [`check`](Auth::check) reports without the early
    /// return that made it a one-at-a-time loop.
    #[must_use]
    pub fn uncovered(&self, required: &[Cap]) -> Vec<Shortage> {
        uncovered(required, |cap| self.covers_cap(cap), None)
    }

    /// Deny with [`BotError::CapabilityDenied`] naming **every** required
    /// capability this proof does not cover.
    ///
    /// Coverage is exact set membership, not subsumption: a proof also denies
    /// a call whose caps it covers only partly, and it denies a call requiring a
    /// capability outside the shipped four just as readily, because no name is
    /// special-cased at check time.
    ///
    /// Reports the whole shortfall rather than its first element — see
    /// [`Deficit`] for why that is the difference between an admission loop and
    /// an admission. The `demand` is left unset here because this function is
    /// handed a capability list and no caller; the substrate fills it in where
    /// it knows the domain.
    ///
    /// # Errors
    ///
    /// [`BotError::CapabilityDenied`] when any required capability is not
    /// covered.
    pub fn check(&self, required: &[Cap]) -> Result<(), BotError> {
        match Deficit::from_shortages(self.uncovered(required)) {
            Some(deficit) => Err(BotError::CapabilityDenied { deficit }),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Auth, Cap, Deficit, Demand};
    use crate::error::BotError;
    use crate::gate::GrantSet;

    /// A typed test failure, in the shape the other modules' tests use. Not a
    /// `panic!`, which this workspace forbids outright.
    fn failed(cause: impl Into<String>) -> BotError {
        BotError::DomainError {
            domain: "cap::tests".into(),
            cause: cause.into(),
        }
    }

    /// Capabilities by name, in the order given.
    ///
    /// `&'static str` rather than `&str` because that is what `Cap::new`
    /// accepts, and the restriction is the point of the `Cow`: a capability
    /// name is a constant or a value the caller owns, never a borrow of
    /// something shorter-lived that a proof would then outlive.
    fn caps(names: &[&'static str]) -> Vec<Cap> {
        names.iter().map(|name| Cap::new(*name)).collect()
    }

    /// The names a deficit reports, in order — what a caller actually reads.
    fn names(deficit: &Deficit) -> Vec<&str> {
        deficit
            .shortages()
            .map(|shortage| shortage.required().as_str())
            .collect()
    }

    /// The whole reason `Deficit` exists.
    ///
    /// The check this replaced returned at the first uncovered capability, so
    /// a caller granting what they were told was refused again for the next
    /// word. Three requirements, one covered, must produce **two** shortages
    /// from **one** refusal.
    #[test]
    fn a_check_names_every_uncovered_requirement_at_once() -> Result<(), BotError> {
        let proof = Auth::new(caps(&[Cap::NET]));
        let required = caps(&[Cap::NET, Cap::FS, Cap::SYS]);

        match proof.check(&required) {
            Err(BotError::CapabilityDenied { deficit }) => {
                assert_eq!(
                    names(&deficit),
                    vec![Cap::FS, Cap::SYS],
                    "the deficit must carry every uncovered requirement, in declaration order: \
                     {deficit}"
                );
                assert_eq!(deficit.len(), 2);
                assert_eq!(
                    deficit.first().required(),
                    &Cap::fs(),
                    "first is the first one, not an arbitrary one: {deficit}"
                );
                Ok(())
            }
            other => Err(failed(format!(
                "a proof covering `bot.net` must not admit `bot.fs` or `bot.sys`: {other:?}"
            ))),
        }
    }

    #[test]
    fn a_requirement_declared_twice_is_reported_once() -> Result<(), BotError> {
        let proof = Auth::new(Vec::new());

        match proof.check(&caps(&[Cap::NET, Cap::NET, Cap::FS])) {
            Err(BotError::CapabilityDenied { deficit }) => {
                assert_eq!(
                    names(&deficit),
                    vec![Cap::NET, Cap::FS],
                    "a domain listing one capability twice states one requirement: {deficit}"
                );
                assert_eq!(deficit.len(), 2, "and `len` must count requirements");
                Ok(())
            }
            other => Err(failed(format!("an empty proof covers nothing: {other:?}"))),
        }
    }

    /// `to_grant_set` is the "seek the missing pieces" step: the repair is
    /// derived from the diagnostic rather than transcribed from it, which is
    /// where a hand-written repair drops everything after the first line.
    #[test]
    fn the_deficit_derives_the_grant_set_that_repairs_it() -> Result<(), BotError> {
        let proof = Auth::new(caps(&[Cap::NET]));
        let required = caps(&[Cap::NET, Cap::FS, Cap::SYS]);

        match proof.check(&required) {
            Err(BotError::CapabilityDenied { deficit }) => {
                let missing: Vec<Cap> = deficit
                    .shortages()
                    .map(|shortage| shortage.required().clone())
                    .collect();
                let repair = deficit.to_grant_set();

                assert!(
                    repair.admit(&missing).is_ok(),
                    "the derived set must admit every requirement the deficit named: {deficit}"
                );
                assert!(
                    !repair.grants(&Cap::net()),
                    "a deficit is the shortfall, not a restatement of the requirement: \
                     `bot.net` was already covered, so it is not in the repair"
                );

                // The documented composition: fold it into what the caller
                // already holds, which is what closes the requirement. `grant`
                // consumes and de-dupes, so this reaches a fixed point.
                let held = GrantSet::empty().grant(Cap::net());
                let closed = missing
                    .iter()
                    .fold(held.clone(), |set, cap| set.grant(cap.clone()));
                assert!(
                    closed.admit(&required).is_ok(),
                    "the deficit's set folded into the held set must close the requirement"
                );
                assert!(
                    held.admit(&required).is_err(),
                    "control: the held set alone must not, or the test proves nothing"
                );
                assert!(
                    closed
                        .grant(Cap::net())
                        .uncovered(&required, &Demand::new("test::domain"))
                        .is_empty(),
                    "re-granting a capability the set already holds changes nothing"
                );
                Ok(())
            }
            other => Err(failed(format!(
                "expected a denial for two uncovered capabilities: {other:?}"
            ))),
        }
    }

    /// The invariant `covers_cap`'s binary search rests on.
    ///
    /// A proof is a set the caller happened to pass as a `Vec`; sorting and
    /// de-duplicating at mint is what makes each coverage question logarithmic
    /// rather than a scan, and the scan was the quadratic term the benchmark
    /// measured.
    #[test]
    fn a_proof_holds_its_capabilities_sorted_and_deduplicated() {
        let proof = Auth::new(caps(&[Cap::SYS, Cap::NET, Cap::SYS, Cap::FS]));

        assert_eq!(
            proof.covers(),
            caps(&[Cap::FS, Cap::NET, Cap::SYS]).as_slice(),
            "a proof is a set: name-ordered, each capability once"
        );
        assert!(
            proof.covers_cap(&Cap::net()),
            "membership answers for a held cap"
        );
        assert!(
            !proof.covers_cap(&Cap::notify()),
            "and denies one it does not hold"
        );
    }

    /// `Auth::uncovered` and `GrantSet::uncovered` are the same walk, which is
    /// what keeps a proof and a gate from naming different holes for the same
    /// requirement. The only intended difference is attribution.
    #[test]
    fn the_proof_and_the_gate_name_the_same_shortfall() {
        let required = caps(&[Cap::NET, Cap::FS]);
        let proof = Auth::new(Vec::new());
        let gate = GrantSet::empty();

        let from_proof = proof.uncovered(&required);
        let from_gate = gate.uncovered(&required, &Demand::new("test::domain"));

        let proof_names: Vec<&str> = from_proof
            .iter()
            .map(|shortage| shortage.required().as_str())
            .collect();
        let gate_names: Vec<&str> = from_gate
            .iter()
            .map(|shortage| shortage.required().as_str())
            .collect();
        assert_eq!(
            proof_names, gate_names,
            "the two checks must agree on which requirements are unmet"
        );

        assert!(
            from_proof
                .iter()
                .all(|shortage| shortage.demand().is_none()),
            "a bare check is handed a capability list and no caller, so it names no domain"
        );
        assert_eq!(
            from_gate
                .first()
                .and_then(|shortage| shortage.demand())
                .map(Demand::domain),
            Some("test::domain"),
            "and the gate, which was told one, must carry it"
        );
    }

    /// The invariant `Cap`'s `Cow` rests on, and the reason it is safe to make
    /// the shipped constants borrow their names.
    ///
    /// A capability that arrived from JSON is `Owned`; a capability built from
    /// a constant is `Borrowed`. If those compared as different values, a grant
    /// set built from a spec would deny a call made with a constant — the two
    /// spellings of `bot.net` would be two capabilities, and the gate would
    /// refuse a bot it had in fact granted. Equality, ordering and hashing
    /// compare the contents, so they are one capability.
    #[test]
    fn a_borrowed_and_an_owned_spelling_are_one_capability() {
        let borrowed = Cap::net();
        let owned = Cap::new(String::from(Cap::NET));

        assert!(matches!(borrowed.0, std::borrow::Cow::Borrowed(_)));
        assert!(matches!(owned.0, std::borrow::Cow::Owned(_)));

        assert_eq!(borrowed, owned, "the two spellings name one capability");
        assert_eq!(Cap::net().cmp(&owned), std::cmp::Ordering::Equal);
        assert!(
            GrantSet::empty().grant(borrowed).grants(&owned),
            "a grant made with a constant must cover a capability read from a spec"
        );
        assert!(
            GrantSet::empty().grant(owned).grants(&Cap::net()),
            "and the reverse, or the gate would depend on which side did the naming"
        );
    }

    #[test]
    fn a_covered_requirement_is_admitted() -> Result<(), BotError> {
        let proof = Auth::new(caps(&[Cap::NET, Cap::FS]));
        proof.check(&caps(&[Cap::FS, Cap::NET]))?;
        assert!(
            proof.uncovered(&caps(&[Cap::NET, Cap::FS])).is_empty(),
            "the total form and the Result form must agree on the empty case"
        );
        Ok(())
    }
}
