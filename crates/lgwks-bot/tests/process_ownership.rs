//! Compatibility target for the process ownership acceptance gate.
//!
//! The ownership journey is maintained in `rt_process.rs`; keeping this named
//! target makes the acceptance command exercise that same black-box contract.

#[path = "rt_process.rs"]
mod rt_process;
