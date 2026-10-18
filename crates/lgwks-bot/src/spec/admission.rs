//! Admission: why a spec could not be materialized, and the complete report of what it still needs.
//! Split from `spec.rs` (#526) so the refusal vocabulary is one file.

use crate::cap::Cap;
use crate::error::{BotError, Escaped};
use crate::gate::GrantSet;

/// Why a spec could not be materialized into a runnable bot.
///
/// Two refusals with two different repairs. [`Admission::Refused`] is a defect
/// in the document or the registry itself — a version this build does not
/// implement, an empty chain list, a registry that declares one identifier
/// twice — and no amount of authority closes it. [`Admission::Needs`] is a
/// complete, attributable report of everything a *well-formed* document asked
/// for that this host cannot supply yet; the repair is a decision the caller
/// makes (grant a capability, install an adapter), never something this crate
/// performs on its own.
///
/// Neither arm is a partial build. A refused spec produces no bot, so no source
/// is polled and no action runs: the materializer is all-or-nothing by
/// construction, because the object a caller would otherwise hold is one whose
/// chains are half-real.
#[non_exhaustive]
#[derive(Debug)]
pub enum Admission {
    /// The document or the registry is malformed, independently of authority.
    Refused(BotError),
    /// Every presently knowable unmet need, in declaration order.
    Needs(NeedSet),
}

impl std::fmt::Display for Admission {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            Self::Refused(ref cause) => write!(formatter, "{cause}"),
            Self::Needs(ref needs) => write!(formatter, "admission refused: {needs}"),
        }
    }
}

impl std::error::Error for Admission {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match *self {
            Self::Refused(ref cause) => Some(cause),
            Self::Needs(_) => None,
        }
    }
}

impl From<BotError> for Admission {
    fn from(cause: BotError) -> Self {
        Self::Refused(cause)
    }
}

/// Every presently knowable unmet need of one materialization, in one value.
///
/// The DX-08 report. A materializer that returned at the first unmet need made
/// admission a loop in which each refusal revealed one more requirement, so a
/// spec short of four things cost four round trips to learn and the repair could
/// not be written until the last of them. This is the whole difference computed
/// in one pass, each entry attributed to the chain index — and, where it belongs
/// to a specific action, the action index — that needs it.
///
/// **A report, never a grant.** [`NeedSet::proposed_grants`] derives the grant
/// set that would close the capability needs, and it is a *proposal*: the
/// trusted host accepts, narrows, or refuses it, and this crate never folds it
/// into authority on its own. No `grant_all` fallback exists, and a spec cannot
/// grant itself anything by naming a domain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NeedSet {
    /// The needs, in declaration order.
    needs: Vec<Need>,
}

impl NeedSet {
    /// Assemble a need set from its entries, taken verbatim and in order.
    #[must_use]
    pub fn new(needs: Vec<Need>) -> Self {
        Self { needs }
    }

    /// The needs, in declaration order.
    #[must_use]
    pub fn needs(&self) -> &[Need] {
        &self.needs
    }

    /// Every need, in declaration order.
    pub fn iter(&self) -> std::slice::Iter<'_, Need> {
        self.needs.iter()
    }

    /// How many needs the report carries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.needs.len()
    }

    /// Whether the report is empty. An empty set never reaches a caller — the
    /// materializer returns a bot instead — so this exists for a caller holding
    /// one, not as a "success" signal.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.needs.is_empty()
    }

    /// The grant set that would close the capability needs, as a **proposal**.
    ///
    /// A repair proposal, not authority: the returned set is data the caller may
    /// inspect, narrow, or refuse, and no code path in this crate admits a bot
    /// through it. Only the [`GrantSet`] the caller passes to the materializer
    /// supplies authority, so a spec still cannot choose what a bot reaches.
    /// Needs that are not about capabilities (an unknown domain, a rejected
    /// target) contribute nothing here, because no grant repairs them.
    #[must_use]
    pub fn proposed_grants(&self) -> GrantSet {
        let mut grants = GrantSet::empty();
        for need in &self.needs {
            match *need {
                Need::MissingCapability { ref capability, .. } => {
                    grants = grants.grant(capability.clone());
                }
                // A lapsed credential is repaired by re-granting the same names:
                // the proposal is the same set a deficit derives, and what makes
                // it a *repair* rather than a shortage is that the caller has
                // already got them.
                Need::CredentialExpired {
                    ref capabilities, ..
                } => {
                    for capability in capabilities {
                        grants = grants.grant(capability.clone());
                    }
                }
                _ => {}
            }
        }
        grants
    }

    /// The repair an adapter reports when its upstream refused the credential it
    /// presented: one need naming every capability to re-grant, attributed to
    /// `domain`.
    ///
    /// Built here rather than assembled by each adapter, because the whole point
    /// of this type is that the shortfall arrives complete and in one piece: an
    /// adapter that named one capability at a time would reproduce the very loop
    /// a `NeedSet` was introduced to end. Sorted and de-duplicated, so two
    /// callers deriving the repair from the same requirements produce the same
    /// need set.
    ///
    /// Not a shortage, and deliberately shaped unlike one: the capabilities are
    /// still granted, so the repair *re-grants* them rather than reporting them
    /// missing.
    #[must_use]
    pub fn expired_credentials(domain: &str, capabilities: &[Cap]) -> Self {
        let mut sorted = capabilities.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        Self::new(vec![Need::CredentialExpired {
            domain: domain.to_owned(),
            capabilities: sorted,
        }])
    }
}

impl std::fmt::Display for NeedSet {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{} unmet need(s)", self.needs.len())?;
        for (index, need) in self.needs.iter().enumerate() {
            formatter.write_str(if index == 0 { ": " } else { ", " })?;
            write!(formatter, "{need}")?;
        }
        Ok(())
    }
}

/// One unmet need, attributed to the chain (and action) that raised it.
///
/// The attribution is the repair: "an unknown domain" is a fact about the
/// document, while "chain 3, action 1 names unknown domain `notify::absent`" is
/// a location in it. Indices are declaration positions, zero-based, matching the
/// order [`BotSpec::chains`](super::BotSpec::chains) and [`ChainSpec::on`](super::ChainSpec::on) are walked.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Need {
    /// A chain's source identifier is not in the registry.
    UnknownSource {
        /// The chain's index in the spec.
        chain: usize,
        /// The identifier the spec named.
        domain: String,
    },
    /// An action's domain identifier is not in the registry.
    UnknownAction {
        /// The owning chain's index in the spec.
        chain: usize,
        /// The action's index within the chain's `on` list.
        action: usize,
        /// The identifier the spec named.
        domain: String,
    },
    /// A source constructor refused the target the spec gave it.
    SourceTargetRejected {
        /// The chain's index in the spec.
        chain: usize,
        /// The source identifier the spec named.
        domain: String,
        /// The constructor's own refusal, escaped.
        cause: String,
    },
    /// An action constructor refused the target the spec gave it.
    ActionTargetRejected {
        /// The owning chain's index in the spec.
        chain: usize,
        /// The action's index within the chain's `on` list.
        action: usize,
        /// The action identifier the spec named.
        domain: String,
        /// The constructor's own refusal, escaped.
        cause: String,
    },
    /// A chain names a condition outside the supported wire vocabulary.
    UnknownCondition {
        /// The owning chain's index in the spec.
        chain: usize,
        /// The condition's index within the chain's `on` list.
        action: usize,
        /// The condition identifier the spec spelled.
        condition: String,
    },
    /// A credential the run's authority rests on expired, or a receiver refused
    /// the one it presented.
    ///
    /// Not a [`Self::MissingCapability`] for the same reason
    /// [`BotError::CredentialExpired`] is not a capability denial: the
    /// capability is still granted and now names nothing, so the repair is a
    /// *re-grant* of capabilities that are already
    /// held. A need set that named them as missing would produce an admission
    /// that grants what it already has and refuses again.
    CredentialExpired {
        /// The domain whose authority lapsed or was rejected upstream.
        domain: String,
        /// Every capability to re-grant, sorted by name so two callers deriving
        /// the repair from the same facts produce the same need.
        capabilities: Vec<Cap>,
    },
    /// A domain requires a capability the caller's grant set does not carry.
    MissingCapability {
        /// The owning chain's index in the spec.
        chain: usize,
        /// The action's index when the requirement is an action's, or `None`
        /// when it is the source's.
        action: Option<usize>,
        /// The domain that declared the requirement.
        domain: String,
        /// The capability it requires and was not granted.
        capability: Cap,
    },
    /// A flow's `observe` or `act` names an identifier the run's registry
    /// does not declare in that role (#388).
    ///
    /// Carries what a reader repairs from: the registry it was looked up in
    /// (`None` when the host was given none) and the closest identifier that
    /// registry does declare, so a typo reads as one.
    UnknownDomain {
        /// `"source"` for `observe`, `"action"` for `act`.
        role: &'static str,
        /// The identifier the flow wrote.
        domain: String,
        /// The registry's declared name, or `None` when no registry was
        /// installed or it was assembled without one.
        registry: Option<String>,
        /// Whether a registry was installed for the run at all.
        installed: bool,
        /// The closest identifier the registry declares in the same role.
        nearest: Option<String>,
    },
}

impl Need {
    /// The chain index this need is attributed to, when it belongs to one.
    ///
    /// `Option` rather than a bare index because one need genuinely has no
    /// chain: a credential that lapsed is a fact about the run's authority, not
    /// about a position in a document, and attributing it to chain 0 would
    /// point a repair at a place it has nothing to do with.
    #[must_use]
    pub fn chain(&self) -> Option<usize> {
        match *self {
            Self::UnknownSource { chain, .. }
            | Self::UnknownAction { chain, .. }
            | Self::SourceTargetRejected { chain, .. }
            | Self::ActionTargetRejected { chain, .. }
            | Self::UnknownCondition { chain, .. }
            | Self::MissingCapability { chain, .. } => Some(chain),
            Self::CredentialExpired { .. } | Self::UnknownDomain { .. } => None,
        }
    }

    /// The action index within the chain, when the need belongs to one action
    /// rather than the source or the chain as a whole.
    #[must_use]
    pub fn action(&self) -> Option<usize> {
        match *self {
            Self::UnknownAction { action, .. }
            | Self::ActionTargetRejected { action, .. }
            | Self::UnknownCondition { action, .. } => Some(action),
            Self::MissingCapability { action, .. } => action,
            Self::UnknownSource { .. }
            | Self::SourceTargetRejected { .. }
            | Self::CredentialExpired { .. }
            | Self::UnknownDomain { .. } => None,
        }
    }
}

impl std::fmt::Display for Need {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            Self::UnknownSource { chain, ref domain } => write!(
                formatter,
                "chain {chain}: unknown source domain {}",
                Escaped(domain)
            ),
            Self::UnknownAction {
                chain,
                action,
                ref domain,
            } => write!(
                formatter,
                "chain {chain} action {action}: unknown action domain {}",
                Escaped(domain)
            ),
            Self::SourceTargetRejected {
                chain,
                ref domain,
                ref cause,
            } => write!(
                formatter,
                "chain {chain}: source {} refused its target: {}",
                Escaped(domain),
                Escaped(cause)
            ),
            Self::ActionTargetRejected {
                chain,
                action,
                ref domain,
                ref cause,
            } => write!(
                formatter,
                "chain {chain} action {action}: action {} refused its target: {}",
                Escaped(domain),
                Escaped(cause)
            ),
            Self::CredentialExpired {
                ref domain,
                ref capabilities,
            } => {
                let names: Vec<&str> = capabilities.iter().map(|cap| cap.as_str()).collect();
                write!(
                    formatter,
                    "{}: re-grant [{}], whose credential expired or was refused upstream",
                    Escaped(domain),
                    names.join(", ")
                )
            }
            Self::UnknownCondition {
                chain,
                action,
                ref condition,
            } => write!(
                formatter,
                "chain {chain} action {action}: unknown condition {}",
                Escaped(condition)
            ),
            Self::MissingCapability {
                chain,
                action,
                ref domain,
                ref capability,
            } => match action {
                Some(action) => write!(
                    formatter,
                    "chain {chain} action {action}: {} requires ungranted capability {}",
                    Escaped(domain),
                    capability
                ),
                None => write!(
                    formatter,
                    "chain {chain}: {} requires ungranted capability {}",
                    Escaped(domain),
                    capability
                ),
            },
            Self::UnknownDomain {
                role,
                ref domain,
                ref registry,
                installed,
                ref nearest,
            } => {
                if !installed {
                    return write!(
                        formatter,
                        "{role} `{}` is named, and this host was given no domain registry; \
                         install one with `HostBuilder::domains`",
                        Escaped(domain)
                    );
                }
                match *registry {
                    Some(ref name) => write!(
                        formatter,
                        "{role} `{}` is not declared in registry `{}`",
                        Escaped(domain),
                        Escaped(name)
                    )?,
                    None => write!(
                        formatter,
                        "{role} `{}` is not declared in the host's registry",
                        Escaped(domain)
                    )?,
                }
                match *nearest {
                    Some(ref close) => {
                        write!(formatter, "; the nearest declared is `{}`", Escaped(close))
                    }
                    None => formatter.write_str("; it declares nothing close"),
                }
            }
        }
    }
}
