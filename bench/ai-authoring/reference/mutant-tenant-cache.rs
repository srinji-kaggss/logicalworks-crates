//! Deliberately wrong solution — `tenant-cache`, resource-keyed map.
//!
//! This is a mutant input for the harness, not an example: it checks the
//! token correctly but keys the map by the resource alone, so the first
//! tenant to ask for a resource decides what every later tenant observes. The
//! oracle's isolation clause must fail it. It is never built by the estate
//! workspace (this directory is not a member), so it is not held to the
//! estate lint contract.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

pub use ai_task_support::tenant_cache::CacheError as CacheError;

/// One entry per resource, with no tenant anywhere: the shape the oracle's
/// isolation clause exists to catch.
fn leaked() -> &'static Mutex<HashMap<String, Vec<u8>>> {
    static CACHED: OnceLock<Mutex<HashMap<String, Vec<u8>>>> = OnceLock::new();
    CACHED.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Refuse the read as the whole answer, naming why on the trace stream.
/// A `return Err(..)` names no cause; returning this names it.
fn err_refused<T>(cause: impl std::fmt::Debug) -> Result<T, CacheError> {
    ai_task_support::diagnostic(format_args!("tenant-cache mutant refused: {cause:?}"));
    Err(CacheError::Refused)
}

pub async fn solve(who: &str, what: &str, proof: &str) -> Result<Vec<u8>, CacheError> {
    if proof != format!("token-for-{who}") {
        return err_refused("the proof does not open this tenant");
    }
    let mut held = match leaked().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    if let Some(found) = held.get(what) {
        return Ok(found.clone());
    }
    let value = format!("value-of-{who}-{what}").into_bytes();
    held.insert(what.to_owned(), value.clone());
    Ok(value)
}
