//! What an untrusted proposal is allowed to name.
//!
//! A [`Surface`] is the host's answer to one question: *which operations exist,
//! and what does each need?* It is built by the host from the work it already
//! has, never from anything a model produced, and a proposal is admitted only
//! against a surface. A name that is not in it is refused rather than resolved,
//! which is what makes "install a tool" and "read a credential" impossible rather
//! than discouraged: there is no operation for either, so no payload can name
//! one.
//!
//! The surface also carries the run's *held* capabilities. Those are the
//! capabilities the host already granted this run, so a proposal naming an
//! operation whose capability the run lacks is refused with the capability
//! named — a repair a caller can make by granting it deliberately, rather than
//! by widening the decoder.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::cap::Cap;

/// One operation a proposal may name, and what it requires.
///
/// Built only through [`SurfaceBuilder::operation`], so an [`Operation`] cannot
/// exist without a name and a capability list, and those are exactly the two
/// facts admission checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Operation {
    /// The name a proposal writes in its `op` field.
    name: String,
    /// The capabilities this operation needs, all of which the run must already
    /// hold.
    requires: Vec<Cap>,
}

impl Operation {
    /// The name a proposal names this operation by.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The capabilities this operation needs.
    #[must_use]
    pub fn requires(&self) -> &[Cap] {
        &self.requires
    }

    /// The first capability of `required` this operation does not have.
    ///
    /// `BTreeSet` intersection so the answer does not depend on declaration
    /// order: two runs over the same surface name the same missing capability.
    #[must_use]
    pub fn missing(&self, held: &BTreeSet<Cap>) -> Option<&Cap> {
        self.requires.iter().find(|cap| !held.contains(cap))
    }
}

/// The operations one run may propose, and the capabilities it holds.
///
/// Immutable and cheap to share: `Arc` on the handles a caller keeps, so a
/// surface crosses threads without copying the registry per proposal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Surface {
    /// The tenant whose boundary this surface draws.
    tenant: String,
    /// The registered operations, keyed by name. `BTreeMap` so iteration order
    /// is the name order rather than a hash order, which keeps a report of
    /// refusals reproducible.
    operations: BTreeMap<String, Operation>,
    /// The capabilities this run already holds.
    held: BTreeSet<Cap>,
}

impl Surface {
    /// Begin building the surface for `tenant`.
    ///
    /// # Errors
    ///
    /// [`SurfaceError::InvalidTenant`] naming what is wrong with the name.
    pub fn builder(tenant: &str) -> Result<SurfaceBuilder, SurfaceError> {
        let tenant = crate::script::Tenant::new(tenant)
            .map_err(|error| SurfaceError::InvalidTenant(error.to_string()))?;
        Ok(SurfaceBuilder {
            tenant,
            operations: BTreeMap::new(),
            held: BTreeSet::new(),
        })
    }

    /// The tenant whose boundary this surface draws.
    #[must_use]
    pub fn tenant(&self) -> &str {
        &self.tenant
    }

    /// The operation named `name`, when it is registered.
    #[must_use]
    pub fn operation(&self, name: &str) -> Option<&Operation> {
        self.operations.get(name)
    }

    /// How many operations are registered.
    #[must_use]
    pub fn registered(&self) -> usize {
        self.operations.len()
    }

    /// Whether this run holds `cap`.
    #[must_use]
    pub fn holds(&self, cap: &Cap) -> bool {
        self.held.contains(cap)
    }

    /// The registered names, in name order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.operations.keys().map(String::as_str)
    }

    /// Check that `operation` may be named, and say why it may not if it may
    /// not.
    ///
    /// The one place the two questions are asked, so a caller cannot decode
    /// without this check and cannot perform one that was missed: the check runs
    /// inside [`crate::proposal::Decoder::decode`] before the payload becomes a
    /// plan.
    ///
    /// # Errors
    ///
    /// [`Refusal::UnknownOperation`](crate::proposal::Refusal::UnknownOperation) when the
    /// name is not registered, or
    /// [`Refusal::CapabilityNotHeld`](crate::proposal::Refusal::CapabilityNotHeld)
    /// naming the capability the run lacks.
    pub fn authorize(&self, name: &str) -> Result<&Operation, crate::proposal::Refusal> {
        use crate::proposal::Refusal;

        let operation = self
            .operations
            .get(name)
            .ok_or_else(|| Refusal::UnknownOperation {
                name: name.to_owned(),
            })?;
        match operation.missing(&self.held) {
            Some(required) => Err(Refusal::CapabilityNotHeld {
                operation: name.to_owned(),
                required: required.as_str().to_owned(),
            }),
            None => Ok(operation),
        }
    }
}

/// The validated construction of one [`Surface`].
#[derive(Debug)]
pub struct SurfaceBuilder {
    /// Who this surface is for.
    tenant: crate::script::Tenant,
    /// The operations declared so far.
    operations: BTreeMap<String, Operation>,
    /// The capabilities the run holds.
    held: BTreeSet<Cap>,
}

impl SurfaceBuilder {
    /// Register `name` as an operation needing exactly `requires`.
    ///
    /// # Errors
    ///
    /// [`SurfaceError::InvalidOperation`] when the name is empty, longer than
    /// the field ceiling, or contains a character that is not a letter, digit,
    /// `-`, `_`, `.` or `:`. The same vocabulary a task name uses, so an
    /// operation name is safe in a step path and in a log line without
    /// escaping.
    pub fn operation(mut self, name: &str, requires: &[Cap]) -> Result<Self, SurfaceError> {
        if !is_operation_name(name) {
            return Err(SurfaceError::InvalidOperation {
                name: name.to_owned(),
            });
        }
        self.operations.insert(
            name.to_owned(),
            Operation {
                name: name.to_owned(),
                requires: requires.to_vec(),
            },
        );
        Ok(self)
    }

    /// Declare that this run already holds every capability in `caps`.
    #[must_use]
    pub fn holding(mut self, caps: &[Cap]) -> Self {
        self.held.extend(caps.iter().cloned());
        self
    }

    /// Declare that this run holds the four shipped capabilities.
    #[must_use]
    pub fn holding_all_shipped(self) -> Self {
        self.holding(&[Cap::net(), Cap::fs(), Cap::sys(), Cap::notify()])
    }

    /// Install the surface.
    #[must_use]
    pub fn build(self) -> Surface {
        Surface {
            tenant: self.tenant.as_str().to_owned(),
            operations: self.operations,
            held: self.held,
        }
    }
}

/// Whether `name` is a usable operation name.
///
/// The same vocabulary [`Tenant`](crate::script::Tenant) enforces, so an
/// operation name and a tenant name are interchangeable in a path and a log line
/// and neither can smuggle a separator.
fn is_operation_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= crate::proposal::MAX_FIELD_BYTES
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:".contains(&byte))
}

// ── SurfaceError ─────────────────────────────────────────────────────────────

/// Why a surface could not be built.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SurfaceError {
    /// The tenant name did not validate.
    InvalidTenant(String),
    /// An operation name did not validate.
    InvalidOperation {
        /// The name given.
        name: String,
    },
}

impl fmt::Display for SurfaceError {
    /// What was refused, with the offending value quoted.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::InvalidTenant(ref reason) => write!(formatter, "InvalidTenant: {reason}"),
            Self::InvalidOperation { ref name } => write!(
                formatter,
                "InvalidOperation: {name:?} is not an ASCII operation name of 1..={} bytes",
                crate::proposal::MAX_FIELD_BYTES
            ),
        }
    }
}

impl std::error::Error for SurfaceError {}
