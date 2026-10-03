//! Repairing a blocked run: the need set, the ticket, and the bounded grant.
//!
//! # What a repair is, and what it is not
//!
//! A run blocked on unmet authority produces a [`RepairTicket`]: an intervention
//! request naming *exactly* the needs that stopped it, bound to the run, the
//! tenant, and the repair epoch it was minted at. A repair is the host answering
//! that ticket with a [`GrantSet`] — a delta limited to the ticket's needs — and
//! resuming the run. It is deliberately narrow:
//!
//! - The grant may not exceed the ticket's needs. A grant naming a capability the
//!   ticket never asked for is [`RepairError::OverWide`] and is refused, because a
//!   repair ticket is a request for one specific authority and answering it with
//!   more is how "repair" becomes "grant-all".
//! - The repair resumes under the run's id. Every step recorded before the block
//!   replays without its body being polled, so the prior *analysis* is not rerun;
//!   only the blocked remainder runs.
//! - The repair is charged against the run's **root** budget ([`RunLedger`]) and
//!   does not refill it. A run that keeps returning `NotApplied` therefore reaches
//!   a finite typed refusal rather than unlimited retries (T13), while an
//!   authorized repair is a distinct event that consumes budget rather than
//!   resetting it.
//!
//! # T24: a ticket, an epoch, and a denial
//!
//! The same ticket delivered twice, a ticket from an older epoch, and a denied
//! repair each have their own refusal, and each leaves the run blocked with its
//! authority unchanged. Nothing in this module mutates anything: every decision
//! is a comparison, and the mutation is the ledger's one ordered step, which
//! refuses exactly these cases before any byte moves.
//!
//! [`RunLedger`]: super::RunLedger

use std::fmt;

use lgwks_std::hash::Hasher;

use crate::cap::{Cap, Deficit};
use crate::effect::RunId;
use crate::gate::GrantSet;

use super::ledger::TicketStamp;

/// The most capabilities one repair ticket may name.
///
/// A ticket is a request, and a request with a thousand entries is not a repair
/// of one blocked step — it is a second admission. The bound keeps a ticket
/// something a person reads and a host grants narrowly.
pub const MAX_TICKET_NEEDS: usize = 64;

/// How a repair was refused.
///
/// Every arm is a refusal before any effect: none of them applies authority,
/// advances an epoch, or lets the blocked remainder run. A caller holding one
/// knows the run is still blocked and its authority unchanged, because that is
/// what the repair door guarantees before it does anything.
#[derive(Debug)]
#[non_exhaustive]
pub enum RepairError {
    /// The ticket names a tenant other than this host's.
    ForeignTenant {
        /// The tenant the ticket names.
        ticket: String,
        /// The tenant this host runs for.
        host: String,
    },
    /// The run's repair ledger has never seen this run, so its budget, epoch and
    /// applied tickets cannot be attributed to it. A repair against a run this
    /// host cannot place would mint an epoch for work it does not own.
    UnknownRun {
        /// The run the ticket names, hex-encoded.
        run: String,
    },
    /// The ticket's epoch is not the run's current epoch, so it was minted
    /// against a repair state the run is no longer in. Both an older ticket and a
    /// newer one are refused by this one check: "stale" names the disagreement,
    /// not the direction.
    StaleEpoch {
        /// The epoch the run is at.
        current: u64,
        /// The epoch the ticket names.
        offered: u64,
    },
    /// This exact ticket was already applied to this run, so applying it again
    /// would widen authority or repeat an effect a second time.
    AlreadyApplied,
    /// The grant does not cover every capability the ticket names, so the repair
    /// would leave the run still blocked. A partial grant is a denial, not a
    /// repair.
    NotAuthorized {
        /// The capabilities the ticket asked for that the grant does not carry.
        missing: Vec<Cap>,
    },
    /// The grant reaches outside the ticket's needs. A repair answers one
    /// request; a wider answer is refused rather than narrowed silently, so what
    /// the caller believes was granted cannot exceed what was asked.
    OverWide {
        /// The capabilities the grant carries that the ticket never asked for.
        beyond: Vec<Cap>,
    },
    /// The run's root budget is spent and an authorized repair does not refill
    /// it, so the run stays blocked at a finite refusal.
    BudgetSpent {
        /// The attempts that would have been charged.
        attempts: u64,
        /// The largest attempt count admitted.
        max_attempts: u64,
    },
    /// The run belongs to a tenant other than the one repairing it.
    LedgerForeignTenant {
        /// The tenant that owns the run.
        owner: String,
        /// The tenant that asked to repair it.
        asked: String,
    },
    /// This host has no repair ledger, so a run has no budget, epoch or applied
    /// tickets and a repair could not be decided at all.
    NoLedger,
    /// The run's device refused the ledger write.
    Store {
        /// The store's own refusal.
        cause: super::StoreError,
    },
}

impl fmt::Display for RepairError {
    /// The refusal, naming both sides of the disagreement where there are two.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::ForeignTenant {
                ref ticket,
                ref host,
            } => write!(
                formatter,
                "the repair ticket names tenant {ticket:?}, not {host:?}; \
                 refusing to repair another tenant's run"
            ),
            Self::UnknownRun { ref run } => write!(
                formatter,
                "no repair ledger for run {run} under this tenant; refusing a repair whose \
                 budget and epoch cannot be attributed"
            ),
            Self::StaleEpoch { current, offered } => write!(
                formatter,
                "repair epoch {offered} is not this run's current epoch {current}; \
                 refusing a stale repair ticket"
            ),
            Self::AlreadyApplied => formatter.write_str(
                "this repair ticket was already applied to this run; refusing to widen authority \
                 or repeat an applied repair",
            ),
            Self::NotAuthorized { ref missing } => write!(
                formatter,
                "the repair grant is missing {} capability the ticket names ({}); leaving the run blocked",
                missing.len(),
                render(missing),
            ),
            Self::OverWide { ref beyond } => write!(
                formatter,
                "the repair grant names {} capability the ticket never asked for ({}); \
                 refusing to widen authority beyond the request",
                beyond.len(),
                render(beyond),
            ),
            Self::BudgetSpent {
                attempts,
                max_attempts,
            } => write!(
                formatter,
                "root budget spent: {attempts} attempts against a ceiling of {max_attempts}; \
                 an authorized repair does not refill it"
            ),
            Self::LedgerForeignTenant {
                ref owner,
                ref asked,
            } => write!(
                formatter,
                "repair ledger says the run belongs to {owner:?}, not {asked:?}"
            ),
            Self::NoLedger => formatter.write_str(
                "this host has no repair ledger, so a repair has no budget, epoch or applied \
                 tickets to be decided against; refusing it unchecked",
            ),
            Self::Store { ref cause } => write!(formatter, "{cause}"),
        }
    }
}

impl std::error::Error for RepairError {
    /// The store's own error, when the refusal came from the device.
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match *self {
            Self::Store { ref cause } => Some(cause),
            _ => None,
        }
    }
}

impl From<super::LeaseRefusal> for RepairError {
    /// Map the ledger's own refusal onto the repair door's vocabulary.
    ///
    /// One conversion rather than a match per call site: every `?` on a ledger
    /// refusal wraps the same four decisions, and a literal per site would be four
    /// chances to name a different refusal for one event.
    fn from(cause: super::LeaseRefusal) -> Self {
        match cause {
            super::LeaseRefusal::ForeignTenant { owner, asked } => {
                Self::LedgerForeignTenant { owner, asked }
            }
            super::LeaseRefusal::StaleEpoch { current, offered } => {
                Self::StaleEpoch { current, offered }
            }
            super::LeaseRefusal::AlreadyApplied => Self::AlreadyApplied,
            super::LeaseRefusal::BudgetSpent {
                attempts,
                max_attempts,
            } => Self::BudgetSpent {
                attempts,
                max_attempts,
            },
        }
    }
}

/// Render a capability list compactly for a refusal message.
fn render(caps: &[Cap]) -> String {
    caps.iter().map(Cap::as_str).collect::<Vec<_>>().join(", ")
}

/// An intervention request: this run is blocked, and here is exactly why.
///
/// A *report*, never a grant — the same rule [`NeedSet`] follows for a spec's
/// unmet needs. The needs are what the run asked for and the host has not
/// supplied; only the host, through [`Host::repair`](super::Host::repair), can
/// turn them into authority, and only for these needs at this epoch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairTicket {
    /// The run that is blocked.
    run: RunId,
    /// The tenant that owns the run.
    tenant: String,
    /// The repair epoch this ticket was minted at.
    epoch: u64,
    /// The unmet needs, in the order admission named them.
    needs: Vec<Cap>,
}

impl RepairTicket {
    /// Build a ticket for `run` blocked on `needs` at `epoch`.
    ///
    /// Crate-private because a ticket is a fact about a run's admission: a caller
    /// free to mint one could assert a block that never happened. The host mints
    /// it from the [`Deficit`] its admission computed, through
    /// [`RepairTicket::of_deficit`].
    pub(crate) fn of(run: RunId, tenant: &str, epoch: u64, needs: Vec<Cap>) -> Self {
        Self {
            run,
            tenant: tenant.to_owned(),
            epoch,
            needs,
        }
    }

    /// Build a ticket from the shortfall an admission computed.
    ///
    /// The one place a ticket is born, so the run, tenant, epoch and needs on a
    /// ticket cannot come from three different places and disagree. Every unmet
    /// requirement is carried, in the order the shortfall named them — the whole
    /// point of the [`Deficit`] being one complete value rather than a sequence of
    /// one-at-a-time answers.
    pub(crate) fn of_deficit(run: RunId, tenant: &str, epoch: u64, deficit: &Deficit) -> Self {
        Self::of(
            run,
            tenant,
            epoch,
            deficit
                .shortages()
                .map(|shortage| shortage.required().clone())
                .collect(),
        )
    }

    /// The run this ticket repairs.
    #[must_use]
    pub const fn run(&self) -> RunId {
        self.run
    }

    /// The tenant that owns the run.
    #[must_use]
    pub fn tenant(&self) -> &str {
        &self.tenant
    }

    /// The repair epoch this ticket was minted at.
    #[must_use]
    pub const fn epoch(&self) -> u64 {
        self.epoch
    }

    /// The unmet needs, in the order admission named them.
    #[must_use]
    pub fn needs(&self) -> &[Cap] {
        &self.needs
    }

    /// This ticket's content-derived identity, as the ledger sees it.
    ///
    /// Hashes the run, the tenant, the epoch and the **sorted** needs. Sorted
    /// because a need set is a set: two tickets naming the same needs in a
    /// different order are the same request, and must share an identity, or a
    /// reordered redelivery would slip past the duplicate check and apply twice.
    /// Content-derived because a caller that rebuilds a ticket from the same facts
    /// must produce the same identity — that is what makes "the same ticket
    /// delivered twice" decidable at all.
    pub(crate) fn stamp(&self) -> TicketStamp {
        let mut sorted = self.needs.clone();
        sorted.sort();
        let mut hasher = Hasher::new();
        hasher.write_framed(b"lgwks-bot/repair-ticket/v1");
        hasher.write_framed(self.run.id().to_hex().as_bytes());
        hasher.write_framed(self.tenant.as_bytes());
        hasher.write_framed(&self.epoch.to_le_bytes());
        for cap in &sorted {
            hasher.write_framed(cap.as_str().as_bytes());
        }
        TicketStamp {
            identity: hasher.finalize().as_bytes().to_vec(),
            epoch: self.epoch,
        }
    }

    /// The capabilities the ticket asks for that `grant` does not carry, in the
    /// order the ticket names them.
    pub(crate) fn missing_from(&self, grant: &GrantSet) -> Vec<Cap> {
        self.needs
            .iter()
            .filter(|cap| !grant.grants(cap))
            .cloned()
            .collect()
    }

    /// The capabilities `grant` carries that the ticket never asked for, drawn
    /// from `candidates`.
    ///
    /// Driven by the candidate list rather than by the grant's own contents,
    /// because a [`GrantSet`] is a membership structure: it answers "is this one
    /// granted" and has no enumeration to walk. So the check asks about each
    /// capability the caller *could* be adding, and reports the ones the grant
    /// carries that the ticket does not ask for. A caller with a custom
    /// capability passes it in `candidates`; the shipped four are the default.
    pub(crate) fn beyond_in(&self, grant: &GrantSet, candidates: &[Cap]) -> Vec<Cap> {
        candidates
            .iter()
            .filter(|cap| grant.grants(cap) && !self.needs.contains(cap))
            .cloned()
            .collect()
    }

    /// Check a grant against this ticket, refusing both a short grant and a wide
    /// one.
    ///
    /// Order matters and is the same order [`Host::repair`](super::Host::repair)
    /// documents: the missing half is reported first, because that is the half a
    /// caller must fix, and a grant that is both short and wide is not repairable
    /// at all.
    pub(crate) fn check_grant(
        &self,
        grant: &GrantSet,
        candidates: &[Cap],
    ) -> Result<(), RepairError> {
        let missing = self.missing_from(grant);
        if !missing.is_empty() {
            return Err(RepairError::NotAuthorized { missing });
        }
        let beyond = self.beyond_in(grant, candidates);
        if !beyond.is_empty() {
            return Err(RepairError::OverWide { beyond });
        }
        Ok(())
    }
}

/// The shipped capabilities, as the over-wide check's default candidate list.
///
/// A [`Cap`] may be any dotted name, so a grant carrying only custom capabilities
/// would pass an over-wide check with an empty candidate list. The repair door
/// therefore takes the candidate list explicitly — the caller knows which custom
/// capabilities exist — and defaults it to these four.
#[must_use]
pub fn shipped_candidates() -> Vec<Cap> {
    vec![Cap::net(), Cap::fs(), Cap::sys(), Cap::notify()]
}
