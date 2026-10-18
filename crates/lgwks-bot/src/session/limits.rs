//! Byte ceilings and the axes they apply to. Split from `session.rs` (#526).

use super::{BotError, Deserialize, Serialize};

/// Maximum JSON flow size accepted before parsing begins.
pub const MAX_FLOW_BYTES: usize = 2_097_152;

/// Alias for [`MAX_FLOW_BYTES`] with an explicit schema-oriented name.
pub const MAX_FLOW_SPEC_BYTES: usize = MAX_FLOW_BYTES;

/// Maximum bytes accepted in one answer utterance.
///
/// An ingress ceiling, checked before the utterance is resolved or recorded.
/// A person typing an answer does not need more than this, and a caller
/// feeding a session from a file or a socket is the case where an unbounded
/// one costs memory for nothing.
pub const MAX_UTTERANCE_BYTES: usize = 8_192;

/// Maximum bytes one stored variable value may occupy when rendered.
///
/// Applies to the *value*, not to the option string that produced it: an
/// integer stores its decimal digits whatever the candidate said, and a choice
/// candidate stores the declared spelling. It is therefore a bound on what
/// interpolation can later paste into a record, which is the size that matters.
pub const MAX_VALUE_BYTES: usize = 65_536;

/// Maximum bytes of one rendered record payload.
///
/// The ceiling that makes expansion a linear operation. Compiling and sizing a
/// template under this bound costs no more than the bound itself, so a
/// document of a few hundred kilobytes cannot become a record of a few
/// gigabytes: the expansion is refused before it is allocated.
pub const MAX_RECORD_BYTES: usize = 1_048_576;

/// Maximum bytes one session retains across all of its records and visited
/// node ids.
///
/// The aggregate ceiling. Per-record and per-value bounds compose into an
/// unbounded total only if the graph can be walked again, and a budget bounds
/// steps rather than bytes, so the sum needs its own bound.
pub const MAX_SESSION_BYTES: usize = 8_388_608;

/// The byte axis a [`ResourceLimits`] ceiling applies to.
///
/// Reported by [`BotError::ResourceLimitAboveCeiling`] so a refusal names which
/// limit was raised, and used to tighten one axis without spelling the other
/// three. A typed axis rather than a flag per axis, because a caller that wants
/// to raise every ceiling should have to write every axis down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ResourceAxis {
    /// Bytes accepted in one answer utterance ([`MAX_UTTERANCE_BYTES`]).
    Utterance,
    /// Bytes one stored variable value may occupy ([`MAX_VALUE_BYTES`]).
    Value,
    /// Bytes of one rendered record payload ([`MAX_RECORD_BYTES`]).
    Record,
    /// Bytes one session retains in total ([`MAX_SESSION_BYTES`]).
    Session,
}

impl ResourceAxis {
    /// The stable label naming this axis.
    ///
    /// A label for diagnostics and serialization prose, not a serde
    /// discriminator: a `ResourceLimits` document spells the axes as its field
    /// names.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match *self {
            Self::Utterance => "utterance",
            Self::Value => "value",
            Self::Record => "record",
            Self::Session => "session",
        }
    }
}

/// Byte ceilings applied to every session, and the one place they are resolved.
///
/// A validated flow bounds *shape*: steps, nodes, reachable transitions, and
/// templates whose markers name declared variables. It does not bound *bytes*,
/// and the two are independent — `${value}` repeated a hundred thousand times
/// in a two-hundred-kilobyte document is a valid flow that interpolates to ten
/// gigabytes. These four ceilings are the byte bound:
///
/// - [`ResourceAxis::Utterance`] — one answer;
/// - [`ResourceAxis::Value`] — one stored value;
/// - [`ResourceAxis::Record`] — one rendered record payload, checked against
///   the compiled template's computed expansion *before* it is allocated;
/// - [`ResourceAxis::Session`] — everything one session retains.
///
/// [`ResourceLimits::shipped`] is the operator's hard ceiling, and it is a
/// ceiling rather than a default: a shorthand constructor may ask for less on
/// any axis and never for more. A request above the ceiling is refused with
/// [`BotError::ResourceLimitAboveCeiling`] where it is made rather than clamped
/// quietly, because a bound that is silently ignored is a bound nobody can
/// reason about.
///
/// `a_tightened_axis_reads_back_and_one_above_the_ceiling_is_refused`
/// (`tests/it/session_front_door.rs`) runs it: tightening the record axis to 4,096 reads back as
/// 4,096 with the session axis untouched and inside the ceiling, while asking the session axis for
/// `usize::MAX` is refused with `ResourceLimitAboveCeiling` naming that axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde", deny_unknown_fields, default)]
#[non_exhaustive]
pub struct ResourceLimits {
    /// Bytes accepted in one answer utterance.
    utterance_bytes: usize,
    /// Bytes one stored variable value may occupy when rendered.
    pub(super) value_bytes: usize,
    /// Bytes of one rendered record payload.
    pub(super) record_bytes: usize,
    /// Bytes one session retains in total.
    session_bytes: usize,
}

impl ResourceLimits {
    /// The operator's shipped ceilings, applied when nothing tighter is asked
    /// for.
    #[must_use]
    pub const fn shipped() -> Self {
        Self {
            utterance_bytes: MAX_UTTERANCE_BYTES,
            value_bytes: MAX_VALUE_BYTES,
            record_bytes: MAX_RECORD_BYTES,
            session_bytes: MAX_SESSION_BYTES,
        }
    }

    /// Return the ceiling applied to one axis.
    #[must_use]
    pub const fn get(self, axis: ResourceAxis) -> usize {
        match axis {
            ResourceAxis::Utterance => self.utterance_bytes,
            ResourceAxis::Value => self.value_bytes,
            ResourceAxis::Record => self.record_bytes,
            ResourceAxis::Session => self.session_bytes,
        }
    }

    /// Return these ceilings with one axis set to `bytes`.
    ///
    /// Tightening only in intent: a value above the operator's ceiling is
    /// accepted by this constructor and refused by [`Self::within_ceiling`],
    /// so the caller learns that the bound it asked for is not the bound in
    /// force. `const` so a caller can build a ceiling at compile time.
    #[must_use]
    pub const fn tighten(mut self, axis: ResourceAxis, bytes: usize) -> Self {
        match axis {
            ResourceAxis::Utterance => self.utterance_bytes = bytes,
            ResourceAxis::Value => self.value_bytes = bytes,
            ResourceAxis::Record => self.record_bytes = bytes,
            ResourceAxis::Session => self.session_bytes = bytes,
        }
        self
    }

    /// The stricter of two ceiling sets, axis by axis.
    ///
    /// How an author's tightening and an operator's tightening combine: the
    /// smaller bound wins, so neither can widen what the other narrowed.
    #[must_use]
    pub const fn narrowed(self, other: Self) -> Self {
        Self {
            utterance_bytes: smaller(self.utterance_bytes, other.utterance_bytes),
            value_bytes: smaller(self.value_bytes, other.value_bytes),
            record_bytes: smaller(self.record_bytes, other.record_bytes),
            session_bytes: smaller(self.session_bytes, other.session_bytes),
        }
    }

    /// Refuse any axis set above the operator's ceiling.
    ///
    /// # Errors
    ///
    /// Returns [`BotError::ResourceLimitAboveCeiling`] naming the first axis
    /// that exceeds its ceiling.
    pub fn within_ceiling(self) -> Result<Self, BotError> {
        let ceiling = Self::shipped();
        for axis in [
            ResourceAxis::Utterance,
            ResourceAxis::Value,
            ResourceAxis::Record,
            ResourceAxis::Session,
        ] {
            let requested = self.get(axis);
            let allowed = ceiling.get(axis);
            if requested > allowed {
                let refusal = Err(BotError::ResourceLimitAboveCeiling {
                    axis,
                    requested,
                    ceiling: allowed,
                });
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "within_ceiling: returning an error to the caller");
                return refusal;
            }
        }
        Ok(self)
    }
}

impl Default for ResourceLimits {
    /// The operator's shipped ceilings — not "no limits".
    fn default() -> Self {
        Self::shipped()
    }
}

/// The operator's shipped byte ceilings, as one named value.
///
/// The case a flow document that declares no ceilings is held to. A constant
/// rather than a call so the case is a *name* at the two places that take it,
/// beside the field whose absence selects it — `unwrap_or_default()` would hide
/// which of the two resource-limit sets a document without a request is held to,
/// and a bare `Default::default()` would say nothing at all.
pub(super) const SHIPPED_LIMITS: ResourceLimits = ResourceLimits::shipped();

/// The smaller of two byte counts, named because `Ord::min` is not `const`.
pub(super) const fn smaller(left: usize, right: usize) -> usize {
    if left < right { left } else { right }
}
