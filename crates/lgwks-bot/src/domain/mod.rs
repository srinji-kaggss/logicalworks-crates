//! `domain` owns the shipped domain vocabulary and enforces
//! INV-BOT-DOMAIN-OPEN: new domains are added here without changing the verb
//! axis, and custom domains outside this crate pass the same capability gate.

pub mod chat;
pub mod data;
pub mod flow;
pub mod fs;
pub mod gh;
pub mod net;
pub mod notify;
pub mod sys;

/// Shipped evaluators — composable conditions for the `Evaluate` verb.
pub mod eval;
