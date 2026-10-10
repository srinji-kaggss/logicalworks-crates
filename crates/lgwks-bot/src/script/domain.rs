//! `observe` and `act`: a registry domain called as a step of a flow (#388).
//!
//! A flow names a domain by the identifier its host's
//! [`DomainRegistry`] declares, the same string a [`BotSpec`](crate::spec::BotSpec)
//! chain names it by, and reaches the same constructor through the same
//! erased verb. That is what makes a script and a spec over one registry one
//! program: the source a flow polls is built by the constructor a spec's chain
//! would build, polled by the same `poll_any`, and its output downcast at the
//! same type boundary (INV-BOT-17).
//!
//! # Two checks, at two moments
//!
//! - **At the flow's entry**, [`admit_domains`] looks up every identifier the
//!   flow writes, in one pass, and refuses the flow before its first step with
//!   one [`Need::UnknownDomain`] per identifier the registry lacks, each naming
//!   the registry and its nearest declared identifier. A flow that would poll
//!   two sources and then fail on a third misspelled one never polls the two.
//! - **At the step**, [`observe`] and [`act`] build the domain from its target
//!   and check its declared capabilities against the run's authority through
//!   [`Scope::require`], so a domain short of authority blocks the run with its
//!   whole shortfall, exactly as a hand-written step's `require` does. The
//!   domain is then handed a grant of exactly those capabilities and nothing
//!   wider.
//!
//! # What `act` does not run
//!
//! `act` is a step, not an effect dispatch: it records no intent before the
//! action runs. An action whose [`EffectLifetime`] is
//! [`External`](EffectLifetime::External) is refused before it runs with
//! [`BotError::UnjournaledEffect`], which names the `BotSpec` chain whose
//! dispatch journals it. Only an effect that stays in the process runs here,
//! and a local effect repeated after a restart is one the process alone could
//! see.

use std::any::{Any, type_name};

use crate::error::BotError;
use crate::gate::GrantSet;
use crate::registry::{DomainRegistry, Role};
use crate::spec::{Erased, Need, NeedSet};
use crate::verb::EffectLifetime;

use super::{FlowError, Scope};

/// One identifier a flow names and the word it names it with, for the
/// admission at the flow's entry. Written by the `script!` expansion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DomainUse {
    /// The registry half the identifier is looked up in.
    role: Role,
    /// The identifier as the flow wrote it.
    id: &'static str,
}

impl DomainUse {
    /// Looked up among the registry's sources, as `observe id of ..` writes it.
    #[must_use]
    pub const fn observe(id: &'static str) -> Self {
        Self {
            role: Role::Source,
            id,
        }
    }

    /// Looked up among the registry's actions, as `act id on .. with ..` writes
    /// it.
    #[must_use]
    pub const fn act(id: &'static str) -> Self {
        Self {
            role: Role::Action,
            id,
        }
    }
}

/// What an `act` step's action returned, held until the flow asks for it as a
/// type.
///
/// Opaque rather than generic: an `act` line is usually a statement, and a
/// statement whose type nothing names cannot be inferred. A flow that wants
/// the output binds the step and asks for it with [`Acted::into_output`].
#[derive(Debug)]
pub struct Acted {
    /// The action's identifier, for a mismatch to name.
    domain: String,
    /// The action's output, erased.
    output: Box<dyn Any>,
}

impl Acted {
    /// The action's output as `T`.
    ///
    /// # Errors
    ///
    /// [`BotError::TypeMismatch`] when the action's output is not a `T`.
    pub fn into_output<T: 'static>(self) -> Result<T, FlowError> {
        let Self { domain, output } = self;
        match output.downcast::<T>() {
            Ok(typed) => Ok(*typed),
            Err(_other_type) => {
                let refusal = Err(FlowError::from(BotError::TypeMismatch {
                    site: "script::act",
                    chain: None,
                    expected: type_name::<T>(),
                    observed: "the output of the action this step named",
                }));
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), %domain, "into_output: returning an error to the caller");
                refusal
            }
        }
    }
}

/// Refuse the flow `scope` is running unless its host's registry declares
/// every identifier in `uses`, each in its role.
///
/// One pass over all of them, so the refusal names every unknown identifier
/// at once; called by the expansion at the flow's entry, before its first
/// step.
///
/// # Errors
///
/// [`BotError::UndeclaredDomains`] naming each identifier the registry lacks,
/// or that no registry was installed; [`BotError::DuplicateDomain`] when the
/// registry itself declares one identifier twice.
pub fn admit_domains(scope: &Scope, uses: &[DomainUse]) -> Result<(), FlowError> {
    let registry = installed();
    if let Some(registry) = registry {
        registry
            .validate()
            .map_err(|refused| FlowError::from(refused).located(scope))?;
    }
    let needs: Vec<Need> = uses
        .iter()
        .filter(|used| !registry.is_some_and(|known| known.declares(used.role, used.id)))
        .map(|used| unknown(registry, used.role, used.id))
        .collect();
    if needs.is_empty() {
        return Ok(());
    }
    let refusal = Err(FlowError::from(BotError::UndeclaredDomains {
        needs: NeedSet::new(needs),
    })
    .located(scope));
    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "admit_domains: returning an error to the caller");
    refusal
}

/// `observe id of target`: build the source the registry declares under `id`
/// from `target`, poll it once under exactly the capabilities it declares, and
/// return its value as `T`.
///
/// # Errors
///
/// The identifier undeclared ([`BotError::UndeclaredDomains`]); the
/// constructor's refusal of `target`; [`FlowError::Blocked`] when the run's
/// authority does not cover the source's capabilities; the source's own
/// failure; [`BotError::TypeMismatch`] when its value is not a `T`.
pub async fn observe<T: 'static>(scope: &Scope, id: &str, target: &str) -> Result<T, FlowError> {
    let registry = resolve(scope, Role::Source, id)?;
    let source = registry
        .build_source(id, target)
        .map_err(|refused| FlowError::from(refused).located(scope))?;
    let (observer, ..) = source.into_parts();
    let caps = observer.required_caps();
    scope.require(caps)?;
    let grants = exactly(caps);
    let polled = observer
        .poll_any(&grants, None)
        .await
        .map_err(|failed| FlowError::from(failed).located(scope))?;
    // With no previous value there is nothing for the poll to be equal to, so
    // `None` is a source that broke the erased contract rather than a reading.
    let Some(Erased { value, witness }) = polled else {
        let refusal = Err(FlowError::from(BotError::TypeMismatch {
            site: "script::observe",
            chain: None,
            expected: type_name::<T>(),
            observed: "no value, from a poll with nothing to compare against",
        })
        .located(scope));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "observe: returning an error to the caller");
        return refusal;
    };
    match value.downcast::<T>() {
        Ok(typed) => Ok(*typed),
        Err(_other_type) => {
            let refusal = Err(FlowError::from(BotError::TypeMismatch {
                site: "script::observe",
                chain: None,
                expected: type_name::<T>(),
                observed: witness.name(),
            })
            .located(scope));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "observe: returning an error to the caller");
            refusal
        }
    }
}

/// `act id on target with value`: build the action the registry declares
/// under `id` from `target` and run it on `value` under exactly the
/// capabilities it declares.
///
/// # Errors
///
/// The identifier undeclared ([`BotError::UndeclaredDomains`]); the
/// constructor's refusal of `target`; [`BotError::UnjournaledEffect`] for an
/// action whose effect leaves the process, before it runs;
/// [`FlowError::Blocked`] when the run's authority does not cover the action's
/// capabilities; [`BotError::TypeMismatch`] when `value` is not the action's
/// input; the action's own failure.
pub async fn act<V: 'static>(
    scope: &Scope,
    id: &str,
    target: &str,
    value: V,
) -> Result<Acted, FlowError> {
    let registry = resolve(scope, Role::Action, id)?;
    let action = registry
        .build_action(id, target)
        .map_err(|refused| FlowError::from(refused).located(scope))?
        .into_execute_any();
    match action.effect_lifetime() {
        EffectLifetime::Local => {}
        EffectLifetime::External => {
            let refusal = Err(FlowError::from(BotError::UnjournaledEffect {
                domain: id.to_owned(),
            })
            .located(scope));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "act: returning an error to the caller");
            return refusal;
        }
    }
    let caps = action.required_caps();
    scope.require(caps)?;
    let grants = exactly(caps);
    let input = Erased::new(value);
    let output = action
        .run_any(&grants, &input)
        .await
        .map_err(|failed| FlowError::from(failed).located(scope))?;
    Ok(Acted {
        domain: id.to_owned(),
        output,
    })
}

/// The registry this run resolves domains against, when its host has one.
fn installed() -> Option<&'static DomainRegistry> {
    super::run_store::authority().and_then(|authority| authority.domains())
}

/// The installed registry when it declares `id` in `role`, or the refusal
/// [`admit_domains`] would have given for it.
///
/// Checked again at the step, not trusted from the entry: a step reached by a
/// flow that was not expanded by `script!` has had no admission.
fn resolve(scope: &Scope, role: Role, id: &str) -> Result<&'static DomainRegistry, FlowError> {
    let registry = installed();
    match registry {
        Some(known) if known.declares(role, id) => Ok(known),
        _ => {
            let refusal = Err(FlowError::from(BotError::UndeclaredDomains {
                needs: NeedSet::new(vec![unknown(registry, role, id)]),
            })
            .located(scope));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "resolve: returning an error to the caller");
            refusal
        }
    }
}

/// The need for `id`, undeclared in `role` of `registry`.
fn unknown(registry: Option<&'static DomainRegistry>, role: Role, id: &str) -> Need {
    Need::UnknownDomain {
        role: role.as_str(),
        domain: id.to_owned(),
        registry: registry.and_then(DomainRegistry::name).map(str::to_owned),
        installed: registry.is_some(),
        nearest: registry
            .and_then(|known| known.nearest(role, id))
            .map(str::to_owned),
    }
}

/// A grant of exactly `caps`: what a domain is handed once the run's authority
/// is known to cover them, so it can reach nothing it did not declare.
fn exactly(caps: &[crate::cap::Cap]) -> GrantSet {
    caps.iter()
        .cloned()
        .fold(GrantSet::empty(), GrantSet::grant)
}
