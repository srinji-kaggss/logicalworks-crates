//! The draws and trace queries the sweeping families use over the seed core.
//!
//! Apart from `seed.rs` because one target needs only the core:
//! `sim_review_path` draws with `between` and `below` and hashes a trace, and
//! never asks for a weighted coin or whether a trace is empty. Every item a
//! target includes must be one it calls — an unused one is a dead-code warning
//! the workspace denies, and an allow is not a fix — so the core holds what all
//! four includers call and this file holds what three of them do.
//!
//! Included beside `seed.rs`, as its sibling, by `lgwks_bot`'s `sim/mod.rs`,
//! `lgwks_ast`'s `it` and `lgwks_std`'s `sim_fs_walk`.

impl super::seed::Rng {
    /// A `bool` that is true with probability `per_mille / 1000`.
    pub fn chance(&mut self, per_mille: u32) -> bool {
        self.below(1000) < per_mille
    }
}

impl super::seed::Trace {
    /// Whether anything was recorded.
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}
