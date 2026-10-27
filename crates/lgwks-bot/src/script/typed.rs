//! Typed domain vocabularies over the erased registry (#651).
//!
//! A domain is looked up by string at admission and at the step, and its
//! values cross the registry erased (`Box<dyn Any>`). That erasure is the
//! dynamic backend every [`BotSpec`](crate::spec::BotSpec) also runs on, and it
//! stays. What this module adds is the typed home the string ids lacked: a
//! trait per domain with its identifier, its output type and its error type, so
//! a misspelled domain is a missing type rather than a string that compiles,
//! and a wrong output type fails where the domain is defined rather than where
//! the flow runs.
//!
//! The grant check stays where the authority lives: at the step, against the
//! run's grant, refusing [`FlowError::Blocked`] with the whole shortfall. A
//! grant is runtime data, so no type can carry it; the types carry the
//! identity and the shape, and the runtime carries the permission.

use super::{FlowError, Scope, act, observe};

/// A source with a compile-time identity and output type.
pub trait TypedSource {
    /// The registry identifier, as the host's registry declares it.
    const ID: &'static str;
    /// What the source produces.
    type Output: 'static;
    /// The domain's own error type.
    type Error: From<FlowError>;
}

/// An action with a compile-time identity, input type and output type.
pub trait TypedAction {
    /// The registry identifier, as the host's registry declares it.
    const ID: &'static str;
    /// What the action is handed.
    type Input: 'static;
    /// What the action returns.
    type Output: 'static;
    /// The domain's own error type.
    type Error: From<FlowError>;
}

/// `observe` a typed source: the identifier and the output type come from the
/// domain, so the call site names no string and no `T`.
///
/// # Errors
///
/// As [`observe`], mapped into the domain's error type.
pub async fn observe_typed<D: TypedSource>(
    scope: &Scope,
    target: &str,
) -> Result<D::Output, D::Error> {
    observe::<D::Output>(scope, D::ID, target)
        .await
        .map_err(D::Error::from)
}

/// `act` on a typed action: the identifier, the input type and the output type
/// come from the domain, so the call site names no string and performs no
/// downcast.
///
/// # Errors
///
/// As [`act`](super::act), mapped into the domain's error type.
pub async fn act_typed<D: TypedAction>(
    scope: &Scope,
    target: &str,
    value: D::Input,
) -> Result<D::Output, D::Error> {
    act(scope, D::ID, target, value)
        .await
        .map_err(D::Error::from)?
        .into_output::<D::Output>()
        .map_err(D::Error::from)
}

/// Map a flow failure into the caller's domain error type.
///
/// The boundary form for a flow that owns its errors: the flow still returns
/// [`FlowError`] to its caller (the framework's refusals are the framework's),
/// and the caller maps once, at the boundary, into the error its own callers
/// match on.
pub fn map_domain<T, E: From<FlowError>>(result: Result<T, FlowError>) -> Result<T, E> {
    result.map_err(E::from)
}

#[cfg(test)]
mod tests {
    use super::{TypedAction, TypedSource, act_typed, map_domain, observe_typed};
    use crate::script::{FlowError, Scope, Tenant};

    /// A test's result: an error fails it with the error's text.
    type TestResult = Result<(), String>;

    /// A domain error type of the test's own, so the mapping is observable.
    #[derive(Debug, PartialEq)]
    struct DomainFailed(String);

    impl std::fmt::Display for DomainFailed {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(formatter, "domain failed: {}", self.0)
        }
    }

    impl std::error::Error for DomainFailed {}

    impl From<FlowError> for DomainFailed {
        fn from(error: FlowError) -> Self {
            Self(error.to_string())
        }
    }

    /// A source the test registry does not declare.
    struct Missing;

    impl TypedSource for Missing {
        const ID: &'static str = "test::missing";
        type Output = u16;
        type Error = DomainFailed;
    }

    /// An action the test registry does not declare.
    struct Vanished;

    impl TypedAction for Vanished {
        const ID: &'static str = "test::vanished";
        type Input = u16;
        type Output = u16;
        type Error = DomainFailed;
    }

    /// A typed call against a scope with no registry is the domain's own error,
    /// converted once, at the boundary — never the erased value.
    #[test]
    fn an_undeclared_typed_domain_is_the_domain_error() -> TestResult {
        let scope = Scope::root(Tenant::new("acme").map_err(|error| error.to_string())?);
        let observed: Result<u16, DomainFailed> =
            crate::block_on(observe_typed::<Missing>(&scope, "target"));
        let Err(DomainFailed(message)) = observed else {
            return Err(String::from("an undeclared source must fail typed"));
        };
        assert!(
            message.contains("test::missing"),
            "the domain error names the identifier: {message}"
        );
        let acted: Result<u16, DomainFailed> =
            crate::block_on(act_typed::<Vanished>(&scope, "target", 1_u16));
        assert!(
            acted.is_err(),
            "an undeclared action must fail typed as well"
        );
        Ok(())
    }

    /// The boundary map converts once, without touching success.
    #[test]
    fn the_boundary_map_converts_failure_and_keeps_success() -> TestResult {
        let ok: Result<u16, DomainFailed> = map_domain(Ok(7_u16));
        assert_eq!(ok, Ok(7_u16), "success crosses the boundary unchanged");
        let failed: Result<u16, FlowError> = Err(FlowError::failed("down"));
        let mapped: Result<u16, DomainFailed> = map_domain(failed);
        assert!(
            matches!(mapped, Err(DomainFailed(_))),
            "failure crosses as the domain's error"
        );
        Ok(())
    }
}
