//! OS signal streams.
//!
//! On Unix, Tokio permits multiple streams for the same signal and delivers
//! each notification to all of them. A long-lived service may still register
//! its signals once at startup and broadcast from there to centralize policy.

pub use lgwks_deps::tokio::signal::*;
